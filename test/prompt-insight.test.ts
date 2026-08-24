import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { sweepPromptInsightTurns } from '../src/prompt-insight.ts'

function writeTranscript(lines: unknown[]): string {
  const dir = mkdtempSync(join(tmpdir(), 'flueny-insight-test-'))
  const path = join(dir, 'transcript.jsonl')
  writeFileSync(path, lines.map((line) => JSON.stringify(line)).join('\n') + '\n')
  return path
}

function userText(text: string): unknown {
  return { type: 'user', message: { role: 'user', content: [{ type: 'text', text }] } }
}

function userTextString(text: string): unknown {
  return { type: 'user', message: { role: 'user', content: text } }
}

function toolResultTurn(id: string): unknown {
  return {
    type: 'user',
    message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, is_error: false, content: 'ok' }] },
  }
}

function assistantText(text: string): unknown {
  return { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text }] } }
}

function assistantToolUse(id: string): unknown {
  return {
    type: 'assistant',
    message: { role: 'assistant', content: [{ type: 'tool_use', id, name: 'Edit', input: { file_path: 'x' } }] },
  }
}

test('pairs a real prompt with the assistant text that follows it', () => {
  const path = writeTranscript([userText('fix the login bug'), assistantText('Fixed it in auth.ts')])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [{ prompt: 'fix the login bug', response: 'Fixed it in auth.ts' }])
  assert.equal(sweep.lineOffset, 2)
})

test('accepts content as a plain string, not only a block array', () => {
  const path = writeTranscript([userTextString('fix the login bug'), assistantText('done')])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [{ prompt: 'fix the login bug', response: 'done' }])
})

test('a tool_result feedback turn is never counted as a prompt', () => {
  // The Messages API shape: a tool result comes back to the model as a
  // role:"user" message, same as a real prompt. Counting it would send the
  // agent's own tool output back to Flueny, not the developer's words.
  const path = writeTranscript([
    userText('refactor the billing module'),
    assistantToolUse('toolu_1'),
    toolResultTurn('toolu_1'),
    assistantText('Refactored billing.ts, all set'),
  ])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [{ prompt: 'refactor the billing module', response: 'Refactored billing.ts, all set' }])
})

test('joins multiple assistant text blocks across a multi-step turn into one response', () => {
  const path = writeTranscript([
    userText('add tests for the auth module'),
    assistantText('Let me look at the existing tests first.'),
    assistantToolUse('toolu_1'),
    toolResultTurn('toolu_1'),
    assistantText('Added three cases covering the token refresh path.'),
  ])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [
    {
      prompt: 'add tests for the auth module',
      response: 'Let me look at the existing tests first.\nAdded three cases covering the token refresh path.',
    },
  ])
})

test('a prompt whose turn produced no assistant text yields nothing, not an empty response', () => {
  const path = writeTranscript([userText('just tool calls, no reply text'), assistantToolUse('toolu_1')])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [])
  // Still advances: the line was read, and a re-sweep from 0 would just find
  // the same nothing again.
  assert.equal(sweep.lineOffset, 2)
})

test('the offset skips lines already swept, same shape as the rejection sweep', () => {
  const path = writeTranscript([userText('first prompt'), assistantText('first reply')])
  const first = sweepPromptInsightTurns(path, 0)
  assert.equal(first.turns.length, 1)

  writeFileSync(path, '', { flag: 'a' }) // no-op, keeps the file as is
  const second = sweepPromptInsightTurns(path, first.lineOffset)
  assert.deepEqual(second.turns, [], 're-sweeping from the returned offset must not repeat a turn')
})

test('a missing transcript sweeps to nothing rather than throwing', () => {
  const sweep = sweepPromptInsightTurns('/does/not/exist.jsonl', 0)
  assert.deepEqual(sweep, { turns: [], lineOffset: 0 })
})

test('an unparseable line is skipped, not fatal to the rest of the sweep', () => {
  const dir = mkdtempSync(join(tmpdir(), 'flueny-insight-test-'))
  const path = join(dir, 'transcript.jsonl')
  writeFileSync(
    path,
    [JSON.stringify(userText('a real prompt')), 'not json at all', JSON.stringify(assistantText('a real reply'))].join(
      '\n',
    ) + '\n',
  )
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [{ prompt: 'a real prompt', response: 'a real reply' }])
})

test('two prompts in one sweep produce two turns, matched in order', () => {
  const path = writeTranscript([
    userText('first task'),
    assistantText('did the first thing'),
    userText('second task'),
    assistantText('did the second thing'),
  ])
  const sweep = sweepPromptInsightTurns(path, 0)
  assert.deepEqual(sweep.turns, [
    { prompt: 'first task', response: 'did the first thing' },
    { prompt: 'second task', response: 'did the second thing' },
  ])
})
