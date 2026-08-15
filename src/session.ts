import { CLIENT_VERSION, currentToken, isOk, refresh, sessionStart } from './api.ts'
import type { Credentials } from './store.ts'
import { findRepo } from './git.ts'
import { repoIdFor } from './repo-id.ts'
import { readBundle, readSession, writeBundle, writeSession } from './store.ts'
import type { SessionState } from './store.ts'
import type { AgentId, PolicyBundle, SessionStartResponse } from './types.ts'
import { readsLocallyDeclaration } from './reads.ts'

// The SessionStart handshake (CEO decision 4A). The client runs on machines
// Flueny does not control, so the server states on every session whether it
// should run at all, what it is allowed to look at, and what it is capable of.
//
// Three things make the client inert, and all three mean the same thing on the
// wire, which is nothing at all:
//
//   no credential      nobody has connected this machine yet
//   kill switch        the org turned the surface off (CEO decision 4A)
//   repo not allowed   CEO decision 14A, and it is FAIL CLOSED: an empty
//                      allowlist matches nothing, so a misconfigured org leaks
//                      no repository rather than every repository
//
// Inert is not an error. A developer who has not connected Flueny, and a
// developer whose org killed it, both get an editor that behaves exactly as if
// this client were not installed.

// Grok injects GROK_* on every hook and on plugin-owned processes. Claude Code
// never sets those, so they are a reliable host signal. An explicit --agent
// flag wins because /flueny:connect runs as a skill, not a hook, and may not
// inherit the hook environment.
export function detectAgent(override?: string | null): AgentId {
  if (override === 'grok-build' || override === 'claude-code') return override
  if (process.env.GROK_PLUGIN_ROOT || process.env.GROK_SESSION_ID || process.env.GROK_HOOK_EVENT) {
    return 'grok-build'
  }
  return 'claude-code'
}

export const AGENT: AgentId = detectAgent()

export interface BeginResult {
  state: SessionState
  handshake: SessionStartResponse | null
  // Which branch the bundle cache took, so the CLI can report it and
  // test/handshake.test.ts can assert it.
  bundleSource: 'server' | 'cache' | 'refetched' | 'none'
}

export async function beginSession(opts: {
  sessionId: string
  cwd: string
}): Promise<BeginResult> {
  const repo = findRepo(opts.cwd)
  const repoId = repo?.remote ? repoIdFor(repo.remote) : null
  const base: SessionState = {
    sessionId: opts.sessionId,
    agent: AGENT,
    startedAt: Date.now(),
    inert: true,
    inertReason: null,
    killSwitch: false,
    dryRun: false,
    dryRunEndsAt: null,
    repoId,
    repoRoot: repo?.root ?? null,
    toolUses: 0,
    subagents: 0,
    pendingEdits: [],
    testsRanThisTurn: false,
    seenToolUseIds: [],
    transcriptOffset: 0,
    eventSeq: 0,
  }

  const creds = await currentToken(AGENT)
  if (!creds) {
    return finish({ ...base, inertReason: 'not connected: run flueny login' }, null, 'none')
  }

  const cached = readBundle()
  let attempt = await runHandshake(creds, opts.sessionId, cached?.etag ?? null)
  let res = attempt.res
  let bundleSource: BeginResult['bundleSource'] = 'none'

  if (!isOk(res.status) || !res.body) {
    // A handshake that did not answer is not a reason to guess. Inert, quietly,
    // and the next session tries again.
    return finish({ ...base, inertReason: `handshake unavailable (${res.status})` }, null, 'none')
  }

  let answer = res.body
  let bundle: PolicyBundle | null = answer.bundle

  if (bundle) {
    writeBundle(bundle)
    bundleSource = 'server'
  } else if (cached) {
    // The steady state, and the whole point of CEO decision 25A: a session start
    // costs one small response once the client already holds the bundle.
    bundle = cached
    bundleSource = 'cache'
  } else {
    // Null bundle with nothing cached should not happen, but it is exactly the
    // shape a stale or hand-edited etag produces, and the failure is silent:
    // no classifier means every pathClass is null and every scorer starves. So
    // ask once more with no etag rather than run blind.
    attempt = await runHandshake(attempt.creds, opts.sessionId, null)
    res = attempt.res
    const refetched = isOk(res.status) ? res.body?.bundle : null
    if (res.body && refetched) {
      answer = res.body
      bundle = refetched
      writeBundle(refetched)
      bundleSource = 'refetched'
    }
  }

  const state: SessionState = {
    ...base,
    killSwitch: answer.killSwitch,
    dryRun: answer.dryRun,
    dryRunEndsAt: answer.dryRunEndsAt,
  }

  if (answer.killSwitch) {
    return finish({ ...state, inertReason: 'kill switch is on for this organisation' }, answer, bundleSource)
  }
  if (!repoId) {
    return finish({ ...state, inertReason: 'no git remote here, so this is not an org repository' }, answer, bundleSource)
  }
  // Fail closed. `includes` on an empty array is false, which is the entire
  // guarantee: an org that registered nothing receives nothing.
  if (!answer.repoAllowlist.includes(repoId)) {
    return finish({ ...state, inertReason: 'this repository is not on the org allowlist' }, answer, bundleSource)
  }
  if (!bundle) {
    return finish({ ...state, inertReason: 'no path classifier available' }, answer, bundleSource)
  }

  return finish({ ...state, inert: false, inertReason: null }, answer, bundleSource)
}

// `/session/start` carries the same hook token as `/events`, so it 401s the same
// way, and a 401 there is the ONE failure this surface deliberately shows a
// client (feature 0028: the client's own credential expiring, not Flueny's
// infrastructure failing). Without this retry a rejected token makes the client
// inert for the whole session and every session after it, silently, because
// `currentToken` only refreshes on the clock and a revoked install still has a
// clock that says fine.
async function runHandshake(
  creds: Credentials,
  sessionId: string,
  bundleEtag: string | null,
): Promise<{ res: Awaited<ReturnType<typeof sessionStart>>; creds: Credentials }> {
  const req = {
    agent: AGENT,
    sessionId,
    clientVersion: CLIENT_VERSION,
    bundleEtag,
    readsLocally: readsLocallyDeclaration(),
  }
  const res = await sessionStart(creds.apiUrl, creds.accessToken, req)
  if (res.status !== 401) return { res, creds }
  const renewed = await refresh(creds, AGENT)
  if (!renewed) return { res, creds }
  return { res: await sessionStart(renewed.apiUrl, renewed.accessToken, req), creds: renewed }
}

function finish(state: SessionState, handshake: SessionStartResponse | null, bundleSource: BeginResult['bundleSource']): BeginResult {
  writeSession(state)
  return { state, handshake, bundleSource }
}

// Hooks fire in whatever order Claude Code runs them, and the plugin can be
// installed halfway through a session, so every later hook has to cope with no
// state on disk. It handshakes rather than assuming, because assuming means
// either sending from a repository nobody allowed or silently sending nothing.
export async function ensureSession(sessionId: string, cwd: string): Promise<SessionState> {
  const existing = readSession(sessionId)
  if (existing) return existing
  return (await beginSession({ sessionId, cwd })).state
}

export function classifierFor(): Record<string, string[]> {
  return readBundle()?.pathClassifier ?? {}
}
