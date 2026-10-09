# Working on Turnscope

Turnscope shows how much of a coding agent's usage limits is left, whether
it will last until the reset, and what used it. It is one Rust binary,
`turnscope` (the command line, an MCP server, and `serve`, a daemon), and a
macOS menu bar app that talks to `serve`.

## Repo map

| Path | What |
|---|---|
| `src/` | The one crate: the command line, the MCP server, `serve`, and what they share |
| `tests/cli.rs` | The binary end to end, over `tests/fixtures/home`, compared with `tests/snapshots/` |
| `contract/` | An example of every message between the app and `serve`; both sides' tests read them |
| `macos/` | The app: `Turnscope.xcodeproj`, the app target `Turnscope/`, the Swift package `TurnscopeKit/`, and `Logos/` |
| `scripts/` | Build, check, screenshot and release scripts |
| `docs/` | The docs, listed at the end |
| `.github/` | The CI and release workflows, Dependabot, the README's screenshots |

| File | Holds |
|---|---|
| `src/main.rs` | The commands and their flags, `catch_up`, `guard`, `statusline`, `doctor`, exit codes |
| `src/agents/` | The `Agent` trait, `agents::ALL`, one file per agent |
| `src/providers/` | The `Provider` trait, `providers::ALL`, one file per provider, `ask` (curl) |
| `src/db.rs` | The schema, as scripts in `REVISIONS` |
| `src/ingest.rs` | Agents' files into responses, sessions and search; `READERS` |
| `src/limits.rs` | Logins into accounts, sign-ins and limit readings |
| `src/forecast.rs` | Pace, outlook, range and work left, and the backtest |
| `src/status.rs`, `src/status/text.rs` | The status every front end shows, and its text |
| `src/sessions.rs`, `src/sessions/handoff.rs` | Finding, showing and reading sessions; the handoff |
| `src/mcp.rs` | The MCP server: `TOOLS`, the caller, the prompt |
| `src/serve.rs` | The app's daemon: JSON-RPC on a Unix socket, `PROTOCOL` |
| `macos/Turnscope/` | `App.swift` (start-up, the panel's window, which `turnscope` runs), `Panel/`, `Settings/`, `Notifier.swift`, `Updater.swift` |
| `macos/TurnscopeKit/Sources/TurnscopeKit/` | `Contract.swift` (the protocol's types), `Client.swift` (the connection to `serve`), `Store.swift` (what views show), `Words*.swift` (every string the user reads), `Launch.swift` |

[docs/architecture.md](docs/architecture.md) has every module and how they
work together. [docs/app.md](docs/app.md) covers the app.

## Commands

```sh
cargo build                                  # target/debug/turnscope
scripts/build-app.sh                         # the app, Debug, for this Mac
TURNSCOPE_BLESS=1 cargo test --test cli      # write the snapshots again, then review the diff
```

Run the binary on scratch data, off the network, over the fixture home:

```sh
TURNSCOPE_OFFLINE=1 target/debug/turnscope --data "$(mktemp -d)" --home tests/fixtures/home sessions
```

The full check, as CI runs it:

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked
swift test --package-path macos/TurnscopeKit -Xswiftc -warnings-as-errors
shellcheck scripts/*.sh
actionlint             # when a workflow changed
```

[docs/development.md](docs/development.md) has the rest: the real-engine
Swift test, the backtest, the scripts, CI and releases.

## Rules

- **Every line has a reason.** A function exists when it holds real logic
  or is an extension point. A string is written where it's used.
- **No compatibility shims.** Nothing is kept for old versions of Turnscope
  or of the agents. A reader reads what its agent writes today.
- **Comments say why**, not what. A reader's notes on its agent's format go
  at the top of its file, with what was measured and when.
- **One place per rule.** In code, an agent or provider is named only in its
  own file and its registry.
- **Decisions are written where they apply:** what was chosen, over what,
  why, and what was measured. `forecast.rs` and `sessions::handoff` show
  how.
- **Never use the user's real data.** Test against copies with `--data` and
  `--home`, never the data directory
  (`~/Library/Application Support/com.joeychilson.turnscope`) or the
  agents' own folders. `turnscope connect <agent>` and `disconnect` change
  an agent's configuration: don't run them unless asked.
- **Fixtures are made up.** Fixtures, tests and snapshots hold no real ids,
  emails, keys or session text.
- **The contract changes in one commit:** `contract/`, the Rust types,
  `Contract.swift` and `docs/protocol.md` together, with both sides' tests
  passing ([how](docs/protocol.md#change-the-protocol)).
- **Plain wording** in everything the user reads: command output, MCP
  answers, the app and the docs. Short sentences in ordinary word order and
  no filler. The same word for the same thing everywhere, as the
  [glossary](docs/architecture.md#glossary) defines it.
- **Lints.** Clippy denies `unwrap`, `dbg!` and `todo!` outside tests, and
  `unsafe` is forbidden.

## When you change X, also update Y

| When you change | Also update |
|---|---|
| A command or flag of the command line, or its output | `docs/cli.md`; its case in `tests/cli.rs` and the snapshots; the README's quick start if it's shown there |
| An MCP tool, its description or inputs, its answer, or the prompt | `docs/mcp.md`, `tests/snapshots/mcp.json`; the README's tool list if a name changes |
| The status, an alert, or any `serve` message | `contract/`, the Rust types, `Contract.swift`, `docs/protocol.md`, and the snapshots. Anything but an added field also raises `serve::PROTOCOL` and `protocolVersion` ([how](docs/protocol.md#change-the-protocol)) |
| An agent, or how an agent's files are read | [docs/adding-an-agent.md](docs/adding-an-agent.md), which lists the docs to update. Raise `ingest::READERS` when a reader would read a file it read before differently |
| A provider | [docs/adding-a-provider.md](docs/adding-a-provider.md), which lists the docs to update |
| The handoff | `docs/handoff.md`, the `handoff*` snapshots and `mcp.json`; try it on the handoff cases ([development.md](docs/development.md#handoff-cases)) |
| The forecast | The decision written in `forecast.rs`, [architecture.md](docs/architecture.md#forecast); run the backtest |
| The database schema | A new script at the end of `db::REVISIONS`: a shipped one is never edited. The tables in [architecture.md](docs/architecture.md#data) |
| An alert kind or a notification setting | `alerts.rs`; the `CHECK` on `alert.kind`, in a new revision; `alerts::Notify` and the `notify.*` keys in `main.rs` and `docs/cli.md`; the contract (above); `Words+Notes.swift`; `Settings/GeneralTab.swift`; [architecture.md](docs/architecture.md#alerts) |
| Text the app shows | `Words` and its tests; run `scripts/screenshots.sh` if the panel looks different |
| An app setting or launch argument | [docs/app.md](docs/app.md), which says how to add one |
| A time form | `src/time.rs`, the flags' help in `main.rs`, the MCP tools' descriptions, [cli.md](docs/cli.md#time-forms) |
| An environment variable | [cli.md](docs/cli.md#environment) if the binary reads it, otherwise `docs/development.md` |
| A script, workflow, secret or requirement | `docs/development.md` |
| The Rust or macOS version | `rust-toolchain.toml` and `rust-version` in `Cargo.toml`; or the deployment target in the Xcode project and `Package.swift`. Then the requirements in `docs/development.md` and the README |
| Anything a user would notice | `CHANGELOG.md`, and the README if it describes it |

## Docs

- [README.md](README.md): for users. What it is, install, quick start,
  connecting agents, privacy, uninstall.
- [docs/architecture.md](docs/architecture.md): how the engine works: data,
  code, processes, status, prices, forecast, alerts, security, and a
  glossary.
- [docs/cli.md](docs/cli.md): every command and flag, names, time forms,
  exit codes, environment, guard and status line.
- [docs/mcp.md](docs/mcp.md): connecting agents, each tool and when to call
  it, the prompt, the protocol and errors.
- [docs/handoff.md](docs/handoff.md): what a handoff holds, its budget, and
  how to test a change to it.
- [docs/protocol.md](docs/protocol.md): the JSON-RPC protocol between the
  app and `serve`.
- [docs/app.md](docs/app.md): the macOS app: its code, data flow, launch
  arguments, notifications, updates, settings, logos and screenshots.
- [docs/development.md](docs/development.md): requirements, build, test,
  scripts, CI and releases.
- [docs/adding-an-agent.md](docs/adding-an-agent.md) and
  [docs/adding-a-provider.md](docs/adding-a-provider.md): step-by-step
  guides.
- [CHANGELOG.md](CHANGELOG.md): what changed in each version; it becomes
  the release notes.
