---
description: What Flueny is measuring on this machine, and how to connect it
allowed-tools: ["Bash"]
---

# Flueny

Flueny measures how you work with Claude Code and shows it to you before anyone
else sees anything. Derived signal only: prompts, code, file contents and tool
output never leave this machine, and your organisation sees group aggregates
rather than anything about you individually.

## What to do

Run `/flueny:status` first and report what it says. It checks the whole chain
and names the first broken link:

```sh
node "${CLAUDE_PLUGIN_ROOT}/src/cli.ts" status
```

- Not signed in on this machine, run `/flueny:connect`.
- Signed in but the repository is not registered, give the user the exact
  repository id from the output and tell them an administrator registers it on
  the Coding operations page in the Flueny app.
- Connected with nothing sent yet, that is normal until the first tool call in a
  registered repository.
- Connected and sending, say when the last event was and stop there.

## Rules

- Report the first broken link, not a summary of everything.
- Never say setup succeeded because a command exited 0. An install is done when
  an event is observed, not when a file is written.
- For what is actually transmitted, point at the Data visibility page in the
  Flueny app. It renders the exact field list rather than a description of it.
