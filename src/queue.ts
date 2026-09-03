import { appendQueue, readQueue, replaceQueue } from './store.ts'
import type { QueuedRecord } from './store.ts'
import { currentToken, isOk, postEvents, refresh } from './api.ts'
import { toWireBatch } from './wire.ts'
import type { AgentId, CodingEvent } from './types.ts'

// A bounded local queue, so a developer working on a plane or behind a flaky VPN
// keeps coding at full speed and the client never grows without limit on their
// disk. When the bound is hit the OLDEST records go: newer signal is the signal
// somebody might still act on, and a queue that drops the new events is a queue
// that reports last Tuesday forever.
export const MAX_QUEUED_EVENTS = 2000

// The backend caps a batch at 500 (ArrayMaxSize on CodingEventBatchDto). Sending
// 501 would be a 400 that the ingest route converts to a 202, so the whole batch
// would vanish with the client believing it landed.
export const MAX_BATCH = 500

export interface FlushResult {
  attempted: number
  sent: number
  remaining: number
  // "Did the bytes leave" only. It is deliberately not "were they kept": ingest
  // answers 202 unconditionally, so nothing downstream may read this as proof
  // that anything was stored.
  delivered: boolean
}

// Returns the events it actually queued, not a count, so the receipt ledger can
// be written from the same list that reaches the wire. Writing the ledger from
// the caller's list instead would make the privacy proof claim a send that
// deduping had already dropped.
export function enqueue(agent: AgentId, sessionId: string, events: CodingEvent[]): CodingEvent[] {
  if (events.length === 0) return []
  const existing = readQueue()
  const known = new Set(existing.map((r) => r.event.eventId))
  const fresh = events.filter((event) => {
    if (known.has(event.eventId)) return false
    known.add(event.eventId)
    return true
  })
  if (fresh.length === 0) return []

  if (existing.length + fresh.length > MAX_QUEUED_EVENTS) {
    // Trim the combined list, not just the part already on disk: one hook can
    // enqueue more than the bound on its own, and slicing only `existing` leaves
    // the queue permanently over its limit.
    const combined = existing.concat(fresh.map((event) => ({ agent, sessionId, event })))
    replaceQueue(combined.slice(-MAX_QUEUED_EVENTS))
    return fresh
  }
  appendQueue(agent, sessionId, fresh)
  return fresh
}

// Feature 0098. `opts.timeoutMs`, when given, overrides the default per-request
// budget (api.ts's HOOK_TIMEOUT_MS) for every request this call makes. Used by
// `onPostToolUse`'s opportunistic live flush: that call sits on the developer's
// critical path for every tool call, not just at Stop/SessionEnd, so it must
// fail fast on a slow network rather than hold up typing. A request that times
// out lands in `failed` exactly like any other failure and stays queued for the
// next full-budget flush (Stop/SessionEnd/next SessionStart), so nothing is
// ever lost, only delayed.
export async function flush(opts: { timeoutMs?: number } = {}): Promise<FlushResult> {
  const queued = readQueue()
  if (queued.length === 0) return { attempted: 0, sent: 0, remaining: 0, delivered: true }

  const groups = groupBySession(queued)
  const failed: QueuedRecord[] = []
  let sent = 0

  for (const group of groups) {
    let creds = await currentToken(group.agent)
    if (!creds) {
      failed.push(...group.records)
      continue
    }
    for (let i = 0; i < group.records.length; i += MAX_BATCH) {
      const chunk = group.records.slice(i, i + MAX_BATCH)
      const batch = toWireBatch(
        group.agent,
        group.sessionId,
        chunk.map((r) => r.event),
      )
      let res = await postEvents(creds.apiUrl, creds.accessToken, batch, opts.timeoutMs)
      // The one failure /events surfaces. Everything else on this route is a 202
      // and must not be retried as if it were an error.
      if (res.status === 401) {
        const renewed = await refresh(creds, group.agent)
        if (!renewed) {
          failed.push(...chunk)
          continue
        }
        creds = renewed
        res = await postEvents(creds.apiUrl, creds.accessToken, batch, opts.timeoutMs)
      }
      if (isOk(res.status)) sent += chunk.length
      else failed.push(...chunk)
    }
  }

  replaceQueue(failed.slice(-MAX_QUEUED_EVENTS))
  return {
    attempted: queued.length,
    sent,
    remaining: failed.length,
    delivered: failed.length === 0,
  }
}

interface Group {
  agent: AgentId
  sessionId: string
  records: QueuedRecord[]
}

function groupBySession(records: QueuedRecord[]): Group[] {
  const groups = new Map<string, Group>()
  for (const record of records) {
    const key = `${record.agent}${record.sessionId}`
    const existing = groups.get(key)
    if (existing) existing.records.push(record)
    else groups.set(key, { agent: record.agent, sessionId: record.sessionId, records: [record] })
  }
  return [...groups.values()]
}
