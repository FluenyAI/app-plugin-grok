#!/bin/sh
# The `flueny` command for a developer, and for the plugin's slash commands:
# `sh flueny.sh status`, `sh flueny.sh login --api-url URL`, and so on.
#
# Same platform pick as flueny-hook.sh, but loud when there is no binary, because
# a person ran this and should hear why nothing happened.

ROOT="${CLAUDE_PLUGIN_ROOT:-${GROK_PLUGIN_ROOT:-}}"
[ -n "$ROOT" ] || ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# One uname for both halves: every process this wrapper spawns is paid on every
# tool call.
case "$(uname -sm 2>/dev/null)" in
  "Darwin arm64") target=darwin-arm64 ;;
  "Darwin x86_64") target=darwin-x64 ;;
  "Linux x86_64"|"Linux amd64") target=linux-x64 ;;
  "Linux aarch64"|"Linux arm64") target=linux-arm64 ;;
  MINGW*x86_64|MSYS*x86_64|CYGWIN*x86_64) target=windows-x64.exe ;;
  *) target=unknown ;;
esac

BIN="$ROOT/bin/flueny-$target"
if [ ! -x "$BIN" ]; then
  echo "Flueny has no client binary for this machine ($(uname -sm 2>/dev/null)) in $ROOT/bin." >&2
  echo "Supported: darwin-arm64, darwin-x64, linux-x64, linux-arm64, windows-x64." >&2
  exit 1
fi
exec "$BIN" "$@"
