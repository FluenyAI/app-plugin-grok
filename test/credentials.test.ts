import { afterEach, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { join } from 'node:path'
import { useTempConfig } from './helpers.ts'

const root = useTempConfig()

const { inferAgentFromToken, listCredentialAgents, readCredentials, writeCredentials } =
  await import('../src/store.ts')

function jwtFor(agent: string): string {
  const payload = Buffer.from(JSON.stringify({ agent, typ: 'coding-hook' })).toString('base64url')
  return `hdr.${payload}.sig`
}

describe('per-agent credentials', () => {
  afterEach(() => {
    for (const name of [
      'credentials.json',
      'credentials.claude-code.json',
      'credentials.grok-build.json',
    ]) {
      try {
        rmSync(join(process.env.FLUENY_CONFIG_DIR!, name), { force: true })
      } catch {
        // ignore
      }
    }
  })

  it('keeps Claude Code and Grok tokens in separate files', () => {
    writeCredentials(
      {
        apiUrl: 'http://api.test',
        clientId: 'flueny-claude-code',
        accessToken: 'claude-token',
        refreshToken: 'claude-refresh',
        expiresAt: 1,
      },
      'claude-code',
    )
    writeCredentials(
      {
        apiUrl: 'http://api.test',
        clientId: 'flueny-claude-code',
        accessToken: 'grok-token',
        refreshToken: 'grok-refresh',
        expiresAt: 2,
      },
      'grok-build',
    )
    assert.equal(readCredentials('claude-code')?.accessToken, 'claude-token')
    assert.equal(readCredentials('grok-build')?.accessToken, 'grok-token')
    assert.deepEqual(listCredentialAgents().sort(), ['claude-code', 'grok-build'])
  })

  it('migrates a legacy credentials.json using the token agent claim', () => {
    const dir = process.env.FLUENY_CONFIG_DIR!
    writeFileSync(
      join(dir, 'credentials.json'),
      JSON.stringify({
        apiUrl: 'http://localhost:3001',
        clientId: 'flueny-claude-code',
        accessToken: jwtFor('grok-build'),
        refreshToken: 'refresh',
        expiresAt: 9,
      }),
      { mode: 0o600 },
    )
    const grok = readCredentials('grok-build')
    assert.equal(grok?.refreshToken, 'refresh')
    assert.equal(grok?.agent, 'grok-build')
    assert.equal(readCredentials('claude-code'), null)
    assert.equal(existsSync(join(dir, 'credentials.json')), false)
    const stored = JSON.parse(readFileSync(join(dir, 'credentials.grok-build.json'), 'utf8'))
    assert.equal(stored.agent, 'grok-build')
  })

  it('reads the agent claim out of a hook token', () => {
    assert.equal(inferAgentFromToken(jwtFor('grok-build')), 'grok-build')
    assert.equal(inferAgentFromToken(jwtFor('claude-code')), 'claude-code')
    assert.equal(inferAgentFromToken('not-a-jwt'), null)
  })
})
