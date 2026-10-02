// Mirror of app-backend/src/integrations/coding/coding-repo-id.ts.
//
// This is a CONTRACT, not an implementation detail. This client hashes the remote
// of the repository it is working in and sends only the hash; the backend compares
// it against the org's allowlist. If the two sides normalize differently, every
// event from a correctly allowlisted repo is dropped and CEO decision 6A
// guarantees nobody sees an error. The steps below are the five written out under
// `## API contract` in app-docs/FEATURES/0028-coding-surface-m1.md, in order.
//
// The tests pin the same hash the backend pins, so a drift on either side fails a
// test rather than silently emptying somebody's dashboard.

use sha2::{Digest, Sha256};

pub const REPO_ID_PREFIX: &str = "sha256:";

fn strip_scheme(s: &str) -> Option<&str> {
    // ^[a-z][a-z0-9+.-]*://
    let at = s.find("://")?;
    let scheme = &s[..at];
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_lowercase() {
        return None;
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '.' | '-')) {
        return None;
    }
    Some(&s[at + 3..])
}

pub fn normalize_remote(remote: &str) -> String {
    let mut s = remote.trim().to_lowercase();
    if s.is_empty() {
        return String::new();
    }
    if let Some(rest) = s.strip_prefix("git+") {
        s = rest.to_string();
    }

    // scp-style `git@host:path` has no scheme and its colon is a separator, not a
    // port, so it has to be rewritten before the port strip below.
    s = if let Some(rest) = strip_scheme(&s) {
        strip_user(rest).to_string()
    } else if let Some(scp) = scp_rewrite(&s) {
        scp
    } else {
        strip_user(&s).to_string()
    };

    // host:port, as /^([^/]+):\d+/ does it: the last colon in the first segment
    // that is followed by a digit.
    let first_segment = s.find('/').unwrap_or(s.len());
    let bytes = s.as_bytes();
    let mut colon = first_segment;
    while colon > 0 {
        colon -= 1;
        if bytes[colon] == b':' && colon > 0 && bytes.get(colon + 1).is_some_and(u8::is_ascii_digit) {
            let mut end = colon + 1;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            s = format!("{}{}", &s[..colon], &s[end..]);
            break;
        }
    }

    // Trailing slashes come off BEFORE the .git suffix, because `.../repo.git/` is
    // a shape git itself accepts and `.git$` would not match through the slash.
    let mut s = s.trim_end_matches('/').to_string();
    if let Some(stripped) = s.strip_suffix(".git") {
        s = stripped.to_string();
    }
    let s = s.trim_end_matches('/');
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch == '/' && out.ends_with('/') {
            continue;
        }
        out.push(ch);
    }
    out
}

// /^([^/@]+@)?([^/:]+):(?!\/\/)(.+)$/
// /^[^/@]+@/: a leading `user@` or `user:token@`.
fn strip_user(s: &str) -> &str {
    match s.find(['/', '@']) {
        Some(i) if s.as_bytes()[i] == b'@' && i > 0 => &s[i + 1..],
        _ => s,
    }
}

fn scp_rewrite(s: &str) -> Option<String> {
    let rest = strip_user(s);
    let colon = rest.find(':')?;
    let host = &rest[..colon];
    if host.is_empty() || host.contains('/') {
        return None;
    }
    let path = &rest[colon + 1..];
    if path.is_empty() || path.starts_with("//") {
        return None;
    }
    Some(format!("{host}/{path}"))
}

pub fn repo_id_for(remote: &str) -> String {
    let normalized = normalize_remote(remote);
    if normalized.is_empty() {
        return String::new();
    }
    let digest = Sha256::digest(normalized.as_bytes());
    format!("{REPO_ID_PREFIX}{}", hex(&digest))
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The hash app-backend pins in coding-repo-id.spec.ts. Written out rather than
    // imported, because a shared import would only prove one side agrees with itself.
    const PINNED: &str = "sha256:97992c958c94ae63d8a4a35d948f6d5f1a49d93a158497f9bf401872ff45d2ae";

    #[test]
    fn every_spelling_of_one_remote_produces_the_pinned_id() {
        for remote in [
            "git@github.com:FluenyAI/app-backend.git",
            "https://github.com/fluenyai/app-backend",
            "ssh://git@github.com:22/FluenyAI/app-backend.git",
            "https://user:token@github.com/fluenyai/app-backend.git/",
            "git+https://github.com/FluenyAI/app-backend.git",
            "  https://github.com/FluenyAI/app-backend/  ",
        ] {
            assert_eq!(normalize_remote(remote), "github.com/fluenyai/app-backend", "{remote}");
            assert_eq!(repo_id_for(remote), PINNED, "{remote}");
        }
    }

    #[test]
    fn a_remote_that_normalizes_to_nothing_produces_no_id() {
        assert_eq!(repo_id_for(""), "");
        assert_eq!(repo_id_for("   "), "");
    }

    #[test]
    fn different_repositories_do_not_collide() {
        assert_ne!(repo_id_for("git@github.com:FluenyAI/app-frontend.git"), PINNED);
    }

    #[test]
    fn host_aliases_and_doubled_slashes() {
        assert_eq!(
            normalize_remote("git@github.com-fluenyai:FluenyAI/app-backend.git"),
            "github.com-fluenyai/fluenyai/app-backend"
        );
        assert_eq!(
            normalize_remote("https://gitlab.com//group//repo.git"),
            "gitlab.com/group/repo"
        );
        assert_eq!(
            normalize_remote("ssh://gitlab.example.com:2222/a/b"),
            "gitlab.example.com/a/b"
        );
    }
}
