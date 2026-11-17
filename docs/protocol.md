# The app's protocol

The menu bar app talks to `turnscope serve` over JSON-RPC 2.0 on a Unix
socket. The server is `src/serve.rs`. The app's side is `Client.swift` and
`Contract.swift` in `macos/TurnscopeKit` ([app.md](app.md)).

## The examples

[`contract/`](../contract) holds a hand-written example of every message,
one file per message, covering every enum value. A file is named for its
method and what it is: `hello.request.json`, `hello.result.json`,
`status.notification.json`. `done.result.json` is the empty result, and
`error.json` is an error. The whole status is in the notification:
`hello.result.json` has only the account that shows `expired`. The examples
are pretty-printed. On the socket, each message is one line.

Both sides test against the examples, so they can't drift apart:

- The app's `ContractTests.swift` decodes every result and notification, and
  checks that each request the app writes is exactly its example. It fails
  on an example it doesn't check.
- `serve::tests` reads every status and alert in the examples into the Rust
  types and writes them back unchanged. It also checks that every value of
  every enum in them is in an example.
- `tests/cli.rs` sends every request example to a running `serve` and expects
  a result. It skips `agent.connect` and `agent.disconnect`, which would run
  an agent's own command, and sends `shutdown` last.

## Transport

- **Socket.** `serve.sock` in the data directory, or
  `$TMPDIR/turnscope-<hash>.sock` when that path is longer than the 103
  bytes macOS allows. Get it from `turnscope serve --socket [--data <dir>]`
  instead of working it out.
- **Access.** Only the owner can use it: the socket is 0600, in a 0700
  directory.
- **Framing.** JSON-RPC 2.0, one UTF-8 message per line.

Try it by hand, with the app running or after `turnscope serve &`:

```sh
nc -U "$(turnscope serve --socket)"
{"jsonrpc":"2.0","id":1,"method":"hello"}
```

## Lifecycle

- **One per data directory.** `serve` holds `serve.lock`. A second `serve`
  prints `turnscope serve: already running for <dir>` and exits 0.
- **Starting.** The app runs `turnscope serve --socket` with its `--data`
  and `--home` to get the socket's path, and `turnscope --version` to get
  its engine's name. Then it connects. If nothing listens, it starts
  `turnscope serve` with the same `--data` and `--home`, and tries to
  connect every 100 ms for 5 seconds. If that `serve` exits with an error
  first, the app shows the last line it printed on standard error as why.
  Its standard error is a file, not a pipe: once the app quit, a pipe
  would end `serve` the first time it printed anything.
- **Reconnecting.** After a dropped connection or a failed attempt, the app
  waits 0.5 s, doubling each time up to 30 s. The wait goes back to 0.5 s
  only after a connection has held for 30 s, so an engine that keeps failing
  isn't started twice a second. A `serve` that doesn't answer `hello` within
  30 s counts as a failed attempt. That's long because `hello` works out the
  whole status, which is slow on a long history. A `status` or `alert` the
  app can't read drops the connection.
- **Exiting.** `serve` exits 60 seconds after its last client leaves, or
  after it starts if no client connects. An app restart or update reconnects
  in that time without reading anything again. `shutdown` makes it exit now.
  If the thread that keeps the data current fails, `serve` exits with an
  error, and the app starts a new one.
- **Another engine.** The app only talks to the engine it ships with. The
  `hello` result names the server, as `turnscope --version` prints it
  (`turnscope 0.1.0`), and the app checks that before reading the rest. If
  it names another engine, the app sends `shutdown`, waits up to 5 s for
  that `serve` to exit, and connects again, starting its own `serve`. It
  does this once per run while another Turnscope app runs. If the next
  `serve` runs another engine too, that app keeps starting it: the app says
  `Turnscope 0.1.0 is also running; quit it to use this one`, and tries
  again as after a failed attempt. So two app versions can't keep stopping
  each other's `serve`. Once no other app runs, a `serve` it left is asked
  to exit at the next try: each try would otherwise keep it from ever
  being alone for 60 s.

## Handshake

The first request on a connection is `hello`. It takes no parameters, and
any sent are passed over. Its result names the engine, and holds the whole
status and every alert not yet acknowledged, so each connection starts in
sync. Until `hello` succeeds, any other request but `shutdown` gets
`-32002`.

```json
{"id":1,"jsonrpc":"2.0","method":"hello"}
```

```json
{"id":1,"jsonrpc":"2.0","result":{"alerts":[…],"server":"turnscope 0.1.0","status":{…}}}
```

## Requests

| Method | Params | Result | Does |
|---|---|---|---|
| `hello` | `{protocol}` | `{server, status, alerts}` | The handshake. `alerts` are oldest first |
| `alerts.ack` | `{ids}` | `{acknowledged}` | Marks alerts as shown, so they aren't sent again. `acknowledged` counts those newly marked. Unknown ids are skipped |
| `limit.breakdown` | `{account, limit}` | `{sessions: [UsedMost]}` | The sessions that used most of a limit |
| `settings.set` | `{settings}` | `{}` | Saves the settings, then pushes the status that carries them |
| `account.hide` | `{account, hidden}` | `{}` | Hides an account from lists and alerts, or shows it again, then pushes the status. An id that names no account changes nothing |
| `agent.connect` | `{agent}` | `{}` | Adds Turnscope's MCP server to an agent with the agent's own command |
| `agent.disconnect` | `{agent}` | `{}` | Removes it with the agent's own command |
| `shutdown` | none | `{}` | Answers, then exits |

- **`limit.breakdown`** takes an account's `id` and a limit's `key`. It
  answers with up to 3 sessions that cost the most on that account in the
  limit's window, the most first, counted as `usage --limit` counts them
  ([cli.md](cli.md#usage)). It looks the limit up in the status last
  pushed, the one the app shows. An account or limit not in it gets
  `-32000`.
- **`settings.set`** takes the whole `Settings`. A field left out is set to
  its default, which turns that notification on.
- **`agent.connect`** and **`agent.disconnect`** take an agent's `id` from
  `status.agents`. The agent's command can take minutes. `serve` runs it on
  another thread and answers when it's done, and answers other requests
  meanwhile. On success it pushes the status. An unknown agent gets
  `-32602`. A command that fails gets `-32000` with its reason.

`turnscope accounts hide|show` hides or shows an account from the command
line. A running `serve` pushes the status that carries the change within a
minute.

```json
{"id":3,"jsonrpc":"2.0","method":"limit.breakdown","params":{"account":"anthropic:acct-1:org-1","limit":"five_hour"}}
```

```json
{"id":3,"jsonrpc":"2.0","result":{"sessions":[{"agent":"claude-code","project":"app","session":"claude-code:0f6e3f6a-713c","sharePercent":31.5,"title":"Fix the parser"}]}}
```

## Notifications

`serve` pushes these. A message the client sends without an `id` is
ignored.

| Method | Params | When |
|---|---|---|
| `status` | `Status` | When anything in it but `at` changed |
| `alert` | `Alert` | When an alert is raised, right after the `status` that raised it |

`serve` works out the status again after a catch-up and after a request
that changes it ([architecture.md](architecture.md#serve) says when). An
agent connected or disconnected outside the app, as with `turnscope
connect`, shows in `agents` within a minute.

Pushes go only to clients that have said `hello`. A client that doesn't
take a message within 2 seconds is dropped.

```json
{"jsonrpc":"2.0","method":"alert","params":{"account":"anthropic:acct-1:org-1","accountTitle":"Claude Max (me@example.com)","id":1,"kind":"runningOut","limit":"five_hour","limitName":"5 hours","replaces":"limit:anthropic:acct-1:org-1:five_hour","resets":{"at":1791471600000,"horizon":"today"},"runsOut":{"at":1791463800000,"horizon":"soon"}}}
```

## Errors

| Code | Meaning | Message |
|---|---|---|
| `-32700` | Not JSON. The `id` is null | `not JSON: <why>` |
| `-32600` | No `method` | `a request needs a method` |
| `-32601` | No such method | `no method <method>` |
| `-32602` | Parameters that don't fit, or an unknown agent | `parameters that don't fit: <why>`, `no agent is <id>` |
| `-32000` | The engine couldn't do it (`error.json`) | Why, such as `no limit <key> of <account>` |
| `-32002` | A request before `hello` | `send hello first` |

## Conventions

- Field names and enum values are camelCase.
- In what `serve` sends, every field is always present. A field with no
  value is `null`, never left out.
- Times are integer UTC milliseconds. Every time to come is a `Moment`.
- Money is a JSON number of dollars.
- `leftPercent` and the ends of a `Spread` are integers. Hours and rates
  are numbers to a tenth. `sharePercent` is a number of points, which may be
  fractional.

## Status

`Status` is everything the app shows, with every decision already made. The
app only chooses words, colors and layout.

What each field holds is said once, on the Rust type that sends it:

| Type | In |
|---|---|
| `Status` and what it holds: `Account`, `Limit`, `Outlook`, `Moment` and the rest | `src/status.rs` |
| `Status.agents`, and `UsedMost`, a session in a `limit.breakdown` result | `src/serve.rs` |
| `Settings`, `Notify` and `Alert` | `src/alerts.rs` |
| An account's `kind` | `src/providers/mod.rs` |

`contract/status.notification.json` holds every field and every value.

## Alerts

[architecture.md](architecture.md#alerts) says when each kind is raised,
and how long `hello` sends it again until a client acknowledges it. Its
fields are on `Alert` in `src/alerts.rs`.

## Change the protocol

There's no protocol version. The app only talks to the engine it ships
with ([Lifecycle](#lifecycle)), so the two sides change together. The app
reads an enum value it doesn't know as an error, not a guess.

Make the whole change in one commit:

1. Change the examples in `contract/`. Cover every new enum value. A new
   message gets its own file.
2. Change the Rust types (`src/status.rs`, `src/alerts.rs`), with what each
   new field holds in its doc comment, or the method in `src/serve.rs`.
3. Change `Contract.swift`, and `Client.swift` or `Store.swift` for a new
   method. Add a new example file to the `answers` or `requests` table in
   `ContractTests.swift`.
4. If the app sends a type back, like `Settings`, model every field of it
   in `Contract.swift`. `serve` sets a field the app leaves out to
   its default.
5. For a new method, notification or error, update this document.
6. Run both sides' tests:

```sh
cargo test serve
swift test --package-path macos/TurnscopeKit
```
