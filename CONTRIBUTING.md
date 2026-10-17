# Contributing to fettle

Thanks for your interest in fettle. The project is pre-alpha: most of the design in [docs/design.md](docs/design.md) is not built yet, and the [roadmap](ROADMAP.md) says what comes first.

## Before you start

Open an issue before starting a large change, so we can agree on the approach before you spend time on it. Small fixes can go straight to a pull request. Security issues go through the [security policy](SECURITY.md), not public issues.

## Setup

Install [rustup](https://rustup.rs/). The toolchain is pinned in `rust-toolchain.toml`; run `rustup toolchain install` in the repository to install it. Install [cargo-deny](https://github.com/EmbarkStudios/cargo-deny) for the dependency checks.

## Checks

CI runs these commands; run them locally before opening a pull request:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
```

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Keep each pull request focused on one change, with tests for new behavior.
3. Add an entry under `Unreleased` in [CHANGELOG.md](CHANGELOG.md) for user-visible changes.
4. Open the pull request and link the issue it addresses.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in fettle by you, as defined in the Apache-2.0 license, shall be dual licensed as in [README.md](README.md#license), without any additional terms or conditions.
