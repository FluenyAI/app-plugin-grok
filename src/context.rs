// What every command and hook needs: where files live, where the credential
// lives, and which host this process is running under. Passed explicitly rather
// than read from globals, so tests run in parallel against their own temp dirs.

use crate::credentials::CredentialStore;
use crate::store::Store;
use crate::types::AgentId;

#[derive(Clone)]
pub struct Ctx {
    pub store: Store,
    pub creds: CredentialStore,
    pub agent: AgentId,
}

impl Ctx {
    pub fn from_env(agent_override: Option<&str>) -> Ctx {
        let store = Store::from_env();
        Ctx {
            creds: CredentialStore::from_env(store.clone()),
            store,
            agent: detect_agent(agent_override),
        }
    }

    pub fn with_agent(&self, agent: AgentId) -> Ctx {
        Ctx { agent, ..self.clone() }
    }
}

/// Grok injects GROK_* on every hook and on plugin-owned processes. Claude Code
/// never sets those, so they are a reliable host signal. An explicit --agent wins
/// because /flueny:connect runs as a command, not a hook, and may not inherit the
/// hook environment.
pub fn detect_agent(override_value: Option<&str>) -> AgentId {
    detect_agent_from(override_value, |key| {
        std::env::var_os(key).is_some_and(|v| !v.is_empty())
    })
}

pub fn detect_agent_from(override_value: Option<&str>, has_env: impl Fn(&str) -> bool) -> AgentId {
    if let Some(agent) = override_value.and_then(AgentId::parse) {
        return agent;
    }
    if ["GROK_PLUGIN_ROOT", "GROK_SESSION_ID", "GROK_HOOK_EVENT"]
        .iter()
        .any(|key| has_env(key))
    {
        return AgentId::GrokBuild;
    }
    AgentId::ClaudeCode
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(keys: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |key| keys.contains(&key)
    }

    #[test]
    fn defaults_to_claude_code() {
        assert_eq!(detect_agent_from(None, env(&[])), AgentId::ClaudeCode);
    }

    #[test]
    fn honours_an_explicit_override() {
        assert_eq!(
            detect_agent_from(Some("claude-code"), env(&["GROK_PLUGIN_ROOT"])),
            AgentId::ClaudeCode
        );
        assert_eq!(
            detect_agent_from(Some("grok-build"), env(&["GROK_PLUGIN_ROOT"])),
            AgentId::GrokBuild
        );
    }

    #[test]
    fn treats_grok_hook_environment_as_grok_build() {
        assert_eq!(detect_agent_from(None, env(&["GROK_HOOK_EVENT"])), AgentId::GrokBuild);
        assert_eq!(detect_agent_from(None, env(&["GROK_PLUGIN_ROOT"])), AgentId::GrokBuild);
    }

    #[test]
    fn ignores_an_unknown_override_and_falls_through_to_the_environment() {
        assert_eq!(
            detect_agent_from(Some("cursor"), env(&["GROK_SESSION_ID"])),
            AgentId::GrokBuild
        );
    }
}
