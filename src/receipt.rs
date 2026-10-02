// The local half of design decision 44. `GET /dry-run` is the server-rendered
// receipt and stays authoritative; this is the copy on the developer's own disk,
// which is what makes `flueny dry-run --today` answerable with the network down
// and without asking Flueny what Flueny received.
//
// `fieldsSent` is generated from the serialized event, never written by hand. A
// hand-written list is a description of the payload that drifts from the payload.

use serde_json::{Map, Value};

use crate::copy::{daily_receipt, plural};
use crate::store::{LedgerCounters, LedgerEntry, Store};
use crate::time::today;
use crate::wire::to_wire_event;

fn str_of<'a>(event: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

fn int_of(event: &Map<String, Value>, key: &str) -> Option<i64> {
    event.get(key).and_then(Value::as_i64)
}

pub fn summary_for(event: &Map<String, Value>) -> String {
    let place = match str_of(event, "pathClass") {
        Some(class) => format!("path class {class}"),
        None => "no classified path".to_string(),
    };
    match str_of(event, "kind").unwrap_or("") {
        "edit-decision" => {
            // `testsRun` is absent on a rejection, because nothing was applied for a
            // test to check. Rendering absent as "tests did not run" would put a
            // claim in the receipt that the payload beside it does not make.
            let tests = match event.get("testsRun").and_then(Value::as_bool) {
                Some(true) => ", tests ran",
                Some(false) => ", tests did not run",
                None => "",
            };
            format!(
                "Agent edit {}, {place}{tests}",
                str_of(event, "decision").unwrap_or("recorded")
            )
        }
        "subagent" => "Subagent delegated".to_string(),
        "session-end" => format!(
            "Session ended after {} min, {} subagents",
            (int_of(event, "durationMs").unwrap_or(0) as f64 / 60_000.0).round() as i64,
            int_of(event, "subagentCount").unwrap_or(0)
        ),
        "test-run" => {
            let mut text = format!(
                "Test run {}, runner {}",
                str_of(event, "testOutcome").unwrap_or("unknown"),
                str_of(event, "testRunner").unwrap_or("not identified")
            );
            for (key, label) in [
                ("testsPassed", "passed"),
                ("testsFailed", "failed"),
                ("testsSkipped", "skipped"),
            ] {
                if let Some(n) = int_of(event, key) {
                    text.push_str(&format!(", {n} {label}"));
                }
            }
            text
        }
        "turn-verification" => {
            let edits = int_of(event, "editsAccepted").unwrap_or(0);
            let runs = int_of(event, "testRuns").unwrap_or(0);
            let green = if event.get("endedGreen").and_then(Value::as_bool) == Some(true) {
                "ended on a passing test run"
            } else {
                "did not end on a passing test run"
            };
            let mut text = format!(
                "Turn checked: {edits} accepted {}, {runs} test {}, {green}",
                plural(edits, "edit"),
                plural(runs, "run")
            );
            let flags: Vec<&str> = event
                .get("weakenedFlags")
                .and_then(Value::as_array)
                .map(|items| items.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if !flags.is_empty() {
                text.push_str(&format!(", flagged {}", flags.join(", ")));
            }
            text
        }
        _ => format!("Agent tool call, {place}"),
    }
}

pub fn entry_for(event: &Value) -> LedgerEntry {
    let wire = to_wire_event(event);
    LedgerEntry {
        at: str_of(&wire, "at").unwrap_or("").to_string(),
        summary: summary_for(&wire),
        fields_sent: wire.keys().cloned().collect(),
        would_block: false,
    }
}

/// `observed` counts raw tool calls this client looked at. `wouldSend` counts
/// derived events. The gap between them is the privacy claim stated as
/// arithmetic, which is why they are two counters and not one.
pub fn record(store: &Store, events: &[Value], observed: i64, day: &str) {
    if observed > 0 {
        store.bump_counters(
            day,
            &LedgerCounters {
                observed,
                ..Default::default()
            },
        );
    }
    if events.is_empty() {
        return;
    }
    let entries: Vec<LedgerEntry> = events.iter().map(entry_for).collect();
    store.append_ledger(day, &entries);
    store.bump_counters(
        day,
        &LedgerCounters {
            would_send: events.len() as i64,
            ..Default::default()
        },
    );
}

pub fn record_today(store: &Store, events: &[Value], observed: i64) {
    record(store, events, observed, &today());
}

// Feature 0094, from the unmerged feat/insight-receipt-visibility change. An
// insight submission is fire and forget with no local queue, so this is its one
// local trace. The entry lists "prompt" and "response" as field NAMES: it says a
// prompt and a reply were sent, never what either one said. Only a delivered
// submission gets a row, because the ledger's whole claim is "this is what left".
pub const INSIGHT_SUMMARY: &str = "Prompt scored for insight (Description axis)";

pub fn entry_for_insight(submission: &Value) -> LedgerEntry {
    LedgerEntry {
        at: submission.get("at").and_then(Value::as_str).unwrap_or("").to_string(),
        summary: INSIGHT_SUMMARY.to_string(),
        fields_sent: submission
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
        would_block: false,
    }
}

pub fn record_insight(store: &Store, submission: &Value, delivered: bool, day: &str) {
    if delivered {
        store.append_ledger(day, &[entry_for_insight(submission)]);
        store.bump_counters(
            day,
            &LedgerCounters {
                insights_sent: 1,
                ..Default::default()
            },
        );
    } else {
        store.bump_counters(
            day,
            &LedgerCounters {
                insights_failed: 1,
                ..Default::default()
            },
        );
    }
}

pub fn receipt_for(store: &Store, day: &str) -> String {
    let counters = store.read_counters(day);
    // Not a placeholder. Nothing is enforced in M1, so blocked is a measured zero.
    daily_receipt(counters.observed, counters.would_send, counters.would_block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TestEnv;
    use serde_json::json;

    #[test]
    fn fields_sent_is_generated_from_the_serialized_event() {
        let entry = entry_for(&json!({
            "eventId": "e1", "kind": "edit-decision", "at": "2026-08-08T10:00:00.000Z",
            "repoId": "sha256:abc", "pathClass": "auth", "decision": "accepted", "testsRun": true,
        }));
        assert_eq!(
            entry.fields_sent,
            ["eventId", "kind", "at", "repoId", "pathClass", "decision", "testsRun"]
        );
        assert!(!entry.would_block);
    }

    #[test]
    fn a_field_that_is_not_sent_is_not_listed_as_sent() {
        let rejected = json!({
            "eventId": "e2", "kind": "edit-decision", "at": "2026-08-08T10:00:00.000Z",
            "repoId": "sha256:abc", "pathClass": "tests", "decision": "rejected",
        });
        let entry = entry_for(&rejected);
        assert!(!entry.fields_sent.contains(&"testsRun".to_string()));
        assert_eq!(entry.summary, "Agent edit rejected, path class tests");
    }

    #[test]
    fn a_summary_never_names_a_path_a_command_or_a_tool_argument() {
        let entry = entry_for(&json!({
            "eventId": "e3", "kind": "tool-use", "at": "2026-08-08T10:00:00.000Z",
            "repoId": "sha256:abc", "pathClass": "frontend", "rawCommand": "rm -rf /",
        }));
        assert_eq!(entry.summary, "Agent tool call, path class frontend");
    }

    #[test]
    fn test_run_and_turn_rows_read_as_counts_and_enums_only() {
        let run = entry_for(&json!({
            "eventId": "t", "kind": "test-run", "at": "a", "repoId": null, "pathClass": null,
            "testOutcome": "failed", "testRunner": "jest", "testsPassed": 3, "testsFailed": 1,
        }));
        assert_eq!(run.summary, "Test run failed, runner jest, 3 passed, 1 failed");
        let turn = entry_for(&json!({
            "eventId": "v", "kind": "turn-verification", "at": "a", "repoId": null, "pathClass": null,
            "editsAccepted": 2, "testRuns": 1, "endedGreen": false, "weakenedFlags": ["skip-added"],
        }));
        assert_eq!(
            turn.summary,
            "Turn checked: 2 accepted edits, 1 test run, did not end on a passing test run, flagged skip-added"
        );
    }

    #[test]
    fn observed_counts_raw_tool_calls_and_would_send_counts_derived_events() {
        let env = TestEnv::new();
        let day = "2026-08-08";
        record(
            &env.ctx.store,
            &[
                json!({"eventId": "a", "kind": "tool-use", "at": "2026-08-08T10:00:00.000Z", "repoId": null, "pathClass": null}),
                json!({"eventId": "b", "kind": "tool-use", "at": "2026-08-08T10:00:01.000Z", "repoId": null, "pathClass": null}),
            ],
            5,
            day,
        );
        assert_eq!(env.ctx.store.read_ledger(day).len(), 2);
        let receipt = receipt_for(&env.ctx.store, day).replace('\n', " ");
        assert!(receipt.contains("observed 5 tool calls today"));
        assert!(receipt.contains("sent 2 derived signals"));
        assert!(receipt.contains("blocked 0 actions"));
    }

    fn insight(at: &str) -> Value {
        json!({
            "sessionId": "s1", "turnId": "t1", "repoId": "sha256:abc", "pathClass": null,
            "prompt": "refactor the pricing module", "response": "Refactored pricing.ts", "at": at,
        })
    }

    #[test]
    fn an_insight_entry_lists_prompt_and_response_as_field_names_never_their_content() {
        let entry = entry_for_insight(&insight("2026-08-08T10:00:00.000Z"));
        assert_eq!(
            entry.fields_sent,
            ["sessionId", "turnId", "repoId", "pathClass", "prompt", "response", "at"]
        );
        assert!(!entry.summary.contains("refactor the pricing module"));
        assert!(!entry.summary.contains("Refactored pricing.ts"));
    }

    #[test]
    fn a_delivered_insight_gets_a_row_and_a_failed_one_only_a_count() {
        let env = TestEnv::new();
        record_insight(&env.ctx.store, &insight("2026-08-09T10:00:00.000Z"), true, "2026-08-09");
        assert_eq!(env.ctx.store.read_ledger("2026-08-09").len(), 1);
        assert_eq!(env.ctx.store.read_counters("2026-08-09").insights_sent, 1);
        assert_eq!(env.ctx.store.read_counters("2026-08-09").insights_failed, 0);

        record_insight(
            &env.ctx.store,
            &insight("2026-08-10T10:00:00.000Z"),
            false,
            "2026-08-10",
        );
        assert_eq!(
            env.ctx.store.read_ledger("2026-08-10").len(),
            0,
            "a failed send never claims a row"
        );
        assert_eq!(env.ctx.store.read_counters("2026-08-10").insights_failed, 1);
    }
}
