import { extractToolFacts } from './extract.ts'
import type { RawPayload } from './extract.ts'
import { sweepTranscript } from './transcript.ts'
import { sweepPromptInsightTurns } from './prompt-insight.ts'
import { AGENT, beginSession, classifierFor, ensureSession } from './session.ts'
import { clearSession, readSession, withSessionLock, writeSession } from './store.ts'
import type { PendingEdit, SessionState } from './store.ts'
import { enqueue, flush } from './queue.ts'
import { currentToken, postInsight, postLiveFeedback } from './api.ts'
import { record } from './receipt.ts'
import type { CodingEvent, InsightSubmission } from './types.ts'

// Feature 0098. The opportunistic flush from `onPostToolUse` (below) is what
// makes the coding surface's live feed actually live rather than
// batched-until-Stop, but it also runs on the developer's critical path for
// EVERY tool call, unlike the full-budget flush at Stop/SessionEnd/
// SessionStart. A short, separate timeout means a slow or unreachable backend
// costs at most this much typing latency, once per tool call, never the full
// HOOK_TIMEOUT_MS: a failed live flush just leaves the event queued for the
// next natural flush point (see queue.ts's `flush()` for why nothing is lost).
const LIVE_FLUSH_TIMEOUT_MS = 400

// The four hooks. All of type "command", because extraction is local.
//
// A hook of type "http" would post whatever Claude Code hands it straight to a
// URL. That is the wrong shape for `/events`, so the batch would be dropped as
// malformed behind a 202 and nobody would see it fail, and it would put the raw
// tool_input and tool_response on the wire, which is the one thing this product
// promises never happens (eng findings 13 and 15, CEO decisions 8A and 33A).
//
// Every handler here returns without writing to stdout. Claude Code feeds a
// SessionStart hook's stdout into the model's context, so a client that printed
// its status would be injecting text into the developer's session. The receipt
// surfaces are CLI commands instead.

export interface HookOutcome {
  sent: number
  inert: boolean
  reason: string | null
}

export async function onSessionStart(payload: RawPayload): Promise<HookOutcome> {
  const sessionId = sessionIdOf(payload)
  const cwd = cwdOf(payload)
  const { state } = await beginSession({ sessionId, cwd })
  // Anything left from a previous session goes out now, which is the one moment
  // the developer is not waiting on a tool call.
  if (!state.inert) await flush()
  return { sent: 0, inert: state.inert, reason: state.inertReason }
}

export async function onPostToolUse(payload: RawPayload): Promise<HookOutcome> {
  const sessionId = sessionIdOf(payload)
  const cwd = cwdOf(payload)
  const state = await ensureSession(sessionId, cwd)
  if (state.inert) return { sent: 0, inert: true, reason: state.inertReason }

  const facts = extractToolFacts(payload, { repoRoot: state.repoRoot, classifier: classifierFor() })
  const events: CodingEvent[] = []

  withSessionLock(sessionId, () => {
    const live = readSession(sessionId) ?? state
    const id = facts.toolUseId ?? nextSeqId(live)
    if (live.seenToolUseIds.includes(id)) return

    live.seenToolUseIds.push(id)
    live.toolUses += 1
    if (facts.kind === 'subagent') live.subagents += 1
    if (facts.isTestCommand) live.testsRanThisTurn = true

    events.push({
      eventId: `${facts.kind === 'subagent' ? 'sa' : 'tu'}:${sessionId}:${id}`,
      kind: facts.kind,
      at: new Date().toISOString(),
      repoId: live.repoId,
      pathClass: facts.pathClass,
    })

    if (facts.isEdit) {
      if (facts.declined) {
        // The rare shape where PostToolUse fires on a decline. The usual case is
        // caught by the transcript sweep at Stop, and the shared eventId keeps
        // the two from counting the same edit twice.
        events.push({
          eventId: `ed:${sessionId}:${id}`,
          kind: 'edit-decision',
          at: new Date().toISOString(),
          repoId: live.repoId,
          pathClass: facts.pathClass,
          decision: 'rejected',
        })
      } else {
        // Held until Stop. `testsRun` is only knowable after the turn, and an
        // accept emitted now would report every accept as unchecked.
        const pending: PendingEdit = { toolUseId: id, at: new Date().toISOString(), pathClass: facts.pathClass }
        live.pendingEdits.push(pending)
      }
    }
    writeSession(live)
  })

  const queued = enqueue(AGENT, sessionId, events)
  record(queued, 1)
  // Feature 0098. Opportunistic, bounded, and never awaited past its own short
  // timeout: this is what a tool call shows up on the live feed within
  // seconds instead of only at the next Stop/SessionEnd. A slow or
  // unreachable backend leaves the event queued, not lost (see
  // LIVE_FLUSH_TIMEOUT_MS above).
  if (queued.length > 0) await flush({ timeoutMs: LIVE_FLUSH_TIMEOUT_MS })
  return { sent: queued.length, inert: false, reason: null }
}

export async function onStop(payload: RawPayload): Promise<HookOutcome> {
  const sessionId = sessionIdOf(payload)
  const cwd = cwdOf(payload)
  const transcript = stringField(payload, 'transcript_path', 'transcriptPath')
  const state = await ensureSession(sessionId, cwd)
  if (state.inert) return { sent: 0, inert: true, reason: state.inertReason }

  const events: CodingEvent[] = []
  // Feature 0094 / 0098. Built inside the lock (turnId material must not race
  // a concurrent Stop), sent outside it (a network call must never hold the
  // session file lock, same reason enqueue/flush already happen after this
  // block ends below). One sweep, two independent destinations: see feature
  // 0098's Decisions for why prompt insights and live feedback share the read
  // but not the opt-in.
  let turnSubmissions: InsightSubmission[] = []
  let sendToInsights = false
  let sendToLiveFeedback = false
  withSessionLock(sessionId, () => {
    const live = readSession(sessionId) ?? state
    events.push(...settleEdits(live))
    sendToInsights = live.promptInsightsEnabled
    sendToLiveFeedback = live.liveFeedbackEnabled

    if (transcript) {
      const sweep = sweepTranscript(transcript, {
        offset: live.transcriptOffset,
        repoRoot: live.repoRoot,
        classifier: classifierFor(),
      })
      live.transcriptOffset = sweep.offset
      for (const rejection of sweep.rejections) {
        const eventId = `ed:${sessionId}:${rejection.toolUseId}`
        if (live.seenToolUseIds.includes(eventId)) continue
        live.seenToolUseIds.push(eventId)
        events.push({
          eventId,
          kind: 'edit-decision',
          at: new Date().toISOString(),
          repoId: live.repoId,
          pathClass: rejection.pathClass,
          decision: 'rejected',
        })
      }

      // Only when at least one of the two effective policies is on. Everyone
      // else takes exactly the path above and this block never runs, never
      // reads a message body, never allocates a string that could hold one.
      if (sendToInsights || sendToLiveFeedback) {
        const insightSweep = sweepPromptInsightTurns(transcript, live.promptInsightLineOffset)
        live.promptInsightLineOffset = insightSweep.lineOffset
        turnSubmissions = insightSweep.turns.map((turn) => {
          live.promptInsightSeq += 1
          return {
            sessionId,
            turnId: `insight:${sessionId}:${live.promptInsightSeq}`,
            repoId: live.repoId,
            pathClass: null,
            prompt: turn.prompt,
            response: turn.response,
            at: new Date().toISOString(),
          }
        })
      }
    }
    writeSession(live)
  })

  const queued = enqueue(AGENT, sessionId, events)
  // A declined tool call is still a tool call this client looked at, and the
  // receipt's `observed` count is meant to be every one of them. It cannot come
  // from PostToolUse, which never fires for a decline.
  record(queued, queued.filter((event) => event.decision === 'rejected').length)
  await flush()
  await sendTurnSubmissions(turnSubmissions, sendToInsights, sendToLiveFeedback)
  return { sent: queued.length, inert: false, reason: null }
}

// Feature 0094 / 0098. Best effort, one attempt per destination, never queued
// to disk: `prompt` and `response` exist as values only for the span of this
// function. A failure here is a missing data point for this one turn, not a
// reason to hold real content anywhere longer than one request needs it (same
// no-retry shape as the backend's own coding-insights / coding-live-feedback
// queues). `toInsights` and `toLiveFeedback` are independent: a turn opted
// into both is sent to both, from the one extracted copy, never re-swept.
async function sendTurnSubmissions(
  submissions: InsightSubmission[],
  toInsights: boolean,
  toLiveFeedback: boolean,
): Promise<void> {
  if (submissions.length === 0 || (!toInsights && !toLiveFeedback)) return
  const creds = await currentToken(AGENT)
  if (!creds) return
  for (const submission of submissions) {
    if (toInsights) {
      try {
        await postInsight(creds.apiUrl, creds.accessToken, submission)
      } catch {
        // Fire and forget. Nothing about a failed scoring pass is worth a
        // retry loop holding this developer's words in memory any longer.
      }
    }
    if (toLiveFeedback) {
      try {
        await postLiveFeedback(creds.apiUrl, creds.accessToken, submission)
      } catch {
        // Same reasoning as above.
      }
    }
  }
}

export async function onSessionEnd(payload: RawPayload): Promise<HookOutcome> {
  const sessionId = sessionIdOf(payload)
  const cwd = cwdOf(payload)
  const state = await ensureSession(sessionId, cwd)
  if (state.inert) {
    clearSession(sessionId)
    return { sent: 0, inert: true, reason: state.inertReason }
  }

  const events: CodingEvent[] = []
  withSessionLock(sessionId, () => {
    const live = readSession(sessionId) ?? state
    events.push(...settleEdits(live))
    events.push({
      eventId: `se:${sessionId}`,
      kind: 'session-end',
      at: new Date().toISOString(),
      repoId: live.repoId,
      pathClass: null,
      subagentCount: live.subagents,
      durationMs: Math.max(0, Date.now() - live.startedAt),
    })
    writeSession(live)
  })

  const queued = enqueue(AGENT, sessionId, events)
  record(queued, 0)
  await flush()
  clearSession(sessionId)
  return { sent: queued.length, inert: false, reason: null }
}

// Turns the turn's buffered edits into decisions, now that whether tests ran is
// known. Mutates `live` in place; the caller holds the lock and writes it.
function settleEdits(live: SessionState): CodingEvent[] {
  const events = live.pendingEdits.map<CodingEvent>((edit) => ({
    eventId: `ed:${live.sessionId}:${edit.toolUseId}`,
    kind: 'edit-decision',
    at: edit.at,
    repoId: live.repoId,
    pathClass: edit.pathClass,
    // An edit that reached PostToolUse was applied, which is what "accepted"
    // means here. A decline never reaches PostToolUse at all, which is why the
    // rejection half comes from the transcript sweep.
    decision: 'accepted',
    testsRun: live.testsRanThisTurn,
  }))
  live.pendingEdits = []
  live.testsRanThisTurn = false
  return events
}

// CEO decision 23A makes the dedupe key `tool_use_id` or `prompt_id`. Claude Code
// does not put either on a PostToolUse payload in every version, so this is the
// fallback: a session-scoped sequence, which is stable across a queue replay,
// which is what dedupe actually protects against.
function nextSeqId(live: SessionState): string {
  live.eventSeq += 1
  return `seq-${live.eventSeq}`
}

function sessionIdOf(payload: RawPayload): string {
  return stringField(payload, 'session_id', 'sessionId') ?? `local-${Date.now()}`
}

function cwdOf(payload: RawPayload): string {
  return stringField(payload, 'cwd', 'workspaceRoot') ?? process.cwd()
}

function stringField(payload: RawPayload, ...keys: string[]): string | null {
  for (const key of keys) {
    const value = payload[key]
    if (typeof value === 'string' && value.length > 0) return value
  }
  return null
}
