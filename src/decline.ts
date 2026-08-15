// The markers a tool result carries when the developer declined or interrupted.
//
// Every marker here is APOSTROPHE FREE on purpose. Claude Code writes "the user
// doesn't want to proceed with this tool use", and inside a JSONL transcript that
// apostrophe may be a plain byte, a curly quote, or a `’` escape depending on
// who wrote the line. The transcript sweep matches on raw bytes and never decodes
// the value it is matching (design decision 57), so it cannot normalize escapes,
// and a marker that spans the apostrophe would match one spelling out of three.
// Matching the substring after it matches all three.
export const DECLINE_MARKERS = [
  'want to proceed with this tool use',
  'want to take this action',
  'the user does not want to',
  'user rejected',
  'user denied',
  'request interrupted by user',
  'tool use was rejected',
] as const

// Bounded on purpose, and bounded tightly.
//
// A decline marker is not somewhere in a decline, it is at the front of one: the
// sentence is the whole response. A wide window turns any result that QUOTES the
// sentence into a rejection the developer never made, and this repository's own
// source is the easiest way to produce one. Measured over 4228 real tool results,
// a window this size found every decline and nothing else, where 4000 found six
// results that only mentioned one.
//
// A tool response can also be megabytes of file content that nothing here has
// any business walking through.
//
// The trade is deliberately asymmetric on the string path. `PostToolUse` mostly
// does not fire for a decline at all, so this is a backstop for the rare shapes
// where it does, and anything it misses the transcript sweep still finds at
// Stop under the same event id. A miss here costs nothing; a false rejection
// costs a number that is wrong in the flattering direction.
const SCAN_LIMIT = 256

const MARKER_BYTES = DECLINE_MARKERS.map((marker) => Buffer.from(marker, 'ascii'))

// The string path, used by the PostToolUse hook, where the payload is already a
// parsed object handed to this process by Claude Code.
export function looksDeclined(response: unknown): boolean {
  if (response === null || response === undefined) return false
  let text: string
  try {
    text = typeof response === 'string' ? response : JSON.stringify(response)
  } catch {
    return false
  }
  const head = text.slice(0, SCAN_LIMIT).toLowerCase()
  return DECLINE_MARKERS.some((marker) => head.includes(marker))
}

// The byte path, used by the transcript sweep. Case-insensitive ASCII comparison
// performed directly on the buffer: no slice, no toString, no string ever exists
// for the value being tested. This is what lets the sweep answer "was this
// declined" without reading the tool result.
export function bytesLookDeclined(buf: Buffer, start: number, end: number): boolean {
  const stop = Math.min(end, start + SCAN_LIMIT)
  for (const needle of MARKER_BYTES) {
    if (indexOfCaseless(buf, start, stop, needle) >= 0) return true
  }
  return false
}

function indexOfCaseless(buf: Buffer, start: number, end: number, needle: Buffer): number {
  const limit = end - needle.length
  const first = needle[0]!
  outer: for (let i = start; i <= limit; i++) {
    if (lower(buf[i]!) !== first) continue
    for (let j = 1; j < needle.length; j++) {
      if (lower(buf[i + j]!) !== needle[j]!) continue outer
    }
    return i
  }
  return -1
}

function lower(byte: number): number {
  return byte >= 0x41 && byte <= 0x5a ? byte + 0x20 : byte
}
