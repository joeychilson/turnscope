# Working on Turnscope

Two Rust crates and a native macOS shell: `crates/turnscope`, the engine
with the `turnscope` binary (the command line and an MCP server);
`crates/turnscope-app`, the shared desktop app core (state, wording, view
models, and C ABI); and `apps/macos`, the SwiftUI + AppKit menu bar shell
statically linked to `turnscope-app`.
[docs/development.md](docs/development.md#test) has the full check CI
runs.

## Never use the user's real data

- Run and test against copies, off the network:
  `TURNSCOPE_OFFLINE=1 target/debug/turnscope --data "$(mktemp -d)" --home target/tmp/home`,
  the fixture home `cargo test --test cli` builds. Never the data directory
  (`~/Library/Application Support/com.joeychilson.turnscope`) or the
  agents' own folders.
- Never run the binaries inside `/Applications/Turnscope.app`: the disk is
  case-insensitive, so its `turnscope` starts the app on the real data.
- `turnscope connect <agent>` and `disconnect` change an agent's
  configuration: don't run them unless asked.
- Fixtures, tests and snapshots hold no real ids, emails, keys or session
  text.

## Rules

- **Less code.** Before adding, look for what to remove. No unused
  options, layers, wrappers or fallbacks; no code for cases that don't
  happen.
- **Delete the old way.** When an agent changes what it writes, change its
  reader and remove the code for the old format. When Turnscope renames or
  removes something, the old name stops working: no fallbacks or aliases.
- **One place per rule.** An agent or provider is named only in its own
  file and its registry, `agents::ALL` or `providers::ALL`. Logic used
  twice is written once.
- **Comments say only what the code can't:** a measured number behind a
  constant, an approach tried and dropped, or an outside fact (what an agent
  writes and the version it was read from, how a provider answers). A line
  or two. No history, no restating the code. Why a change was made goes in
  its commit.
- **Tests check what a user or agent sees,** the fewest that fail without
  the behavior. No tests of internals, no padding.
- **Measure or don't claim.** A speed, size or reliability claim in code,
  docs or a PR comes with its numbers.
- **Plain wording** in everything the user reads: short sentences, no
  filler, and the same word for the same thing, as the
  [glossary](docs/architecture.md#glossary) defines it.

## When you change X, also update Y

| When you change | Also update |
|---|---|
| A command or flag of the command line, or its output | `docs/cli.md`; its case in `crates/turnscope/tests/cli.rs`; the README's command line section if it's shown there |
| An MCP tool, its description or inputs, or its answer | `docs/mcp.md` |
| The status, an alert, or what the engine tells an app | The Rust types; `crates/turnscope/tests/fixtures/status.json` for a status field; `crates/turnscope-app/src/state.rs` and `view.rs` (and `apps/macos/Sources/Bridge.swift` for view fields) |
| An agent, or how an agent's files are read | [docs/adding-an-agent.md](docs/adding-an-agent.md). Raise `ingest::READERS` when a reader would read a file it read before differently |
| A provider | [docs/adding-a-provider.md](docs/adding-a-provider.md) |
| The forecast | [architecture.md](docs/architecture.md#forecast); run the backtest |
| The database schema | A new script at the end of `db::REVISIONS`: a shipped one is never edited. The tables in [architecture.md](docs/architecture.md#data) |
| An alert kind or a notification setting | `alerts.rs`; the `CHECK` on `alert.kind`, in a new revision; `alerts::Notify`; `crates/turnscope-app/src/words.rs` and `apps/macos/Sources/Settings/GeneralTab.swift`; [architecture.md](docs/architecture.md#alerts) |
| Text the app shows | `crates/turnscope-app/src/words.rs` and its tests, or the view that shows a fixed label; run `scripts/screenshots.sh` if the panel looks different |
| An app setting or launch argument | [docs/app.md](docs/app.md) |
| A time form | `src/time.rs`, the flags' help in `main.rs`, the MCP tools' descriptions, [cli.md](docs/cli.md#time-forms) |
| An environment variable | [cli.md](docs/cli.md#environment) if the binary reads it, otherwise `docs/development.md` |
| A script, workflow, secret or requirement | `docs/development.md` |
| Anything a user would notice | `CHANGELOG.md`, and the README if it describes it |
