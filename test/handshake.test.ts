import test from 'node:test'
import assert from 'node:assert/strict'
import { rmSync } from 'node:fs'
import { BUNDLE, captureFetch, handshakeBody, makeRepo, useTempConfig } from './helpers.ts'

// The handshake is what makes this client rollback-able (CEO decision 4A): it
// runs on machines Flueny does not control, so the server states on every session
// whether it should run at all and what it is allowed to look at.
//
// Three branches matter enough to be held by a test, because all three fail
// silently by design and none of them can be seen from the server side.

const root = useTempConfig()

const { beginSession } = await import('../src/session.ts')
const { configDir, writeCredentials } = await import('../src/store.ts')
const { repoIdFor } = await import('../src/repo-id.ts')
const { onPostToolUse } = await import('../src/hooks.ts')

const REMOTE = 'git@github.com:FluenyAI/app-backend.git'
const repoDir = makeRepo(root, REMOTE)
const repoId = repoIdFor(REMOTE)

function connect(): void {
  writeCredentials({
    apiUrl: 'http://api.test',
    clientId: 'flueny-claude-code',
    accessToken: 'token',
    refreshToken: 'refresh',
    expiresAt: Date.now() + 3_600_000,
  })
}

function forgetBundle(): void {
  rmSync(`${configDir()}/bundle.json`, { force: true })
}

test('the kill switch makes the client inert, with no allowlist and no bundle', async () => {
  connect()
  forgetBundle()
  const { calls, restore } = captureFetch(() => ({
    status: 200,
    body: handshakeBody({ killSwitch: true, repoAllowlist: [], bundle: null }),
  }))
  try {
    const { state } = await beginSession({ sessionId: 'kill-1', cwd: repoDir })
    assert.equal(state.inert, true)
    assert.equal(state.killSwitch, true)
    assert.match(state.inertReason ?? '', /kill switch/)

    // And inert means inert: a tool call after a killed handshake sends nothing.
    const outcome = await onPostToolUse({
      session_id: 'kill-1',
      cwd: repoDir,
      tool_name: 'Edit',
      tool_input: { file_path: `${repoDir}/src/app.ts` },
    })
    assert.equal(outcome.sent, 0)
    assert.equal(calls.filter((call) => call.url.endsWith('/events')).length, 0)
  } finally {
    restore()
  }
})

test('the repo allowlist is fail closed: an empty list sends nothing', async () => {
  connect()
  forgetBundle()
  const { calls, restore } = captureFetch(() => ({ status: 200, body: handshakeBody({ repoAllowlist: [] }) }))
  try {
    const { state } = await beginSession({ sessionId: 'allow-empty', cwd: repoDir })
    assert.equal(state.inert, true)
    assert.match(state.inertReason ?? '', /not on the org allowlist/)
    assert.equal(state.repoId, repoId, 'the repo id was derived, it just is not allowed')

    const outcome = await onPostToolUse({
      session_id: 'allow-empty',
      cwd: repoDir,
      tool_name: 'Edit',
      tool_input: { file_path: `${repoDir}/src/app.ts` },
    })
    assert.equal(outcome.sent, 0)
    assert.equal(calls.filter((call) => call.url.endsWith('/events')).length, 0)
  } finally {
    restore()
  }
})

test('a repository with no git remote is inert, never sent under a null repoId', async () => {
  connect()
  forgetBundle()
  const { restore } = captureFetch(() => ({ status: 200, body: handshakeBody({ repoAllowlist: [repoId] }) }))
  try {
    const { state } = await beginSession({ sessionId: 'no-remote', cwd: root })
    assert.equal(state.inert, true)
    assert.equal(state.repoId, null)
    assert.match(state.inertReason ?? '', /no git remote/)
  } finally {
    restore()
  }
})

test('an allowlisted repository is live', async () => {
  connect()
  forgetBundle()
  const { restore } = captureFetch(() => ({ status: 200, body: handshakeBody({ repoAllowlist: [repoId] }) }))
  try {
    const { state } = await beginSession({ sessionId: 'allow-ok', cwd: repoDir })
    assert.equal(state.inert, false)
    assert.equal(state.inertReason, null)
    assert.equal(state.repoId, repoId)
  } finally {
    restore()
  }
})

test('the bundle ETag path: sent, then cached, then refetched when the cache is gone', async () => {
  connect()
  forgetBundle()

  // 1. First handshake. No etag held, so the server sends the bundle.
  let first = captureFetch(() => ({ status: 200, body: handshakeBody({ repoAllowlist: [repoId] }) }))
  try {
    const result = await beginSession({ sessionId: 'etag-1', cwd: repoDir })
    assert.equal(result.bundleSource, 'server')
    assert.equal((first.calls[0]?.body as { bundleEtag: string | null }).bundleEtag, null)
  } finally {
    first.restore()
  }

  // 2. Steady state. The client offers its etag and the server answers null,
  //    which is the whole of CEO decision 25A: one small response per session.
  const second = captureFetch((_url, body) => {
    const req = body as { bundleEtag: string | null }
    assert.equal(req.bundleEtag, BUNDLE.etag, 'the client did not offer its cached etag')
    return { status: 200, body: handshakeBody({ repoAllowlist: [repoId], bundle: null }) }
  })
  try {
    const result = await beginSession({ sessionId: 'etag-2', cwd: repoDir })
    assert.equal(result.bundleSource, 'cache')
    assert.equal(result.state.inert, false, 'a cached bundle still has to make the session live')
    assert.equal(second.calls.length, 1)
  } finally {
    second.restore()
  }

  // 3. The failure mode that would otherwise be silent: null bundle with nothing
  //    cached. Without the refetch every pathClass is null forever and no scorer
  //    ever sees a class, with no error anywhere.
  forgetBundle()
  let answered = 0
  const third = captureFetch(() => {
    answered += 1
    return {
      status: 200,
      body: handshakeBody({ repoAllowlist: [repoId], bundle: answered === 1 ? null : BUNDLE }),
    }
  })
  try {
    const result = await beginSession({ sessionId: 'etag-3', cwd: repoDir })
    assert.equal(result.bundleSource, 'refetched')
    assert.equal(third.calls.length, 2)
    assert.equal(result.state.inert, false)
  } finally {
    third.restore()
  }
})

test('a handshake that does not answer leaves the client inert rather than guessing', async () => {
  connect()
  const { restore } = captureFetch(() => ({ status: 503, body: {} }))
  try {
    const { state } = await beginSession({ sessionId: 'down', cwd: repoDir })
    assert.equal(state.inert, true)
    assert.match(state.inertReason ?? '', /handshake unavailable/)
  } finally {
    restore()
  }
})

test('a rejected hook token refreshes and retries rather than going inert forever', async () => {
  // The failure this catches was found live, not in review: the client thought
  // its token was valid (the clock said so), the server rejected it, and
  // beginSession read the 401 as "handshake unavailable". The client then went
  // inert for that session and every session after it, silently, with a working
  // refresh token sitting on disk the whole time.
  connect()
  forgetBundle()
  let handshakes = 0
  const { calls, restore } = captureFetch((url) => {
    if (url.endsWith('/oauth/token')) {
      return { status: 200, body: { access_token: 'fresh', refresh_token: 'r2', expires_in: 3600 } }
    }
    handshakes += 1
    return handshakes === 1
      ? { status: 401, body: { message: 'Invalid or expired hook token' } }
      : { status: 201, body: handshakeBody({ repoAllowlist: [repoId] }) }
  })
  try {
    const { state } = await beginSession({ sessionId: 'expired-token', cwd: repoDir })
    assert.equal(state.inert, false, 'a refreshable 401 must not leave the client inert')
  } finally {
    restore()
  }
  assert.equal(calls.filter((c) => c.url.endsWith('/oauth/token')).length, 1)
  assert.equal(handshakes, 2)
})

test('a 401 with no usable refresh token is inert, not a crash', async () => {
  connect()
  forgetBundle()
  const { restore } = captureFetch((url) =>
    url.endsWith('/oauth/token') ? { status: 400, body: { error: 'invalid_grant' } } : { status: 401, body: {} },
  )
  try {
    const { state } = await beginSession({ sessionId: 'dead-token', cwd: repoDir })
    assert.equal(state.inert, true)
    assert.match(state.inertReason ?? '', /handshake unavailable \(401\)/)
  } finally {
    restore()
  }
})

test('the handshake accepts the 201 NestJS actually returns, not only 200', async () => {
  // POST /session/start has no explicit @HttpCode on the backend, so it answers
  // 201. A client that compared the status to 200 would treat every successful
  // handshake as a failure, which is what happened the first time this ran live.
  connect()
  forgetBundle()
  const { restore } = captureFetch(() => ({ status: 201, body: handshakeBody({ repoAllowlist: [repoId] }) }))
  try {
    const { state } = await beginSession({ sessionId: 'created-201', cwd: repoDir })
    assert.equal(state.inert, false)
  } finally {
    restore()
  }
})
