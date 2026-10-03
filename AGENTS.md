# Project conventions

## What this is

Turnscope is a macOS menu bar app that reads the history coding agents keep on this Mac: their sessions, what they used, and their subscriptions' limits. For people, it is a menu bar item, its panel and notifications, about limits: whether they can keep going. For agents, it is an MCP server, serving what people don't need to see: pacing themselves against limits, what used a limit and why, and another agent's work to pick up.

It is two programs in one bundle. The engine and the command line are Rust (`crates/`): they read the history, keep the ledger, work out every figure and rule, and serve MCP. The app is Swift (`macos/`): it shows what the engine says and nothing else, and talks to it over one contract (`contract/`).

The goal is an app that needs only bug fixes and new agents. Favor designs that settle a question once over ones that are quicker now and need revisiting.

## Where knowledge lives

- The code documents itself. Each module's documentation says what it is responsible for, how it works and why; each public item says its contract. Read a module's documentation before changing it, and change the documentation in the same commit as the behavior.
- Measured findings about an agent's format belong in its reader's module documentation (`crates/engine/src/agent/<agent>.rs`), with their numbers and the date they were measured. A finding nobody measured does not belong there.
- What the engine and the app say to each other is `contract/`: a feed, the other lines `turnscope watch` writes, and every request. Both sides' tests read it.

## Workspace

| Path                   | What                                                                                                  |
| ---------------------- | ----------------------------------------------------------------------------------------------------- |
| `crates/engine/`       | `turnscope-engine`: reading agents' history, the ledger, pricing, limits, queries. No interface.       |
| `crates/mcp/`          | `turnscope-mcp`: the MCP server, over the engine's public API only.                                   |
| `crates/cli/`          | `turnscope`: the command line, `turnscope watch` (the app's feed), and connecting agents.             |
| `macos/Turnscope/`     | The menu bar app's sources, in `macos/Turnscope.xcodeproj`, with its icon, `Turnscope.icon`.          |
| `macos/TurnscopeKit/`  | A Swift package the app uses: the feed, the engine client, words. Tested with `swift test`, no screen. |
| `contract/`            | The lines `turnscope watch` and the app exchange, which both sides' tests check.                     |

Add a crate, a target or a package only when a responsibility needs a boundary the compiler enforces. `macos/Logos/` holds the providers' logos, bundled as a folder. The Xcode project keeps no list of files: `macos/Turnscope/` is a synchronized folder, so a file added there is in the app. Its one other phase builds the command line for every architecture being built (`scripts/build-helper.sh`) and copies it into `Contents/Helpers`, signed. Its one dependency beside `TurnscopeKit` is Sparkle, pinned exactly, with the version resolved committed.

The app is `Turnscope.app`: the app at `Contents/MacOS/Turnscope`, and the command line at `Contents/Helpers/turnscope`, which the app runs as `turnscope watch` and agents' MCP configurations run as `turnscope mcp`. `scripts/` builds it for development (`build.sh`) and for release (`release.sh`), checks a built app runs (`smoke.sh`), prints a release's notes from `CHANGELOG.md` (`notes.sh`), draws the README's screenshots from the contract's feed (`screenshots.sh`), and refreshes the price catalog and the logos from models.dev (`catalog.sh`). Both builds resolve Swift packages into `target/swiftpm/`.

## Engine rules

These are the invariants the numbers depend on.

- Readers produce observations and nothing else. The ledger stores what each artifact said, against that artifact, and never anything derived from it. Merged usage, costs and totals are computed from the ledger into a cache that can always be deleted and built again.
- Combining reports must not depend on order: taking the largest of each count, the earliest start, the most authoritative title. A rule that gives a different answer when files are read in a different order is a bug. Keep the in-memory and SQL versions of each rule identical, and cover them with the order and growth tests in `crates/engine/tests/`.
- Unknown is never zero. A count, price or limit that is missing stays `None` and is shown as unknown. A cost says what of it is not at list prices: what a provider billed, or an agent's own estimate.
- Anything a reader does not recognize becomes a diagnostic: a record type, a part, a usage field, a value out of range. Never skip it silently. A count out of range invalidates its record rather than being clamped into a plausible one. Each reader lists the kinds it knows and passes over, so a new kind is noticed.
- What a reader records as said, for search, is exactly what its conversation shows as the person's and the models' entries. Text an agent injects is neither, even when it arrives as the person's message. Every reader's tests check this with `agent::tests::assert_said_as_shown`.
- Increase a reader's `version()` whenever a change would read an existing artifact differently. Only that agent's artifacts are read again. Increase the cache's `SCHEMA` whenever its schema or anything it works out changes; the cache is then built again.
- Ledger migrations only ever add. Never edit a migration that has shipped.
- An agent's own totals are the reference. Usage the transcripts don't show is recorded as outside the conversation, never dropped and never spread invisibly into other figures.
- Sums stop at `i64::MAX`, in SQL and in Rust alike, rather than fail or wrap; a cost that reaches it is unknown.
- The engine never writes to agents' files or sign-ins, and never refreshes a credential. Connecting an agent, on the person's click, runs the agent's own command for adding an MCP server (`crates/cli/src/connect.rs`); nothing writes an agent's files itself.

## Rust

- Name modules for stable responsibilities, such as `ledger.rs` or `price.rs`. Do not create files for individual functions or steps, and do not add `util.rs`, `utils.rs`, or `helpers.rs`.
- Keep unit tests in one inline `#[cfg(test)] mod tests` at the bottom of the file they test, after every production item, with their imports and fixtures inside that module. Integration tests exercise a crate's public API from its `tests/` directory, in files named for the behavior they cover. Do not create `tests.rs`, `test.rs`, `*_test.rs`, or `*_tests.rs`.
- Parsing and arithmetic tests need independently established expected values: fixtures shaped like the agents' real records, with totals worked out by hand and the arithmetic in a comment. A round trip through the same code can hide a matching pair of bugs.
- A test of an invariant must fail when the invariant is broken. When adding one, break the rule on purpose once and watch the test fail.
- Untrusted input produces diagnostics or typed errors, never panics. Validate lengths and bounds before allocating, use checked or saturating arithmetic according to the contract, and avoid unchecked narrowing conversions, ignored errors, and success-shaped fallbacks for invalid data. Reserve `expect` for documented internal invariants.
- Prefer concrete types and ordinary control flow. Introduce traits, generics, builders and macros only for code that already uses them; the agent reader trait qualifies because every agent implements it. Add a variant, method or field when its first use arrives, not before, and remove what nothing uses.
- Keep items `pub(crate)` unless another crate needs them. The engine's public API is what `lib.rs` re-exports.
- Use domain types where they prevent mistakes of identity, unit or time: `SessionKey`, `Tokens`, `Usd`, `Instant`, `ModelKey`.
- Document public items with their contracts, bounds, errors, and non-obvious behavior. Comments explain why or record an invariant; they do not narrate syntax or development history.
- Warnings are errors. Do not add `#[allow]`, relax the workspace lints, or weaken a check to make work pass; a necessary exception is a decision to raise. `unsafe` is forbidden: native macOS work belongs in the Swift app.

## The contract

- Every rule that decides what the app says is on the Rust side: how a limit stands (`LimitState::standing`), which limit matters (`AccountLimits::deciding`), the order accounts are listed in, what used a limit and the advice drawn from it (`crates/cli/src/feed.rs`). The app only lays out what the feed says and puts it into words. A rule in Swift is a rule decided twice.
- Formatting (numbers, times, relative dates, sentences) belongs to the app (`macos/TurnscopeKit/Sources/TurnscopeKit/Words.swift`), never the engine. The feed carries facts: percents, instants in RFC 3339 UTC, keys.
- A change to what crosses the contract changes `contract/`, `feed::VERSION` and the app's reading of it in the same commit. Write the files from the Rust side with `TURNSCOPE_WRITE_CONTRACT=1 cargo test -p turnscope`, and review the diff.
- `turnscope watch` writes a feed only when it says something the last didn't, and no more than once in 100 ms; it stops when its input closes, so it never outlives the app. The app starts it again if it ends, after a wait that doubles to a minute, and tells it again whether the panel is open.
- What used each limit, the costliest part of a feed, is worked out only while the panel is open: the app says so with `{"do": "panel", "open": true}`, and the feed is worked out again at once. With the panel closed, a feed takes milliseconds, and the app wakes only while something it shows counts down.

## The app

- The app is grayscale until something needs the person: amber for a limit that at this pace runs out before it resets, red for one used up. The one other color is green, `active` in `Style.swift`, for a session at work this moment, since it asks nothing of the person. Native controls keep the system's accent. A calm panel means the person can keep going.
- Every limit is said in words first, in one sentence (`Words.sentence`): when it runs out at this pace and how long before it resets, or about how much will be left when it resets, or when a used-up one is back. Figures and bars back the sentence up. Limits read as what is left, rounded down; times read as a person says them, "Thu 9 AM", and stretches as "1h 30m".
- A limit belongs to an account, not an agent. The panel shows every account in use, most urgent first, each a row that opens in place; the only one in use shows its limits unopened. The rest fold into one line, where an account is hidden, and those hidden gather at its end. Settings is a page of the panel in three tabs, General, Agents and Accounts, sliding in from the side it lies on; there are no other windows. An agent in use that isn't connected is said in one line the person can put away.
- The menu bar item and the panel are AppKit's, a status item and a borderless panel, with SwiftUI inside. The panel's window is still and transparent, and the panel draws its own glass, so its height moves with what is inside it and nothing jumps. Don't use `MenuBarExtra`: it resizes its window as content changes, and content overlaps while it does.
- What moves, moves on one spring (`spring` in `Style.swift`). New content fades in once its room has opened (`appears`), so nothing moving passes over text.
- The app holds no rule and no state the engine keeps: an account the person hides is hidden in the engine, which the MCP server heeds too. Only preferences of the app's own, what the menu bar shows and which notifications are sent, are the app's, in its defaults.
- Connect finds agents' commands on the PATH the person's interactive login shell has (`$SHELL -ilc`), where installers add their folders, and runs them with it, each with a time limit.
- Views read a `Store`, which the engine client feeds. Engine work never runs on the main thread: it runs in `turnscope watch`, a process of its own.
- Every control is reachable from the keyboard and named for VoiceOver by what it does; the panel closes on Escape, and its pages go back on it. Text a person reads can be selected unless it sits in a row that chooses something.
- The app is Swift 6 with complete concurrency checking; UI types are `@MainActor`. Keep `TurnscopeKit` free of AppKit and SwiftUI, so what can be tested without a screen is.

## Privacy and the network

- Agents' files and sign-ins are only read. The data directory the engine makes is its owner's alone.
- Network requests go to three kinds of destination: providers' usage endpoints, using sign-ins and keys the agents already keep, which are each subscription's and OpenRouter's key endpoint (`https://openrouter.ai/api/v1/key`) for the limit an OpenRouter key carries; models.dev for prices; and GitHub, for Turnscope's own releases. The engine's go through `/usr/bin/curl`, run from Rust, with any credential on standard input. The app's one request is Sparkle's, for the appcast and a release's disk image on GitHub (`macos/Turnscope/Updater.swift`). Adding a destination is an architectural change, not a convenience.
- No telemetry.

## Verification

- Run `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked`, `swift test --package-path macos/TurnscopeKit` and `scripts/build.sh`, which builds the app with warnings as errors. CI (`.github/workflows/ci.yml`) runs these on every push to main and every pull request, on macOS 26 with Xcode 26, and then `scripts/release.sh` and `scripts/smoke.sh`, as a release builds.
- Scripts are bash 3.2, as macOS ships it, with `set -euo pipefail`; they pass `shellcheck`, and the workflows pass `actionlint`. Workflows pin each action to a commit, its version in a comment.
- Checkouts never share a target directory. Cargo names a workspace's crates by their path within it, so a second checkout building into the same target takes the first's builds for its own.
- For any change to a reader, merging or pricing, run `cargo run --release -p turnscope -- doctor --strict --data <scratch dir>` against this Mac's real history. Compare its figures with the last run and explain every change. `--strict` fails when anything could not be read or was not understood; handle it, or record why in the reader's documentation.
- For any change to updating or releasing, run `scripts/release.sh` with the release's environment (it says what that is) and install the image it makes over an older release, as an update does (`docs/releasing.md`).
- For any change to what the app shows, look at it before calling it done: `scripts/build.sh`, then `open target/xcode/Build/Products/Debug/Turnscope.app --args --data <scratch dir> --open` opens the panel on this Mac's history (`--open account` opens the first account in use, `--open resting` lists the accounts not in use, and `--open settings`, `--open agents` and `--open accounts` those tabs of Settings). `--snapshot <dir> --fixture <feed.json>`, with the same `--open`, draws that state off-screen in light and dark. `scripts/screenshots.sh` draws the README's screenshots again; run it when a change shows in them. Run from Xcode, the scheme passes `--data /tmp/turnscope-dev`. Never drive the person's own pointer or keyboard. Notifications post only from an app in an Applications folder.
- Pass `--data` with a scratch directory during development, so the app's own ledger is never written by work in progress.
- Keep performance within these targets, and measure in a release build before claiming an improvement. Measured on 2026-09-30 in release builds over this Mac's history, about 180K responses in 1,830 sessions: launch to the menu bar 138–172 ms; the first feed 0.6 s; a feed's engine work 22 ms with the panel open and 1–8 ms with it closed; a line an agent writes caught up in 20–23 ms warm; idle, 2 wakeups a minute; memory, the app 23 MB and `turnscope watch` 15–34 MB. Panel open to fully drawn is not yet measured.

| Measure                                           | Target                                                                                                                                                |
| ------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| Launch to menu bar item                           | under 300 ms                                                                                                                                          |
| Panel open to fully drawn                         | under 50 ms                                                                                                                                           |
| The first feed after launch                       | under 1 s                                                                                                                                             |
| A feed worked out again                           | under 50 ms                                                                                                                                           |
| A line an agent writes reaching the panel         | under 250 ms                                                                                                                                          |
| CPU and wakeups with nothing open and nothing new | none but a look, every 30 s, at where sign-ins are kept and for any agent's missing history folder, a feed each minute, and Sparkle's daily look for a new release |
| Memory, app and engine together                   | under 120 MB                                                                                                                                          |

## Releases

- A release is never made unsigned: signed with the Developer ID, with the hardened runtime and a secure timestamp, notarized and stapled, and offered in an appcast signed with Sparkle's key. Without a Developer ID, `scripts/release.sh` signs ad hoc and notarizes nothing, to try a release build here, as CI does on every push to main and every pull request; its header says what it builds and what it needs.
- A release is a tag `vX.Y.Z` matching the workspace version in `Cargo.toml`, on a commit on main, with its section in `CHANGELOG.md`, which becomes its notes. Every copy installed reads the latest release's `appcast.xml`, so a version published is one every copy is offered: it is never published and then taken back.
- The Sparkle key is made once with Sparkle's `generate_keys` and kept in the keychain of whoever made it. Losing it means no copy installed can be updated again, so keep it backed up.
- `docs/releasing.md` is the checklist: the secrets and variable a release needs, setting the repository up, and cutting a release.

## Commits

- Use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) for commit subjects: `type(scope): description`. Scope is optional for changes that span the project.
- Types: `feat` adds a capability; `fix` corrects behavior; `perf` improves measured performance; `refactor` changes structure without changing behavior; `docs`, `test`, `build`, `ci`, and `style` cover their respective concerns; `chore` covers other maintenance. Choose the primary purpose of the change.
- Use a stable scope naming an area rather than a file path or function, such as `engine`, `pricing`, `limits`, `feed`, `app`, `panel`, `mcp` or `release`. Use `repo` or `deps` for shared concerns. Scopes use lowercase letters, digits, and hyphens.
- Keep the complete subject at most 72 characters. Start the description with a lowercase imperative verb, omit the final period, and describe the actual result. Examples: `feat(engine): read Codex history`, `fix(pricing): charge 1-hour cache writes at twice the input rate`.
- Make each commit coherent and buildable, keeping implementation together with its tests. Keep unrelated cleanup out of feature and fix commits, and use the body for motivation, measurements, and non-obvious decisions the subject cannot carry.
- A change people will notice adds a line under `## [Unreleased]` in `CHANGELOG.md`, in the same commit, written for the people using the app: it becomes the release's notes, which Sparkle shows before an update installs.
- Mark an incompatible change to a stored format, a command's interface, or a setting with `!` before the colon and a `BREAKING CHANGE:` footer explaining the impact. Include measurements for `perf` claims.
