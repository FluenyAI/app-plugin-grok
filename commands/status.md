---
description: Check whether Flueny is actually receiving anything from this machine
allowed-tools: ["Bash"]
---

# Flueny status

Diagnose the whole chain, not just the last step. Each link can fail while every
earlier one still looks healthy, which is how a machine ends up "installed" and
silent.

## Steps

1. Run the client's own check:

   ```sh
   node "${CLAUDE_PLUGIN_ROOT}/src/cli.ts" status
   ```

2. Read the result against these links, in order, and report the FIRST broken
   one rather than a summary:

   - **No credential.** The machine has never signed in. Run `/flueny:connect`.
   - **Handshake failing.** Sign-in exists but the server rejected the session.
     Report the status code verbatim; this is a Flueny problem, not the user's.
   - **Session inert, repository not allowlisted.** The most common case, and
     silent by design. Give the user the exact repository id and tell them an
     administrator registers it on the Coding operations page.
   - **Kill switch on.** The organisation has paused collection. Nothing to fix
     locally.
   - **Connected but no events yet.** Normal until the first tool call in an
     allowlisted repository.

3. If everything is healthy, say when the last event was sent, and nothing more.

## Rules

- Report the first broken link, not a list of everything.
- Never say setup succeeded because a command exited 0.
- Do not speculate about what leaves the machine. Point at the Data visibility
  page in the Flueny app, which renders the exact field list.
