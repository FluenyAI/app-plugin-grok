// The hooks block for an install without the plugin (`flueny install`).
//
// Every hook is `type: "command"`. This is not a preference: a hook of type "http"
// posts the raw hook payload to a URL with no local code in between, which would
// put `tool_input` and `tool_response` off the machine.

use serde_json::{Value, json};

// A hook that hangs is an editor that hangs. The client's own network calls are
// bounded tighter still, so this timeout is the backstop, not the mechanism.
const TIMEOUT_SECONDS: u64 = 10;

pub const HOOK_EVENTS: [(&str, &str); 5] = [
    ("SessionStart", "session-start"),
    ("PostToolUse", "post-tool-use"),
    // Feature 0126: Claude Code reports a Bash call that exited non-zero here and
    // not in PostToolUse, so without it a failing test run is invisible.
    ("PostToolUseFailure", "post-tool-use-failure"),
    ("Stop", "stop"),
    ("SessionEnd", "session-end"),
];

pub fn settings_fragment(binary: &str) -> Value {
    let mut hooks = serde_json::Map::new();
    for (event, arg) in HOOK_EVENTS {
        let command = json!({
            "type": "command",
            "command": format!("{} hook {arg}", quote(binary)),
            "timeout": TIMEOUT_SECONDS,
        });
        // Every tool, not just the editing ones: Delegation counts Task calls and
        // Diligence needs to see a test command run.
        let matcher = if event.starts_with("PostToolUse") {
            json!({ "matcher": "*", "hooks": [command] })
        } else {
            json!({ "hooks": [command] })
        };
        hooks.insert(event.to_string(), json!([matcher]));
    }
    json!({ "hooks": hooks })
}

// Paths on a developer's machine contain spaces far more often than anyone
// designing a shell command line expects, and this repository lives in one.
fn quote(value: &str) -> String {
    let plain = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if plain {
        return value.to_string();
    }
    let mut out = String::from("\"");
    for c in value.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commands(fragment: &Value) -> Vec<&Value> {
        fragment["hooks"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|m| {
                m.as_array()
                    .unwrap()
                    .iter()
                    .flat_map(|g| g["hooks"].as_array().unwrap().iter())
            })
            .collect()
    }

    #[test]
    fn every_hook_is_type_command_never_type_http() {
        let fragment = settings_fragment("/opt/flueny/bin/flueny");
        let all = commands(&fragment);
        assert_eq!(all.len(), 5);
        for command in all {
            assert_eq!(command["type"], "command");
            assert!(
                command["timeout"].as_u64().unwrap() > 0,
                "an unbounded hook is an editor that can hang"
            );
        }
        assert!(!fragment.to_string().contains("\"http\""));
    }

    #[test]
    fn the_m1_hooks_plus_the_failure_hook_are_registered_and_no_gate_is() {
        let fragment = settings_fragment("/opt/flueny/bin/flueny");
        let mut keys: Vec<&String> = fragment["hooks"].as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "PostToolUse",
                "PostToolUseFailure",
                "SessionEnd",
                "SessionStart",
                "Stop"
            ]
        );
        // PreToolUse is the gate: M3, and canEnforce is false for every agent.
        assert!(!fragment.to_string().contains("PreToolUse"));
    }

    #[test]
    fn post_tool_use_matches_every_tool() {
        let fragment = settings_fragment("/opt/flueny/bin/flueny");
        assert_eq!(fragment["hooks"]["PostToolUse"][0]["matcher"], "*");
        assert_eq!(fragment["hooks"]["PostToolUseFailure"][0]["matcher"], "*");
    }

    #[test]
    fn a_path_with_a_space_survives_the_shell() {
        let fragment = settings_fragment("/Users/x/Flueny AI/plugin/bin/flueny");
        assert_eq!(
            fragment["hooks"]["SessionStart"][0]["hooks"][0]["command"],
            "\"/Users/x/Flueny AI/plugin/bin/flueny\" hook session-start"
        );
    }

    #[test]
    fn each_hook_is_told_which_event_it_is_handling() {
        let fragment = settings_fragment("/opt/flueny/bin/flueny");
        let mut events: Vec<&str> = commands(&fragment)
            .iter()
            .map(|c| c["command"].as_str().unwrap().rsplit(' ').next().unwrap())
            .collect();
        events.sort();
        assert_eq!(
            events,
            [
                "post-tool-use",
                "post-tool-use-failure",
                "session-end",
                "session-start",
                "stop"
            ]
        );
    }

    #[test]
    fn the_plugin_hooks_json_registers_the_same_events_through_the_wrapper() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/hooks/hooks.json")).unwrap();
        let hooks: Value = serde_json::from_str(&text).unwrap();
        for (event, arg) in HOOK_EVENTS {
            let command = hooks["hooks"][event][0]["hooks"][0]["command"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                command,
                format!("sh \"${{CLAUDE_PLUGIN_ROOT}}/hooks/flueny-hook.sh\" {arg}"),
                "{event}"
            );
            assert_eq!(hooks["hooks"][event][0]["hooks"][0]["type"], "command");
        }
    }
}

#[cfg(test)]
mod version_tests {
    // Every version field moves together. The binary sends CARGO_PKG_VERSION as
    // `clientVersion` at handshake, so a manifest that says otherwise describes a
    // client nobody is running.
    #[test]
    fn every_manifest_version_matches_the_binary() {
        let root = env!("CARGO_MANIFEST_DIR");
        for file in [
            ".claude-plugin/plugin.json",
            ".grok-plugin/plugin.json",
            ".claude-plugin/marketplace.json",
            ".grok-plugin/marketplace.json",
        ] {
            let text = std::fs::read_to_string(format!("{root}/{file}")).unwrap();
            let value: serde_json::Value = serde_json::from_str(&text).unwrap();
            let version = value
                .get("version")
                .or_else(|| value["plugins"][0].get("version"))
                .and_then(serde_json::Value::as_str);
            assert_eq!(version, Some(env!("CARGO_PKG_VERSION")), "{file}");
        }
        assert_eq!(crate::api::CLIENT_VERSION, "0.2.1");
    }
}
