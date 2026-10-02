# flueny, the Grok client

Flueny measures how you work with a coding agent and shows it to you first. This repository is the
client: a single native `flueny` binary that runs as the agent's hooks, derives a small set of
facts on your machine, and sends only those. Your prompts, your code and your file contents never
leave the machine, and your organisation sees group aggregates rather than anything about you
individually.

This repository is the Grok marketplace source. Its client is byte identical to
`FluenyAI/app-plugin-claude-code`, which Claude Code installs from. Under Grok the client reports
`agent: grok-build`, and each host keeps its own credential, so both can stay connected on one
machine.

## Install

In a terminal, then restart Grok or press `r` in `/plugins`:

```
grok plugin marketplace add FluenyAI/app-plugin-grok
grok plugin install flueny --trust
/flueny:connect
```

`--trust` is required so the plugin's hooks can run. Claude Code installs the same client from
`FluenyAI/app-plugin-claude-code`.

Nothing else is needed: no Node, no Python, no compiler, no network at install time. The plugin
carries a prebuilt binary for each supported platform under `bin/`, and `hooks/flueny-hook.sh`
picks the one for your machine:

| Platform | Binary |
| --- | --- |
| macOS, Apple silicon | `bin/flueny-darwin-arm64` |
| macOS, Intel | `bin/flueny-darwin-x64` |
| Linux, x86_64 (static, any distribution) | `bin/flueny-linux-x64` |
| Linux, arm64 (static, any distribution) | `bin/flueny-linux-arm64` |
| Windows, x86_64 (through `sh`, as Git Bash provides) | `bin/flueny-windows-x64.exe` |

On any other platform the hooks exit 0 and do nothing, silently.

`/flueny:connect` prints a short code and a link; the link opens the approval page with the code
already filled in. `/flueny:status` says whether this machine is actually sending anything, and
which link in the chain is broken when it is not. `/flueny:api` shows or switches which Flueny the
machine reports to (staging, production, or a URL).

For a repository to produce any signal at all, an admin has to register its git remote on the
Coding operations page (`PUT /integrations/coding/allowlist`). An unregistered repository is inert
by design, and that is the fail-closed direction. Paste the output of `git remote get-url origin`
rather than the address in a browser: a custom SSH host alias derives a different id.

### Where the sign-in lives

The hook token is stored in the operating system's credential store: the macOS Keychain, the
Windows Credential Manager, or the Secret Service on Linux. Only on a machine with no store at all
(a headless box, a container, a CI runner) does it fall back to a file readable only by you
(mode 0600) under `~/.config/flueny`, and `flueny status` says which one is in use on its
`Credential store` line. A credential left by an earlier version of this client in a 0600 file is
moved into the store the first time it is read, and the file is deleted.

On macOS the Keychain may ask once whether `flueny` can use the Flueny entry after the plugin is
updated, because each build is a new binary. Choose Always Allow. A hook never waits on that
prompt for more than a moment; it skips sending for that one call instead.

### Without the plugin

Still supported, for a repository-scoped install or for an enterprise pushing managed settings to a
fleet. Use the binary for your platform from `bin/`:

```sh
flueny login                 # add --api-url for a local stack
flueny install               # prints the hooks block to merge by hand
flueny status                # what this client is doing, and whether it is inert
flueny dry-run --today       # every row sent today, with the exact field names
flueny logout                # forget the credential
```

Merging that block into a `settings.json` that already has hooks is the step that goes wrong, so
back the file up first. The plugin exists to avoid it.

## What it does

Nothing is enforced. There is no `PreToolUse` gate, `capabilities.canEnforce` is `false` for every
agent, and this client does not stub one.

1. **`SessionStart` handshake** against `POST /integrations/coding/session/start`. Honours
   `killSwitch`, `repoAllowlist` (fail closed), `dryRun` and `dryRunEndsAt`, caches the policy
   bundle by ETag, and declares what it reads locally (`readsLocally`).
2. **Local extraction.** The raw hook payload is read here and discarded with the process. What
   leaves is a `CodingEvent`: ids, enums, booleans and counts, none of which can hold prompt text,
   code, file contents, `tool_input` or `tool_response`.
3. **A bounded local queue** (2,000 events, oldest dropped first) and
   `POST /integrations/coding/events`, batched at 500, deduped on `eventId`, with one token refresh
   on a 401. `PostToolUse` flushes opportunistically within a 400ms budget, so a tool call reaches
   the live feed within seconds; a slow or unreachable backend leaves the event queued for the next
   `Stop`, `SessionEnd` or `SessionStart`.
4. **OAuth device flow** against `POST /oauth/device` and `POST /oauth/token`.
5. **`flueny dry-run --today`** and the end of day receipt (design decision 44), generated from the
   serialized events, never written by hand.
6. **Engineering signal (feature 0126).** Whether the agent's work ends verified, computed on this
   machine from text the client already reads. See below.
7. **Three opt-ins, all off by default**, each resolved by the server at handshake and fail closed:
   - prompt insight scoring (0094): the turn's prompt and reply go to `/insights` for a
     Description tip, at `Stop`, once, never queued to disk;
   - live feedback (0098): the same turn goes to `/live-feedback` for a coaching nudge;
   - raw activity (0109): the repo-relative file path and Bash command text go to `/raw-activity`.

   `flueny status` states each one plainly, plus how many insight submissions were delivered or
   failed today.

## The engineering signal

A test command used to be a regex on the command text, so a failing `npm test` counted the same as
a passing one. Now:

- **`test-run`**, one per test command: the runner (jest, vitest, mocha, node, pytest, go, cargo,
  rspec, phpunit, dotnet, jvm, playwright, cypress, other), the outcome, and the passed, failed
  and skipped counts from the runner's own summary. `passed` needs an exit 0 the shell really
  reported for the test command, or a summary that says nothing failed; a run whose outcome cannot
  be established is `unknown`, never guessed as passed. A piped or `|| true` command does not lend
  its exit status to the tests.
- **`turn-verification`**, one per agent turn that made an accepted edit, sent at `Stop`: accepted
  edits, source and test files changed, whether a test was edited first, test runs, whether the
  turn ended on a passing run and how many edits came after it, the longest streak of the same
  command failing, files edited three or more times, whether a sensitive path (auth, infra,
  payments, security) was changed without ending green, subagent calls, and weakened-test flags.
- **Weakened tests**, detected on the edit's old and new text and on Bash commands, all locally:
  `skip-added`, `only-added`, `assertion-removed`, `test-deleted`, `snapshot-updated`,
  `type-suppression`, `lint-suppression`, `no-verify`. Only the kind of change is sent. A flagged
  change the developer declined or reverted in the same turn counts as caught.
- **`reverted` edit decisions**, when a file the agent edited is restored with `git checkout --`,
  `git restore`, `git stash`, `git reset --hard`, `git revert HEAD`, or an edit that puts it back
  to its content before the agent.

How each host reports a command's exit status, established by driving them:

| Host | Exit 0 | Non-zero exit |
| --- | --- | --- |
| Claude Code 2.1.287 | `PostToolUse`, `tool_response` has `stdout`/`stderr`, no exit code | `PostToolUseFailure` only, `error: "Exit code N\n..."` |
| Grok 1.0.34 | `PostToolUse`, `toolResult.exit_code` | `PostToolUse`, `toolResult.exit_code` |

That is why the plugin registers `PostToolUseFailure` as well: without it, under Claude Code, a
failing test run never reached the client at all. Grok accepts the same event name.

## The privacy promise, and where it is held

`src/wire.rs` is the only path to `/events`. It **rebuilds** an outgoing event key by key from a
fixed list rather than filtering a raw one, so an extractor that starts carrying an extra field
cannot leak it by accident, a value of the wrong type is dropped, and an enum field accepts only
its closed list of values.

What the client reads on your machine to derive those events is declared in `src/reads.rs`, sent
at handshake, and rendered on the Data visibility page: raw tool inputs and outputs, the
transcript's tool-use decision records (prompt and reply text are never decoded), test command
output, edit text and Bash commands scanned for weakened tests, and the content of a file the
agent just edited, reduced to a hash. A test fails when a module starts reading the filesystem
without a declaration.

The redaction tests drive hook payloads stuffed with prompt text, diffs, file contents, test
output, secrets and absolute paths through the real hook path against a real local HTTP server,
and fail if any of it appears in a request body. They assert on **what was sent**, never on a
response status: `/events` answers `202` to everything, so a test that checked the status would
pass against a client that leaked everything. Local state is held to the same rule: the session
file keys files by a hash of their path and never holds a command, an output or edit text.

## Why the hooks are `type: "command"`

A hook of type `http` posts the raw payload to a URL with no local code in between. Pointed at
`/events` it fails twice over: the body is not a `CodingEventBatch`, so ingest drops it and still
answers `202`, and the body it does post contains `tool_input` and `tool_response`, the prompt
text, code and file contents this product promises never leave the machine (CEO decisions 8A and
33A).

A command hook spawns a process per tool call, which is why the client is a native binary. Measured
on an Apple silicon Mac against a local API, median per hook through `flueny-hook.sh`:

| Hook | Rust client | Node client it replaces |
| --- | --- | --- |
| `PostToolUse`, inert repository | 8.5 ms | 65.7 ms |
| `PostToolUse` with the live flush | 17.8 ms | 107.1 ms |
| `PostToolUse` Edit with weakened-test scan | 16.5 ms | |
| `PostToolUse` test run with summary parsing | 16.3 ms | |
| `Stop` | 8.6 ms | |

Reading the token from the macOS Keychain adds about 10 ms to a hook that sends.

Hooks fail open: they never print to stdout, never exit non-zero, and stop themselves after 8
seconds, well inside the host's own timeout.

## Where rejections come from

`PostToolUse` fires after a tool has run, so a tool the developer declined never reaches it. The
second source that does not need a `PreToolUse` gate is the session transcript, swept locally at
`Stop` (`src/transcript.rs`) by a byte scanner that never decodes a line. What comes out of that
sweep is tool-use ids and one class label each.

## Development

Rust 1.97, no other toolchain:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

`FLUENY_CONFIG_DIR` points the client at another config directory, `FLUENY_CREDENTIAL_STORE=file`
forces the 0600 file, and `FLUENY_CREDENTIAL_SERVICE` renames the credential store entry, so a
development run never touches a real sign-in.

The binaries under `bin/` are committed on purpose (a marketplace install is a git clone) and are
rebuilt by the `binaries` workflow: run it on a branch to commit fresh binaries there, or push a
`v*` tag to build the tagged source onto a `chore/binaries-<tag>` branch with a PR. The `ci`
workflow runs fmt, clippy and the tests on Linux, macOS and Windows.

## Layout

| Path | What it is |
| --- | --- |
| `src/cli.rs` | `login`, `status`, `api`, `dry-run`, `install`, `logout`, `hook` |
| `src/hooks.rs` | the hook handlers |
| `src/extract.rs` | raw payload in, derived facts out. The discard happens here |
| `src/signal/` | the 0126 detectors: test runs, weakened tests, git reverts |
| `src/verify.rs` | turn verification, computed at `Stop` |
| `src/revert.rs` | reverted agent edits |
| `src/transcript.rs` | the local sweep that finds declined edits |
| `src/prompt_insight.rs` | the opt-in prompt and reply read |
| `src/wire.rs` | the redaction boundary |
| `src/session.rs` | the handshake, and the three ways to be inert |
| `src/queue.rs` | bounded queue, dedupe, batching, flush |
| `src/credentials.rs` | the OS credential store, and the 0600 fallback |
| `src/repo_id.rs` | mirror of the backend's remote normalization contract |
| `src/classify.rs` | the path classifier from the policy bundle |
| `src/copy.rs` | the terminal strings and the voice rules |
| `src/reads.rs` | what the client reads locally, declared |
| `src/store.rs` | everything this client writes to disk |
| `hooks/` | `hooks.json`, the hook wrapper, and the `flueny.sh` launcher |
| `bin/` | the per-platform binaries |
