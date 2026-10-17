# AGENTS.md

Guidance for coding agents working in this repository.

## Commands

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

All four run in CI and must pass before a change is done.

## Crates

- `crates/fettle`: the CLI. Evaluates `fettle.ts`, opens SSH connections, prints plans.
- `crates/fettle-agent`: the binary that runs on the target. Holds the `Resource` trait and its implementations.
- `crates/fettle-protocol`: serializable data shared by both: catalog, plan, and CLI-agent messages.

A new crate is named for its responsibility. Do not add generic crates such as `core`, `common` or `utils`.

## Design invariants

See [docs/design.md](docs/design.md). Never break these:

- User code never runs on the target. `fettle.ts` is evaluated on the operator's machine into a catalog.
- The agent is a static binary and needs nothing on the target besides SSH.
- Real state is always read again. The state file is used for removal and drift, never as the source of truth.

## Conventions

- English everywhere: code, identifiers, comments, docs, commit messages.
- Comments explain why, not what. Prefer clear names and small functions over comments.
- User-visible changes get an entry under `Unreleased` in `CHANGELOG.md`.
