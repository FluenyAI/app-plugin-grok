import test from 'node:test'
import assert from 'node:assert/strict'
import { captureFetch, useTempConfig } from './helpers.ts'

// The queue is bounded and deduped on `eventId` (CEO decision 23A). Both are
// asserted on what was sent, because ingest answers 202 to everything: a replay
// is dropped silently and still returns 202, so the response says nothing.

useTempConfig()

const { enqueue, flush, MAX_QUEUED_EVENTS, MAX_BATCH } = await import('../src/queue.ts')
const { readQueue, replaceQueue, writeCredentials } = await import('../src/store.ts')
const { CODING_EVENT_FIELDS } = await import('../src/types.ts')
import type { CodingEvent } from '../src/types.ts'

function event(id: string): CodingEvent {
  return { eventId: id, kind: 'tool-use', at: new Date().toISOString(), repoId: 'sha256:x', pathClass: 'tests' }
}

function connect(): void {
  writeCredentials({
    apiUrl: 'http://api.test',
    clientId: 'flueny-claude-code',
    accessToken: 'token',
    refreshToken: 'refresh',
    expiresAt: Date.now() + 3_600_000,
  })
}

test('an eventId already queued is not queued again', () => {
  replaceQueue([])
  assert.equal(enqueue('claude-code', 's1', [event('a'), event('b')]).length, 2)
  assert.equal(enqueue('claude-code', 's1', [event('a'), event('c')]).length, 1)
  assert.deepEqual(
    readQueue().map((r) => r.event.eventId),
    ['a', 'b', 'c'],
  )
})

test('a duplicate inside one call is queued once', () => {
  replaceQueue([])
  assert.equal(enqueue('claude-code', 's1', [event('dup'), event('dup')]).length, 1)
  assert.equal(readQueue().length, 1)
})

test('the queue is bounded and sheds the oldest, never the newest', () => {
  replaceQueue([])
  const many = Array.from({ length: MAX_QUEUED_EVENTS + 50 }, (_, i) => event(`e${i}`))
  enqueue('claude-code', 's1', many)
  const queued = readQueue()
  assert.equal(queued.length, MAX_QUEUED_EVENTS)
  // The last event written is still there; the first is gone. A queue that
  // dropped the new ones would keep reporting last Tuesday forever.
  assert.equal(queued.at(-1)?.event.eventId, `e${MAX_QUEUED_EVENTS + 49}`)
  assert.equal(
    queued.some((r) => r.event.eventId === 'e0'),
    false,
  )
})

test('flush sends every queued event once and empties the queue', async () => {
  connect()
  replaceQueue([])
  enqueue('claude-code', 's1', [event('x'), event('y')])
  enqueue('claude-code', 's2', [event('z')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    const result = await flush()
    assert.equal(result.sent, 3)
    assert.equal(result.remaining, 0)
  } finally {
    restore()
  }

  // One batch per session, because CodingEventBatch carries a single sessionId.
  assert.equal(calls.length, 2)
  const ids = calls.flatMap((call) => (call.body as { events: CodingEvent[] }).events.map((e) => e.eventId))
  assert.deepEqual(ids.sort(), ['x', 'y', 'z'])
  assert.equal(readQueue().length, 0)
})

test('a batch never exceeds the size the backend accepts', async () => {
  connect()
  replaceQueue([])
  enqueue(
    'claude-code',
    's1',
    Array.from({ length: MAX_BATCH + 20 }, (_, i) => event(`b${i}`)),
  )
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await flush()
  } finally {
    restore()
  }
  assert.equal(calls.length, 2)
  for (const call of calls) {
    const batch = call.body as { events: CodingEvent[] }
    assert.ok(batch.events.length <= MAX_BATCH, `a batch of ${batch.events.length} would be rejected as malformed`)
  }
})

test('a 401 refreshes the hook token once and resends, and only a 401 does', async () => {
  connect()
  replaceQueue([])
  enqueue('claude-code', 's1', [event('needs-refresh')])

  let ingestCalls = 0
  const { calls, restore } = captureFetch((url) => {
    if (url.endsWith('/oauth/token')) {
      return { status: 200, body: { access_token: 'fresh', refresh_token: 'r2', expires_in: 3600 } }
    }
    ingestCalls += 1
    return ingestCalls === 1 ? { status: 401, body: {} } : { status: 202, body: {} }
  })
  try {
    const result = await flush()
    assert.equal(result.sent, 1)
  } finally {
    restore()
  }
  assert.equal(calls.filter((c) => c.url.endsWith('/oauth/token')).length, 1)
  assert.equal(ingestCalls, 2)
  assert.equal(readQueue().length, 0)
})

test('an unreachable backend keeps the events rather than dropping them', async () => {
  connect()
  replaceQueue([])
  enqueue('claude-code', 's1', [event('kept')])
  const original = globalThis.fetch
  globalThis.fetch = (() => Promise.reject(new Error('ECONNREFUSED'))) as typeof globalThis.fetch
  try {
    const result = await flush()
    assert.equal(result.sent, 0)
    assert.equal(result.delivered, false)
  } finally {
    globalThis.fetch = original
  }
  assert.deepEqual(
    readQueue().map((r) => r.event.eventId),
    ['kept'],
  )
})

test('everything on the wire is a whitelisted field', async () => {
  connect()
  replaceQueue([])
  enqueue('claude-code', 's1', [event('fields')])
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await flush()
  } finally {
    restore()
  }
  const batch = calls[0]?.body as { events: Record<string, unknown>[] }
  for (const key of Object.keys(batch.events[0] ?? {})) {
    assert.ok((CODING_EVENT_FIELDS as readonly string[]).includes(key))
  }
})

// The server was made tolerant on 2026-08-08 (feature 0028, the `/events`
// tolerance list): over 500 events is TRUNCATED, not refused, and nothing tells
// the client. The tail is then lost permanently. So the cap is enforced here,
// and these are the tests that hold it, because no response ever will.

test('a batch stays well inside the 1 MB body limit at the field maxima', () => {
  const maxed: CodingEvent[] = Array.from({ length: MAX_BATCH }, (_, i) => ({
    eventId: `${i}`.padStart(200, 'x'), // MaxLength(200)
    kind: 'edit-decision',
    at: '2026-08-08T12:34:56.789Z',
    repoId: `sha256:${'a'.repeat(64)}`,
    pathClass: 'x'.repeat(64), // MaxLength(64)
    decision: 'accepted',
    testsRun: true,
    subagentCount: 1000,
    durationMs: 999_999_999,
  }))
  const bytes = Buffer.byteLength(JSON.stringify({ agent: 'claude-code', sessionId: 'x'.repeat(200), events: maxed }))
  assert.ok(bytes < 1_000_000, `${bytes} bytes would be refused by the 1 MB body limit`)
})

test('the client chunks rather than letting the server truncate the tail', async () => {
  connect()
  replaceQueue([])
  const count = MAX_BATCH * 2 + 1
  enqueue(
    'claude-code',
    's1',
    Array.from({ length: count }, (_, i) => event(`t${i}`)),
  )
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await flush()
  } finally {
    restore()
  }
  const ids = calls.flatMap((call) => (call.body as { events: CodingEvent[] }).events.map((e) => e.eventId))
  // Every event reached a request body. A client that posted one oversized batch
  // would see the same 202 and lose everything after the 500th.
  assert.equal(new Set(ids).size, count)
  assert.equal(calls.length, 3)
})

test('a 5xx is not read as delivery, and a 202 with a garbage body still is', async () => {
  connect()
  replaceQueue([])
  enqueue('claude-code', 's1', [event('server-error')])
  let broken = captureFetch(() => ({ status: 500, body: {} }))
  try {
    assert.equal((await flush()).sent, 0)
  } finally {
    broken.restore()
  }
  assert.equal(readQueue().length, 1, 'a 5xx must keep the event, not drop it')

  // Ingest answers 202 with an empty body by contract. A client that required a
  // parseable body would retry forever against a correct server.
  const empty = captureFetch(() => ({ status: 202, body: null }))
  try {
    assert.equal((await flush()).sent, 1)
  } finally {
    empty.restore()
  }
  assert.equal(readQueue().length, 0)
})
