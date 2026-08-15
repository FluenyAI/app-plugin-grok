---
description: Connect this machine to Flueny (device sign-in, no bash required)
allowed-tools: ["Bash", "AskUserQuestion"]
---

# Connect this machine to Flueny

Sign this machine in so Flueny can measure coding signal. Nothing is scored until
it is connected, and prompts and code never leave the machine either way.

## Steps

1. If the user gave an API URL as an argument, use it. Otherwise check for an
   existing credential first:

   ```sh
   sh "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/flueny-hook.sh" --version >/dev/null 2>&1
   ls "${XDG_CONFIG_HOME:-$HOME/.config}/flueny"/credentials*.json 2>/dev/null
   cat "${XDG_CONFIG_HOME:-$HOME/.config}/flueny/credentials.grok-build.json" 2>/dev/null
   cat "${XDG_CONFIG_HOME:-$HOME/.config}/flueny/credentials.claude-code.json" 2>/dev/null
   cat "${XDG_CONFIG_HOME:-$HOME/.config}/flueny/credentials.json" 2>/dev/null
   ```

   If that file exists and has an `apiUrl`, reuse it and tell the user which one
   you are reusing. If it does not, ask the user for their Flueny URL with
   AskUserQuestion rather than guessing one.

2. Start the sign-in. This prints a short code and a link, then waits.
   If you are Grok (or `GROK_PLUGIN_ROOT` is set), pass `--agent grok-build`.
   Claude Code omits the flag. Use `${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}`
   as the plugin root so both hosts resolve the same files.

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" login --api-url <URL>
   ```

   Grok:

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" login --agent grok-build --api-url <URL>
   ```

   Show the user the code and the link exactly as printed. The link opens a page
   with the code already filled in. Do not paraphrase the code.

3. When it returns, confirm the state rather than assuming it worked:

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" status
   ```

4. Report what is actually true. If `status` says the session is inert, say so
   and say why, rather than reporting success. The usual cause is the current
   repository not being on the organisation's allowlist, which an administrator
   fixes on the Coding operations page. If that is the cause, give the user the
   exact repository id from `status` so they can hand it over.

## Rules

- Never invent a Flueny URL. Ask.
- Never report "connected" on the strength of the login command exiting 0. An
  install is done when an event is observed, not when a file is written.
- If sign-in fails, print the failure verbatim. Do not retry in a loop.
