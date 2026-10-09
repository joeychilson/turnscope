# Development

How to build, test and release Turnscope. [AGENTS.md](../AGENTS.md) has
the map of the repo, the rules, and what to update when you change
something. [architecture.md](architecture.md) explains how the engine works.

## Requirements

- macOS 26, on Apple silicon or Intel. It's the app's deployment target.
- Rust 1.98.1. `rust-toolchain.toml` pins it with clippy, rustfmt and both
  Mac targets, and rustup installs it on first use.
- Xcode 26, for the app and the Swift tests.
- `shellcheck` and `actionlint`, only to run CI's lint job yourself.

## Build

```sh
cargo build            # target/debug/turnscope
scripts/build-app.sh   # target/xcode/Build/Products/Debug/Turnscope.app
```

The Xcode project builds its own `turnscope` in a build phase that runs
`scripts/build-cli.sh`, and puts it in `Contents/Helpers`. You can also open
`macos/Turnscope.xcodeproj` in Xcode and run the Turnscope scheme.

Run either on a scratch data directory, so your own data is left alone:

```sh
target/debug/turnscope --data /tmp/ts status
open target/xcode/Build/Products/Debug/Turnscope.app --args --data /tmp/ts --open
```

Both still read your agents' history and logins, and ask the providers for
your limits. To leave your agents out too, add `--home` with a copy of a
home, or the test fixture's: `--home "$PWD/tests/fixtures/home"`. To keep
the command line off the network, set `TURNSCOPE_OFFLINE=1`
([cli.md](cli.md#environment)).

The app's other launch arguments are in
[app.md](app.md#launch-arguments).

## Test

Before every commit, run what CI runs:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked
swift test --package-path macos/TurnscopeKit -Xswiftc -warnings-as-errors
shellcheck scripts/*.sh
actionlint
```

CI runs the Rust tests in the `America/Chicago` time zone, so a test that
takes local time for UTC fails there. Run `TZ=America/Chicago cargo test`
to see it here.

| Tests | Where | Cover |
|---|---|---|
| Unit | `#[cfg(test)]` in each module | Each agent's reader on synthetic logs, merging reports in any order, opening the database, prices, the forecast, each provider's answers, alerts, time, redaction, the contract's examples |
| End to end | `tests/cli.rs` | The text and `--json` of `status`, `usage`, `sessions`, `handoff`, `accounts`, `config` and `connect` by snapshot; mistakes and their exit codes; `guard`; `statusline`; an MCP session; `serve` over a real socket |
| Swift | `macos/TurnscopeKit/Tests/` | Decoding and encoding every contract example, the client against a stand-in `serve`, launch arguments, the wording |

**Fixtures.** `tests/cli.rs` reads a home with two synthetic Claude Code
sessions (`tests/fixtures/home`) and writes a database of four synthetic
accounts in SQL. Fixtures never hold real ids, emails, keys or session text.

**Snapshots.** The end-to-end tests compare each answer with its file in
`tests/snapshots/`. After a deliberate change to output, write them again
and review the diff:

```sh
TURNSCOPE_BLESS=1 cargo test --test cli
```

**The real engine from Swift.** One Swift test runs the app's client
against a real `turnscope serve`. It runs only when `TURNSCOPE_BINARY` is
set, so CI skips it. The path must be absolute, because `swift test` runs
in the package's folder:

```sh
cargo build
TURNSCOPE_BINARY="$PWD/target/debug/turnscope" swift test --package-path macos/TurnscopeKit
```

**Variables.** `TURNSCOPE_NOW` and `TURNSCOPE_OFFLINE` are in
[cli.md](cli.md#environment). These are for development only:

| Variable | Does |
|---|---|
| `TURNSCOPE_BLESS` | Writes the snapshots instead of comparing them |
| `TURNSCOPE_BINARY` | The `turnscope` that the Swift engine test and the app run, instead of the bundled one |
| `TURNSCOPE_BACKTEST` | The copy of a data directory the forecast backtest replays |
| `TURNSCOPE_HANDOFF_CASES` | The folder of handoff cases |
| `TURNSCOPE_HANDOFF_LABEL` | The name of the version being tested, in each handoff's file name |
| `TURNSCOPE_HANDOFF_BUDGET` | The handoff's budget in tokens (default 16,000) |

## Forecast backtest

`forecast::tests::backtest` replays real limit history and scores the
forecast: its point error, how often its 80% ranges hold, and how often it
declines to forecast. It needs weeks of history to say much. Run it on a
copy of a data directory, never the real one:

```sh
cp -cR ~/Library/Application\ Support/com.joeychilson.turnscope /tmp/ts-copy
TURNSCOPE_BACKTEST=/tmp/ts-copy cargo test --release backtest -- --ignored --nocapture
```

[architecture.md](architecture.md#forecast) says what the forecast chose
and what it was compared with.

## Handoff cases

`sessions::handoff::handoff_cases` writes the handoff of each case in `<dir>/cut/`,
a real session stopped part way, to `<dir>/cases/<case>/handoff-<label>.md`:

```sh
TURNSCOPE_HANDOFF_CASES=<dir> TURNSCOPE_HANDOFF_LABEL=new cargo test hand_off_each_case -- --ignored
```

[handoff.md](handoff.md) says how the cases are made and how to compare two
versions.

## Scripts

| Script | Does |
|---|---|
| `scripts/build-app.sh` | Builds the app for this Mac into `target/xcode/Build/Products/Debug/Turnscope.app`: Debug, signed ad hoc, warnings as errors, with the version in `Cargo.toml`. Swift packages are resolved into `target/swiftpm/`. |
| `scripts/build-cli.sh` | The Xcode project's "Build turnscope" phase. Builds `turnscope` for each architecture in `ARCHS` (release for a Release build, debug otherwise) and joins them with `lipo` into `BUILT_PRODUCTS_DIR`. Run by hand, it builds for this Mac in debug into `target/helper/`. |
| `scripts/build-release.sh` | Builds the release into `target/dist`: the app for Apple silicon and Intel, `Turnscope-<version>.dmg`, and `appcast.xml`. Its environment is below. |
| `scripts/check-app.sh [app]` | Checks that a built app's `turnscope` runs: `doctor` creates a database, `serve` answers a hello with a status, and `mcp` answers a handshake and a tool list with the app's version. It runs in throwaway folders, `HOME` included. The default app is `target/dist/Turnscope.app`. |
| `scripts/screenshots.sh` | Builds the app with `build-app.sh`, then draws the README's screenshots, light and dark, from `contract/status.notification.json` into `.github/screenshots/`. |
| `scripts/release-notes.sh X.Y.Z` | Prints the version's section of `CHANGELOG.md`, without its heading. Fails if the section is missing or empty. |

`build-release.sh` reads its settings from the environment, so it runs the
same on a Mac and in the release workflow:

| Variable | Holds |
|---|---|
| `SIGN_IDENTITY` | The Developer ID Application identity, by name or hash. Without it, everything is signed ad hoc and nothing is notarized: a build to try here. |
| `NOTARY_KEY` | The path of an App Store Connect API key's `.p8` file |
| `NOTARY_KEY_ID` | That key's ID |
| `NOTARY_ISSUER` | That key's issuer ID |
| `SPARKLE_PUBLIC_KEY` | The public key the app checks updates with |
| `SPARKLE_PRIVATE_KEY` | Its private half, as `generate_keys -x` exports it. It signs the disk image. Without it, there's no appcast. |

With `SIGN_IDENTITY` set, all the others are needed. The script signs the
app from the inside out, the order Sparkle requires: Sparkle's helpers, its
framework, `turnscope`, then the app. It then notarizes and staples the app
and the disk image, and checks that the update's signature passes with
`SPARKLE_PUBLIC_KEY`. It exits 2 when a variable it needs is missing,
`NOTARY_KEY` names no file, or the version isn't `X.Y.Z`.

## CI

**Verify** (`.github/workflows/ci.yml`) runs on pushes to `main` and on pull
requests, unless only Markdown, `docs/` or `.github/screenshots/` changed.
The release runs it too. A new push to a pull request cancels its earlier
run.

| Job | Runner | Steps |
|---|---|---|
| `lint` | Ubuntu 24.04 | `shellcheck scripts/*.sh`, then `actionlint` |
| `rust` | macOS 26, in `America/Chicago` | `cargo fmt --check`, clippy, `cargo test` and `cargo doc`, warnings as errors |
| `app` | macOS 26, Xcode 26 | `swift test` with warnings as errors, `scripts/build-release.sh` unsigned, then `scripts/check-app.sh`. On a tag, only `swift test`: the release builds and checks its own app, signed. |

Actions are pinned by commit. Dependabot (`.github/dependabot.yml`) opens
one pull request a week for the actions and one for the Rust crates, with
versions at least 7 days old. Sparkle is pinned to an exact version in the
Xcode project and is updated by hand.

## Release

**Setup, once.** Releases are signed with a Developer ID and notarized, and
updates are signed for Sparkle. The repository needs these secrets and one
variable:

| Name | Kind | Holds |
|---|---|---|
| `DEVELOPER_ID_CERTIFICATE` | Secret | The Developer ID Application certificate and key, exported as a `.p12` and base64-encoded |
| `DEVELOPER_ID_PASSWORD` | Secret | The `.p12`'s password |
| `NOTARY_KEY` | Secret | An App Store Connect API key: the contents of its `.p8` file |
| `NOTARY_KEY_ID` | Secret | That key's ID |
| `NOTARY_ISSUER` | Secret | That key's issuer ID |
| `SPARKLE_PRIVATE_KEY` | Secret | The update signing key, from `generate_keys -x <file>` |
| `SPARKLE_PUBLIC_KEY` | Variable | Its public half, from `generate_keys -p` |

Sparkle's `generate_keys` is in `target/swiftpm/artifacts/sparkle/Sparkle/bin/`
after a build. It keeps the key in your login Keychain. Keep the private
key safe: without it, no installed copy will accept an update.

**Each release:**

1. Set `version` in `Cargo.toml`, and run `cargo build` so `Cargo.lock`
   has it too. The app takes its version from the crate.
2. Add a `## X.Y.Z` section to `CHANGELOG.md`. It becomes the release
   notes.
3. Merge to `main`, then push the tag `vX.Y.Z` on that commit.

The tag starts **Release** (`.github/workflows/release.yml`):

| Job | Does |
|---|---|
| `check` | Checks that the tag matches `Cargo.toml`, is on `main` and isn't released yet; that `CHANGELOG.md` has the version's notes; and that every secret and the variable are set |
| `verify` | All of Verify |
| `publish` | Imports the certificate into a keychain made for the run, runs `scripts/build-release.sh` signed, runs `scripts/check-app.sh` on the result, and publishes the release with the disk image and `appcast.xml` |

Installed copies read `appcast.xml` from the latest release, so publishing
the release offers the update. If a run fails after creating its draft
release, run it again: the draft is replaced. A published release is never
changed; tag the next version instead.
