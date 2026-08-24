import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { captureFetch, useTempConfig } from './helpers.ts'

// Feature 0094's whole point is that a scored turn leaves on Stop, which fires
// after every assistant response, not on SessionEnd (/exit). hooks.ts had no
// test of its own orchestration before this file: prompt-insight.ts, queue.ts
// and api.ts were each tested in isolation, but nothing asserted which hook
// actually calls sendPromptInsights, or that SessionEnd stays uninvolved. A
// regression that moved (or duplicated) insight submission onto SessionEnd
// would have passed every existing test in this repo.

useTempConfig()

const { onStop, onSessionEnd } = await import('../src/hooks.ts')
const { writeCredentials, writeSession } = await import('../src/store.ts')
import type { SessionState } from '../src/store.ts'

function connect(): void {
  writeCredentials({
    apiUrl: 'http://api.test',
    clientId: 'flueny-claude-code',
    accessToken: 'token',
    refreshToken: 'refresh',
    expiresAt: Date.now() + 3_600_000,
  })
}

function seedSession(sessionId: string, over: Partial<SessionState> = {}): void {
  writeSession({
    sessionId,
    agent: 'claude-code',
    startedAt: Date.now(),
    inert: false,
    inertReason: null,
    killSwitch: false,
    dryRun: false,
    dryRunEndsAt: null,
    repoId: 'sha256:repo',
    repoRoot: null,
    toolUses: 0,
    subagents: 0,
    pendingEdits: [],
    testsRanThisTurn: false,
    seenToolUseIds: [],
    transcriptOffset: 0,
    eventSeq: 0,
    promptInsightsEnabled: false,
    promptInsightLineOffset: 0,
    promptInsightSeq: 0,
    ...over,
  })
}

function writeTranscript(lines: unknown[]): string {
  const dir = mkdtempSync(join(tmpdir(), 'flueny-hooks-test-'))
  const path = join(dir, 'transcript.jsonl')
  writeFileSync(path, lines.map((line) => JSON.stringify(line)).join('\n') + '\n')
  return path
}

function userText(text: string): unknown {
  return { type: 'user', message: { role: 'user', content: [{ type: 'text', text }] } }
}

function assistantText(text: string): unknown {
  return { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text }] } }
}

test('a turn is scored on Stop, immediately, not deferred to SessionEnd or /exit', async () => {
  connect()
  const sessionId = 'stop-sends-insight'
  seedSession(sessionId, { promptInsightsEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const insightCalls = calls.filter((c) => c.url.endsWith('/integrations/coding/insights'))
  assert.equal(insightCalls.length, 1, 'Stop must send the turn itself, not queue it for exit to flush')
  assert.equal((insightCalls[0]?.body as { prompt: string }).prompt, 'fix the login bug')
  assert.equal((insightCalls[0]?.body as { response: string }).response, 'Fixed it in auth.ts')
})

test('a developer who has not opted in sends nothing on Stop, even with a transcript', async () => {
  connect()
  const sessionId = 'stop-opted-out'
  seedSession(sessionId, { promptInsightsEnabled: false })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/insights')), false)
})

test('SessionEnd never posts an insight itself: exiting is not part of this path', async () => {
  connect()
  const sessionId = 'session-end-no-insight'
  seedSession(sessionId, { promptInsightsEnabled: true })

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onSessionEnd({ session_id: sessionId, cwd: '/tmp' })
  } finally {
    restore()
  }

  assert.equal(
    calls.some((c) => c.url.endsWith('/integrations/coding/insights')),
    false,
    'insight submission is Stop-only; SessionEnd must never be required for a scored turn to reach the backend',
  )
})
