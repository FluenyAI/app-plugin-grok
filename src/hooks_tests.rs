// Hook orchestration and the redaction boundary, driven through the real hook
// path against a real local HTTP server. Every privacy assertion here is about
// what was SENT, never about a response: `/events` answers 202 to everything, so a
// status-based test would pass against a client that leaked everything.

use super::*;
use crate::classify::test_classifier;
use crate::repo_id::repo_id_for;
use crate::store::SessionState;
use crate::testing::{MockServer, Reply, TestEnv, handshake_body};
use crate::types::{AgentId, CODING_EVENT_FIELDS};
use serde_json::json;

const REMOTE: &str = "git@github.com:FluenyAI/app-backend.git";

// Every one of these is a string the client must be structurally incapable of
// transmitting. Deliberately unusual, so a substring search is decisive.
const POISON: &[&str] = &[
    "PROMPTTEXT-refactor-the-billing-module",
    "CODEBODY-const-apiKey-equals-sk-live-1234",
    "FILECONTENTS-line-one-line-two",
    "TOOLRESPONSE-diff-plus-minus",
    "SECRETVALUE-hunter2",
    "/Users/someone/private/notes",
    "BRANCHNAME-feat-secret-project",
    "TESTOUTPUT-stack-trace-with-secret",
    "TESTNAME-should-charge-the-card",
];

fn allowlisting(over: Value) -> MockServer {
    MockServer::start(move |path, _| {
        if path.ends_with("/session/start") {
            let mut body = json!({ "repoAllowlist": [repo_id_for(REMOTE)] });
            for (k, v) in over.as_object().unwrap() {
                body[k] = v.clone();
            }
            return Reply::Json(200, handshake_body(body));
        }
        Reply::Json(202, json!({}))
    })
}

fn live_env(over: Value) -> (TestEnv, MockServer, std::path::PathBuf) {
    let env = TestEnv::new();
    let repo = env.make_repo(REMOTE);
    let server = allowlisting(over);
    env.connect(&server.url);
    (env, server, repo)
}

fn seed(env: &TestEnv, session_id: &str, over: impl FnOnce(&mut SessionState)) {
    let mut state = SessionState {
        session_id: session_id.into(),
        agent: Some(AgentId::ClaudeCode),
        started_at: now_ms(),
        repo_id: Some("sha256:repo".into()),
        ..SessionState::default()
    };
    over(&mut state);
    env.ctx.store.write_session(&mut state);
}

fn transcript(env: &TestEnv, name: &str, lines: &[Value]) -> String {
    let path = env.dir.path().join(name);
    std::fs::write(&path, lines.iter().map(|l| l.to_string() + "\n").collect::<String>()).unwrap();
    path.to_string_lossy().to_string()
}

fn user_text(text: &str) -> Value {
    json!({ "type": "user", "message": { "role": "user", "content": [{ "type": "text", "text": text }] } })
}

fn assistant_text(text: &str) -> Value {
    json!({ "type": "assistant", "message": { "role": "assistant", "content": [{ "type": "text", "text": text }] } })
}

fn sent_events(server: &MockServer) -> Vec<Value> {
    server
        .calls_to("/events")
        .iter()
        .flat_map(|c| c.body["events"].as_array().cloned().unwrap_or_default())
        .collect()
}

fn of_kind(server: &MockServer, kind: &str) -> Vec<Value> {
    sent_events(server).into_iter().filter(|e| e["kind"] == kind).collect()
}

fn assert_no_poison(server: &MockServer) {
    let everything = server.everything_sent();
    for poison in POISON {
        assert!(!everything.contains(poison), "the wire carried {poison}");
    }
}

fn assert_only_whitelisted(server: &MockServer) {
    for call in server.calls_to("/events") {
        let mut keys: Vec<&String> = call.body.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["agent", "events", "sessionId"]);
        for event in call.body["events"].as_array().unwrap() {
            for key in event.as_object().unwrap().keys() {
                assert!(
                    CODING_EVENT_FIELDS.contains(&key.as_str()),
                    "unexpected event field {key}"
                );
            }
        }
    }
}

fn hostile_payload(session: &str, repo: &Path) -> Value {
    json!({
        "session_id": session,
        "transcript_path": "/Users/someone/private/notes/transcript.jsonl",
        "cwd": repo,
        "hook_event_name": "PostToolUse",
        "permission_mode": "acceptEdits",
        "prompt": "PROMPTTEXT-refactor-the-billing-module",
        "tool_name": "Edit",
        "tool_input": {
            "file_path": repo.join("src/auth/session.ts"),
            "old_string": "CODEBODY-const-apiKey-equals-sk-live-1234",
            "new_string": "FILECONTENTS-line-one-line-two",
            "command": "git checkout BRANCHNAME-feat-secret-project",
            "env": { "API_KEY": "SECRETVALUE-hunter2" },
        },
        "tool_response": {
            "filePath": "/Users/someone/private/notes/x.ts",
            "structuredPatch": [{ "lines": ["+TOOLRESPONSE-diff-plus-minus"] }],
            "content": "FILECONTENTS-line-one-line-two",
        },
    })
}

// ---- redaction (ported from test/redaction.test.ts) ----

#[test]
fn extraction_returns_no_field_that_can_hold_raw_payload_content() {
    let env = TestEnv::new();
    let repo = env.make_repo(REMOTE);
    let payload = hostile_payload("s", &repo);
    let facts = extract_tool_facts(
        payload.as_object().unwrap(),
        Some(&repo.to_string_lossy()),
        &test_classifier(),
        false,
    );
    let text = format!("{facts:?}");
    for poison in POISON {
        assert!(!text.contains(poison), "extraction leaked {poison}");
    }
    assert_eq!(facts.path_class.as_deref(), Some("auth"));
    assert!(facts.is_edit);
}

#[test]
fn a_hostile_payload_reaches_the_network_as_derived_signal_only() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(&env.ctx, &hostile_payload("session-redaction", &repo), false);
    on_stop(&env.ctx, &json!({ "session_id": "session-redaction", "cwd": repo }));
    assert!(
        !server.calls_to("/events").is_empty(),
        "nothing was posted, so this test proved nothing"
    );
    assert_no_poison(&server);
    assert_only_whitelisted(&server);
    assert!(
        of_kind(&server, "edit-decision")
            .iter()
            .any(|e| e["decision"] == "accepted" && e["pathClass"] == "auth"),
        "the edit decision was not derived at all"
    );
}

#[test]
fn with_prompt_insight_scoring_off_a_transcript_never_reaches_insights() {
    let (env, server, repo) = live_env(json!({}));
    let path = transcript(
        &env,
        "off.jsonl",
        &[
            user_text("PROMPTTEXT-off-by-default"),
            assistant_text("REPLYTEXT-off-by-default"),
        ],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "s-off", "cwd": repo, "transcript_path": path }),
    );
    assert!(server.calls_to("/insights").is_empty());
    let everything = server.everything_sent();
    assert!(!everything.contains("PROMPTTEXT-off-by-default") && !everything.contains("REPLYTEXT-off-by-default"));
}

#[test]
fn with_prompt_insight_scoring_on_exactly_the_turn_is_sent_to_insights_and_nowhere_else() {
    let (env, server, repo) = live_env(json!({ "promptInsightsEnabled": true }));
    let path = transcript(
        &env,
        "on.jsonl",
        &[
            user_text("refactor the pricing module"),
            assistant_text("Refactored pricing.ts"),
        ],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "s-on", "cwd": repo, "transcript_path": path }),
    );
    let calls = server.calls_to("/insights");
    assert_eq!(calls.len(), 1);
    let body = &calls[0].body;
    assert_eq!(body["sessionId"], "s-on");
    assert_eq!(body["prompt"], "refactor the pricing module");
    assert_eq!(body["response"], "Refactored pricing.ts");
    let mut keys: Vec<&String> = body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["at", "pathClass", "prompt", "repoId", "response", "sessionId", "turnId"]
    );
    for call in server.calls_to("/events") {
        assert!(!call.raw.contains("refactor the pricing module") && !call.raw.contains("Refactored pricing.ts"));
    }
}

#[test]
fn with_live_feedback_off_a_transcript_never_reaches_live_feedback() {
    let (env, server, repo) = live_env(json!({}));
    let path = transcript(
        &env,
        "lf-off.jsonl",
        &[user_text("PROMPTTEXT-lf-off"), assistant_text("REPLYTEXT-lf-off")],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "s-lf-off", "cwd": repo, "transcript_path": path }),
    );
    assert!(server.calls_to("/live-feedback").is_empty());
    assert!(!server.everything_sent().contains("PROMPTTEXT-lf-off"));
}

#[test]
fn with_live_feedback_on_exactly_the_turn_is_sent_to_live_feedback_and_nowhere_else() {
    let (env, server, repo) = live_env(json!({ "liveFeedbackEnabled": true }));
    let path = transcript(
        &env,
        "lf-on.jsonl",
        &[
            user_text("add a health check endpoint"),
            assistant_text("Added GET /health"),
        ],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "s-lf-on", "cwd": repo, "transcript_path": path }),
    );
    let calls = server.calls_to("/live-feedback");
    assert_eq!(calls.len(), 1);
    let mut keys: Vec<&String> = calls[0].body.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["at", "pathClass", "prompt", "repoId", "response", "sessionId", "turnId"]
    );
    for call in server.calls() {
        if !call.path.ends_with("/live-feedback") {
            assert!(!call.raw.contains("add a health check endpoint") && !call.raw.contains("Added GET /health"));
        }
    }
    assert!(server.calls_to("/insights").is_empty());
}

// Feature 0109: with the developer's own raw-activity opt-in, a real repo-relative
// path and real Bash command text cross, ONLY those, ONLY on /raw-activity.
#[test]
fn with_raw_activity_on_exactly_the_real_path_and_command_cross_on_raw_activity() {
    let (env, server, repo) = live_env(json!({ "rawActivityEnabled": true }));
    let session = "session-raw-activity";
    let mut edit = hostile_payload(session, &repo);
    edit["session_id"] = json!(session);
    on_post_tool_use(&env.ctx, &edit, false);
    on_post_tool_use(
        &env.ctx,
        &json!({
            "session_id": session, "cwd": repo, "tool_name": "Bash",
            "tool_input": { "command": "RAWCOMMAND-git-status-check" },
            "tool_response": { "stdout": "TOOLRESPONSE-diff-plus-minus" },
        }),
        false,
    );
    on_stop(&env.ctx, &json!({ "session_id": session, "cwd": repo }));
    let calls = server.calls_to("/raw-activity");
    assert_eq!(calls.len(), 2, "one /raw-activity call per tool call carrying raw data");
    let path_call = calls.iter().find(|c| c.body.get("rawPath").is_some()).unwrap();
    assert_eq!(path_call.body["rawPath"], "src/auth/session.ts");
    assert!(path_call.body.get("rawCommand").is_none());
    let command_call = calls.iter().find(|c| c.body.get("rawCommand").is_some()).unwrap();
    assert_eq!(command_call.body["rawCommand"], "RAWCOMMAND-git-status-check");
    for call in &calls {
        let mut keys: Vec<&String> = call.body.as_object().unwrap().keys().collect();
        keys.sort();
        let raw = if call.body.get("rawPath").is_some() {
            "rawPath"
        } else {
            "rawCommand"
        };
        assert_eq!(keys, ["at", "eventId", raw, "sessionId"]);
    }
    assert_no_poison(&server);
}

// ---- orchestration (ported from test/hooks.test.ts, with the insight receipt) ----

fn insight_snapshot(env: &TestEnv) -> (i64, i64, usize) {
    let counters = env.ctx.store.read_counters(&today());
    (
        counters.insights_sent,
        counters.insights_failed,
        env.ctx.store.read_ledger(&today()).len(),
    )
}

#[test]
fn a_turn_is_scored_on_stop_immediately_and_leaves_a_local_trace() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    env.connect(&server.url);
    seed(&env, "stop-sends-insight", |s| s.prompt_insights_enabled = true);
    let path = transcript(
        &env,
        "t.jsonl",
        &[user_text("fix the login bug"), assistant_text("Fixed it in auth.ts")],
    );
    let before = insight_snapshot(&env);
    on_stop(
        &env.ctx,
        &json!({ "session_id": "stop-sends-insight", "cwd": "/tmp", "transcript_path": path }),
    );
    let calls = server.calls_to("/integrations/coding/insights");
    assert_eq!(calls.len(), 1, "Stop must send the turn itself");
    assert_eq!(calls[0].body["prompt"], "fix the login bug");
    let after = insight_snapshot(&env);
    assert_eq!((after.0 - before.0, after.1 - before.1, after.2 - before.2), (1, 0, 1));
}

#[test]
fn a_submission_the_backend_rejects_counts_as_failed_not_silently_dropped() {
    let env = TestEnv::new();
    let server = MockServer::start(|path, _| {
        if path.ends_with("/insights") {
            Reply::Json(500, json!({}))
        } else {
            Reply::Json(202, json!({}))
        }
    });
    env.connect(&server.url);
    seed(&env, "stop-insight-rejected", |s| s.prompt_insights_enabled = true);
    let path = transcript(
        &env,
        "t.jsonl",
        &[user_text("fix the login bug"), assistant_text("Fixed it")],
    );
    let before = insight_snapshot(&env);
    on_stop(
        &env.ctx,
        &json!({ "session_id": "stop-insight-rejected", "cwd": "/tmp", "transcript_path": path }),
    );
    let after = insight_snapshot(&env);
    assert_eq!((after.0 - before.0, after.1 - before.1, after.2 - before.2), (0, 1, 0));
}

#[test]
fn a_submission_attempted_with_no_credential_still_counts_as_failed() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    seed(&env, "stop-insight-no-credential", |s| s.prompt_insights_enabled = true);
    let path = transcript(
        &env,
        "t.jsonl",
        &[user_text("fix the login bug"), assistant_text("Fixed it")],
    );
    let before = insight_snapshot(&env);
    on_stop(
        &env.ctx,
        &json!({ "session_id": "stop-insight-no-credential", "cwd": "/tmp", "transcript_path": path }),
    );
    assert!(server.calls().is_empty());
    let after = insight_snapshot(&env);
    assert_eq!((after.1 - before.1, after.2 - before.2), (1, 0));
}

#[test]
fn a_developer_who_has_not_opted_in_sends_nothing_on_stop() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    env.connect(&server.url);
    seed(&env, "opted-out", |_| {});
    let path = transcript(
        &env,
        "t.jsonl",
        &[user_text("fix the login bug"), assistant_text("Fixed it")],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "opted-out", "cwd": "/tmp", "transcript_path": path }),
    );
    assert!(server.calls_to("/insights").is_empty() && server.calls_to("/live-feedback").is_empty());
}

#[test]
fn session_end_never_posts_an_insight_or_live_feedback() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    env.connect(&server.url);
    seed(&env, "end", |s| {
        s.prompt_insights_enabled = true;
        s.live_feedback_enabled = true;
    });
    on_session_end(&env.ctx, &json!({ "session_id": "end", "cwd": "/tmp" }));
    assert!(server.calls_to("/insights").is_empty());
    assert!(server.calls_to("/live-feedback").is_empty());
    assert_eq!(of_kind(&server, "session-end").len(), 1);
}

#[test]
fn a_turn_with_both_opt_ins_reaches_both_endpoints_from_one_sweep() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    env.connect(&server.url);
    seed(&env, "both", |s| {
        s.prompt_insights_enabled = true;
        s.live_feedback_enabled = true;
    });
    let path = transcript(
        &env,
        "t.jsonl",
        &[user_text("fix the login bug"), assistant_text("Fixed it")],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "both", "cwd": "/tmp", "transcript_path": path }),
    );
    let insights = server.calls_to("/insights");
    let feedback = server.calls_to("/live-feedback");
    assert_eq!((insights.len(), feedback.len()), (1, 1));
    assert_eq!(insights[0].body["turnId"], feedback[0].body["turnId"]);
}

#[test]
fn tool_activity_accumulates_is_attached_to_live_feedback_only_then_resets() {
    let env = TestEnv::new();
    let server = MockServer::accepting();
    env.connect(&server.url);
    seed(&env, "activity", |s| {
        s.live_feedback_enabled = true;
        s.prompt_insights_enabled = true;
    });
    on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": "activity", "cwd": "/tmp", "tool_name": "Read", "tool_input": { "file_path": "/tmp/README.md" } }),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": "activity", "cwd": "/tmp", "tool_name": "Bash", "tool_input": { "command": "npm test" } }),
        false,
    );
    let path = transcript(
        &env,
        "t1.jsonl",
        &[
            user_text("review recent changes"),
            assistant_text("Looked and ran tests"),
        ],
    );
    on_stop(
        &env.ctx,
        &json!({ "session_id": "activity", "cwd": "/tmp", "transcript_path": path }),
    );
    let feedback = server.calls_to("/live-feedback");
    let categories: Vec<&str> = feedback[0].body["toolActivity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["toolCategory"].as_str().unwrap())
        .collect();
    assert_eq!(categories, ["read", "bash"]);
    assert!(server.calls_to("/insights")[0].body.get("toolActivity").is_none());

    // The same transcript, one turn longer, so the second Stop really does score a
    // turn and the assertion below is about an actual submission.
    let path = transcript(
        &env,
        "t1.jsonl",
        &[
            user_text("review recent changes"),
            assistant_text("Looked and ran tests"),
            user_text("one more thing"),
            assistant_text("Done"),
        ],
    );
    let second = MockServer::accepting();
    env.connect(&second.url);
    on_stop(
        &env.ctx,
        &json!({ "session_id": "activity", "cwd": "/tmp", "transcript_path": path }),
    );
    assert!(second.calls_to("/live-feedback")[0].body.get("toolActivity").is_none());
}

#[test]
fn a_tool_call_is_flushed_to_events_within_the_same_post_tool_use() {
    let (env, server, repo) = live_env(json!({}));
    let outcome = on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": "live", "cwd": repo, "tool_name": "Read", "tool_input": { "file_path": repo.join("src/app.ts") } }),
        false,
    );
    assert_eq!(outcome.sent, 1);
    assert_eq!(
        server.calls_to("/events").len(),
        1,
        "PostToolUse must flush live, without waiting for Stop"
    );
}

#[test]
fn a_live_flush_that_cannot_reach_the_backend_does_not_lose_the_event() {
    let env = TestEnv::new();
    let repo = env.make_repo(REMOTE);
    let server = MockServer::start(|path, _| {
        if path.ends_with("/session/start") {
            Reply::Json(200, handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)] })))
        } else {
            Reply::Drop
        }
    });
    env.connect(&server.url);
    let started = std::time::Instant::now();
    let outcome = on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": "drop", "cwd": repo, "tool_name": "Read", "tool_input": { "file_path": repo.join("src/app.ts") } }),
        false,
    );
    assert_eq!(outcome.sent, 1);
    assert_eq!(
        env.ctx.store.read_queue().len(),
        1,
        "the event must stay queued after a failed live send"
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}

// ---- feature 0126: test runs, verification, weakened tests, reverts ----

const JEST_FAIL: &str = "FAIL src/billing/charge.test.ts\n  \u{25cf} TESTNAME-should-charge-the-card\n\n    TESTOUTPUT-stack-trace-with-secret SECRETVALUE-hunter2\n\nTests:       1 failed, 2 passed, 3 total\nSnapshots:   0 total\nTime:        1.2 s\n";
const JEST_PASS: &str = "PASS src/billing/charge.test.ts\n  \u{2713} TESTNAME-should-charge-the-card (3 ms)\n\nTests:       3 passed, 3 total\n";

fn claude_bash(session: &str, repo: &Path, id: &str, command: &str, stdout: &str) -> Value {
    json!({
        "session_id": session, "cwd": repo, "hook_event_name": "PostToolUse", "tool_name": "Bash", "tool_use_id": id,
        "tool_input": { "command": command, "description": "PROMPTTEXT-refactor-the-billing-module" },
        "tool_response": { "stdout": "", "stderr": stdout, "interrupted": false, "isImage": false },
        "duration_ms": 1234,
    })
}

fn claude_bash_failure(session: &str, repo: &Path, id: &str, command: &str, output: &str, code: i32) -> Value {
    json!({
        "session_id": session, "cwd": repo, "hook_event_name": "PostToolUseFailure", "tool_name": "Bash", "tool_use_id": id,
        "tool_input": { "command": command },
        "error": format!("Exit code {code}\n{output}"), "is_interrupt": false, "duration_ms": 2100,
    })
}

fn claude_edit(session: &str, repo: &Path, id: &str, rel: &str, old: &str, new: &str, original: &str) -> Value {
    json!({
        "session_id": session, "cwd": repo, "hook_event_name": "PostToolUse", "tool_name": "Edit", "tool_use_id": id,
        "tool_input": { "file_path": repo.join(rel), "old_string": old, "new_string": new },
        "tool_response": { "filePath": repo.join(rel), "oldString": old, "newString": new, "originalFile": original, "structuredPatch": [] },
    })
}

#[test]
fn a_failing_test_run_under_claude_code_is_a_failed_test_run_with_counts_only() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_bash_failure("tr-fail", &repo, "toolu_t1", "npx jest src/billing", JEST_FAIL, 1),
        true,
    );
    let runs = of_kind(&server, "test-run");
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run["testOutcome"], "failed");
    assert_eq!(run["testRunner"], "jest");
    assert_eq!(
        (run["testsPassed"].as_i64(), run["testsFailed"].as_i64()),
        (Some(2), Some(1))
    );
    assert_eq!(run["durationMs"], 2100);
    assert_eq!(run["pathClass"], Value::Null);
    // The failed call is still a tool call: before 0126 it produced nothing at all.
    assert_eq!(of_kind(&server, "tool-use").len(), 1);
    assert_no_poison(&server);
    assert_only_whitelisted(&server);
}

#[test]
fn a_passing_test_run_under_claude_code_is_passed() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_bash("tr-pass", &repo, "toolu_t2", "npm test", JEST_PASS),
        false,
    );
    let run = &of_kind(&server, "test-run")[0];
    assert_eq!(run["testOutcome"], "passed");
    assert_eq!(
        run["testRunner"], "jest",
        "npm test resolves its runner from the output"
    );
    assert_eq!(run["testsPassed"], 3);
    assert_eq!(run["durationMs"], 1234);
    assert_no_poison(&server);
}

#[test]
fn a_piped_test_run_with_no_summary_is_unknown_never_passed() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_bash("tr-pipe", &repo, "toolu_t3", "npm test 2>&1 | tail -3", "done\n"),
        false,
    );
    assert_eq!(of_kind(&server, "test-run")[0]["testOutcome"], "unknown");
}

#[test]
fn a_non_zero_exit_with_no_summary_is_an_error() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_bash_failure(
            "tr-err",
            &repo,
            "toolu_t4",
            "cargo test",
            "error[E0425]: cannot find value `x`",
            101,
        ),
        true,
    );
    let run = &of_kind(&server, "test-run")[0];
    assert_eq!(run["testOutcome"], "error");
    assert_eq!(run["testRunner"], "cargo");
}

#[test]
fn grok_reports_the_exit_code_in_post_tool_use() {
    let env = TestEnv::for_agent(AgentId::GrokBuild);
    let repo = env.make_repo(REMOTE);
    let server = allowlisting(json!({}));
    env.connect(&server.url);
    on_post_tool_use(
        &env.ctx,
        &json!({
            "sessionId": "grok-1", "cwd": repo, "hookEventName": "post_tool_use", "toolName": "run_terminal_command",
            "toolUseId": "g1", "toolInput": { "command": "pytest -q" },
            "toolResult": { "type": "Bash", "command": "pytest -q", "exit_code": 1,
                "output_for_prompt": "FAILED tests/test_pay.py::TESTNAME-should-charge-the-card\n==== 1 failed, 4 passed in 0.20s ====" },
        }),
        false,
    );
    let run = &of_kind(&server, "test-run")[0];
    assert_eq!(run["testOutcome"], "failed");
    assert_eq!(run["testRunner"], "pytest");
    assert_eq!(
        (run["testsPassed"].as_i64(), run["testsFailed"].as_i64()),
        (Some(4), Some(1))
    );
    assert_eq!(server.calls_to("/events")[0].body["agent"], "grok-build");
    assert_no_poison(&server);
}

#[test]
fn a_turn_is_verified_at_stop_with_every_field() {
    let (env, server, repo) = live_env(json!({}));
    let s = "tv-1";
    // Test first, then source, then a failing run, a fix, and a passing run.
    on_post_tool_use(
        &env.ctx,
        &claude_edit(
            s,
            &repo,
            "e1",
            "src/billing/charge.test.ts",
            "",
            "it('charges', () => { expect(x).toBe(1) })",
            "describe('x', () => {})",
        ),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &claude_edit(s, &repo, "e2", "src/billing/charge.ts", "a", "b", "a"),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &claude_bash_failure(s, &repo, "b1", "npx jest", JEST_FAIL, 1),
        true,
    );
    on_post_tool_use(
        &env.ctx,
        &claude_edit(s, &repo, "e3", "src/billing/charge.ts", "b", "c", "b"),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": s, "cwd": repo, "tool_name": "Task", "tool_use_id": "sub1", "tool_input": {} }),
        false,
    );
    on_post_tool_use(&env.ctx, &claude_bash(s, &repo, "b2", "npx jest", JEST_PASS), false);
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));

    let turns = of_kind(&server, "turn-verification");
    assert_eq!(turns.len(), 1);
    let t = &turns[0];
    assert_eq!(t["editsAccepted"], 3);
    assert_eq!(t["sourceFilesChanged"], 1);
    assert_eq!(t["testFilesChanged"], 1);
    assert_eq!(t["testFirst"], true);
    assert_eq!(t["testRuns"], 2);
    assert_eq!(t["endedGreen"], true);
    assert_eq!(t["editsAfterLastGreen"], 0);
    assert_eq!(t["failingRunStreakMax"], 1);
    assert_eq!(t["reworkFiles"], 0);
    assert_eq!(t["sensitiveUntested"], false);
    assert_eq!(t["weakenedFlags"], json!([]));
    assert_eq!(t["weakenedCaught"], 0);
    // Agreed with the backend: subagent tool uses in this turn.
    assert_eq!(t["subagentCount"], 1);
    assert_eq!(t["pathClass"], Value::Null);
    // The accepted edits saw the test run.
    assert!(of_kind(&server, "edit-decision").iter().all(|e| e["testsRun"] == true));
    assert_no_poison(&server);
    assert_only_whitelisted(&server);

    // A second Stop with nothing new sends no second verification.
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));
    assert_eq!(of_kind(&server, "turn-verification").len(), 1);
}

#[test]
fn a_skip_added_to_a_test_file_is_flagged_and_the_text_never_leaves() {
    let (env, server, repo) = live_env(json!({}));
    let s = "weak-1";
    let old = "it('TESTNAME-should-charge-the-card', () => {\n  expect(charge(SECRETVALUE_hunter2)).toBe(true)\n})";
    let new =
        "it.skip('TESTNAME-should-charge-the-card', () => {\n  expect(charge(SECRETVALUE_hunter2)).toBe(true)\n})";
    on_post_tool_use(
        &env.ctx,
        &claude_edit(s, &repo, "e1", "src/billing/charge.test.ts", old, new, old),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &claude_bash(
            s,
            &repo,
            "b1",
            "git commit --no-verify -m 'BRANCHNAME-feat-secret-project'",
            "[main abc] x",
        ),
        false,
    );
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));
    let t = &of_kind(&server, "turn-verification")[0];
    assert_eq!(t["weakenedFlags"], json!(["no-verify", "skip-added"]));
    assert_eq!(t["weakenedCaught"], 0);
    assert_eq!(t["sensitiveUntested"], false, "a tests path is not sensitive");
    assert_eq!(t["endedGreen"], false);
    assert!(!server.everything_sent().contains("SECRETVALUE_hunter2"));
    assert_no_poison(&server);
}

#[test]
fn git_restore_of_an_agent_edited_file_is_a_reverted_decision_and_catches_its_flags() {
    let (env, server, repo) = live_env(json!({}));
    let s = "revert-1";
    let old = "expect(a).toBe(1)\nexpect(b).toBe(2)";
    on_post_tool_use(
        &env.ctx,
        &claude_edit(s, &repo, "e1", "src/auth/login.test.ts", old, "expect(a).toBe(1)", old),
        false,
    );
    on_post_tool_use(
        &env.ctx,
        &claude_bash(s, &repo, "b1", "git restore src/auth/login.test.ts", ""),
        false,
    );
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));
    let reverted: Vec<Value> = of_kind(&server, "edit-decision")
        .into_iter()
        .filter(|e| e["decision"] == "reverted")
        .collect();
    assert_eq!(reverted.len(), 1);
    assert_eq!(reverted[0]["pathClass"], "tests");
    assert_eq!(
        reverted[0]["eventId"], "rv:revert-1:b1:0",
        "the id names the tool call, never the file"
    );
    let t = &of_kind(&server, "turn-verification")[0];
    assert_eq!(t["weakenedFlags"], json!(["assertion-removed"]));
    assert_eq!(t["weakenedCaught"], 1);
    assert_no_poison(&server);
}

#[test]
fn an_edit_that_failed_to_apply_is_not_an_accepted_edit() {
    let (env, server, repo) = live_env(json!({}));
    let s = "edit-fail";
    on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": s, "cwd": repo, "tool_name": "Edit", "tool_use_id": "e1",
                 "tool_input": { "file_path": repo.join("src/a.ts"), "old_string": "x", "new_string": "y" },
                 "error": "String to replace not found in file." }),
        true,
    );
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));
    assert!(of_kind(&server, "edit-decision").is_empty());
    assert!(of_kind(&server, "turn-verification").is_empty());
    assert_eq!(of_kind(&server, "tool-use").len(), 1);
}

#[test]
fn a_declined_edit_reported_as_a_failure_is_a_rejection_and_a_caught_flag() {
    let (env, server, repo) = live_env(json!({}));
    let s = "decline";
    on_post_tool_use(
        &env.ctx,
        &json!({ "session_id": s, "cwd": repo, "tool_name": "Edit", "tool_use_id": "e1",
                 "tool_input": { "file_path": repo.join("src/x.test.ts"), "old_string": "it('a', f)", "new_string": "it.only('a', f)" },
                 "error": "The user doesn't want to proceed with this tool use." }),
        true,
    );
    on_post_tool_use(&env.ctx, &claude_edit(s, &repo, "e2", "src/x.ts", "a", "b", "a"), false);
    on_stop(&env.ctx, &json!({ "session_id": s, "cwd": repo }));
    let decisions = of_kind(&server, "edit-decision");
    assert!(
        decisions
            .iter()
            .any(|e| e["decision"] == "rejected" && e["eventId"] == "ed:decline:e1")
    );
    let t = &of_kind(&server, "turn-verification")[0];
    assert_eq!(t["editsAccepted"], 1);
    assert_eq!(t["weakenedFlags"], json!(["only-added"]));
    assert_eq!(t["weakenedCaught"], 1);
}

#[test]
fn a_sensitive_edit_without_a_passing_run_is_sensitive_untested() {
    let (env, server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_edit("sens", &repo, "e1", "src/auth/jwt.ts", "a", "b", "a"),
        false,
    );
    on_session_end(&env.ctx, &json!({ "session_id": "sens", "cwd": repo }));
    // No Stop: SessionEnd still settles the turn.
    let t = &of_kind(&server, "turn-verification")[0];
    assert_eq!(t["sensitiveUntested"], true);
    assert_eq!(t["editsAfterLastGreen"], t["editsAccepted"]);
}

#[test]
fn the_receipt_lists_test_runs_and_turns_as_enums_and_counts() {
    let (env, _server, repo) = live_env(json!({}));
    on_post_tool_use(
        &env.ctx,
        &claude_bash_failure("rc", &repo, "b1", "npx jest", JEST_FAIL, 1),
        true,
    );
    let rows = env.ctx.store.read_ledger(&today());
    let run = rows.iter().find(|r| r.summary.starts_with("Test run")).unwrap();
    // Jest prints skipped only when there are some, so its absence is a real zero.
    assert_eq!(
        run.summary,
        "Test run failed, runner jest, 2 passed, 1 failed, 0 skipped"
    );
    assert!(run.fields_sent.contains(&"testOutcome".to_string()));
    let text = serde_json::to_string(&rows).unwrap();
    for poison in POISON {
        assert!(!text.contains(poison), "the ledger holds {poison}");
    }
}

#[test]
fn the_session_file_never_holds_a_command_output_or_edit_text() {
    let (env, _server, repo) = live_env(json!({}));
    let s = "local-1";
    on_post_tool_use(&env.ctx, &hostile_payload(s, &repo), false);
    on_post_tool_use(
        &env.ctx,
        &claude_bash_failure(s, &repo, "b1", "npx jest TESTNAME-should-charge-the-card", JEST_FAIL, 1),
        true,
    );
    let text = std::fs::read_to_string(env.ctx.store.dir.join(format!("sessions/{s}.json"))).unwrap();
    for poison in POISON {
        assert!(!text.contains(poison), "the session file holds {poison}");
    }
    assert!(!text.contains("src/auth/session.ts"), "files are keyed by hash");
}
