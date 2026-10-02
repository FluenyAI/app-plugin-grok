// Git commands that put a file back the way it was (feature 0126).
//
// The `reverted` edit decision existed in the contract from M1 and was never
// emitted, so the revert penalty was dead code. This reads the Bash command
// text locally and says which paths it restores. Paths are returned exactly as
// written: the caller resolves them against the hook's `cwd` (and a `git -C`
// directory is not applied here), classifies them, and sends only the class.
//
// Nothing here runs git. A command like `git revert <sha>` names a commit, not
// files, and answering which files it touches would mean reading the object
// database, so it is reported as `RevertOther` for the caller to decide.

use super::testrun::{program_name, segments, strip_wrappers, words};
use super::weakened::split_git;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevertCommand {
    /// Specific paths (or `.`) restored in the working tree.
    Paths(Vec<String>),
    /// The whole working tree: `git reset --hard`, `git stash`.
    All,
    /// `git revert HEAD`: undoes the most recent commit.
    RevertHead,
    /// `git revert <anything else>`: a commit this module cannot map to files.
    RevertOther,
}

/// One entry per git invocation in the command that restores something.
pub fn revert_targets(command: &str) -> Vec<RevertCommand> {
    let mut out = Vec::new();
    for seg in segments(command) {
        let (_, rest) = strip_wrappers(&words(&seg.text));
        let Some(first) = rest.first() else {
            continue;
        };
        if program_name(first) != "git" {
            continue;
        }
        let (_, sub, args) = split_git(&rest[1..]);
        let found = match sub {
            Some("checkout") => checkout(args),
            Some("restore") => restore(args),
            Some("reset") => args.iter().any(|a| a == "--hard").then_some(RevertCommand::All),
            Some("stash") => stash(args),
            Some("revert") => revert(args),
            _ => None,
        };
        out.extend(found);
    }
    out
}

/// A segment that runs `git commit` for real. The caller uses it to know which
/// agent edits are now committed, which `git revert HEAD` would undo.
pub fn is_git_commit(command: &str) -> bool {
    segments(command).iter().any(|seg| {
        let (_, rest) = strip_wrappers(&words(&seg.text));
        let Some(first) = rest.first() else {
            return false;
        };
        if program_name(first) != "git" {
            return false;
        }
        let (_, sub, args) = split_git(&rest[1..]);
        sub == Some("commit") && !args.iter().any(|a| a == "--dry-run")
    })
}

// `git checkout -- a` restores. `git checkout main` switches branches. Without
// a `--` git decides by looking at the disk, which this cannot, so: two or more
// positionals are `<ref> <paths>`, and a single one is a path only when it is
// `.` or plainly a file (a dot in its last component, or a leading `./`). A
// branch called `feature/x` stays a branch switch.
fn checkout(args: &[String]) -> Option<RevertCommand> {
    if let Some(at) = args.iter().position(|a| a == "--") {
        let paths: Vec<String> = args.get(at + 1..)?.to_vec();
        return (!paths.is_empty()).then_some(RevertCommand::Paths(paths));
    }
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "-b" | "-B" | "--orphan" | "--detach" | "-"))
    {
        return None;
    }
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    match positional.as_slice() {
        [] => None,
        [only] => looks_like_path(only).then(|| RevertCommand::Paths(vec![(*only).clone()])),
        [_, paths @ ..] => Some(RevertCommand::Paths(paths.iter().map(|p| (*p).clone()).collect())),
    }
}

fn looks_like_path(arg: &str) -> bool {
    if arg == "." || arg.starts_with("./") || arg.starts_with("../") {
        return true;
    }
    let last = arg.rsplit('/').next().unwrap_or(arg);
    // `v1.2` is a tag, `README.md` is a file. A dot followed by a letter in
    // the last component reads as an extension.
    last.rsplit_once('.')
        .is_some_and(|(stem, ext)| !stem.is_empty() && ext.chars().next().is_some_and(|c| c.is_ascii_alphabetic()))
}

// `git restore --staged a` only touches the index: the working tree, which is
// where the agent's edit is, is unchanged. Adding `--worktree` makes it both.
fn restore(args: &[String]) -> Option<RevertCommand> {
    let mut staged = false;
    let mut worktree = false;
    let mut paths = Vec::new();
    let mut skip_next = false;
    let mut literal = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if literal {
            paths.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => literal = true,
            "--staged" | "-S" => staged = true,
            "--worktree" | "-W" => worktree = true,
            "-SW" | "-WS" => {
                staged = true;
                worktree = true;
            }
            "-s" | "--source" => skip_next = true,
            a if a.starts_with('-') => {}
            a => paths.push(a.to_string()),
        }
    }
    if staged && !worktree {
        return None;
    }
    (!paths.is_empty()).then_some(RevertCommand::Paths(paths))
}

// `git stash` and `git stash push` put the working tree back to HEAD. With a
// pathspec, only those paths. The read-only and re-apply subcommands do not.
fn stash(args: &[String]) -> Option<RevertCommand> {
    let sub = args.iter().find(|a| !a.starts_with('-')).map(String::as_str);
    match sub {
        None => Some(RevertCommand::All),
        Some("push") => {
            let after: Vec<String> = args
                .iter()
                .skip_while(|a| a.as_str() != "push")
                .skip(1)
                .cloned()
                .collect();
            let paths = stash_push_paths(&after);
            if paths.is_empty() {
                Some(RevertCommand::All)
            } else {
                Some(RevertCommand::Paths(paths))
            }
        }
        Some("save") => Some(RevertCommand::All),
        // A bare word after flags, like `git stash -m msg`: the word is the
        // message, and the stash is a push.
        Some(_) if args.first().is_some_and(|a| a.starts_with('-')) => {
            let first_word = sub.unwrap_or("");
            if matches!(
                first_word,
                "pop" | "apply" | "list" | "show" | "drop" | "clear" | "branch" | "create" | "store"
            ) {
                None
            } else {
                Some(RevertCommand::All)
            }
        }
        Some(_) => None,
    }
}

fn stash_push_paths(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip_next = false;
    let mut literal = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if literal {
            out.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => literal = true,
            "-m" | "--message" | "--pathspec-from-file" => skip_next = true,
            a if a.starts_with('-') => {}
            a => out.push(a.to_string()),
        }
    }
    out
}

fn revert(args: &[String]) -> Option<RevertCommand> {
    if args
        .iter()
        .any(|a| matches!(a.as_str(), "--abort" | "--continue" | "--quit" | "--skip"))
    {
        return None;
    }
    let mut targets = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "-m" | "--mainline" | "-S" | "--strategy" | "-X" | "--strategy-option" => skip_next = true,
            a if a.starts_with('-') => {}
            a => targets.push(a),
        }
    }
    if targets.is_empty() {
        return None;
    }
    let is_head = |t: &str| matches!(t, "HEAD" | "HEAD~0" | "HEAD^0" | "@");
    if targets.iter().all(|t| is_head(t)) {
        Some(RevertCommand::RevertHead)
    } else {
        Some(RevertCommand::RevertOther)
    }
}

#[cfg(test)]
mod tests {
    use super::RevertCommand::*;
    use super::*;

    fn p(paths: &[&str]) -> RevertCommand {
        Paths(paths.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn checkout() {
        assert_eq!(
            revert_targets("git checkout -- src/a.ts src/b.ts"),
            vec![p(&["src/a.ts", "src/b.ts"])]
        );
        assert_eq!(revert_targets("git checkout HEAD -- src/a.ts"), vec![p(&["src/a.ts"])]);
        assert_eq!(
            revert_targets("git checkout origin/main -- src/a.ts"),
            vec![p(&["src/a.ts"])]
        );
        assert_eq!(revert_targets("git checkout ."), vec![p(&["."])]);
        assert_eq!(revert_targets("git checkout -- ."), vec![p(&["."])]);
        assert_eq!(
            revert_targets("git checkout src/auth/session.ts"),
            vec![p(&["src/auth/session.ts"])]
        );
        assert_eq!(revert_targets("git checkout HEAD src/a.ts"), vec![p(&["src/a.ts"])]);
        // Branch switches are not reverts.
        assert_eq!(revert_targets("git checkout main"), vec![]);
        assert_eq!(revert_targets("git checkout feature/login"), vec![]);
        assert_eq!(revert_targets("git checkout v1.2"), vec![]);
        assert_eq!(revert_targets("git checkout -b fix/x origin/main"), vec![]);
        assert_eq!(revert_targets("git checkout -"), vec![]);
    }

    #[test]
    fn restore() {
        assert_eq!(
            revert_targets("git restore src/a.ts src/b.ts"),
            vec![p(&["src/a.ts", "src/b.ts"])]
        );
        assert_eq!(
            revert_targets("git restore --worktree src/a.ts"),
            vec![p(&["src/a.ts"])]
        );
        assert_eq!(
            revert_targets("git restore --staged --worktree src/a.ts"),
            vec![p(&["src/a.ts"])]
        );
        assert_eq!(revert_targets("git restore -s HEAD src/a.ts"), vec![p(&["src/a.ts"])]);
        assert_eq!(
            revert_targets("git restore --source=HEAD~1 src/a.ts"),
            vec![p(&["src/a.ts"])]
        );
        assert_eq!(revert_targets("git restore ."), vec![p(&["."])]);
        assert_eq!(
            revert_targets("git restore -- 'src/my file.ts'"),
            vec![p(&["src/my file.ts"])]
        );
        // Index only: the agent's edit is still in the working tree.
        assert_eq!(revert_targets("git restore --staged src/a.ts"), vec![]);
        assert_eq!(revert_targets("git restore"), vec![]);
    }

    #[test]
    fn reset_stash_revert_clean() {
        assert_eq!(revert_targets("git reset --hard"), vec![All]);
        assert_eq!(revert_targets("git reset --hard HEAD~1"), vec![All]);
        assert_eq!(revert_targets("git reset HEAD src/a.ts"), vec![]);
        assert_eq!(revert_targets("git reset --soft HEAD~1"), vec![]);
        assert_eq!(revert_targets("git stash"), vec![All]);
        assert_eq!(revert_targets("git stash push"), vec![All]);
        assert_eq!(revert_targets("git stash -u"), vec![All]);
        assert_eq!(revert_targets("git stash -m wip"), vec![All]);
        assert_eq!(revert_targets("git stash save 'wip'"), vec![All]);
        assert_eq!(
            revert_targets("git stash push -m wip -- src/a.ts"),
            vec![p(&["src/a.ts"])]
        );
        for c in [
            "git stash pop",
            "git stash apply",
            "git stash list",
            "git stash show -p",
            "git stash drop",
        ] {
            assert_eq!(revert_targets(c), vec![], "{c}");
        }
        assert_eq!(revert_targets("git revert HEAD"), vec![RevertHead]);
        assert_eq!(revert_targets("git revert --no-edit HEAD"), vec![RevertHead]);
        assert_eq!(revert_targets("git revert HEAD~0"), vec![RevertHead]);
        assert_eq!(revert_targets("git revert abc1234"), vec![RevertOther]);
        assert_eq!(revert_targets("git revert -m 1 HEAD"), vec![RevertHead]);
        assert_eq!(revert_targets("git revert --abort"), vec![]);
        assert_eq!(revert_targets("git revert --continue"), vec![]);
        assert_eq!(revert_targets("git clean -fd"), vec![]);
    }

    #[test]
    fn globals_wrappers_and_several_segments() {
        assert_eq!(revert_targets("git -C ../api restore src/a.ts"), vec![p(&["src/a.ts"])]);
        assert_eq!(
            revert_targets("cd api && git --no-pager checkout -- a.ts && git stash pop"),
            vec![p(&["a.ts"])]
        );
        assert_eq!(
            revert_targets("git restore a.ts; git reset --hard"),
            vec![p(&["a.ts"]), All]
        );
        assert_eq!(revert_targets("echo git reset --hard"), vec![]);
        assert_eq!(revert_targets("git commit -m 'git reset --hard'"), vec![]);
        assert_eq!(revert_targets(""), vec![]);
    }

    #[test]
    fn commits() {
        assert!(is_git_commit("git add -A && git commit -m 'feat: x'"));
        assert!(is_git_commit("HUSKY=0 git -C repo commit -am x"));
        assert!(!is_git_commit("git commit --dry-run -m x"));
        assert!(!is_git_commit("git log --grep commit"));
        assert!(!is_git_commit("echo git commit"));
    }
}
