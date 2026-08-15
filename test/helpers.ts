import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'

// Test scaffolding. Every test that touches disk points FLUENY_CONFIG_DIR at a
// fresh temp directory, so a test run can never read or write the developer's
// real credential.

export function useTempConfig(): string {
  const dir = mkdtempSync(join(tmpdir(), 'flueny-test-'))
  process.env.FLUENY_CONFIG_DIR = join(dir, 'config')
  return dir
}

// A directory that looks like a git clone to src/git.ts, which reads .git/config
// and nothing else.
export function makeRepo(root: string, remote: string): string {
  const dir = join(root, 'repo')
  mkdirSync(join(dir, '.git'), { recursive: true })
  writeFileSync(
    join(dir, '.git', 'config'),
    ['[core]', '\tbare = false', '[remote "origin"]', `\turl = ${remote}`, '\tfetch = +refs/heads/*'].join('\n'),
  )
  return dir
}

export interface Capture {
  url: string
  body: unknown
  raw: string
}

// Replaces global fetch and records every request body, which is what the
// redaction test asserts against: what was SENT, never what came back. Ingest
// answers 202 to everything including malformed input, so a response-based
// assertion would pass on a client that sent nothing at all.
export function captureFetch(responder: (url: string, body: unknown) => { status: number; body: unknown }): {
  calls: Capture[]
  restore: () => void
} {
  const calls: Capture[] = []
  const original = globalThis.fetch
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = String(input)
    const raw = typeof init?.body === 'string' ? init.body : ''
    const body: unknown = raw ? JSON.parse(raw) : null
    calls.push({ url, body, raw })
    const answer = responder(url, body)
    return new Response(JSON.stringify(answer.body ?? {}), {
      status: answer.status,
      headers: { 'content-type': 'application/json' },
    })
  }) as typeof globalThis.fetch
  return { calls, restore: () => (globalThis.fetch = original) }
}

export const BUNDLE = {
  etag: 'etag-one',
  schemaVersion: 1,
  pathClassifier: {
    tests: ['**/*.test.*', '**/*.spec.*', '**/tests/**', '**/__tests__/**', 'test/**'],
    auth: ['**/auth/**', '**/*auth*', '**/session*', '**/*jwt*', '**/*passkey*'],
    docs: ['**/*.md', 'docs/**'],
    backend: ['**/*.ts', '**/*.py', '**/*.go', '**/*.rs', 'src/**'],
  },
  rules: [] as never[],
}

export function handshakeBody(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    killSwitch: false,
    capabilities: { agent: 'claude-code', canStreamEvents: true, canInjectContext: true, canEnforce: false },
    dryRun: false,
    dryRunEndsAt: null,
    repoAllowlist: [],
    bundle: BUNDLE,
    intervention: null,
    ...over,
  }
}
