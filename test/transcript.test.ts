import test from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, statSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { sweepTranscript } from '../src/transcript.ts'
import { BUNDLE } from './helpers.ts'

// The transcript sweep is the only source of rejections in M1, and it is also the
// place a leak would be easiest: it reads a file full of prompts and code. What
// it returns is asserted here to be ids and class labels and nothing else.

const dir = mkdtempSync(join(tmpdir(), 'flueny-transcript-'))
const REPO = '/repo'

function writeTranscript(name: string, lines: unknown[]): string {
  const path = join(dir, name)
  writeFileSync(path, lines.map((line) => JSON.stringify(line)).join('\n') + '\n')
  return path
}

function toolUse(id: string, path: string): unknown {
  return {
    type: 'assistant',
    message: {
      role: 'assistant',
      content: [
        { type: 'text', text: 'PROMPTTEXT-let me update the tests' },
        { type: 'tool_use', id, name: 'Edit', input: { file_path: path, new_string: 'CODEBODY-secret' } },
      ],
    },
  }
}

function toolResult(id: string, content: string, isError = true): unknown {
  return {
    type: 'user',
    message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: id, is_error: isError, content }] },
  }
}

test('a declined tool result becomes one rejection with a path class', () => {
  const path = writeTranscript('one.jsonl', [
    toolUse('toolu_1', `${REPO}/src/pricing.test.ts`),
    toolResult('toolu_1', "The user doesn't want to proceed with this tool use."),
  ])
  const result = sweepTranscript(path, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
  assert.deepEqual(result.rejections, [{ toolUseId: 'toolu_1', pathClass: 'tests' }])
  assert.ok(result.offset > 0)

  // Nothing from the transcript survives the sweep except the id and the class.
  const serialized = JSON.stringify(result.rejections)
  assert.equal(serialized.includes('PROMPTTEXT'), false)
  assert.equal(serialized.includes('CODEBODY'), false)
  assert.equal(serialized.includes('pricing.test.ts'), false)
})

test('a successful tool result is not a rejection', () => {
  const path = writeTranscript('ok.jsonl', [
    toolUse('toolu_2', `${REPO}/src/app.ts`),
    toolResult('toolu_2', 'The file has been updated.', false),
  ])
  assert.deepEqual(
    sweepTranscript(path, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier }).rejections,
    [],
  )
})

test('the offset stops a rejection being reported on every later turn', () => {
  const path = writeTranscript('offset.jsonl', [
    toolUse('toolu_3', `${REPO}/src/auth/login.ts`),
    toolResult('toolu_3', 'The user rejected this edit.'),
  ])
  const first = sweepTranscript(path, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
  assert.equal(first.rejections.length, 1)
  assert.equal(first.rejections[0]?.pathClass, 'auth')

  const second = sweepTranscript(path, { offset: first.offset, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
  assert.deepEqual(second.rejections, [])
  assert.equal(second.offset, first.offset)
})

test('a transcript that shrank is re-read from the start rather than trusted', () => {
  const path = writeTranscript('rotated.jsonl', [
    toolUse('toolu_4', `${REPO}/README.md`),
    toolResult('toolu_4', 'The user does not want to take this action'),
  ])
  // An offset past the end of the file means the file was rotated or replaced.
  const result = sweepTranscript(path, { offset: 999_999, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
  assert.equal(result.rejections.length, 1)
  assert.equal(result.rejections[0]?.pathClass, 'docs')
})

test('a missing or unparseable transcript sweeps to nothing rather than throwing', () => {
  const missing = sweepTranscript(join(dir, 'nope.jsonl'), {
    offset: 12,
    repoRoot: REPO,
    classifier: BUNDLE.pathClassifier,
  })
  assert.deepEqual(missing.rejections, [])
  assert.equal(missing.offset, 12, 'a missing file must not reset the offset')

  const broken = writeTranscript('broken.jsonl', [])
  writeFileSync(broken, 'not json at all\n{"half":\n')
  assert.deepEqual(
    sweepTranscript(broken, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier }).rejections,
    [],
  )
})

test('one rejection is reported once even if the result appears twice', () => {
  const path = writeTranscript('dupe.jsonl', [
    toolUse('toolu_5', `${REPO}/src/app.ts`),
    toolResult('toolu_5', "The user doesn't want to proceed with this tool use."),
    toolResult('toolu_5', "The user doesn't want to proceed with this tool use."),
  ])
  assert.equal(
    sweepTranscript(path, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier }).rejections.length,
    1,
  )
})

// Design decision 57's condition, expressed as a test rather than a comment.
//
// The transcript came back into scope on one term: tool-use decision records
// only, and prompt text and assistant response text are never materialised, not
// even transiently, not even locally. "Never sent" was already covered by
// redaction.test.ts. This is the stronger claim: never READ.
//
// It works by watching the only two places bytes can become a value, a Buffer
// decode and a JSON parse, for the duration of one sweep, and failing if a
// sentinel planted in the message bodies ever appears in one. An implementation
// that decodes a line or parses a record fails this even though its return value
// would look perfectly clean.
test('the sweep never materialises prompt or response text, only ids and paths', () => {
  const secret = 'SENTINEL-e7b41f-NEVER-READ'
  const path = writeTranscript('never-read.jsonl', [
    {
      type: 'assistant',
      message: {
        role: 'assistant',
        content: [
          { type: 'text', text: `${secret} let me refactor the pricing module` },
          {
            type: 'tool_use',
            id: 'toolu_57',
            name: 'Edit',
            input: { file_path: `${REPO}/src/auth/session.ts`, new_string: `${secret} in a diff` },
          },
        ],
      },
    },
    {
      type: 'user',
      message: {
        role: 'user',
        content: [
          { type: 'text', text: `${secret} in a follow up prompt` },
          {
            type: 'tool_result',
            tool_use_id: 'toolu_57',
            is_error: true,
            content: `The user doesn't want to proceed with this tool use. ${secret}`,
          },
        ],
      },
    },
  ])

  const decoded: string[] = []
  const realToString = Buffer.prototype.toString
  const realParse = JSON.parse
  Buffer.prototype.toString = function (this: Buffer, ...args: unknown[]) {
    const out = (realToString as (...a: unknown[]) => string).apply(this, args)
    decoded.push(out)
    return out
  } as typeof Buffer.prototype.toString
  JSON.parse = ((text: string, reviver?: (k: string, v: unknown) => unknown) => {
    decoded.push(String(text))
    return realParse(text, reviver)
  }) as typeof JSON.parse

  let result
  try {
    result = sweepTranscript(path, { offset: 0, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
  } finally {
    Buffer.prototype.toString = realToString
    JSON.parse = realParse
  }

  // The sweep still has to work. A version that reads nothing would pass the
  // leak assertion below and be useless.
  assert.deepEqual(result.rejections, [{ toolUseId: 'toolu_57', pathClass: 'auth' }])

  const leaked = decoded.filter((value) => value.includes(secret))
  assert.deepEqual(
    leaked,
    [],
    `prompt or response text was materialised ${leaked.length} time(s): ${JSON.stringify(leaked.slice(0, 2))}`,
  )
})

// ---------------------------------------------------------------------------
// The shapes a real session produces.
//
// Everything above this line is built from fixtures written by this file, and a
// fixture written beside an implementation agrees with it by construction. The
// cases below were taken from 31 real Claude Code transcripts on the machine
// this was built on, and three of them failed when they were first run:
//
//   - a declined result whose `tool_use_id` is written BEFORE `"type"`, which is
//     the order real transcripts use for the majority of tool results
//   - a tool result that QUOTES the decline sentence, this repository's own
//     `decline.ts` being the easiest way to produce one
//   - a complete first line sitting exactly on the 8 MB tail boundary
//
// Key order in JSON is not a contract, so the sweep must not depend on one.
// ---------------------------------------------------------------------------

const DECLINE =
  "The user doesn't want to proceed with this tool use. The tool use was rejected " +
  '(eg. if it was a file edit, the new_string was NOT written to the file).'

// The exact block shape Claude Code writes, `caller` and all.
function realUse(id: string, path: string): unknown {
  return { type: 'tool_use', id, name: 'Edit', input: { file_path: path, old_string: 'a', new_string: 'b' }, caller: 'assistant' }
}

// A denial as Claude Code writes it today: `tool_use_id` last.
function idLast(id: string, content = DECLINE, isError = true): unknown {
  return { type: 'tool_result', content, is_error: isError, tool_use_id: id }
}

// The order every other tool result uses: `tool_use_id` first.
function idFirst(id: string, content = DECLINE, isError = true): unknown {
  return { tool_use_id: id, type: 'tool_result', content, is_error: isError }
}

function assistantLine(blocks: unknown[]): string {
  return JSON.stringify({ parentUuid: 'p', type: 'assistant', message: { role: 'assistant', content: blocks }, uuid: 'u' })
}

function userLine(blocks: unknown[]): string {
  return JSON.stringify({ parentUuid: 'p', type: 'user', message: { role: 'user', content: blocks }, uuid: 'u' })
}

function writeRaw(name: string, body: string): string {
  const path = join(dir, name)
  writeFileSync(path, body)
  return path
}

function sweep(path: string, offset = 0) {
  return sweepTranscript(path, { offset, repoRoot: REPO, classifier: BUNDLE.pathClassifier })
}

test('a decline is found whichever side of "type" the tool_use_id is written', () => {
  for (const [label, result] of [
    ['tool_use_id last', idLast('toolu_order')],
    ['tool_use_id first', idFirst('toolu_order')],
  ] as const) {
    const path = writeRaw(
      `order-${label.replace(/\W+/g, '-')}.jsonl`,
      assistantLine([realUse('toolu_order', `${REPO}/src/auth/session.ts`)]) + '\n' + userLine([result]) + '\n',
    )
    assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_order', pathClass: 'auth' }], label)
  }
})

test('a tool call and its decline on one line still pair up', () => {
  const path = writeRaw(
    'same-line.jsonl',
    userLine([realUse('toolu_same', `${REPO}/src/auth/login.ts`), idLast('toolu_same')]) + '\n',
  )
  assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_same', pathClass: 'auth' }])
})

test('a text block after the tool call does not swallow its id or its path', () => {
  const path = writeRaw(
    'text-after.jsonl',
    assistantLine([realUse('toolu_after', `${REPO}/src/app.test.ts`), { type: 'text', text: 'now I will run the tests' }]) +
      '\n' +
      userLine([idLast('toolu_after')]) +
      '\n',
  )
  assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_after', pathClass: 'tests' }])
})

test('an escaped quote, a backslash and non-ASCII in the path all classify', () => {
  for (const [name, path] of [
    ['quote', `${REPO}/src/we"ird/thing.test.ts`],
    ['backslash', `${REPO}/src/we\\ird/thing.test.ts`],
    ['unicode', `${REPO}/src/测试/спец/thing.test.ts`],
  ] as const) {
    const file = writeRaw(
      `escape-${name}.jsonl`,
      assistantLine([realUse(`toolu_${name}`, path)]) + '\n' + userLine([idLast(`toolu_${name}`)]) + '\n',
    )
    assert.deepEqual(sweep(file).rejections, [{ toolUseId: `toolu_${name}`, pathClass: 'tests' }], name)
  }
})

test('a several hundred KB tool result does not hide the decline after it', () => {
  const path = writeRaw(
    'huge-result.jsonl',
    assistantLine([realUse('toolu_huge', `${REPO}/src/auth/token.ts`)]) +
      '\n' +
      userLine([idFirst('toolu_bulk', 'x'.repeat(400_000), false)]) +
      '\n' +
      userLine([idLast('toolu_huge')]) +
      '\n',
  )
  assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_huge', pathClass: 'auth' }])
})

test('two declines on one line are two rejections, not one', () => {
  const path = writeRaw(
    'two-declines.jsonl',
    assistantLine([realUse('toolu_x', `${REPO}/src/auth/x.ts`), realUse('toolu_y', `${REPO}/README.md`)]) +
      '\n' +
      userLine([idLast('toolu_x'), idLast('toolu_y')]) +
      '\n',
  )
  assert.deepEqual(sweep(path).rejections, [
    { toolUseId: 'toolu_x', pathClass: 'auth' },
    { toolUseId: 'toolu_y', pathClass: 'docs' },
  ])
})

// The false-rejection half of design decision 46. Discernment is a rejection
// rate, so a result that MENTIONS a decline and is counted as one does not make
// the number noisy, it makes it wrong in the flattering direction.
test('a tool result that only quotes the decline sentence is not a rejection', () => {
  const quoted = "src/decline.ts:12:  'want to proceed with this tool use',\nsrc/decline.ts:16:  'user rejected',"
  for (const [label, result] of [
    ['grep output, id last', idLast('toolu_quote', quoted, false)],
    ['grep output, id first', idFirst('toolu_quote', quoted, false)],
  ] as const) {
    const path = writeRaw(
      `quoted-${label.replace(/\W+/g, '-')}.jsonl`,
      assistantLine([realUse('toolu_quote', `${REPO}/src/decline.ts`)]) + '\n' + userLine([result]) + '\n',
    )
    assert.deepEqual(sweep(path).rejections, [], label)
  }
})

// Both halves of that test, pinned separately, because each one alone lets a
// real false positive through and loosening either is a deliberate act.
test('a decline is an error result AND its content opens with the sentence', () => {
  const notAnError = writeRaw(
    'not-an-error.jsonl',
    assistantLine([realUse('toolu_ok', `${REPO}/src/auth/a.ts`)]) + '\n' + userLine([idLast('toolu_ok', DECLINE, false)]) + '\n',
  )
  assert.deepEqual(sweep(notAnError).rejections, [], 'is_error:false is not a decline however it reads')

  const buried = writeRaw(
    'buried.jsonl',
    assistantLine([realUse('toolu_buried', `${REPO}/src/auth/b.ts`)]) +
      '\n' +
      userLine([idLast('toolu_buried', `${'-'.repeat(600)}${DECLINE}`)]) +
      '\n',
  )
  assert.deepEqual(sweep(buried).rejections, [], 'the sentence 600 bytes in is a quotation, not a decline')
})

const MB8 = 8 * 1024 * 1024

// Pads `body` with one filler record so the whole string is exactly `total`
// bytes, which is how the tail boundary gets placed on a chosen byte.
function padTo(body: string, total: number): string {
  const bytes = (s: string) => Buffer.byteLength(s)
  const need = total - bytes(body)
  const line = (n: number) => JSON.stringify({ type: 'filler', text: 'f'.repeat(n) }) + '\n'
  const first = line(Math.max(0, need - 25))
  return body + line(Math.max(0, need - 25) + (need - bytes(first)))
}

test('a whole record sitting exactly on the 8 MB tail boundary is not thrown away', () => {
  const tail = padTo(userLine([idLast('toolu_edge')]) + '\n', MB8)
  const head = 'q'.repeat(199_999) + '\n'
  const path = writeRaw('boundary-exact.jsonl', head + tail)
  assert.equal(statSync(path).size - MB8, Buffer.byteLength(head), 'the cut must land on the first byte of the record')
  assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_edge', pathClass: null }])
})

test('a record the 8 MB tail cuts in half is skipped, and the next one is not', () => {
  const half = userLine([idLast('toolu_half')]) + '\n'
  const rest = padTo(half + userLine([idLast('toolu_whole')]) + '\n', MB8 + Math.floor(Buffer.byteLength(half) / 2))
  const path = writeRaw('boundary-partial.jsonl', 'q'.repeat(199_999) + '\n' + rest)
  const into = statSync(path).size - MB8 - 200_000
  assert.ok(into > 0 && into < Buffer.byteLength(half), `the cut must land inside the first record, was ${into}`)
  assert.deepEqual(sweep(path).rejections, [{ toolUseId: 'toolu_whole', pathClass: null }])
})

// The stronger version of the "never materialises" test above.
//
// That one is three lines long, which is a weak witness for a claim about a file
// full of prompts and diffs. This one is a session at working scale: 400 turns,
// thinking blocks, multi-block messages, a 300 KB tool result, and a sentinel in
// every message body. It asserts two things rather than one. Nothing decoded
// contains the sentinel, and every value that came into existence is a bounded
// scalar: no newline, no length beyond the path bound. A decode of a line, or of
// a record, fails the second assertion even if the sentinel happened to miss it.
//
// The same audit was run against 31 real Claude Code transcripts on the machine
// this was built on, 89.9 MB in total: 10524 values materialised, the longest
// 192 characters, every one of them an id or a path.
test('at session scale, only bounded scalars are ever decoded', () => {
  const secret = 'SENTINEL-3c9d02-NEVER-READ'
  const lines: string[] = []
  for (let turn = 0; turn < 400; turn++) {
    lines.push(
      assistantLine([
        { type: 'thinking', thinking: `${secret} weighing the options for turn ${turn}`, signature: 'sig' },
        { type: 'text', text: `${secret} here is what I am going to do, at some length. ${'reasoning '.repeat(40)}` },
        realUse(`toolu_scale_${turn}`, `${REPO}/src/module-${turn}/thing.ts`),
      ]),
    )
    lines.push(
      userLine([
        idFirst(`toolu_scale_${turn}`, `${secret} ${'file contents that must never be read '.repeat(turn === 7 ? 8000 : 30)}`, false),
      ]),
    )
  }
  lines.push(assistantLine([realUse('toolu_scale_declined', `${REPO}/src/auth/session.ts`)]))
  lines.push(userLine([{ type: 'text', text: `${secret} no thanks` }, idLast('toolu_scale_declined')]))

  const path = writeRaw('at-scale.jsonl', lines.join('\n') + '\n')
  assert.ok(statSync(path).size > 1_000_000, 'the fixture has to be big enough to be a real witness')

  const decoded: string[] = []
  const realToString = Buffer.prototype.toString
  const realParse = JSON.parse
  Buffer.prototype.toString = function (this: Buffer, ...args: unknown[]) {
    const out = (realToString as (...a: unknown[]) => string).apply(this, args)
    decoded.push(out)
    return out
  } as typeof Buffer.prototype.toString
  JSON.parse = ((text: string, reviver?: (k: string, v: unknown) => unknown) => {
    decoded.push(String(text))
    return realParse(text, reviver)
  }) as typeof JSON.parse

  let result
  try {
    result = sweep(path)
  } finally {
    Buffer.prototype.toString = realToString
    JSON.parse = realParse
  }

  assert.deepEqual(result.rejections, [{ toolUseId: 'toolu_scale_declined', pathClass: 'auth' }])
  assert.ok(decoded.length > 0, 'a sweep that decoded nothing at all is not evidence of anything')

  const leaked = decoded.filter((value) => value.includes(secret))
  assert.deepEqual(leaked, [], `message text was materialised ${leaked.length} time(s)`)

  const unbounded = decoded.filter((value) => value.includes('\n') || value.length > 4096)
  assert.deepEqual(
    unbounded.map((value) => value.length),
    [],
    'every value the sweep materialises must be one bounded scalar, never a line or a record',
  )
})
