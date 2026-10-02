---
description: Connect this machine to Flueny (device sign-in, no bash required)
allowed-tools: ["Bash", "AskUserQuestion"]
---

# Connect this machine to Flueny

Sign this machine in so Flueny can measure coding signal. Nothing is scored until
it is connected, and prompts and code never leave the machine either way.

## Steps

1. If the user gave an API URL as an argument, use it. Otherwise ask the client
   which Flueny this machine already reports to. The credential lives in the OS
   credential store (or a 0600 file where there is none), so ask the client
   rather than looking for a file (Grok, or `GROK_PLUGIN_ROOT` set: add
   `--agent grok-build`):

   ```sh
   sh "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/flueny.sh" api
   ```

   If it prints an `API` line, reuse that URL and tell the user which one you are
   reusing. If it says this host is not connected, ask the user for their Flueny
   URL with AskUserQuestion rather than guessing one.

2. Start the sign-in. This prints a short code and a link, then waits.
   If you are Grok (or `GROK_PLUGIN_ROOT` is set), pass `--agent grok-build`.
   Claude Code omits the flag. Use `${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}`
   as the plugin root so both hosts resolve the same files.

   ```sh
   sh "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/flueny.sh" login --api-url <URL>
   ```

   Grok:

   ```sh
   sh "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/flueny.sh" login --agent grok-build --api-url <URL>
   ```

   Show the user the code and the link exactly as printed. The link opens a page
   with the code already filled in. Do not paraphrase the code.

3. When it returns, confirm the state rather than assuming it worked:

   ```sh
   sh "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/hooks/flueny.sh" status
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
