#!/bin/sh
# Runs one Flueny hook.
#
# Why a wrapper rather than `node ... cli.ts` straight from hooks.json: a hook
# does not inherit an interactive shell's environment, so a developer using nvm,
# asdf, fnm or Homebrew node may have no `node` on PATH at all. The previous
# installer solved that by baking the absolute nvm binary into the settings
# block, which then broke whenever they changed node version.
#
# Every path fails open. A hook that exits non-zero, prints to stdout, or hangs
# is a hook that interferes with the developer's session, and this tool is not
# permitted to be in their way. Silence is the correct failure.

set -u

find_node() {
  if command -v node >/dev/null 2>&1; then command -v node; return; fi
  # Version managers, newest first. `ls -t` rather than a sort: names are not
  # sortable as versions and the newest install is the best guess.
  for base in "$HOME/.nvm/versions/node" "$HOME/.local/share/fnm/node-versions" "$HOME/.asdf/installs/nodejs"; do
    [ -d "$base" ] || continue
    for dir in $(ls -t "$base" 2>/dev/null); do
      for candidate in "$base/$dir/bin/node" "$base/$dir/installation/bin/node"; do
        [ -x "$candidate" ] && { echo "$candidate"; return; }
      done
    done
  done
  for candidate in /opt/homebrew/bin/node /usr/local/bin/node /usr/bin/node; do
    [ -x "$candidate" ] && { echo "$candidate"; return; }
  done
}

NODE="$(find_node)"
[ -n "${NODE:-}" ] || exit 0

ROOT="${CLAUDE_PLUGIN_ROOT:-${GROK_PLUGIN_ROOT:-}}"
[ -n "$ROOT" ] || exit 0

exec "$NODE" "$ROOT/src/cli.ts" hook "$1"
