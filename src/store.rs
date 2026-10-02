// Everything this client persists, in one place, so "what is on disk" is a
// question with one answer. The credential is the exception: it lives in the OS
// credential store (credentials.rs), and only falls back to a 0600 file here
// when there is no store on the machine.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{AgentId, PolicyBundle, ToolActivityEntry};

pub const MAX_SEEN_TOOL_USE_IDS: usize = 2000;

/// The per-process view of where this client keeps its files. Tests point it at
/// a temp directory, so a test run never reads or writes a real credential.
#[derive(Debug, Clone)]
pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir }
    }

    /// `FLUENY_CONFIG_DIR`, else `$XDG_CONFIG_HOME/flueny`, else `~/.config/flueny`.
    pub fn from_env() -> Store {
        if let Some(dir) = std::env::var_os("FLUENY_CONFIG_DIR").filter(|v| !v.is_empty()) {
            return Store::new(PathBuf::from(dir));
        }
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
            return Store::new(PathBuf::from(xdg).join("flueny"));
        }
        Store::new(home_dir().join(".config").join("flueny"))
    }

    pub fn path(&self, name: &str) -> PathBuf {
        ensure_dir(&self.dir);
        self.dir.join(name)
    }

    // ---- policy bundle cache (CEO decision 25A) ----

    pub fn read_bundle(&self) -> Option<PolicyBundle> {
        read_json(&self.path("bundle.json"))
    }

    pub fn write_bundle(&self, bundle: &PolicyBundle) {
        write_json(&self.path("bundle.json"), bundle);
    }

    pub fn forget_bundle(&self) {
        let _ = fs::remove_file(self.path("bundle.json"));
    }

    // ---- opt-in caches (features 0094, 0098, 0109, 0115) ----
    //
    // SessionState carries the authoritative value per active session, but
    // `flueny status` runs with no session in progress, the same reason
    // bundle.json exists: the last known answer, refreshed on every handshake,
    // so the terminal can say plainly whether each opt-in is on without a
    // network round trip. One file each, because the opt-ins are independent.

    pub fn read_flag(&self, name: &str) -> bool {
        read_json::<Value>(&self.path(&format!("{name}.json")))
            .and_then(|v| v.get("enabled").and_then(Value::as_bool))
            .unwrap_or(false)
    }

    pub fn write_flag(&self, name: &str, enabled: bool) {
        write_json(
            &self.path(&format!("{name}.json")),
            &serde_json::json!({ "enabled": enabled }),
        );
    }

    // ---- the most recent handshake, for `flueny status` ----
    //
    // A session that cannot handshake sends nothing, and before this record the
    // only trace was a reason inside one session file. `status` reads this so a
    // developer told "Connected" can also see that the last session start failed.

    pub fn read_last_handshake(&self) -> Option<LastHandshake> {
        read_json(&self.path("last-handshake.json"))
    }

    pub fn write_last_handshake(&self, record: &LastHandshake) {
        write_json(&self.path("last-handshake.json"), record);
    }

    // ---- per-session state ----

    fn session_path(&self, session_id: &str) -> PathBuf {
        let dir = self.dir.join("sessions");
        ensure_dir(&dir);
        dir.join(format!("{}.json", safe(session_id)))
    }

    pub fn read_session(&self, session_id: &str) -> Option<SessionState> {
        read_json(&self.session_path(session_id))
    }

    pub fn write_session(&self, state: &mut SessionState) {
        if state.seen_tool_use_ids.len() > MAX_SEEN_TOOL_USE_IDS {
            let drop = state.seen_tool_use_ids.len() - MAX_SEEN_TOOL_USE_IDS;
            state.seen_tool_use_ids.drain(..drop);
        }
        let path = self.session_path(&state.session_id);
        write_json(&path, state);
    }

    pub fn clear_session(&self, session_id: &str) {
        let _ = fs::remove_file(self.session_path(session_id));
    }

    /// The host runs PostToolUse concurrently when the model calls several tools
    /// at once, and the session file is read-modify-write. mkdir is atomic on
    /// every filesystem this runs on, so it is the lock. A stale lock expires
    /// rather than wedging every later hook, because a wedged hook is a wedged
    /// editor.
    pub fn with_session_lock<T>(&self, session_id: &str, f: impl FnOnce() -> T) -> T {
        let mut lock = self.session_path(session_id).into_os_string();
        lock.push(".lock");
        let lock = PathBuf::from(lock);
        let deadline = Instant::now() + Duration::from_millis(2000);
        loop {
            if fs::create_dir(&lock).is_ok() {
                break;
            }
            if Instant::now() > deadline {
                let _ = fs::remove_dir_all(&lock);
                continue;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
        struct Release(PathBuf);
        impl Drop for Release {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _release = Release(lock);
        f()
    }

    // ---- outbound queue ----

    pub fn queue_path(&self) -> PathBuf {
        self.path("queue.jsonl")
    }

    /// Append-only JSONL, one write call per hook: an append of a line under the
    /// pipe buffer is atomic enough for concurrent hooks, which a
    /// read-modify-write JSON array would not be.
    pub fn append_queue(&self, agent: AgentId, session_id: &str, events: &[Value]) {
        if events.is_empty() {
            return;
        }
        let mut body = String::new();
        for event in events {
            let record = QueuedRecord {
                agent,
                session_id: session_id.to_string(),
                event: event.clone(),
            };
            if let Ok(line) = serde_json::to_string(&record) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        append_private(&self.queue_path(), &body);
    }

    pub fn read_queue(&self) -> Vec<QueuedRecord> {
        let Ok(text) = fs::read_to_string(self.queue_path()) else {
            return Vec::new();
        };
        // A torn final line from a killed process is dropped: losing one event
        // and keeping the queue readable is the right way round.
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<QueuedRecord>(line).ok())
            .collect()
    }

    pub fn replace_queue(&self, records: &[QueuedRecord]) {
        let mut body = String::new();
        for record in records {
            if let Ok(line) = serde_json::to_string(record) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        write_private(&self.queue_path(), body.as_bytes());
    }

    // ---- local receipt ledger (design decision 44) ----

    fn ledger_dir(&self) -> PathBuf {
        let dir = self.dir.join("ledger");
        ensure_dir(&dir);
        dir
    }

    pub fn append_ledger(&self, day: &str, entries: &[LedgerEntry]) {
        if entries.is_empty() {
            return;
        }
        let mut body = String::new();
        for entry in entries {
            if let Ok(line) = serde_json::to_string(entry) {
                body.push_str(&line);
                body.push('\n');
            }
        }
        append_private(&self.ledger_dir().join(format!("{day}.jsonl")), &body);
    }

    pub fn read_ledger(&self, day: &str) -> Vec<LedgerEntry> {
        let Ok(text) = fs::read_to_string(self.ledger_dir().join(format!("{day}.jsonl"))) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<LedgerEntry>(line).ok())
            .collect()
    }

    /// Counters are separate from the entry log because `observed` counts raw
    /// tool calls the client looked at, and the whole point is that most of them
    /// produce no entry at all. Read per field with defaults, so a counters file
    /// written earlier the same day by an older client cannot turn into nonsense.
    pub fn bump_counters(&self, day: &str, delta: &LedgerCounters) -> LedgerCounters {
        let path = self.ledger_dir().join(format!("{day}.counters.json"));
        let current: LedgerCounters = read_json(&path).unwrap_or_default();
        let next = LedgerCounters {
            observed: current.observed + delta.observed,
            would_send: current.would_send + delta.would_send,
            would_block: current.would_block + delta.would_block,
            insights_sent: current.insights_sent + delta.insights_sent,
            insights_failed: current.insights_failed + delta.insights_failed,
        };
        write_json(&path, &next);
        next
    }

    pub fn read_counters(&self, day: &str) -> LedgerCounters {
        read_json(&self.ledger_dir().join(format!("{day}.counters.json"))).unwrap_or_default()
    }

    // The receipt is printed once a day. Which day it was last printed for lives
    // here rather than in a session file, because a developer runs several sessions.
    pub fn read_receipt_day(&self) -> Option<String> {
        read_json::<Value>(&self.path("receipt.json"))
            .and_then(|v| v.get("day").and_then(Value::as_str).map(str::to_string))
    }

    pub fn write_receipt_day(&self, day: &str) {
        write_json(&self.path("receipt.json"), &serde_json::json!({ "day": day }));
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedRecord {
    pub agent: AgentId,
    pub session_id: String,
    pub event: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LedgerCounters {
    pub observed: i64,
    pub would_send: i64,
    pub would_block: i64,
    // Feature 0094. Insight submissions never retry and never queue to disk, so
    // whether one actually reached the backend is a fact a developer otherwise
    // has no way to check.
    pub insights_sent: i64,
    pub insights_failed: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    pub at: String,
    pub summary: String,
    pub fields_sent: Vec<String>,
    // Literally false until `/gate` exists at M3.
    pub would_block: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PendingEdit {
    pub tool_use_id: String,
    pub at: String,
    pub path_class: Option<String>,
}

/// One session's state, read and written by every hook. Field names are the TS
/// client's, so a session that was started by the old client and continues
/// under this one still loads, and every field defaults so a missing one is
/// never a parse failure.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionState {
    pub session_id: String,
    pub agent: Option<AgentId>,
    pub started_at: i64,
    // Inert covers every reason to send nothing: kill switch, a repo that is not
    // on the allowlist, no credential. The reason is kept for `flueny status`.
    pub inert: bool,
    pub inert_reason: Option<String>,
    pub kill_switch: bool,
    pub dry_run: bool,
    pub dry_run_ends_at: Option<String>,
    pub repo_id: Option<String>,
    pub repo_root: Option<String>,
    pub tool_uses: i64,
    pub subagents: i64,
    // Buffered edit decisions, held until Stop because `testsRun` is only
    // knowable after the turn.
    pub pending_edits: Vec<PendingEdit>,
    pub tests_ran_this_turn: bool,
    pub seen_tool_use_ids: Vec<String>,
    // Byte offset the transcript sweep has already read.
    pub transcript_offset: u64,
    // Only ever incremented, so a synthesized id is unique within a session.
    pub event_seq: i64,
    pub prompt_insights_enabled: bool,
    pub prompt_insight_line_offset: usize,
    pub prompt_insight_seq: i64,
    pub live_feedback_enabled: bool,
    pub raw_activity_enabled: bool,
    pub weekly_limit_reporting_enabled: bool,
    pub turn_tool_activity: Vec<ToolActivityEntry>,
    // Feature 0126. Everything below is local bookkeeping for turn verification
    // and revert detection. Files are keyed by a hash of their repo-relative
    // path, so this file never holds a path, a command or any content.
    pub turn: TurnState,
    pub turn_seq: i64,
    pub files: BTreeMap<String, FileTrack>,
    pub last_commit_files: Vec<String>,
    // Set only when the handshake got no usable answer (no response, a timeout,
    // 408, 429 or a 5xx). The session stays inert but a later hook tries again
    // from this time (epoch ms), so one blip at SessionStart no longer mutes a
    // session that can run for hours. None means there is nothing to retry.
    pub handshake_retry_at: Option<i64>,
    pub handshake_failures: u32,
}

/// The outcome of the latest handshake attempt on this machine. Holds a status
/// code and a short error class, never a URL, a token or a response body.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LastHandshake {
    pub at: i64,
    pub ok: bool,
    pub status: u16,
    pub error: Option<String>,
    pub retry_at: Option<i64>,
}

/// The current agent turn, from one Stop to the next.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnState {
    // Orders edits and test runs against each other inside the turn.
    pub step: i64,
    pub edits: Vec<TurnEdit>,
    pub runs: Vec<TurnRun>,
    pub flagged: Vec<FlaggedChange>,
    pub subagents: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnEdit {
    pub step: i64,
    // Empty when the edit named no file inside the repository.
    pub file: String,
    pub test: bool,
    pub sensitive: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TurnRun {
    pub step: i64,
    pub outcome: String,
    // A hash of the normalized command, so "the same command failed again" is
    // answerable without keeping the command.
    pub command: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FlaggedChange {
    pub step: i64,
    pub file: String,
    pub flags: Vec<String>,
    pub caught: bool,
}

/// One file the agent edited in this session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FileTrack {
    pub path_class: Option<String>,
    // Hashes of every ancestor directory, so `git restore src/` can be matched
    // against a file without the file's path being stored.
    pub dirs: Vec<String>,
    // A hash of the content before the agent first touched the file, when the
    // host said what it was.
    pub pre_hash: Option<String>,
    // Changed by the agent and not committed or reverted since.
    pub dirty: bool,
    pub reverts: i64,
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn ensure_dir(path: &Path) {
    if path.is_dir() {
        return;
    }
    let _ = fs::create_dir_all(path);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Ok(body) = serde_json::to_vec(value) {
        write_private(path, &body);
    }
}

/// Written through a temp file so a hook killed mid-write leaves the previous
/// state rather than a truncated JSON file that every later hook fails to parse.
pub fn write_private(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        ensure_dir(parent);
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let written = (|| -> std::io::Result<()> {
        let mut file = private_options().write(true).create(true).truncate(true).open(&tmp)?;
        file.write_all(body)?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
}

fn append_private(path: &Path, body: &str) {
    if let Some(parent) = path.parent() {
        ensure_dir(parent);
    }
    if let Ok(mut file) = private_options().append(true).create(true).open(path) {
        let _ = file.write_all(body.as_bytes());
    }
}

fn private_options() -> OpenOptions {
    #[allow(unused_mut)]
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn safe(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(100)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn a_session_written_by_the_ts_client_still_loads() {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        let legacy = r#"{"sessionId":"s1","agent":"claude-code","startedAt":1,"inert":false,"inertReason":null,
            "killSwitch":false,"dryRun":false,"dryRunEndsAt":null,"repoId":"sha256:x","repoRoot":"/r","toolUses":2,
            "subagents":0,"pendingEdits":[{"toolUseId":"t","at":"a","pathClass":"auth"}],"testsRanThisTurn":true,
            "seenToolUseIds":["t"],"transcriptOffset":12,"eventSeq":0,"promptInsightsEnabled":false,
            "promptInsightLineOffset":0,"promptInsightSeq":0,"liveFeedbackEnabled":false,"rawActivityEnabled":false,
            "turnToolActivity":[]}"#;
        fs::create_dir_all(store.dir.join("sessions")).unwrap();
        fs::write(store.dir.join("sessions/s1.json"), legacy).unwrap();
        let state = store.read_session("s1").unwrap();
        assert_eq!(state.pending_edits.len(), 1);
        assert_eq!(state.transcript_offset, 12);
        assert_eq!(state.turn, TurnState::default());
    }

    #[test]
    fn private_files_are_0600() {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        store.write_flag("prompt-insights", true);
        assert!(store.read_flag("prompt-insights"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.path("prompt-insights.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn counters_written_by_an_older_client_read_per_field() {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        fs::create_dir_all(store.dir.join("ledger")).unwrap();
        fs::write(
            store.dir.join("ledger/2026-08-08.counters.json"),
            r#"{"observed":3,"wouldSend":2,"wouldBlock":0}"#,
        )
        .unwrap();
        let next = store.bump_counters(
            "2026-08-08",
            &LedgerCounters {
                insights_sent: 1,
                ..Default::default()
            },
        );
        assert_eq!(next.observed, 3);
        assert_eq!(next.insights_sent, 1);
        assert_eq!(next.insights_failed, 0);
    }

    #[test]
    fn a_torn_queue_line_is_dropped_not_fatal() {
        let dir = TempDir::new();
        let store = Store::new(dir.path().join("config"));
        store.append_queue(AgentId::ClaudeCode, "s", &[serde_json::json!({"eventId": "a"})]);
        append_private(&store.queue_path(), "{\"agent\":\"claude-code\",\"sess");
        assert_eq!(store.read_queue().len(), 1);
    }
}
