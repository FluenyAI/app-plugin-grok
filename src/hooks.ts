import { extractToolFacts } from './extract.ts'
import type { RawPayload } from './extract.ts'
import { sweepTranscript } from './transcript.ts'
import { AGENT, beginSession, classifierFor, ensureSession } from './session.ts'
import { clearSession, readSession, withSessionLock, writeSession } from './store.ts'
import type { PendingEdit, SessionState } from './store.ts'
import { enqueue, flush } from './queue.ts'
import { record } from './receipt.ts'
import type { CodingEvent } from './types.ts'

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
  return { sent: queued.length, inert: false, reason: null }
}

export async function onStop(payload: RawPayload): Promise<HookOutcome> {
  const sessionId = sessionIdOf(payload)
  const cwd = cwdOf(payload)
  const transcript = stringField(payload, 'transcript_path', 'transcriptPath')
  const state = await ensureSession(sessionId, cwd)
  if (state.inert) return { sent: 0, inert: true, reason: state.inertReason }

  const events: CodingEvent[] = []
  withSessionLock(sessionId, () => {
    const live = readSession(sessionId) ?? state
    events.push(...settleEdits(live))

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
    }
    writeSession(live)
  })

  const queued = enqueue(AGENT, sessionId, events)
  // A declined tool call is still a tool call this client looked at, and the
  // receipt's `observed` count is meant to be every one of them. It cannot come
  // from PostToolUse, which never fires for a decline.
  record(queued, queued.filter((event) => event.decision === 'rejected').length)
  await flush()
  return { sent: queued.length, inert: false, reason: null }
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
