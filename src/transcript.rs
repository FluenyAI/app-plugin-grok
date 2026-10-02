// Where rejections come from.
//
// `PostToolUse` fires after a tool has run, so a tool the developer declined never
// reaches it. Discernment is built on exactly that fact, so without another source
// this milestone can only ever report a 0% rejection rate, which is not a low
// number, it is a wrong one. The other source that does not require a PreToolUse
// gate is the session transcript.
//
// DESIGN DECISION 57 GOVERNS THIS FILE. The transcript came back into scope on one
// condition: tool-use decision records only, and prompt text and assistant
// response text are never materialised, not even transiently, not even locally.
//
// So this is a byte scanner. No line is ever decoded. No record is ever parsed.
// The only strings that come into existence are the short scalars pulled out by
// name, bounded: tool use ids, and the file path a tool was pointed at. Every one
// of them is made in `read_value`, the single place bytes become a String, and the
// tests record every value it makes and fail if a sentinel planted in a prompt
// ever shows up in one.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Map, Value};

use crate::classify::classify_path;
use crate::decline::bytes_look_declined;
use crate::extract::to_repo_relative;

const MAX_TAIL_BYTES: u64 = 8 * 1024 * 1024;

// Bounds on the only two values this file is allowed to read.
const MAX_ID_BYTES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

const NL: u8 = b'\n';

const TOOL_USE: &[u8] = br#""type":"tool_use""#;
const TOOL_RESULT: &[u8] = br#""type":"tool_result""#;
// The needles carry their own opening quote, so `"id":"` cannot match inside
// `"tool_use_id":"` and `"path":"` cannot match inside `"file_path":"`.
const ID_KEY: &[u8] = br#""id":""#;
const TOOL_USE_ID_KEY: &[u8] = br#""tool_use_id":""#;
const CONTENT_KEY: &[u8] = br#""content":"#;
const IS_ERROR_TRUE: &[u8] = br#""is_error":true"#;
const PATH_KEYS: [&[u8]; 4] = [
    br#""file_path":""#,
    br#""notebook_path":""#,
    br#""filePath":""#,
    br#""path":""#,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub tool_use_id: String,
    pub path_class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepResult {
    pub rejections: Vec<Rejection>,
    pub offset: u64,
}

pub fn sweep_transcript(
    transcript: &Path,
    offset: u64,
    repo_root: Option<&str>,
    classifier: &Map<String, Value>,
) -> SweepResult {
    let Ok(size) = std::fs::metadata(transcript).map(|m| m.len()) else {
        return SweepResult {
            rejections: Vec::new(),
            offset,
        };
    };
    // A transcript that shrank was rotated or replaced, so the stored offset
    // points at nothing meaningful and starting over is the only honest reading.
    let offset = if offset > size { 0 } else { offset };
    if size == 0 {
        return SweepResult {
            rejections: Vec::new(),
            offset: 0,
        };
    }

    // One byte before the tail, when there is one, so a tail that begins exactly
    // on a record boundary is told apart from one that begins mid-record.
    let tail_start = size.saturating_sub(MAX_TAIL_BYTES);
    let start = if tail_start > 0 { tail_start - 1 } else { 0 };
    let Some(buffer) = read_range(transcript, start, size - start) else {
        return SweepResult {
            rejections: Vec::new(),
            offset: size,
        };
    };
    let first_line_partial = tail_start > 0 && buffer.first() != Some(&NL);

    // Tool calls are indexed from the whole tail, not only from new bytes: the
    // call that got declined can be on the far side of the offset.
    let mut paths: HashMap<String, Option<String>> = HashMap::new();
    let mut rejections = Vec::new();
    let mut seen = HashSet::new();
    let scan = Scan { repo_root, classifier };

    let mut line_start = 0usize;
    while line_start < buffer.len() {
        let line_end = find(&buffer, line_start, buffer.len(), &[NL]).unwrap_or(buffer.len());
        let absolute = start + line_start as u64;
        // The first line of a truncated tail is usually half a record, which can
        // still carry a whole marker and would be counted twice later.
        if !(first_line_partial && line_start == 0) {
            scan.line(
                &buffer,
                line_start,
                line_end,
                absolute >= offset,
                &mut paths,
                &mut rejections,
                &mut seen,
            );
        }
        line_start = line_end + 1;
    }
    SweepResult {
        rejections,
        offset: size,
    }
}

struct Scan<'a> {
    repo_root: Option<&'a str>,
    classifier: &'a Map<String, Value>,
}

#[derive(Clone, Copy)]
struct Block {
    at: usize,
    len: usize,
    is_use: bool,
}

impl Scan<'_> {
    // One line, as bytes. A block owns the span between the marker before it and
    // the marker after it, so a key search cannot bleed into a block two along.
    // The span reaches BACKWARDS past the marker as well as forwards, because JSON
    // key order is not a contract and Claude Code writes declined results with
    // `tool_use_id` last and ordinary ones with it first.
    #[allow(clippy::too_many_arguments)]
    fn line(
        &self,
        buf: &[u8],
        line_start: usize,
        line_end: usize,
        is_new: bool,
        paths: &mut HashMap<String, Option<String>>,
        rejections: &mut Vec<Rejection>,
        seen: &mut HashSet<String>,
    ) {
        let blocks = block_starts(buf, line_start, line_end);
        for (i, block) in blocks.iter().enumerate() {
            let lower = if i > 0 {
                blocks[i - 1].at + blocks[i - 1].len
            } else {
                line_start
            };
            let upper = blocks.get(i + 1).map(|b| b.at).unwrap_or(line_end);
            let from = object_start(buf, lower, block.at);

            if block.is_use {
                let Some(id) = block_string_field(buf, from, block.at, upper, ID_KEY, MAX_ID_BYTES) else {
                    continue;
                };
                // The path is read forwards only: `input` follows `type` in every
                // shape Claude Code emits, and a backwards search could reach the
                // previous tool call's path, a wrong class rather than a missing one.
                let raw = PATH_KEYS
                    .iter()
                    .find_map(|key| read_string_field(buf, block.at, upper, key, MAX_PATH_BYTES));
                let class = raw.and_then(|p| classify_path(self.classifier, &to_repo_relative(&p, self.repo_root)));
                paths.insert(id, class);
                continue;
            }

            // A result older than the offset was already reported on an earlier sweep.
            if !is_new || !declined(buf, block.at, upper) {
                continue;
            }
            let Some(id) = block_string_field(buf, from, block.at, upper, TOOL_USE_ID_KEY, MAX_ID_BYTES) else {
                continue;
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let path_class = paths.get(&id).cloned().flatten();
            rejections.push(Rejection {
                tool_use_id: id,
                path_class,
            });
        }
    }
}

// A decline is an ERROR result whose content BEGINS with the decline sentence. A
// result that merely quotes the sentence fails both conditions. Measured over 4228
// real tool results, both together find all 15 genuine declines and nothing else.
// The known risk, stated because it is silent if it happens: a Claude Code release
// that stops writing `is_error` on a denial takes the rejection rate to zero.
const DECLINE_WINDOW: usize = 256;

fn declined(buf: &[u8], from: usize, to: usize) -> bool {
    if find(buf, from, to, IS_ERROR_TRUE).is_none() {
        return false;
    }
    let Some(at) = find(buf, from, to, CONTENT_KEY) else {
        return false;
    };
    let value_start = at + CONTENT_KEY.len();
    // Answered on the bytes, without reading the tool result it is asking about.
    bytes_look_declined(buf, value_start, to.min(value_start + DECLINE_WINDOW))
}

// Where the object holding this marker opens. Only short scalars can sit between
// `{` and the `"type"` key, and a budget stops the walk at the previous block.
const OBJECT_START_BUDGET: usize = 512;

fn object_start(buf: &[u8], lower: usize, anchor: usize) -> usize {
    let floor = lower.max(anchor.saturating_sub(OBJECT_START_BUDGET));
    (floor..anchor).rev().find(|&i| buf[i] == b'{').unwrap_or(anchor)
}

// One short scalar belonging to THIS block. Behind the marker first, because a
// key that sits behind it is inside this object by construction.
fn block_string_field(buf: &[u8], from: usize, anchor: usize, upper: usize, key: &[u8], max: usize) -> Option<String> {
    if let Some(behind) = rfind(buf, from, anchor, key) {
        return read_value(buf, behind + key.len(), anchor, max);
    }
    read_string_field(buf, anchor, upper, key, max)
}

fn block_starts(buf: &[u8], from: usize, to: usize) -> Vec<Block> {
    let mut found = Vec::new();
    for (needle, is_use) in [(TOOL_USE, true), (TOOL_RESULT, false)] {
        let mut at = from;
        while let Some(hit) = find(buf, at, to, needle) {
            found.push(Block {
                at: hit,
                len: needle.len(),
                is_use,
            });
            at = hit + needle.len();
        }
    }
    found.sort_by_key(|b| b.at);
    found
}

fn read_string_field(buf: &[u8], from: usize, to: usize, key: &[u8], max: usize) -> Option<String> {
    let at = find(buf, from, to, key)?;
    read_value(buf, at + key.len(), to, max)
}

/// The only place in the sweep where bytes become a string, bounded by `max` so a
/// key whose value is unexpectedly enormous is abandoned rather than read.
fn read_value(buf: &[u8], value_start: usize, to: usize, max: usize) -> Option<String> {
    let limit = to.min(value_start + max).min(buf.len());
    let mut i = value_start;
    while i < limit {
        match buf[i] {
            b'\\' => i += 2,
            b'"' => {
                let raw = buf.get(value_start..i)?;
                let quoted = [b"\"".as_slice(), raw, b"\""].concat();
                let value: String = serde_json::from_slice(&quoted).ok()?;
                #[cfg(test)]
                tests::MATERIALISED.with(|m| m.borrow_mut().push(value.clone()));
                return Some(value);
            }
            _ => i += 1,
        }
    }
    None
}

fn find(buf: &[u8], from: usize, to: usize, needle: &[u8]) -> Option<usize> {
    let to = to.min(buf.len());
    if from >= to || to - from < needle.len() {
        return None;
    }
    buf[from..to]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

// The last occurrence that lies wholly inside [from, to).
fn rfind(buf: &[u8], from: usize, to: usize, needle: &[u8]) -> Option<usize> {
    let to = to.min(buf.len());
    if from >= to || to - from < needle.len() {
        return None;
    }
    buf[from..to]
        .windows(needle.len())
        .rposition(|w| w == needle)
        .map(|p| p + from)
}

fn read_range(path: &Path, start: u64, length: u64) -> Option<Vec<u8>> {
    let mut file = File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buffer = Vec::with_capacity(length as usize);
    file.take(length).read_to_end(&mut buffer).ok()?;
    Some(buffer)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::classify::test_classifier;
    use crate::testing::TempDir;
    use serde_json::json;
    use std::cell::RefCell;

    thread_local! {
        // Every value read_value materialised on this thread.
        pub static MATERIALISED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    const REPO: &str = "/repo";

    fn sweep(path: &Path, offset: u64) -> SweepResult {
        sweep_transcript(path, offset, Some(REPO), &test_classifier())
    }

    fn write(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    fn lines(values: &[Value]) -> String {
        values.iter().map(|v| v.to_string() + "\n").collect()
    }

    fn tool_use(id: &str, path: &str) -> Value {
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [
            { "type": "text", "text": "PROMPTTEXT-let me update the tests" },
            { "type": "tool_use", "id": id, "name": "Edit", "input": { "file_path": path, "new_string": "CODEBODY-secret" } }
        ]}})
    }

    fn tool_result(id: &str, content: &str, is_error: bool) -> Value {
        json!({ "type": "user", "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": id, "is_error": is_error, "content": content }
        ]}})
    }

    fn rejection(id: &str, class: Option<&str>) -> Rejection {
        Rejection {
            tool_use_id: id.into(),
            path_class: class.map(str::to_string),
        }
    }

    #[test]
    fn a_declined_tool_result_becomes_one_rejection_with_a_path_class() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "one.jsonl",
            &lines(&[
                tool_use("toolu_1", &format!("{REPO}/src/pricing.test.ts")),
                tool_result("toolu_1", "The user doesn't want to proceed with this tool use.", true),
            ]),
        );
        let result = sweep(&path, 0);
        assert_eq!(result.rejections, [rejection("toolu_1", Some("tests"))]);
        assert!(result.offset > 0);
        let text = format!("{:?}", result.rejections);
        assert!(!text.contains("PROMPTTEXT") && !text.contains("CODEBODY") && !text.contains("pricing.test.ts"));
    }

    #[test]
    fn a_successful_tool_result_is_not_a_rejection() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "ok.jsonl",
            &lines(&[
                tool_use("toolu_2", &format!("{REPO}/src/app.ts")),
                tool_result("toolu_2", "The file has been updated.", false),
            ]),
        );
        assert!(sweep(&path, 0).rejections.is_empty());
    }

    #[test]
    fn the_offset_stops_a_rejection_being_reported_on_every_later_turn() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "offset.jsonl",
            &lines(&[
                tool_use("toolu_3", &format!("{REPO}/src/auth/login.ts")),
                tool_result("toolu_3", "The user rejected this edit.", true),
            ]),
        );
        let first = sweep(&path, 0);
        assert_eq!(first.rejections, [rejection("toolu_3", Some("auth"))]);
        let second = sweep(&path, first.offset);
        assert!(second.rejections.is_empty());
        assert_eq!(second.offset, first.offset);
    }

    #[test]
    fn a_transcript_that_shrank_is_re_read_from_the_start() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "rotated.jsonl",
            &lines(&[
                tool_use("toolu_4", &format!("{REPO}/README.md")),
                tool_result("toolu_4", "The user does not want to take this action", true),
            ]),
        );
        assert_eq!(sweep(&path, 999_999).rejections, [rejection("toolu_4", Some("docs"))]);
    }

    #[test]
    fn a_missing_or_unparseable_transcript_sweeps_to_nothing() {
        let dir = TempDir::new();
        let missing = sweep(&dir.path().join("nope.jsonl"), 12);
        assert!(missing.rejections.is_empty());
        assert_eq!(missing.offset, 12, "a missing file must not reset the offset");
        let broken = write(&dir, "broken.jsonl", "not json at all\n{\"half\":\n");
        assert!(sweep(&broken, 0).rejections.is_empty());
    }

    #[test]
    fn one_rejection_is_reported_once_even_if_the_result_appears_twice() {
        let dir = TempDir::new();
        let decline = "The user doesn't want to proceed with this tool use.";
        let path = write(
            &dir,
            "dupe.jsonl",
            &lines(&[
                tool_use("toolu_5", &format!("{REPO}/src/app.ts")),
                tool_result("toolu_5", decline, true),
                tool_result("toolu_5", decline, true),
            ]),
        );
        assert_eq!(sweep(&path, 0).rejections.len(), 1);
    }

    fn materialised_during(f: impl FnOnce() -> SweepResult) -> (SweepResult, Vec<String>) {
        MATERIALISED.with(|m| m.borrow_mut().clear());
        let result = f();
        let values = MATERIALISED.with(|m| m.borrow().clone());
        (result, values)
    }

    // Design decision 57's condition as a test: never READ, not only never sent.
    #[test]
    fn the_sweep_never_materialises_prompt_or_response_text_only_ids_and_paths() {
        let dir = TempDir::new();
        let secret = "SENTINEL-e7b41f-NEVER-READ";
        let path = write(
            &dir,
            "never-read.jsonl",
            &lines(&[
                json!({ "type": "assistant", "message": { "role": "assistant", "content": [
                    { "type": "text", "text": format!("{secret} let me refactor the pricing module") },
                    { "type": "tool_use", "id": "toolu_57", "name": "Edit",
                      "input": { "file_path": format!("{REPO}/src/auth/session.ts"), "new_string": format!("{secret} in a diff") } }
                ]}}),
                json!({ "type": "user", "message": { "role": "user", "content": [
                    { "type": "text", "text": format!("{secret} in a follow up prompt") },
                    { "type": "tool_result", "tool_use_id": "toolu_57", "is_error": true,
                      "content": format!("The user doesn't want to proceed with this tool use. {secret}") }
                ]}}),
            ]),
        );
        let (result, values) = materialised_during(|| sweep(&path, 0));
        assert_eq!(result.rejections, [rejection("toolu_57", Some("auth"))]);
        assert!(!values.is_empty());
        let leaked: Vec<&String> = values.iter().filter(|v| v.contains(secret)).collect();
        assert!(
            leaked.is_empty(),
            "prompt or response text was materialised: {leaked:?}"
        );
    }

    // ---- the shapes a real session produces ----

    const DECLINE: &str = "The user doesn't want to proceed with this tool use. The tool use was rejected \
        (eg. if it was a file edit, the new_string was NOT written to the file).";

    fn real_use(id: &str, path: &str) -> Value {
        json!({ "type": "tool_use", "id": id, "name": "Edit", "input": { "file_path": path, "old_string": "a", "new_string": "b" }, "caller": "assistant" })
    }

    fn id_last(id: &str, content: &str, is_error: bool) -> Value {
        // Built by hand so the key order is exactly what Claude Code writes.
        serde_json::from_str(&format!(
            r#"{{"type":"tool_result","content":{},"is_error":{is_error},"tool_use_id":"{id}"}}"#,
            Value::String(content.into())
        ))
        .unwrap()
    }

    fn id_first(id: &str, content: &str, is_error: bool) -> Value {
        serde_json::from_str(&format!(
            r#"{{"tool_use_id":"{id}","type":"tool_result","content":{},"is_error":{is_error}}}"#,
            Value::String(content.into())
        ))
        .unwrap()
    }

    fn assistant_line(blocks: Vec<Value>) -> String {
        format!(
            r#"{{"parentUuid":"p","type":"assistant","message":{{"role":"assistant","content":{}}},"uuid":"u"}}"#,
            Value::Array(blocks)
        )
    }

    fn user_line(blocks: Vec<Value>) -> String {
        format!(
            r#"{{"parentUuid":"p","type":"user","message":{{"role":"user","content":{}}},"uuid":"u"}}"#,
            Value::Array(blocks)
        )
    }

    #[test]
    fn a_decline_is_found_whichever_side_of_type_the_tool_use_id_is_written() {
        let dir = TempDir::new();
        for (label, result) in [
            ("last", id_last("toolu_order", DECLINE, true)),
            ("first", id_first("toolu_order", DECLINE, true)),
        ] {
            let path = write(
                &dir,
                &format!("order-{label}.jsonl"),
                &format!(
                    "{}\n{}\n",
                    assistant_line(vec![real_use("toolu_order", &format!("{REPO}/src/auth/session.ts"))]),
                    user_line(vec![result])
                ),
            );
            assert_eq!(
                sweep(&path, 0).rejections,
                [rejection("toolu_order", Some("auth"))],
                "{label}"
            );
        }
    }

    #[test]
    fn a_tool_call_and_its_decline_on_one_line_still_pair_up() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "same-line.jsonl",
            &format!(
                "{}\n",
                user_line(vec![
                    real_use("toolu_same", &format!("{REPO}/src/auth/login.ts")),
                    id_last("toolu_same", DECLINE, true)
                ])
            ),
        );
        assert_eq!(sweep(&path, 0).rejections, [rejection("toolu_same", Some("auth"))]);
    }

    #[test]
    fn a_text_block_after_the_tool_call_does_not_swallow_its_id_or_path() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "text-after.jsonl",
            &format!(
                "{}\n{}\n",
                assistant_line(vec![
                    real_use("toolu_after", &format!("{REPO}/src/app.test.ts")),
                    json!({ "type": "text", "text": "now I will run the tests" })
                ]),
                user_line(vec![id_last("toolu_after", DECLINE, true)])
            ),
        );
        assert_eq!(sweep(&path, 0).rejections, [rejection("toolu_after", Some("tests"))]);
    }

    #[test]
    fn an_escaped_quote_a_backslash_and_non_ascii_in_the_path_all_classify() {
        let dir = TempDir::new();
        for (name, p) in [
            ("quote", format!("{REPO}/src/we\"ird/thing.test.ts")),
            ("backslash", format!("{REPO}/src/we\\ird/thing.test.ts")),
            (
                "unicode",
                format!("{REPO}/src/\u{6d4b}\u{8bd5}/\u{441}\u{43f}\u{435}\u{446}/thing.test.ts"),
            ),
        ] {
            let id = format!("toolu_{name}");
            let path = write(
                &dir,
                &format!("escape-{name}.jsonl"),
                &format!(
                    "{}\n{}\n",
                    assistant_line(vec![real_use(&id, &p)]),
                    user_line(vec![id_last(&id, DECLINE, true)])
                ),
            );
            assert_eq!(sweep(&path, 0).rejections, [rejection(&id, Some("tests"))], "{name}");
        }
    }

    #[test]
    fn a_several_hundred_kb_tool_result_does_not_hide_the_decline_after_it() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "huge-result.jsonl",
            &format!(
                "{}\n{}\n{}\n",
                assistant_line(vec![real_use("toolu_huge", &format!("{REPO}/src/auth/token.ts"))]),
                user_line(vec![id_first("toolu_bulk", &"x".repeat(400_000), false)]),
                user_line(vec![id_last("toolu_huge", DECLINE, true)])
            ),
        );
        assert_eq!(sweep(&path, 0).rejections, [rejection("toolu_huge", Some("auth"))]);
    }

    #[test]
    fn two_declines_on_one_line_are_two_rejections() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            "two.jsonl",
            &format!(
                "{}\n{}\n",
                assistant_line(vec![
                    real_use("toolu_x", &format!("{REPO}/src/auth/x.ts")),
                    real_use("toolu_y", &format!("{REPO}/README.md"))
                ]),
                user_line(vec![
                    id_last("toolu_x", DECLINE, true),
                    id_last("toolu_y", DECLINE, true)
                ])
            ),
        );
        assert_eq!(
            sweep(&path, 0).rejections,
            [rejection("toolu_x", Some("auth")), rejection("toolu_y", Some("docs"))]
        );
    }

    #[test]
    fn a_tool_result_that_only_quotes_the_decline_sentence_is_not_a_rejection() {
        let dir = TempDir::new();
        let quoted = "src/decline.ts:12:  'want to proceed with this tool use',\nsrc/decline.ts:16:  'user rejected',";
        for (label, result) in [
            ("last", id_last("toolu_quote", quoted, false)),
            ("first", id_first("toolu_quote", quoted, false)),
        ] {
            let path = write(
                &dir,
                &format!("quoted-{label}.jsonl"),
                &format!(
                    "{}\n{}\n",
                    assistant_line(vec![real_use("toolu_quote", &format!("{REPO}/src/decline.ts"))]),
                    user_line(vec![result])
                ),
            );
            assert!(sweep(&path, 0).rejections.is_empty(), "{label}");
        }
    }

    #[test]
    fn a_decline_is_an_error_result_and_its_content_opens_with_the_sentence() {
        let dir = TempDir::new();
        let not_error = write(
            &dir,
            "not-an-error.jsonl",
            &format!(
                "{}\n{}\n",
                assistant_line(vec![real_use("toolu_ok", &format!("{REPO}/src/auth/a.ts"))]),
                user_line(vec![id_last("toolu_ok", DECLINE, false)])
            ),
        );
        assert!(
            sweep(&not_error, 0).rejections.is_empty(),
            "is_error:false is not a decline however it reads"
        );
        let buried = write(
            &dir,
            "buried.jsonl",
            &format!(
                "{}\n{}\n",
                assistant_line(vec![real_use("toolu_buried", &format!("{REPO}/src/auth/b.ts"))]),
                user_line(vec![id_last(
                    "toolu_buried",
                    &format!("{}{DECLINE}", "-".repeat(600)),
                    true
                )])
            ),
        );
        assert!(
            sweep(&buried, 0).rejections.is_empty(),
            "600 bytes in is a quotation, not a decline"
        );
    }

    const MB8: usize = 8 * 1024 * 1024;

    // Pads `body` with one filler record so the whole string is exactly `total` bytes.
    fn pad_to(body: &str, total: usize) -> String {
        let need = total - body.len();
        let line = |n: usize| format!("{}\n", json!({ "type": "filler", "text": "f".repeat(n) }));
        let overhead = line(0).len();
        format!("{body}{}", line(need - overhead))
    }

    #[test]
    fn a_whole_record_sitting_exactly_on_the_8mb_tail_boundary_is_not_thrown_away() {
        let dir = TempDir::new();
        let tail = pad_to(
            &format!("{}\n", user_line(vec![id_last("toolu_edge", DECLINE, true)])),
            MB8,
        );
        assert_eq!(tail.len(), MB8);
        let head = format!("{}\n", "q".repeat(199_999));
        let path = write(&dir, "boundary-exact.jsonl", &format!("{head}{tail}"));
        assert_eq!(sweep(&path, 0).rejections, [rejection("toolu_edge", None)]);
    }

    #[test]
    fn a_record_the_8mb_tail_cuts_in_half_is_skipped_and_the_next_one_is_not() {
        let dir = TempDir::new();
        let half = format!("{}\n", user_line(vec![id_last("toolu_half", DECLINE, true)]));
        let rest = pad_to(
            &format!("{half}{}\n", user_line(vec![id_last("toolu_whole", DECLINE, true)])),
            MB8 + half.len() / 2,
        );
        let head = format!("{}\n", "q".repeat(199_999));
        let path = write(&dir, "boundary-partial.jsonl", &format!("{head}{rest}"));
        let into = std::fs::metadata(&path).unwrap().len() as usize - MB8 - head.len();
        assert!(
            into > 0 && into < half.len(),
            "the cut must land inside the first record, was {into}"
        );
        assert_eq!(sweep(&path, 0).rejections, [rejection("toolu_whole", None)]);
    }

    // At working scale: 400 turns, thinking blocks, a 300 KB tool result, and a
    // sentinel in every message body. Nothing decoded holds the sentinel, and every
    // value that came into existence is one bounded scalar.
    #[test]
    fn at_session_scale_only_bounded_scalars_are_ever_decoded() {
        let dir = TempDir::new();
        let secret = "SENTINEL-3c9d02-NEVER-READ";
        let mut body = String::new();
        for turn in 0..400 {
            body.push_str(&assistant_line(vec![
                json!({ "type": "thinking", "thinking": format!("{secret} weighing turn {turn}"), "signature": "sig" }),
                json!({ "type": "text", "text": format!("{secret} here is the plan. {}", "reasoning ".repeat(40)) }),
                real_use(
                    &format!("toolu_scale_{turn}"),
                    &format!("{REPO}/src/module-{turn}/thing.ts"),
                ),
            ]));
            body.push('\n');
            let repeat = if turn == 7 { 8000 } else { 30 };
            body.push_str(&user_line(vec![id_first(
                &format!("toolu_scale_{turn}"),
                &format!("{secret} {}", "file contents that must never be read ".repeat(repeat)),
                false,
            )]));
            body.push('\n');
        }
        body.push_str(&assistant_line(vec![real_use(
            "toolu_scale_declined",
            &format!("{REPO}/src/auth/session.ts"),
        )]));
        body.push('\n');
        body.push_str(&user_line(vec![
            json!({ "type": "text", "text": format!("{secret} no thanks") }),
            id_last("toolu_scale_declined", DECLINE, true),
        ]));
        body.push('\n');
        let path = write(&dir, "at-scale.jsonl", &body);
        assert!(
            body.len() > 1_000_000,
            "the fixture has to be big enough to be a real witness"
        );

        let (result, values) = materialised_during(|| sweep(&path, 0));
        assert_eq!(result.rejections, [rejection("toolu_scale_declined", Some("auth"))]);
        assert!(
            !values.is_empty(),
            "a sweep that decoded nothing is not evidence of anything"
        );
        assert!(
            values.iter().all(|v| !v.contains(secret)),
            "message text was materialised"
        );
        let unbounded: Vec<usize> = values
            .iter()
            .filter(|v| v.contains('\n') || v.len() > 4096)
            .map(String::len)
            .collect();
        assert!(
            unbounded.is_empty(),
            "every value must be one bounded scalar, never a line or a record"
        );
    }
}
