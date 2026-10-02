// Weakened tests (feature 0126): did the agent get to green by making the
// tests ask for less?
//
// Everything here runs on text the client already reads locally: the old and
// new strings of an edit, and the Bash command. What leaves is a flag from a
// fixed list of eight. No string from the edit or the command is returned.
//
// Every edit check is count based: a flag is raised only when the edit adds
// more of a marker than it removes (or, for assertions and tests, removes more
// than it adds). An edit that touches a file which already has a `.skip`, or
// that moves an assertion from one place to another, is not a weakening, and
// flagging it would be a judgement the product cannot substantiate.

use std::sync::LazyLock;

use regex_lite::Regex;

use super::testrun::{classify_words, program_name, segments, strip_wrappers, words};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WeakenedFlag {
    SkipAdded,
    OnlyAdded,
    AssertionRemoved,
    TestDeleted,
    SnapshotUpdated,
    TypeSuppression,
    LintSuppression,
    NoVerify,
}

impl WeakenedFlag {
    pub const ALL: [WeakenedFlag; 8] = [
        WeakenedFlag::SkipAdded,
        WeakenedFlag::OnlyAdded,
        WeakenedFlag::AssertionRemoved,
        WeakenedFlag::TestDeleted,
        WeakenedFlag::SnapshotUpdated,
        WeakenedFlag::TypeSuppression,
        WeakenedFlag::LintSuppression,
        WeakenedFlag::NoVerify,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            WeakenedFlag::SkipAdded => "skip-added",
            WeakenedFlag::OnlyAdded => "only-added",
            WeakenedFlag::AssertionRemoved => "assertion-removed",
            WeakenedFlag::TestDeleted => "test-deleted",
            WeakenedFlag::SnapshotUpdated => "snapshot-updated",
            WeakenedFlag::TypeSuppression => "type-suppression",
            WeakenedFlag::LintSuppression => "lint-suppression",
            WeakenedFlag::NoVerify => "no-verify",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.as_str() == s)
    }
}

const TEST_DIRS: &[&str] = &[
    "tests",
    "test",
    "spec",
    "specs",
    "__tests__",
    "e2e",
    "cypress",
    "playwright",
];

/// A test file, by the policy bundle's class or by the naming conventions of
/// the runners `testrun.rs` knows.
pub fn is_test_path(rel_path: &str, path_class: Option<&str>) -> bool {
    if path_class == Some("tests") {
        return true;
    }
    let path = rel_path.replace('\\', "/");
    let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty() && *p != ".").collect();
    let Some(name) = parts.pop() else {
        return false;
    };
    if parts
        .iter()
        .any(|dir| TEST_DIRS.contains(&dir.to_ascii_lowercase().as_str()))
    {
        return true;
    }
    let lower = name.to_ascii_lowercase();
    lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains(".cy.")
        || lower.ends_with("_test.go")
        || (lower.starts_with("test_") && lower.ends_with(".py"))
        || lower.ends_with("_test.py")
        || lower.ends_with("_spec.rb")
        || lower.ends_with("_test.rb")
        || name.ends_with("Test.java")
        || name.ends_with("Tests.java")
        || name.ends_with("IT.java")
        || name.ends_with("Test.kt")
        || name.ends_with("Tests.kt")
        || name.ends_with("Tests.cs")
        || name.ends_with("Test.cs")
        || name.ends_with("Test.php")
        || name.ends_with("Tests.swift")
}

/// Snapshot files: jest and vitest `.snap`, insta `.snap`, syrupy `.ambr`.
pub fn is_snapshot_path(rel_path: &str) -> bool {
    let path = rel_path.replace('\\', "/");
    path.ends_with(".snap") || path.ends_with(".ambr") || path.split('/').any(|p| p == "__snapshots__")
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).unwrap());
    };
}

// The `(?:^|[^.\w$])` guard keeps `model.fit(` and `/re/.test(` from reading
// as a focused or declared test.
re!(
    SKIP_MARKERS,
    concat!(
        r"(?m)\b(?:it|test|describe|context|suite|specify)\.skip\b",
        r"|(?:^|[^.\w$])x(?:it|describe|test|context|specify)\s*\(",
        r"|\bthis\.skip\s*\(",
        r"|@pytest\.mark\.skip",
        r"|\bpytest\.skip\s*\(",
        r"|@unittest\.skip",
        r"|\bt\.Skip(?:f|Now)?\s*\(",
        r"|#\[ignore(?:\s*=[^\]\n]*)?\]",
        r"|@Disabled\b",
        r"|@Ignore\b",
        r"|\[(?:Fact|Theory)\s*\(\s*Skip\s*=",
        r"|\[Ignore\b",
        r"|\bmarkTest(?:Skipped|Incomplete)\s*\("
    )
);
// rspec's bare `skip "why"`, `pending "why"` and `xit "does"`, only in a spec
// file, because `skip "x"` in Ruby is otherwise an ordinary method call.
re!(RSPEC_SKIP, r#"(?m)^\s*(?:skip|pending|xit|xdescribe|xcontext)\s+['"]"#);
re!(
    ONLY_MARKERS,
    concat!(
        r"\b(?:it|test|describe|context|suite|specify)\.only\b",
        r"|(?:^|[^.\w$])f(?:it|describe|test|context)\s*\(",
        r"|\bfocus:\s*true\b",
        r"|[,\s]:focus\b"
    )
);
re!(
    ASSERTIONS,
    concat!(
        r"(?m)\bexpect\s*[\(\{]",
        r"|\bassert(?:_eq|_ne|_matches)?!\s*\(",
        r"|\b(?:require|assert)\.\w+\s*\(",
        r"|\bassert\w*\s*\(",
        r"|\bAssert\.\w+\s*\(",
        r"|^\s*assert\s",
        r"|\.should\b",
        r"|\bshould\s*\(",
        r"|\bt\.(?:Errorf|Error|Fatalf|Fatal|FailNow|Fail)\s*\(",
        r"|\bXCTAssert\w*\s*\("
    )
);
// Declarations include the skipped and focused forms, so turning `it(` into
// `it.skip(` is a skip, not a deleted test.
re!(
    TEST_DECLS,
    concat!(
        r"(?m)(?:^|[^.\w$])[xf]?(?:it|test|specify)(?:\.(?:skip|only|todo|each|concurrent|failing))*\s*[\(`]",
        r"|\bdef test_?\w*\s*\(",
        r"|\bfunc Test\w*\s*\(",
        r"|#\[(?:\w+::)*test(?:\([^\]\n]*\))?\]",
        r"|@Test\b",
        r"|\[(?:Fact|Theory|Test|TestMethod|TestCase)\b",
        r"|\bpublic function test\w*\s*\(",
        r#"|^\s*[xf]?(?:it|specify|scenario)\s+['"]"#
    )
);
re!(
    TYPE_SUPPRESSIONS,
    r"@ts-ignore|@ts-expect-error|@ts-nocheck|\bas\s+any\b|<any>|#\s*type:\s*ignore|#\s*pyright:\s*ignore"
);
re!(
    LINT_SUPPRESSIONS,
    concat!(
        r"eslint-disable|biome-ignore|#\s*noqa\b|#\s*pylint:\s*disable|#!?\[allow\(",
        r"|//\s*nolint\b|rubocop:disable|@SuppressWarnings\b|@phpstan-ignore|@psalm-suppress",
        r"|swiftlint:disable|#pragma\s+warning\s+disable|//\s*NOSONAR"
    )
);

fn count(re: &Regex, text: &str) -> usize {
    re.find_iter(text).count()
}

fn added(re: &Regex, old: &str, new: &str) -> bool {
    count(re, new) > count(re, old)
}

fn removed(re: &Regex, old: &str, new: &str) -> bool {
    count(re, new) < count(re, old)
}

/// Flags for one applied (or declined) edit, from its before and after text.
pub fn flags_for_edit(rel_path: &str, path_class: Option<&str>, old: &str, new: &str) -> Vec<WeakenedFlag> {
    let mut flags = Vec::new();
    let is_test = is_test_path(rel_path, path_class);
    let is_spec_rb = rel_path.ends_with("_spec.rb");

    let skip = added(&SKIP_MARKERS, old, new) || (is_spec_rb && added(&RSPEC_SKIP, old, new));
    if skip {
        flags.push(WeakenedFlag::SkipAdded);
    }
    if added(&ONLY_MARKERS, old, new) {
        flags.push(WeakenedFlag::OnlyAdded);
    }
    if is_test {
        if removed(&ASSERTIONS, old, new) {
            flags.push(WeakenedFlag::AssertionRemoved);
        }
        let emptied = new.trim().is_empty() && !old.trim().is_empty();
        if emptied || removed(&TEST_DECLS, old, new) {
            flags.push(WeakenedFlag::TestDeleted);
        }
    }
    if is_snapshot_path(rel_path) && old != new {
        flags.push(WeakenedFlag::SnapshotUpdated);
    }
    if added(&TYPE_SUPPRESSIONS, old, new) {
        flags.push(WeakenedFlag::TypeSuppression);
    }
    if added(&LINT_SUPPRESSIONS, old, new) {
        flags.push(WeakenedFlag::LintSuppression);
    }
    flags.sort();
    flags.dedup();
    flags
}

const HOOKED_GIT: &[&str] = &["commit", "push", "merge", "rebase", "cherry-pick", "am"];

/// Flags from a Bash command. `is_test` decides whether a removed path was a
/// test file; the caller owns path resolution and classification.
pub fn flags_for_command(command: &str, is_test: &dyn Fn(&str) -> bool) -> Vec<WeakenedFlag> {
    let mut flags = Vec::new();
    for seg in segments(command) {
        let (env, rest) = strip_wrappers(&words(&seg.text));
        let Some(first) = rest.first() else {
            continue;
        };
        let program = program_name(first);
        let args = &rest[1..];

        if program == "git" {
            git_flags(&env, args, is_test, &mut flags);
        }

        if env.iter().any(|e| e == "INSTA_UPDATE=always")
            || args.iter().any(|a| {
                matches!(
                    a.as_str(),
                    "--update-snapshots" | "--updateSnapshot" | "--snapshot-update"
                ) || a.starts_with("--update-snapshots=")
            })
        {
            flags.push(WeakenedFlag::SnapshotUpdated);
        }
        // `-u` is everywhere (`git push -u`), so it only counts on a test run.
        if classify_words(&rest).is_some() && args.iter().any(|a| a == "-u" || a == "--update") {
            flags.push(WeakenedFlag::SnapshotUpdated);
        }
        if program == "cargo"
            && args.first().map(String::as_str) == Some("insta")
            && (matches!(args.get(1).map(String::as_str), Some("accept" | "review"))
                || args.iter().any(|a| a == "--accept"))
        {
            flags.push(WeakenedFlag::SnapshotUpdated);
        }

        if matches!(program.as_str(), "rm" | "unlink" | "del" | "trash") && paths_of(args).iter().any(|p| is_test(p)) {
            flags.push(WeakenedFlag::TestDeleted);
        }
    }
    flags.sort();
    flags.dedup();
    flags
}

fn git_flags(env: &[String], args: &[String], is_test: &dyn Fn(&str) -> bool, flags: &mut Vec<WeakenedFlag>) {
    let (globals, sub, sub_args) = split_git(args);
    let Some(sub) = sub else {
        return;
    };
    if HOOKED_GIT.contains(&sub) {
        let hooks_off = env.iter().any(|e| e == "HUSKY=0" || e.starts_with("SKIP="))
            || globals.iter().any(|g| g.starts_with("core.hooksPath="));
        if hooks_off && matches!(sub, "commit" | "push") {
            flags.push(WeakenedFlag::NoVerify);
        }
        if sub_args.iter().any(|a| a == "--no-verify") {
            flags.push(WeakenedFlag::NoVerify);
        }
        if sub == "commit" && commit_short_no_verify(sub_args) {
            flags.push(WeakenedFlag::NoVerify);
        }
    }
    if sub == "rm" && paths_of(sub_args).iter().any(|p| is_test(p)) {
        flags.push(WeakenedFlag::TestDeleted);
    }
}

/// `git <globals> <sub> <args>`. Returns the `-c` values among the globals,
/// because `-c core.hooksPath=/dev/null` is a no-verify by another name.
pub(crate) fn split_git(args: &[String]) -> (Vec<&str>, Option<&str>, &[String]) {
    let mut configs = Vec::new();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        match arg.as_str() {
            "-C" | "--git-dir" | "--work-tree" | "--namespace" => i += 2,
            "-c" => {
                if let Some(v) = args.get(i + 1) {
                    configs.push(v.as_str());
                }
                i += 2;
            }
            a if a.starts_with('-') => i += 1,
            sub => return (configs, Some(sub), args.get(i + 1..).unwrap_or(&[])),
        }
    }
    (configs, None, &[])
}

// `git commit -n` and clusters like `-nm`. A value-taking short flag ends the
// cluster, so the `n` in `-mnope` is part of a message, not a flag, and the
// word after `-m` is skipped whole.
fn commit_short_no_verify(args: &[String]) -> bool {
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--" {
            break;
        }
        if matches!(
            arg.as_str(),
            "--message"
                | "--file"
                | "--author"
                | "--date"
                | "--reuse-message"
                | "--reedit-message"
                | "--fixup"
                | "--squash"
                | "--template"
                | "--trailer"
        ) {
            skip_next = true;
            continue;
        }
        let Some(cluster) = arg.strip_prefix('-') else {
            continue;
        };
        if cluster.starts_with('-') {
            continue;
        }
        for (idx, c) in cluster.char_indices() {
            if c == 'n' {
                return true;
            }
            if matches!(c, 'm' | 'F' | 'C' | 'c' | 't') {
                if idx + c.len_utf8() == cluster.len() {
                    skip_next = true;
                }
                break;
            }
        }
    }
    false
}

/// Non-flag arguments, plus everything after `--`.
pub(crate) fn paths_of(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut literal = false;
    for arg in args {
        if literal {
            out.push(arg.as_str());
        } else if arg == "--" {
            literal = true;
        } else if !arg.starts_with('-') {
            out.push(arg.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::WeakenedFlag::*;
    use super::*;

    fn edit(path: &str, old: &str, new: &str) -> Vec<WeakenedFlag> {
        flags_for_edit(path, None, old, new)
    }

    fn test_files(p: &str) -> bool {
        is_test_path(p, None)
    }

    fn cmd(c: &str) -> Vec<WeakenedFlag> {
        flags_for_command(c, &test_files)
    }

    #[test]
    fn flag_strings_round_trip() {
        for flag in WeakenedFlag::ALL {
            assert_eq!(WeakenedFlag::parse(flag.as_str()), Some(flag));
        }
        assert_eq!(WeakenedFlag::parse("skipped"), None);
        assert_eq!(NoVerify.as_str(), "no-verify");
    }

    #[test]
    fn test_paths() {
        for p in [
            "src/auth/session.test.ts",
            "src/a.spec.tsx",
            "pkg/charge_test.go",
            "tests/test_api.py",
            "app/test_models.py",
            "app/models_test.py",
            "spec/models/user_spec.rb",
            "src/test/java/com/acme/UserServiceTest.java",
            "src/test/kotlin/FooTests.kt",
            "Api.Tests/UserTests.cs",
            "tests/Unit/LoginTest.php",
            "src/__tests__/a.ts",
            "e2e/login.ts",
            "cypress/e2e/cart.cy.ts",
            "src\\auth\\session.test.ts",
        ] {
            assert!(is_test_path(p, None), "{p}");
        }
        for p in [
            "src/auth/session.ts",
            "src/testing.ts",
            "src/contest.py",
            "README.md",
            "attest/main.go",
            "",
        ] {
            assert!(!is_test_path(p, None), "{p}");
        }
        assert!(is_test_path("anything.rs", Some("tests")));
    }

    #[test]
    fn snapshot_paths() {
        assert!(is_snapshot_path("src/__snapshots__/a.test.ts.snap"));
        assert!(is_snapshot_path("tests/snapshots/x__y.snap"));
        assert!(is_snapshot_path("tests/__snapshots__/test_api.ambr"));
        assert!(!is_snapshot_path("src/snapshot.ts"));
    }

    #[test]
    fn typescript_skip_only_and_moves() {
        let old =
            "describe('auth', () => {\n  it('logs in', async () => {\n    expect(await login()).toBe(true)\n  })\n})\n";
        let skipped = old.replace("it('logs in'", "it.skip('logs in'");
        assert_eq!(edit("src/auth.test.ts", old, &skipped), vec![SkipAdded]);
        let x = old.replace("it('logs in'", "xit('logs in'");
        assert_eq!(edit("src/auth.test.ts", old, &x), vec![SkipAdded]);
        let only = old.replace("describe('auth'", "describe.only('auth'");
        assert_eq!(edit("src/auth.test.ts", old, &only), vec![OnlyAdded]);
        let fit = old.replace("it('logs in'", "fit('logs in'");
        assert_eq!(edit("src/auth.test.ts", old, &fit), vec![OnlyAdded]);
        // An edit to a file that already has a skip, adding nothing new.
        let both = format!("{skipped}\n// note");
        assert_eq!(edit("src/auth.test.ts", &skipped, &both), vec![]);
        // `model.fit(` and `/x/.test(` are not tests.
        assert_eq!(
            edit("src/train.ts", "const a = 1", "model.fit(data)\nconst ok = /x/.test(s)"),
            vec![]
        );
    }

    #[test]
    fn typescript_assertions_and_deleted_tests() {
        let old = "it('charges', () => {\n  expect(total).toBe(4)\n  expect(tax).toBe(1)\n})\n";
        let fewer = "it('charges', () => {\n  expect(total).toBe(4)\n})\n";
        assert_eq!(edit("src/charge.test.ts", old, fewer), vec![AssertionRemoved]);
        // Same assertion in a source file is not a test change.
        assert_eq!(edit("src/charge.ts", old, fewer), vec![]);
        // Moving an assertion is not removing it.
        let moved = "it('charges', () => {\n  expect(tax).toBe(1)\n  expect(total).toBe(4)\n})\n";
        assert_eq!(edit("src/charge.test.ts", old, moved), vec![]);
        // Adding a test and an assertion is not flagged.
        let more = format!("{old}it('refunds', () => {{\n  expect(refund()).toBe(0)\n}})\n");
        assert_eq!(edit("src/charge.test.ts", old, &more), vec![]);
        // Deleting a whole case removes a declaration and its assertions.
        let two = format!("{old}it('refunds', () => {{\n  expect(refund()).toBe(0)\n}})\n");
        assert_eq!(
            edit("src/charge.test.ts", &two, old),
            vec![AssertionRemoved, TestDeleted]
        );
        // Emptying the file.
        assert_eq!(
            edit("src/charge.test.ts", old, "  \n"),
            vec![AssertionRemoved, TestDeleted]
        );
        // test.each with a template literal still declares a test.
        assert_eq!(
            edit("a.test.ts", "test.each`\n a | b\n`('x', () => {})", ""),
            vec![TestDeleted]
        );
    }

    #[test]
    fn python() {
        let old = "def test_login(client):\n    resp = client.post('/login')\n    assert resp.status_code == 200\n    assert resp.json()['ok']\n";
        let skipped = format!("@pytest.mark.skip(reason='flaky')\n{old}");
        assert_eq!(edit("tests/test_auth.py", old, &skipped), vec![SkipAdded]);
        let weaker = "def test_login(client):\n    resp = client.post('/login')\n    assert resp.status_code == 200\n";
        assert_eq!(edit("tests/test_auth.py", old, weaker), vec![AssertionRemoved]);
        let unit = "    self.assertEqual(a, b)\n    self.assertTrue(c)\n";
        assert_eq!(
            edit("tests/test_x.py", unit, "    self.assertEqual(a, b)\n"),
            vec![AssertionRemoved]
        );
        assert_eq!(
            edit("app/models.py", "x = foo()", "x = foo()  # type: ignore"),
            vec![TypeSuppression]
        );
        assert_eq!(
            edit("app/models.py", "import os", "import os  # noqa: F401"),
            vec![LintSuppression]
        );
    }

    #[test]
    fn go() {
        let old = "func TestCharge(t *testing.T) {\n\tif got := Charge(3); got != 4 {\n\t\tt.Errorf(\"got %d\", got)\n\t}\n}\n";
        let skipped = old.replace("{\n\tif", "{\n\tt.Skip(\"later\")\n\tif");
        assert_eq!(edit("billing/charge_test.go", old, &skipped), vec![SkipAdded]);
        let gutted = "func TestCharge(t *testing.T) {\n\tCharge(3)\n}\n";
        assert_eq!(edit("billing/charge_test.go", old, gutted), vec![AssertionRemoved]);
        assert_eq!(
            edit("billing/charge.go", "x := 1", "x := 1 //nolint:ineffassign"),
            vec![LintSuppression]
        );
    }

    #[test]
    fn rust() {
        let old = "#[test]\nfn adds() {\n    assert_eq!(add(1, 2), 3);\n}\n";
        let ignored = "#[test]\n#[ignore = \"slow\"]\nfn adds() {\n    assert_eq!(add(1, 2), 3);\n}\n";
        // In src/lib.rs, not a test path: #[ignore] is specific enough anyway.
        assert_eq!(edit("src/lib.rs", old, ignored), vec![SkipAdded]);
        assert_eq!(
            edit("tests/math.rs", old, "#[test]\nfn adds() {\n    add(1, 2);\n}\n"),
            vec![AssertionRemoved]
        );
        assert_eq!(
            edit(
                "tests/math.rs",
                &format!("{old}#[tokio::test]\nasync fn b() {{}}\n"),
                old
            ),
            vec![TestDeleted]
        );
        assert_eq!(
            edit("src/lib.rs", "fn a() {}", "#[allow(dead_code)]\nfn a() {}"),
            vec![LintSuppression]
        );
    }

    #[test]
    fn ruby() {
        let old = "describe User do\n  it 'validates email' do\n    expect(user).to be_valid\n  end\nend\n";
        let skipped = old.replace("  it 'validates", "  xit 'validates");
        assert_eq!(edit("spec/models/user_spec.rb", old, &skipped), vec![SkipAdded]);
        let focused = old.replace("it 'validates email'", "it 'validates email', focus: true");
        assert_eq!(edit("spec/models/user_spec.rb", old, &focused), vec![OnlyAdded]);
        let pending = old.replace("    expect", "    pending 'later'\n    expect");
        assert_eq!(edit("spec/models/user_spec.rb", old, &pending), vec![SkipAdded]);
        assert_eq!(
            edit(
                "app/models/user.rb",
                "def a; end",
                "def a; end # rubocop:disable Metrics"
            ),
            vec![LintSuppression]
        );
    }

    #[test]
    fn java_csharp_php() {
        let java = "  @Test\n  void adds() {\n    assertEquals(3, add(1, 2));\n  }\n";
        let disabled = java.replace("  @Test\n", "  @Test\n  @Disabled\n");
        assert_eq!(edit("src/test/java/MathTest.java", java, &disabled), vec![SkipAdded]);
        let cs = "    [Fact]\n    public void Adds() { Assert.Equal(3, Add(1, 2)); }\n";
        let cs_skip = cs.replace("[Fact]", "[Fact(Skip = \"flaky\")]");
        assert_eq!(edit("Math.Tests/MathTests.cs", cs, &cs_skip), vec![SkipAdded]);
        assert_eq!(
            edit(
                "Math.Tests/MathTests.cs",
                cs,
                "    [Fact]\n    public void Adds() { Add(1, 2); }\n"
            ),
            vec![AssertionRemoved]
        );
        let php = "    public function testLogin(): void\n    {\n        $this->assertTrue($ok);\n        $this->assertSame(200, $code);\n    }\n";
        let php_less = "    public function testLogin(): void\n    {\n        $this->assertTrue($ok);\n    }\n";
        assert_eq!(edit("tests/LoginTest.php", php, php_less), vec![AssertionRemoved]);
        let php_skip = php.replace("    {\n", "    {\n        $this->markTestSkipped('later');\n");
        assert_eq!(edit("tests/LoginTest.php", php, &php_skip), vec![SkipAdded]);
    }

    #[test]
    fn suppressions_in_any_file() {
        assert_eq!(
            edit("src/a.ts", "const x = y", "// @ts-expect-error\nconst x = y as any"),
            vec![TypeSuppression]
        );
        assert_eq!(
            edit(
                "src/a.ts",
                "const x = y",
                "// eslint-disable-next-line no-console\nconst x = y"
            ),
            vec![LintSuppression]
        );
        // Removing a suppression is the opposite of weakening.
        assert_eq!(edit("src/a.ts", "// @ts-ignore\nconst x = y", "const x = y"), vec![]);
        // `as anything` is not `as any`.
        assert_eq!(edit("src/a.ts", "", "const x = y as anything"), vec![]);
    }

    #[test]
    fn snapshots() {
        assert_eq!(
            edit(
                "src/__snapshots__/a.test.ts.snap",
                "exports[`a`] = `1`;",
                "exports[`a`] = `2`;"
            ),
            vec![SnapshotUpdated]
        );
        assert_eq!(edit("src/__snapshots__/a.test.ts.snap", "x", "x"), vec![]);
    }

    #[test]
    fn no_verify_commands() {
        assert_eq!(cmd("git commit --no-verify -m 'wip'"), vec![NoVerify]);
        assert_eq!(cmd("git commit -n -m wip"), vec![NoVerify]);
        assert_eq!(cmd("git commit -anm wip"), vec![NoVerify]);
        assert_eq!(cmd("git push --no-verify origin main"), vec![NoVerify]);
        assert_eq!(cmd("HUSKY=0 git commit -m x"), vec![NoVerify]);
        assert_eq!(cmd("SKIP=eslint git commit -m x"), vec![NoVerify]);
        assert_eq!(cmd("git -c core.hooksPath=/dev/null commit -m x"), vec![NoVerify]);
        assert_eq!(cmd("git -C repo rebase --no-verify main"), vec![NoVerify]);
        // `-m` takes the next word: a message that looks like a flag is not one.
        assert_eq!(cmd("git commit -m -nothing"), vec![]);
        assert_eq!(cmd("git commit -mnope"), vec![]);
        assert_eq!(cmd("git commit -m 'fix --no-verify docs'"), vec![]);
        assert_eq!(cmd("git log -n 5"), vec![]);
        assert_eq!(cmd("git push -u origin feat/x"), vec![]);
        assert_eq!(cmd("echo git commit --no-verify"), vec![]);
    }

    #[test]
    fn snapshot_commands() {
        assert_eq!(cmd("npx jest -u"), vec![SnapshotUpdated]);
        assert_eq!(cmd("npx jest --updateSnapshot src"), vec![SnapshotUpdated]);
        assert_eq!(cmd("npx vitest run -u"), vec![SnapshotUpdated]);
        assert_eq!(cmd("npm test -- -u"), vec![SnapshotUpdated]);
        assert_eq!(cmd("npx playwright test --update-snapshots"), vec![SnapshotUpdated]);
        assert_eq!(cmd("cargo insta accept"), vec![SnapshotUpdated]);
        assert_eq!(cmd("cargo insta test --accept"), vec![SnapshotUpdated]);
        assert_eq!(cmd("INSTA_UPDATE=always cargo test"), vec![SnapshotUpdated]);
        assert_eq!(cmd("pytest --snapshot-update"), vec![SnapshotUpdated]);
        assert_eq!(cmd("npx jest"), vec![]);
        assert_eq!(cmd("sort -u names.txt"), vec![]);
    }

    #[test]
    fn deleted_test_files() {
        assert_eq!(cmd("rm src/auth.test.ts"), vec![TestDeleted]);
        assert_eq!(cmd("rm -f src/a.ts tests/test_b.py"), vec![TestDeleted]);
        assert_eq!(cmd("git rm -q spec/user_spec.rb"), vec![TestDeleted]);
        assert_eq!(cmd("rm -rf dist && rm src/a.ts"), vec![]);
        assert_eq!(cmd("rm -- -weird.test.ts"), vec![TestDeleted]);
    }

    #[test]
    fn several_flags_in_one_command_are_sorted_and_deduped() {
        assert_eq!(
            cmd("rm a.test.ts && npx jest -u && git commit -n -m x && git push --no-verify"),
            vec![TestDeleted, SnapshotUpdated, NoVerify]
        );
    }
}
