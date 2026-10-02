// The path classifier. Eng finding 9: one classifier feeds extraction here today
// and Cedar at M3, so the rules ship in the policy bundle rather than being
// compiled into the client. What does NOT ship is the sensitive-path list the
// scorers use, per eng finding 12, so nothing here can be read back as "which
// paths Flueny grades you on".
//
// The output is a single short label like "tests" or "auth". The path itself is
// never transmitted, and this is the only thing derived from it.

use serde_json::{Map, Value};

/// Backend order (coding-bundle.service.ts) is significant: first match wins, and
/// the broad language buckets are last. The map preserves JSON insertion order,
/// and it is iterated in that order and never sorted.
pub fn classify_path(classifier: &Map<String, Value>, rel_path: &str) -> Option<String> {
    let normalized = rel_path.replace('\\', "/");
    let mut path = normalized.as_str();
    if let Some(rest) = path.strip_prefix("./") {
        path = rest;
    }
    let path = path.trim_start_matches('/');
    if path.is_empty() {
        return None;
    }
    for (path_class, patterns) in classifier {
        let Some(patterns) = patterns.as_array() else { continue };
        for pattern in patterns.iter().filter_map(Value::as_str) {
            if glob_match(pattern, path) {
                return Some(path_class.clone());
            }
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Literal(char),
    // `*`: anything but a slash.
    Star,
    // `**` not followed by a slash: anything at all.
    AnyDeep,
    // `**/`: zero or more whole directories, so `**/*.md` matches `README.md`.
    AnyDirs,
    // `?`: one character that is not a slash.
    One,
}

fn tokenize(pattern: &str) -> Vec<Token> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    out.push(Token::AnyDirs);
                    i += 3;
                } else {
                    out.push(Token::AnyDeep);
                    i += 2;
                }
            }
            '*' => {
                out.push(Token::Star);
                i += 1;
            }
            '?' => {
                out.push(Token::One);
                i += 1;
            }
            c => {
                out.push(Token::Literal(c));
                i += 1;
            }
        }
    }
    out
}

/// A deliberately small glob subset: `**`, `*` and `?`, which is everything the
/// bundle uses, matched directly with no regex. The patterns are server-authored
/// and this runs on every tool call.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let tokens = tokenize(pattern);
    let text: Vec<char> = path.chars().collect();
    matches(&tokens, &text)
}

fn matches(tokens: &[Token], text: &[char]) -> bool {
    // Memoized over (token index, text index); patterns and paths are short.
    let mut memo = vec![vec![None::<bool>; text.len() + 1]; tokens.len() + 1];
    fn go(t: usize, s: usize, tokens: &[Token], text: &[char], memo: &mut Vec<Vec<Option<bool>>>) -> bool {
        if let Some(known) = memo[t][s] {
            return known;
        }
        let result = match tokens.get(t) {
            None => s == text.len(),
            Some(Token::Literal(c)) => text.get(s) == Some(c) && go(t + 1, s + 1, tokens, text, memo),
            Some(Token::One) => text.get(s).is_some_and(|c| *c != '/') && go(t + 1, s + 1, tokens, text, memo),
            Some(Token::Star) => {
                let mut end = s;
                loop {
                    if go(t + 1, end, tokens, text, memo) {
                        break true;
                    }
                    if end >= text.len() || text[end] == '/' {
                        break false;
                    }
                    end += 1;
                }
            }
            Some(Token::AnyDeep) => (s..=text.len()).any(|end| go(t + 1, end, tokens, text, memo)),
            Some(Token::AnyDirs) => {
                // Zero directories, or any prefix that ends just after a slash.
                go(t + 1, s, tokens, text, memo)
                    || (s..text.len()).any(|i| text[i] == '/' && go(t + 1, i + 1, tokens, text, memo))
            }
        };
        memo[t][s] = Some(result);
        result
    }
    go(0, 0, tokens, text, &mut memo)
}

#[cfg(test)]
pub(crate) fn test_classifier() -> Map<String, Value> {
    // The bundle exactly as coding-bundle.service.ts builds it, in its order.
    serde_json::from_str(
        r#"{
        "tests": ["**/*.test.*", "**/*.spec.*", "**/tests/**", "**/__tests__/**", "test/**"],
        "auth": ["**/auth/**", "**/*auth*", "**/session*", "**/*jwt*", "**/*passkey*"],
        "security": ["**/crypto/**", "**/*secret*", "**/*credential*", "**/security/**"],
        "payments": ["**/billing/**", "**/payments/**", "**/*stripe*", "**/*invoice*"],
        "infra": ["**/Dockerfile*", "**/docker-compose*.yml", "**/*.tf", ".github/workflows/**", "deploy/**", "k8s/**"],
        "migrations": ["**/migrations/**"],
        "config": ["**/*.env*", "**/*.config.*", "**/*.yaml", "**/*.yml"],
        "docs": ["**/*.md", "docs/**"],
        "frontend": ["**/*.tsx", "**/components/**", "**/styles/**"],
        "backend": ["**/*.ts", "**/*.py", "**/*.go", "**/*.rs", "src/**"]
    }"#,
    )
    .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::to_repo_relative;

    #[test]
    fn paths_classify_to_the_first_matching_class_in_bundle_order() {
        let classifier = test_classifier();
        for (path, expected) in [
            ("src/integrations/coding/coding.spec.ts", Some("tests")),
            ("test/e2e/login.ts", Some("tests")),
            ("src/auth/jwt.strategy.ts", Some("auth")),
            ("src/billing/invoice.ts", Some("payments")),
            ("deploy/k8s.yaml", Some("infra")),
            ("src/database/migrations/1721200000000-Coding.ts", Some("migrations")),
            ("README.md", Some("docs")),
            ("docs/architecture.md", Some("docs")),
            ("src/components/button.tsx", Some("frontend")),
            ("src/main.ts", Some("backend")),
            ("LICENSE", None),
        ] {
            assert_eq!(classify_path(&classifier, path).as_deref(), expected, "{path}");
        }
    }

    #[test]
    fn a_spec_file_is_tests_even_though_it_is_also_a_ts_file() {
        assert_eq!(
            classify_path(&test_classifier(), "src/auth/auth.service.spec.ts").as_deref(),
            Some("tests")
        );
    }

    #[test]
    fn a_leading_double_star_also_matches_at_the_root() {
        assert!(glob_match("**/*.md", "README.md"));
        assert!(glob_match("**/*.md", "docs/deep/file.md"));
        assert!(!glob_match("**/*.md", "README.txt"));
    }

    #[test]
    fn a_single_star_does_not_cross_a_directory_boundary() {
        assert!(glob_match("src/*.ts", "src/main.ts"));
        assert!(!glob_match("src/*.ts", "src/deep/main.ts"));
        assert!(glob_match("src/**", "src/a/b/c.ts"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "a/c"));
    }

    #[test]
    fn a_path_outside_the_repository_classifies_as_nothing() {
        assert_eq!(
            to_repo_relative("/Users/someone/notes/auth-notes.md", Some("/repo")),
            ""
        );
        assert_eq!(classify_path(&test_classifier(), ""), None);
        assert_eq!(to_repo_relative("/repo/src/auth/x.ts", Some("/repo")), "src/auth/x.ts");
    }

    #[test]
    fn backslash_paths_classify_the_same_as_forward_slash_paths() {
        assert_eq!(
            classify_path(&test_classifier(), "src\\auth\\jwt.ts").as_deref(),
            Some("auth")
        );
    }
}
