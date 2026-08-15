import { relative, isAbsolute } from 'node:path'
import { classifyPath } from './classify.ts'
import { looksDeclined } from './decline.ts'

// Local extraction. This is the point of the whole product.
//
// Everything Claude Code hands a hook arrives here: the prompt-shaped fields, the
// tool arguments, the tool result. What leaves this module is a `ToolFacts`: a
// handful of booleans, one opaque id and one short class label. There is no field
// on `ToolFacts` that can hold a path, a command, a diff or a response body, so
// the discard is structural rather than a promise about calling code.
//
// The raw payload is never written to disk and never returned from these
// functions. test/redaction.test.ts drives a payload stuffed with markers through
// extraction and serialization and fails if any of them survives.

export type RawPayload = Record<string, unknown>

export interface ToolFacts {
  // Dedupe key material only. Claude Code does not put a tool_use_id on every
  // PostToolUse payload, so this is null more often than the contract implies and
  // the caller synthesizes a session-scoped id. See the README's contract notes.
  toolUseId: string | null
  kind: 'tool-use' | 'subagent'
  isEdit: boolean
  isTestCommand: boolean
  pathClass: string | null
  // A tool_response that says the developer declined. PostToolUse mostly does not
  // fire in that case, so this catches the shapes where it does and the
  // transcript sweep at Stop catches the rest.
  declined: boolean
}

const EDIT_TOOLS = new Set([
  'edit',
  'write',
  'multiedit',
  'notebookedit',
  'applypatch',
  'update',
  'search_replace', // Grok's name for Edit / Write / MultiEdit
])
const SUBAGENT_TOOLS = new Set(['task', 'spawn_subagent'])

// Deliberately conservative. A false positive here marks an accept as checked
// when it was not, which inflates Diligence, and an inflated score is worse than
// a missing one on a surface whose whole claim is that it does not judge you.
const TEST_COMMAND = new RegExp(
  [
    String.raw`\b(npm|pnpm|yarn|bun)\s+(run\s+)?tests?\b`,
    String.raw`\bnpx?\s+(jest|vitest|mocha|ava|playwright|cypress)\b`,
    String.raw`\b(jest|vitest|pytest|tox|rspec|phpunit)\b`,
    String.raw`\bgo\s+test\b`,
    String.raw`\bcargo\s+test\b`,
    String.raw`\bmvn\s+(\S+\s+)*test\b`,
    String.raw`\bgradle(w)?\s+(\S+\s+)*test\b`,
    String.raw`\bdotnet\s+test\b`,
    String.raw`\bnode\s+--test\b`,
    String.raw`\bmake\s+test\b`,
  ].join('|'),
  'i',
)

// The decline markers live in decline.ts, which is the single home for them.
//
// There were two lists. This one matched a stringified response here, and the
// transcript sweep needed the same knowledge against raw bytes. Two lists of the
// same product decision on somebody else's release train is a list that drifts,
// and the half that drifts silently is the one the rejection rate depends on.

export function extractToolFacts(
  payload: RawPayload,
  opts: { repoRoot: string | null; classifier: Record<string, string[]> },
): ToolFacts {
  const toolName = firstString(payload, ['tool_name', 'toolName']) ?? ''
  const lower = toolName.toLowerCase()
  const input = isRecord(payload.tool_input)
    ? payload.tool_input
    : isRecord(payload.toolInput)
      ? payload.toolInput
      : {}

  const rawPath = firstString(input, ['file_path', 'notebook_path', 'path', 'filePath'])
  const pathClass = rawPath ? classifyPath(opts.classifier, toRepoRelative(rawPath, opts.repoRoot)) : null

  const command = firstString(input, ['command'])

  return {
    toolUseId: firstString(payload, ['tool_use_id', 'toolUseId']),
    kind: SUBAGENT_TOOLS.has(lower) ? 'subagent' : 'tool-use',
    isEdit: EDIT_TOOLS.has(lower),
    isTestCommand: command !== null && TEST_COMMAND.test(command),
    pathClass,
    declined: looksDeclined(payload.tool_response ?? payload.toolResult),
  }
}

// A path outside the repository classifies as nothing. The alternative, feeding
// an absolute path into a classifier whose patterns are repo-relative, produces
// confident nonsense: `/Users/someone/auth-notes.md` would score as `auth`.
export function toRepoRelative(path: string, repoRoot: string | null): string {
  if (!repoRoot) return isAbsolute(path) ? '' : path
  if (!isAbsolute(path)) return path
  const rel = relative(repoRoot, path)
  return rel.startsWith('..') ? '' : rel
}

// Re-exported so the import path callers already use keeps working. The
// implementation, and the marker list behind it, are in decline.ts.
export { looksDeclined } from './decline.ts'

function firstString(source: RawPayload, keys: string[]): string | null {
  for (const key of keys) {
    const value = source[key]
    if (typeof value === 'string' && value.length > 0) return value
  }
  return null
}

function isRecord(value: unknown): value is RawPayload {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
