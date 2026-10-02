// A bounded local queue, so a developer working on a plane or behind a flaky VPN
// keeps coding at full speed and the client never grows without limit on their
// disk. When the bound is hit the OLDEST records go: newer signal is the signal
// somebody might still act on.

use std::collections::HashSet;

use serde_json::Value;

use crate::api::{HOOK_TIMEOUT_MS, current_token, is_ok, post_events, refresh};
use crate::context::Ctx;
use crate::store::QueuedRecord;
use crate::types::AgentId;
use crate::wire::to_wire_batch;

pub const MAX_QUEUED_EVENTS: usize = 2000;

// The backend caps a batch at 500 and truncates past it, silently, behind a 202.
// So the cap is enforced here, because no response ever will.
pub const MAX_BATCH: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlushResult {
    pub attempted: usize,
    pub sent: usize,
    pub remaining: usize,
    // "Did the bytes leave" only, never "were they kept": ingest answers 202
    // unconditionally.
    pub delivered: bool,
}

fn event_id(event: &Value) -> Option<&str> {
    event.get("eventId").and_then(Value::as_str)
}

/// Returns the events it actually queued, so the receipt ledger is written from
/// the same list that reaches the wire, never from one dedupe already trimmed.
pub fn enqueue(ctx: &Ctx, agent: AgentId, session_id: &str, events: Vec<Value>) -> Vec<Value> {
    if events.is_empty() {
        return Vec::new();
    }
    let existing = ctx.store.read_queue();
    let mut known: HashSet<String> = existing
        .iter()
        .filter_map(|r| event_id(&r.event).map(str::to_string))
        .collect();
    let fresh: Vec<Value> = events
        .into_iter()
        .filter(|event| match event_id(event) {
            Some(id) => known.insert(id.to_string()),
            None => false,
        })
        .collect();
    if fresh.is_empty() {
        return fresh;
    }
    if existing.len() + fresh.len() > MAX_QUEUED_EVENTS {
        // Trim the combined list: one hook can enqueue more than the bound on its
        // own, and slicing only the part on disk leaves the queue over its limit.
        let mut combined = existing;
        combined.extend(fresh.iter().map(|event| QueuedRecord {
            agent,
            session_id: session_id.to_string(),
            event: event.clone(),
        }));
        let start = combined.len() - MAX_QUEUED_EVENTS;
        ctx.store.replace_queue(&combined[start..]);
        return fresh;
    }
    ctx.store.append_queue(agent, session_id, &fresh);
    fresh
}

/// `timeout_ms` overrides the per-request budget, for the opportunistic live
/// flush from PostToolUse (feature 0098): that call sits on the critical path of
/// every tool call, so it fails fast and leaves the event queued for the next
/// full-budget flush. Nothing is ever lost, only delayed.
pub fn flush(ctx: &Ctx, timeout_ms: Option<u64>) -> FlushResult {
    let timeout_ms = timeout_ms.unwrap_or(HOOK_TIMEOUT_MS);
    let queued = ctx.store.read_queue();
    if queued.is_empty() {
        return FlushResult {
            attempted: 0,
            sent: 0,
            remaining: 0,
            delivered: true,
        };
    }
    let mut failed: Vec<QueuedRecord> = Vec::new();
    let mut sent = 0;

    // One batch per (agent, session), because a batch carries a single sessionId,
    // in first-seen order.
    let mut groups: Vec<(AgentId, String, Vec<QueuedRecord>)> = Vec::new();
    for record in &queued {
        match groups
            .iter_mut()
            .find(|(a, s, _)| *a == record.agent && *s == record.session_id)
        {
            Some(group) => group.2.push(record.clone()),
            None => groups.push((record.agent, record.session_id.clone(), vec![record.clone()])),
        }
    }

    for (agent, session_id, records) in groups {
        let Some(mut creds) = current_token(ctx, agent) else {
            failed.extend(records);
            continue;
        };
        for chunk in records.chunks(MAX_BATCH) {
            let events: Vec<Value> = chunk.iter().map(|r| r.event.clone()).collect();
            let batch = to_wire_batch(agent, &session_id, &events);
            let mut res = post_events(&creds.api_url, &creds.access_token, &batch, timeout_ms);
            // The one failure /events surfaces. Everything else is a 202 and must
            // not be retried as if it were an error.
            if res.status == 401 {
                match refresh(ctx, &creds, agent) {
                    Some(renewed) => {
                        creds = renewed;
                        res = post_events(&creds.api_url, &creds.access_token, &batch, timeout_ms);
                    }
                    None => {
                        failed.extend(chunk.iter().cloned());
                        continue;
                    }
                }
            }
            if is_ok(res.status) {
                sent += chunk.len();
            } else {
                failed.extend(chunk.iter().cloned());
            }
        }
    }

    let start = failed.len().saturating_sub(MAX_QUEUED_EVENTS);
    ctx.store.replace_queue(&failed[start..]);
    FlushResult {
        attempted: queued.len(),
        sent,
        remaining: failed.len(),
        delivered: failed.is_empty(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{MockServer, Reply, TestEnv, closed_url};
    use crate::types::CODING_EVENT_FIELDS;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn event(id: &str) -> Value {
        json!({ "eventId": id, "kind": "tool-use", "at": crate::time::now_iso(), "repoId": "sha256:x", "pathClass": "tests" })
    }

    fn ids(env: &TestEnv) -> Vec<String> {
        env.ctx
            .store
            .read_queue()
            .iter()
            .map(|r| r.event["eventId"].as_str().unwrap().to_string())
            .collect()
    }

    const CC: AgentId = AgentId::ClaudeCode;

    #[test]
    fn an_event_id_already_queued_is_not_queued_again() {
        let env = TestEnv::new();
        assert_eq!(enqueue(&env.ctx, CC, "s1", vec![event("a"), event("b")]).len(), 2);
        assert_eq!(enqueue(&env.ctx, CC, "s1", vec![event("a"), event("c")]).len(), 1);
        assert_eq!(ids(&env), ["a", "b", "c"]);
    }

    #[test]
    fn a_duplicate_inside_one_call_is_queued_once() {
        let env = TestEnv::new();
        assert_eq!(enqueue(&env.ctx, CC, "s1", vec![event("dup"), event("dup")]).len(), 1);
        assert_eq!(env.ctx.store.read_queue().len(), 1);
    }

    #[test]
    fn the_queue_is_bounded_and_sheds_the_oldest_never_the_newest() {
        let env = TestEnv::new();
        let many: Vec<Value> = (0..MAX_QUEUED_EVENTS + 50).map(|i| event(&format!("e{i}"))).collect();
        enqueue(&env.ctx, CC, "s1", many);
        let queued = ids(&env);
        assert_eq!(queued.len(), MAX_QUEUED_EVENTS);
        assert_eq!(queued.last().unwrap(), &format!("e{}", MAX_QUEUED_EVENTS + 49));
        assert!(!queued.contains(&"e0".to_string()));
    }

    #[test]
    fn flush_sends_every_queued_event_once_and_empties_the_queue() {
        let env = TestEnv::new();
        let server = MockServer::accepting();
        env.connect(&server.url);
        enqueue(&env.ctx, CC, "s1", vec![event("x"), event("y")]);
        enqueue(&env.ctx, CC, "s2", vec![event("z")]);
        let result = flush(&env.ctx, None);
        assert_eq!((result.sent, result.remaining), (3, 0));
        // One batch per session, because a batch carries a single sessionId.
        assert_eq!(server.calls().len(), 2);
        let mut sent: Vec<String> = server
            .calls()
            .iter()
            .flat_map(|c| {
                c.body["events"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["eventId"].as_str().unwrap().to_string())
                    .collect::<Vec<_>>()
            })
            .collect();
        sent.sort();
        assert_eq!(sent, ["x", "y", "z"]);
        assert!(env.ctx.store.read_queue().is_empty());
    }

    #[test]
    fn a_batch_never_exceeds_the_size_the_backend_accepts_and_the_tail_is_not_lost() {
        let env = TestEnv::new();
        let server = MockServer::accepting();
        env.connect(&server.url);
        let count = MAX_BATCH * 2 + 1;
        enqueue(
            &env.ctx,
            CC,
            "s1",
            (0..count).map(|i| event(&format!("t{i}"))).collect(),
        );
        flush(&env.ctx, None);
        assert_eq!(server.calls().len(), 3);
        let mut seen = HashSet::new();
        for call in server.calls() {
            let events = call.body["events"].as_array().unwrap();
            assert!(events.len() <= MAX_BATCH);
            for e in events {
                seen.insert(e["eventId"].as_str().unwrap().to_string());
            }
        }
        assert_eq!(seen.len(), count);
    }

    #[test]
    fn a_401_refreshes_the_hook_token_once_and_resends() {
        let env = TestEnv::new();
        let ingest = Arc::new(AtomicUsize::new(0));
        let counter = ingest.clone();
        let server = MockServer::start(move |path, _| {
            if path.ends_with("/oauth/token") {
                return Reply::Json(
                    200,
                    json!({ "access_token": "fresh", "refresh_token": "r2", "expires_in": 3600 }),
                );
            }
            if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::Json(401, json!({}))
            } else {
                Reply::Json(202, json!({}))
            }
        });
        env.connect(&server.url);
        enqueue(&env.ctx, CC, "s1", vec![event("needs-refresh")]);
        assert_eq!(flush(&env.ctx, None).sent, 1);
        assert_eq!(server.calls_to("/oauth/token").len(), 1);
        assert_eq!(ingest.load(Ordering::SeqCst), 2);
        assert!(env.ctx.store.read_queue().is_empty());
        // The resend carried the refreshed token, and the store holds it now.
        assert_eq!(env.ctx.creds.read(CC).unwrap().access_token, "fresh");
    }

    #[test]
    fn an_unreachable_backend_keeps_the_events() {
        let env = TestEnv::new();
        env.connect(&closed_url());
        enqueue(&env.ctx, CC, "s1", vec![event("kept")]);
        let result = flush(&env.ctx, None);
        assert_eq!(result.sent, 0);
        assert!(!result.delivered);
        assert_eq!(ids(&env), ["kept"]);
    }

    #[test]
    fn everything_on_the_wire_is_a_whitelisted_field() {
        let env = TestEnv::new();
        let server = MockServer::accepting();
        env.connect(&server.url);
        let mut poisoned = event("fields");
        poisoned["tool_input"] = json!({ "command": "SECRET" });
        enqueue(&env.ctx, CC, "s1", vec![poisoned]);
        flush(&env.ctx, None);
        let call = &server.calls()[0];
        for key in call.body["events"][0].as_object().unwrap().keys() {
            assert!(CODING_EVENT_FIELDS.contains(&key.as_str()));
        }
        assert!(!call.raw.contains("SECRET"));
    }

    #[test]
    fn a_batch_stays_well_inside_the_1mb_body_limit_at_the_field_maxima() {
        let maxed: Vec<Value> = (0..MAX_BATCH)
            .map(|i| {
                json!({
                    "eventId": format!("{:x>200}", i), "kind": "turn-verification", "at": "2026-08-08T12:34:56.789Z",
                    "repoId": format!("sha256:{}", "a".repeat(64)), "pathClass": "x".repeat(64),
                    "decision": "accepted", "testsRun": true, "subagentCount": 1000, "durationMs": 999_999_999,
                    "testOutcome": "passed", "testRunner": "playwright", "testsPassed": 999_999, "testsFailed": 999_999,
                    "testsSkipped": 999_999, "editsAccepted": 999_999, "sourceFilesChanged": 999_999,
                    "testFilesChanged": 999_999, "testFirst": true, "testRuns": 999_999, "endedGreen": true,
                    "editsAfterLastGreen": 999_999, "failingRunStreakMax": 999_999, "reworkFiles": 999_999,
                    "sensitiveUntested": true, "weakenedFlags": crate::types::WEAKENED_FLAGS, "weakenedCaught": 999_999,
                })
            })
            .collect();
        let bytes = to_wire_batch(CC, &"x".repeat(200), &maxed).to_string().len();
        assert!(
            bytes < 1_000_000,
            "{bytes} bytes would be refused by the 1 MB body limit"
        );
    }

    #[test]
    fn a_5xx_is_not_delivery_and_a_202_with_an_empty_body_is() {
        let env = TestEnv::new();
        let broken = MockServer::start(|_, _| Reply::Json(500, json!({})));
        env.connect(&broken.url);
        enqueue(&env.ctx, CC, "s1", vec![event("server-error")]);
        assert_eq!(flush(&env.ctx, None).sent, 0);
        assert_eq!(env.ctx.store.read_queue().len(), 1, "a 5xx must keep the event");

        let empty = MockServer::start(|_, _| Reply::Json(202, Value::Null));
        env.connect(&empty.url);
        assert_eq!(flush(&env.ctx, None).sent, 1);
        assert!(env.ctx.store.read_queue().is_empty());
    }
}
