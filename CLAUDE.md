@../app-docs/CLAUDE.md
@../app-docs/FEATURES/INDEX.md

# app-plugin-claude-code

The Claude Code client for the Flueny coding agent surface. Separate repo per eng finding 10,
with tests from commit one.

If the two imports at the top of this file did not resolve, `app-docs` is not checked out as a
sibling and you are in the wrong directory.

## What this is

The client half of `../app-docs/FEATURES/0028-coding-surface-m1.md`. Read that contract and
`../app-docs/designs/coding-agent-surface.md` before writing code.

Hard constraints, from CEO 8A and 33A. These are the whole point of the product:

- No prompt text, no code, no file contents ever leave the machine.
- Extraction happens here, client side. The backend receives derived `CodingEvent` only.
- Raw `tool_input` and `tool_response` are read locally and discarded, never transmitted.

Hooks are either `type: "http"` (no local code) or `type: "command"` (spawns a process).
Because extraction is local, this must be `type: "command"`. That is eng findings 13 and 15,
and it is why a plain http hook pointed at `/events` does not work: it would post the wrong
shape and be dropped as malformed, and it would ship raw payloads off the machine.

## M1 scope

Nothing is enforced. `canEnforce` is false for every agent. Do not build a `PreToolUse` gate.

- `SessionStart` handshake against `POST /integrations/coding/session/start`
- Extract and classify locally, queue with bounds, POST derived events to `/events`
- Honour `killSwitch`, `repoAllowlist` and `dryRun` from the handshake
- OAuth device flow, credential in the OS keychain
- `flueny dry-run --today` and the end of day receipt (design decision 44)
- The six terminal strings, verbatim and in the voice defined in the design plan

## Local backend

An isolated stack is already running. Do not start your own.

    API http://localhost:3011

`app-backend` ships a smoke script that exercises this whole client half over HTTP. Read it
first, it is the reference implementation of the protocol you are matching.
