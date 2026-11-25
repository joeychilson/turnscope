# Development

How to build, test and release Turnscope. [AGENTS.md](../AGENTS.md) has
the rules and what to update when you change something.
[architecture.md](architecture.md) explains how the engine works.

## Requirements

- macOS 26, on Apple silicon or Intel. It's the app's deployment target.
- Rust 1.100.0. `rust-toolchain.toml` pins it with clippy, rustfmt and both
  Mac targets, and rustup installs it on first use.
- Xcode 26, to build and bundle the macOS app: `swiftc` compiles the
  SwiftUI shell against `libturnscope_app.a` and `actool` compiles its
  icon. `cargo test` also runs `xcrun swiftc` on macOS to verify the
  Rust–Swift wire models in `turnscope-app`. Signing a release also uses
  `notarytool`, `stapler` and `swift`. The engine and command line
  (`crates/turnscope`) build and test with Cargo alone.
- `shellcheck`, `actionlint` and `cargo-deny`, only to run CI's lint job
  yourself.

## Layout

```
crates/turnscope/        the engine, the command line and the MCP server
crates/turnscope-app/    app core (state, wording, view models, C ABI)
apps/macos/              the macOS SwiftUI + AppKit shell and its bundle assets
assets/logos/            shared provider and agent SVG logos
docs/                    how it works, and how to change it
scripts/                 building, signing and checking the app
```

## Build

```sh
cargo build       # target/debug/turnscope and target/debug/libturnscope_app.a
scripts/app.sh    # target/debug/Turnscope.app, signed ad hoc
```

## Run on scratch data

Run on an empty data directory, the test fixture's home and off the
network, so neither your own data nor your agents' are touched:

```sh
cargo test --test cli     # copies the fixture home to target/tmp/home
scripts/app.sh            # builds target/debug/Turnscope.app
export TURNSCOPE_OFFLINE=1
target/debug/turnscope --data "$(mktemp -d)" --home "$PWD/target/tmp/home" usage --since 2026-09-01
target/debug/Turnscope.app/Contents/MacOS/Turnscope --data "$(mktemp -d)" --home "$PWD/target/tmp/home" --open
target/debug/Turnscope.app/Contents/MacOS/Turnscope --fixture crates/turnscope/tests/fixtures/status.json --open agents
```

The fixture home holds made-up sessions of every agent, from September
2026, and no logins, so no account's limits are read. `--fixture` shows a
made-up status with accounts, and runs no engine.

Without `--home`, Turnscope reads your agents' history and logins, and
without `TURNSCOPE_OFFLINE`, it asks the providers for their limits. Never
point `--data` at the real data directory: a newer build migrates it, and
the installed app then refuses it.

`open` starts the app in `/` without your shell's environment, so give it
absolute paths and the variables it needs:

```sh
open --env TURNSCOPE_OFFLINE=1 target/debug/Turnscope.app --args \
  --data "$(mktemp -d)" --home "$PWD/target/tmp/home" --open
```

The app posts notifications and updates itself only from a bundle.

`connect <agent>` and `disconnect` run your agent's own command, with
`HOME` set to the home `--home` names. Don't run them to try something out:
not every agent has been checked to keep its settings in that home.

The app's other launch arguments are in
[app.md](app.md#launch-arguments).

## Test

Before every commit, run what CI runs:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
shellcheck scripts/*.sh
actionlint
cargo deny check advisories
```

CI runs the tests in the `America/Chicago` time zone, so a test that takes
local time for UTC fails there. Run `TZ=America/Chicago cargo test` to see
it here.

Unit tests are in each module. The end-to-end tests,
`crates/turnscope/tests/cli.rs`, run the binary on a fixture home and
compare its answers with snapshots.

**Fixtures.** `crates/turnscope/tests/cli.rs` reads a home
(`crates/turnscope/tests/fixtures/home`) with two synthetic Claude Code
sessions and one of each other agent, and writes a database of four
synthetic accounts in SQL. It copies the home to `target/tmp/home` once a
run and builds each database there from the `.sql` file beside it, so no
binary database is kept in the repository. `crates/turnscope/tests/fixtures/status.json` is a
made-up status, which the app's tests, `--fixture` and the README's
screenshots use. Fixtures never hold real ids, emails, keys or session
text.

**Snapshots.** The end-to-end tests compare each answer with its file in
`crates/turnscope/tests/snapshots/`. After a deliberate change to output,
write them again and review the diff:

```sh
TURNSCOPE_BLESS=1 cargo test --test cli
```

Each line of output in the examples of [cli.md](cli.md) and
[mcp.md](mcp.md) must be a line of some snapshot, so update an example
that shows output you changed.

**The caller.** Turnscope takes the nearest agent above it for the caller,
so each end-to-end test runs `turnscope` under a shell named as an agent's
command: `claude`, or another agent's in a test about that agent. The tests
answer the same in a terminal, in CI and in any agent.

**The panel.** What needs a screen is checked by drawing it:
`--snapshot` with `--fixture` draws the panel headless
([app.md](app.md#launch-arguments)), and `scripts/check-app.sh` has a
built app draw it.

**Variables.** `TURNSCOPE_NOW` and `TURNSCOPE_OFFLINE` are in
[cli.md](cli.md#environment). These are for development only:

| Variable | Does |
|---|---|
| `TURNSCOPE_BLESS` | Writes the snapshots instead of comparing them |
| `TURNSCOPE_BACKTEST` | The copy of a data directory the forecast backtest replays |

## Forecast backtest

`forecast::tests::backtest` replays real limit history and scores the
forecast: its point error, how often its fast end holds, and how often it
declines to forecast. It needs weeks of history to say much. Run it on a
copy of a data directory, never the real one:

```sh
cp -cR ~/Library/Application\ Support/com.joeychilson.turnscope /tmp/ts-copy
TURNSCOPE_BACKTEST=/tmp/ts-copy cargo test --release -p turnscope backtest -- --ignored --nocapture
```

`forecast.rs` records what the forecast chose and what it was compared
with.

## Scripts

Each script in `scripts/` says in its header what it does and what it
reads. To build and sign a release on a Mac, run `scripts/app.sh --release
&& scripts/sign-release.sh`. Without `SIGN_IDENTITY`, `sign-release.sh`
signs ad hoc and notarizes nothing: a build to try here.

## CI

**Verify** (`.github/workflows/ci.yml`) runs on every pull request, since
its checks are required to merge. On pushes to `main` it runs unless only
Markdown, `docs/` or `.github/screenshots/` changed; a change to
`docs/cli.md` or `docs/mcp.md` runs it, since a test checks their
examples. The release runs it too. A new push to a pull request cancels
its earlier run.

Its jobs: `lint`, on Ubuntu, runs shellcheck, actionlint and `cargo deny
check advisories` (a [RustSec](https://rustsec.org) advisory on a crate
built for either Mac fails it); `rust`, on macOS 26 in `America/Chicago`,
runs the Rust checks of [Test](#test); `app` builds the release app, signs
it ad hoc and runs `scripts/check-app.sh`, but not on a tag, where the
release builds and checks its own.

The `main` ruleset (Settings → Rules) takes changes only by squashed pull
request, once Verify's `lint`, `rust` and `app` pass.

A new advisory can fail `cargo deny` on a pull request that changed no
crate. Update the crate it names.

Actions are pinned by commit. Dependabot (`.github/dependabot.yml`) opens
one pull request a week for the actions and one for the Rust crates, with
versions at least 7 days old. Sparkle is pinned to a version and checksum
in `scripts/sparkle.sh`, and actionlint and cargo-deny to a version and
checksum in `ci.yml`. These are updated by hand.

The `rust` and `app` jobs and the release use Xcode 26.6, named in both
workflows: `rust` compiles the app's wire models for the Rust–Swift
contract test. A newer Xcode comes in a pull request that changes all
three, once
[GitHub's macOS 26 image](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md)
has it.

## Release

**Setup, once.** Releases are signed with a Developer ID and notarized, and
updates are signed for Sparkle. The secrets belong to the `release`
environment (Settings → Environments), which only a `v*` tag can use, so
only the release's `check` and `publish` jobs can read them. The variable
is the repository's.

| Name | Kind | Holds |
|---|---|---|
| `DEVELOPER_ID_CERTIFICATE` | Secret | The Developer ID Application certificate and key, exported as a `.p12` and base64-encoded |
| `DEVELOPER_ID_PASSWORD` | Secret | The `.p12`'s password |
| `NOTARY_KEY` | Secret | An App Store Connect API key: the contents of its `.p8` file |
| `NOTARY_KEY_ID` | Secret | That key's ID |
| `NOTARY_ISSUER` | Secret | That key's issuer ID |
| `SPARKLE_PRIVATE_KEY` | Secret | The update signing key, from `generate_keys -x <file>` |
| `SPARKLE_PUBLIC_KEY` | Variable | Its public half, from `generate_keys -p` |

Sparkle's `generate_keys` is in `target/sparkle/bin/` after a build. It
keeps the key in your login Keychain. Keep the private key safe: without
it, no installed copy will accept an update.

**Each release:**

1. Set `version` under `[workspace.package]` in `Cargo.toml`, and run
   `cargo build` so `Cargo.lock` has it too. The command line and the app
   take their version from it.
2. Add a `## X.Y.Z` section to `CHANGELOG.md`. It becomes the release
   notes, on the GitHub release and in the window that offers the update.
3. Merge to `main`, then push the tag `vX.Y.Z` on that commit. The
   `release tags` ruleset lets only the owner push, move or delete a `v*`
   tag.

The tag starts **Release** (`.github/workflows/release.yml`):

| Job | Does |
|---|---|
| `check` | Checks that the tag matches `Cargo.toml`, is on `main` and isn't released yet; that `CHANGELOG.md` has the version's notes; and that every secret and the variable are set |
| `verify` | All of Verify |
| `build` | Runs `scripts/app.sh --release` while `verify` runs, and hands the app to `publish`, which waits for both, as an artifact |
| `publish` | Fetches Sparkle's tools with `scripts/sparkle.sh`, imports the certificate into a keychain made for the run, runs `scripts/sign-release.sh` with the keys, removes the keychain and the notary key, runs `scripts/check-app.sh` on the result, and publishes the release with the disk image and `appcast.xml` |

The build and the keys are on separate runners. Whatever the build runs,
such as a dependency's build script, has no key to reach. The job that
holds the keys builds nothing, and fetches Sparkle before the keys are
there.

Installed copies read `appcast.xml` from the latest release, so publishing
the release offers the update. If a run fails after creating its draft
release, re-run all its jobs: the draft is replaced. Re-running only the
failed jobs fails after a day, when the unsigned app's artifact expires. A
published release is never changed; tag the next version instead.
