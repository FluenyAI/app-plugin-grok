import { describe, it } from 'node:test'
import assert from 'node:assert/strict'
import { extractToolFacts } from '../src/extract.ts'
import { BUNDLE } from './helpers.ts'

describe('extractToolFacts', () => {
  it('treats a Grok search_replace payload as an edit', () => {
    const facts = extractToolFacts(
      {
        sessionId: 'grok-session',
        cwd: '/repo',
        hookEventName: 'post_tool_use',
        toolName: 'search_replace',
        toolUseId: 'tu-1',
        toolInput: { path: 'src/auth/session.ts', old_string: 'a', new_string: 'b' },
        toolResult: { ok: true },
      },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(facts.isEdit, true)
    assert.equal(facts.kind, 'tool-use')
    assert.equal(facts.pathClass, 'auth')
    assert.equal(facts.toolUseId, 'tu-1')
    assert.equal(facts.declined, false)
  })

  it('treats spawn_subagent as a subagent', () => {
    const facts = extractToolFacts(
      {
        toolName: 'spawn_subagent',
        toolInput: { description: 'explore the repo' },
      },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(facts.kind, 'subagent')
    assert.equal(facts.isEdit, false)
  })

  it('reads a test command from Grok run_terminal_command', () => {
    const facts = extractToolFacts(
      {
        toolName: 'run_terminal_command',
        toolInput: { command: 'npm test' },
      },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(facts.isTestCommand, true)
    assert.equal(facts.isEdit, false)
  })
})
