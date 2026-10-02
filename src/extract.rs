// Local extraction. This is the point of the whole product.
//
// Everything the host hands a hook arrives here: the prompt-shaped fields, the
// tool arguments, the tool result. What leaves this module by default is a
// `ToolFacts`: a handful of booleans, one opaque id and short class labels. There
// is no field on `ToolFacts` that can hold a diff, file contents, a tool response
// body or an environment variable. That discard is structural and unconditional.
//
// Feature 0109 is the one deliberate exception: when the developer has explicitly
// opted into `rawActivityEnabled`, extraction may ALSO return exactly two bounded
// strings, the repo-relative file path and the Bash command text, each truncated
// to MAX_RAW_LENGTH.
//
// Feature 0126 adds local views (`bash_result`, `edit_texts`, `original_content`)
// that hand the test-run, weakened-test and revert detectors what they need.
// Those values live for the span of one hook call, inside hooks.rs, and only
// enums and counts derived from them are ever queued.

use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use regex_lite::Regex;
use serde_json::{Map, Value};

use crate::classify::classify_path;
use crate::decline::looks_declined;
use crate::signal::testrun::detect_test_command;
use crate::types::AgentId;

pub type Payload = Map<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolFacts {
    // Dedupe key material only. Not every host version puts a tool_use_id on
    // every payload, so the caller synthesizes a session-scoped id when absent.
    pub tool_use_id: Option<String>,
    pub kind: &'static str,
    pub is_edit: bool,
    pub is_test_command: bool,
    pub path_class: Option<String>,
    // A tool_response that says the developer declined.
    pub declined: bool,
    // Feature 0108. Six fixed values, never the raw tool name.
    pub tool_category: &'static str,
    // Feature 0108. Only meaningful when tool_category is bash.
    pub command_category: Option<&'static str>,
    // Feature 0109. Present only under the raw-activity opt-in.
    pub raw_path: Option<String>,
    pub raw_command: Option<String>,
}

// Feature 0109. A pathologically long input cannot turn the opt-in channel into
// an exfiltration vector for more than a bounded amount of text per tool call.
pub const MAX_RAW_LENGTH: usize = 500;

const EDIT_TOOLS: &[&str] = &[
    "edit",
    "write",
    "multiedit",
    "notebookedit",
    "applypatch",
    "apply_patch",
    "update",
    "search_replace", // Grok's name for Edit / Write / MultiEdit
];
const SUBAGENT_TOOLS: &[&str] = &["task", "agent", "spawn_subagent"];
const READ_TOOLS: &[&str] = &["read", "read_file", "cat", "view"];
const BASH_TOOLS: &[&str] = &["bash", "shell", "terminal", "run_terminal_cmd", "run_terminal_command"];
const SEARCH_TOOLS: &[&str] = &[
    "grep",
    "glob",
    "search",
    "find",
    "search_files",
    "codebase_search",
    "ls",
    "list_dir",
];
const WEB_TOOLS: &[&str] = &["webfetch", "web_fetch", "websearch", "web_search", "browse"];

fn classify_tool(lower: &str, is_edit: bool) -> &'static str {
    if is_edit {
        "edit"
    } else if BASH_TOOLS.contains(&lower) {
        "bash"
    } else if READ_TOOLS.contains(&lower) {
        "read"
    } else if SEARCH_TOOLS.contains(&lower) {
        "search"
    } else if WEB_TOOLS.contains(&lower) {
        "web"
    } else {
        "other"
    }
}

// Deliberately conservative, and kept from the TS client so commandCategory and
// testsRun mean what they meant. A false positive marks an accept as checked when
// it was not, which inflates Diligence. Feature 0126's segment-aware detector is
// OR'd in, which only adds real runner invocations this list missed (./mvnw
// test, playwright test, cypress run).
static TEST_COMMAND: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        &[
            r"\b(npm|pnpm|yarn|bun)\s+(run\s+)?tests?\b",
            r"\bnpx?\s+(jest|vitest|mocha|ava|playwright|cypress)\b",
            r"\b(jest|vitest|pytest|tox|rspec|phpunit)\b",
            r"\bgo\s+test\b",
            r"\bcargo\s+test\b",
            r"\bmvn\s+(\S+\s+)*test\b",
            r"\bgradle(w)?\s+(\S+\s+)*test\b",
            r"\bdotnet\s+test\b",
            r"\bnode\s+--test\b",
            r"\bmake\s+test\b",
        ]
        .map(|p| format!("(?i:{p})"))
        .join("|"),
    )
    .expect("static regex")
});

pub fn is_test_command(command: &str) -> bool {
    TEST_COMMAND.is_match(command) || detect_test_command(command).is_some()
}

pub fn tool_name(payload: &Payload) -> String {
    first_string(payload, &["tool_name", "toolName"])
        .unwrap_or_default()
        .to_string()
}

pub fn tool_input(payload: &Payload) -> &Map<String, Value> {
    static EMPTY: LazyLock<Map<String, Value>> = LazyLock::new(Map::new);
    payload
        .get("tool_input")
        .and_then(Value::as_object)
        .or_else(|| payload.get("toolInput").and_then(Value::as_object))
        .unwrap_or(&EMPTY)
}

/// The tool result under any of the names the hosts use: Claude Code's
/// `tool_response`, Grok's `toolResult` (with a `tool_response` alias), and the
/// SDK's `tool_result`.
pub fn tool_response(payload: &Payload) -> Option<&Value> {
    ["tool_response", "toolResult", "toolResponse", "tool_result"]
        .iter()
        .find_map(|key| payload.get(*key).filter(|v| !v.is_null()))
}

pub fn file_path_in(input: &Map<String, Value>) -> Option<&str> {
    first_string(
        input,
        &["file_path", "notebook_path", "path", "filePath", "target_file"],
    )
}

pub fn command_in(input: &Map<String, Value>) -> Option<&str> {
    first_string(input, &["command", "cmd"])
}

pub fn extract_tool_facts(
    payload: &Payload,
    repo_root: Option<&str>,
    classifier: &Map<String, Value>,
    include_raw: bool,
) -> ToolFacts {
    let lower = tool_name(payload).to_lowercase();
    let input = tool_input(payload);
    let repo_relative = file_path_in(input).map(|p| to_repo_relative(p, repo_root));
    let path_class = repo_relative.as_deref().and_then(|rel| classify_path(classifier, rel));
    let command = command_in(input);
    let is_edit = EDIT_TOOLS.contains(&lower.as_str());
    let tool_category = classify_tool(&lower, is_edit);
    let is_test = command.is_some_and(is_test_command);
    ToolFacts {
        tool_use_id: first_string(payload, &["tool_use_id", "toolUseId"]).map(str::to_string),
        kind: if SUBAGENT_TOOLS.contains(&lower.as_str()) {
            "subagent"
        } else {
            "tool-use"
        },
        is_edit,
        is_test_command: is_test,
        path_class,
        declined: looks_declined(tool_response(payload)),
        tool_category,
        command_category: (tool_category == "bash").then_some(if is_test { "test" } else { "other" }),
        raw_path: repo_relative
            .filter(|rel| include_raw && !rel.is_empty())
            .map(|rel| truncate(&rel, MAX_RAW_LENGTH)),
        raw_command: command
            .filter(|_| include_raw && tool_category == "bash")
            .map(|c| truncate(c, MAX_RAW_LENGTH)),
    }
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

/// A path outside the repository classifies as nothing. The alternative, feeding
/// an absolute path into a classifier whose patterns are repo-relative, produces
/// confident nonsense: `/Users/someone/auth-notes.md` would score as `auth`.
pub fn to_repo_relative(path: &str, repo_root: Option<&str>) -> String {
    let p = Path::new(path);
    let absolute = p.has_root() || is_windows_absolute(path);
    let Some(root) = repo_root else {
        return if absolute { String::new() } else { path.to_string() };
    };
    if !absolute {
        return path.to_string();
    }
    let normal = normalize(p);
    match normal.strip_prefix(normalize(Path::new(root))) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => String::new(),
    }
}

/// Resolves a path as a shell would from `cwd`, then makes it repo-relative.
/// Empty when it lands outside the repository. `.` at the root is the root.
pub fn resolve_from(cwd: &Path, path: &str, repo_root: Option<&str>) -> Option<String> {
    let root = repo_root?;
    let joined = if Path::new(path).has_root() || is_windows_absolute(path) {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let normal = normalize(&joined);
    let rel = normal.strip_prefix(normalize(Path::new(root))).ok()?;
    Some(
        rel.to_string_lossy()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_string(),
    )
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn is_windows_absolute(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn first_string<'a>(source: &'a Map<String, Value>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| source.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
}

// ---- feature 0126: local views, never queued ----

/// What a Bash call's result says about how it ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BashResult {
    pub exit_code: Option<i32>,
    pub interrupted: bool,
    // Started in the background: it has not finished, so it has no outcome yet.
    pub background: bool,
    // The output, for the runner summary parser only.
    pub output: String,
}

static EXIT_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*(?:error:\s*)?exit code\s+(-?\d+)").expect("static regex"));

/// How each host reports a command's exit status, established by driving them
/// (recorded in the 0126 feature file):
///
/// - Claude Code 2.1.287: a zero exit fires PostToolUse, whose `tool_response`
///   is `{stdout, stderr, interrupted, isImage, noOutputExpected}` with no exit
///   code at all. A non-zero exit does NOT fire PostToolUse; it fires
///   PostToolUseFailure with `error: "Exit code 3\n<stdout>\n<stderr>"` and
///   `is_interrupt`. So under Claude Code a PostToolUse for Bash means exit 0.
/// - Grok 1.0.34: a non-zero `run_terminal_command` exit still fires PostToolUse,
///   and `toolResult` is a tagged object carrying `exit_code`. An oversized result
///   arrives as a plain string (`toolResultTruncated`), with no exit code.
///   PostToolUseFailure is for a tool that failed to dispatch.
pub fn bash_result(payload: &Payload, failure_event: bool, agent: AgentId) -> BashResult {
    if failure_event {
        let error = first_string(payload, &["error", "errorMessage"]).unwrap_or("");
        return BashResult {
            exit_code: EXIT_CODE
                .captures(error)
                .and_then(|c| c.get(1))
                .and_then(|m| m.as_str().parse().ok()),
            interrupted: payload
                .get("is_interrupt")
                .or_else(|| payload.get("isInterrupt"))
                .and_then(Value::as_bool)
                == Some(true),
            background: false,
            output: error.to_string(),
        };
    }
    let Some(response) = tool_response(payload) else {
        return BashResult::default();
    };
    let Some(object) = response.as_object() else {
        return BashResult {
            output: response.as_str().unwrap_or("").to_string(),
            ..BashResult::default()
        };
    };
    let exit_code = ["exit_code", "exitCode", "returnCode", "return_code", "code"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_i64))
        .map(|n| n as i32);
    let interrupted = object.get("interrupted").and_then(Value::as_bool) == Some(true);
    let background = ["backgroundTaskId", "background_task_id", "backgroundTaskID"]
        .iter()
        .any(|key| object.get(*key).is_some_and(|v| !v.is_null()));
    let mut output = String::new();
    for key in [
        "stdout",
        "stderr",
        "output",
        "output_for_prompt",
        "outputForPrompt",
        "content",
        "result",
    ] {
        if let Some(text) = object.get(key).and_then(Value::as_str) {
            output.push_str(text);
            output.push('\n');
        }
    }
    // The Claude Code fact above: a PostToolUse in its Bash shape is a zero exit.
    let claude_shape = agent == AgentId::ClaudeCode && object.contains_key("stdout");
    BashResult {
        exit_code: exit_code.or((claude_shape && !interrupted && !background).then_some(0)),
        interrupted,
        background,
        output,
    }
}

/// The text an edit replaced and the text it wrote, when the payload says both.
/// None when the old text is unknown, because judging "an assertion was removed"
/// against an unknown original would be a guess.
pub fn edit_texts(payload: &Payload) -> Option<(String, String)> {
    let input = tool_input(payload);
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        let mut old = String::new();
        let mut new = String::new();
        for edit in edits {
            old.push_str(edit.get("old_string").and_then(Value::as_str).unwrap_or(""));
            old.push('\n');
            new.push_str(edit.get("new_string").and_then(Value::as_str).unwrap_or(""));
            new.push('\n');
        }
        return Some((old, new));
    }
    let text = |keys: &[&str]| keys.iter().find_map(|k| input.get(*k).and_then(Value::as_str));
    if let (Some(old), Some(new)) = (
        text(&["old_string", "oldString", "old_str"]),
        text(&["new_string", "newString", "new_str"]),
    ) {
        return Some((old.to_string(), new.to_string()));
    }
    if let Some(patch) = text(&["patch", "input", "diff"])
        && (patch.contains("\n+") || patch.contains("\n-") || patch.starts_with("*** Begin Patch"))
    {
        return Some(split_patch(patch));
    }
    // An empty write still counts: it is how a whole test file gets emptied.
    if let Some(content) = text(&["content", "file_text", "contents"]) {
        // A whole-file write: the old text is the file before it, which only the
        // host can say. Claude Code puts it in `tool_response.originalFile`.
        return match original_content(payload) {
            Some(Some(original)) => Some((original, content.to_string())),
            Some(None) => Some((String::new(), content.to_string())),
            None => None,
        };
    }
    None
}

/// The file before this edit, from the host's result. `Some(None)` is a file the
/// edit created, `None` is "the host did not say".
pub fn original_content(payload: &Payload) -> Option<Option<String>> {
    let response = tool_response(payload)?.as_object()?;
    match response.get("originalFile").or_else(|| response.get("original_file")) {
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(Value::Null) => Some(None),
        _ => None,
    }
}

fn split_patch(patch: &str) -> (String, String) {
    let mut old = String::new();
    let mut new = String::new();
    for line in patch.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if let Some(rest) = line.strip_prefix('-') {
            old.push_str(rest);
            old.push('\n');
        } else if let Some(rest) = line.strip_prefix('+') {
            new.push_str(rest);
            new.push('\n');
        }
    }
    (old, new)
}

pub fn duration_ms(payload: &Payload) -> Option<i64> {
    ["duration_ms", "durationMs"]
        .iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_f64))
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| n as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::test_classifier;
    use serde_json::json;

    fn facts(payload: Value, include_raw: bool) -> ToolFacts {
        extract_tool_facts(
            payload.as_object().unwrap(),
            Some("/repo"),
            &test_classifier(),
            include_raw,
        )
    }

    #[test]
    fn treats_a_grok_search_replace_payload_as_an_edit() {
        let f = facts(
            json!({
                "sessionId": "grok-session", "cwd": "/repo", "hookEventName": "post_tool_use",
                "toolName": "search_replace", "toolUseId": "tu-1",
                "toolInput": { "path": "src/auth/session.ts", "old_string": "a", "new_string": "b" },
                "toolResult": { "ok": true },
            }),
            false,
        );
        assert!(f.is_edit);
        assert_eq!(f.kind, "tool-use");
        assert_eq!(f.path_class.as_deref(), Some("auth"));
        assert_eq!(f.tool_use_id.as_deref(), Some("tu-1"));
        assert!(!f.declined);
        assert_eq!(f.tool_category, "edit");
        assert_eq!(f.command_category, None);
    }

    #[test]
    fn treats_spawn_subagent_and_task_as_subagents() {
        let f = facts(
            json!({ "toolName": "spawn_subagent", "toolInput": { "description": "explore" } }),
            false,
        );
        assert_eq!(f.kind, "subagent");
        assert!(!f.is_edit);
        assert_eq!(f.tool_category, "other");
        assert_eq!(
            facts(json!({ "tool_name": "Task", "tool_input": {} }), false).kind,
            "subagent"
        );
    }

    #[test]
    fn reads_a_test_command_from_grok_run_terminal_command() {
        let f = facts(
            json!({ "toolName": "run_terminal_command", "toolInput": { "command": "npm test" } }),
            false,
        );
        assert!(f.is_test_command);
        assert_eq!(f.tool_category, "bash");
        assert_eq!(f.command_category, Some("test"));
    }

    #[test]
    fn classifies_tools_into_the_six_buckets() {
        assert_eq!(
            facts(
                json!({ "toolName": "Bash", "toolInput": { "command": "git status" } }),
                false
            )
            .command_category,
            Some("other")
        );
        assert_eq!(
            facts(
                json!({ "toolName": "Read", "toolInput": { "file_path": "/repo/src/index.ts" } }),
                false
            )
            .tool_category,
            "read"
        );
        assert_eq!(
            facts(json!({ "toolName": "Grep", "toolInput": { "pattern": "foo" } }), false).tool_category,
            "search"
        );
        assert_eq!(
            facts(
                json!({ "toolName": "Glob", "toolInput": { "pattern": "**/*.ts" } }),
                false
            )
            .tool_category,
            "search"
        );
        assert_eq!(
            facts(
                json!({ "toolName": "WebFetch", "toolInput": { "url": "https://x" } }),
                false
            )
            .tool_category,
            "web"
        );
        assert_eq!(
            facts(json!({ "toolName": "mcp__server__do", "toolInput": {} }), false).tool_category,
            "other"
        );
    }

    #[test]
    fn never_returns_raw_path_or_command_without_the_opt_in() {
        let edit = facts(
            json!({ "toolName": "Edit", "toolInput": { "file_path": "/repo/src/auth/session.ts" } }),
            false,
        );
        let bash = facts(
            json!({ "toolName": "Bash", "toolInput": { "command": "git checkout my-branch" } }),
            false,
        );
        assert_eq!((edit.raw_path, edit.raw_command), (None, None));
        assert_eq!((bash.raw_path, bash.raw_command), (None, None));
    }

    #[test]
    fn returns_the_real_repo_relative_path_and_bash_text_with_the_opt_in() {
        let edit = facts(
            json!({ "toolName": "Edit", "toolInput": { "file_path": "/repo/src/auth/session.ts" } }),
            true,
        );
        assert_eq!(edit.raw_path.as_deref(), Some("src/auth/session.ts"));
        assert_eq!(edit.raw_command, None);
        let bash = facts(
            json!({ "toolName": "Bash", "toolInput": { "command": "git checkout my-branch" } }),
            true,
        );
        assert_eq!(bash.raw_command.as_deref(), Some("git checkout my-branch"));
        let read = facts(
            json!({ "toolName": "Read", "toolInput": { "file_path": "/repo/README.md" } }),
            true,
        );
        assert_eq!(read.raw_command, None);
    }

    #[test]
    fn truncates_raw_text_at_500_and_never_returns_a_path_outside_the_repository() {
        let long = facts(
            json!({ "toolName": "Bash", "toolInput": { "command": format!("echo {}", "x".repeat(600)) } }),
            true,
        );
        assert_eq!(long.raw_command.unwrap().len(), 500);
        let outside = facts(
            json!({ "toolName": "Read", "toolInput": { "file_path": "/Users/someone/private/notes.md" } }),
            true,
        );
        assert_eq!(outside.raw_path, None);
    }

    #[test]
    fn claude_code_bash_results_read_as_exit_zero_and_failures_carry_the_code() {
        let ok =
            json!({ "tool_response": { "stdout": "hello", "stderr": "", "interrupted": false, "isImage": false } });
        let r = bash_result(ok.as_object().unwrap(), false, AgentId::ClaudeCode);
        assert_eq!(r.exit_code, Some(0));
        // The same shape under Grok says nothing about the exit code.
        assert_eq!(
            bash_result(ok.as_object().unwrap(), false, AgentId::GrokBuild).exit_code,
            None
        );

        let failed = json!({ "error": "Exit code 3\nsome-out\nsome-err", "is_interrupt": false });
        let r = bash_result(failed.as_object().unwrap(), true, AgentId::ClaudeCode);
        assert_eq!(r.exit_code, Some(3));
        assert!(r.output.contains("some-err"));

        let background = json!({ "tool_response": { "stdout": "", "backgroundTaskId": "b1" } });
        assert_eq!(
            bash_result(background.as_object().unwrap(), false, AgentId::ClaudeCode).exit_code,
            None
        );
        let interrupted = json!({ "tool_response": { "stdout": "", "interrupted": true } });
        assert_eq!(
            bash_result(interrupted.as_object().unwrap(), false, AgentId::ClaudeCode).exit_code,
            None
        );
    }

    #[test]
    fn grok_results_carry_exit_code_and_a_truncated_result_has_none() {
        let tagged = json!({ "toolResult": { "type": "Bash", "command": "npm test", "exit_code": 1, "output_for_prompt": "Tests: 1 failed" } });
        let r = bash_result(tagged.as_object().unwrap(), false, AgentId::GrokBuild);
        assert_eq!(r.exit_code, Some(1));
        assert!(r.output.contains("1 failed"));
        let truncated = json!({ "toolResult": "Tests: 2 passed, 2 total", "toolResultTruncated": true });
        let r = bash_result(truncated.as_object().unwrap(), false, AgentId::GrokBuild);
        assert_eq!(r.exit_code, None);
        assert!(r.output.contains("2 passed"));
    }

    #[test]
    fn edit_texts_cover_edit_multiedit_write_and_patches() {
        let edit = json!({ "tool_input": { "old_string": "a", "new_string": "b" } });
        assert_eq!(edit_texts(edit.as_object().unwrap()), Some(("a".into(), "b".into())));
        let multi = json!({ "tool_input": { "edits": [{ "old_string": "a", "new_string": "b" }, { "old_string": "c", "new_string": "d" }] } });
        assert_eq!(
            edit_texts(multi.as_object().unwrap()),
            Some(("a\nc\n".into(), "b\nd\n".into()))
        );
        let write = json!({ "tool_input": { "content": "new" }, "tool_response": { "originalFile": "old" } });
        assert_eq!(
            edit_texts(write.as_object().unwrap()),
            Some(("old".into(), "new".into()))
        );
        let create = json!({ "tool_input": { "content": "new" }, "tool_response": { "originalFile": null } });
        assert_eq!(edit_texts(create.as_object().unwrap()), Some(("".into(), "new".into())));
        // A whole-file write with no original is not judged at all.
        let unknown = json!({ "tool_input": { "content": "new" } });
        assert_eq!(edit_texts(unknown.as_object().unwrap()), None);
        let patch = json!({ "tool_input": { "patch": "--- a/x\n+++ b/x\n@@\n-old line\n+new line\n" } });
        assert_eq!(
            edit_texts(patch.as_object().unwrap()),
            Some(("old line\n".into(), "new line\n".into()))
        );
    }

    #[test]
    fn resolves_shell_paths_against_cwd_inside_the_repository_only() {
        let cwd = Path::new("/repo/src");
        assert_eq!(
            resolve_from(cwd, "auth/x.ts", Some("/repo")).as_deref(),
            Some("src/auth/x.ts")
        );
        assert_eq!(
            resolve_from(cwd, "../README.md", Some("/repo")).as_deref(),
            Some("README.md")
        );
        assert_eq!(
            resolve_from(Path::new("/repo"), ".", Some("/repo")).as_deref(),
            Some("")
        );
        assert_eq!(resolve_from(cwd, "/etc/passwd", Some("/repo")), None);
    }
}
