# AGENTS.md

Instructions for coding agents (Codex, Claude Code and others) working in this
repo. Read `docs/ARCHITECTURE.md` before touching the ledger, the mandate or
crate boundaries. `docs/ROADMAP.md` lists what is next and who owns it.

## Hard rule: Rust, plus SolidJS for the human frontend

Everything is Rust: the core, git layer, actions, CLI, MCP server, local API
server (`gitbots ui`) and the hosted Cloudflare layer. The **one exception** is the
human-facing frontend in `web/`, which is **SolidJS + TypeScript** (Vite). Do
not add any other language. Shell is fine inside CI steps, workflow `run`
lines and dev scripts in `scripts/`. If something looks impossible in Rust,
stop and ask; don't work around it.

The frontend talks only to the JSON API in `docs/API.md`. It never reads git
or the ledger directly, so the same app works against `gitbots ui` locally and
the hosted layer later. Change the contract in `docs/API.md` first, then the
Rust server (`crates/gitbots/src/ui.rs`), then `web/src/api/`.

## Crates

| crate          | role                                                                 |
|----------------|----------------------------------------------------------------------|
| `gitbots-core`    | pure domain: ids, identity, events, ledger layout, manifest/mandate, recipes, redaction, board + stats folds |
| `gitbots-git`     | gix ledger branches, workrooms (worktrees), hooks, sync, union merges |
| `gitbots-actions` | workflow spec, engine, runners (`local`, `celesto`)                  |
| `gitbots`         | `Project` facade, the `gitbots` CLI binary, `gitbots mcp`, `gitbots ui` (API server) |
| `web/`         | SolidJS frontend for humans (inbox, board, review, activity, agents) |

Dependency direction (`a <- b` means b depends on a):

```
gitbots-core <- gitbots-git     <- gitbots
gitbots-core <- gitbots-actions <- gitbots
```

- `gitbots-actions` must not depend on `gitbots-git`. It writes logs through a
  `LogSink` trait so a hosted runner can reuse it unchanged.
- Shared dependency versions live in `[workspace.dependencies]`; crates use
  `foo.workspace = true`.

## gitbots-core stays pure

- No IO: no filesystem, network, processes, env vars, clocks
  (`now_utc`, `SystemTime::now`) or randomness. Callers pass in ids (`Ulid`)
  and timestamps.
- It must build for `wasm32-unknown-unknown` (it will run in a Cloudflare
  Worker). CI checks this. Never add `tokio`, `gix`, HTTP clients or anything
  that needs a host OS; check a new dependency builds for wasm32 first.

## Conventions

- Edition 2024, MSRV 1.88 (`rust-version` in the workspace `Cargo.toml`).
  Don't use APIs stabilized after 1.88.
- Errors: `gitbots-core` uses `thiserror` enums only. The IO crates (`gitbots-git`,
  `gitbots-actions`) return `anyhow::Result` and wrap the conditions callers
  branch on in a typed error (e.g. `LedgerError`) that callers `downcast_ref`.
  The `gitbots` binary, CLI and MCP handlers use `anyhow`.
- Tests live next to the code (`#[cfg(test)] mod tests`). Cross-module tests go
  in `src/tests.rs`, end-to-end tests in `crates/<crate>/tests/`.
- No `unwrap`/`expect` in library code outside tests, unless the invariant is
  local and commented (see the `ulid()` accessor in `gitbots-core/src/id.rs`).
- `unsafe_code` is forbidden workspace-wide. Clippy warnings fail CI.
- Formatting comes from `rustfmt.toml` (width 100, small heuristics `Max`).
- Module docs in `gitbots-core` are the spec for the ledger. Keep them current.

## Ledger compatibility

The ledger branches (`gitbots/activity`, `gitbots/logs`) outlive any one binary and
are union-merged across machines. Treat their format as a public API.

- **Append-only.** Never edit or delete a ledger file; write a new one.
- Readers ignore unknown fields. Kinds a build doesn't know, or can't parse,
  become `EventBody::Unknown` and are kept verbatim.
- **Kinds only gain optional fields** (`#[serde(default, skip_serializing_if = ...)]`).
  A breaking change gets a new kind name. `v` versions the envelope, not payloads.
- **Paths are derived from ids** (UTC time of the ULID), never from the wall
  clock. Use the helpers in `gitbots_core::ledger`; don't build paths by hand.
- Commit boundaries carry no meaning. Writers may batch files into one commit.
- **Never force-push or rewrite a ledger branch.** Only an approved rewrite
  bumps `epoch` in `LEDGER.json`.
- Redact (`gitbots_core::redact`) anything before it is written to a ledger.

## Checks

Run all four before handing work back. CI runs the same
(`.github/workflows/ci.yml`).

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check -p gitbots-core --target wasm32-unknown-unknown  # rustup target add wasm32-unknown-unknown
```

## Ownership

| area | owner |
|------|-------|
| `crates/gitbots-actions`, runners (local, Celesto Computers, later Cloudflare Containers), hosted actions | Dominik (`gstohl`, UTC+2) |
| Product and UX: dashboard, Workrooms UI, Agent Commons UX, onboarding | Santosh |
| `crates/gitbots-core` and the ledger format | shared: both founders must agree |

A change to the ledger format, the event envelope or the manifest schema needs
sign-off from both founders. Say so in the PR.

## Dogfood

Once the `gitbots` CLI works, agents in this repo use it on this repo:

- run inside an gitbots session and do the work in a workroom (attempt), not on
  `main`;
- report and submit through gitbots, and leave approval to someone outside your
  session family;
- don't edit `.gitbots/**`, `.github/**`, `.claude/**`, `.codex/**`, `AGENTS.md`
  or `CLAUDE.md` from a workroom; the default mandate denies those paths.

Until then: work on a branch, keep `Co-Authored-By` trailers, and say in the PR
which agent (provider/model/client) did the work.
