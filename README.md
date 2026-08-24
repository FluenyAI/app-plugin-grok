# flueny, the Grok client

**This is a P0 dogfood spike, not the shipped client.** It exists to close the loop on one
machine and de-risk the protocol in feature 0028 before anybody writes the real thing. It is
TypeScript on Node, and the shipped client will not be.

> **The Rust rewrite is still required before any customer install.** Hooks of type `command`
> spawn a process per invocation (eng findings 13 and 15). Measured on this machine, a
> `PostToolUse` hook through this client costs **~60ms**, which is Node interpreter startup and
> almost nothing else; a native binary is ~1 to 5ms. At the tool-call rate of a working session
> that is the difference between invisible and noticeable, and it is paid on the developer's
> critical path every time. This spike is the protocol proof. It is not the product.

The other spike compromise: the hook token lives in a **mode 0600 file** under the config
directory, not the OS keychain. A 0600 file is readable by any process running as the developer,
which is a materially weaker promise than a Keychain prompt. Feature 0028 says keychain, and the
shipped client does keychain.

## What it does

A real Grok session emits derived coding signal to Flueny. Nothing is enforced: there is
no `PreToolUse` gate, `capabilities.canEnforce` is `false` for every agent in M1, and this
client does not stub one.

1. **`SessionStart` handshake** against `POST /integrations/coding/session/start`. Honours
   `killSwitch`, `repoAllowlist` (fail closed), `dryRun` and `dryRunEndsAt`, and caches the
   policy bundle by ETag.
2. **Local extraction.** The raw hook payload is read here and discarded with the process. What
   leaves is a `CodingEvent`: nine fields, none of which can hold prompt text, code, file
   contents, `tool_input` or `tool_response`.
3. **A bounded local queue** and `POST /integrations/coding/events`, batched at 500, deduped on
   `eventId`.
4. **OAuth device flow** against `POST /oauth/device` and `POST /oauth/token`.
5. **`flueny dry-run --today`** and the end of day receipt (design decision 44).
6. **Feature 0094, prompt insight scoring, off by default.** Only when the handshake says
   `promptInsightsEnabled: true` for this developer (their own opt-in, or their org's policy),
   `POST /integrations/coding/insights` carries that turn's real prompt and the agent's reply,
   read at `Stop`, held as local values for one request, never queued to disk the way `CodingEvent`
   is. `flueny status` says plainly whether this is on. `src/reads.ts` declares it.

## Install

Requires Node 22.18 or newer. Node runs the TypeScript directly, so there is no build step.

Grok (terminal, then restart Grok or press `r` in `/plugins`):

```
grok plugin marketplace add FluenyAI/app-plugin-grok
grok plugin install flueny --trust
/flueny:connect
```

`--trust` is required so the plugin's hooks can run. Under Grok the client reports
`agent: grok-build`. Claude Code uses a separate repo,
`FluenyAI/app-plugin-claude-code`. Each host keeps its own credential, so both
tools can stay connected on one machine.

That is the whole thing. The plugin declares its own hooks in `hooks/hooks.json`, so
host settings files are never edited, there is no JSON to merge by hand, and
uninstalling `flueny` removes it cleanly. Paths inside the plugin resolve through
`${CLAUDE_PLUGIN_ROOT}` or `${GROK_PLUGIN_ROOT}`, so moving or reinstalling the
checkout does not break the hooks.

`/flueny:connect` prints a short code and a link. The link opens the page with the code already
filled in. `/flueny:status` says whether this machine is actually sending anything, and which
link in the chain is broken when it is not.

For a repository to produce any signal at all, an admin has to register its git remote:
`PUT /integrations/coding/allowlist`, surfaced in the app at `/coding-governance/operations`.
An unregistered repository is inert by design, and that is the fail-closed direction. Paste the
output of `git remote get-url origin` rather than the address in a browser: a custom SSH host
alias derives a different id, and nothing reports the mismatch.

### Without the plugin

Still supported, and still the right answer for a repository-scoped install or for an
enterprise pushing managed settings to a fleet.

```sh
npm install                                    # typescript, for the typecheck. No runtime deps
node src/cli.ts login                          # add --api-url for a local stack
node src/cli.ts install                        # prints the hooks block to merge by hand
```

Merging that block into a `settings.json` that already has hooks is the step that goes wrong,
so back the file up first. The plugin exists to avoid it.

```sh
node src/cli.ts status            # what this client is doing, and whether it is inert
node src/cli.ts dry-run --today   # every row sent today, with the exact field names
node src/cli.ts logout            # forget the credential
```

## Why the hooks are `type: "command"`

Claude Code hooks are either `type: "http"`, which posts the raw payload to a URL with no local
code in between, or `type: "command"`, which spawns a process. Because extraction is local, this
has to be `command`.

An `http` hook pointed at `/events` fails twice over. The body is not a `CodingEventBatch`, so
ingest drops it and still answers `202`, which means nobody ever finds out. And the body it does
post contains `tool_input` and `tool_response`, which are the prompt text, the code and the file
contents this product promises never leave the machine (CEO decisions 8A and 33A).

## Where the promise is held

`src/wire.ts` is the only path to the network. It **rebuilds** an outgoing event key by key from
a fixed list rather than filtering a raw one, so an extractor that starts carrying an extra
field cannot leak it by accident, and a value of the wrong type is dropped rather than passed
through.

`test/redaction.test.ts` drives a hook payload stuffed with prompt text, a diff, file contents,
a secret and an absolute path through the real hook path, and fails if any of it appears in a
request body. It asserts on **what was sent**, never on a response status: `/events` answers
`202` to everything, including malformed input and load shedding, so a test that checked the
status would pass against a client that leaked everything.

## Where rejections come from

`PostToolUse` fires after a tool has run, so a tool the developer declined never reaches it.
Discernment is the competency built on exactly that fact ("You rejected 3 of 11 agent edits this
week"), so without a second source this milestone could only ever report a 0% rejection rate,
which is not a low number, it is a wrong one. The second source that does not need a
`PreToolUse` gate is the session transcript, swept locally at `Stop`
(`src/transcript.ts`). What comes out of that sweep is tool-use ids and one class label each.

## Tests

```sh
npm test        # node:test, no framework
npm run check   # typecheck plus tests
```

Tests exist from commit one (eng finding 10). The load-bearing ones: redaction, allowlist fail
closed, kill switch inert, dedupe, the handshake ETag branch, the 500-event batch cap, and the
401 refresh on both `/events` and `/session/start`.

## Layout

| File | What it is |
| --- | --- |
| `src/cli.ts` | `login`, `status`, `dry-run`, `install`, `logout`, `hook` |
| `src/hooks.ts` | the four hook handlers |
| `src/extract.ts` | raw payload in, derived facts out. The discard happens here |
| `src/transcript.ts` | the local sweep that finds declined edits |
| `src/wire.ts` | the redaction boundary |
| `src/session.ts` | the handshake, and the three ways to be inert |
| `src/queue.ts` | bounded queue, dedupe, batching, flush |
| `src/repo-id.ts` | mirror of the backend's remote normalization contract |
| `src/classify.ts` | the path classifier from the policy bundle |
| `src/copy.ts` | the six terminal strings and the voice rules |
| `src/settings.ts` | the Claude Code hooks block |
| `src/store.ts` | everything this client writes to disk |
