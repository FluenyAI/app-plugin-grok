// `flueny`. One command (`hook`) is what the host calls; the rest a developer
// calls.
//
// Nothing here prints colour, an emoji or a box character, and nothing exceeds 80
// columns. That is the terminal voice from the design plan, and it is also just
// what survives a CI log and a piped stdout.

use std::io::{IsTerminal, Read};
use std::time::Duration;

use serde_json::{Value, json};

use crate::api::{
    CLIENT_VERSION, DEFAULT_API_URL, DEFAULT_CLIENT_ID, current_token, exchange_device_code, is_ok, session_start,
    start_device,
};
use crate::api_url::{API_TARGETS, describe_api, resolve_api_target};
use crate::context::Ctx;
use crate::copy::{setup_connected, weekly_summary, wrap80};
use crate::credentials::{Credentials, Location};
use crate::hooks::{on_post_tool_use, on_session_end, on_session_start, on_stop};
use crate::receipt::receipt_for;
use crate::session::handshake_request;
use crate::settings::settings_fragment;
use crate::time::{day, now_ms, parse_iso, today};
use crate::types::AgentId;

pub fn main(args: Vec<String>) -> i32 {
    let command = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];
    if command == "hook" {
        return hook(rest);
    }
    let ctx = Ctx::from_env(flag(rest, "--agent").as_deref());
    match command {
        "login" => login(&ctx, rest),
        "logout" => logout(&ctx),
        "status" => status(&ctx),
        "dry-run" => dry_run(&ctx, rest),
        "install" => install(rest),
        "api" => api(&ctx, rest),
        "version" | "--version" | "-V" => {
            say(&format!("flueny {CLIENT_VERSION}"));
            0
        }
        _ => usage(),
    }
}

fn usage() -> i32 {
    say(&[
        "flueny, the Flueny client for Claude Code and Grok.",
        "",
        "  flueny login [--api-url URL] [--agent AGENT] [--label NAME]",
        "  flueny status                                 what this client is doing",
        "  flueny api [staging|production|URL]           which Flueny this reports to",
        "  flueny dry-run --today                        what was sent today, in full",
        "  flueny install [--print]                      the hook settings to merge by hand",
        "  flueny logout                                 forget the credential",
        "  flueny hook <event>                           called by the agent, reads stdin",
    ]
    .join("\n"));
    0
}

// ---- hook: what the host calls ----

/// A hook that fails is a hook that must fail silently. The host shows a non-zero
/// exit to the developer, and CEO decision 6A is that a developer never sees
/// Flueny's plumbing. So: nothing on stdout, exit 0 whatever happens, including a
/// panic, and a watchdog that ends the process before the host's own timeout
/// would kill it and report a failure.
fn hook(argv: &[String]) -> i32 {
    std::panic::set_hook(Box::new(|_| {}));
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(8));
        std::process::exit(0);
    });
    let event = argv.first().cloned().unwrap_or_default();
    let _ = std::panic::catch_unwind(move || {
        let payload = read_stdin_json();
        let ctx = Ctx::from_env(None);
        run_hook(&ctx, &event, &payload);
    });
    0
}

pub fn run_hook(ctx: &Ctx, event: &str, payload: &Value) {
    match event {
        "session-start" => {
            on_session_start(ctx, payload);
        }
        "post-tool-use" => {
            on_post_tool_use(ctx, payload, false);
        }
        "post-tool-use-failure" => {
            on_post_tool_use(ctx, payload, true);
        }
        "stop" => {
            on_stop(ctx, payload);
        }
        "session-end" => {
            on_session_end(ctx, payload);
        }
        _ => {}
    }
}

// Hook payloads arrive on stdin as one JSON object. Nothing read here is written
// anywhere: extraction happens in extract.rs and the raw value is discarded with
// this process.
fn read_stdin_json() -> Value {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return json!({});
    }
    let mut text = String::new();
    if stdin.read_to_string(&mut text).is_err() {
        return json!({});
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) if value.is_object() => value,
        _ => json!({}),
    }
}

// ---- api: which Flueny this machine reports to ----

// Switching is a re-login, not a rewrite of a stored URL: the credential is
// minted by one environment's backend and means nothing to another. The existing
// credential stays until the new sign-in succeeds.
fn api(ctx: &Ctx, argv: &[String]) -> i32 {
    let agent = ctx.agent;
    let creds = ctx.creds.read(agent);
    let target = positional(argv);
    let Some(target) = target else {
        match &creds {
            Some(c) => say(&format!("API              {}", describe_api(&c.api_url))),
            None => say(&format!("{} is not connected on this machine.", agent.label())),
        }
        say("");
        say("Switch with one of:");
        for (name, url) in API_TARGETS {
            say(&format!("  flueny api {name:<11} {url}"));
        }
        say(&format!("  flueny api {:<11} a local or self-hosted Flueny", "<URL>"));
        return 0;
    };
    let Some(url) = resolve_api_target(&target) else {
        say(&format!("Flueny does not know an API called \"{target}\"."));
        say(&format!(
            "Use {}, or a full http(s) URL.",
            API_TARGETS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(" or ")
        ));
        return 1;
    };
    if creds.as_ref().is_some_and(|c| c.api_url == url) {
        say(&format!("{} already reports to {}.", agent.label(), describe_api(&url)));
        return 0;
    }
    say(&format!("Moving {} to {}.", agent.label(), describe_api(&url)));
    if let Some(c) = &creds {
        say(&format!(
            "It stays signed in to {} until this succeeds.",
            describe_api(&c.api_url)
        ));
    }
    say("");
    let mut next: Vec<String> = argv.iter().filter(|a| **a != target).cloned().collect();
    next.extend(["--api-url".to_string(), url]);
    login(ctx, &next)
}

// ---- login: OAuth device authorization grant (CEO decision 15A) ----

fn login(ctx: &Ctx, argv: &[String]) -> i32 {
    let api_url = flag(argv, "--api-url")
        .or_else(|| std::env::var("FLUENY_API_URL").ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| DEFAULT_API_URL.to_string())
        .trim_end_matches('/')
        .to_string();
    let client_id = flag(argv, "--client-id").unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string());
    let label = flag(argv, "--label").unwrap_or_else(|| gethostname::gethostname().to_string_lossy().to_string());
    let agent = ctx.agent;

    let grant = start_device(&api_url, &client_id, agent, &label);
    let Some(body) = grant.body.as_ref().filter(|_| is_ok(grant.status)) else {
        say(&format!(
            "Flueny could not start sign-in against {api_url} ({}).",
            grant.status
        ));
        return 1;
    };
    let text = |key: &str| body.get(key).and_then(Value::as_str).unwrap_or("").to_string();
    let verification_uri = text("verification_uri");
    say(&[
        format!("Open {verification_uri} and enter this code:"),
        String::new(),
        format!("    {}", text("user_code")),
        String::new(),
        format!("Or open {}", text("verification_uri_complete")),
        "Waiting for approval. Ctrl-C to stop.".to_string(),
    ]
    .join("\n"));

    let expires_in = body.get("expires_in").and_then(Value::as_i64).unwrap_or(600);
    let deadline = now_ms() + expires_in * 1000;
    let mut interval = Duration::from_secs(body.get("interval").and_then(Value::as_u64).unwrap_or(5).max(1));
    let device_code = text("device_code");
    loop {
        if now_ms() > deadline {
            say("The code expired before it was approved. Run flueny login again.");
            return 1;
        }
        std::thread::sleep(interval);
        let token = exchange_device_code(&api_url, &client_id, &device_code);
        let access = token
            .body
            .as_ref()
            .and_then(|b| b.get("access_token"))
            .and_then(Value::as_str);
        if is_ok(token.status)
            && let (Some(access), Some(token_body)) = (access, token.body.as_ref())
        {
            let creds = Credentials {
                api_url: api_url.clone(),
                app_url: Some(origin_of(&verification_uri)),
                client_id: client_id.clone(),
                access_token: access.to_string(),
                refresh_token: token_body
                    .get("refresh_token")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                expires_at: now_ms() + token_body.get("expires_in").and_then(Value::as_i64).unwrap_or(3600) * 1000,
                agent: Some(agent),
            };
            let location = ctx.creds.write(&creds, agent);
            return after_login(ctx, &creds, &verification_uri, &location);
        }
        // RFC 8628: authorization_pending means keep polling, slow_down means back
        // off. Anything else is terminal.
        let error = token
            .body
            .as_ref()
            .and_then(|b| b.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string);
        match error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                interval += Duration::from_secs(5);
                continue;
            }
            Some(other) => say(&format!("Sign-in failed: {other}. Run flueny login again.")),
            None => say(&format!("Sign-in failed: {}. Run flueny login again.", token.status)),
        }
        return 1;
    }
}

// The handshake runs once here so the setup line can state the real dry-run
// window rather than the default from the design plan.
fn after_login(ctx: &Ctx, creds: &Credentials, verification_uri: &str, location: &Location) -> i32 {
    let request = handshake_request(
        ctx,
        &format!("setup-{}", now_ms()),
        ctx.store.read_bundle().map(|b| b.etag),
    );
    let res = session_start(&creds.api_url, &creds.access_token, &request);
    let body = res.body.unwrap_or(Value::Null);
    let dry_run = body.get("dryRun").and_then(Value::as_bool) == Some(true);
    let days = days_until(body.get("dryRunEndsAt").and_then(Value::as_str));
    say("");
    say(&setup_connected(dry_run, days, &origin_of(verification_uri)));
    if body.get("killSwitch").and_then(Value::as_bool) == Some(true) {
        say("");
        say("Flueny is turned off for your organisation. This client will send nothing.");
    }
    say("");
    say(&wrap80(&format!(
        "The sign-in is stored in {}.",
        describe_location(ctx, location)
    )));
    say("");
    say(&format!("Next: restart {}.", ctx.agent.label()));
    0
}

fn describe_location(ctx: &Ctx, location: &Location) -> String {
    let why = if ctx.creds.backend_name().is_none() {
        "because FLUENY_CREDENTIAL_STORE=file asked for it"
    } else {
        "because this machine has no OS credential store"
    };
    match location {
        Location::System(name) => format!("the {name}"),
        Location::File(path) => format!("a file readable only by you ({}), {why}", path.display()),
    }
}

fn logout(ctx: &Ctx) -> i32 {
    ctx.creds.clear(ctx.agent);
    let remaining = ctx.creds.list_agents();
    say(&format!("Disconnected {} on this machine.", ctx.agent.label()));
    if remaining.is_empty() {
        say("Flueny will send nothing until you run flueny login again.");
    } else {
        say(&format!("Still connected: {}.", labels(&remaining)));
    }
    0
}

fn labels(agents: &[AgentId]) -> String {
    agents.iter().map(|a| a.label()).collect::<Vec<_>>().join(", ")
}

// ---- status ----

fn status(ctx: &Ctx) -> i32 {
    let agent = ctx.agent;
    let others: Vec<AgentId> = ctx.creds.list_agents().into_iter().filter(|a| *a != agent).collect();
    ctx.creds.read(agent); // migrates a legacy file before it is located
    let Some((creds, location)) = ctx.creds.read_located(agent) else {
        if others.is_empty() {
            say("Flueny is not connected on this machine. Run flueny login.");
        } else {
            say(&format!("{} is not connected on this machine.", agent.label()));
            say(&format!("Still connected: {}.", labels(&others)));
            say(&format!(
                "Run flueny login --agent {} to connect this host.",
                agent.as_str()
            ));
        }
        return 0;
    };
    let store = &ctx.store;
    let bundle = store.read_bundle();
    let counters = store.read_counters(&today());
    let opt_in = |name: &str, on: &str| {
        if store.read_flag(name) {
            on.to_string()
        } else {
            "off: neither ever leaves this machine".to_string()
        }
    };
    let lines = [
        format!("API              {}", creds.api_url),
        format!("Agent            {}", agent.label()),
        format!(
            "Also connected   {}",
            if others.is_empty() {
                "none".to_string()
            } else {
                labels(&others)
            }
        ),
        format!(
            "Credential       {}",
            if creds.expires_at > now_ms() {
                "valid"
            } else {
                "expired, refreshes on next hook"
            }
        ),
        format!(
            "Credential store {}",
            match &location {
                Location::System(name) => name.to_string(),
                Location::File(_) if ctx.creds.backend_name().is_none() => {
                    "a 0600 file, because FLUENY_CREDENTIAL_STORE=file".to_string()
                }
                Location::File(_) => "a 0600 file: no OS credential store on this machine".to_string(),
            }
        ),
        format!("Config           {}", store.dir.display()),
        format!(
            "Policy bundle    {}",
            bundle
                .map(|b| format!("etag {}, schema {}", b.etag, b.schema_version))
                .unwrap_or_else(|| "not cached yet".into())
        ),
        format!("Queued events    {}", store.read_queue().len()),
        format!("Observed today   {}", counters.observed),
        format!("Sent today       {}", counters.would_send),
        format!("Blocked today    {}", counters.would_block),
        // Last known from the most recent handshake, not fetched fresh: a question
        // about local state should not cost a network round trip.
        format!(
            "Prompt scoring   {}",
            opt_in(
                "prompt-insights",
                "on: your prompts and the agent's replies are sent for scoring"
            )
        ),
        format!(
            "Live feedback    {}",
            opt_in(
                "live-feedback",
                "on: your prompts and the agent's replies are sent for a real-time coaching nudge"
            )
        ),
        format!(
            "Raw activity     {}",
            opt_in("raw-activity", "on: real file paths and Bash command text may be sent")
        ),
        // Feature 0094, from feat/insight-receipt-visibility: delivered and failed
        // today, the only local evidence a submission reached the backend.
        format!(
            "Insights today   {} sent, {} failed",
            counters.insights_sent, counters.insights_failed
        ),
        // Feature 0115. The handshake can turn it on, but neither host gives a hook
        // the weekly-limit percentage, so this client has nothing to send.
        format!(
            "Weekly limit     {}",
            if store.read_flag("weekly-limit") {
                "on for you, but not reported: the agent gives this client no local source for it"
            } else {
                "off"
            }
        ),
    ];
    say(&lines.join("\n"));

    if current_token(ctx, agent).is_none() {
        say("");
        say("The credential could not be refreshed. Run flueny login again.");
    }
    say("");
    say(&receipt_for(store, &today()));
    if let Some(summary) = weekly(ctx, &creds) {
        say("");
        say(&summary);
    }
    0
}

// Rendered from the local ledger, so it can only ever state what this machine
// actually derived. `/coding/signal` stays authoritative (design decision 46).
fn weekly(ctx: &Ctx, creds: &Credentials) -> Option<String> {
    let mut decisions = 0;
    let mut rejected = 0;
    let mut touched_tests = 0;
    for i in 0..7 {
        for entry in ctx.store.read_ledger(&day(now_ms() - i * 86_400_000)) {
            if !entry.summary.starts_with("Agent edit ") {
                continue;
            }
            decisions += 1;
            if entry.summary.starts_with("Agent edit rejected") {
                rejected += 1;
                if entry.summary.contains("path class tests") {
                    touched_tests += 1;
                }
            }
        }
    }
    if decisions == 0 {
        return None;
    }
    let app = creds.app_url.clone().unwrap_or_else(|| origin_of(&creds.api_url));
    Some(weekly_summary(rejected, decisions, touched_tests, &app))
}

// ---- dry-run ----

fn dry_run(ctx: &Ctx, argv: &[String]) -> i32 {
    if !argv.iter().any(|a| a == "--today") {
        say("Usage: flueny dry-run --today");
        return 1;
    }
    let today = today();
    say(&receipt_for(&ctx.store, &today));
    let entries = ctx.store.read_ledger(&today);
    if entries.is_empty() {
        say("");
        say("Nothing has been sent today.");
        return 0;
    }
    say("");
    say(&format!("Every row Flueny sent today, {} in total:", entries.len()));
    say("");
    for entry in &entries {
        say(&format!("  {}  {}", entry.at.get(11..19).unwrap_or(""), entry.summary));
        say(&format!("            fields: {}", entry.fields_sent.join(", ")));
    }
    say("");
    // "prompt" only ever appears as a field name on a prompt insight row, so this
    // is how the closing line stays true either way.
    if entries.iter().any(|e| e.fields_sent.iter().any(|f| f == "prompt")) {
        say(&wrap80(
            "These field names are the whole payload for each row. No code and no file contents are in any of it. \
             Rows marked \"Prompt scored for insight\" are the one exception to prompt text never leaving this \
             machine: prompt insight scoring is on, so those did send your prompt and the agent's reply, once each, \
             for a single scoring pass.",
        ));
    } else {
        say(&wrap80(
            "These field names are the whole payload. No prompt, no code and no file contents are in it.",
        ));
    }
    if ctx.store.read_receipt_day().as_deref() != Some(today.as_str()) {
        ctx.store.write_receipt_day(&today);
    }
    0
}

// ---- install ----

fn install(argv: &[String]) -> i32 {
    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "flueny".into());
    say(&serde_json::to_string_pretty(&settings_fragment(&binary)).unwrap_or_default());
    if argv.iter().any(|a| a == "--print") {
        return 0;
    }
    say("");
    say(&wrap80(
        "Merge the hooks block above into ~/.claude/settings.json, or into .claude/settings.json in one \
         repository, then restart Claude Code. Every hook is type command on purpose: extraction runs here, \
         and an http hook would post raw tool payloads off this machine.",
    ));
    0
}

// ---- helpers ----

fn flag(argv: &[String], name: &str) -> Option<String> {
    let at = argv.iter().position(|a| a == name)?;
    argv.get(at + 1).cloned()
}

fn positional(argv: &[String]) -> Option<String> {
    let mut skip = false;
    for arg in argv {
        if skip {
            skip = false;
            continue;
        }
        if arg.starts_with("--") {
            skip = matches!(arg.as_str(), "--agent" | "--api-url" | "--label" | "--client-id");
            continue;
        }
        return Some(arg.clone());
    }
    None
}

pub fn days_until(iso: Option<&str>) -> i64 {
    let Some(at) = iso.and_then(parse_iso) else { return 0 };
    let ms = at - now_ms();
    if ms <= 0 { 0 } else { (ms + 86_399_999) / 86_400_000 }
}

fn origin_of(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    format!("{}{}", &url[..scheme_end + 3], &rest[..host_end])
}

fn say(text: &str) {
    println!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_and_days_until() {
        assert_eq!(
            origin_of("https://app.flueny.ai/coding/connect?code=x"),
            "https://app.flueny.ai"
        );
        assert_eq!(origin_of("not a url"), "not a url");
        assert_eq!(days_until(None), 0);
        assert_eq!(days_until(Some("2000-01-01T00:00:00Z")), 0);
        let in_three = crate::time::iso(now_ms() + 3 * 86_400_000 - 1000);
        assert_eq!(days_until(Some(&in_three)), 3);
    }

    #[test]
    fn the_api_target_is_the_first_positional_argument() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            positional(&args(&["--agent", "grok-build", "staging"])).as_deref(),
            Some("staging")
        );
        assert_eq!(positional(&args(&["--agent", "grok-build"])), None);
    }

    #[test]
    fn an_unknown_hook_event_does_nothing() {
        let env = crate::testing::TestEnv::new();
        run_hook(&env.ctx, "pre-tool-use", &json!({ "session_id": "s" }));
        assert!(env.ctx.store.read_session("s").is_none());
    }
}
