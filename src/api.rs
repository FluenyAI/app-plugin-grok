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

pub fn current_token(ctx: &Ctx, agent: AgentId) -> Option<Credentials> {
    let creds = ctx.creds.read(agent)?;
    if creds.expires_at - REFRESH_MARGIN_MS > now_ms() {
        return Some(creds);
    }
    refresh(ctx, &creds, agent)
}

pub fn refresh(ctx: &Ctx, creds: &Credentials, agent: AgentId) -> Option<Credentials> {
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
    use crate::testing::{MockServer, Reply, closed_url};

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
}
