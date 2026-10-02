// Where the repository is and what its origin remote is called.
//
// Read from `.git/config` rather than shelled out to `git`, for two reasons. A
// hook of type "command" already pays one process spawn per invocation (eng
// findings 13 and 15), and adding a second one to answer a question a 2KB file
// answers is the wrong trade. And `git remote get-url` inherits the developer's
// environment, which on a machine with credential helpers can prompt.
//
// Nothing here reads a tracked file, a diff or an object. Only `.git/config`.

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub root: PathBuf,
    pub remote: Option<String>,
}

/// Walks up from `from` looking for `.git`. `.git` is a directory in a normal
/// clone and a file containing `gitdir: <path>` inside a worktree or a submodule,
/// and this product is developed in worktrees, so the file form is not exotic.
pub fn find_repo(from: &Path) -> Option<RepoInfo> {
    let start = if from.is_absolute() {
        from.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(from)
    };
    let mut dir: &Path = &start;
    loop {
        if let Some(git_dir) = resolve_git_dir(&dir.join(".git")) {
            return Some(RepoInfo {
                root: dir.to_path_buf(),
                remote: read_origin_remote(&git_dir),
            });
        }
        dir = dir.parent()?;
    }
}

fn resolve_git_dir(dot_git: &Path) -> Option<PathBuf> {
    let meta = fs::metadata(dot_git).ok()?;
    if meta.is_dir() {
        return Some(dot_git.to_path_buf());
    }
    if !meta.is_file() {
        return None;
    }
    let text = fs::read_to_string(dot_git).ok()?;
    let pointer = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("gitdir:"))?
        .trim();
    if pointer.is_empty() {
        return None;
    }
    let target = dot_git.parent()?.join(pointer);
    // A linked worktree's gitdir is `<main>/.git/worktrees/<name>`, whose own
    // config file is usually empty; the remotes live in the main `.git/config`.
    let text = target.to_string_lossy().to_string();
    let marker = format!("{}worktrees{}", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR);
    match text.find(&marker).or_else(|| text.find("/worktrees/")) {
        Some(at) => Some(PathBuf::from(&text[..at])),
        None => Some(target),
    }
}

/// `origin` if it exists, otherwise the first remote in the file. A repo with one
/// remote under another name is common enough (`upstream`, a fork) that refusing
/// to look would read as "Flueny does not see this repo" with no way to tell why.
pub fn read_origin_remote(git_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(git_dir.join("config")).ok()?;
    let mut remotes: Vec<(String, String)> = Vec::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if let Some(name) = line
            .strip_prefix("[remote")
            .and_then(|rest| rest.trim().strip_prefix('"'))
            .and_then(|rest| rest.strip_suffix("\"]"))
        {
            current = Some(name.to_string());
            continue;
        }
        if line.starts_with('[') {
            current = None;
            continue;
        }
        if let (Some(name), Some(rest)) = (&current, line.strip_prefix("url"))
            && let Some(value) = rest.trim_start().strip_prefix('=')
        {
            let value = value.trim();
            if !value.is_empty() && !remotes.iter().any(|(n, _)| n == name) {
                remotes.push((name.clone(), value.to_string()));
            }
        }
    }
    remotes
        .iter()
        .find(|(name, _)| name == "origin")
        .or_else(|| remotes.first())
        .map(|(_, url)| url.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::TempDir;

    #[test]
    fn origin_wins_and_the_first_remote_is_the_fallback() {
        let dir = TempDir::new();
        let git = dir.path().join(".git");
        fs::create_dir_all(&git).unwrap();
        fs::write(
            git.join("config"),
            "[core]\n\tbare = false\n[remote \"upstream\"]\n\turl = git@github.com:a/up.git\n[remote \"origin\"]\n\turl = git@github.com:a/origin.git\n",
        )
        .unwrap();
        let repo = find_repo(&dir.path().join("src/deep")).unwrap();
        assert_eq!(repo.root, dir.path());
        assert_eq!(repo.remote.as_deref(), Some("git@github.com:a/origin.git"));

        fs::write(git.join("config"), "[remote \"upstream\"]\n\turl = https://x/y\n").unwrap();
        assert_eq!(read_origin_remote(&git).as_deref(), Some("https://x/y"));
    }

    #[test]
    fn a_worktree_reads_the_main_config() {
        let dir = TempDir::new();
        let main_git = dir.path().join("main/.git");
        fs::create_dir_all(main_git.join("worktrees/feat")).unwrap();
        fs::write(
            main_git.join("config"),
            "[remote \"origin\"]\n\turl = https://github.com/a/b.git\n",
        )
        .unwrap();
        let tree = dir.path().join("tree");
        fs::create_dir_all(&tree).unwrap();
        fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", main_git.join("worktrees/feat").display()),
        )
        .unwrap();
        let repo = find_repo(&tree).unwrap();
        assert_eq!(repo.remote.as_deref(), Some("https://github.com/a/b.git"));
    }

    #[test]
    fn no_repository_is_none() {
        let dir = TempDir::new();
        // The temp dir lives outside any repository on a normal machine; if the
        // test runner itself sits in one, the walk legitimately finds it.
        if let Some(repo) = find_repo(dir.path()) {
            assert!(!repo.root.starts_with(dir.path()));
        }
    }
}
