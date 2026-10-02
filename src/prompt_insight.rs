// Feature 0094. Reads real prompt and reply text from the transcript, for a
// developer who has opted into prompt insight scoring or live feedback (or whose
// org enforces it on). Gated entirely by the caller (hooks.rs checks the session's
// opt-in flags before this is ever called): nothing here decides whether to run.
//
// This is deliberately NOT a change to sweep_transcript (transcript.rs, design
// decision 57). That function never decodes a line, and it stays that way for
// every developer who has not opted in. This is a separate function with a
// separate budget: decoding the two things a developer consented to send.
//
// A user turn only counts as a real prompt when its content holds no tool_result
// block. A tool result comes back to the model as a `role: "user"` message too,
// and counting one would send the agent's own tool output, not the developer's
// words.

use std::path::Path;

use serde_json::Value;

const MAX_READ_BYTES: u64 = 8 * 1024 * 1024;

// The backend's InsightSubmissionDto and LiveFeedbackSubmissionDto cap prompt and
// response at 20000 characters each, and a longer one is a 400 for the whole
// submission. Truncating here keeps a long turn scoreable.
pub const MAX_TEXT_CHARS: usize = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInsightTurn {
    pub prompt: String,
    pub response: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptInsightSweep {
    pub turns: Vec<PromptInsightTurn>,
    // Lines already scanned, not bytes.
    pub line_offset: usize,
}

pub fn sweep_prompt_insight_turns(transcript: &Path, line_offset: usize) -> PromptInsightSweep {
    let Ok(meta) = std::fs::metadata(transcript) else {
        return PromptInsightSweep {
            turns: Vec::new(),
            line_offset,
        };
    };
    let Ok(raw) = std::fs::read_to_string(transcript) else {
        return PromptInsightSweep {
            turns: Vec::new(),
            line_offset,
        };
    };
    if meta.len() > MAX_READ_BYTES {
        // Not worth reading whole on a decode-everything path. Skip to the end; the
        // next Stop picks up from here with nothing to catch up on.
        return PromptInsightSweep {
            turns: Vec::new(),
            line_offset: raw.split('\n').count(),
        };
    }

    let lines: Vec<&str> = raw.split('\n').filter(|line| !line.is_empty()).collect();
    let start = if line_offset > lines.len() { 0 } else { line_offset };
    let mut turns = Vec::new();
    let mut pending_prompt: Option<String> = None;
    let mut pending_response: Vec<String> = Vec::new();

    let flush = |prompt: &mut Option<String>, parts: &mut Vec<String>, turns: &mut Vec<PromptInsightTurn>| {
        if let Some(prompt) = prompt.take() {
            let response = parts.join("\n").trim().to_string();
            // A prompt whose turn produced no assistant text is simply never scored.
            if !response.is_empty() {
                turns.push(PromptInsightTurn {
                    prompt: cap(&prompt),
                    response: cap(&response),
                });
            }
        }
        parts.clear();
    };

    for line in &lines[start..] {
        let Ok(parsed) = serde_json::from_str::<Value>(line) else {
            continue; // a torn final line from a still-being-written transcript
        };
        match parsed.get("type").and_then(Value::as_str) {
            Some("user") => {
                let Some(prompt) = real_user_prompt(&parsed) else {
                    continue;
                };
                flush(&mut pending_prompt, &mut pending_response, &mut turns);
                pending_prompt = Some(prompt);
            }
            Some("assistant") if pending_prompt.is_some() => {
                if let Some(text) = text_of(&parsed) {
                    pending_response.push(text);
                }
            }
            _ => {}
        }
    }
    flush(&mut pending_prompt, &mut pending_response, &mut turns);
    PromptInsightSweep {
        turns,
        line_offset: lines.len(),
    }
}

fn cap(text: &str) -> String {
    text.chars().take(MAX_TEXT_CHARS).collect()
}

fn content(line: &Value) -> Option<&Value> {
    line.get("message")?.get("content")
}

fn real_user_prompt(line: &Value) -> Option<String> {
    let content = content(line)?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    let blocks = content.as_array()?;
    if blocks
        .iter()
        .any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
    {
        return None;
    }
    joined_text(blocks)
}

fn text_of(line: &Value) -> Option<String> {
    let content = content(line)?;
    if let Some(text) = content.as_str() {
        let trimmed = text.trim();
        return (!trimmed.is_empty()).then(|| trimmed.to_string());
    }
    joined_text(content.as_array()?)
}

fn joined_text(blocks: &[Value]) -> Option<String> {
    let text = blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;
    use serde_json::json;

    fn write(dir: &TempDir, lines: &[Value]) -> std::path::PathBuf {
        let path = dir.path().join("transcript.jsonl");
        std::fs::write(&path, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
        path
    }

    fn user_text(text: &str) -> Value {
        json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } })
    }

    fn assistant_text(text: &str) -> Value {
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } })
    }

    fn tool_result_turn(id: &str) -> Value {
        json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": id, "is_error": false, "content": "ok" }] } })
    }

    fn assistant_tool_use(id: &str) -> Value {
        json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "tool_use", "id": id, "name": "Edit", "input": { "file_path": "x" } }] } })
    }

    fn turn(prompt: &str, response: &str) -> PromptInsightTurn {
        PromptInsightTurn {
            prompt: prompt.into(),
            response: response.into(),
        }
    }

    #[test]
    fn pairs_a_real_prompt_with_the_assistant_text_that_follows_it() {
        let dir = TempDir::new();
        let sweep = sweep_prompt_insight_turns(
            &write(
                &dir,
                &[user_text("fix the login bug"), assistant_text("Fixed it in auth.ts")],
            ),
            0,
        );
        assert_eq!(sweep.turns, [turn("fix the login bug", "Fixed it in auth.ts")]);
        assert_eq!(sweep.line_offset, 2);
    }

    #[test]
    fn accepts_content_as_a_plain_string() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            &[
                json!({ "type": "user", "message": { "role": "user", "content": "fix the login bug" } }),
                assistant_text("done"),
            ],
        );
        assert_eq!(
            sweep_prompt_insight_turns(&path, 0).turns,
            [turn("fix the login bug", "done")]
        );
    }

    #[test]
    fn a_tool_result_feedback_turn_is_never_counted_as_a_prompt() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            &[
                user_text("refactor the billing module"),
                assistant_tool_use("toolu_1"),
                tool_result_turn("toolu_1"),
                assistant_text("Refactored billing.ts, all set"),
            ],
        );
        assert_eq!(
            sweep_prompt_insight_turns(&path, 0).turns,
            [turn("refactor the billing module", "Refactored billing.ts, all set")]
        );
    }

    #[test]
    fn joins_multiple_assistant_text_blocks_across_a_multi_step_turn() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            &[
                user_text("add tests for the auth module"),
                assistant_text("Let me look at the existing tests first."),
                assistant_tool_use("toolu_1"),
                tool_result_turn("toolu_1"),
                assistant_text("Added three cases covering the token refresh path."),
            ],
        );
        assert_eq!(
            sweep_prompt_insight_turns(&path, 0).turns,
            [turn(
                "add tests for the auth module",
                "Let me look at the existing tests first.\nAdded three cases covering the token refresh path."
            )]
        );
    }

    #[test]
    fn a_prompt_with_no_assistant_text_yields_nothing_but_still_advances() {
        let dir = TempDir::new();
        let sweep = sweep_prompt_insight_turns(
            &write(&dir, &[user_text("just tool calls"), assistant_tool_use("toolu_1")]),
            0,
        );
        assert!(sweep.turns.is_empty());
        assert_eq!(sweep.line_offset, 2);
    }

    #[test]
    fn the_offset_skips_lines_already_swept() {
        let dir = TempDir::new();
        let path = write(&dir, &[user_text("first prompt"), assistant_text("first reply")]);
        let first = sweep_prompt_insight_turns(&path, 0);
        assert_eq!(first.turns.len(), 1);
        assert!(sweep_prompt_insight_turns(&path, first.line_offset).turns.is_empty());
    }

    #[test]
    fn a_missing_transcript_sweeps_to_nothing() {
        assert_eq!(
            sweep_prompt_insight_turns(Path::new("/does/not/exist.jsonl"), 0),
            PromptInsightSweep {
                turns: Vec::new(),
                line_offset: 0
            }
        );
    }

    #[test]
    fn an_unparseable_line_is_skipped() {
        let dir = TempDir::new();
        let path = dir.path().join("t.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\nnot json at all\n{}\n",
                user_text("a real prompt"),
                assistant_text("a real reply")
            ),
        )
        .unwrap();
        assert_eq!(
            sweep_prompt_insight_turns(&path, 0).turns,
            [turn("a real prompt", "a real reply")]
        );
    }

    #[test]
    fn two_prompts_produce_two_turns_in_order() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            &[
                user_text("first task"),
                assistant_text("did the first thing"),
                user_text("second task"),
                assistant_text("did the second thing"),
            ],
        );
        assert_eq!(
            sweep_prompt_insight_turns(&path, 0).turns,
            [
                turn("first task", "did the first thing"),
                turn("second task", "did the second thing")
            ]
        );
    }

    #[test]
    fn text_past_the_backend_cap_is_truncated_rather_than_rejected_whole() {
        let dir = TempDir::new();
        let path = write(
            &dir,
            &[user_text(&"p".repeat(25_000)), assistant_text(&"r".repeat(30_000))],
        );
        let sweep = sweep_prompt_insight_turns(&path, 0);
        assert_eq!(sweep.turns[0].prompt.chars().count(), MAX_TEXT_CHARS);
        assert_eq!(sweep.turns[0].response.chars().count(), MAX_TEXT_CHARS);
    }
}
