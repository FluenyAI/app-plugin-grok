// The Flueny client for Claude Code and Grok (feature 0028, rewritten in Rust by
// feature 0126). A single `flueny` binary: the hooks the host spawns once per tool
// call, and the commands a developer runs.
//
// The promise the whole crate is built around: no prompt text, no code and no file
// contents ever leave this machine. Extraction happens here, and `wire.rs` is the
// one boundary every event crosses on its way out.

pub mod api;
pub mod api_url;
pub mod classify;
pub mod cli;
pub mod context;
pub mod copy;
pub mod credentials;
pub mod decline;
pub mod extract;
pub mod git;
pub mod hooks;
pub mod prompt_insight;
pub mod queue;
pub mod reads;
pub mod receipt;
pub mod repo_id;
pub mod revert;
pub mod session;
pub mod settings;
pub mod signal;
pub mod store;
pub mod time;
pub mod transcript;
pub mod types;
pub mod verify;
pub mod wire;

#[cfg(test)]
mod testing;
