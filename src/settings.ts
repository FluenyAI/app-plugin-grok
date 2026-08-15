// The Claude Code settings fragment that wires this client in.
//
// Every hook is `type: "command"`. This is not a preference.
//
// A hook of type "http" posts the raw hook payload to a URL with no local code in
// between. Pointed at `/integrations/coding/events` it would fail twice over: the
// body is not a `CodingEventBatch`, so ingest would drop it as malformed and
// still answer 202, and the body it did post would contain `tool_input` and
// `tool_response`, which are the prompt text, the code and the file contents this
// product promises never leave the machine (CEO decisions 8A and 33A, eng
// findings 13 and 15).
//
// `type: "command"` costs a process spawn per invocation, which is the reason the
// shipped client is a native binary and this spike is not. See the README.

export type HookEvent = 'SessionStart' | 'PostToolUse' | 'Stop' | 'SessionEnd'

export interface HookCommand {
  type: 'command'
  command: string
  timeout: number
}

export interface HookMatcher {
  matcher?: string
  hooks: HookCommand[]
}

export interface SettingsFragment {
  hooks: Record<HookEvent, HookMatcher[]>
}

// A hook that hangs is an editor that hangs, so every one of them is bounded well
// under the time a developer would notice. The client's own network calls are
// bounded tighter still (see api.ts), so this timeout is the backstop, not the
// mechanism.
const TIMEOUT_SECONDS = 10

export function settingsFragment(scriptPath: string, nodePath = process.execPath): SettingsFragment {
  const run = (event: string): HookCommand => ({
    type: 'command',
    command: `${quote(nodePath)} ${quote(scriptPath)} hook ${event}`,
    timeout: TIMEOUT_SECONDS,
  })
  return {
    hooks: {
      SessionStart: [{ hooks: [run('session-start')] }],
      // Every tool, not just the editing ones. Delegation is counted from Task
      // calls and Diligence needs to see a test command run, so a matcher that
      // named only Edit and Write would starve two of the three competencies.
      PostToolUse: [{ matcher: '*', hooks: [run('post-tool-use')] }],
      Stop: [{ hooks: [run('stop')] }],
      SessionEnd: [{ hooks: [run('session-end')] }],
    },
  }
}

// Paths on a developer's machine contain spaces far more often than anyone
// designing a shell command line expects, and this repository lives in one.
function quote(value: string): string {
  return /[^A-Za-z0-9_@%+=:,./-]/.test(value) ? `"${value.replace(/(["\\$`])/g, '\\$1')}"` : value
}
