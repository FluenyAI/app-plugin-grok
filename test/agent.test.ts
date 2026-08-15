import { afterEach, describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { detectAgent } from '../src/session.ts'

const GROK_KEYS = ['GROK_PLUGIN_ROOT', 'GROK_SESSION_ID', 'GROK_HOOK_EVENT'] as const

afterEach(() => {
  for (const key of GROK_KEYS) delete process.env[key]
})

describe('detectAgent', () => {
  it('defaults to claude-code', () => {
    assert.equal(detectAgent(), 'claude-code')
  })

  it('honours an explicit override', () => {
    process.env.GROK_PLUGIN_ROOT = '/tmp/plugin'
    assert.equal(detectAgent('claude-code'), 'claude-code')
    assert.equal(detectAgent('grok-build'), 'grok-build')
  })

  it('treats Grok hook environment as grok-build', () => {
    process.env.GROK_HOOK_EVENT = 'post_tool_use'
    assert.equal(detectAgent(), 'grok-build')
  })

  it('ignores an unknown override and falls through to the environment', () => {
    process.env.GROK_SESSION_ID = 'abc'
    assert.equal(detectAgent('cursor'), 'grok-build')
  })
})
