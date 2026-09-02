// The wire contract, mirrored from app-backend/src/integrations/coding/coding.types.ts
// and from `## API contract` in app-docs/FEATURES/0028-coding-surface-m1.md.
//
// The load-bearing constraint on this file: there is no field here that can carry
// prompt text, code or file contents. Extraction happens on this machine and the
// backend receives derived signal only (CEO decisions 8A and 33A). If a field
// ever needs adding here, it is a cross-repo change: the feature file first.

export type AgentId = 'claude-code' | 'grok-build' | 'cursor' | 'windsurf' | 'cline' | 'copilot'

export type CodingEventKind = 'tool-use' | 'edit-decision' | 'session-end' | 'subagent'

export type EditDecision = 'accepted' | 'rejected' | 'reverted'

export interface CodingEvent {
  eventId: string
  kind: CodingEventKind
  at: string
  repoId: string | null
  pathClass: string | null
  decision?: EditDecision
  testsRun?: boolean
  subagentCount?: number
  durationMs?: number
}

export interface CodingEventBatch {
  agent: AgentId
  sessionId: string
  events: CodingEvent[]
}

// The whitelist the wire serializer is built from. Nothing outside this list can
// reach a request body, and test/redaction.test.ts is what holds that true.
export const CODING_EVENT_FIELDS = [
  'eventId',
  'kind',
  'at',
  'repoId',
  'pathClass',
  'decision',
  'testsRun',
  'subagentCount',
  'durationMs',
] as const

export interface AgentCapabilities {
  agent: AgentId
  canStreamEvents: boolean
  canInjectContext: boolean
  canEnforce: boolean
}

export interface PolicyBundle {
  etag: string
  schemaVersion: number
  // pathClass -> glob patterns. First match wins, so JSON insertion order is the
  // contract and this must stay an ordered object, never a Map rebuilt by key.
  pathClassifier: Record<string, string[]>
  rules: never[]
}

export interface SessionStartRequest {
  agent: AgentId
  sessionId: string
  clientVersion: string
  bundleEtag: string | null
  // Design decision 57's second list, client-declared. The server can enforce
  // `neverSent`; it can only believe this one, which is why the client says it
  // and the server echoes it attributed to this agent and clientVersion.
  readsLocally: string[]
}

export interface SessionStartResponse {
  killSwitch: boolean
  capabilities: AgentCapabilities
  dryRun: boolean
  dryRunEndsAt: string | null
  repoAllowlist: string[]
  bundle: PolicyBundle | null
  intervention: string | null
  // Feature 0094. This developer's resolved prompt-insights policy, server
  // authoritative. Optional so a client talking to an older backend still
  // works: fail closed on absence, exactly like every other missing-field
  // case in this client. Never read as anything but false unless present and
  // literally true.
  promptInsightsEnabled?: boolean
  // Feature 0098. Same fail-closed rule as promptInsightsEnabled above, for
  // the separate live-feedback opt-in (see feature 0098's Decisions for why
  // these are two flags, not one).
  liveFeedbackEnabled?: boolean
}

// Feature 0094. The only shape in this client allowed to carry prompt or
// response text, and only once SessionState.promptInsightsEnabled is true.
// Mirrors app-backend's InsightSubmissionDto exactly; not part of
// CODING_EVENT_FIELDS or CodingEvent, a deliberately separate type so the
// redaction guarantee on the event pipeline is unaffected by this one
// existing at all.
export interface InsightSubmission {
  sessionId: string
  turnId: string
  repoId: string | null
  pathClass: string | null
  prompt: string
  response: string
  at: string
}

// Feature 0098. The only other shape in this client allowed to carry prompt or
// response text, alongside InsightSubmission above, and only once
// SessionState.liveFeedbackEnabled is true. Mirrors app-backend's
// LiveFeedbackSubmissionDto exactly. Same shape as InsightSubmission today,
// kept as a separate type anyway: the two opt-ins are independent, and a
// future divergence in what either submission carries must not require
// threading a new field through both call sites by accident.
export interface LiveFeedbackSubmission {
  sessionId: string
  turnId: string
  repoId: string | null
  pathClass: string | null
  prompt: string
  response: string
  at: string
}

export interface DeviceCodeGrant {
  device_code: string
  user_code: string
  verification_uri: string
  verification_uri_complete: string
  expires_in: number
  interval: number
}

export interface TokenResponse {
  access_token: string
  token_type: 'Bearer'
  expires_in: number
  refresh_token: string
  scope: string
}
