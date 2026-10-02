// Test scaffolding.
//
// Every test that touches disk gets its own temp config directory, so a test run
// can never read or write the developer's real credential, and tests stay
// independent when cargo runs them in parallel.
//
// MockServer is a real HTTP server on 127.0.0.1 that records every request body
// byte for byte. The redaction tests assert against what was SENT over a socket,
// never against what came back: ingest answers 202 to everything including
// malformed input, so a response-based assertion would pass on a client that
// sent nothing at all, or everything.

#![cfg(test)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::context::Ctx;
use crate::credentials::{CredentialStore, Credentials, MemoryBackend};
use crate::store::Store;
use crate::types::AgentId;

static COUNTER: AtomicUsize = AtomicUsize::new(0);

pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new() -> TempDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("flueny-test-{}-{n}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        // Canonical, so /var and /private/var on macOS do not disagree about
        // which repository a path is inside.
        TempDir(path.canonicalize().unwrap())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Clone)]
pub struct Capture {
    pub path: String,
    pub body: Value,
    pub raw: String,
}

pub enum Reply {
    Json(u16, Value),
    // Close the socket without answering: a network failure, as the client sees it.
    Drop,
}

type Responder = dyn Fn(&str, &Value) -> Reply + Send + Sync;

pub struct MockServer {
    pub url: String,
    calls: Arc<Mutex<Vec<Capture>>>,
    stop: Arc<AtomicBool>,
    addr: std::net::SocketAddr,
}

impl MockServer {
    pub fn start(responder: impl Fn(&str, &Value) -> Reply + Send + Sync + 'static) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let responder: Arc<Responder> = Arc::new(responder);
        {
            let calls = calls.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    handle(stream, &calls, &responder);
                }
            });
        }
        MockServer {
            url: format!("http://{addr}"),
            calls,
            stop,
            addr,
        }
    }

    /// Answers 202 with an empty object to everything.
    pub fn accepting() -> MockServer {
        MockServer::start(|_, _| Reply::Json(202, json!({})))
    }

    pub fn calls(&self) -> Vec<Capture> {
        self.calls.lock().unwrap().clone()
    }

    pub fn calls_to(&self, suffix: &str) -> Vec<Capture> {
        self.calls().into_iter().filter(|c| c.path.ends_with(suffix)).collect()
    }

    pub fn everything_sent(&self) -> String {
        self.calls()
            .iter()
            .map(|c| c.raw.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }
}

fn handle(stream: TcpStream, calls: &Mutex<Vec<Capture>>, responder: &Arc<Responder>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() || request_line.is_empty() {
        return;
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).is_err() {
            return;
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; length];
    if reader.read_exact(&mut body).is_err() {
        return;
    }
    let raw = String::from_utf8_lossy(&body).to_string();
    let parsed: Value = serde_json::from_str(&raw).unwrap_or(Value::Null);
    calls.lock().unwrap().push(Capture {
        path: path.clone(),
        body: parsed.clone(),
        raw,
    });
    let mut stream = stream;
    match responder(&path, &parsed) {
        Reply::Drop => {}
        Reply::Json(status, value) => {
            let text = value.to_string();
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    }
}

/// A URL nothing listens on.
pub fn closed_url() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

/// A config directory, an in-memory credential store, and an agent: everything a
/// hook needs, isolated per test.
pub struct TestEnv {
    pub dir: TempDir,
    pub ctx: Ctx,
}

impl TestEnv {
    pub fn new() -> TestEnv {
        TestEnv::for_agent(AgentId::ClaudeCode)
    }

    pub fn for_agent(agent: AgentId) -> TestEnv {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        let creds = CredentialStore::new(store.clone(), Some(Arc::new(MemoryBackend::default())));
        TestEnv {
            ctx: Ctx { store, creds, agent },
            dir,
        }
    }

    pub fn connect(&self, api_url: &str) {
        self.ctx.creds.write(
            &Credentials {
                api_url: api_url.to_string(),
                app_url: None,
                client_id: "flueny-claude-code".into(),
                access_token: "token".into(),
                refresh_token: "refresh".into(),
                expires_at: crate::time::now_ms() + 3_600_000,
                agent: None,
            },
            self.ctx.agent,
        );
    }

    /// A directory that looks like a git clone to git.rs, which reads .git/config
    /// and nothing else.
    pub fn make_repo(&self, remote: &str) -> PathBuf {
        let dir = self.dir.path().join("repo");
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(
            dir.join(".git/config"),
            format!("[core]\n\tbare = false\n[remote \"origin\"]\n\turl = {remote}\n\tfetch = +refs/heads/*\n"),
        )
        .unwrap();
        dir
    }
}

pub fn bundle() -> Value {
    json!({
        "etag": "etag-one",
        "schemaVersion": 1,
        "pathClassifier": {
            "tests": ["**/*.test.*", "**/*.spec.*", "**/tests/**", "**/__tests__/**", "test/**"],
            "auth": ["**/auth/**", "**/*auth*", "**/session*", "**/*jwt*", "**/*passkey*"],
            "security": ["**/crypto/**", "**/*secret*", "**/*credential*", "**/security/**"],
            "payments": ["**/billing/**", "**/payments/**", "**/*stripe*", "**/*invoice*"],
            "docs": ["**/*.md", "docs/**"],
            "backend": ["**/*.ts", "**/*.py", "**/*.go", "**/*.rs", "src/**"]
        },
        "rules": []
    })
}

pub fn handshake_body(over: Value) -> Value {
    let mut body = json!({
        "killSwitch": false,
        "capabilities": { "agent": "claude-code", "canStreamEvents": true, "canInjectContext": true, "canEnforce": false },
        "dryRun": false,
        "dryRunEndsAt": null,
        "repoAllowlist": [],
        "bundle": bundle(),
        "intervention": null,
    });
    if let (Some(target), Some(extra)) = (body.as_object_mut(), over.as_object()) {
        for (k, v) in extra {
            target.insert(k.clone(), v.clone());
        }
    }
    body
}
