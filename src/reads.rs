// What this client reads on the machine, derives from, and discards.
//
// Design decision 57 split one promise into two. `neverSent` is a claim about the
// backend, which the server can enforce. This is the other claim, about a binary
// on someone else's laptop, which the server can only believe, so the client
// declares it at handshake and the server echoes it attributed to this agent and
// clientVersion. A confident false statement about the developer's own machine,
// on the one page whose entire value is being checkable, is the worst outcome.
//
// THE RULE THIS FILE EXISTS TO ENFORCE: a new local read gets an entry here in the
// same commit. The drift test below fails when a module starts reading the
// filesystem without either declaring what it reads or being listed as
// infrastructure that touches no developer content.

pub struct LocalRead {
    /// Developer-facing, rendered verbatim on `/coding/privacy`.
    pub what: &'static str,
    /// The module that performs the read. Checked by the drift test.
    pub site: &'static str,
    /// Why it is read at all, so the page can answer "and why do you need that".
    pub why: &'static str,
}

pub const READS_LOCALLY: &[LocalRead] = &[
    LocalRead {
        what: "Raw tool inputs and outputs, including file contents, diffs and command output",
        site: "src/extract.rs",
        why: "To derive which class of path was touched, which kind of tool ran and whether tests ran. Nothing read here is transmitted.",
    },
    LocalRead {
        what: "The session transcript, tool-use decision records only",
        site: "src/transcript.rs",
        why: "Claude Code does not fire a hook when you decline an edit, so declines exist nowhere else. Prompt text and assistant replies are never decoded.",
    },
    LocalRead {
        what: "Your prompt and the agent's reply, only for a turn scored while prompt insight scoring or live feedback is on for you",
        site: "src/prompt_insight.rs",
        why: "Prompt insight scoring (feature 0094) grades the prompt for Description and hands back a tip; live feedback (feature 0098) hands back a real-time coaching nudge instead. Held as local values for one request, never written to disk, never logged. Off by default for both; your organisation can enforce either on or off, or leave it to you.",
    },
    LocalRead {
        what: "The output of test commands the agent runs, to read the pass, fail and skip counts the test runner printed",
        site: "src/signal/testrun.rs",
        why: "To report whether a test run passed, failed or errored. Only the outcome, the runner name and three counts leave this machine; the output itself is discarded with the hook process.",
    },
    LocalRead {
        what: "The old and new text of agent edits, and the text of Bash commands, scanned for weakened tests: skipped or focused tests, removed assertions, deleted tests, updated snapshots, type and lint suppressions, and --no-verify",
        site: "src/signal/weakened.rs",
        why: "So a change that gets to green by weakening the tests is told apart from one that fixed the code. Only the kind of change is sent, never the text it was found in.",
    },
    LocalRead {
        what: "The content of a file the agent edited, right after the edit, reduced to a hash on this machine",
        site: "src/revert.rs",
        why: "To notice when an edit puts a file back the way it was before the agent touched it, which counts as a reverted agent edit. The content and the hash both stay here.",
    },
];

/// The wire form. Order is stable so a diff between two client versions is readable.
pub fn reads_locally_declaration() -> Vec<String> {
    READS_LOCALLY.iter().map(|read| read.what.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    // Modules that touch the filesystem but never developer content. Each one is
    // listed with why, because "it is fine" is what an exemption list degrades into.
    const INFRASTRUCTURE: &[(&str, &str)] = &[
        (
            "src/store.rs",
            "reads only this client's own config, queue and ledger under its config dir",
        ),
        (
            "src/credentials.rs",
            "reads only this client's own credential file, the fallback when no OS store exists",
        ),
        (
            "src/git.rs",
            "reads the git remote, which is in the contract and is sent as a hashed repoId",
        ),
        ("src/testing.rs", "test scaffolding, compiled only into tests"),
        ("src/reads.rs", "this drift test reads the crate's own source"),
        ("src/hooks_tests.rs", "tests only, compiled only into tests"),
        ("src/api.rs", "reads an HTTP response body, not a file"),
        (
            "src/cli.rs",
            "reads the hook payload from stdin, which extract.rs then derives from and discards",
        ),
    ];

    const FS_READ: &[&str] = &[
        "fs::read",
        "read_to_string(",
        "File::open",
        "OpenOptions",
        "read_dir(",
        "read_at(",
        "read_exact(",
    ];

    fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn a_module_that_reads_the_filesystem_is_declared_or_named_as_infrastructure() {
        let mut files = Vec::new();
        sources(&root().join("src"), &mut files);
        let mut undeclared = Vec::new();
        for file in files {
            let rel = file.strip_prefix(root()).unwrap().to_string_lossy().replace('\\', "/");
            // Only non-test code counts: a test module may read a fixture.
            let body = std::fs::read_to_string(&file).unwrap();
            let code = body.split("#[cfg(test)]").next().unwrap_or("");
            if !FS_READ.iter().any(|needle| code.contains(needle)) {
                continue;
            }
            let declared = READS_LOCALLY.iter().any(|read| read.site == rel);
            let infra = INFRASTRUCTURE.iter().any(|(site, _)| *site == rel);
            if !declared && !infra {
                undeclared.push(rel);
            }
        }
        assert!(
            undeclared.is_empty(),
            "{undeclared:?} reads the filesystem but is not declared in src/reads.rs and is not named as \
             infrastructure. Add an entry saying what it reads and why, or add it to INFRASTRUCTURE here \
             with a reason. Do not delete this test."
        );
    }

    #[test]
    fn every_declared_read_site_exists_and_every_entry_is_answerable() {
        assert!(!READS_LOCALLY.is_empty());
        for read in READS_LOCALLY {
            assert!(
                root().join(read.site).is_file(),
                "{} is declared but does not exist",
                read.site
            );
            assert!(read.what.len() > 20, "too terse to be meaningful: {}", read.what);
            assert!(
                read.why.len() > 20,
                "a read with no stated reason invites the worst reading: {}",
                read.what
            );
            // The backend accepts at most 500 characters per entry and 50 entries.
            assert!(read.what.len() <= 500);
            let text = format!("{} {}", read.what, read.why);
            assert!(
                !text.contains('\u{2013}') && !text.contains('\u{2014}'),
                "no em or en dashes"
            );
        }
        assert!(READS_LOCALLY.len() <= 50);
    }

    #[test]
    fn the_transcript_the_raw_payload_and_the_0126_reads_are_declared_by_name() {
        let sites: Vec<&str> = READS_LOCALLY.iter().map(|read| read.site).collect();
        for site in [
            "src/extract.rs",
            "src/transcript.rs",
            "src/signal/testrun.rs",
            "src/signal/weakened.rs",
            "src/revert.rs",
        ] {
            assert!(sites.contains(&site), "{site} is not declared");
        }
    }
}
