// The wire contract, mirrored from app-backend/src/integrations/coding/coding.types.ts
// and from `## API contract` in app-docs/FEATURES/0028-coding-surface-m1.md and
// 0126-companion-engineering-signal.md.
//
// The load-bearing constraint on this file: there is no field here that can carry
// prompt text, code or file contents. Extraction happens on this machine and the
// backend receives derived signal only (CEO decisions 8A and 33A). If a field
// ever needs adding here, it is a cross-repo change: the feature file first.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentId {
    #[serde(rename = "claude-code")]
    ClaudeCode,
    #[serde(rename = "grok-build")]
    GrokBuild,
}

impl AgentId {
    pub fn as_str(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "claude-code",
            AgentId::GrokBuild => "grok-build",
        }
    }

    pub fn parse(value: &str) -> Option<AgentId> {
        match value {
            "claude-code" => Some(AgentId::ClaudeCode),
            "grok-build" => Some(AgentId::GrokBuild),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentId::ClaudeCode => "Claude Code",
            AgentId::GrokBuild => "Grok",
        }
    }
}

pub const KNOWN_AGENTS: [AgentId; 2] = [AgentId::ClaudeCode, AgentId::GrokBuild];

pub const EVENT_KINDS: [&str; 6] = [
    "tool-use",
    "edit-decision",
    "session-end",
    "subagent",
    // Feature 0126. Additive: an older backend drops an unknown kind per event
    // in coding-event-parse.ts and keeps the rest of the batch.
    "test-run",
    "turn-verification",
];

pub const TEST_OUTCOMES: [&str; 4] = ["passed", "failed", "error", "unknown"];

pub const TEST_RUNNERS: [&str; 14] = [
    "jest",
    "vitest",
    "mocha",
    "node",
    "pytest",
    "go",
    "cargo",
    "rspec",
    "phpunit",
    "dotnet",
    "jvm",
    "playwright",
    "cypress",
    "other",
];

pub const WEAKENED_FLAGS: [&str; 8] = [
    "skip-added",
    "only-added",
    "assertion-removed",
    "test-deleted",
    "snapshot-updated",
    "type-suppression",
    "lint-suppression",
    "no-verify",
];

/// One derived event. Every field is optional except the first five, and none of
/// them can hold free text: strings are ids, enums or a short class label.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodingEvent {
    pub event_id: String,
    pub kind: String,
    pub at: String,
    pub repo_id: Option<String>,
    pub path_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subagent_count: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_category: Option<String>,
    // Feature 0126, kind 'test-run'.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests_skipped: Option<i64>,
    // Feature 0126, kind 'turn-verification'.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits_accepted: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_files_changed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_files_changed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_first: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_runs: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ended_green: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edits_after_last_green: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failing_run_streak_max: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rework_files: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sensitive_untested: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weakened_flags: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weakened_caught: Option<i64>,
}

impl CodingEvent {
    pub fn new(event_id: String, kind: &str, at: String, repo_id: Option<String>, path_class: Option<String>) -> Self {
        CodingEvent {
            event_id,
            kind: kind.to_string(),
            at,
            repo_id,
            path_class,
            ..CodingEvent::default()
        }
    }

    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }
}

// The whitelist the wire serializer is built from. Nothing outside this list can
// reach a request body, and the redaction tests are what hold that true.
pub const CODING_EVENT_FIELDS: [&str; 28] = [
    "eventId",
    "kind",
    "at",
    "repoId",
    "pathClass",
    "decision",
    "testsRun",
    "subagentCount",
    "durationMs",
    "toolCategory",
    "commandCategory",
    "testOutcome",
    "testRunner",
    "testsPassed",
    "testsFailed",
    "testsSkipped",
    "editsAccepted",
    "sourceFilesChanged",
    "testFilesChanged",
    "testFirst",
    "testRuns",
    "endedGreen",
    "editsAfterLastGreen",
    "failingRunStreakMax",
    "reworkFiles",
    "sensitiveUntested",
    "weakenedFlags",
    "weakenedCaught",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PolicyBundle {
    pub etag: String,
    pub schema_version: i64,
    // pathClass -> glob patterns. First match wins, so JSON insertion order is the
    // contract, which is why serde_json runs with preserve_order.
    pub path_classifier: serde_json::Map<String, serde_json::Value>,
    pub rules: Vec<serde_json::Value>,
}

/// The handshake answer. Every opt-in flag is read fail closed: absent or
/// anything but literally true means off.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionStartResponse {
    pub kill_switch: bool,
    pub dry_run: bool,
    pub dry_run_ends_at: Option<String>,
    pub repo_allowlist: Vec<String>,
    pub bundle: Option<PolicyBundle>,
    pub prompt_insights_enabled: bool,
    pub live_feedback_enabled: bool,
    pub raw_activity_enabled: bool,
    pub weekly_limit_reporting_enabled: bool,
}

impl SessionStartResponse {
    pub fn from_value(value: &serde_json::Value) -> Option<SessionStartResponse> {
        let obj = value.as_object()?;
        let flag = |key: &str| obj.get(key).and_then(|v| v.as_bool()) == Some(true);
        Some(SessionStartResponse {
            kill_switch: flag("killSwitch"),
            dry_run: flag("dryRun"),
            dry_run_ends_at: obj.get("dryRunEndsAt").and_then(|v| v.as_str()).map(str::to_string),
            repo_allowlist: obj
                .get("repoAllowlist")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default(),
            bundle: obj
                .get("bundle")
                .filter(|v| v.is_object())
                .and_then(|v| serde_json::from_value::<PolicyBundle>(v.clone()).ok()),
            prompt_insights_enabled: flag("promptInsightsEnabled"),
            live_feedback_enabled: flag("liveFeedbackEnabled"),
            raw_activity_enabled: flag("rawActivityEnabled"),
            weekly_limit_reporting_enabled: flag("weeklyLimitReportingEnabled"),
        })
    }
}

// Feature 0109. One tool call's worth of coaching context. toolCategory and
// pathClass are bounded; rawPath and rawCommand exist only under the
// raw-activity opt-in.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolActivityEntry {
    pub tool_category: String,
    pub path_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_command: Option<String>,
}

// Feature 0094. The only shape in this client allowed to carry prompt or
// response text, and only once the handshake said promptInsightsEnabled. Not
// part of CODING_EVENT_FIELDS: a deliberately separate type, so the redaction
// guarantee on the event pipeline is unaffected by this one existing at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InsightSubmission {
    pub session_id: String,
    pub turn_id: String,
    pub repo_id: Option<String>,
    pub path_class: Option<String>,
    pub prompt: String,
    pub response: String,
    pub at: String,
}

// Feature 0098. Same shape plus 0109's tool activity, on its own opt-in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveFeedbackSubmission {
    pub session_id: String,
    pub turn_id: String,
    pub repo_id: Option<String>,
    pub path_class: Option<String>,
    pub prompt: String,
    pub response: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_activity: Option<Vec<ToolActivityEntry>>,
}

// Feature 0109. Its own DTO and its own endpoint, never folded into the event
// pipeline, which feeds every rollup the backend computes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawActivityDetail {
    pub session_id: String,
    pub event_id: String,
    pub at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_command: Option<String>,
}
