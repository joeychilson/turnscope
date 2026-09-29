# Contributing

Thanks for helping. Turnscope is small on purpose: the goal is an app that
needs only bug fixes and new agents. A bug report, support for a new agent or
a change in an agent's format, and fixes are the most welcome. For anything
larger, open an issue first to talk it over.

## Build and run

You need macOS 26 and Xcode 26. `rustup` installs the Rust toolchain
`rust-toolchain.toml` names on first use.

```sh
scripts/build.sh
open target/xcode/Build/Products/Debug/Turnscope.app --args --data /tmp/turnscope-dev --open
```

`--data` keeps the ledger in a scratch folder, so work in progress never
writes the one your installed copy keeps. Or open `macos/Turnscope.xcodeproj`
and run the Turnscope scheme, which does the same.

## Check a change

These are what CI runs, and each must pass:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
swift test --package-path macos/TurnscopeKit
scripts/build.sh
scripts/release.sh && scripts/smoke.sh   # the release build, signed ad hoc
```

A change to how an agent's history is read, merged or priced also needs
`cargo run --release -p turnscope -- doctor --strict --data <scratch dir>` over
your own history; say in the pull request what its figures were before and
after.

## The contract

The Rust side (`crates/`) works out every figure and rule; the Swift app
(`macos/`) only shows what it is told. What passes between them is in
`contract/`, which both sides' tests read. A change to it changes both sides
in the same commit: write the files again with
`TURNSCOPE_WRITE_CONTRACT=1 cargo test -p turnscope` and review the diff.

## Conventions

[AGENTS.md](AGENTS.md) is the full set of conventions, for people and coding
agents alike: the engine's rules, how Rust and Swift code is written, privacy,
and how to verify a change. In short:

- Commit subjects are [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/),
  such as `fix(engine): read Codex's archived sessions`.
- Keep each commit coherent and buildable, with its tests.
- A change people will notice adds a line under `## [Unreleased]` in
  [CHANGELOG.md](CHANGELOG.md).
- Nothing new goes over the network, and nothing writes an agent's files.
