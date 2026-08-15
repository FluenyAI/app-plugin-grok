import test from 'node:test'
import assert from 'node:assert/strict'
import {
  COLUMNS,
  capabilityUnlock,
  dailyReceipt,
  denial,
  nudge,
  setupConnected,
  weeklySummary,
} from '../src/copy.ts'

// The voice rules from `### Terminal copy` in the design plan, as assertions.
// The section says every one of them is testable in review, so they are tested
// here instead: a review catches them once, a test catches them every time.

const ALL = (): string[] => [
  setupConnected({ dryRun: true, dryRunDays: 7, appUrl: 'https://app.flueny.ai' }),
  setupConnected({ dryRun: false, dryRunDays: 0, appUrl: 'https://app.flueny.ai' }),
  dailyReceipt({ toolCalls: 41, signals: 6, blocked: 0 }),
  nudge({ finding: 'this concatenates user input into SQL.', standard: 'SEC-12', url: 'https://app.flueny.ai/knowledge/sec-12' }),
  denial({
    reason: 'it writes outside the approved repository scope.',
    rule: 'Repository scope (org standard SEC-12)',
    url: 'https://app.flueny.ai/knowledge/sec-12',
    token: 'a3f21c',
  }),
  weeklySummary({ rejected: 3, decisions: 11, touchedTests: 2, appUrl: 'https://app.flueny.ai' }),
  capabilityUnlock({ pathClass: 'infra' }),
]

test('no rendered line exceeds 80 columns', () => {
  for (const block of ALL()) {
    for (const line of block.split('\n')) {
      assert.ok(line.length <= COLUMNS, `${line.length} columns: ${line}`)
    }
  }
})

test('every string is ASCII, with no emoji and no box drawing', () => {
  for (const block of ALL()) {
    assert.ok(/^[\x20-\x7e\n]*$/.test(block), block)
  }
})

test('no em dashes and no en dashes anywhere', () => {
  for (const block of ALL()) {
    assert.equal(/[–—]/.test(block), false, block)
  }
})

test('no praise and no adjectives of judgement', () => {
  // "The fact is the compliment." A tool that does not judge you is easier to
  // believe when it says it is not watching you.
  const praise = /\b(great|nice|excellent|good job|well done|awesome|impressive|amazing|perfect|strong|poor|bad)\b/i
  for (const block of ALL()) {
    assert.equal(praise.test(block), false, block)
  }
})

test('second person for the developer, and never "we"', () => {
  for (const block of ALL()) {
    assert.equal(/\bwe\b|\bour\b|\bus\b/i.test(block), false, block)
  }
})

test('the words are the design plan exemplars, reflowed but not rewritten', () => {
  const flat = (text: string): string => text.split('\n').join(' ')

  assert.equal(
    flat(setupConnected({ dryRun: true, dryRunDays: 7, appUrl: '<url>' })),
    'Flueny is connected. Dry run is on for 7 days: nothing is scored, nothing is blocked. ' +
      'Your prompts and your code never leave this machine. See what is sent: <url>/coding/privacy',
  )

  // Design decision 58 corrected this one. The plan's original exemplar read
  // "observed 41 tool calls, would have sent 6 signals", which implies signals
  // are a filtered subset of calls. One PostToolUse yields a tool-use event and,
  // on an edit, an edit-decision event too, so signals run at about twice tool
  // calls: the live receipt printed 27 and 50. The numbers here are that measured
  // pair rather than an invented one, so the exemplar cannot drift back into
  // describing a model the client does not have.
  assert.equal(
    flat(dailyReceipt({ toolCalls: 27, signals: 50, blocked: 0 })),
    'Flueny observed 27 tool calls today and sent 50 derived signals. ' +
      'It blocked 0 actions. No prompt, no code and no file content left this machine. ' +
      'See exactly what: flueny dry-run --today',
  )

  assert.equal(
    flat(weeklySummary({ rejected: 3, decisions: 11, touchedTests: 2, appUrl: '<url>' })),
    'You rejected 3 of 11 agent edits this week. Two of them touched tests. ' +
      'Full breakdown: <url>/coding/signal',
  )

  assert.equal(
    flat(
      denial({
        reason: 'it writes outside the approved repository scope.',
        rule: 'Repository scope (org standard SEC-12)',
        url: '<url>/knowledge/sec-12',
        token: 'a3f21c',
      }),
    ),
    'Flueny blocked this command because it writes outside the approved repository scope. ' +
      'Rule: Repository scope (org standard SEC-12) Why: <url>/knowledge/sec-12 ' +
      'Proceed anyway: flueny allow --once a3f21c (recorded)',
  )

  assert.equal(
    flat(capabilityUnlock({ pathClass: 'infra' })),
    'Flueny: agent writes to infra/ are now available on this account.',
  )
})

test('the denial is four parts, in the order the plan gives them', () => {
  const lines = denial({ reason: 'x.', rule: 'R', url: 'u', token: 't' }).split('\n')
  assert.equal(lines.length, 4)
  assert.match(lines[0] ?? '', /^Flueny blocked this command because/)
  assert.match(lines[1] ?? '', /^Rule:/)
  assert.match(lines[2] ?? '', /^Why:/)
  assert.match(lines[3] ?? '', /^Proceed anyway:/)
})

test('a number the product cannot substantiate is not printed', () => {
  // Zero rejections is a fact about rejections, not a fact about tests, so the
  // sentence about tests does not appear at all.
  const none = weeklySummary({ rejected: 0, decisions: 11, touchedTests: 0, appUrl: '<url>' })
  assert.equal(none.includes('touched tests'), false)
  assert.match(none, /You rejected 0 of 11 agent edits this week\./)

  // And the setup line states the window the server actually gave, not the 7
  // days the exemplar happens to use.
  assert.match(setupConnected({ dryRun: true, dryRunDays: 3, appUrl: '<url>' }), /on for 3 days/)
  assert.match(setupConnected({ dryRun: true, dryRunDays: 1, appUrl: '<url>' }), /on for 1 day:/)
  assert.match(setupConnected({ dryRun: false, dryRunDays: 0, appUrl: '<url>' }), /Dry run is over/)
})

test('singulars read as English', () => {
  assert.match(dailyReceipt({ toolCalls: 1, signals: 1, blocked: 0 }), /1 tool call today/)
  assert.match(dailyReceipt({ toolCalls: 1, signals: 1, blocked: 0 }), /sent 1 derived signal\./)
  assert.match(dailyReceipt({ toolCalls: 1, signals: 1, blocked: 1 }), /blocked 1 action\./)
})
