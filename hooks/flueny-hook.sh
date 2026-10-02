#!/bin/sh
# Runs one Flueny hook: `sh flueny-hook.sh <event>`, from hooks/hooks.json.
#
# The client is a native binary per platform, committed under bin/. This picks the
# one for this machine and execs it, so the hook costs one process, not a shell
# plus an interpreter.
#
# Every path fails open. A hook that exits non-zero, prints to stdout, or hangs is
# a hook that interferes with the developer's session, and this tool is not
# permitted to be in their way. No binary for this platform means no Flueny on
# this machine, silently, never an error in the editor.

ROOT="${CLAUDE_PLUGIN_ROOT:-${GROK_PLUGIN_ROOT:-}}"
[ -n "$ROOT" ] || ROOT="$(cd "$(dirname "$0")/.." 2>/dev/null && pwd)"
[ -n "$ROOT" ] || exit 0

# One uname for both halves: every process this wrapper spawns is paid on every
# tool call.
case "$(uname -sm 2>/dev/null)" in
  "Darwin arm64") target=darwin-arm64 ;;
  "Darwin x86_64") target=darwin-x64 ;;
  "Linux x86_64"|"Linux amd64") target=linux-x64 ;;
  "Linux aarch64"|"Linux arm64") target=linux-arm64 ;;
  MINGW*x86_64|MSYS*x86_64|CYGWIN*x86_64) target=windows-x64.exe ;;
  *) exit 0 ;;
esac

BIN="$ROOT/bin/flueny-$target"
[ -x "$BIN" ] || exit 0

exec "$BIN" hook "${1:-}" 2>/dev/null
