---
description: Show or change which Flueny this machine reports to (staging, production, or a URL)
allowed-tools: ["Bash", "AskUserQuestion"]
---

# Which Flueny is this machine reporting to?

A machine reports to exactly one Flueny. Staging and production are separate
installs holding separate signal, so a machine connected to one is invisible in
the other, and the credential from one is meaningless to the other.

## Steps

1. Show where this machine currently reports:

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" api
   ```

2. If the user did not name a target, stop here and report what it said. Do not
   switch a machine because the user asked where it points.

3. If they did name one, switch to it. Pass `--agent grok-build` if you are Grok
   (or `GROK_PLUGIN_ROOT` is set); Claude Code omits the flag.

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" api <staging|production|URL>
   ```

   This is a sign-in, not a settings change: it prints a code and a link and
   then waits. Show the code and the link exactly as printed. The old credential
   stays until the new sign-in succeeds, so an abort leaves the machine where it
   was.

4. Confirm the result rather than assuming it worked:

   ```sh
   node "${GROK_PLUGIN_ROOT:-$CLAUDE_PLUGIN_ROOT}/src/cli.ts" status
   ```

   Report the `API` line back. If the session is inert, say so and say why: a
   repository allowlisted on one Flueny is not allowlisted on another, so this is
   the expected first state after a switch and an administrator fixes it on the
   Coding operations page of the environment just moved to.

## Rules

- Never switch without being asked to. Reporting where a machine points is the
  common case; moving it is not.
- Never invent a URL. `staging` and `production` are the names the client knows;
  anything else the user must give in full.
- Signal already sent stays where it was sent. Say that plainly if the user
  expects their history to follow them, because it does not.
