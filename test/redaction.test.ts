import test from 'node:test'
import assert from 'node:assert/strict'
import { BUNDLE, captureFetch, handshakeBody, makeRepo, useTempConfig } from './helpers.ts'

// The test the product rests on.
//
// It drives a hook payload stuffed with every kind of thing that must never
// leave the machine (prompt text, a diff, file contents, a tool response, an
// absolute path, a secret) through the real extraction and queue path, and
// fails if any of it appears in a request body.
//
// It asserts on what was SENT, never on a response status. `/events` answers 202
// unconditionally, including on malformed input and on load shedding, so a test
// that checked the status would pass against a client that leaked everything.

const root = useTempConfig()

const { CODING_EVENT_FIELDS } = await import('../src/types.ts')
const { writeBundle, writeCredentials } = await import('../src/store.ts')
const { toWireEvent } = await import('../src/wire.ts')
const { extractToolFacts } = await import('../src/extract.ts')
const { onPostToolUse, onStop } = await import('../src/hooks.ts')
const { repoIdFor } = await import('../src/repo-id.ts')

const REMOTE = 'git@github.com:FluenyAI/app-backend.git'
const repoDir = makeRepo(root, REMOTE)
const repoId = repoIdFor(REMOTE)

// Every one of these is a string the client must be structurally incapable of
// transmitting. They are deliberately unusual so a substring search is decisive.
const POISON = [
  'PROMPTTEXT-refactor-the-billing-module',
  'CODEBODY-const-apiKey-equals-sk-live-1234',
  'FILECONTENTS-line-one-line-two',
  'TOOLRESPONSE-diff-plus-minus',
  'SECRETVALUE-hunter2',
  '/Users/someone/private/notes',
  'BRANCHNAME-feat-secret-project',
]

function hostilePayload(): Record<string, unknown> {
  return {
    session_id: 'session-redaction',
    transcript_path: `/Users/someone/private/notes/transcript.jsonl`,
    cwd: repoDir,
    hook_event_name: 'PostToolUse',
    permission_mode: 'acceptEdits',
    prompt: 'PROMPTTEXT-refactor-the-billing-module',
    tool_name: 'Edit',
    tool_input: {
      file_path: `${repoDir}/src/auth/session.ts`,
      old_string: 'CODEBODY-const-apiKey-equals-sk-live-1234',
      new_string: 'FILECONTENTS-line-one-line-two',
      command: 'git checkout BRANCHNAME-feat-secret-project',
      env: { API_KEY: 'SECRETVALUE-hunter2' },
    },
    tool_response: {
      filePath: '/Users/someone/private/notes/x.ts',
      structuredPatch: [{ lines: ['+TOOLRESPONSE-diff-plus-minus'] }],
      content: 'FILECONTENTS-line-one-line-two',
    },
  }
}

test('extraction returns no field that can hold raw payload content', () => {
  const facts = extractToolFacts(hostilePayload(), { repoRoot: repoDir, classifier: BUNDLE.pathClassifier })
  const serialized = JSON.stringify(facts)
  for (const poison of POISON) {
    assert.ok(!serialized.includes(poison), `extraction leaked ${poison}`)
  }
  // It did do its job: the path became a class and the raw path is gone.
  assert.equal(facts.pathClass, 'auth')
  assert.equal(facts.isEdit, true)
})

test('the wire serializer rebuilds from a fixed key list', () => {
  const smuggled = {
    eventId: 'e1',
    kind: 'tool-use',
    at: '2026-08-08T00:00:00.000Z',
    repoId: null,
    pathClass: 'tests',
    prompt: 'PROMPTTEXT-refactor-the-billing-module',
    tool_input: { command: 'SECRETVALUE-hunter2' },
    tool_response: 'TOOLRESPONSE-diff-plus-minus',
    filePath: '/Users/someone/private/notes',
  } as unknown as Parameters<typeof toWireEvent>[0]

  const wire = toWireEvent(smuggled)
  for (const key of Object.keys(wire)) {
    assert.ok((CODING_EVENT_FIELDS as readonly string[]).includes(key), `unexpected wire field ${key}`)
  }
  const serialized = JSON.stringify(wire)
  for (const poison of POISON) {
    assert.ok(!serialized.includes(poison), `wire serializer leaked ${poison}`)
  }
})

test('a hostile payload reaches the network as derived signal only', async () => {
  writeBundle(BUNDLE)
  writeCredentials({
    apiUrl: 'http://api.test',
    clientId: 'flueny-claude-code',
    accessToken: 'token',
    refreshToken: 'refresh',
    expiresAt: Date.now() + 3_600_000,
  })

  const { calls, restore } = captureFetch((url) => {
    if (url.endsWith('/session/start')) {
      return { status: 200, body: handshakeBody({ repoAllowlist: [repoId] }) }
    }
    return { status: 202, body: {} }
  })

  try {
    await onPostToolUse(hostilePayload())
    await onStop({ session_id: 'session-redaction', cwd: repoDir })
  } finally {
    restore()
  }

  const ingest = calls.filter((call) => call.url.endsWith('/events'))
  assert.ok(ingest.length > 0, 'nothing was posted, so this test proved nothing')

  const everything = calls.map((call) => call.raw).join('\n')
  for (const poison of POISON) {
    assert.ok(!everything.includes(poison), `the wire carried ${poison}`)
  }

  // And the keys, positively: a leak that happened to avoid every marker string
  // would still be a new key on the event.
  for (const call of ingest) {
    const batch = call.body as { agent: string; sessionId: string; events: Record<string, unknown>[] }
    assert.deepEqual(Object.keys(batch).sort(), ['agent', 'events', 'sessionId'])
    for (const event of batch.events) {
      for (const key of Object.keys(event)) {
        assert.ok((CODING_EVENT_FIELDS as readonly string[]).includes(key), `unexpected event field ${key}`)
      }
    }
  }

  const events = ingest.flatMap((call) => (call.body as { events: Record<string, unknown>[] }).events)
  assert.ok(
    events.some((e) => e.kind === 'edit-decision' && e.decision === 'accepted' && e.pathClass === 'auth'),
    'the edit decision was not derived at all',
  )
})
