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

### Agent build

The agent ships as one static musl binary per architecture. Building it needs [zig](https://ziglang.org) and `cargo install --locked cargo-zigbuild`:

```sh
cargo zigbuild -p fettle-agent --release --locked --target x86_64-unknown-linux-musl
cargo zigbuild -p fettle-agent --release --locked --target aarch64-unknown-linux-musl
```

Binaries land in `target/<triple>/release/fettle-agent`. These are not required locally: the `agent` CI job builds both and checks that they are static and run on stock Debian and Ubuntu.

The CLI bundles the agents named by `FETTLE_AGENT_X86_64` and `FETTLE_AGENT_AARCH64` at build time. Each is optional: a CLI built without one fails to connect to hosts of that architecture, naming it. To build the CLI with both:

```sh
FETTLE_AGENT_X86_64=$PWD/target/x86_64-unknown-linux-musl/release/fettle-agent \
FETTLE_AGENT_AARCH64=$PWD/target/aarch64-unknown-linux-musl/release/fettle-agent \
  cargo build -p fettle --release --locked
```

`crates/fettle/tests/session.rs` runs end to end only when `FETTLE_TEST_SSH` names a disposable SSH destination, such as `ssh://root@127.0.0.1:2222`; the `agent` CI job runs it against a `debian:13` container.

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
