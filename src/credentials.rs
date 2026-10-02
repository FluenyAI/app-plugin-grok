// The hook token, and where it lives.
//
// In the OS credential store: macOS Keychain, Windows Credential Manager, or the
// Secret Service on Linux. A 0600 file is readable by any process running as the
// developer, which is a materially weaker promise, so the file is only the
// fallback for a machine with no store at all (a headless Linux box, a container,
// a CI runner), and `flueny status` names which one is in use.
//
// One entry per agent. Claude Code and Grok share a machine and a repository, and
// ingest attributes a batch to the hook token's agent, so a single credential made
// the last login steal the other host's events.
//
// Migration: a credential the TS client left in `credentials.<agent>.json` (or the
// older single `credentials.json`) moves into the store the first time it is read
// with a store available, and the file is deleted.
//
// The hook token is also cached in `token.<agent>.json` (0600), without the
// refresh token, whenever the credential is in a store. Every tool call starts a
// fresh hook process, and an unsigned or ad hoc signed binary has no stable
// identity for "Always Allow", so reading the Keychain on every hook meant a
// Keychain prompt on every tool call, and a prompt left waiting dropped events.
// The hook token is short lived and can only write events, so a 0600 copy costs
// little; the refresh token, which can mint new ones, never leaves the store.
// Hooks read the cache and go to the store only to refresh.

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::store::{Store, read_json, write_json};
use crate::types::{AgentId, KNOWN_AGENTS};

const SERVICE: &str = "flueny";

/// The store's service name. `FLUENY_CREDENTIAL_SERVICE` exists for development
/// and measurement, so a scratch run never reads or overwrites the real entry.
fn service() -> String {
    std::env::var("FLUENY_CREDENTIAL_SERVICE")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| SERVICE.to_string())
}

// A credential store call can block: a Keychain access prompt waits for the
// developer, and a Secret Service that is registered but not running waits for
// D-Bus activation. A hook must not wait that long, so every call is bounded and
// a call that times out reads as "no credential this time", never as "no store",
// so it cannot quietly downgrade the token to the file.
const STORE_TIMEOUT: Duration = Duration::from_millis(2500);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Credentials {
    pub api_url: String,
    // Where the Flueny app lives, which is NOT the API origin. Captured at login
    // from the device grant's `verification_uri`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_url: Option<String>,
    pub client_id: String,
    pub access_token: String,
    pub refresh_token: String,
    // Epoch millis.
    #[serde(deserialize_with = "millis")]
    pub expires_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentId>,
}

fn millis<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    let value = f64::deserialize(d)?;
    Ok(value as i64)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreError {
    // There is no credential store on this machine (or it refused to open).
    Unavailable,
    // The store exists but did not answer in time.
    Timeout,
}

/// One OS credential store, or a stand-in for tests.
pub trait SecretBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn get(&self, account: &str) -> Result<Option<String>, StoreError>;
    fn set(&self, account: &str, secret: &str) -> Result<(), StoreError>;
    fn delete(&self, account: &str) -> Result<(), StoreError>;
}

/// The platform store through the `keyring` crate.
pub struct OsKeyring;

impl OsKeyring {
    fn bounded<T: Send + 'static>(
        op: impl FnOnce() -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(op());
        });
        rx.recv_timeout(STORE_TIMEOUT).unwrap_or(Err(StoreError::Timeout))
    }

    fn entry(account: &str) -> Result<keyring::Entry, StoreError> {
        keyring::Entry::new(&service(), account).map_err(|_| StoreError::Unavailable)
    }
}

impl SecretBackend for OsKeyring {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "macos") {
            "macOS Keychain"
        } else if cfg!(target_os = "windows") {
            "Windows Credential Manager"
        } else {
            "Secret Service"
        }
    }

    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        let account = account.to_string();
        Self::bounded(move || match Self::entry(&account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(StoreError::Unavailable),
        })
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), StoreError> {
        let account = account.to_string();
        let secret = secret.to_string();
        Self::bounded(move || {
            Self::entry(&account)?
                .set_password(&secret)
                .map_err(|_| StoreError::Unavailable)
        })
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        let account = account.to_string();
        Self::bounded(move || match Self::entry(&account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(StoreError::Unavailable),
        })
    }
}

/// An in-memory store for tests, which can also play "no store on this machine".
#[derive(Default, Clone)]
pub struct MemoryBackend {
    pub entries: Arc<Mutex<HashMap<String, String>>>,
    pub unavailable: bool,
}

impl SecretBackend for MemoryBackend {
    fn name(&self) -> &'static str {
        "test store"
    }

    fn get(&self, account: &str) -> Result<Option<String>, StoreError> {
        if self.unavailable {
            return Err(StoreError::Unavailable);
        }
        Ok(self
            .entries
            .lock()
            .map_err(|_| StoreError::Unavailable)?
            .get(account)
            .cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), StoreError> {
        if self.unavailable {
            return Err(StoreError::Unavailable);
        }
        self.entries
            .lock()
            .map_err(|_| StoreError::Unavailable)?
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        if self.unavailable {
            return Err(StoreError::Unavailable);
        }
        self.entries
            .lock()
            .map_err(|_| StoreError::Unavailable)?
            .remove(account);
        Ok(())
    }
}

/// Where a credential is held, for `flueny status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    System(&'static str),
    File(PathBuf),
}

#[derive(Clone)]
pub struct CredentialStore {
    store: Store,
    backend: Option<Arc<dyn SecretBackend>>,
    // A store read can cost milliseconds (a Keychain lookup is the slowest thing
    // a hook does locally), and one hook can need the token more than once. The
    // process is short lived, so the cache is too.
    cache: Arc<Mutex<HashMap<&'static str, (Credentials, Location)>>>,
}

impl CredentialStore {
    pub fn new(store: Store, backend: Option<Arc<dyn SecretBackend>>) -> CredentialStore {
        CredentialStore {
            store,
            backend,
            cache: Arc::default(),
        }
    }

    /// The OS store, unless `FLUENY_CREDENTIAL_STORE=file` asks for the file, which
    /// is for CI and for machines where a store prompt is not wanted.
    pub fn from_env(store: Store) -> CredentialStore {
        let forced_file = std::env::var("FLUENY_CREDENTIAL_STORE").is_ok_and(|v| v.eq_ignore_ascii_case("file"));
        let backend: Option<Arc<dyn SecretBackend>> = if forced_file { None } else { Some(Arc::new(OsKeyring)) };
        CredentialStore::new(store, backend)
    }

    fn file_path(&self, agent: AgentId) -> PathBuf {
        self.store.path(&format!("credentials.{}.json", agent.as_str()))
    }

    fn legacy_path(&self) -> PathBuf {
        self.store.path("credentials.json")
    }

    fn token_path(&self, agent: AgentId) -> PathBuf {
        self.store.path(&format!("token.{}.json", agent.as_str()))
    }

    /// The cached hook token, with an empty refresh token. Only kept while the
    /// credential itself is in a store: with no store the credential file already
    /// holds everything, and a second copy would be pointless.
    pub fn read_hook_token(&self, agent: AgentId) -> Option<Credentials> {
        self.backend.as_ref()?;
        read_json(&self.token_path(agent))
    }

    /// Caches the hook token for later hooks, never the refresh token.
    pub fn cache_hook_token(&self, creds: &Credentials, agent: AgentId) {
        if self.backend.is_none() {
            return;
        }
        let cached = Credentials {
            agent: Some(agent),
            refresh_token: String::new(),
            ..creds.clone()
        };
        write_json(&self.token_path(agent), &cached);
    }

    pub fn read(&self, agent: AgentId) -> Option<Credentials> {
        self.migrate_legacy();
        self.read_located(agent).map(|(creds, _)| creds)
    }

    /// The credential and where it came from. Moves a file credential into the
    /// store when there is one.
    pub fn read_located(&self, agent: AgentId) -> Option<(Credentials, Location)> {
        if let Some(hit) = self.cache.lock().ok().and_then(|c| c.get(agent.as_str()).cloned()) {
            return Some(hit);
        }
        let found = self.read_uncached(agent)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(agent.as_str(), found.clone());
        }
        Some(found)
    }

    fn read_uncached(&self, agent: AgentId) -> Option<(Credentials, Location)> {
        let file = self.file_path(agent);
        if let Some(backend) = &self.backend {
            match backend.get(agent.as_str()) {
                Ok(Some(secret)) => {
                    if let Ok(creds) = serde_json::from_str::<Credentials>(&secret) {
                        // A stale file next to a stored credential is a second copy
                        // of a token for no reason.
                        let _ = fs::remove_file(&file);
                        return Some((creds, Location::System(backend.name())));
                    }
                }
                Ok(None) => {
                    let creds: Credentials = read_json(&file)?;
                    let secret = serde_json::to_string(&Credentials {
                        agent: Some(agent),
                        ..creds.clone()
                    })
                    .ok()?;
                    if backend.set(agent.as_str(), &secret).is_ok() {
                        let _ = fs::remove_file(&file);
                        return Some((creds, Location::System(backend.name())));
                    }
                    return Some((creds, Location::File(file)));
                }
                Err(StoreError::Timeout) => return None,
                Err(StoreError::Unavailable) => {}
            }
        }
        let creds: Credentials = read_json(&file)?;
        Some((creds, Location::File(file)))
    }

    pub fn write(&self, creds: &Credentials, agent: AgentId) -> Location {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(agent.as_str());
        }
        let stored = Credentials {
            agent: Some(agent),
            ..creds.clone()
        };
        let file = self.file_path(agent);
        if let Some(backend) = &self.backend
            && let Ok(secret) = serde_json::to_string(&stored)
            && backend.set(agent.as_str(), &secret).is_ok()
        {
            let _ = fs::remove_file(&file);
            self.cache_hook_token(&stored, agent);
            return Location::System(backend.name());
        }
        // The store refused: the file holds the whole credential, so a cached token
        // would only go stale next to it.
        let _ = fs::remove_file(self.token_path(agent));
        write_json(&file, &stored);
        Location::File(file)
    }

    pub fn clear(&self, agent: AgentId) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.remove(agent.as_str());
        }
        self.migrate_legacy();
        if let Some(backend) = &self.backend {
            let _ = backend.delete(agent.as_str());
        }
        let _ = fs::remove_file(self.file_path(agent));
        let _ = fs::remove_file(self.token_path(agent));
    }

    pub fn list_agents(&self) -> Vec<AgentId> {
        self.migrate_legacy();
        KNOWN_AGENTS
            .into_iter()
            .filter(|agent| self.read_located(*agent).is_some())
            .collect()
    }

    /// Where a new credential would go right now, for `flueny status`.
    pub fn backend_name(&self) -> Option<&'static str> {
        self.backend.as_ref().map(|b| b.name())
    }

    fn migrate_legacy(&self) {
        let path = self.legacy_path();
        let Some(legacy) = read_json::<Credentials>(&path) else {
            return;
        };
        let agent = legacy
            .agent
            .or_else(|| infer_agent_from_token(&legacy.access_token))
            .unwrap_or(AgentId::ClaudeCode);
        if self.read_located(agent).is_none() {
            self.write(&legacy, agent);
        }
        let _ = fs::remove_file(path);
    }
}

/// The `agent` claim of a hook token, read without verifying it: it only decides
/// which local slot a legacy credential migrates into.
pub fn infer_agent_from_token(access_token: &str) -> Option<AgentId> {
    let payload = access_token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    AgentId::parse(value.get("agent")?.as_str()?)
}

fn base64url_decode(input: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for ch in input.bytes() {
        let value = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn jwt_for(agent: &str) -> String {
        let payload = serde_json::json!({ "agent": agent, "typ": "coding-hook" }).to_string();
        format!("hdr.{}.sig", base64url_encode(payload.as_bytes()))
    }

    fn base64url_encode(bytes: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |acc, (i, b)| acc | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..=chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            }
        }
        out
    }

    fn creds(token: &str) -> Credentials {
        Credentials {
            api_url: "http://api.test".into(),
            app_url: None,
            client_id: "flueny-claude-code".into(),
            access_token: token.into(),
            refresh_token: format!("{token}-refresh"),
            expires_at: 1,
            agent: None,
        }
    }

    fn with_store(unavailable: bool) -> (TempDir, CredentialStore, MemoryBackend) {
        let dir = TempDir::new();
        let memory = MemoryBackend {
            unavailable,
            ..Default::default()
        };
        let store = CredentialStore::new(Store::new(dir.path().join("config")), Some(Arc::new(memory.clone())));
        (dir, store, memory)
    }

    #[test]
    fn keeps_claude_code_and_grok_tokens_apart_in_the_store() {
        let (_dir, store, memory) = with_store(false);
        assert_eq!(
            store.write(&creds("claude-token"), AgentId::ClaudeCode),
            Location::System("test store")
        );
        store.write(&creds("grok-token"), AgentId::GrokBuild);
        assert_eq!(store.read(AgentId::ClaudeCode).unwrap().access_token, "claude-token");
        assert_eq!(store.read(AgentId::GrokBuild).unwrap().access_token, "grok-token");
        assert_eq!(store.list_agents(), vec![AgentId::ClaudeCode, AgentId::GrokBuild]);
        assert_eq!(memory.entries.lock().unwrap().len(), 2);
        // Nothing on disk when the store took it.
        assert!(!store.file_path(AgentId::ClaudeCode).exists());
    }

    #[test]
    fn a_0600_file_from_the_ts_client_moves_into_the_store_and_is_deleted() {
        let (_dir, store, memory) = with_store(false);
        let file = store.file_path(AgentId::ClaudeCode);
        write_json(&file, &creds("from-file"));
        let (read, location) = store.read_located(AgentId::ClaudeCode).unwrap();
        assert_eq!(read.access_token, "from-file");
        assert_eq!(location, Location::System("test store"));
        assert!(!file.exists(), "the migrated file must be deleted");
        let stored: Credentials =
            serde_json::from_str(memory.entries.lock().unwrap().get("claude-code").unwrap()).unwrap();
        assert_eq!(stored.agent, Some(AgentId::ClaudeCode));
    }

    #[test]
    fn with_no_store_the_file_is_the_fallback_and_says_so() {
        let (_dir, store, _memory) = with_store(true);
        let location = store.write(&creds("file-token"), AgentId::GrokBuild);
        assert!(matches!(location, Location::File(_)));
        let (read, location) = store.read_located(AgentId::GrokBuild).unwrap();
        assert_eq!(read.access_token, "file-token");
        assert!(matches!(location, Location::File(_)));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.file_path(AgentId::GrokBuild))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn migrates_a_legacy_credentials_json_using_the_token_agent_claim() {
        let (_dir, store, _memory) = with_store(false);
        let mut legacy = creds(&jwt_for("grok-build"));
        legacy.refresh_token = "refresh".into();
        write_json(&store.legacy_path(), &legacy);
        let grok = store.read(AgentId::GrokBuild).unwrap();
        assert_eq!(grok.refresh_token, "refresh");
        assert_eq!(store.read(AgentId::ClaudeCode), None);
        assert!(!store.legacy_path().exists());
    }

    #[test]
    fn reads_the_agent_claim_out_of_a_hook_token() {
        assert_eq!(infer_agent_from_token(&jwt_for("grok-build")), Some(AgentId::GrokBuild));
        assert_eq!(
            infer_agent_from_token(&jwt_for("claude-code")),
            Some(AgentId::ClaudeCode)
        );
        assert_eq!(infer_agent_from_token("not-a-jwt"), None);
    }

    #[test]
    fn clear_removes_both_copies() {
        let (_dir, store, _memory) = with_store(false);
        store.write(&creds("t"), AgentId::ClaudeCode);
        store.clear(AgentId::ClaudeCode);
        assert_eq!(store.read(AgentId::ClaudeCode), None);
        assert_eq!(store.read_hook_token(AgentId::ClaudeCode), None);
        assert!(!store.token_path(AgentId::ClaudeCode).exists());
        assert!(store.list_agents().is_empty());
    }

    #[test]
    fn a_stored_credential_caches_the_hook_token_but_never_the_refresh_token() {
        let (_dir, store, _memory) = with_store(false);
        store.write(&creds("hook"), AgentId::ClaudeCode);
        let cached = store.read_hook_token(AgentId::ClaudeCode).unwrap();
        assert_eq!(cached.access_token, "hook");
        assert_eq!(cached.refresh_token, "");
        assert_eq!(cached.agent, Some(AgentId::ClaudeCode));
        let on_disk = fs::read_to_string(store.token_path(AgentId::ClaudeCode)).unwrap();
        assert!(
            !on_disk.contains("hook-refresh"),
            "the refresh token must stay in the store"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(store.token_path(AgentId::ClaudeCode))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Grok's slot is its own.
        assert_eq!(store.read_hook_token(AgentId::GrokBuild), None);
    }

    #[test]
    fn with_no_store_there_is_no_token_cache() {
        let (_dir, store, _memory) = with_store(true);
        store.write(&creds("file-token"), AgentId::ClaudeCode);
        assert!(!store.token_path(AgentId::ClaudeCode).exists());
        assert_eq!(store.read_hook_token(AgentId::ClaudeCode), None);
    }

    #[test]
    fn a_ts_credential_with_float_millis_still_parses() {
        let parsed: Credentials = serde_json::from_str(
            r#"{"apiUrl":"a","clientId":"c","accessToken":"t","refreshToken":"r","expiresAt":1790931452621.5}"#,
        )
        .unwrap();
        assert_eq!(parsed.expires_at, 1_790_931_452_621);
    }
}
