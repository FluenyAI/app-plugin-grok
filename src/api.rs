// Every request this client makes, and the hook token's lifecycle.

use std::time::Duration;

use serde_json::{Value, json};

use crate::context::Ctx;
use crate::credentials::Credentials;
use crate::time::now_ms;
use crate::types::AgentId;

/// Sent at handshake as `clientVersion`, and the same number as plugin.json.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DEFAULT_CLIENT_ID: &str = "flueny-claude-code";
// Only a fallback: `login` prefers --api-url, then FLUENY_API_URL, and every hook
// afterwards reads the apiUrl stored in the credential, never this constant.
pub const DEFAULT_API_URL: &str = "https://api.flueny.dev";

// Every network call a hook makes is on the developer's critical path, so all of
// them are bounded. A Flueny outage must cost a couple of seconds once, never a
// hung editor: the client-side half of CEO decision 6A.
pub const HOOK_TIMEOUT_MS: u64 = 2500;
const LOGIN_TIMEOUT_MS: u64 = 10_000;

// Hook tokens are short lived by design (eng finding 5). Refreshing a minute early
// costs one round trip; refreshing on the 401 costs a round trip on a hook that is
// already holding up a tool call.
const REFRESH_MARGIN_MS: i64 = 60_000;

/// The backend is NestJS, so a POST with no explicit @HttpCode answers 201, not
/// 200. `/session/start` is one of those. Nothing here compares a status to a
/// literal success code.
pub fn is_ok(status: u16) -> bool {
    (200..300).contains(&status)
}

#[derive(Debug, Clone)]
pub struct HttpResult {
    // 0 when the request never got an answer (refused, timed out, DNS).
    pub status: u16,
    pub body: Option<Value>,
    pub text: String,
}

pub fn post(base: &str, path: &str, token: Option<&str>, body: &Value, timeout_ms: u64) -> HttpResult {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(timeout_ms)))
        .http_status_as_error(false)
        .user_agent(format!("flueny/{CLIENT_VERSION}"))
        .build()
        .into();
    let mut request = agent
        .post(format!("{base}{path}"))
        .header("content-type", "application/json");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    match request.send(body.to_string()) {
        Ok(mut response) => {
            let status = response.status().as_u16();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            let body = if text.is_empty() {
                None
            } else {
                serde_json::from_str(&text).ok()
            };
            HttpResult { status, body, text }
        }
        Err(err) => HttpResult {
            status: 0,
            body: None,
            text: err.to_string(),
        },
    }
}

// ---- OAuth device authorization grant (RFC 8628, CEO decision 15A) ----

pub fn start_device(base: &str, client_id: &str, agent: AgentId, device_label: &str) -> HttpResult {
    post(
        base,
        "/integrations/coding/oauth/device",
        None,
        &json!({ "clientId": client_id, "agent": agent.as_str(), "deviceLabel": device_label }),
        LOGIN_TIMEOUT_MS,
    )
}

pub fn exchange_device_code(base: &str, client_id: &str, device_code: &str) -> HttpResult {
    post(
        base,
        "/integrations/coding/oauth/token",
        None,
        &json!({
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "client_id": client_id,
            "device_code": device_code,
        }),
        LOGIN_TIMEOUT_MS,
    )
}

pub fn exchange_refresh_token(base: &str, client_id: &str, refresh_token: &str) -> HttpResult {
    post(
        base,
        "/integrations/coding/oauth/token",
        None,
        &json!({ "grant_type": "refresh_token", "client_id": client_id, "refresh_token": refresh_token }),
        LOGIN_TIMEOUT_MS,
    )
}

fn fresh(creds: &Credentials) -> bool {
    creds.expires_at - REFRESH_MARGIN_MS > now_ms()
}

/// The hook token, from the 0600 cache while it is fresh, so a hook only touches
/// the OS store (and can only raise a Keychain prompt) when it has to refresh.
pub fn current_token(ctx: &Ctx, agent: AgentId) -> Option<Credentials> {
    if let Some(cached) = ctx.creds.read_hook_token(agent)
        && fresh(&cached)
    {
        return Some(cached);
    }
    let creds = ctx.creds.read(agent)?;
    if fresh(&creds) {
        ctx.creds.cache_hook_token(&creds, agent);
        return Some(creds);
    }
    refresh(ctx, &creds, agent)
}

pub fn refresh(ctx: &Ctx, creds: &Credentials, agent: AgentId) -> Option<Credentials> {
    // A token from the cache carries no refresh token: that one only lives in the
    // store, so read it there.
    let stored;
    let creds = if creds.refresh_token.is_empty() {
        stored = ctx.creds.read(agent)?;
        &stored
    } else {
        creds
    };
    let res = exchange_refresh_token(&creds.api_url, &creds.client_id, &creds.refresh_token);
    let body = res.body.filter(|_| is_ok(res.status))?;
    let access = body.get("access_token")?.as_str()?.to_string();
    let next = Credentials {
        agent: Some(agent),
        access_token: access,
        refresh_token: body
            .get("refresh_token")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| creds.refresh_token.clone()),
        expires_at: now_ms() + body.get("expires_in").and_then(Value::as_i64).unwrap_or(3600) * 1000,
        ..creds.clone()
    };
    ctx.creds.write(&next, agent);
    Some(next)
}

// ---- the client-facing endpoints ----

pub fn session_start(base: &str, token: &str, request: &Value, timeout_ms: u64) -> HttpResult {
    post(
        base,
        "/integrations/coding/session/start",
        Some(token),
        request,
        timeout_ms,
    )
}

/// Always 202, including on malformed input and on load shedding, so the status
/// says nothing about whether the events were kept. The one exception is 401,
/// which is the client's own credential expiring and is the caller's cue to
/// refresh. `timeout_ms` is exposed for the 400ms live flush (feature 0098).
pub fn post_events(base: &str, token: &str, batch: &Value, timeout_ms: u64) -> HttpResult {
    post(base, "/integrations/coding/events", Some(token), batch, timeout_ms)
}

/// Feature 0094. Deliberately not queued to disk: prompt and response text exist
/// as values for the span of one request, and a failure is a missing data point.
pub fn post_insight(base: &str, token: &str, submission: &Value) -> HttpResult {
    post(
        base,
        "/integrations/coding/insights",
        Some(token),
        submission,
        HOOK_TIMEOUT_MS,
    )
}

/// Feature 0098. Same no-retry, no-disk shape as post_insight, its own opt-in.
pub fn post_live_feedback(base: &str, token: &str, submission: &Value) -> HttpResult {
    post(
        base,
        "/integrations/coding/live-feedback",
        Some(token),
        submission,
        HOOK_TIMEOUT_MS,
    )
}

/// Feature 0109. Called from PostToolUse, so the caller passes the short cap.
pub fn post_raw_activity(base: &str, token: &str, detail: &Value, timeout_ms: u64) -> HttpResult {
    post(
        base,
        "/integrations/coding/raw-activity",
        Some(token),
        detail,
        timeout_ms,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::credentials::{CredentialStore, MemoryBackend};
    use crate::store::Store;
    use crate::testing::{MockServer, Reply, TempDir, closed_url};

    #[test]
    fn a_201_is_success_and_a_closed_port_is_status_zero() {
        assert!(is_ok(201) && is_ok(202) && !is_ok(401) && !is_ok(0));
        let server = MockServer::start(|_, _| Reply::Json(201, json!({ "ok": true })));
        let res = post(&server.url, "/x", Some("t"), &json!({ "a": 1 }), 1000);
        assert_eq!(res.status, 201);
        assert_eq!(res.body, Some(json!({ "ok": true })));
        assert_eq!(server.calls()[0].body, json!({ "a": 1 }));

        let res = post(&closed_url(), "/x", None, &json!({}), 1000);
        assert_eq!(res.status, 0);
    }

    #[test]
    fn an_empty_or_garbage_body_is_not_an_error() {
        let server = MockServer::start(|_, _| Reply::Json(202, Value::Null));
        let res = post(&server.url, "/x", None, &json!({}), 1000);
        assert_eq!(res.status, 202);
    }

    fn ctx_with_memory_store() -> (TempDir, Ctx, MemoryBackend) {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        let memory = MemoryBackend::default();
        let creds = CredentialStore::new(store.clone(), Some(Arc::new(memory.clone())));
        let agent = AgentId::ClaudeCode;
        (dir, Ctx { store, creds, agent }, memory)
    }

    fn signed_in(api_url: &str, expires_at: i64) -> Credentials {
        Credentials {
            api_url: api_url.to_string(),
            app_url: None,
            client_id: DEFAULT_CLIENT_ID.into(),
            access_token: "hook".into(),
            refresh_token: "stored-refresh".into(),
            expires_at,
            agent: None,
        }
    }

    #[test]
    fn a_fresh_cached_token_is_used_without_reading_the_store() {
        let (_dir, ctx, memory) = ctx_with_memory_store();
        ctx.creds
            .write(&signed_in("http://api.test", now_ms() + 3_600_000), ctx.agent);
        // Every hook is a new process. Empty the store to prove this one never
        // asks it, which on macOS is what raises the Keychain prompt.
        memory.entries.lock().unwrap().clear();
        let fresh_ctx = Ctx {
            store: ctx.store.clone(),
            creds: CredentialStore::new(ctx.store.clone(), Some(Arc::new(memory.clone()))),
            agent: ctx.agent,
        };
        let token = current_token(&fresh_ctx, fresh_ctx.agent).unwrap();
        assert_eq!(token.access_token, "hook");
        assert_eq!(token.refresh_token, "");
    }

    #[test]
    fn a_refresh_from_the_cache_uses_the_stored_refresh_token_and_updates_both() {
        let server = MockServer::start(|path, _| {
            if path.ends_with("/oauth/token") {
                return Reply::Json(
                    200,
                    json!({ "access_token": "renewed", "refresh_token": "rotated", "expires_in": 3600 }),
                );
            }
            Reply::Json(404, json!({}))
        });
        let (_dir, ctx, _memory) = ctx_with_memory_store();
        // Expired, so the next hook must refresh.
        ctx.creds.write(&signed_in(&server.url, now_ms() - 1), ctx.agent);
        let cached = ctx.creds.read_hook_token(ctx.agent).unwrap();
        assert_eq!(cached.refresh_token, "");

        let renewed = refresh(&ctx, &cached, ctx.agent).unwrap();
        assert_eq!(renewed.access_token, "renewed");
        let sent = &server.calls_to("/oauth/token")[0].body;
        assert_eq!(sent["refresh_token"], "stored-refresh");

        assert_eq!(ctx.creds.read(ctx.agent).unwrap().refresh_token, "rotated");
        let recached = ctx.creds.read_hook_token(ctx.agent).unwrap();
        assert_eq!(recached.access_token, "renewed");
        assert_eq!(recached.refresh_token, "");
        assert_eq!(current_token(&ctx, ctx.agent).unwrap().access_token, "renewed");
    }
}
