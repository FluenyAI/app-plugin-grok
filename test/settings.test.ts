import test from 'node:test'
import assert from 'node:assert/strict'
import { settingsFragment } from '../src/settings.ts'

// The hook wiring. `type: "command"` is the load-bearing part of this file, so it
// is the load-bearing part of this test: an http hook would post the raw payload,
// which is the wrong shape for /events and would put tool_input and tool_response
// off the machine.

test('every hook is type command, never type http', () => {
  const fragment = settingsFragment('/opt/flueny/cli.ts', '/usr/bin/node')
  const commands = Object.values(fragment.hooks).flatMap((matchers) => matchers.flatMap((m) => m.hooks))
  assert.equal(commands.length, 4)
  for (const command of commands) {
    assert.equal(command.type, 'command')
    assert.ok(command.timeout > 0, 'an unbounded hook is an editor that can hang')
  }
  assert.equal(JSON.stringify(fragment).includes('"http"'), false)
})

test('the four M1 hooks are registered, and no gate is', () => {
  const fragment = settingsFragment('/opt/flueny/cli.ts', '/usr/bin/node')
  assert.deepEqual(Object.keys(fragment.hooks).sort(), ['PostToolUse', 'SessionEnd', 'SessionStart', 'Stop'])
  // PreToolUse is the gate. It is M3, canEnforce is false for every agent in M1,
  // and a stubbed gate would read as "coming soon" to anyone reading the config.
  assert.equal(JSON.stringify(fragment).includes('PreToolUse'), false)
})

test('PostToolUse matches every tool, not just the editing ones', () => {
  // Delegation counts Task calls and Diligence needs to see a test command run,
  // so a matcher naming only Edit and Write would starve two competencies.
  const fragment = settingsFragment('/opt/flueny/cli.ts', '/usr/bin/node')
  assert.equal(fragment.hooks.PostToolUse[0]?.matcher, '*')
})

test('a path with a space survives the shell', () => {
  const fragment = settingsFragment('/Users/x/Flueny AI/plugin/src/cli.ts', '/usr/bin/node')
  const command = fragment.hooks.SessionStart[0]?.hooks[0]?.command ?? ''
  assert.equal(command, '/usr/bin/node "/Users/x/Flueny AI/plugin/src/cli.ts" hook session-start')
})

test('each hook is told which event it is handling', () => {
  const fragment = settingsFragment('/opt/flueny/cli.ts', '/usr/bin/node')
  const events = Object.values(fragment.hooks)
    .flatMap((matchers) => matchers.flatMap((m) => m.hooks))
    .map((h) => h.command.split(' ').pop())
  assert.deepEqual(events.sort(), ['post-tool-use', 'session-end', 'session-start', 'stop'])
})
