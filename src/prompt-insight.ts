import { readFileSync } from 'node:fs'

// Feature 0094. Reads real prompt and reply text from the transcript, for a
// developer who has opted into prompt insight scoring (or whose org enforces
// it on). Gated entirely by the caller (hooks.ts#onStop checks
// SessionState.promptInsightsEnabled before this is ever called): nothing
// here decides whether to run, only how to read once running is already
// decided.
//
// This is deliberately NOT a change to sweepTranscript (transcript.ts,
// design decision 57). That function's whole point is that it never decodes
// a line, enforced by test/transcript.test.ts's Buffer/JSON.parse spy, and it
// stays that way for every developer who has not opted in. This is a
// separate function with a separate, generous budget: decoding the two
// things a developer has consented to sending, nothing else.
//
// A user turn only counts as a real prompt when its content holds no
// tool_result block. A tool result comes back to the model as a
// `role: "user"` message too (the Messages API shape, see the toolUse/
// toolResult fixtures in test/transcript.test.ts), and counting one as a
// prompt would send an agent's own tool output back to Flueny, not the
// developer's words.

const MAX_READ_BYTES = 8 * 1024 * 1024

interface TranscriptLine {
  type?: string
  message?: {
    role?: string
    content?: unknown
  }
}

interface ContentBlock {
  type?: string
  text?: string
}

export interface PromptInsightTurn {
  prompt: string
  response: string
}

export interface PromptInsightSweep {
  turns: PromptInsightTurn[]
  // Lines already scanned, not bytes: a JSONL file's lines are the only unit
  // that can be skipped without re-parsing, and counting them avoids the
  // partial-line bookkeeping sweepTranscript needs for its byte-tail read.
  lineOffset: number
}

export function sweepPromptInsightTurns(transcriptPath: string, lineOffset: number): PromptInsightSweep {
  let raw: string
  try {
    raw = readFileSync(transcriptPath, { encoding: 'utf8' })
  } catch {
    return { turns: [], lineOffset }
  }
  if (Buffer.byteLength(raw, 'utf8') > MAX_READ_BYTES) {
    // A transcript this large on an opt-in, decode-everything path is not
    // worth reading whole. Skip to the end rather than spend the turn on it;
    // the next Stop picks up from here with nothing to catch up on.
    const lines = raw.split('\n')
    return { turns: [], lineOffset: lines.length }
  }

  const lines = raw.split('\n').filter((line) => line.length > 0)
  const start = lineOffset > lines.length ? 0 : lineOffset
  const turns: PromptInsightTurn[] = []

  let pendingPrompt: string | null = null
  let pendingResponseParts: string[] = []

  const flush = () => {
    if (pendingPrompt !== null) {
      const response = pendingResponseParts.join('\n').trim()
      if (response.length > 0) turns.push({ prompt: pendingPrompt, response })
      // A prompt whose turn produced no assistant text (tool calls only, or
      // the turn is still in flight) contributes nothing this sweep. It is
      // not lost: the next Stop re-scans from the same line offset only if
      // this function never advances past it, and it does advance, so an
      // all-tool-calls turn is simply never scored, matching how a prompt
      // that gets no reply is not a "quality" question this rubric can answer.
    }
    pendingPrompt = null
    pendingResponseParts = []
  }

  for (let i = start; i < lines.length; i++) {
    const parsed = parseLine(lines[i]!)
    if (!parsed) continue

    if (parsed.type === 'user') {
      const prompt = realUserPrompt(parsed)
      if (prompt === null) continue // synthetic tool-result feedback, not a human prompt
      flush()
      pendingPrompt = prompt
      continue
    }

    if (parsed.type === 'assistant' && pendingPrompt !== null) {
      const text = assistantText(parsed)
      if (text) pendingResponseParts.push(text)
    }
  }
  flush()

  return { turns, lineOffset: lines.length }
}

function parseLine(line: string): TranscriptLine | null {
  try {
    return JSON.parse(line) as TranscriptLine
  } catch {
    return null // a torn final line from a still-being-written transcript
  }
}

function realUserPrompt(line: TranscriptLine): string | null {
  const content = line.message?.content
  if (typeof content === 'string') {
    const trimmed = content.trim()
    return trimmed.length > 0 ? trimmed : null
  }
  if (!Array.isArray(content)) return null
  const blocks = content as ContentBlock[]
  if (blocks.some((b) => b?.type === 'tool_result')) return null
  const text = blocks
    .filter((b) => b?.type === 'text' && typeof b.text === 'string')
    .map((b) => b.text as string)
    .join('\n')
    .trim()
  return text.length > 0 ? text : null
}

function assistantText(line: TranscriptLine): string | null {
  const content = line.message?.content
  if (typeof content === 'string') return content.trim() || null
  if (!Array.isArray(content)) return null
  const blocks = content as ContentBlock[]
  const text = blocks
    .filter((b) => b?.type === 'text' && typeof b.text === 'string')
    .map((b) => b.text as string)
    .join('\n')
    .trim()
  return text.length > 0 ? text : null
}
