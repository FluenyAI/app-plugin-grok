import { CODING_EVENT_FIELDS } from './types.ts'
import type { AgentId, CodingEvent, CodingEventBatch } from './types.ts'

// The redaction boundary. Nothing reaches the network except through here.
//
// This is a rebuild, not a filter: the outgoing object is constructed key by key
// from a fixed list, so an extractor that starts carrying an extra field cannot
// leak it by accident. A `delete raw.tool_input` style filter fails the opposite
// way, silently, the first time a payload gains a field nobody wrote a rule for,
// and this is the one promise the whole product rests on (CEO decisions 8A, 33A).
//
// It also drops values of the wrong type rather than passing them through, so a
// string smuggled into `durationMs` is not a channel either.

export function toWireEvent(event: CodingEvent): CodingEvent {
  const out: Record<string, unknown> = {}
  for (const field of CODING_EVENT_FIELDS) {
    const value = (event as unknown as Record<string, unknown>)[field]
    if (value === undefined) continue
    switch (field) {
      case 'eventId':
      case 'kind':
      case 'at':
        if (typeof value === 'string') out[field] = value
        break
      case 'repoId':
      case 'pathClass':
      case 'decision':
        if (value === null || typeof value === 'string') out[field] = value
        break
      case 'testsRun':
        if (typeof value === 'boolean') out[field] = value
        break
      case 'subagentCount':
      case 'durationMs':
        if (typeof value === 'number' && Number.isFinite(value)) out[field] = Math.trunc(value)
        break
    }
  }
  return out as unknown as CodingEvent
}

export function toWireBatch(agent: AgentId, sessionId: string, events: CodingEvent[]): CodingEventBatch {
  return { agent, sessionId, events: events.map(toWireEvent) }
}
