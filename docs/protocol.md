# The app's protocol

The menu bar app talks to `turnscope serve` over JSON-RPC 2.0 on a Unix
socket. The server is `src/serve.rs`. The app's side is `Client.swift` and
`Contract.swift` in `macos/TurnscopeKit` ([app.md](app.md)).

## The examples

[`contract/`](../contract) holds a hand-written example of every message,
one file per message, covering every enum value. A file is named for its
method and what it is: `hello.request.json`, `hello.result.json`,
`status.notification.json`. `done.result.json` is the empty result, and
`error.json` is an error. The examples are pretty-printed. On the socket,
each message is one line.

Both sides test against the examples, so they can't drift apart:

- The app's `ContractTests.swift` decodes every result and notification, and
  checks that each request the app writes has exactly its example's
  `params`. It fails on an example it doesn't check.
- `serve::tests` reads every status and alert in the examples into the Rust
  types and writes them back unchanged.
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
{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":1}}
```

## Lifecycle

- **One per data directory.** `serve` holds `serve.lock`. A second `serve`
  prints `turnscope serve: already running for <dir>` and exits 0.
- **Starting.** The app runs `turnscope serve --socket` with its `--data`
  and `--home` to get the socket's path, and `turnscope --version` to get
  its engine's name. Then it connects. If nothing listens, it starts
  `turnscope serve` with the same `--data` and `--home`, and tries to
  connect every 100 ms for 5 seconds.
- **Reconnecting.** After a dropped connection or a failed attempt, the app
  waits 0.5 s, doubling each time up to 30 s. The wait goes back to 0.5 s
  only after a connection has held for 30 s, so an engine that keeps failing
  isn't started twice a second.
- **Exiting.** `serve` exits `--linger` seconds (default 60) after its last
  client leaves, or after it starts if no client connects. An app restart or
  update reconnects in that time without reading anything again. `shutdown`
  makes it exit now. If the thread that keeps the data current fails, `serve`
  exits with an error, and the app starts a new one.
- **Another engine.** The app only talks to the engine it ships with. The
  `hello` result names the server, as `turnscope --version` prints it
  (`turnscope 0.1.0`), and the app checks that before reading the rest. If
  it names another engine, or the answer is `-32001`, the app sends
  `shutdown` and starts its own `serve`. It does this once per run, so two
  app versions can't keep stopping each other's `serve`.

## Handshake

The first request on a connection is `hello`. Its result holds the whole
status and every alert not yet acknowledged, so each connection starts in
sync. Until `hello` succeeds, any other request but `shutdown` gets
`-32002`.

```json
{"id":1,"jsonrpc":"2.0","method":"hello","params":{"protocol":1}}
```

```json
{"id":1,"jsonrpc":"2.0","result":{"alerts":[…],"server":"turnscope 0.1.0","status":{…}}}
```

A protocol the server doesn't speak gets `-32001` (`error.json`):

```json
{"error":{"code":-32001,"data":{"supported":[1]},"message":"this turnscope speaks protocol 1, not 2"},"id":1,"jsonrpc":"2.0"}
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
  answers with up to 3 sessions that cost the most on that account since
  the limit's window started (the last 7 days when its start isn't known),
  the most first. It looks the limit up in the status last pushed. An
  account or limit not in it gets `-32000`.
- **`settings.set`** takes the whole `Settings`. A field left out is set to
  its default, which turns that notification on.
- **`agent.connect`** and **`agent.disconnect`** take an agent's `id` from
  `status.agents`. The agent's command can take minutes. `serve` runs it on
  another thread and answers when it's done, and answers other requests
  meanwhile. On success it pushes the status. An unknown agent gets
  `-32602`. A command that fails gets `-32000` with its reason.

`turnscope config set` and `turnscope accounts hide|show` change the same
settings from the command line. A running `serve` pushes the status that
carries them within a minute.

```json
{"id":3,"jsonrpc":"2.0","method":"limit.breakdown","params":{"account":"anthropic:acct-1:org-1","limit":"five_hour"}}
```

```json
{"id":3,"jsonrpc":"2.0","result":{"sessions":[{"agent":"claude-code","project":"app","running":true,"session":"claude-code:0f6e3f6a-713c","sharePercent":31.5,"title":"Fix the parser"}]}}
```

## Notifications

`serve` pushes these. A message the client sends without an `id` is
ignored.

| Method | Params | When |
|---|---|---|
| `status` | `Status` | When anything in it but `at` changed |
| `alert` | `Alert` | When an alert is raised, right after the `status` that raised it |

`serve` works out the status again after each catch-up and after a
successful `settings.set`, `account.hide`, `agent.connect` or
`agent.disconnect`. A catch-up runs when `serve` starts, once writes to an
agent's folders have paused for 250 ms (a second at most), and every minute.
Alerts are raised only when the status changed.

Pushes go only to clients that have said `hello`. A client that doesn't
take a message within 2 seconds is dropped.

```json
{"jsonrpc":"2.0","method":"alert","params":{"account":"anthropic:acct-1:org-1","accountTitle":"Claude Max (me@example.com)","at":1791460500000,"id":1,"kind":"runningOut","limit":"five_hour","limitName":"5 hours","replaces":"limit:anthropic:acct-1:org-1:five_hour","resets":{"at":1791471600000,"horizon":"soon"},"runsOut":{"at":1791464400000,"horizon":"soon"}}}
```

## Errors

| Code | Meaning | Message |
|---|---|---|
| `-32700` | Not JSON. The `id` is null | `not JSON: <why>` |
| `-32600` | No `method` | `a request needs a method` |
| `-32601` | No such method | `no method <method>` |
| `-32602` | Parameters that don't fit, or an unknown agent | `parameters that don't fit: <why>`, `no agent is <id>` |
| `-32000` | The engine couldn't do it | Why, such as `no limit <key> of <account>` |
| `-32001` | Unsupported protocol. `data.supported` lists those spoken | `this turnscope speaks protocol 1, not <n>` |
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

The app reads only the fields it shows. It doesn't read an account's
`usedBy`, a limit's `readAt` or `work`, a window's `startsAt`, the outlook's
`soonest`, `latest` or `leftAtReset`, or an alert's `at`. The command line
and MCP server use those.

| Field | Type | Holds |
|---|---|---|
| `at` | ms | The minute it was worked out for |
| `historyReadAt` | ms or null | When the agents' history was last read. Null until the first read finishes |
| `accounts` | [Account] | Every account, hidden ones too, in the order to list them |
| `agents` | [Agent] | Every agent Turnscope reads, installed or not |
| `settings` | Settings | The user's settings |

Accounts are listed in this order:

1. Accounts in use: the most urgent first (used up, then running out), then
   the soonest to run out, then the least left.
2. Other accounts with a limit to show, the most left first.
3. Accounts with a refused login.
4. Accounts with nothing to show.

### Agent

| Field | Type | Holds |
|---|---|---|
| `id` | string | `claude-code` |
| `name` | string | `Claude Code` |
| `installed` | bool | Its folder exists |
| `connection` | string | Whether it runs Turnscope's MCP server |
| `inUse` | bool | Its responses drew on an account in the last 30 minutes |

| `connection` | Means |
|---|---|
| `connected` | It runs this binary's MCP server in every one of its folders. A link to the binary counts |
| `outdated` | A folder of it runs another copy's |
| `available` | Otherwise: a folder of it runs none |

### Account

| Field | Type | Holds |
|---|---|---|
| `id` | string | `anthropic:acct-1:org-1` |
| `provider` | string | The provider's id: `anthropic` |
| `kind` | `subscription`, `apiKey` | A plan with limits that reset, or a key paid for as used |
| `title` | string | `Claude Max`, `OpenRouter API key` |
| `label` | string or null | What tells it apart from another of its kind, such as an email address |
| `agents` | [string] | The agents signed in to it now, by id |
| `usedBy` | [string] | The agents that drew on it in the last 30 minutes, the latest first |
| `inUse` | bool | Drawn on in the last 30 minutes, or a limit rose at its last reading, taken in the last 30 minutes, as use elsewhere makes it |
| `recent` | bool | In use, or drawn on in the last 7 days |
| `hidden` | bool | The user hid it |
| `state` | State | How its limits were last read |
| `limits` | [Limit] | Its limits as last read, by key. Empty if never read |
| `deciding` | string or null | The key of the limit that decides how the account stands. Null with no limits |
| `spend` | Spend or null | For an API key, this month's cost. Null for a subscription |

The deciding limit is chosen from the limits not reset since they were
read. Limits on the whole account win over a single model's or a single
key's. Among those, the most urgent wins, then the soonest to run out, then
the least left: at the reset for a limit that lasts, where that's
forecast, or else now. When every limit reset since it was read, the last
to reset decides.

### State

| `kind` | Other fields | Means |
|---|---|---|
| `live` | `readAt` | Read at the last attempt |
| `asOf` | `readAt`, `why` | As read at `readAt`, and not since, because of `why` |
| `unread` | `why` | Never read, because of `why` |

| `why` | Means |
|---|---|
| `signedOut` | No agent here is signed in to it now |
| `signIn` | Its provider refused the login |
| `expired` | Every login to it expired. Its agent renews one when next used |
| `readFailed` | Its provider couldn't be reached or gave an answer Turnscope doesn't understand, or nothing has read it for 30 minutes |

### Limit

| Field | Type | Holds |
|---|---|---|
| `key` | string | The provider's key: `five_hour`, `seven_day`. An API key's own limit is `key-<fingerprint>` |
| `name` | string | The provider's name: `5 hours`, `Weekly`, `Credits` |
| `scope` | string or null | The one model it applies to, such as `Opus`. Null for every model |
| `window` | Window or null | Null for a limit that never resets, such as credits |
| `readAt` | ms | When it was read |
| `leftPercent` | integer | Percent left, 0 to 100, rounded down. It's 100 once its window reset since it was read |
| `money` | Money or null | For a limit of money, its size and what's left |
| `resetSince` | ms or null | When its window reset, if it has since it was read |
| `heldBy` | [string] | For an API key's own limit, the agents holding the key. Otherwise empty |
| `outlook` | Outlook | Where it's headed |
| `work` | Work or null | The pace of work, and the work left at it |

A limit from the contract's example:

```json
{
  "key": "five_hour", "name": "5 hours", "scope": null,
  "leftPercent": 28, "money": null, "resetSince": null, "heldBy": [],
  "readAt": 1791460680000,
  "window": {"startsAt": 1791453600000, "resets": {"at": 1791471600000, "horizon": "soon"}},
  "outlook": {
    "kind": "runsOut",
    "likely":  {"at": 1791464400000, "horizon": "soon"},
    "soonest": {"at": 1791463200000, "horizon": "soon"},
    "latest":  {"at": 1791469800000, "horizon": "soon"}
  },
  "work": {"ratePerHour": 25.0, "leftHours": {"low": 0.7, "likely": 1.1, "high": 2.2}}
}
```

### Window

| Field | Type | Holds |
|---|---|---|
| `startsAt` | ms or null | When the window started. Null for a window that rolls |
| `resets` | Moment | When it resets |

### Moment

A time to come, rounded to the minute, with how soon it is. The horizon
decides how the time is worded: a countdown, a time of day, a weekday or a
date.

| Field | Type | Holds |
|---|---|---|
| `at` | ms | The time |
| `horizon` | string | How soon it is |

| `horizon` | Means |
|---|---|
| `soon` | Within 3 hours |
| `today` | Later today, in local time |
| `thisWeek` | Within 7 days |
| `later` | Further off |

### Money

| Field | Type | Holds |
|---|---|---|
| `sizeUsd` | number | The limit's size |
| `leftUsd` | number | What's left, rounded down to the cent |

### Outlook

| `kind` | Other fields | Means |
|---|---|---|
| `lasts` | `leftAtReset` | It lasts until its reset. `leftAtReset` is a Spread of the percent left then, or null with nothing used yet |
| `runsOut` | `likely`, `soonest`, `latest` | It runs out before its reset. Each is a Moment: likely at `likely`, within the 80% range from `soonest` to `latest`. `latest` is null when the slow end of the range lasts |
| `usedUp` | `back` | It's used up. `back` is the Moment it resets, or null for a limit that never resets |
| `unknown` | `reason` | No forecast, because of `reason` |

`runsOut` times are rounded to 10 minutes within a day and to the hour
beyond, and are never in the past.

| `reason` | Means |
|---|---|
| `notEnoughData` | Too little of the window has passed or been used to tell a pace ([architecture.md](architecture.md#forecast)) |
| `stale` | Read more than 30 minutes ago, or its window reset since it was read |
| `noReset` | It never resets, such as credits |
| `rolling` | Its window rolls: it has a reset but no start to pace from |

### Spread

Percent left at the reset, as an 80% range, each end rounded down.

| Field | Type | Holds |
|---|---|---|
| `low` | integer | At the fast end of the range |
| `likely` | integer | At the likely pace |
| `high` | integer | At the slow end |

### Work

The pace of work counts only the hours agents were working on the account,
not clock time ([architecture.md](architecture.md#forecast)). `work` is null
until at least an hour of work and 2 points have been measured, and always
for a window that rolls.

| Field | Type | Holds |
|---|---|---|
| `ratePerHour` | number | Points of the limit an hour of work uses |
| `leftHours` | HoursLeft or null | Hours of work left at that rate. Null when the limit lasts until its reset even with nonstop work |

### HoursLeft

Hours of work left, as an 80% range.

| Field | Type | Holds |
|---|---|---|
| `low` | number | The fewest |
| `likely` | number | The likely |
| `high` | number | The most |

### Spend

| Field | Type | Holds |
|---|---|---|
| `monthUsd` | number | What it cost so far this calendar month |
| `forecastUsd` | number or null | The whole month at the pace so far. Null in the month's first day |
| `partial` | bool | Some of this month's usage has no known price, so it cost more than `monthUsd` |

### Settings

| Field | Type | Holds |
|---|---|---|
| `notify` | Notify | Which alerts to send |

### Notify

Each is on by default.

| Field | Type | Sends an alert when |
|---|---|---|
| `runningOut` | bool | A limit in use will run out before it resets |
| `usedUp` | bool | A limit in use is used up |
| `reset` | bool | A limit that was used up is back |
| `signIn` | bool | An account in use needs signing in again |

### UsedMost

A session in a `limit.breakdown` result.

| Field | Type | Holds |
|---|---|---|
| `session` | string | The session's id: `claude-code:0f6e3f6a-713c` |
| `title` | string or null | Its title |
| `project` | string or null | The name of its project's folder |
| `agent` | string | The agent's id |
| `sharePercent` | number or null | About how many points of the limit it used: its share of the cost, times the points used. Null when the window's start isn't known, or nothing used had a price |
| `running` | bool | Whether it's running now |

## Alerts

[architecture.md](architecture.md#alerts) says when each kind is raised.
Each is raised once per account, limit, kind and window. An alert of a kind
turned off, or for a hidden account, is recorded as acknowledged and never
sent.

An alert is sent right after the status that raised it. Until a client
acknowledges it with `alerts.ack`, it's sent again in every `hello` result
while it still applies: a `runningOut` or `usedUp` alert until its window
resets, and a `reset` or `signIn` alert for a day.

| Field | Type | Holds |
|---|---|---|
| `id` | integer | The id to acknowledge it by |
| `kind` | `runningOut`, `usedUp`, `reset`, `signIn` | What happened |
| `at` | ms | When it was raised |
| `account` | string | The account's id |
| `accountTitle` | string | Its title and label when raised: `Claude Max (me@example.com)` |
| `limit` | string or null | The limit's key. Null for `signIn` |
| `limitName` | string or null | The limit's name. Null for `signIn` |
| `replaces` | string | The notification it takes the place of: `limit:<account>:<key>` or `account:<account>`. A limit that's back replaces the one saying it was used up |
| `resets` | Moment or null | When the limit's window resets |
| `runsOut` | Moment or null | For `runningOut`, when it likely runs out. Otherwise null |

The horizons of `resets` and `runsOut` are worked out when the alert is
sent, not when it was raised.

## Versioning

`protocol` is an integer, now `1`: `PROTOCOL` in `src/serve.rs` and
`protocolVersion` in `Contract.swift`. Within a version, fields may be
added, and clients ignore fields they don't know. Anything else needs a new
version: a new enum value, a field removed or renamed, or a field with a
new meaning. The app treats an enum value it doesn't know as an error,
not a guess.

### Change the protocol

Make the whole change in one commit:

1. Change the examples in `contract/`. Cover every new enum value. A new
   message gets its own file.
2. Change the Rust types (`src/status.rs`, `src/alerts.rs`) or the method
   in `src/serve.rs`.
3. Change `Contract.swift`, and `Client.swift` or `Store.swift` for a new
   method. Add a new example file to the `answers` or `requests` table in
   `ContractTests.swift`.
4. If the app sends a type back, like `Settings`, model every field of it
   in `Contract.swift`. `serve` sets a field the app leaves out to
   its default.
5. For anything but an added field, raise `PROTOCOL` and
   `protocolVersion`, and update `hello.request.json` and `error.json`.
6. Update this document.
7. Run both sides' tests:

```sh
cargo test serve
swift test --package-path macos/TurnscopeKit
```
