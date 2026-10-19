# Working on Turnscope

One Rust binary, `turnscope` (the command line, an MCP server, and `serve`,
a daemon), and a macOS app in `macos/` that talks to `serve`.
[docs/development.md](docs/development.md#test) has the full check CI
runs, Swift tests included.

## Never use the user's real data

- Run and test against copies, off the network:
  `TURNSCOPE_OFFLINE=1 target/debug/turnscope --data "$(mktemp -d)" --home tests/fixtures/home`.
  Never the data directory
  (`~/Library/Application Support/com.joeychilson.turnscope`) or the
  agents' own folders.
- `turnscope connect <agent>` and `disconnect` change an agent's
  configuration: don't run them unless asked.
- Fixtures, tests and snapshots hold no real ids, emails, keys or session
  text.

## Rules

- **Delete the old way.** When an agent changes what it writes, change its
  reader and remove the code for the old format. When Turnscope renames or
  removes something, the old name stops working: no fallbacks or aliases.
- **One place per rule.** An agent or provider is named only in its own
  file and its registry, `agents::ALL` or `providers::ALL`.
- **Decisions are written where they apply:** what was chosen, over what,
  why, and what was measured. `forecast.rs` and `sessions::handoff` show
  how.
- **Plain wording** in everything the user reads: short sentences, no
  filler, and the same word for the same thing, as the
  [glossary](docs/architecture.md#glossary) defines it.

## When you change X, also update Y

| When you change | Also update |
|---|---|
| A command or flag of the command line, or its output | `docs/cli.md`; its case in `tests/cli.rs`; the README's quick start if it's shown there |
| An MCP tool, its description or inputs, its answer, or the prompt | `docs/mcp.md`; the README's tool list if a name changes |
| The status, an alert, or any `serve` message | In one commit: `contract/`, the Rust types, `Contract.swift`, `docs/protocol.md`. Anything but an added field also raises `serve::PROTOCOL` and `protocolVersion` ([how](docs/protocol.md#change-the-protocol)) |
| An agent, or how an agent's files are read | [docs/adding-an-agent.md](docs/adding-an-agent.md), which lists the docs to update. Raise `ingest::READERS` when a reader would read a file it read before differently |
| A provider | [docs/adding-a-provider.md](docs/adding-a-provider.md), which lists the docs to update |
| The handoff | `docs/handoff.md`; try it on the handoff cases ([development.md](docs/development.md#handoff-cases)) |
| The forecast | The decision written in `forecast.rs`, [architecture.md](docs/architecture.md#forecast); run the backtest |
| The database schema | A new script at the end of `db::REVISIONS`: a shipped one is never edited. The tables in [architecture.md](docs/architecture.md#data) |
| An alert kind or a notification setting | `alerts.rs`; the `CHECK` on `alert.kind`, in a new revision; `alerts::Notify` and the `notify.*` keys in `main.rs` and `docs/cli.md`; the contract (above); `Words+Notes.swift`; `Settings/GeneralTab.swift`; [architecture.md](docs/architecture.md#alerts) |
| Text the app shows | `Words` and its tests; run `scripts/screenshots.sh` if the panel looks different |
| An app setting or launch argument | [docs/app.md](docs/app.md), which says how to add one |
| A time form | `src/time.rs`, the flags' help in `main.rs`, the MCP tools' descriptions, [cli.md](docs/cli.md#time-forms) |
| An environment variable | [cli.md](docs/cli.md#environment) if the binary reads it, otherwise `docs/development.md` |
| A script, workflow, secret or requirement | `docs/development.md` |
| Anything a user would notice | `CHANGELOG.md`, and the README if it describes it |
