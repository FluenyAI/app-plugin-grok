// The hooks. All of type "command", because extraction is local.
//
// A hook of type "http" would post whatever the host hands it straight to a URL.
// That is the wrong shape for `/events`, so the batch would be dropped as malformed
// behind a 202, and it would put the raw tool_input and tool_response on the wire,
// which is the one thing this product promises never happens (eng findings 13 and
// 15, CEO decisions 8A and 33A).
//
// Nothing here writes to stdout. The host feeds a SessionStart hook's stdout into
// the model's context, so a client that printed its status would be injecting text
// into the developer's session. The receipt surfaces are CLI commands instead.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::api::{current_token, is_ok, post_insight, post_live_feedback, post_raw_activity};
use crate::classify::classify_path;
use crate::context::Ctx;
use crate::decline::looks_declined;
use crate::extract::{
    BashResult, Payload, ToolFacts, bash_result, command_in, duration_ms, edit_texts, extract_tool_facts, file_path_in,
    original_content, to_repo_relative, tool_input,
};
use crate::prompt_insight::sweep_prompt_insight_turns;
use crate::queue::{enqueue, flush};
use crate::receipt::{record_insight, record_today};
use crate::revert::{Reverted, file_key, track_command, track_edit};
use crate::session::{begin_session, classifier_for, ensure_session};
use crate::signal::testrun::{TestRunner, command_key, detect_test_command, outcome, parse_summary};
use crate::signal::weakened::{flags_for_command, flags_for_edit, is_test_path};
use crate::store::{PendingEdit, SessionState};
use crate::time::{now_iso, now_ms, today};
use crate::transcript::sweep_transcript;
use crate::types::{CodingEvent, InsightSubmission, LiveFeedbackSubmission, RawActivityDetail, ToolActivityEntry};
use crate::verify::{is_sensitive, turn_verification};

// Feature 0098. The opportunistic flush from PostToolUse makes the live feed live,
// but it runs on the critical path of EVERY tool call, so a slow or unreachable
// backend costs at most this much, once per tool call. A failed live flush leaves
// the event queued for the next natural flush point.
pub const LIVE_FLUSH_TIMEOUT_MS: u64 = 400;

// Feature 0109. Coaching context for one turn, not an audit log: past the cap,
// later calls in the same turn are simply not added.
const MAX_TURN_TOOL_ACTIVITY: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookOutcome {
    pub sent: usize,
    pub inert: bool,
    pub reason: Option<String>,
}

fn inert(state: &SessionState) -> HookOutcome {
    HookOutcome {
        sent: 0,
        inert: true,
        reason: state.inert_reason.clone(),
    }
}

pub fn on_session_start(ctx: &Ctx, payload: &Value) -> HookOutcome {
    let payload = as_payload(payload);
    let state = begin_session(ctx, &session_id_of(payload), &cwd_of(payload)).state;
    // Anything left from a previous session goes out now, the one moment the
    // developer is not waiting on a tool call.
    if !state.inert {
        flush(ctx, None);
    }
    HookOutcome {
        sent: 0,
        inert: state.inert,
        reason: state.inert_reason,
    }
}

/// PostToolUse, and (feature 0126) PostToolUseFailure with `failure` set. Under
/// Claude Code a Bash call that exits non-zero fires only the failure event, so
/// without it a failing test run, the most important one, was never seen.
pub fn on_post_tool_use(ctx: &Ctx, payload: &Value, failure: bool) -> HookOutcome {
    let payload = as_payload(payload);
    let session_id = session_id_of(payload);
    let cwd = cwd_of(payload);
    let state = ensure_session(ctx, &session_id, &cwd);
    if state.inert {
        return inert(&state);
    }

    let classifier = classifier_for(ctx);
    let facts = extract_tool_facts(
        payload,
        state.repo_root.as_deref(),
        &classifier,
        state.raw_activity_enabled,
    );
    // A failure whose error is a decline: the developer said no. Anything else
    // that failed (an old_string that was not found) never changed a file.
    let declined = facts.declined || (failure && looks_declined(payload.get("error")));
    let local = LocalSignal::read(ctx, payload, &facts, failure, state.repo_root.as_deref(), &classifier);

    let mut events: Vec<CodingEvent> = Vec::new();
    let mut raw_event_id: Option<String> = None;
    ctx.store.with_session_lock(&session_id, || {
        let mut live = ctx.store.read_session(&session_id).unwrap_or_else(|| state.clone());
        let id = facts.tool_use_id.clone().unwrap_or_else(|| next_seq_id(&mut live));
        if live.seen_tool_use_ids.contains(&id) {
            return;
        }
        live.seen_tool_use_ids.push(id.clone());
        live.tool_uses += 1;
        if facts.kind == "subagent" {
            live.subagents += 1;
            live.turn.subagents += 1;
        }
        if facts.is_test_command {
            live.tests_ran_this_turn = true;
        }

        let at = now_iso();
        let event_id = format!(
            "{}:{session_id}:{id}",
            if facts.kind == "subagent" { "sa" } else { "tu" }
        );
        raw_event_id = Some(event_id.clone());
        let mut event = CodingEvent::new(
            event_id,
            facts.kind,
            at.clone(),
            live.repo_id.clone(),
            facts.path_class.clone(),
        );
        event.tool_category = Some(facts.tool_category.to_string());
        event.command_category = facts.command_category.map(str::to_string);
        events.push(event);

        if live.turn_tool_activity.len() < MAX_TURN_TOOL_ACTIVITY {
            live.turn_tool_activity.push(ToolActivityEntry {
                tool_category: facts.tool_category.to_string(),
                path_class: facts.path_class.clone(),
                raw_path: facts.raw_path.clone(),
                raw_command: facts.raw_command.clone(),
            });
        }

        if facts.is_edit {
            if declined {
                // The rare shape where a hook fires on a decline. The usual case is
                // caught by the transcript sweep at Stop, and the shared eventId
                // keeps the two from counting the same edit twice. A weakened change
                // the developer refused is a caught one.
                let mut rejected = CodingEvent::new(
                    format!("ed:{session_id}:{id}"),
                    "edit-decision",
                    at.clone(),
                    live.repo_id.clone(),
                    facts.path_class.clone(),
                );
                rejected.decision = Some("rejected".into());
                events.push(rejected);
                live.turn
                    .record_flags(local.file.clone(), local.edit_flags.clone(), true);
            } else if !failure {
                // Held until Stop: `testsRun` is only knowable after the turn.
                live.pending_edits.push(PendingEdit {
                    tool_use_id: id.clone(),
                    at: at.clone(),
                    path_class: facts.path_class.clone(),
                });
                live.turn.record_edit(
                    local.file.clone(),
                    local.is_test_file,
                    is_sensitive(facts.path_class.as_deref()),
                );
                live.turn
                    .record_flags(local.file.clone(), local.edit_flags.clone(), false);
                if let Some(rel) = &local.rel
                    && let Some(reverted) = track_edit(
                        &mut live,
                        rel,
                        facts.path_class.clone(),
                        local.original.clone(),
                        &local.absolute,
                    )
                {
                    events.push(reverted_event(&mut live, &session_id, &id, 0, &reverted));
                }
            }
        }

        if let Some(run) = &local.test_run {
            let mut test = CodingEvent::new(
                format!("tr:{session_id}:{id}"),
                "test-run",
                at.clone(),
                live.repo_id.clone(),
                None,
            );
            test.test_outcome = Some(run.outcome.to_string());
            test.test_runner = Some(run.runner.to_string());
            test.tests_passed = run.passed;
            test.tests_failed = run.failed;
            test.tests_skipped = run.skipped;
            test.duration_ms = duration_ms(payload);
            events.push(test);
            live.turn.record_run(run.outcome, run.key.clone());
        }

        if local.command_succeeded {
            live.turn
                .record_flags(String::new(), local.command_flags.clone(), false);
            if let Some(command) = &local.command {
                let reverted = track_command(&mut live, command, &cwd);
                for (index, r) in reverted.iter().enumerate() {
                    events.push(reverted_event(&mut live, &session_id, &id, index, r));
                }
            }
        }
        ctx.store.write_session(&mut live);
    });

    let queued = enqueue(
        ctx,
        ctx.agent,
        &session_id,
        events.iter().map(CodingEvent::to_value).collect(),
    );
    record_today(&ctx.store, &queued, 1);
    if !queued.is_empty() {
        flush(ctx, Some(LIVE_FLUSH_TIMEOUT_MS));
    }

    // Feature 0109. Same critical-path discipline as the live flush, best effort,
    // never queued to disk, and only when this call was not a dedupe skip.
    if let Some(event_id) = raw_event_id
        && (facts.raw_path.is_some() || facts.raw_command.is_some())
        && let Some(creds) = current_token(ctx, ctx.agent)
    {
        let detail = RawActivityDetail {
            session_id: session_id.clone(),
            event_id,
            at: now_iso(),
            raw_path: facts.raw_path.clone(),
            raw_command: facts.raw_command.clone(),
        };
        if let Ok(body) = serde_json::to_value(&detail) {
            post_raw_activity(&creds.api_url, &creds.access_token, &body, LIVE_FLUSH_TIMEOUT_MS);
        }
    }
    HookOutcome {
        sent: queued.len(),
        inert: false,
        reason: None,
    }
}

// The id is the tool call that caused the revert plus its index in that call,
// never anything derived from the file: a hash of a path is still a path to anyone
// holding a list of likely ones.
fn reverted_event(
    live: &mut SessionState,
    session_id: &str,
    tool_use_id: &str,
    index: usize,
    reverted: &Reverted,
) -> CodingEvent {
    live.turn.catch_file(&reverted.file);
    let mut event = CodingEvent::new(
        format!("rv:{session_id}:{tool_use_id}:{index}"),
        "edit-decision",
        now_iso(),
        live.repo_id.clone(),
        reverted.path_class.clone(),
    );
    event.decision = Some("reverted".into());
    event
}

/// Everything the 0126 detectors derive from this one tool call. Built from the
/// raw payload before the lock is taken, and dropped when the hook returns: the
/// command and the output never reach the session file or the queue.
struct LocalSignal {
    rel: Option<String>,
    file: String,
    absolute: PathBuf,
    is_test_file: bool,
    original: Option<Option<String>>,
    edit_flags: Vec<String>,
    command: Option<String>,
    command_flags: Vec<String>,
    command_succeeded: bool,
    test_run: Option<TestRun>,
}

struct TestRun {
    outcome: &'static str,
    runner: &'static str,
    passed: Option<i64>,
    failed: Option<i64>,
    skipped: Option<i64>,
    key: String,
}

impl LocalSignal {
    fn read(
        ctx: &Ctx,
        payload: &Payload,
        facts: &ToolFacts,
        failure: bool,
        repo_root: Option<&str>,
        classifier: &Map<String, Value>,
    ) -> LocalSignal {
        let input = tool_input(payload);
        let rel = file_path_in(input)
            .map(|p| to_repo_relative(p, repo_root))
            .filter(|r| !r.is_empty());
        let absolute = match (repo_root, &rel) {
            (Some(root), Some(rel)) => Path::new(root).join(rel),
            _ => PathBuf::new(),
        };
        let is_test_file = rel
            .as_deref()
            .is_some_and(|r| is_test_path(r, facts.path_class.as_deref()));
        let edit_flags = match (facts.is_edit, &rel, edit_texts(payload)) {
            (true, Some(rel), Some((old, new))) => flags_for_edit(rel, facts.path_class.as_deref(), &old, &new)
                .iter()
                .map(|f| f.as_str().to_string())
                .collect(),
            _ => Vec::new(),
        };

        let mut signal = LocalSignal {
            file: rel.as_deref().map(file_key).unwrap_or_default(),
            rel,
            absolute,
            is_test_file,
            original: original_content(payload),
            edit_flags,
            command: None,
            command_flags: Vec::new(),
            command_succeeded: false,
            test_run: None,
        };
        if facts.tool_category != "bash" {
            return signal;
        }
        let Some(command) = command_in(input) else {
            return signal;
        };
        let result: BashResult = bash_result(payload, failure, ctx.agent);
        signal.command_succeeded = !failure && result.exit_code.unwrap_or(0) == 0 && !result.interrupted;
        let is_test = |path: &str| {
            let rel = to_repo_relative(path, repo_root);
            let rel = if rel.is_empty() { path.to_string() } else { rel };
            is_test_path(&rel, classify_path(classifier, &rel).as_deref())
        };
        signal.command_flags = flags_for_command(command, &is_test)
            .iter()
            .map(|f| f.as_str().to_string())
            .collect();
        signal.command = Some(command.to_string());

        // A background command has not finished, so it has no outcome to report.
        if let Some(invocation) = detect_test_command(command).filter(|_| !result.background) {
            let summary = parse_summary(invocation.runner, &result.output);
            let counts = summary.as_ref().map(|(_, c)| *c);
            let runner = summary
                .as_ref()
                .map(|(r, _)| *r)
                .or(invocation.runner)
                .unwrap_or(TestRunner::Other);
            signal.test_run = Some(TestRun {
                outcome: outcome(
                    result.exit_code,
                    invocation.exit_trust,
                    counts.as_ref(),
                    result.interrupted,
                )
                .as_str(),
                runner: runner.as_str(),
                passed: counts.and_then(|c| c.passed).map(i64::from),
                failed: counts.and_then(|c| c.failed).map(i64::from),
                skipped: counts.and_then(|c| c.skipped).map(i64::from),
                key: crate::revert::content_hash(command_key(command).as_bytes()),
            });
        }
        signal
    }
}

pub fn on_stop(ctx: &Ctx, payload: &Value) -> HookOutcome {
    let payload = as_payload(payload);
    let session_id = session_id_of(payload);
    let cwd = cwd_of(payload);
    let transcript = string_field(payload, &["transcript_path", "transcriptPath"]).map(PathBuf::from);
    let state = ensure_session(ctx, &session_id, &cwd);
    if state.inert {
        return inert(&state);
    }

    let mut events: Vec<CodingEvent> = Vec::new();
    // Feature 0094 / 0098. Built inside the lock (turnId material must not race a
    // concurrent Stop), sent outside it (a network call must never hold the lock).
    let mut submissions: Vec<InsightSubmission> = Vec::new();
    let mut to_insights = false;
    let mut to_live_feedback = false;
    let mut tool_activity: Vec<ToolActivityEntry> = Vec::new();
    let mut rejected = 0i64;
    ctx.store.with_session_lock(&session_id, || {
        let mut live = ctx.store.read_session(&session_id).unwrap_or_else(|| state.clone());
        events.extend(settle_edits(&mut live));
        to_insights = live.prompt_insights_enabled;
        to_live_feedback = live.live_feedback_enabled;
        // Snapshot and reset every Stop, whatever the opt-ins say, so switching
        // one on mid-session never inherits a stale backlog.
        tool_activity = std::mem::take(&mut live.turn_tool_activity);

        if let Some(transcript) = &transcript {
            let classifier = classifier_for(ctx);
            let sweep = sweep_transcript(
                transcript,
                live.transcript_offset,
                live.repo_root.as_deref(),
                &classifier,
            );
            live.transcript_offset = sweep.offset;
            for rejection in sweep.rejections {
                let event_id = format!("ed:{session_id}:{}", rejection.tool_use_id);
                if live.seen_tool_use_ids.contains(&event_id) {
                    continue;
                }
                live.seen_tool_use_ids.push(event_id.clone());
                let mut event = CodingEvent::new(
                    event_id,
                    "edit-decision",
                    now_iso(),
                    live.repo_id.clone(),
                    rejection.path_class,
                );
                event.decision = Some("rejected".into());
                events.push(event);
                rejected += 1;
            }

            // Only when at least one of the two policies is on. Everyone else never
            // reaches this block, never reads a message body.
            if to_insights || to_live_feedback {
                let sweep = sweep_prompt_insight_turns(transcript, live.prompt_insight_line_offset);
                live.prompt_insight_line_offset = sweep.line_offset;
                for turn in sweep.turns {
                    live.prompt_insight_seq += 1;
                    submissions.push(InsightSubmission {
                        session_id: session_id.clone(),
                        turn_id: format!("insight:{session_id}:{}", live.prompt_insight_seq),
                        repo_id: live.repo_id.clone(),
                        path_class: None,
                        prompt: turn.prompt,
                        response: turn.response,
                        at: now_iso(),
                    });
                }
            }
        }
        events.extend(settle_turn(&mut live));
        ctx.store.write_session(&mut live);
    });

    let queued = enqueue(
        ctx,
        ctx.agent,
        &session_id,
        events.iter().map(CodingEvent::to_value).collect(),
    );
    // A declined tool call is still a tool call this client looked at, and the
    // receipt's `observed` count is every one of them. PostToolUse never fires for
    // one, so it is counted here.
    record_today(&ctx.store, &queued, rejected);
    flush(ctx, None);
    send_turn_submissions(ctx, &submissions, to_insights, to_live_feedback, tool_activity);
    HookOutcome {
        sent: queued.len(),
        inert: false,
        reason: None,
    }
}

/// Feature 0094 / 0098 / 0109. One attempt per destination, never queued to disk:
/// `prompt` and `response` exist as values only for the span of this function.
/// `toolActivity` is attached only to the live-feedback copy; the insights DTO has
/// no field for it and would 400 the whole request under forbidNonWhitelisted.
fn send_turn_submissions(
    ctx: &Ctx,
    submissions: &[InsightSubmission],
    to_insights: bool,
    to_live_feedback: bool,
    tool_activity: Vec<ToolActivityEntry>,
) {
    if submissions.is_empty() || (!to_insights && !to_live_feedback) {
        return;
    }
    let day = today();
    let Some(creds) = current_token(ctx, ctx.agent) else {
        // The one local trace a failed insight leaves (feat/insight-receipt-visibility).
        if to_insights {
            for submission in submissions {
                if let Ok(body) = serde_json::to_value(submission) {
                    record_insight(&ctx.store, &body, false, &day);
                }
            }
        }
        return;
    };
    for submission in submissions {
        if to_insights && let Ok(body) = serde_json::to_value(submission) {
            let res = post_insight(&creds.api_url, &creds.access_token, &body);
            record_insight(&ctx.store, &body, is_ok(res.status), &day);
        }
        if to_live_feedback {
            let feedback = LiveFeedbackSubmission {
                session_id: submission.session_id.clone(),
                turn_id: submission.turn_id.clone(),
                repo_id: submission.repo_id.clone(),
                path_class: submission.path_class.clone(),
                prompt: submission.prompt.clone(),
                response: submission.response.clone(),
                at: submission.at.clone(),
                tool_activity: (!tool_activity.is_empty()).then(|| tool_activity.clone()),
            };
            if let Ok(body) = serde_json::to_value(&feedback) {
                post_live_feedback(&creds.api_url, &creds.access_token, &body);
            }
        }
    }
}

pub fn on_session_end(ctx: &Ctx, payload: &Value) -> HookOutcome {
    let payload = as_payload(payload);
    let session_id = session_id_of(payload);
    let state = ensure_session(ctx, &session_id, &cwd_of(payload));
    if state.inert {
        ctx.store.clear_session(&session_id);
        return inert(&state);
    }
    let mut events: Vec<CodingEvent> = Vec::new();
    ctx.store.with_session_lock(&session_id, || {
        let mut live = ctx.store.read_session(&session_id).unwrap_or_else(|| state.clone());
        events.extend(settle_edits(&mut live));
        // A session that ends without a final Stop (an interrupt, a closed
        // terminal) still has a turn to account for.
        events.extend(settle_turn(&mut live));
        let mut end = CodingEvent::new(
            format!("se:{session_id}"),
            "session-end",
            now_iso(),
            live.repo_id.clone(),
            None,
        );
        end.subagent_count = Some(live.subagents);
        end.duration_ms = Some((now_ms() - live.started_at).max(0));
        events.push(end);
        ctx.store.write_session(&mut live);
    });
    let queued = enqueue(
        ctx,
        ctx.agent,
        &session_id,
        events.iter().map(CodingEvent::to_value).collect(),
    );
    record_today(&ctx.store, &queued, 0);
    flush(ctx, None);
    ctx.store.clear_session(&session_id);
    HookOutcome {
        sent: queued.len(),
        inert: false,
        reason: None,
    }
}

/// Turns the turn's buffered edits into decisions, now that whether tests ran is
/// known. An edit that reached PostToolUse was applied, which is what "accepted"
/// means here.
fn settle_edits(live: &mut SessionState) -> Vec<CodingEvent> {
    let events = live
        .pending_edits
        .iter()
        .map(|edit| {
            let mut event = CodingEvent::new(
                format!("ed:{}:{}", live.session_id, edit.tool_use_id),
                "edit-decision",
                edit.at.clone(),
                live.repo_id.clone(),
                edit.path_class.clone(),
            );
            event.decision = Some("accepted".into());
            event.tests_run = Some(live.tests_ran_this_turn);
            event
        })
        .collect();
    live.pending_edits.clear();
    live.tests_ran_this_turn = false;
    events
}

/// Feature 0126. One turn-verification per turn that made an accepted edit, then
/// a fresh turn.
fn settle_turn(live: &mut SessionState) -> Vec<CodingEvent> {
    let turn = std::mem::take(&mut live.turn);
    live.turn_seq += 1;
    let base = CodingEvent::new(
        format!("tv:{}:{}", live.session_id, live.turn_seq),
        "turn-verification",
        now_iso(),
        live.repo_id.clone(),
        None,
    );
    turn_verification(&turn, base).into_iter().collect()
}

// The dedupe key is the host's tool_use_id when it sends one. When it does not,
// a session-scoped sequence, stable across a queue replay.
fn next_seq_id(live: &mut SessionState) -> String {
    live.event_seq += 1;
    format!("seq-{}", live.event_seq)
}

fn as_payload(value: &Value) -> &Payload {
    static EMPTY: std::sync::LazyLock<Payload> = std::sync::LazyLock::new(Map::new);
    value.as_object().unwrap_or(&EMPTY)
}

fn session_id_of(payload: &Payload) -> String {
    string_field(payload, &["session_id", "sessionId"]).unwrap_or_else(|| format!("local-{}", now_ms()))
}

fn cwd_of(payload: &Payload) -> PathBuf {
    string_field(payload, &["cwd", "workspaceRoot"])
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default()
}

fn string_field(payload: &Payload, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| payload.get(*key).and_then(Value::as_str).filter(|s| !s.is_empty()))
        .map(str::to_string)
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
