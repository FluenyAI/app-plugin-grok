import { appendLedger, bumpCounters, readCounters, readLedger, today } from './store.ts'
import type { LedgerEntry } from './store.ts'
import { toWireEvent } from './wire.ts'
import { dailyReceipt } from './copy.ts'
import type { CodingEvent } from './types.ts'

// The local half of design decision 44. `GET /dry-run` is the server-rendered
// receipt and stays authoritative; this is the copy on the developer's own disk,
// which is what makes `flueny dry-run --today` answerable with the network down
// and, more to the point, answerable without asking Flueny what Flueny received.
//
// `fieldsSent` is generated from the serialized event, never written by hand. A
// hand-written list is a description of the payload that drifts from the payload,
// and the one thing this ledger exists to be is not that.

export function summaryFor(event: CodingEvent): string {
  const where = event.pathClass ? `path class ${event.pathClass}` : 'no classified path'
  switch (event.kind) {
    case 'edit-decision': {
      // `testsRun` is absent on a rejection, because nothing was applied for a
      // test to check. Rendering absent as "tests did not run" would put a claim
      // in the receipt that the payload beside it does not make, on the one
      // surface whose whole job is that the two agree.
      const tests = event.testsRun === undefined ? '' : `, tests ${event.testsRun ? 'ran' : 'did not run'}`
      return `Agent edit ${event.decision ?? 'recorded'}, ${where}${tests}`
    }
    case 'subagent':
      return 'Subagent delegated'
    case 'session-end':
      return `Session ended after ${Math.round((event.durationMs ?? 0) / 60000)} min, ${event.subagentCount ?? 0} subagents`
    case 'tool-use':
    default:
      return `Agent tool call, ${where}`
  }
}

export function entryFor(event: CodingEvent): LedgerEntry {
  const wire = toWireEvent(event)
  return {
    at: wire.at,
    summary: summaryFor(wire),
    fieldsSent: Object.keys(wire),
    // Literally false until `/gate` exists at M3. Typed as the literal on the
    // wire contract so it cannot drift into a promise M1 does not keep.
    wouldBlock: false,
  }
}

// `observed` counts raw tool calls this client looked at. `wouldSend` counts
// derived events. The gap between them is the privacy claim stated as
// arithmetic, which is why they are two counters and not one.
export function record(events: CodingEvent[], observed: number, day = today()): void {
  if (observed > 0) bumpCounters(day, { observed })
  if (events.length === 0) return
  appendLedger(day, events.map(entryFor))
  bumpCounters(day, { wouldSend: events.length })
}

export function receiptFor(day = today()): string {
  const counters = readCounters(day)
  return dailyReceipt({
    toolCalls: counters.observed,
    signals: counters.wouldSend,
    // Not a placeholder. Nothing is enforced in M1, so this is a measured zero.
    blocked: counters.wouldBlock,
  })
}

export function entriesFor(day = today()): LedgerEntry[] {
  return readLedger(day)
}
