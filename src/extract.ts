import { relative, isAbsolute } from 'node:path'
import { classifyPath } from './classify.ts'
import { looksDeclined } from './decline.ts'
import type { CodingCommandCategory, CodingToolCategory } from './types.ts'

// Local extraction. This is the point of the whole product.
//
// Everything Claude Code hands a hook arrives here: the prompt-shaped fields, the
// tool arguments, the tool result. What leaves this module by default is a
// `ToolFacts`: a handful of booleans, one opaque id and one short class label.
// There is no field on `ToolFacts` that can hold a diff, file contents, a tool
// response body or an environment variable -- that discard is structural and
// unconditional, not a promise about calling code, and nothing in this file
// changes it.
//
// Feature 0109 is the one deliberate exception: when the developer has
// explicitly opted into `rawActivityEnabled` (resolved server-side, threaded in
// as `opts.includeRaw`), extraction is ALSO allowed to return exactly two
// bounded strings: the repo-relative file path (`rawPath`, the same value
// `pathClass` is already derived from) and the Bash command text (`rawCommand`,
// only for a tool this file already classifies as `toolCategory: 'bash'`), each
// truncated to MAX_RAW_LENGTH. `includeRaw` defaults to false, so a caller that
// forgets to pass it gets the original, fully structural behaviour.
//
// The raw payload is never written to disk and never returned from these
// functions. test/redaction.test.ts drives a payload stuffed with markers through
// extraction and serialization and fails if any of them survives -- including,
// with `includeRaw: true`, a positive-case assertion that ONLY the real path and
// the real command text cross that boundary, and nothing else ever does.

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
  // Feature 0108. A coarse bucket for which tool ran, so a developer glancing at
  // the live feed sees "Edit" or "Bash" instead of an undifferentiated "Tool
  // use". Six fixed values only, same reasoning as pathClass: a reviewable set,
  // never the raw tool name.
  toolCategory: CodingToolCategory
  // Feature 0108. Only meaningful when toolCategory is 'bash'; null otherwise.
  // Reuses the same conservative TEST_COMMAND detection isTestCommand already
  // does, so this is a rename of an existing signal for that one category, not
  // a new detector.
  commandCategory: CodingCommandCategory | null
  // Feature 0109. Present ONLY when opts.includeRaw was true AND a non-empty
  // repo-relative path exists. Absent, not null: absence is the only way this
  // shape can mean "no raw path available", so a bug cannot forward `null` and
  // have it misread as "raw data was requested but there was none".
  rawPath?: string
  // Feature 0109. Present ONLY when opts.includeRaw was true AND toolCategory
  // is 'bash' AND a command string exists. Same absent-not-null discipline as
  // rawPath.
  rawCommand?: string
}

// Feature 0109. Both rawPath and rawCommand are truncated to this length before
// they ever leave extractToolFacts, so a pathologically long input cannot turn
// this opt-in channel into an exfiltration vector for more than a bounded
// amount of text per tool call.
const MAX_RAW_LENGTH = 500

function truncate(value: string, max: number): string {
  return value.length > max ? value.slice(0, max) : value
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
const READ_TOOLS = new Set(['read', 'read_file', 'cat', 'view'])
const BASH_TOOLS = new Set(['bash', 'shell', 'terminal', 'run_terminal_cmd', 'run_terminal_command'])
const SEARCH_TOOLS = new Set(['grep', 'glob', 'search', 'find', 'search_files', 'codebase_search', 'ls'])
const WEB_TOOLS = new Set(['webfetch', 'web_fetch', 'websearch', 'web_search', 'browse'])

// isEdit is checked first: EDIT_TOOLS and BASH_TOOLS/SEARCH_TOOLS/etc are
// disjoint by construction, but a tool that somehow matched two sets should
// still resolve to the more specific, already-computed classification rather
// than whichever set this function happens to check first.
function classifyTool(lower: string, isEdit: boolean): CodingToolCategory {
  if (isEdit) return 'edit'
  if (BASH_TOOLS.has(lower)) return 'bash'
  if (READ_TOOLS.has(lower)) return 'read'
  if (SEARCH_TOOLS.has(lower)) return 'search'
  if (WEB_TOOLS.has(lower)) return 'web'
  return 'other'
}

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
  opts: { repoRoot: string | null; classifier: Record<string, string[]>; includeRaw?: boolean },
): ToolFacts {
  const toolName = firstString(payload, ['tool_name', 'toolName']) ?? ''
  const lower = toolName.toLowerCase()
  const input = isRecord(payload.tool_input)
    ? payload.tool_input
    : isRecord(payload.toolInput)
      ? payload.toolInput
      : {}

  const filePath = firstString(input, ['file_path', 'notebook_path', 'path', 'filePath'])
  const repoRelativePath = filePath !== null ? toRepoRelative(filePath, opts.repoRoot) : null
  const pathClass = repoRelativePath !== null ? classifyPath(opts.classifier, repoRelativePath) : null

  const command = firstString(input, ['command'])
  const isEdit = EDIT_TOOLS.has(lower)
  const isTestCommand = command !== null && TEST_COMMAND.test(command)
  const toolCategory = classifyTool(lower, isEdit)
  const includeRaw = opts.includeRaw ?? false

  return {
    toolUseId: firstString(payload, ['tool_use_id', 'toolUseId']),
    kind: SUBAGENT_TOOLS.has(lower) ? 'subagent' : 'tool-use',
    isEdit,
    isTestCommand,
    pathClass,
    declined: looksDeclined(payload.tool_response ?? payload.toolResult),
    toolCategory,
    commandCategory: toolCategory === 'bash' ? (isTestCommand ? 'test' : 'other') : null,
    ...(includeRaw && repoRelativePath ? { rawPath: truncate(repoRelativePath, MAX_RAW_LENGTH) } : {}),
    ...(includeRaw && toolCategory === 'bash' && command !== null
      ? { rawCommand: truncate(command, MAX_RAW_LENGTH) }
      : {}),
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
