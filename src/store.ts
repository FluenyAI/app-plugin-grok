import {
  appendFileSync,
  chmodSync,
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { homedir } from 'node:os'
import { join } from 'node:path'
import type { AgentId, CodingEvent, PolicyBundle } from './types.ts'

// Everything this client persists, in one place, so "what is on disk" is a
// question with one answer.
//
// SPIKE NOTE: the credential is a mode 0600 file here. The shipped client puts it
// in the OS keychain (feature 0028, `## API contract`, "Errors, env vars,
// cookies"). A 0600 file is readable by any process running as the developer,
// which is a materially weaker promise than the Keychain prompt, and it is
// acceptable only because this spike runs on one machine to prove the protocol.

export interface Credentials {
  apiUrl: string
  // Where the Flueny app lives, which is NOT the API origin. Captured at login
  // from the device grant's `verification_uri`, because that is the only place
  // the backend tells the client its own front end. Without it `flueny status`
  // built links off the API origin and printed a URL that 404s. Optional so a
  // credential written by an older client still loads.
  appUrl?: string
  clientId: string
  accessToken: string
  refreshToken: string
  // Epoch millis. Refreshed proactively, because a 401 mid-session costs a round
  // trip on a hook that is already blocking the developer's tool call.
  expiresAt: number
  // Which host this token was minted for. Required on new writes. Missing on a
  // file written before per-agent credentials, which migrate via the JWT claim.
  agent?: AgentId
}

export interface SessionState {
  sessionId: string
  agent: AgentId
  startedAt: number
  // Inert covers every reason to send nothing: kill switch, a repo that is not
  // on the allowlist, no credential. The reason is kept for `flueny status`,
  // because "Flueny is doing nothing" with no explanation is the failure mode
  // the allowlist decision in the feature file is written against.
  inert: boolean
  inertReason: string | null
  killSwitch: boolean
  dryRun: boolean
  dryRunEndsAt: string | null
  repoId: string | null
  repoRoot: string | null
  toolUses: number
  subagents: number
  // Buffered edit decisions. They are held until Stop because `testsRun` is only
  // knowable after the turn: an accept followed by a test run is a different
  // fact from an accept that was never checked, and emitting at PostToolUse
  // would make every accept look unchecked.
  pendingEdits: PendingEdit[]
  testsRanThisTurn: boolean
  seenToolUseIds: string[]
  // Byte offset the transcript sweep has already read. Rejections are the one
  // signal PostToolUse cannot see, and re-reading from zero every turn would
  // re-report every rejection in the session.
  transcriptOffset: number
  // Only ever incremented, so a synthesized event id is unique within a session
  // when Claude Code hands the hook no tool_use_id of its own.
  eventSeq: number
}

export interface PendingEdit {
  toolUseId: string
  at: string
  pathClass: string | null
}

export interface LedgerCounters {
  observed: number
  wouldSend: number
  wouldBlock: number
}

export interface LedgerEntry {
  at: string
  summary: string
  fieldsSent: string[]
  wouldBlock: false
}

const MAX_SEEN_TOOL_USE_IDS = 2000

export function configDir(): string {
  const override = process.env.FLUENY_CONFIG_DIR
  if (override) return override
  const xdg = process.env.XDG_CONFIG_HOME
  return xdg ? join(xdg, 'flueny') : join(homedir(), '.config', 'flueny')
}

function ensureDir(path: string): string {
  mkdirSync(path, { recursive: true, mode: 0o700 })
  return path
}

// Written through a temp file so a hook killed mid-write leaves the previous
// state rather than a truncated JSON file that every later hook fails to parse.
function writePrivate(path: string, body: string): void {
  const tmp = `${path}.${process.pid}.tmp`
  writeFileSync(tmp, body, { mode: 0o600 })
  renameSync(tmp, path)
  chmodSync(path, 0o600)
}

function readJson<T>(path: string): T | null {
  try {
    return JSON.parse(readFileSync(path, 'utf8')) as T
  } catch {
    return null
  }
}

// ---- credentials ----
//
// One file per agent. Claude Code and Grok share a machine and a repository,
// and ingest attributes a batch to the hook token's agent, so a single
// credentials.json made the last login steal the other host's events.

const KNOWN_AGENTS: AgentId[] = ['claude-code', 'grok-build']

function credentialsPath(agent: AgentId): string {
  return join(ensureDir(configDir()), `credentials.${agent}.json`)
}

function legacyCredentialsPath(): string {
  return join(ensureDir(configDir()), 'credentials.json')
}

export function inferAgentFromToken(accessToken: string): AgentId | null {
  try {
    const parts = accessToken.split('.')
    if (parts.length < 2 || !parts[1]) return null
    const payload = JSON.parse(Buffer.from(parts[1], 'base64url').toString('utf8')) as {
      agent?: unknown
    }
    if (payload.agent === 'claude-code' || payload.agent === 'grok-build') return payload.agent
    return null
  } catch {
    return null
  }
}

function migrateLegacyCredentials(): void {
  const legacy = readJson<Credentials>(legacyCredentialsPath())
  if (!legacy) return
  const agent = legacy.agent ?? inferAgentFromToken(legacy.accessToken) ?? 'claude-code'
  const dest = credentialsPath(agent)
  if (!existsSync(dest)) {
    writePrivate(dest, JSON.stringify({ ...legacy, agent }, null, 2))
  }
  rmSync(legacyCredentialsPath(), { force: true })
}

export function readCredentials(agent: AgentId = 'claude-code'): Credentials | null {
  migrateLegacyCredentials()
  return readJson<Credentials>(credentialsPath(agent))
}

export function writeCredentials(creds: Credentials, agent: AgentId = creds.agent ?? 'claude-code'): void {
  migrateLegacyCredentials()
  writePrivate(credentialsPath(agent), JSON.stringify({ ...creds, agent }, null, 2))
}

export function clearCredentials(agent: AgentId = 'claude-code'): void {
  migrateLegacyCredentials()
  rmSync(credentialsPath(agent), { force: true })
}

export function listCredentialAgents(): AgentId[] {
  migrateLegacyCredentials()
  return KNOWN_AGENTS.filter((agent) => existsSync(credentialsPath(agent)))
}

// ---- policy bundle cache (CEO decision 25A) ----

function bundlePath(): string {
  return join(ensureDir(configDir()), 'bundle.json')
}

export function readBundle(): PolicyBundle | null {
  return readJson<PolicyBundle>(bundlePath())
}

export function writeBundle(bundle: PolicyBundle): void {
  writePrivate(bundlePath(), JSON.stringify(bundle))
}

// ---- per-session state ----

function sessionPath(sessionId: string): string {
  return join(ensureDir(join(configDir(), 'sessions')), `${safe(sessionId)}.json`)
}

export function readSession(sessionId: string): SessionState | null {
  return readJson<SessionState>(sessionPath(sessionId))
}

export function writeSession(state: SessionState): void {
  if (state.seenToolUseIds.length > MAX_SEEN_TOOL_USE_IDS) {
    state.seenToolUseIds = state.seenToolUseIds.slice(-MAX_SEEN_TOOL_USE_IDS)
  }
  writePrivate(sessionPath(state.sessionId), JSON.stringify(state))
}

export function clearSession(sessionId: string): void {
  rmSync(sessionPath(sessionId), { force: true })
}

// Claude Code runs PostToolUse concurrently when the model calls several tools
// at once, and the session file is read-modify-write. mkdir is atomic on every
// filesystem this runs on, so it is the lock. A stale lock expires rather than
// wedging every later hook, because a wedged hook is a wedged editor.
export function withSessionLock<T>(sessionId: string, fn: () => T): T {
  const lock = `${sessionPath(sessionId)}.lock`
  const deadline = Date.now() + 2000
  for (;;) {
    try {
      mkdirSync(lock)
      break
    } catch {
      if (Date.now() > deadline) {
        rmSync(lock, { recursive: true, force: true })
        continue
      }
      sleep(15)
    }
  }
  try {
    return fn()
  } finally {
    rmSync(lock, { recursive: true, force: true })
  }
}

function sleep(ms: number): void {
  // Synchronous on purpose: a hook is a short-lived process and an async lock
  // would mean restructuring every caller around a promise for 15ms.
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms)
}

// ---- outbound queue ----

export function queuePath(): string {
  return join(ensureDir(configDir()), 'queue.jsonl')
}

// Append-only JSONL. `appendFileSync` on a line under the pipe buffer is atomic
// enough for concurrent hooks, which a read-modify-write JSON array would not be.
export function appendQueue(agent: AgentId, sessionId: string, events: CodingEvent[]): void {
  if (events.length === 0) return
  const lines = events.map((event) => JSON.stringify({ agent, sessionId, event })) as string[]
  appendFileSync(queuePath(), `${lines.join('\n')}\n`, { mode: 0o600 })
}

export interface QueuedRecord {
  agent: AgentId
  sessionId: string
  event: CodingEvent
}

export function readQueue(): QueuedRecord[] {
  if (!existsSync(queuePath())) return []
  const out: QueuedRecord[] = []
  for (const line of readFileSync(queuePath(), 'utf8').split('\n')) {
    if (!line.trim()) continue
    try {
      out.push(JSON.parse(line) as QueuedRecord)
    } catch {
      // A torn final line from a killed process. Dropping it loses one event and
      // keeps the queue readable, which is the right way round.
    }
  }
  return out
}

export function replaceQueue(records: QueuedRecord[]): void {
  writePrivate(queuePath(), records.map((r) => JSON.stringify(r)).join('\n') + (records.length ? '\n' : ''))
}

// ---- local receipt ledger (design decision 44) ----

export function ledgerDir(): string {
  return ensureDir(join(configDir(), 'ledger'))
}

export function today(now = new Date()): string {
  return now.toISOString().slice(0, 10)
}

export function appendLedger(day: string, entries: LedgerEntry[]): void {
  if (entries.length === 0) return
  const lines = entries.map((e) => JSON.stringify(e)) as string[]
  appendFileSync(join(ledgerDir(), `${day}.jsonl`), `${lines.join('\n')}\n`, { mode: 0o600 })
}

export function readLedger(day: string): LedgerEntry[] {
  const path = join(ledgerDir(), `${day}.jsonl`)
  if (!existsSync(path)) return []
  const out: LedgerEntry[] = []
  for (const line of readFileSync(path, 'utf8').split('\n')) {
    if (!line.trim()) continue
    try {
      out.push(JSON.parse(line) as LedgerEntry)
    } catch {
      // Same reasoning as the queue: a torn line is one lost row, not a dead file.
    }
  }
  return out
}

// Counters are separate from the entry log because `observed` counts raw tool
// calls the client looked at, and the whole point is that most of them produce
// no entry at all. A ledger with 6 rows and an observed count of 41 is the
// privacy claim stated as arithmetic.
export function bumpCounters(day: string, delta: Partial<LedgerCounters>): LedgerCounters {
  const path = join(ledgerDir(), `${day}.counters.json`)
  const current = readJson<LedgerCounters>(path) ?? { observed: 0, wouldSend: 0, wouldBlock: 0 }
  const next: LedgerCounters = {
    observed: current.observed + (delta.observed ?? 0),
    wouldSend: current.wouldSend + (delta.wouldSend ?? 0),
    wouldBlock: current.wouldBlock + (delta.wouldBlock ?? 0),
  }
  writePrivate(path, JSON.stringify(next))
  return next
}

export function readCounters(day: string): LedgerCounters {
  return readJson<LedgerCounters>(join(ledgerDir(), `${day}.counters.json`)) ?? {
    observed: 0,
    wouldSend: 0,
    wouldBlock: 0,
  }
}

// The receipt is printed once a day. Which day it was last printed for lives
// here rather than in a session file, because a developer runs several sessions.
export function readReceiptDay(): string | null {
  return readJson<{ day: string }>(join(ensureDir(configDir()), 'receipt.json'))?.day ?? null
}

export function writeReceiptDay(day: string): void {
  writePrivate(join(ensureDir(configDir()), 'receipt.json'), JSON.stringify({ day }))
}

function safe(value: string): string {
  return value.replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, 100)
}
