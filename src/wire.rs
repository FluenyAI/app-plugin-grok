// The redaction boundary. Nothing reaches the network except through here.
//
// This is a rebuild, not a filter: the outgoing object is constructed key by key
// from a fixed list, so an extractor that starts carrying an extra field cannot
// leak it by accident. A "remove tool_input" style filter fails the opposite way,
// silently, the first time a payload gains a field nobody wrote a rule for, and
// this is the one promise the whole product rests on (CEO decisions 8A, 33A).
//
// It works on the JSON value as it sits in the queue, not on the typed struct,
// because the queue is a file on disk and a file can hold anything. Values of
// the wrong type are dropped rather than passed through, so a string smuggled
// into `durationMs` is not a channel either, and the 0126 enums are checked
// against their closed lists so a free-text string cannot ride in `testRunner`.

use serde_json::{Map, Value, json};

use crate::types::{AgentId, CODING_EVENT_FIELDS, TEST_OUTCOMES, TEST_RUNNERS, WEAKENED_FLAGS};

const MAX_WEAKENED_FLAGS: usize = 8;

pub fn to_wire_event(event: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(source) = event.as_object() else {
        return out;
    };
    for field in CODING_EVENT_FIELDS {
        let Some(value) = source.get(field) else {
            continue;
        };
        let kept = match field {
            "eventId" | "kind" | "at" => value.as_str().map(|s| Value::String(s.to_string())),
            "repoId" | "pathClass" | "decision" | "toolCategory" | "commandCategory" => match value {
                Value::Null => Some(Value::Null),
                Value::String(s) => Some(Value::String(s.clone())),
                _ => None,
            },
            "testOutcome" => one_of(value, &TEST_OUTCOMES),
            "testRunner" => one_of(value, &TEST_RUNNERS),
            "testsRun" | "testFirst" | "endedGreen" | "sensitiveUntested" => value.as_bool().map(Value::Bool),
            // Parity with the TS client: any finite number, truncated.
            "subagentCount" | "durationMs" => value
                .as_f64()
                .filter(|n| n.is_finite())
                .map(|n| json!(n.trunc() as i64)),
            "weakenedFlags" => weakened(value),
            // Feature 0126 counts: whole numbers, never negative.
            _ => value
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
                .map(|n| json!(n.trunc() as i64)),
        };
        if let Some(kept) = kept {
            out.insert(field.to_string(), kept);
        }
    }
    out
}

fn one_of(value: &Value, allowed: &[&str]) -> Option<Value> {
    let text = value.as_str()?;
    allowed.contains(&text).then(|| Value::String(text.to_string()))
}

fn weakened(value: &Value) -> Option<Value> {
    let items = value.as_array()?;
    let mut kept: Vec<Value> = Vec::new();
    for item in items {
        let Some(flag) = item.as_str() else { continue };
        if !WEAKENED_FLAGS.contains(&flag) || kept.iter().any(|k| k.as_str() == Some(flag)) {
            continue;
        }
        kept.push(Value::String(flag.to_string()));
        if kept.len() == MAX_WEAKENED_FLAGS {
            break;
        }
    }
    Some(Value::Array(kept))
}

pub fn to_wire_batch(agent: AgentId, session_id: &str, events: &[Value]) -> Value {
    let events: Vec<Value> = events.iter().map(|e| Value::Object(to_wire_event(e))).collect();
    json!({ "agent": agent.as_str(), "sessionId": session_id, "events": events })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuilds_from_the_fixed_list_and_drops_everything_else() {
        let smuggled = json!({
            "eventId": "e1",
            "kind": "tool-use",
            "at": "2026-08-08T00:00:00.000Z",
            "repoId": null,
            "pathClass": "tests",
            "prompt": "PROMPTTEXT-refactor-the-billing-module",
            "tool_input": { "command": "SECRETVALUE-hunter2" },
            "tool_response": "TOOLRESPONSE-diff-plus-minus",
            "filePath": "/Users/someone/private/notes",
        });
        let wire = to_wire_event(&smuggled);
        for key in wire.keys() {
            assert!(
                CODING_EVENT_FIELDS.contains(&key.as_str()),
                "unexpected wire field {key}"
            );
        }
        let text = serde_json::to_string(&wire).unwrap();
        for poison in ["PROMPTTEXT", "SECRETVALUE", "TOOLRESPONSE", "/Users/someone"] {
            assert!(!text.contains(poison), "wire serializer leaked {poison}");
        }
    }

    #[test]
    fn values_of_the_wrong_type_are_dropped() {
        let wire = to_wire_event(&json!({
            "eventId": "e", "kind": "test-run", "at": "t", "repoId": null, "pathClass": null,
            "durationMs": "SECRET-in-a-number-field",
            "testsRun": "yes",
            "testRunner": "SECRET-free-text-runner",
            "testOutcome": "passed",
            "testsPassed": -3,
            "testsFailed": 2.9,
            "weakenedFlags": ["skip-added", "SECRET", "skip-added", 7, "no-verify"],
            "endedGreen": 1,
        }));
        assert!(!wire.contains_key("durationMs"));
        assert!(!wire.contains_key("testsRun"));
        assert!(!wire.contains_key("testRunner"));
        assert!(!wire.contains_key("testsPassed"));
        assert!(!wire.contains_key("endedGreen"));
        assert_eq!(wire["testOutcome"], "passed");
        assert_eq!(wire["testsFailed"], 2);
        assert_eq!(wire["weakenedFlags"], json!(["skip-added", "no-verify"]));
    }

    #[test]
    fn field_order_is_the_contract_order() {
        let wire = to_wire_event(&json!({
            "testsRun": true, "decision": "accepted", "pathClass": "auth", "repoId": "sha256:abc",
            "at": "2026-08-08T10:00:00.000Z", "kind": "edit-decision", "eventId": "e1",
        }));
        let keys: Vec<&str> = wire.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["eventId", "kind", "at", "repoId", "pathClass", "decision", "testsRun"]
        );
    }

    #[test]
    fn a_batch_has_exactly_three_keys() {
        let batch = to_wire_batch(
            AgentId::GrokBuild,
            "s",
            &[json!({"eventId": "x", "kind": "tool-use", "at": "t"})],
        );
        let mut keys: Vec<&String> = batch.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["agent", "events", "sessionId"]);
        assert_eq!(batch["agent"], "grok-build");
    }
}
