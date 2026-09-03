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
    assert.equal(facts.toolCategory, 'edit')
    assert.equal(facts.commandCategory, null)
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
    assert.equal(facts.toolCategory, 'other')
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
    assert.equal(facts.toolCategory, 'bash')
    assert.equal(facts.commandCategory, 'test')
  })

  it('classifies a non-test Bash command as bash/other', () => {
    const facts = extractToolFacts(
      { toolName: 'Bash', toolInput: { command: 'git status' } },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(facts.toolCategory, 'bash')
    assert.equal(facts.commandCategory, 'other')
  })

  it('classifies Read as read', () => {
    const facts = extractToolFacts(
      { toolName: 'Read', toolInput: { file_path: '/repo/src/index.ts' } },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(facts.toolCategory, 'read')
    assert.equal(facts.commandCategory, null)
  })

  it('classifies Grep and Glob as search', () => {
    const grep = extractToolFacts(
      { toolName: 'Grep', toolInput: { pattern: 'foo' } },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    const glob = extractToolFacts(
      { toolName: 'Glob', toolInput: { pattern: '**/*.ts' } },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(grep.toolCategory, 'search')
    assert.equal(glob.toolCategory, 'search')
  })

  it('classifies WebFetch as web and an unrecognized tool as other', () => {
    const web = extractToolFacts(
      { toolName: 'WebFetch', toolInput: { url: 'https://example.com' } },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    const unknown = extractToolFacts(
      { toolName: 'mcp__some_server__do_thing', toolInput: {} },
      { repoRoot: '/repo', classifier: BUNDLE.pathClassifier },
    )
    assert.equal(web.toolCategory, 'web')
    assert.equal(unknown.toolCategory, 'other')
  })
})
