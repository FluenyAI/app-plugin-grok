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
// actually calls sendTurnSubmissions, or that SessionEnd stays uninvolved. A
// regression that moved (or duplicated) insight submission onto SessionEnd
// would have passed every existing test in this repo.

useTempConfig()

const { onPostToolUse, onStop, onSessionEnd } = await import('../src/hooks.ts')
const { clearCredentials, readCounters, today, writeCredentials, writeSession } = await import('../src/store.ts')
const { entriesFor } = await import('../src/receipt.ts')
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
    liveFeedbackEnabled: false,
    rawActivityEnabled: false,
    turnToolActivity: [],
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

// today() is real wall-clock time and every test in this file shares one
// counters file and one ledger file for it, so an exact assertion like
// "insightsSent is 1" would break depending on test order. Diffing against a
// snapshot taken before the call under test keeps each test's claim about
// itself only, whatever ran before it the same day.
function insightSnapshot(): { sent: number; failed: number; rows: number } {
  const day = today()
  const counters = readCounters(day)
  return {
    sent: counters.insightsSent,
    failed: counters.insightsFailed,
    rows: entriesFor(day).length,
  }
}

test('a turn is scored on Stop, immediately, not deferred to SessionEnd or /exit', async () => {
  connect()
  const sessionId = 'stop-sends-insight'
  seedSession(sessionId, { promptInsightsEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const before = insightSnapshot()
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

  // The local trace this whole file exists to protect: a delivered turn is
  // now answerable from disk, not only inferable from "nothing threw".
  const after = insightSnapshot()
  assert.equal(after.sent - before.sent, 1)
  assert.equal(after.failed - before.failed, 0)
  assert.equal(after.rows - before.rows, 1, 'a delivered turn adds exactly one ledger row')
})

test('a submission the backend rejects counts as failed, visibly, not silently dropped', async () => {
  connect()
  const sessionId = 'stop-insight-rejected'
  seedSession(sessionId, { promptInsightsEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const before = insightSnapshot()
  const { restore } = captureFetch((url) =>
    url.endsWith('/integrations/coding/insights') ? { status: 500, body: {} } : { status: 202, body: {} },
  )
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const after = insightSnapshot()
  assert.equal(after.sent - before.sent, 0)
  assert.equal(after.failed - before.failed, 1)
  assert.equal(after.rows - before.rows, 0, 'a rejected submission must not claim a row that left the machine')
})

test('a submission attempted with no credential on this machine still counts as failed', async () => {
  // Every test in this file shares one temp config dir, so an earlier test's
  // connect() would otherwise leave a credential behind. Clearing it is what
  // actually exercises currentToken(AGENT) resolving null, the branch
  // sendTurnSubmissions takes before it ever reaches the network.
  clearCredentials('claude-code')
  const sessionId = 'stop-insight-no-credential'
  seedSession(sessionId, { promptInsightsEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const before = insightSnapshot()
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/insights')), false)
  const after = insightSnapshot()
  assert.equal(after.failed - before.failed, 1)
  assert.equal(after.rows - before.rows, 0)
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

// Feature 0098. Mirrors the three tests above exactly, on the independent
// liveFeedbackEnabled flag.
test('a turn is sent to /live-feedback on Stop when live feedback is on', async () => {
  connect()
  const sessionId = 'stop-sends-live-feedback'
  seedSession(sessionId, { liveFeedbackEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const liveFeedbackCalls = calls.filter((c) => c.url.endsWith('/integrations/coding/live-feedback'))
  assert.equal(liveFeedbackCalls.length, 1)
  assert.equal((liveFeedbackCalls[0]?.body as { prompt: string }).prompt, 'fix the login bug')
  assert.equal((liveFeedbackCalls[0]?.body as { response: string }).response, 'Fixed it in auth.ts')
  // Never rides along on the other opt-in's endpoint.
  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/insights')), false)
})

test('a developer who has not opted into live feedback sends nothing to /live-feedback', async () => {
  connect()
  const sessionId = 'stop-live-feedback-off'
  seedSession(sessionId, { liveFeedbackEnabled: false })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/live-feedback')), false)
})

test('SessionEnd never posts live feedback either: exiting is not part of this path', async () => {
  connect()
  const sessionId = 'session-end-no-live-feedback'
  seedSession(sessionId, { liveFeedbackEnabled: true })

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onSessionEnd({ session_id: sessionId, cwd: '/tmp' })
  } finally {
    restore()
  }

  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/live-feedback')), false)
})

// The independence itself: each opt-in controls only its own endpoint, and
// the sweep is shared rather than run twice.
test('a turn with both opt-ins on reaches both endpoints, from one sweep', async () => {
  connect()
  const sessionId = 'stop-both-opt-ins'
  seedSession(sessionId, { promptInsightsEnabled: true, liveFeedbackEnabled: true })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const insightCalls = calls.filter((c) => c.url.endsWith('/integrations/coding/insights'))
  const liveFeedbackCalls = calls.filter((c) => c.url.endsWith('/integrations/coding/live-feedback'))
  assert.equal(insightCalls.length, 1)
  assert.equal(liveFeedbackCalls.length, 1)
  assert.equal((insightCalls[0]?.body as { turnId: string }).turnId, (liveFeedbackCalls[0]?.body as { turnId: string }).turnId)
})

test('a turn with neither opt-in on sweeps nothing and calls neither endpoint', async () => {
  connect()
  const sessionId = 'stop-neither-opt-in'
  seedSession(sessionId, { promptInsightsEnabled: false, liveFeedbackEnabled: false })
  const transcript = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])

  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/insights')), false)
  assert.equal(calls.some((c) => c.url.endsWith('/integrations/coding/live-feedback')), false)
})

// Feature 0109. turnToolActivity accumulates one entry per PostToolUse and is
// attached only to the live-feedback submission (never to InsightSubmission,
// which has no field for it), then reset so a later Stop with no new tool
// calls does not resend the same activity.
test('turnToolActivity accumulates across tool calls and is attached to the live-feedback submission, then reset', async () => {
  connect()
  const sessionId = 'stop-attaches-tool-activity'
  seedSession(sessionId, { liveFeedbackEnabled: true })

  const { restore: restoreTools } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onPostToolUse({ session_id: sessionId, cwd: '/tmp', tool_name: 'Read', tool_input: { file_path: '/tmp/README.md' } })
    await onPostToolUse({ session_id: sessionId, cwd: '/tmp', tool_name: 'Bash', tool_input: { command: 'npm test' } })
  } finally {
    restoreTools()
  }

  const transcript = writeTranscript([userText('review recent changes'), assistantText('Looked at README and ran tests')])
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const liveFeedbackCalls = calls.filter((c) => c.url.endsWith('/integrations/coding/live-feedback'))
  assert.equal(liveFeedbackCalls.length, 1)
  const toolActivity = (liveFeedbackCalls[0]?.body as { toolActivity?: { toolCategory: string }[] }).toolActivity
  assert.equal(Array.isArray(toolActivity), true)
  assert.deepEqual(
    toolActivity?.map((e) => e.toolCategory),
    ['read', 'bash'],
  )

  // A second Stop with no tool calls in between must not resend it.
  const transcript2 = writeTranscript([userText('one more thing'), assistantText('Done')])
  const { calls: calls2, restore: restore2 } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript2 })
  } finally {
    restore2()
  }
  const secondCall = calls2.find((c) => c.url.endsWith('/integrations/coding/live-feedback'))
  assert.equal((secondCall?.body as { toolActivity?: unknown[] } | undefined)?.toolActivity, undefined)
})

test('InsightSubmission never carries toolActivity, even when both opt-ins are on', async () => {
  connect()
  const sessionId = 'stop-insight-no-tool-activity'
  seedSession(sessionId, { promptInsightsEnabled: true, liveFeedbackEnabled: true })

  const { restore: restoreTools } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onPostToolUse({ session_id: sessionId, cwd: '/tmp', tool_name: 'Bash', tool_input: { command: 'npm test' } })
  } finally {
    restoreTools()
  }

  const transcript = writeTranscript([userText('ran the tests'), assistantText('All green')])
  const { calls, restore } = captureFetch(() => ({ status: 202, body: {} }))
  try {
    await onStop({ session_id: sessionId, cwd: '/tmp', transcript_path: transcript })
  } finally {
    restore()
  }

  const insightCall = calls.find((c) => c.url.endsWith('/integrations/coding/insights'))
  assert.equal(insightCall && 'toolActivity' in (insightCall.body as Record<string, unknown>), false)
  const liveFeedbackCall = calls.find((c) => c.url.endsWith('/integrations/coding/live-feedback'))
  assert.equal(Array.isArray((liveFeedbackCall?.body as { toolActivity?: unknown[] } | undefined)?.toolActivity), true)
})
