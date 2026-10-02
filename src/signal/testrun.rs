// Test runs, from the command text and the runner's own summary (feature 0126).
//
// `testsRun` used to be a regex on the command text, so a failing `npm test`
// counted the same as a passing one. This module answers three narrower
// questions, all locally and all from text the client already reads:
//
//   was this Bash call a test invocation, and of which runner
//   how far can its exit status be trusted (a pipe or a `|| true` hides it)
//   what did the runner say it ran, passed, failed and skipped
//
// Only enums and counts leave this module. The command and the output are
// read here and discarded by the caller.
//
// The rule the whole file is written around: never guess `passed`. A run whose
// outcome cannot be established is `unknown`, because a false green inflates
// Diligence, and an inflated score is worse than a missing one.

use std::sync::LazyLock;

use regex_lite::Regex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestRunner {
    Jest,
    Vitest,
    Mocha,
    Node,
    Pytest,
    Go,
    Cargo,
    Rspec,
    Phpunit,
    Dotnet,
    Jvm,
    Playwright,
    Cypress,
    Other,
}

impl TestRunner {
    pub fn as_str(self) -> &'static str {
        match self {
            TestRunner::Jest => "jest",
            TestRunner::Vitest => "vitest",
            TestRunner::Mocha => "mocha",
            TestRunner::Node => "node",
            TestRunner::Pytest => "pytest",
            TestRunner::Go => "go",
            TestRunner::Cargo => "cargo",
            TestRunner::Rspec => "rspec",
            TestRunner::Phpunit => "phpunit",
            TestRunner::Dotnet => "dotnet",
            TestRunner::Jvm => "jvm",
            TestRunner::Playwright => "playwright",
            TestRunner::Cypress => "cypress",
            TestRunner::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Passed,
    Failed,
    Error,
    Unknown,
}

impl TestOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            TestOutcome::Passed => "passed",
            TestOutcome::Failed => "failed",
            TestOutcome::Error => "error",
            TestOutcome::Unknown => "unknown",
        }
    }
}

/// How much the shell's exit status says about the test command itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitTrust {
    /// The test command is the last thing that ran, so the status is its own.
    Full,
    /// Only `&&` follows it: an exit 0 means it passed, a non-zero might be a
    /// later command failing.
    ZeroOnly,
    /// Piped, or followed by `;` or `||`: the status belongs to something else.
    None,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestInvocation {
    /// None when the command names no runner (`npm test`, `make test`), and the
    /// runner is resolved from the output instead.
    pub runner: Option<TestRunner>,
    pub exit_trust: ExitTrust,
}

/// What a runner summary said. A field the summary did not state stays None:
/// a count the product cannot substantiate is never printed or sent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub passed: Option<u32>,
    pub failed: Option<u32>,
    pub skipped: Option<u32>,
}

// ---------------------------------------------------------------------------
// Shell scanning. Shared with weakened.rs and gitcmd.rs, which need the same
// segments. Deliberately small: quotes and the four connectors, nothing else.
// A command substitution or a heredoc is read as plain words, which can only
// make a detector miss, never invent a run.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Connector {
    And,
    Or,
    Semi,
    Pipe,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Segment {
    pub text: String,
    /// The connector that follows this segment, None for the last one.
    pub after: Option<Connector>,
}

/// Splits a command on `&&`, `||`, `;`, `|` and newlines, outside quotes.
/// A lone `&` is not a separator, because `2>&1` and `&>` are far more common
/// in agent commands than backgrounding.
pub(crate) fn segments(command: &str) -> Vec<Segment> {
    let chars: Vec<char> = command.chars().collect();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if let Some(q) = quote {
            current.push(c);
            if c == '\\' && q == '"' {
                if let Some(next) = chars.get(i + 1) {
                    current.push(*next);
                    i += 2;
                    continue;
                }
            } else if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        let next = chars.get(i + 1).copied();
        let connector = match c {
            '\'' | '"' => {
                quote = Some(c);
                current.push(c);
                i += 1;
                continue;
            }
            '\\' => {
                current.push(c);
                if let Some(n) = next {
                    current.push(n);
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            '&' if next == Some('&') => Some((Connector::And, 2)),
            '|' if next == Some('|') => Some((Connector::Or, 2)),
            // `|&` pipes stderr too. Still a pipe.
            '|' if next == Some('&') => Some((Connector::Pipe, 2)),
            '|' => Some((Connector::Pipe, 1)),
            ';' => Some((Connector::Semi, 1)),
            '\n' => Some((Connector::Semi, 1)),
            _ => None,
        };
        match connector {
            Some((kind, width)) => {
                push_segment(&mut out, &mut current, Some(kind));
                i += width;
            }
            None => {
                current.push(c);
                i += 1;
            }
        }
    }
    push_segment(&mut out, &mut current, None);
    // The final segment's `after` is None by construction. A trailing
    // connector (`npm test;`) leaves the last real segment pointing at an
    // empty one, which says nothing about trust, so it is cleared.
    if let Some(last) = out.last_mut()
        && last.after == Some(Connector::Semi)
    {
        last.after = None;
    }
    out
}

fn push_segment(out: &mut Vec<Segment>, current: &mut String, after: Option<Connector>) {
    let text = current.trim().to_string();
    current.clear();
    if text.is_empty() {
        // An empty segment carries no command, but its connector still
        // matters for the segment before it (`a && && b` is not real shell,
        // `a |\n b` is). Keep the stronger of the two on the previous one.
        if let (Some(prev), Some(kind)) = (out.last_mut(), after)
            && prev.after == Some(Connector::Semi)
        {
            prev.after = Some(kind);
        }
        return;
    }
    out.push(Segment { text, after });
}

/// Shell words with quotes removed. Bounded by the segment it is given.
pub(crate) fn words(segment: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = segment.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(n) = chars.next() {
                        current.push(n);
                    }
                } else {
                    current.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(n) = chars.next() {
                        current.push(n);
                    }
                    in_word = true;
                }
                c if c.is_whitespace() => {
                    if in_word {
                        out.push(std::mem::take(&mut current));
                        in_word = false;
                    }
                }
                _ => {
                    current.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        out.push(current);
    }
    out
}

pub(crate) fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The last path component, so `./node_modules/.bin/jest` reads as `jest`.
pub(crate) fn program_name(word: &str) -> String {
    let normalized = word.replace('\\', "/");
    let base = normalized.rsplit('/').next().unwrap_or("");
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".cmd"))
        .unwrap_or(base);
    base.to_ascii_lowercase()
}

/// Leading env assignments, split off and returned separately because some
/// of them are the signal (`HUSKY=0 git commit`), then wrappers that only
/// launch the real program. What is left starts with the program itself.
pub(crate) fn strip_wrappers(words: &[String]) -> (Vec<String>, Vec<String>) {
    let mut env = Vec::new();
    let mut rest: Vec<String> = words.to_vec();
    while let Some(first) = rest.first().cloned() {
        if is_env_assignment(&first) {
            env.push(first);
            rest.remove(0);
            continue;
        }
        let program = program_name(&first);
        let second = rest.get(1).map(|w| program_name(w));
        match program.as_str() {
            "time" | "sudo" | "env" | "nice" | "command" | "exec" | "nohup" => {
                rest.remove(0);
                // Their own flags (`sudo -E`, `env -i`) are not the program.
                while rest.first().is_some_and(|w| w.starts_with('-')) {
                    rest.remove(0);
                }
            }
            "npx" | "bunx" | "pnpx" => {
                rest.remove(0);
                while rest.first().is_some_and(|w| w.starts_with('-')) {
                    rest.remove(0);
                }
            }
            "pnpm" | "yarn" if matches!(second.as_deref(), Some("exec" | "dlx")) => {
                rest.drain(0..2);
                drop_double_dash(&mut rest);
            }
            "npm" if second.as_deref() == Some("exec") => {
                rest.drain(0..2);
                drop_double_dash(&mut rest);
            }
            "bundle" if second.as_deref() == Some("exec") => {
                rest.drain(0..2);
            }
            "poetry" | "uv" | "pipenv" | "hatch" | "pdm" | "rye" if second.as_deref() == Some("run") => {
                rest.drain(0..2);
            }
            p if is_python(p) && rest.get(1).map(String::as_str) == Some("-m") && rest.len() > 2 => {
                // `python -m pytest`: the module is the program.
                rest.drain(0..2);
            }
            _ => break,
        }
    }
    (env, rest)
}

fn drop_double_dash(rest: &mut Vec<String>) {
    while rest.first().is_some_and(|w| w.starts_with('-')) {
        rest.remove(0);
    }
}

fn is_python(program: &str) -> bool {
    program == "python"
        || program == "py"
        || program
            .strip_prefix("python")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The arguments that are not flags, in order.
fn positionals(args: &[String]) -> Vec<&str> {
    args.iter()
        .map(String::as_str)
        .filter(|a| !a.starts_with('-'))
        .collect()
}

/// `Some(runner)` when the words (wrappers already stripped) are a test
/// invocation. The inner Option is the runner, None when only the output can
/// say which one it was.
pub(crate) fn classify_words(words: &[String]) -> Option<Option<TestRunner>> {
    let program = program_name(words.first()?);
    let args = &words[1..];
    let first = positionals(args).first().copied();
    match program.as_str() {
        "npm" | "pnpm" | "yarn" | "bun" => {
            let mut rest = args;
            if matches!(rest.first().map(String::as_str), Some("run" | "run-script")) {
                rest = &rest[1..];
            } else if program == "bun" && rest.first().map(String::as_str) == Some("test") {
                // `bun test` is bun's own runner, not a package script.
                return Some(Some(TestRunner::Other));
            }
            let script = rest.first()?;
            let is_test = script == "test"
                || script.starts_with("test:")
                || (program == "npm" && (script == "t" || script == "tst"));
            is_test.then_some(None)
        }
        "deno" => (first == Some("test")).then_some(Some(TestRunner::Other)),
        "jest" => Some(Some(TestRunner::Jest)),
        "vitest" => match first {
            Some("bench" | "init" | "list" | "dev" | "serve") => None,
            _ => Some(Some(TestRunner::Vitest)),
        },
        "mocha" | "_mocha" => Some(Some(TestRunner::Mocha)),
        "node" => args.iter().any(|a| a == "--test").then_some(Some(TestRunner::Node)),
        "pytest" | "py.test" => Some(Some(TestRunner::Pytest)),
        "tox" | "nox" => Some(Some(TestRunner::Other)),
        "go" => (first == Some("test")).then_some(Some(TestRunner::Go)),
        "cargo" => {
            let rest: Vec<&str> = args
                .iter()
                .map(String::as_str)
                .filter(|a| !a.starts_with('+'))
                .collect();
            match rest.first().copied() {
                Some("test") => Some(Some(TestRunner::Cargo)),
                Some("nextest") if rest.get(1) == Some(&"run") => Some(Some(TestRunner::Cargo)),
                _ => None,
            }
        }
        "rspec" => Some(Some(TestRunner::Rspec)),
        "rake" => match first {
            Some("spec") => Some(Some(TestRunner::Rspec)),
            Some("test") => Some(Some(TestRunner::Other)),
            _ => None,
        },
        "rails" => (first == Some("test")).then_some(Some(TestRunner::Other)),
        "phpunit" | "pest" | "paratest" => Some(Some(TestRunner::Phpunit)),
        "php" => {
            let inner = first.map(program_name);
            match inner.as_deref() {
                Some("phpunit" | "pest" | "paratest") => Some(Some(TestRunner::Phpunit)),
                Some("artisan") => (positionals(args).get(1) == Some(&"test")).then_some(Some(TestRunner::Phpunit)),
                _ => None,
            }
        }
        "dotnet" => (first == Some("test")).then_some(Some(TestRunner::Dotnet)),
        "mvn" | "mvnw" => positionals(args)
            .iter()
            .any(|g| matches!(*g, "test" | "verify" | "install") || g.ends_with(":test"))
            .then_some(Some(TestRunner::Jvm)),
        "gradle" | "gradlew" => positionals(args)
            .iter()
            .any(|t| matches!(*t, "test" | "check") || t.ends_with(":test") || t.ends_with(":check"))
            .then_some(Some(TestRunner::Jvm)),
        "playwright" => (first == Some("test")).then_some(Some(TestRunner::Playwright)),
        "cypress" => (first == Some("run")).then_some(Some(TestRunner::Cypress)),
        "make" => positionals(args)
            .iter()
            .any(|t| matches!(*t, "test" | "check" | "tests"))
            .then_some(None),
        _ => None,
    }
}

pub fn detect_test_command(command: &str) -> Option<TestInvocation> {
    let segs = segments(command);
    for (index, seg) in segs.iter().enumerate() {
        let (_, rest) = strip_wrappers(&words(&seg.text));
        let Some(runner) = classify_words(&rest) else {
            continue;
        };
        return Some(TestInvocation {
            runner,
            exit_trust: trust_after(&segs[index..]),
        });
    }
    None
}

// `segs[0]` is the test segment. Its exit status is the shell's only when
// nothing after it can replace it.
fn trust_after(segs: &[Segment]) -> ExitTrust {
    let connectors: Vec<Connector> = segs.iter().filter_map(|s| s.after).collect();
    if connectors.is_empty() {
        return ExitTrust::Full;
    }
    if connectors.iter().all(|c| *c == Connector::And) {
        return ExitTrust::ZeroOnly;
    }
    ExitTrust::None
}

/// The same test command written twice reads as the same key, so a streak of
/// failures of one command can be told apart from two different commands.
pub fn command_key(command: &str) -> String {
    command
        .split_whitespace()
        .skip_while(|w| is_env_assignment(w))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Runner summaries.
// ---------------------------------------------------------------------------

const MAX_SCAN_BYTES: usize = 256 * 1024;

static ANSI: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\x1b\[[0-9;?]*[A-Za-z]").unwrap());

/// The tail of the output with colour codes and carriage returns removed. A
/// summary is at the end of a run, and a test log can be megabytes that
/// nothing here has any business walking through.
fn clean(output: &str) -> String {
    let mut start = output.len().saturating_sub(MAX_SCAN_BYTES);
    while start < output.len() && !output.is_char_boundary(start) {
        start += 1;
    }
    let tail = output.get(start..).unwrap_or("");
    ANSI.replace_all(tail, "").replace('\r', "")
}

fn num(s: &str) -> u32 {
    s.parse::<u64>().map(|n| n.min(u32::MAX as u64) as u32).unwrap_or(0)
}

/// Sums `<n> <word>` tokens, keyed by word.
fn tokens(re: &Regex, text: &str) -> Vec<(String, u32)> {
    re.captures_iter(text)
        .filter_map(|c| Some((c.get(2)?.as_str().to_string(), num(c.get(1)?.as_str()))))
        .collect()
}

fn sum_of(toks: &[(String, u32)], keys: &[&str]) -> u32 {
    toks.iter()
        .filter(|(k, _)| keys.contains(&k.as_str()))
        .fold(0u32, |acc, (_, n)| acc.saturating_add(*n))
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

re!(JEST_LINE, r"(?m)^\s*Tests:\s+(.*?)(\d+) total");
re!(
    WORD_COUNTS,
    r"(\d+) (failed|passed|skipped|todo|pending|flaky|errors?|incomplete|risky)"
);
fn parse_jest(text: &str) -> Option<Counts> {
    let caps = JEST_LINE.captures_iter(text).last()?;
    let toks = tokens(&WORD_COUNTS, caps.get(1)?.as_str());
    // Jest leaves out a zero, so a stated total makes every missing kind zero.
    Some(Counts {
        passed: Some(sum_of(&toks, &["passed"])),
        failed: Some(sum_of(&toks, &["failed"])),
        skipped: Some(sum_of(&toks, &["skipped", "todo"])),
    })
}

// ` Tests  1 failed | 5 passed | 1 skipped (7)`. `Test Files` is a count of
// files, not tests, and is never read.
re!(VITEST_LINE, r"(?m)^\s*Tests\s{2,}(\d+ [^\n]*?)\s*\((\d+)\)\s*$");
fn parse_vitest(text: &str) -> Option<Counts> {
    let caps = VITEST_LINE.captures_iter(text).last()?;
    let toks = tokens(&WORD_COUNTS, caps.get(1)?.as_str());
    if toks.is_empty() {
        return None;
    }
    Some(Counts {
        passed: Some(sum_of(&toks, &["passed"])),
        failed: Some(sum_of(&toks, &["failed"])),
        skipped: Some(sum_of(&toks, &["skipped", "todo"])),
    })
}

re!(MOCHA_PASSING, r"(?m)^\s*(\d+) passing\b");
re!(MOCHA_FAILING, r"(?m)^\s*(\d+) failing\b");
re!(MOCHA_PENDING, r"(?m)^\s*(\d+) pending\b");
fn last_num(re: &Regex, text: &str) -> Option<u32> {
    re.captures_iter(text)
        .last()
        .and_then(|c| c.get(1).map(|m| num(m.as_str())))
}
fn parse_mocha(text: &str) -> Option<Counts> {
    // Mocha always prints `N passing`, even at zero; the other two only when
    // they are not zero.
    let passed = last_num(&MOCHA_PASSING, text)?;
    Some(Counts {
        passed: Some(passed),
        failed: Some(last_num(&MOCHA_FAILING, text).unwrap_or(0)),
        skipped: Some(last_num(&MOCHA_PENDING, text).unwrap_or(0)),
    })
}

// TAP (`# pass 3`) and the spec reporter (`ℹ pass 3`). Cancelled tests fail
// the run, so they count as failed.
re!(
    NODE_LINE,
    r"(?m)^(?:#|ℹ)\s+(pass|fail|skipped|todo|cancelled|tests)\s+(\d+)\s*$"
);
fn parse_node(text: &str) -> Option<Counts> {
    let mut seen = std::collections::HashMap::new();
    for caps in NODE_LINE.captures_iter(text) {
        if let (Some(k), Some(v)) = (caps.get(1), caps.get(2)) {
            seen.insert(k.as_str().to_string(), num(v.as_str()));
        }
    }
    if !seen.contains_key("pass") && !seen.contains_key("fail") {
        return None;
    }
    let get = |k: &str| seen.get(k).copied().unwrap_or(0);
    Some(Counts {
        passed: Some(get("pass")),
        failed: Some(get("fail").saturating_add(get("cancelled"))),
        skipped: Some(get("skipped").saturating_add(get("todo"))),
    })
}

// The `===== ... in 0.12s =====` footer, and the bare `-q` form without the
// rules. Errors (setup or collection) count as failed. xfailed, xpassed and
// deselected are not counted at all: they are neither a pass the developer
// wrote nor a failure the run reports, and guessing which they are would be a
// number nobody can substantiate.
re!(PYTEST_FOOTER, r"(?m)^=+ ([^=\n]*?) in [\d.]+s(?: \([^)\n]*\))? =+\s*$");
re!(
    PYTEST_QUIET,
    r"(?m)^(\d+ (?:failed|passed|errors?|skipped)(?:, \d+ [a-z]+)*) in [\d.]+s(?: \([^)\n]*\))?\s*$"
);
re!(
    PYTEST_COUNTS,
    r"(\d+) (failed|passed|skipped|errors?|xfailed|xpassed|deselected|warnings?|rerun)"
);
fn parse_pytest(text: &str) -> Option<Counts> {
    let body = PYTEST_FOOTER
        .captures_iter(text)
        .last()
        .or_else(|| PYTEST_QUIET.captures_iter(text).last())?
        .get(1)?
        .as_str()
        .to_string();
    let toks = tokens(&PYTEST_COUNTS, &body);
    let counted = sum_of(&toks, &["failed", "passed", "skipped", "error", "errors"]);
    if counted == 0 && !body.contains("no tests ran") && toks.is_empty() {
        return None;
    }
    Some(Counts {
        passed: Some(sum_of(&toks, &["passed"])),
        failed: Some(sum_of(&toks, &["failed", "error", "errors"])),
        skipped: Some(sum_of(&toks, &["skipped"])),
    })
}

re!(
    GO_PACKAGE,
    r"(?m)^(ok|FAIL)\s+\S+\s+(?:[\d.]+s|\(cached\)|\[[^\]\n]+\])"
);
re!(GO_FAIL_LINE, r"(?m)^--- FAIL: ");
re!(GO_PASS_LINE, r"(?m)^--- PASS: ");
re!(GO_SKIP_LINE, r"(?m)^--- SKIP: ");
fn parse_go(text: &str) -> Option<Counts> {
    // Only top-level `--- FAIL:` lines count: a subtest is indented, and its
    // parent fails with it, so counting both would double the number.
    let packages: Vec<&str> = GO_PACKAGE
        .captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| m.as_str()))
        .collect();
    let fails = GO_FAIL_LINE.find_iter(text).count() as u32;
    let passes = GO_PASS_LINE.find_iter(text).count() as u32;
    let skips = GO_SKIP_LINE.find_iter(text).count() as u32;
    if packages.is_empty() && fails == 0 && passes == 0 {
        return None;
    }
    // A FAIL package with no failing test is a build failure: there is no
    // count to read, so failed stays unknown rather than zero.
    let build_failed = packages.contains(&"FAIL") && fails == 0;
    let verbose = passes > 0;
    Some(Counts {
        passed: verbose.then_some(passes),
        failed: if build_failed { None } else { Some(fails) },
        skipped: verbose.then_some(skips),
    })
}

re!(
    CARGO_RESULT,
    r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored"
);
re!(
    NEXTEST_SUMMARY,
    r"(?m)Summary \[\s*[\d.]+s\]\s+\d+ tests? run: ([^\n]*)$"
);
re!(NEXTEST_COUNTS, r"(\d+) (passed|failed|skipped|flaky|timed out)");
fn parse_cargo(text: &str) -> Option<Counts> {
    if let Some(caps) = NEXTEST_SUMMARY.captures_iter(text).last() {
        let toks = tokens(&NEXTEST_COUNTS, caps.get(1)?.as_str());
        return Some(Counts {
            passed: Some(sum_of(&toks, &["passed", "flaky"])),
            failed: Some(sum_of(&toks, &["failed", "timed out"])),
            skipped: Some(sum_of(&toks, &["skipped"])),
        });
    }
    // One line per test binary in a workspace: they add up.
    let mut found = false;
    let mut counts = (0u32, 0u32, 0u32);
    for caps in CARGO_RESULT.captures_iter(text) {
        found = true;
        let n = |i| caps.get(i).map(|m| num(m.as_str())).unwrap_or(0);
        counts.0 = counts.0.saturating_add(n(1));
        counts.1 = counts.1.saturating_add(n(2));
        counts.2 = counts.2.saturating_add(n(3));
    }
    found.then_some(Counts {
        passed: Some(counts.0),
        failed: Some(counts.1),
        skipped: Some(counts.2),
    })
}

re!(RSPEC_LINE, r"(\d+) examples?, (\d+) failures?(?:, (\d+) pending)?");
fn parse_rspec(text: &str) -> Option<Counts> {
    let caps = RSPEC_LINE.captures_iter(text).last()?;
    let n = |i| caps.get(i).map(|m| num(m.as_str())).unwrap_or(0);
    let (examples, failures, pending) = (n(1), n(2), n(3));
    Some(Counts {
        passed: Some(examples.saturating_sub(failures).saturating_sub(pending)),
        failed: Some(failures),
        skipped: Some(pending),
    })
}

re!(PHPUNIT_OK, r"OK \((\d+) tests?, \d+ assertions?\)");
re!(PHPUNIT_TESTS, r"(?m)^Tests: (\d+), Assertions: \d+([^\n]*)$");
re!(
    PHPUNIT_KINDS,
    r"(Errors|Failures|Skipped|Incomplete|Risky|Warnings|Deprecations|Notices): (\d+)"
);
re!(PEST_LINE, r"(?m)^\s*Tests:\s+(\d+ [^\n]*?)\s*\(\d+ assertions?\)");
fn parse_phpunit(text: &str) -> Option<Counts> {
    if let Some(caps) = PEST_LINE.captures_iter(text).last() {
        let toks = tokens(&WORD_COUNTS, caps.get(1)?.as_str());
        return Some(Counts {
            passed: Some(sum_of(&toks, &["passed"])),
            failed: Some(sum_of(&toks, &["failed"])),
            skipped: Some(sum_of(&toks, &["skipped", "incomplete", "todo"])),
        });
    }
    let ok = PHPUNIT_OK.captures_iter(text).last();
    let tests = PHPUNIT_TESTS.captures_iter(text).last();
    match (ok, tests) {
        (_, Some(caps)) => {
            let total = caps.get(1).map(|m| num(m.as_str())).unwrap_or(0);
            let mut kinds = std::collections::HashMap::new();
            for k in PHPUNIT_KINDS.captures_iter(caps.get(2).map(|m| m.as_str()).unwrap_or("")) {
                if let (Some(name), Some(v)) = (k.get(1), k.get(2)) {
                    kinds.insert(name.as_str().to_string(), num(v.as_str()));
                }
            }
            let get = |k: &str| kinds.get(k).copied().unwrap_or(0);
            let failed = get("Errors").saturating_add(get("Failures"));
            let skipped = get("Skipped").saturating_add(get("Incomplete"));
            Some(Counts {
                passed: Some(total.saturating_sub(failed).saturating_sub(skipped)),
                failed: Some(failed),
                skipped: Some(skipped),
            })
        }
        (Some(caps), None) => Some(Counts {
            passed: caps.get(1).map(|m| num(m.as_str())),
            failed: Some(0),
            skipped: Some(0),
        }),
        (None, None) => None,
    }
}

re!(
    DOTNET_LINE,
    r"(?:Passed|Failed)!\s+-\s+Failed:\s+(\d+),\s+Passed:\s+(\d+),\s+Skipped:\s+(\d+),\s+Total:\s+(\d+)"
);
re!(DOTNET_OLD_TOTAL, r"(?m)^\s*Total tests:\s*(\d+)");
re!(DOTNET_OLD_PASSED, r"(?m)^\s*Passed:\s*(\d+)\s*$");
re!(DOTNET_OLD_FAILED, r"(?m)^\s*Failed:\s*(\d+)\s*$");
re!(DOTNET_OLD_SKIPPED, r"(?m)^\s*Skipped:\s*(\d+)\s*$");
fn parse_dotnet(text: &str) -> Option<Counts> {
    // One line per test project: they add up.
    let mut found = false;
    let mut c = (0u32, 0u32, 0u32);
    for caps in DOTNET_LINE.captures_iter(text) {
        found = true;
        let n = |i| caps.get(i).map(|m| num(m.as_str())).unwrap_or(0);
        c.0 = c.0.saturating_add(n(2));
        c.1 = c.1.saturating_add(n(1));
        c.2 = c.2.saturating_add(n(3));
    }
    if found {
        return Some(Counts {
            passed: Some(c.0),
            failed: Some(c.1),
            skipped: Some(c.2),
        });
    }
    let total_at = DOTNET_OLD_TOTAL.find_iter(text).last()?.start();
    let after = text.get(total_at..).unwrap_or("");
    Some(Counts {
        passed: last_num(&DOTNET_OLD_PASSED, after),
        failed: Some(last_num(&DOTNET_OLD_FAILED, after).unwrap_or(0)),
        skipped: Some(last_num(&DOTNET_OLD_SKIPPED, after).unwrap_or(0)),
    })
}

re!(
    MAVEN_LINE,
    r"Tests run: (\d+), Failures: (\d+), Errors: (\d+), Skipped: (\d+)"
);
re!(GRADLE_LINE, r"(\d+) tests? completed, (\d+) failed(?:, (\d+) skipped)?");
fn parse_jvm(text: &str) -> Option<Counts> {
    // Maven prints a line per class and then the aggregate under `Results:`,
    // so the last line is the whole run.
    if let Some(caps) = MAVEN_LINE.captures_iter(text).last() {
        let n = |i| caps.get(i).map(|m| num(m.as_str())).unwrap_or(0);
        let failed = n(2).saturating_add(n(3));
        return Some(Counts {
            passed: Some(n(1).saturating_sub(failed).saturating_sub(n(4))),
            failed: Some(failed),
            skipped: Some(n(4)),
        });
    }
    let caps = GRADLE_LINE.captures_iter(text).last()?;
    let n = |i| caps.get(i).map(|m| num(m.as_str())).unwrap_or(0);
    Some(Counts {
        passed: Some(n(1).saturating_sub(n(2)).saturating_sub(n(3))),
        failed: Some(n(2)),
        skipped: Some(n(3)),
    })
}

// `  2 passed (3.4s)`, `  1 failed`, `  1 flaky`. A flaky test passed on
// retry. An interrupted one is counted as failed, the conservative side.
re!(
    PLAYWRIGHT_LINE,
    r"(?m)^\s+(\d+) (passed|failed|flaky|skipped|did not run|interrupted)(?: \([\d.]+m?s\))?\s*$"
);
fn parse_playwright(text: &str) -> Option<Counts> {
    let mut seen = std::collections::HashMap::new();
    for caps in PLAYWRIGHT_LINE.captures_iter(text) {
        if let (Some(n), Some(k)) = (caps.get(1), caps.get(2)) {
            seen.insert(k.as_str().to_string(), num(n.as_str()));
        }
    }
    if !seen.contains_key("passed") && !seen.contains_key("failed") {
        return None;
    }
    let get = |k: &str| seen.get(k).copied().unwrap_or(0);
    Some(Counts {
        passed: Some(get("passed").saturating_add(get("flaky"))),
        failed: Some(get("failed").saturating_add(get("interrupted"))),
        skipped: Some(get("skipped").saturating_add(get("did not run"))),
    })
}

re!(CYPRESS_PASSING, r"Passing:\s+(\d+)");
re!(CYPRESS_FAILING, r"Failing:\s+(\d+)");
re!(CYPRESS_PENDING, r"Pending:\s+(\d+)");
re!(CYPRESS_SKIPPED, r"Skipped:\s+(\d+)");
fn sum_all(re: &Regex, text: &str) -> u32 {
    re.captures_iter(text)
        .filter_map(|c| c.get(1).map(|m| num(m.as_str())))
        .fold(0u32, |a, b| a.saturating_add(b))
}
fn parse_cypress(text: &str) -> Option<Counts> {
    // One results box per spec file: they add up.
    if !CYPRESS_PASSING.is_match(text) || !CYPRESS_FAILING.is_match(text) {
        return None;
    }
    Some(Counts {
        passed: Some(sum_all(&CYPRESS_PASSING, text)),
        failed: Some(sum_all(&CYPRESS_FAILING, text)),
        skipped: Some(sum_all(&CYPRESS_PENDING, text).saturating_add(sum_all(&CYPRESS_SKIPPED, text))),
    })
}

type Parser = fn(&str) -> Option<Counts>;

// Order matters when nothing names the runner. Cypress before Jest, because
// both print `Tests:`; Pest (phpunit) before Jest for the same reason;
// Playwright last, because its bare `2 passed` lines are the least specific.
const PARSERS: &[(TestRunner, Parser)] = &[
    (TestRunner::Cypress, parse_cypress),
    (TestRunner::Jest, parse_jest),
    (TestRunner::Phpunit, parse_phpunit),
    (TestRunner::Vitest, parse_vitest),
    (TestRunner::Mocha, parse_mocha),
    (TestRunner::Node, parse_node),
    (TestRunner::Pytest, parse_pytest),
    (TestRunner::Cargo, parse_cargo),
    (TestRunner::Go, parse_go),
    (TestRunner::Rspec, parse_rspec),
    (TestRunner::Dotnet, parse_dotnet),
    (TestRunner::Jvm, parse_jvm),
    (TestRunner::Playwright, parse_playwright),
];

pub fn parse_summary(hint: Option<TestRunner>, output: &str) -> Option<(TestRunner, Counts)> {
    if output.is_empty() {
        return None;
    }
    let text = clean(output);
    if let Some(hint) = hint
        && let Some((runner, parser)) = PARSERS.iter().find(|(r, _)| *r == hint)
        && let Some(counts) = parser(&text)
    {
        return Some((*runner, counts));
    }
    PARSERS
        .iter()
        .filter(|(r, _)| Some(*r) != hint)
        .find_map(|(runner, parser)| parser(&text).map(|c| (*runner, c)))
}

/// The contract's outcome rules. Never guesses `passed`.
pub fn outcome(exit_code: Option<i32>, trust: ExitTrust, summary: Option<&Counts>, interrupted: bool) -> TestOutcome {
    // A run that was stopped says nothing about the code under test.
    if interrupted {
        return TestOutcome::Unknown;
    }
    // The runner said tests failed. That holds whatever the shell reported,
    // including `npm test || true`.
    if summary.is_some_and(|s| s.failed.unwrap_or(0) > 0) {
        return TestOutcome::Failed;
    }
    match (exit_code, trust) {
        // The status is the test command's own, or only `&&` follows it.
        (Some(0), ExitTrust::Full | ExitTrust::ZeroOnly) => return TestOutcome::Passed,
        // Non-zero and no failing test reported: a compile error, a missing
        // script, a coverage threshold, a crashed runner.
        (Some(_), ExitTrust::Full) => return TestOutcome::Error,
        // ZeroOnly non-zero, an untrusted status, or no status at all: only
        // the summary can answer.
        _ => {}
    }
    match summary {
        Some(s) if s.failed == Some(0) && s.passed.unwrap_or(0) > 0 => TestOutcome::Passed,
        _ => TestOutcome::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detect(cmd: &str) -> Option<(Option<TestRunner>, ExitTrust)> {
        detect_test_command(cmd).map(|i| (i.runner, i.exit_trust))
    }

    #[test]
    fn detects_runners_named_in_the_command() {
        let cases: &[(&str, Option<TestRunner>)] = &[
            ("npx jest src/auth", Some(TestRunner::Jest)),
            ("./node_modules/.bin/jest --runInBand", Some(TestRunner::Jest)),
            ("npx vitest run", Some(TestRunner::Vitest)),
            ("vitest related src/a.ts", Some(TestRunner::Vitest)),
            ("pnpm exec vitest --run", Some(TestRunner::Vitest)),
            ("npx mocha 'test/**/*.js'", Some(TestRunner::Mocha)),
            ("node --test test/", Some(TestRunner::Node)),
            (
                "node --experimental-strip-types --test test/*.test.ts",
                Some(TestRunner::Node),
            ),
            ("pytest -x tests/test_api.py", Some(TestRunner::Pytest)),
            ("python -m pytest -q", Some(TestRunner::Pytest)),
            ("python3.12 -m pytest", Some(TestRunner::Pytest)),
            ("uv run pytest", Some(TestRunner::Pytest)),
            ("poetry run pytest -k auth", Some(TestRunner::Pytest)),
            ("tox -e py312", Some(TestRunner::Other)),
            ("go test ./...", Some(TestRunner::Go)),
            ("cargo test --workspace", Some(TestRunner::Cargo)),
            ("cargo +nightly test", Some(TestRunner::Cargo)),
            ("cargo nextest run", Some(TestRunner::Cargo)),
            ("bundle exec rspec spec/models", Some(TestRunner::Rspec)),
            ("bundle exec rake spec", Some(TestRunner::Rspec)),
            ("rails test", Some(TestRunner::Other)),
            ("vendor/bin/phpunit --filter Login", Some(TestRunner::Phpunit)),
            ("./vendor/bin/pest", Some(TestRunner::Phpunit)),
            ("php artisan test", Some(TestRunner::Phpunit)),
            ("php vendor/bin/phpunit", Some(TestRunner::Phpunit)),
            ("dotnet test MyApp.sln", Some(TestRunner::Dotnet)),
            ("mvn -q test", Some(TestRunner::Jvm)),
            ("./mvnw verify", Some(TestRunner::Jvm)),
            ("./gradlew test --tests Foo", Some(TestRunner::Jvm)),
            ("gradle :app:check", Some(TestRunner::Jvm)),
            ("npx playwright test", Some(TestRunner::Playwright)),
            ("npx cypress run --spec cypress/e2e/a.cy.ts", Some(TestRunner::Cypress)),
            ("bun test", Some(TestRunner::Other)),
            ("deno test -A", Some(TestRunner::Other)),
            ("CI=true FORCE_COLOR=0 npx jest", Some(TestRunner::Jest)),
            ("time cargo test", Some(TestRunner::Cargo)),
        ];
        for (cmd, runner) in cases {
            let got = detect_test_command(cmd);
            assert_eq!(got.map(|i| i.runner), Some(*runner), "{cmd}");
        }
    }

    #[test]
    fn package_scripts_leave_the_runner_to_the_output() {
        for cmd in [
            "npm test",
            "npm t",
            "npm run test",
            "npm run test:unit",
            "pnpm test",
            "yarn test",
            "yarn test:e2e",
            "bun run test",
            "make test",
            "make -j4 check",
        ] {
            assert_eq!(detect(cmd), Some((None, ExitTrust::Full)), "{cmd}");
        }
    }

    #[test]
    fn commands_that_only_mention_a_runner_are_not_test_runs() {
        for cmd in [
            "grep jest package.json",
            "cat test.py",
            "echo npm test",
            "git commit -m \"fix go test\"",
            "npm install jest",
            "npm i -D vitest",
            "ls tests",
            "vitest bench",
            "cargo build --release",
            "go build ./...",
            "npm run lint",
            "rg 'pytest' -n",
            "node scripts/build.js",
            "mvn package -DskipTests",
            "./gradlew assemble",
            "git checkout test",
            "",
        ] {
            assert_eq!(detect(cmd), None, "{cmd}");
        }
    }

    #[test]
    fn exit_trust_follows_what_runs_after_the_test() {
        assert_eq!(detect("cd api && npm test"), Some((None, ExitTrust::Full)));
        assert_eq!(detect("npm test; "), Some((None, ExitTrust::Full)));
        assert_eq!(
            detect("npx jest && echo ok"),
            Some((Some(TestRunner::Jest), ExitTrust::ZeroOnly))
        );
        assert_eq!(detect("npm test 2>&1 | tail -20"), Some((None, ExitTrust::None)));
        assert_eq!(detect("npm test |& tee out.log"), Some((None, ExitTrust::None)));
        assert_eq!(detect("npm test || true"), Some((None, ExitTrust::None)));
        assert_eq!(detect("npm test; echo done"), Some((None, ExitTrust::None)));
        assert_eq!(
            detect("cargo test && cargo clippy | head"),
            Some((Some(TestRunner::Cargo), ExitTrust::None))
        );
        assert_eq!(
            detect("go test ./...\necho finished"),
            Some((Some(TestRunner::Go), ExitTrust::None))
        );
    }

    #[test]
    fn separators_inside_quotes_do_not_split() {
        assert_eq!(
            detect("npx jest -t 'a && b | c'"),
            Some((Some(TestRunner::Jest), ExitTrust::Full))
        );
        assert_eq!(
            detect("pytest -k \"login or logout; x\""),
            Some((Some(TestRunner::Pytest), ExitTrust::Full))
        );
    }

    #[test]
    fn the_first_test_segment_wins() {
        assert_eq!(
            detect("cargo test && npm test"),
            Some((Some(TestRunner::Cargo), ExitTrust::ZeroOnly))
        );
    }

    #[test]
    fn command_key_ignores_env_and_spacing() {
        assert_eq!(command_key("CI=1  npx   jest  src"), "npx jest src");
        assert_eq!(command_key("npx jest src"), command_key("FOO=bar npx jest src"));
        assert_ne!(command_key("npx jest a"), command_key("npx jest b"));
    }

    const JEST_FAIL: &str = "\x1b[1mPASS\x1b[22m src/a.test.ts\n\x1b[1mFAIL\x1b[22m src/b.test.ts\n  \u{25cf} b > works\n\n    expect(received).toBe(expected)\n\n\x1b[1mTest Suites: \x1b[22m\x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m, \x1b[1m\x1b[32m1 passed\x1b[39m\x1b[22m, 2 total\n\x1b[1mTests:       \x1b[22m\x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m, \x1b[1m\x1b[33m1 skipped\x1b[39m\x1b[22m, \x1b[1m\x1b[32m2 passed\x1b[39m\x1b[22m, 4 total\nSnapshots:   0 total\nTime:        1.234 s\nRan all test suites.\n";

    #[test]
    fn jest_summary_with_colour() {
        let (runner, c) = parse_summary(None, JEST_FAIL).unwrap();
        assert_eq!(runner, TestRunner::Jest);
        assert_eq!(
            c,
            Counts {
                passed: Some(2),
                failed: Some(1),
                skipped: Some(1)
            }
        );
    }

    #[test]
    fn jest_green_run_and_todo() {
        let out = "PASS src/a.test.ts\nTest Suites: 1 passed, 1 total\nTests:       3 passed, 1 todo, 4 total\nTime: 0.5 s\r\n";
        let (_, c) = parse_summary(Some(TestRunner::Jest), out).unwrap();
        assert_eq!(
            c,
            Counts {
                passed: Some(3),
                failed: Some(0),
                skipped: Some(1)
            }
        );
    }

    const VITEST_FAIL: &str = " \x1b[31m\u{276f}\x1b[39m src/math.test.ts (3 tests | 1 failed) 4ms\n\n\x1b[2m Test Files \x1b[22m \x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m\x1b[2m | \x1b[22m\x1b[1m\x1b[32m2 passed\x1b[39m\x1b[22m\x1b[90m (3)\x1b[39m\n\x1b[2m      Tests \x1b[22m \x1b[1m\x1b[31m1 failed\x1b[39m\x1b[22m\x1b[2m | \x1b[22m\x1b[1m\x1b[32m5 passed\x1b[39m\x1b[22m\x1b[2m | \x1b[22m\x1b[33m1 skipped\x1b[39m\x1b[90m (7)\x1b[39m\n\x1b[2m   Start at \x1b[22m 10:00:00\n\x1b[2m   Duration \x1b[22m 412ms\n";

    #[test]
    fn vitest_reads_tests_not_test_files() {
        let (runner, c) = parse_summary(None, VITEST_FAIL).unwrap();
        assert_eq!(runner, TestRunner::Vitest);
        assert_eq!(
            c,
            Counts {
                passed: Some(5),
                failed: Some(1),
                skipped: Some(1)
            }
        );
        let green = " Test Files  2 passed (2)\n      Tests  12 passed (12)\n   Duration  300ms\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Vitest), green).unwrap().1,
            Counts {
                passed: Some(12),
                failed: Some(0),
                skipped: Some(0)
            }
        );
    }

    #[test]
    fn mocha_summary() {
        let out = "  auth\n    \u{2713} logs in\n    1) rejects a bad password\n\n\n  3 passing (12ms)\n  2 pending\n  1 failing\n\n  1) auth\n       rejects a bad password:\n     AssertionError: expected 200 to equal 401\n";
        let (runner, c) = parse_summary(None, out).unwrap();
        assert_eq!(runner, TestRunner::Mocha);
        assert_eq!(
            c,
            Counts {
                passed: Some(3),
                failed: Some(1),
                skipped: Some(2)
            }
        );
        let green = "\n  12 passing (40ms)\n\n";
        assert_eq!(parse_summary(None, green).unwrap().1.failed, Some(0));
    }

    #[test]
    fn node_test_tap_and_spec_reporters() {
        let tap = "TAP version 13\n# Subtest: adds\nok 1 - adds\nnot ok 2 - subtracts\n1..2\n# tests 4\n# suites 0\n# pass 2\n# fail 1\n# cancelled 0\n# skipped 1\n# todo 0\n# duration_ms 51.2\n";
        let (runner, c) = parse_summary(Some(TestRunner::Node), tap).unwrap();
        assert_eq!(runner, TestRunner::Node);
        assert_eq!(
            c,
            Counts {
                passed: Some(2),
                failed: Some(1),
                skipped: Some(1)
            }
        );
        let spec = "\u{2714} adds (0.5ms)\n\u{2139} tests 3\n\u{2139} suites 0\n\u{2139} pass 3\n\u{2139} fail 0\n\u{2139} cancelled 0\n\u{2139} skipped 0\n\u{2139} todo 0\n\u{2139} duration_ms 40.1\n";
        assert_eq!(
            parse_summary(None, spec).unwrap(),
            (
                TestRunner::Node,
                Counts {
                    passed: Some(3),
                    failed: Some(0),
                    skipped: Some(0)
                }
            )
        );
    }

    #[test]
    fn pytest_footers() {
        let fail = "============================= test session starts ==============================\ncollected 8 items\n\ntests/test_api.py ..F.s.xE\n\n=================================== FAILURES ===================================\n\x1b[31m\x1b[1m===== \x1b[31m1 failed\x1b[0m, \x1b[32m3 passed\x1b[0m, \x1b[33m1 skipped\x1b[0m, \x1b[33m1 xfailed\x1b[0m, \x1b[31m1 error\x1b[0m\x1b[31m in 0.12s\x1b[0m\x1b[31m =====\x1b[0m\n";
        let (runner, c) = parse_summary(None, fail).unwrap();
        assert_eq!(runner, TestRunner::Pytest);
        assert_eq!(
            c,
            Counts {
                passed: Some(3),
                failed: Some(2),
                skipped: Some(1)
            }
        );
        let green = "collected 5 items\n\ntests/test_a.py .....  [100%]\n\n============================== 5 passed in 0.03s ===============================\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Pytest), green).unwrap().1,
            Counts {
                passed: Some(5),
                failed: Some(0),
                skipped: Some(0)
            }
        );
        let none =
            "collected 0 items\n\n============================ no tests ran in 0.01s =============================\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Pytest), none).unwrap().1,
            Counts {
                passed: Some(0),
                failed: Some(0),
                skipped: Some(0)
            }
        );
        let quiet = "..F\nFAILED tests/test_a.py::test_x - assert 1 == 2\n1 failed, 2 passed in 0.05s\n";
        assert_eq!(
            parse_summary(None, quiet).unwrap(),
            (
                TestRunner::Pytest,
                Counts {
                    passed: Some(2),
                    failed: Some(1),
                    skipped: Some(0)
                }
            )
        );
    }

    #[test]
    fn go_test_package_and_verbose_lines() {
        let quiet = "ok  \tgithub.com/acme/api/auth\t0.012s\n--- FAIL: TestCharge (0.00s)\n    charge_test.go:21: got 3, want 4\nFAIL\nFAIL\tgithub.com/acme/api/billing\t0.020s\nok  \tgithub.com/acme/api/db\t(cached)\nFAIL\n";
        let (runner, c) = parse_summary(Some(TestRunner::Go), quiet).unwrap();
        assert_eq!(runner, TestRunner::Go);
        assert_eq!(
            c,
            Counts {
                passed: None,
                failed: Some(1),
                skipped: None
            }
        );

        let verbose = "=== RUN   TestA\n--- PASS: TestA (0.00s)\n=== RUN   TestB\n    --- FAIL: TestB/sub (0.00s)\n--- FAIL: TestB (0.00s)\n=== RUN   TestC\n--- SKIP: TestC (0.00s)\nFAIL\nFAIL\tgithub.com/acme/x\t0.01s\n";
        assert_eq!(
            parse_summary(None, verbose).unwrap(),
            (
                TestRunner::Go,
                Counts {
                    passed: Some(1),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );

        let build = "# github.com/acme/x\n./x.go:3:2: undefined: foo\nFAIL\tgithub.com/acme/x [build failed]\nFAIL\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Go), build).unwrap().1,
            Counts {
                passed: None,
                failed: None,
                skipped: None
            }
        );
        let green = "ok  \tgithub.com/acme/x\t0.004s\n";
        assert_eq!(parse_summary(None, green).unwrap().1.failed, Some(0));
    }

    #[test]
    fn cargo_sums_every_binary_and_reads_nextest() {
        let out = "   Compiling flueny v0.2.0\n    Finished `test` profile\n     Running unittests src/lib.rs\n\nrunning 4 tests\ntest a ... ok\ntest b ... ignored\n\ntest result: \x1b[32mok\x1b[0m. 3 passed; 0 failed; 1 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\n     Running tests/redaction.rs\n\nrunning 2 tests\ntest leak ... FAILED\n\ntest result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s\n\nerror: test failed, to rerun pass `--test redaction`\n";
        // The colour sits between `result:` and `ok`, so it has to come off first.
        let (runner, c) = parse_summary(Some(TestRunner::Cargo), out).unwrap();
        assert_eq!(runner, TestRunner::Cargo);
        assert_eq!(
            c,
            Counts {
                passed: Some(4),
                failed: Some(1),
                skipped: Some(1)
            }
        );
        let nextest = "    Starting 12 tests across 3 binaries\n        PASS [   0.004s] flueny wire::tests::a\n------------\n     Summary [   0.123s] 12 tests run: 10 passed, 1 failed, 1 skipped\n";
        assert_eq!(
            parse_summary(None, nextest).unwrap(),
            (
                TestRunner::Cargo,
                Counts {
                    passed: Some(10),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );
    }

    #[test]
    fn rspec_phpunit_and_pest() {
        let rspec = "..F*.\n\nFailures:\n\n  1) User validates email\n\nFinished in 0.4 seconds (files took 1.2 seconds to load)\n5 examples, 1 failure, 1 pending\n";
        assert_eq!(
            parse_summary(None, rspec).unwrap(),
            (
                TestRunner::Rspec,
                Counts {
                    passed: Some(3),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );
        let ok = "PHPUnit 11.2.1 by Sebastian Bergmann.\n\n...                                                                 3 / 3 (100%)\n\nTime: 00:00.012, Memory: 8.00 MB\n\nOK (3 tests, 5 assertions)\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Phpunit), ok).unwrap().1,
            Counts {
                passed: Some(3),
                failed: Some(0),
                skipped: Some(0)
            }
        );
        let failing = "..FES\n\nThere was 1 failure:\n\n1) LoginTest::testRejects\n\nFAILURES!\nTests: 5, Assertions: 9, Failures: 1, Errors: 1, Skipped: 1, Incomplete: 0.\n";
        assert_eq!(
            parse_summary(None, failing).unwrap(),
            (
                TestRunner::Phpunit,
                Counts {
                    passed: Some(2),
                    failed: Some(2),
                    skipped: Some(1)
                }
            )
        );
        let pest = "   PASS  Tests\\Unit\\ExampleTest\n  \u{2713} that true is true\n\n  Tests:    1 failed, 2 passed (3 assertions)\n  Duration: 0.12s\n";
        assert_eq!(
            parse_summary(None, pest).unwrap(),
            (
                TestRunner::Phpunit,
                Counts {
                    passed: Some(2),
                    failed: Some(1),
                    skipped: Some(0)
                }
            )
        );
    }

    #[test]
    fn dotnet_sums_projects_and_reads_the_old_format() {
        let out = "Passed!  - Failed:     0, Passed:     3, Skipped:     0, Total:     3, Duration: 12 ms - Api.Tests.dll (net8.0)\nFailed!  - Failed:     1, Passed:     4, Skipped:     1, Total:     6, Duration: 40 ms - Core.Tests.dll (net8.0)\n";
        assert_eq!(
            parse_summary(None, out).unwrap(),
            (
                TestRunner::Dotnet,
                Counts {
                    passed: Some(7),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );
        let old = "Starting test execution, please wait...\n\nTotal tests: 3\n     Passed: 2\n     Failed: 1\n Total time: 1.2 Seconds\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Dotnet), old).unwrap().1,
            Counts {
                passed: Some(2),
                failed: Some(1),
                skipped: Some(0)
            }
        );
    }

    #[test]
    fn maven_reads_the_aggregate_and_gradle_its_line() {
        let maven = "[INFO] Tests run: 2, Failures: 0, Errors: 0, Skipped: 0, Time elapsed: 0.1 s - in com.acme.ATest\n[ERROR] Tests run: 2, Failures: 1, Errors: 0, Skipped: 1, Time elapsed: 0.1 s <<< FAILURE! - in com.acme.BTest\n[INFO] \n[INFO] Results:\n[INFO] \n[ERROR] Tests run: 4, Failures: 1, Errors: 0, Skipped: 1\n[INFO] BUILD FAILURE\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Jvm), maven).unwrap().1,
            Counts {
                passed: Some(2),
                failed: Some(1),
                skipped: Some(1)
            }
        );
        let gradle = "> Task :app:test FAILED\n\nAppTest > adds() FAILED\n    org.opentest4j.AssertionFailedError at AppTest.java:12\n\n5 tests completed, 1 failed, 1 skipped\n\nFAILURE: Build failed with an exception.\n";
        assert_eq!(
            parse_summary(None, gradle).unwrap(),
            (
                TestRunner::Jvm,
                Counts {
                    passed: Some(3),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );
        assert_eq!(parse_summary(Some(TestRunner::Jvm), "BUILD SUCCESSFUL in 3s\n"), None);
    }

    #[test]
    fn playwright_and_cypress() {
        let pw = "Running 6 tests using 2 workers\n\n  \u{2718}  1 [chromium] \u{203a} login.spec.ts:3:1 \u{203a} logs in (1.2s)\n\n  1 failed\n    [chromium] \u{203a} login.spec.ts:3:1 \u{203a} logs in\n  1 flaky\n    [chromium] \u{203a} cart.spec.ts:9:1 \u{203a} adds\n  1 skipped\n  3 passed (3.4s)\n";
        assert_eq!(
            parse_summary(Some(TestRunner::Playwright), pw).unwrap().1,
            Counts {
                passed: Some(4),
                failed: Some(1),
                skipped: Some(1)
            }
        );
        let cy = "  (Results)\n\n  \u{250c}\u{2500}\u{2500}\u{2510}\n  \u{2502} Tests:        3        \u{2502}\n  \u{2502} Passing:      2        \u{2502}\n  \u{2502} Failing:      1        \u{2502}\n  \u{2502} Pending:      0        \u{2502}\n  \u{2502} Skipped:      0        \u{2502}\n  \u{2514}\u{2500}\u{2500}\u{2518}\n  (Results)\n  \u{2502} Tests:        2        \u{2502}\n  \u{2502} Passing:      1        \u{2502}\n  \u{2502} Failing:      0        \u{2502}\n  \u{2502} Pending:      1        \u{2502}\n  \u{2502} Skipped:      0        \u{2502}\n\n  \u{2716}  1 of 2 failed (50%)                     00:04        5        3        1        1        -\n";
        assert_eq!(
            parse_summary(None, cy).unwrap(),
            (
                TestRunner::Cypress,
                Counts {
                    passed: Some(3),
                    failed: Some(1),
                    skipped: Some(1)
                }
            )
        );
    }

    #[test]
    fn a_hint_that_does_not_match_falls_back_to_the_output() {
        let (runner, _) = parse_summary(Some(TestRunner::Jest), VITEST_FAIL).unwrap();
        assert_eq!(runner, TestRunner::Vitest);
    }

    #[test]
    fn no_summary_is_none_and_never_panics() {
        assert_eq!(parse_summary(None, ""), None);
        assert_eq!(parse_summary(None, "npm ERR! Missing script: \"test\"\n"), None);
        assert_eq!(
            parse_summary(Some(TestRunner::Cargo), "error[E0425]: cannot find value\n"),
            None
        );
        // Multi-byte text across the scan boundary must not split a char.
        let big = format!(
            "{}\u{00e9}\n{}",
            "\u{00e9}".repeat(200_000),
            "Tests:       1 passed, 1 total\n"
        );
        assert_eq!(parse_summary(None, &big).unwrap().0, TestRunner::Jest);
        let huge_numbers = "Tests:       99999999999999999999 passed, 1 total\n";
        assert!(parse_summary(None, huge_numbers).is_some());
    }

    #[test]
    fn only_the_tail_is_scanned() {
        let early = "Tests:       5 failed, 5 total\n";
        let filler = "x".repeat(MAX_SCAN_BYTES + 10);
        let late = "Tests:       5 passed, 5 total\n";
        let out = format!("{early}{filler}\n{late}");
        assert_eq!(parse_summary(None, &out).unwrap().1.failed, Some(0));
    }

    fn c(p: Option<u32>, f: Option<u32>) -> Counts {
        Counts {
            passed: p,
            failed: f,
            skipped: None,
        }
    }

    #[test]
    fn outcome_rules() {
        use ExitTrust::*;
        use TestOutcome::*;
        // Interrupted is unknown whatever else is true.
        assert_eq!(outcome(Some(0), Full, Some(&c(Some(3), Some(0))), true), Unknown);
        // A reported failure wins over every exit status.
        assert_eq!(outcome(Some(0), Full, Some(&c(Some(3), Some(1))), false), Failed);
        assert_eq!(outcome(Some(0), None, Some(&c(Some(3), Some(1))), false), Failed);
        assert_eq!(
            outcome(Option::None, None, Some(&c(Option::None, Some(2))), false),
            Failed
        );
        // Trusted exit 0.
        assert_eq!(outcome(Some(0), Full, Option::None, false), Passed);
        assert_eq!(outcome(Some(0), ZeroOnly, Option::None, false), Passed);
        // Trusted non-zero with no failing test: error.
        assert_eq!(outcome(Some(1), Full, Option::None, false), Error);
        assert_eq!(outcome(Some(1), Full, Some(&c(Some(5), Some(0))), false), Error);
        assert_eq!(
            outcome(Some(2), Full, Some(&c(Option::None, Option::None)), false),
            Error
        );
        // ZeroOnly non-zero: only the summary can say.
        assert_eq!(outcome(Some(1), ZeroOnly, Option::None, false), Unknown);
        assert_eq!(outcome(Some(1), ZeroOnly, Some(&c(Some(4), Some(0))), false), Passed);
        // Never guess passed.
        assert_eq!(outcome(Option::None, Full, Option::None, false), Unknown);
        assert_eq!(outcome(Some(0), None, Option::None, false), Unknown);
        assert_eq!(outcome(Some(0), None, Some(&c(Some(4), Some(0))), false), Passed);
        assert_eq!(outcome(Some(0), None, Some(&c(Some(0), Some(0))), false), Unknown);
        assert_eq!(
            outcome(Option::None, Full, Some(&c(Option::None, Some(0))), false),
            Unknown
        );
    }

    #[test]
    fn end_to_end_npm_test_piped_into_tail() {
        let inv = detect_test_command("npm test 2>&1 | tail -5").unwrap();
        let summary = parse_summary(inv.runner, JEST_FAIL);
        let (runner, counts) = summary.unwrap();
        assert_eq!(runner, TestRunner::Jest);
        assert_eq!(
            outcome(Some(0), inv.exit_trust, Some(&counts), false),
            TestOutcome::Failed
        );
        assert_eq!(outcome(Some(0), inv.exit_trust, None, false), TestOutcome::Unknown);
    }

    #[test]
    fn enum_strings_match_the_contract() {
        assert_eq!(TestRunner::Jvm.as_str(), "jvm");
        assert_eq!(TestRunner::Node.as_str(), "node");
        assert_eq!(TestOutcome::Error.as_str(), "error");
        assert_eq!(TestOutcome::Unknown.as_str(), "unknown");
    }

    #[test]
    fn words_and_segments() {
        assert_eq!(
            words(r#"git commit -m "fix: a \"b\" c" 'd e' f\ g"#),
            vec!["git", "commit", "-m", "fix: a \"b\" c", "d e", "f g"]
        );
        let segs = segments("a && b || c; d | e");
        let kinds: Vec<_> = segs.iter().map(|s| s.after).collect();
        assert_eq!(
            kinds,
            vec![
                Some(Connector::And),
                Some(Connector::Or),
                Some(Connector::Semi),
                Some(Connector::Pipe),
                None
            ]
        );
        assert_eq!(segments("echo '&&' && x").len(), 2);
        assert!(segments("unterminated 'quote && x").len() == 1);
    }
}
