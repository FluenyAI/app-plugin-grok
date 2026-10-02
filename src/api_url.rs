// Which Flueny a machine reports to.
//
// Staging and production are separate installs holding separate signal: a
// machine connected to one is invisible in the other, and a credential minted by
// one means nothing to the other. So the target is worth naming rather than
// remembering as a hostname, and worth resolving strictly, because pointing a
// client at the wrong host sends one company's derived signal to another
// company's server.

pub const API_TARGETS: [(&str, &str); 2] = [
    ("staging", "https://api.flueny.dev"),
    ("production", "https://api.flueny.ai"),
];

/// A known name, or a full http(s) URL, or None. Fails closed: a bare hostname
/// is refused rather than given a scheme, because a typo of the real host would
/// otherwise resolve to something that looks plausible and is not.
pub fn resolve_api_target(input: &str) -> Option<String> {
    let raw = input.trim();
    let lower = raw.to_ascii_lowercase();
    if let Some((_, url)) = API_TARGETS.iter().find(|(name, _)| *name == lower) {
        return Some(url.to_string());
    }
    let rest = raw.strip_prefix("https://").or_else(|| raw.strip_prefix("http://"))?;
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return None;
    }
    Some(raw.trim_end_matches('/').to_string())
}

/// `staging (https://api.flueny.dev)` for a known URL, the URL itself otherwise.
pub fn describe_api(url: &str) -> String {
    match API_TARGETS.iter().find(|(_, known)| *known == url) {
        Some((name, _)) => format!("{name} ({url})"),
        None => url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_environments_are_reachable_by_name_however_typed() {
        assert_eq!(resolve_api_target("staging").as_deref(), Some("https://api.flueny.dev"));
        assert_eq!(
            resolve_api_target("production").as_deref(),
            Some("https://api.flueny.ai")
        );
        assert_eq!(
            resolve_api_target("  Production ").as_deref(),
            Some("https://api.flueny.ai")
        );
    }

    #[test]
    fn a_full_url_is_accepted() {
        assert_eq!(
            resolve_api_target("http://localhost:3011").as_deref(),
            Some("http://localhost:3011")
        );
        assert_eq!(
            resolve_api_target("https://flueny.acme.internal/").as_deref(),
            Some("https://flueny.acme.internal")
        );
    }

    #[test]
    fn anything_else_is_refused_rather_than_guessed_at() {
        assert_eq!(resolve_api_target("api.flueny.ai"), None);
        assert_eq!(resolve_api_target("prod"), None);
        assert_eq!(resolve_api_target(""), None);
        assert_eq!(resolve_api_target("ftp://api.flueny.ai"), None);
    }

    #[test]
    fn a_known_url_is_described_by_name() {
        assert_eq!(
            describe_api("https://api.flueny.dev"),
            "staging (https://api.flueny.dev)"
        );
        assert_eq!(describe_api("http://localhost:3011"), "http://localhost:3011");
    }
}
