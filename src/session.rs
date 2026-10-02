// The SessionStart handshake (CEO decision 4A). The client runs on machines Flueny
// does not control, so the server states on every session whether it should run
// at all, what it is allowed to look at, and what it is capable of.
//
// Three things make the client inert, and all three mean the same thing on the
// wire, which is nothing at all:
//
//   no credential      nobody has connected this machine yet
//   kill switch        the org turned the surface off (CEO decision 4A)
//   repo not allowed   CEO decision 14A, and it is FAIL CLOSED: an empty
//                      allowlist matches nothing, so a misconfigured org leaks
//                      no repository rather than every repository
//
// Inert is not an error. A developer who has not connected Flueny, and a developer
// whose org killed it, both get an editor that behaves exactly as if this client
// were not installed.

use std::path::Path;

use serde_json::{Map, Value, json};

use crate::api::{CLIENT_VERSION, HttpResult, current_token, is_ok, refresh, session_start};
use crate::context::Ctx;
use crate::credentials::Credentials;
use crate::git::find_repo;
use crate::reads::reads_locally_declaration;
use crate::repo_id::repo_id_for;
use crate::store::SessionState;
use crate::time::now_ms;
use crate::types::{PolicyBundle, SessionStartResponse};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleSource {
    Server,
    Cache,
    Refetched,
    None,
}

pub struct BeginResult {
    pub state: SessionState,
    pub handshake: Option<SessionStartResponse>,
    pub bundle_source: BundleSource,
}

pub fn begin_session(ctx: &Ctx, session_id: &str, cwd: &Path) -> BeginResult {
    let repo = find_repo(cwd);
    let repo_id = repo
        .as_ref()
        .and_then(|r| r.remote.as_deref())
        .map(repo_id_for)
        .filter(|id| !id.is_empty());
    let base = SessionState {
        session_id: session_id.to_string(),
        agent: Some(ctx.agent),
        started_at: now_ms(),
        inert: true,
        repo_id: repo_id.clone(),
        repo_root: repo.as_ref().map(|r| r.root.to_string_lossy().to_string()),
        ..SessionState::default()
    };

    let Some(creds) = current_token(ctx, ctx.agent) else {
        return finish(
            ctx,
            SessionState {
                inert_reason: Some("not connected: run flueny login".into()),
                ..base
            },
            None,
            BundleSource::None,
        );
    };

    let cached = ctx.store.read_bundle();
    let (mut res, creds) = run_handshake(ctx, creds, session_id, cached.as_ref().map(|b| b.etag.clone()));
    let Some(mut answer) = res
        .body
        .as_ref()
        .filter(|_| is_ok(res.status))
        .and_then(SessionStartResponse::from_value)
    else {
        // A handshake that did not answer is not a reason to guess. Inert,
        // quietly, and the next session tries again.
        return finish(
            ctx,
            SessionState {
                inert_reason: Some(format!("handshake unavailable ({})", res.status)),
                ..base
            },
            None,
            BundleSource::None,
        );
    };

    let mut bundle_source = BundleSource::None;
    let mut bundle: Option<PolicyBundle> = answer.bundle.clone();
    if let Some(fresh) = &bundle {
        ctx.store.write_bundle(fresh);
        bundle_source = BundleSource::Server;
    } else if let Some(cached) = cached {
        // The steady state, and the whole point of CEO decision 25A: a session
        // start costs one small response once the client already holds the bundle.
        bundle = Some(cached);
        bundle_source = BundleSource::Cache;
    } else {
        // Null bundle with nothing cached is exactly the shape a stale or
        // hand-edited etag produces, and the failure is silent: no classifier means
        // every pathClass is null. So ask once more with no etag rather than run blind.
        res = run_handshake(ctx, creds, session_id, None).0;
        if let Some(retry) = res
            .body
            .as_ref()
            .filter(|_| is_ok(res.status))
            .and_then(SessionStartResponse::from_value)
            && let Some(refetched) = retry.bundle.clone()
        {
            ctx.store.write_bundle(&refetched);
            bundle = Some(refetched);
            answer = retry;
            bundle_source = BundleSource::Refetched;
        }
    }

    // Written from the answer whenever there is one, independent of whether this
    // session ends up inert for an unrelated reason, so `flueny status` reflects
    // the org's actual policy for this developer. Fail closed on absence.
    ctx.store.write_flag("prompt-insights", answer.prompt_insights_enabled);
    ctx.store.write_flag("live-feedback", answer.live_feedback_enabled);
    ctx.store.write_flag("raw-activity", answer.raw_activity_enabled);
    ctx.store
        .write_flag("weekly-limit", answer.weekly_limit_reporting_enabled);

    let state = SessionState {
        kill_switch: answer.kill_switch,
        dry_run: answer.dry_run,
        dry_run_ends_at: answer.dry_run_ends_at.clone(),
        prompt_insights_enabled: answer.prompt_insights_enabled,
        live_feedback_enabled: answer.live_feedback_enabled,
        raw_activity_enabled: answer.raw_activity_enabled,
        weekly_limit_reporting_enabled: answer.weekly_limit_reporting_enabled,
        ..base
    };
    let inert = |reason: &str| SessionState {
        inert_reason: Some(reason.to_string()),
        ..state.clone()
    };

    let result = if answer.kill_switch {
        inert("kill switch is on for this organisation")
    } else if repo_id.is_none() {
        inert("no git remote here, so this is not an org repository")
    } else if !repo_id.as_ref().is_some_and(|id| answer.repo_allowlist.contains(id)) {
        // Fail closed: `contains` on an empty list is false, which is the entire
        // guarantee. An org that registered nothing receives nothing.
        inert("this repository is not on the org allowlist")
    } else if bundle.is_none() {
        inert("no path classifier available")
    } else {
        SessionState {
            inert: false,
            inert_reason: None,
            ..state.clone()
        }
    };
    finish(ctx, result, Some(answer), bundle_source)
}

/// The handshake request. `readsLocally` is design decision 57's client-declared
/// list, attributed by the agent and clientVersion on the same request.
pub fn handshake_request(ctx: &Ctx, session_id: &str, bundle_etag: Option<String>) -> Value {
    json!({
        "agent": ctx.agent.as_str(),
        "sessionId": session_id,
        "clientVersion": CLIENT_VERSION,
        "bundleEtag": bundle_etag,
        "readsLocally": reads_locally_declaration(),
    })
}

// `/session/start` carries the same hook token as `/events`, so it 401s the same
// way, and a 401 there is the ONE failure this surface deliberately shows a client.
// Without this retry a rejected token makes the client inert for every session
// after it, silently, because the clock-based refresh still thinks it is fine.
fn run_handshake(ctx: &Ctx, creds: Credentials, session_id: &str, etag: Option<String>) -> (HttpResult, Credentials) {
    let request = handshake_request(ctx, session_id, etag);
    let res = session_start(&creds.api_url, &creds.access_token, &request);
    if res.status != 401 {
        return (res, creds);
    }
    match refresh(ctx, &creds, ctx.agent) {
        Some(renewed) => (
            session_start(&renewed.api_url, &renewed.access_token, &request),
            renewed,
        ),
        None => (res, creds),
    }
}

fn finish(
    ctx: &Ctx,
    mut state: SessionState,
    handshake: Option<SessionStartResponse>,
    bundle_source: BundleSource,
) -> BeginResult {
    ctx.store.write_session(&mut state);
    BeginResult {
        state,
        handshake,
        bundle_source,
    }
}

/// Hooks fire in whatever order the host runs them, and the plugin can be
/// installed halfway through a session, so every later hook has to cope with no
/// state on disk. It handshakes rather than assuming.
pub fn ensure_session(ctx: &Ctx, session_id: &str, cwd: &Path) -> SessionState {
    if let Some(existing) = ctx.store.read_session(session_id) {
        return existing;
    }
    begin_session(ctx, session_id, cwd).state
}

pub fn classifier_for(ctx: &Ctx) -> Map<String, Value> {
    ctx.store.read_bundle().map(|b| b.path_classifier).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::on_post_tool_use;
    use crate::testing::{MockServer, Reply, TestEnv, bundle, handshake_body};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const REMOTE: &str = "git@github.com:FluenyAI/app-backend.git";

    fn edit_payload(session: &str, repo: &Path) -> Value {
        json!({ "session_id": session, "cwd": repo, "tool_name": "Edit", "tool_input": { "file_path": repo.join("src/app.ts") } })
    }

    #[test]
    fn the_kill_switch_makes_the_client_inert() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| {
            Reply::Json(
                200,
                handshake_body(json!({ "killSwitch": true, "repoAllowlist": [], "bundle": null })),
            )
        });
        env.connect(&server.url);
        let result = begin_session(&env.ctx, "kill-1", &repo);
        assert!(result.state.inert);
        assert!(result.state.kill_switch);
        assert!(result.state.inert_reason.unwrap().contains("kill switch"));

        // And inert means inert: a tool call after a killed handshake sends nothing.
        let outcome = on_post_tool_use(&env.ctx, &edit_payload("kill-1", &repo), false);
        assert_eq!(outcome.sent, 0);
        assert!(server.calls_to("/events").is_empty());
    }

    #[test]
    fn the_repo_allowlist_is_fail_closed() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| Reply::Json(200, handshake_body(json!({ "repoAllowlist": [] }))));
        env.connect(&server.url);
        let result = begin_session(&env.ctx, "allow-empty", &repo);
        assert!(result.state.inert);
        assert!(result.state.inert_reason.unwrap().contains("not on the org allowlist"));
        assert_eq!(
            result.state.repo_id,
            Some(repo_id_for(REMOTE)),
            "the id was derived, it just is not allowed"
        );
        assert_eq!(
            on_post_tool_use(&env.ctx, &edit_payload("allow-empty", &repo), false).sent,
            0
        );
        assert!(server.calls_to("/events").is_empty());
    }

    #[test]
    fn a_repository_with_no_git_remote_is_inert_never_sent_under_a_null_repo_id() {
        let env = TestEnv::new();
        let plain = env.dir.path().join("plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        std::fs::write(plain.join(".git/config"), "[core]\n").unwrap();
        let server = MockServer::start(|_, _| {
            Reply::Json(200, handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)] })))
        });
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "no-remote", &plain).state;
        assert!(state.inert);
        assert_eq!(state.repo_id, None);
        assert!(state.inert_reason.unwrap().contains("no git remote"));
    }

    #[test]
    fn an_allowlisted_repository_is_live() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| {
            Reply::Json(200, handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)] })))
        });
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "allow-ok", &repo).state;
        assert!(!state.inert);
        assert_eq!(state.inert_reason, None);
        assert_eq!(state.repo_id, Some(repo_id_for(REMOTE)));
        // The handshake carried the attribution and the 0.2.0 client version.
        let start = &server.calls_to("/session/start")[0];
        assert_eq!(start.body["clientVersion"], "0.2.0");
        assert_eq!(start.body["agent"], "claude-code");
        assert_eq!(start.body["readsLocally"], json!(reads_locally_declaration()));
    }

    #[test]
    fn the_bundle_etag_path_sent_then_cached_then_refetched() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let allow = json!({ "repoAllowlist": [repo_id_for(REMOTE)] });

        let first = MockServer::start({
            let allow = allow.clone();
            move |_, _| Reply::Json(200, handshake_body(allow.clone()))
        });
        env.connect(&first.url);
        let result = begin_session(&env.ctx, "etag-1", &repo);
        assert_eq!(result.bundle_source, BundleSource::Server);
        assert_eq!(first.calls()[0].body["bundleEtag"], Value::Null);

        let second = MockServer::start(|_, body| {
            assert_eq!(
                body["bundleEtag"], "etag-one",
                "the client did not offer its cached etag"
            );
            Reply::Json(
                200,
                handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)], "bundle": null })),
            )
        });
        env.connect(&second.url);
        let result = begin_session(&env.ctx, "etag-2", &repo);
        assert_eq!(result.bundle_source, BundleSource::Cache);
        assert!(
            !result.state.inert,
            "a cached bundle still has to make the session live"
        );
        assert_eq!(second.calls().len(), 1);

        env.ctx.store.forget_bundle();
        let answered = Arc::new(AtomicUsize::new(0));
        let third = MockServer::start({
            let answered = answered.clone();
            move |_, _| {
                let n = answered.fetch_add(1, Ordering::SeqCst);
                let b = if n == 0 { Value::Null } else { bundle() };
                Reply::Json(
                    200,
                    handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)], "bundle": b })),
                )
            }
        });
        env.connect(&third.url);
        let result = begin_session(&env.ctx, "etag-3", &repo);
        assert_eq!(result.bundle_source, BundleSource::Refetched);
        assert_eq!(third.calls().len(), 2);
        assert!(!result.state.inert);
    }

    #[test]
    fn a_handshake_that_does_not_answer_leaves_the_client_inert() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| Reply::Json(503, json!({})));
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "down", &repo).state;
        assert!(state.inert);
        assert!(state.inert_reason.unwrap().contains("handshake unavailable"));
    }

    #[test]
    fn a_rejected_hook_token_refreshes_and_retries_rather_than_going_inert_forever() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let handshakes = Arc::new(AtomicUsize::new(0));
        let server = MockServer::start({
            let handshakes = handshakes.clone();
            move |path, _| {
                if path.ends_with("/oauth/token") {
                    return Reply::Json(
                        200,
                        json!({ "access_token": "fresh", "refresh_token": "r2", "expires_in": 3600 }),
                    );
                }
                if handshakes.fetch_add(1, Ordering::SeqCst) == 0 {
                    Reply::Json(401, json!({ "message": "Invalid or expired hook token" }))
                } else {
                    Reply::Json(201, handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)] })))
                }
            }
        });
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "expired-token", &repo).state;
        assert!(!state.inert, "a refreshable 401 must not leave the client inert");
        assert_eq!(server.calls_to("/oauth/token").len(), 1);
        assert_eq!(handshakes.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_401_with_no_usable_refresh_token_is_inert_not_a_crash() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|path, _| {
            if path.ends_with("/oauth/token") {
                Reply::Json(400, json!({ "error": "invalid_grant" }))
            } else {
                Reply::Json(401, json!({}))
            }
        });
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "dead-token", &repo).state;
        assert!(state.inert);
        assert!(state.inert_reason.unwrap().contains("handshake unavailable (401)"));
    }

    #[test]
    fn the_handshake_accepts_the_201_nestjs_actually_returns() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| {
            Reply::Json(201, handshake_body(json!({ "repoAllowlist": [repo_id_for(REMOTE)] })))
        });
        env.connect(&server.url);
        assert!(!begin_session(&env.ctx, "created-201", &repo).state.inert);
    }

    #[test]
    fn every_opt_in_is_fail_closed_and_cached_for_status() {
        let env = TestEnv::new();
        let repo = env.make_repo(REMOTE);
        let server = MockServer::start(|_, _| {
            Reply::Json(
                200,
                handshake_body(json!({
                    "repoAllowlist": [repo_id_for(REMOTE)],
                    "promptInsightsEnabled": "true",
                    "liveFeedbackEnabled": true,
                    "weeklyLimitReportingEnabled": true,
                })),
            )
        });
        env.connect(&server.url);
        let state = begin_session(&env.ctx, "flags", &repo).state;
        assert!(!state.prompt_insights_enabled, "a string is not literally true");
        assert!(state.live_feedback_enabled);
        assert!(!state.raw_activity_enabled, "absent is off");
        assert!(state.weekly_limit_reporting_enabled);
        assert!(!env.ctx.store.read_flag("prompt-insights"));
        assert!(env.ctx.store.read_flag("live-feedback"));
    }
}
