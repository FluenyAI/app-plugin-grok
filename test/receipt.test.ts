import test from 'node:test'
import assert from 'node:assert/strict'
import { useTempConfig } from './helpers.ts'

// The local receipt ledger, which is `flueny dry-run --today`. It is the privacy
// proof stated as field names, so the one thing it must never do is describe a
// payload that is not the payload.

useTempConfig()

const { entryFor, record, receiptFor, summaryFor, entriesFor } = await import('../src/receipt.ts')
import type { CodingEvent } from '../src/types.ts'

test('fieldsSent is generated from the serialized event, not written by hand', () => {
  const entry = entryFor({
    eventId: 'e1',
    kind: 'edit-decision',
    at: '2026-08-08T10:00:00.000Z',
    repoId: 'sha256:abc',
    pathClass: 'auth',
    decision: 'accepted',
    testsRun: true,
  })
  assert.deepEqual(entry.fieldsSent, ['eventId', 'kind', 'at', 'repoId', 'pathClass', 'decision', 'testsRun'])
  assert.equal(entry.wouldBlock, false)
})

test('a field that is not sent is not listed as sent', () => {
  // A rejected edit carries no `testsRun`, because nothing was applied for a
  // test to check. The ledger must not list it, and the summary must not claim
  // it either way.
  const rejected: CodingEvent = {
    eventId: 'e2',
    kind: 'edit-decision',
    at: '2026-08-08T10:00:00.000Z',
    repoId: 'sha256:abc',
    pathClass: 'tests',
    decision: 'rejected',
  }
  assert.equal(entryFor(rejected).fieldsSent.includes('testsRun'), false)
  assert.equal(summaryFor(rejected), 'Agent edit rejected, path class tests')
  assert.equal(summaryFor(rejected).includes('tests did not run'), false)
})

test('a summary never names a path, a command or a tool argument', () => {
  const summary = summaryFor({
    eventId: 'e3',
    kind: 'tool-use',
    at: '2026-08-08T10:00:00.000Z',
    repoId: 'sha256:abc',
    pathClass: 'frontend',
  })
  assert.equal(summary, 'Agent tool call, path class frontend')
})

test('observed counts raw tool calls and wouldSend counts derived events', () => {
  const day = '2026-08-08'
  record(
    [
      { eventId: 'a', kind: 'tool-use', at: '2026-08-08T10:00:00.000Z', repoId: null, pathClass: null },
      { eventId: 'b', kind: 'tool-use', at: '2026-08-08T10:00:01.000Z', repoId: null, pathClass: null },
    ],
    5,
    day,
  )
  assert.equal(entriesFor(day).length, 2)
  // Flattened, because the receipt is reflowed to 80 columns and the sentence
  // can break across a line.
  const receipt = receiptFor(day).split('\n').join(' ')
  assert.match(receipt, /observed 5 tool calls today/)
  // Nothing is enforced in M1, so the zero is measured, not a placeholder.
  assert.match(receipt, /sent 2 derived signals/)
  assert.match(receipt, /blocked 0 actions/)
})
