// Reverted agent edits (feature 0126).
//
// The `reverted` decision was in the contract from M1 and never emitted, so the
// revert penalty in the scorers was dead code. A file the agent edited in this
// session counts as reverted when:
//
//   git checkout -- <path>, git restore <path>, git stash   names it (or a parent)
//   git reset --hard                                        it had uncommitted agent edits
//   git revert HEAD                                         the last commit in this session held it
//   an edit puts it back to its content before the agent    the hashes match
//
// Files are tracked by a hash of their repo-relative path, and their parents by a
// hash of each ancestor directory, so the session file never holds a path. The
// one disk read here is the file the agent just edited, reduced to a hash and
// compared against the hash of its original, and it is declared in reads.rs.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::extract::resolve_from;
use crate::repo_id::hex;
use crate::signal::gitcmd::{RevertCommand, is_git_commit, revert_targets};
use crate::store::{FileTrack, SessionState};

// A file bigger than this is not read back. Missing a revert by edit on a huge
// file costs one event; reading it on every edit costs every developer latency.
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

fn digest(tag: &str, value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(tag.as_bytes());
    hasher.update(value);
    hex(&hasher.finalize()[..12])
}

pub fn file_key(rel: &str) -> String {
    digest("f:", rel.as_bytes())
}

fn dir_key(rel_dir: &str) -> String {
    digest("d:", rel_dir.trim_end_matches('/').as_bytes())
}

/// Every ancestor of a repo-relative path, the root included as "".
fn dir_keys(rel: &str) -> Vec<String> {
    let mut keys = vec![dir_key("")];
    let mut at = 0;
    while let Some(slash) = rel[at..].find('/') {
        keys.push(dir_key(&rel[..at + slash]));
        at += slash + 1;
    }
    keys
}

pub fn content_hash(bytes: &[u8]) -> String {
    digest("c:", bytes)
}

fn hash_file(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_READ_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_READ_BYTES).read_to_end(&mut bytes).ok()?;
    Some(content_hash(&bytes))
}

/// A file whose agent edits were undone, as a key and its path class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reverted {
    pub file: String,
    pub path_class: Option<String>,
    pub ordinal: i64,
}

fn mark_reverted(key: &str, track: &mut FileTrack) -> Reverted {
    track.dirty = false;
    track.reverts += 1;
    Reverted {
        file: key.to_string(),
        path_class: track.path_class.clone(),
        ordinal: track.reverts,
    }
}

/// An accepted agent edit. `original` is what the host said the file held before
/// this edit (`Some(None)` for a file it created), `absolute` where it is on disk.
pub fn track_edit(
    state: &mut SessionState,
    rel: &str,
    path_class: Option<String>,
    original: Option<Option<String>>,
    absolute: &Path,
) -> Option<Reverted> {
    if rel.is_empty() {
        return None;
    }
    let key = file_key(rel);
    let first_touch = !state.files.contains_key(&key);
    let track = state.files.entry(key.clone()).or_insert_with(|| FileTrack {
        path_class: path_class.clone(),
        dirs: dir_keys(rel),
        ..FileTrack::default()
    });
    if first_touch {
        // The content before the agent, when the host said what it was. A file
        // the agent created has no "before" to return to by editing.
        if let Some(Some(text)) = &original {
            track.pre_hash = Some(content_hash(text.as_bytes()));
        }
        track.dirty = true;
        return None;
    }
    let was_dirty = track.dirty;
    track.dirty = true;
    if was_dirty
        && let Some(pre) = track.pre_hash.clone()
        && hash_file(absolute).as_ref() == Some(&pre)
    {
        return Some(mark_reverted(&key, track));
    }
    None
}

/// A Bash command that succeeded. Reverts first, then a commit, so
/// `git restore x && git commit` reads in the order it ran.
pub fn track_command(state: &mut SessionState, command: &str, cwd: &Path) -> Vec<Reverted> {
    let mut reverted = Vec::new();
    let root = state.repo_root.clone();
    for target in revert_targets(command) {
        match target {
            RevertCommand::Paths(paths) => {
                for path in paths {
                    let Some(rel) = resolve_from(cwd, &path, root.as_deref()) else {
                        continue;
                    };
                    let exact = file_key(&rel);
                    let dir = dir_key(&rel);
                    for (key, track) in state.files.iter_mut() {
                        if track.dirty && (*key == exact || track.dirs.contains(&dir)) {
                            reverted.push(mark_reverted(key, track));
                        }
                    }
                }
            }
            RevertCommand::All => {
                for (key, track) in state.files.iter_mut() {
                    if track.dirty {
                        reverted.push(mark_reverted(key, track));
                    }
                }
            }
            RevertCommand::RevertHead => {
                for key in std::mem::take(&mut state.last_commit_files) {
                    if let Some(track) = state.files.get_mut(&key) {
                        reverted.push(mark_reverted(&key, track));
                    }
                }
            }
            // Which files another commit touched is a question for git itself,
            // and spawning it from a hook is the cost this client exists to avoid.
            RevertCommand::RevertOther => {}
        }
    }
    if is_git_commit(command) {
        let committed: Vec<String> = state
            .files
            .iter()
            .filter(|(_, t)| t.dirty)
            .map(|(k, _)| k.clone())
            .collect();
        for key in &committed {
            if let Some(track) = state.files.get_mut(key) {
                track.dirty = false;
            }
        }
        if !committed.is_empty() {
            state.last_commit_files = committed;
        }
    }
    reverted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    fn state(root: &Path) -> SessionState {
        SessionState {
            repo_root: Some(root.to_string_lossy().to_string()),
            ..SessionState::default()
        }
    }

    fn edit(state: &mut SessionState, root: &Path, rel: &str, original: Option<Option<&str>>) -> Option<Reverted> {
        track_edit(
            state,
            rel,
            Some("backend".into()),
            original.map(|o| o.map(str::to_string)),
            &root.join(rel),
        )
    }

    #[test]
    fn git_restore_of_a_file_or_a_parent_reverts_only_dirty_agent_files() {
        let dir = TempDir::new();
        let root = dir.path();
        let mut s = state(root);
        edit(&mut s, root, "src/auth/login.ts", Some(Some("old")));
        edit(&mut s, root, "src/billing/pay.ts", Some(Some("old")));
        edit(&mut s, root, "README.md", Some(Some("old")));

        let reverted = track_command(&mut s, "git restore src/auth/login.ts", root);
        assert_eq!(reverted.len(), 1);
        assert_eq!(reverted[0].file, file_key("src/auth/login.ts"));
        assert_eq!(reverted[0].path_class.as_deref(), Some("backend"));

        // A parent directory, from a cwd inside the repository.
        let reverted = track_command(&mut s, "git checkout -- ..", &root.join("src/billing"));
        assert_eq!(reverted.len(), 1, "only the still-dirty billing file is under src/");
        assert_eq!(reverted[0].file, file_key("src/billing/pay.ts"));

        // Nothing is reverted twice.
        assert!(track_command(&mut s, "git checkout -- src", root).is_empty());
        assert_eq!(track_command(&mut s, "git reset --hard", root).len(), 1);
    }

    #[test]
    fn a_commit_protects_files_from_reset_and_revert_head_undoes_the_commit() {
        let dir = TempDir::new();
        let root = dir.path();
        let mut s = state(root);
        edit(&mut s, root, "src/a.ts", Some(Some("a")));
        edit(&mut s, root, "src/b.ts", Some(Some("b")));
        assert!(track_command(&mut s, "git add -A && git commit -m 'wip'", root).is_empty());
        assert!(
            track_command(&mut s, "git reset --hard", root).is_empty(),
            "committed work is not reverted by reset"
        );
        let reverted = track_command(&mut s, "git revert --no-edit HEAD", root);
        assert_eq!(reverted.len(), 2);
        assert!(track_command(&mut s, "git revert abc1234", root).is_empty());
    }

    #[test]
    fn an_edit_that_restores_the_original_content_is_a_revert() {
        let dir = TempDir::new();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut s = state(root);
        assert_eq!(edit(&mut s, root, "src/x.ts", Some(Some("original"))), None);
        std::fs::write(root.join("src/x.ts"), "changed").unwrap();
        // A second edit that leaves it changed is not a revert.
        std::fs::write(root.join("src/x.ts"), "changed again").unwrap();
        assert_eq!(edit(&mut s, root, "src/x.ts", Some(Some("changed"))), None);
        // A third that puts it back is.
        std::fs::write(root.join("src/x.ts"), "original").unwrap();
        let reverted = edit(&mut s, root, "src/x.ts", Some(Some("changed again"))).unwrap();
        assert_eq!(reverted.ordinal, 1);
    }

    #[test]
    fn a_created_file_or_an_unknown_original_is_never_a_revert_by_edit() {
        let dir = TempDir::new();
        let root = dir.path();
        std::fs::write(root.join("new.ts"), "").unwrap();
        let mut s = state(root);
        edit(&mut s, root, "new.ts", Some(None));
        assert_eq!(edit(&mut s, root, "new.ts", None), None);
        edit(&mut s, root, "grok.ts", None);
        assert_eq!(edit(&mut s, root, "grok.ts", None), None);
    }

    #[test]
    fn the_session_file_holds_hashes_never_paths() {
        let dir = TempDir::new();
        let root = dir.path();
        let mut s = state(root);
        edit(
            &mut s,
            root,
            "src/secret-project/plan.ts",
            Some(Some("CONTENT-never-stored")),
        );
        let text = serde_json::to_string(&s.files).unwrap();
        assert!(!text.contains("secret-project") && !text.contains("plan.ts") && !text.contains("CONTENT"));
    }
}
