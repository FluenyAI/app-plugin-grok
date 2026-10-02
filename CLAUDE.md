@../app-docs/CLAUDE.md
@../app-docs/FEATURES/INDEX.md

# app-plugin-grok

The Grok client for the Flueny coding agent surface: one native `flueny` binary, written in Rust,
that runs as the agent's hooks. `src/`, `bin/`, `hooks/`, `commands/`, the crate files and the
workflows are kept byte identical to `app-plugin-claude-code`, so a change to one is mirrored in
the other. Only the README, this file and the Grok manifest text differ.

If the two imports at the top of this file did not resolve, `app-docs` is not checked out as a
sibling and you are in the wrong directory.

## What this is

The client half of `../app-docs/FEATURES/0028-coding-surface-m1.md`, rewritten in Rust by
`../app-docs/FEATURES/0126-companion-engineering-signal.md`. Read those and
`../app-docs/designs/coding-agent-surface.md` before changing behaviour.

Hard constraints, from CEO 8A and 33A. These are the whole point of the product:

- No prompt text, no code, no file contents ever leave the machine.
- Extraction happens here, client side. The backend receives derived `CodingEvent` only.
- Raw `tool_input` and `tool_response` are read locally and discarded, never transmitted.
- `src/wire.rs` is the one boundary every event crosses, and it rebuilds events key by key from
  `CODING_EVENT_FIELDS`. A new field is a cross-repo change: the feature file's API contract first.
- A new local read gets an entry in `src/reads.rs` in the same commit. A test enforces it.
- Hooks fail open: nothing on stdout, exit 0 always, bounded network calls.

Hooks are `type: "command"`, never `type: "http"`: an http hook would post the raw payload off the
machine, in the wrong shape for `/events`.

## Toolchain

Rust 1.97 (Homebrew `cargo` works; there is no Node runtime any more). Before finishing work:

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests run against a real local HTTP server (`src/testing.rs`) and assert on the request bodies
that were sent. Every test gets its own temp config directory and an in-memory credential store, so
`cargo test` never touches a real sign-in or the Keychain.

For a manual run, point the binary away from your real setup:

```sh
FLUENY_CONFIG_DIR=/tmp/flueny-dev FLUENY_CREDENTIAL_STORE=file target/release/flueny status
```

`FLUENY_CREDENTIAL_SERVICE=<name>` uses a different Keychain entry instead of the file.

## Binaries

`bin/flueny-<os>-<arch>` are committed (feature 0126 decision): a marketplace install is a git
clone, so nothing can compile on a developer's machine. `hooks/flueny-hook.sh` picks the one for
the machine and exits 0 when none matches; `hooks/flueny.sh` is the same pick for the slash
commands and for people. Rebuild them with the `binaries` GitHub workflow, never by hand on a
release. Bump the version in `Cargo.toml` and every manifest together (a test checks they agree).

## Local backend

To test against a local stack, run `app-backend` (it serves on :3001 by default) and connect with
`flueny login --api-url http://localhost:3001`. `app-backend/src/scripts/smoke-coding.ts` exercises
the whole client protocol over HTTP and is the reference for what the server expects.
