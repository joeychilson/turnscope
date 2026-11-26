# Command line

`turnscope` is one binary: the command line and the MCP server (`mcp`).
Inside the app it is
`/Applications/Turnscope.app/Contents/Helpers/turnscope`; the
[README](../README.md#install) shows how to put it on your `PATH`.

## Commands

| Command | What it does |
|---|---|
| [`status`](#status) | Every account's limits: what's left, when each resets, its outlook and the work left |
| [`usage`](#usage) | What was used and what it cost, or what used a limit |
| [`accounts`](#accounts) | List accounts, or hide or show one |
| [`connect`, `disconnect`](#connect-and-disconnect) | See which agents run Turnscope's MCP server, add it to one, or remove it |
| [`doctor`](#doctor) | What can't be read, and how to fix it |
| [`mcp`](#mcp) | The MCP server agents run |
| [`guard`](#guard) | For an agent's hook: exit 2 when a limit is under a percent left |
| [`statusline`](#status-line) | One line for an agent's status line |

`turnscope <command> --help` prints a command's flags. This page adds what
the help doesn't say.

The examples below are the output of the snapshot tests
(`crates/turnscope/tests/snapshots/`), run on synthetic accounts and
sessions at Thu Oct 1 12:00 UTC.

## Global options

Every command takes `--data <dir>`, where Turnscope keeps its data, and
`--home <dir>`, the home folder to read the agents' history and logins
from; `connect` and `disconnect` change the agents' settings there. Every
command that prints data takes `--json`. Examples of the JSON are in
`crates/turnscope/tests/snapshots/*.json`.

Before answering, `status`, `usage`, `accounts` and `doctor` catch up:
they read what's new in the agents' history, and limits and prices as they
come due ([architecture.md](architecture.md#catching-up)). `accounts hide`
and `show` don't. If catching up fails, they answer from what's known and
say why on standard error.

The first time, and again after an update that changes how history is
read, they read all of your agents' history, which can take minutes. They
say `Reading your agents' history…` on standard error first. While another
Turnscope reads history, such as the app or an agent's MCP server, they
wait for it, and after a second say
`Waiting while another Turnscope reads your agents' history…`.

## Names

- **Accounts** are named by id, or by part of a title, label or id, in any
  case: `claude`, `openrouter`, `me@example.com`. Commands that act on one
  account need the name to match exactly one. When it matches several, the
  message lists them.
- **Limits** are named by part of a name as shown or of a key, in any
  case: `week` names Weekly (`seven_day`), `"5 hours"` names 5 hours, and
  `opus` names Weekly (Opus). The name the [status line](#status-line)
  shows names a limit too: `5h`. A model's limit takes its model after its
  name, as `status` shows it, or before it, as the app does:
  `"weekly (opus)"` or `"opus weekly"`. Where no limit's name holds the
  name as given, it's read as a sentence says it, as `guard` does:
  `"5-hour limit"` names 5 hours, and `"weekly limit"` Weekly. A
  key's limit is named by the agents that hold the key too: `pi` names
  Pi's key limit. A name that fits several limits names the first by key.
- **Agents** are named by id: `claude-code`, `codex`, `opencode`, `pi`,
  `grok-build`, `antigravity`.

`usage --limit`, `guard` and `statusline` default to the calling agent's
account. They find the calling agent the way the MCP server does
([who is asking](mcp.md#who-is-asking)). Run from a plain terminal, there
is no calling agent.

## Time forms

`usage --since` and `--until`, and the MCP tool `explain_usage`'s `since`
and `until`, take these, in any case:

| Form | Example | Means |
|---|---|---|
| A day | `today`, `yesterday` | From midnight |
| A period | `week`, `month` | Since Monday, or since the 1st, at midnight |
| A count back | `24h`, `7d`, `4w` | That many hours, days or weeks before now |
| A date | `2026-10-01` | From midnight that day |
| A time | `2026-10-01T09:00:00Z` | RFC 3339, with `Z` or an offset such as `-05:00` |
| Now | `now` | Now (`--until` / `until` only) |

Times are read and shown in the Mac's time zone, or the one `TZ` names. A
date in another year than now's shows its year, `Jan 1 2020 00:00`, and a
span of a year or more is in years, `(6y ago)`.

## Exit codes

| Code | Means |
|---|---|
| 0 | Done |
| 1 | Failed; the message says how to fix it |
| 2 | Bad usage: an unknown flag or value, a time Turnscope can't read, a name that matches several |
| 3 | What you named doesn't exist: an account or a limit |

[`guard`](#guard) and [`statusline`](#status-line) have exit codes of their
own.

## Environment

| Variable | Effect |
|---|---|
| `TURNSCOPE_OFFLINE` | Set to anything but `0`: no provider or models.dev is asked; the database alone answers. |
| `TURNSCOPE_NOW` | Fixes the clock, in UTC milliseconds, so output can be reproduced. |
| `HOME` | The default for `--home`. |
| `TZ` | The time zone times are read and shown in. |
| `SHELL` | `connect` and `disconnect` find the agent's command on the `PATH` this shell sets as an interactive login shell. Default `/bin/zsh`. |
| `CLAUDE_CODE_SESSION_ID` | Set by Claude Code: the calling session, whose account is yours. |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME` | The agent folder the calling agent uses, when it isn't its own: which account is yours. |

Variables used only by tests are in [development.md](development.md#test).

## Status

```text
turnscope status [--account <name>] [--all] [--hours <n>]
```

Every account's limits: what's left, when each resets, its outlook at this
pace, and the hours of work left. An API key also shows its spend this
month and, after the month's first day, the month at that pace, to two
figures: `on pace for about $62`.

`--account` may name several accounts, hidden or not. `--hours` takes
zero or more, decimals too.

```console
$ turnscope status
Claude Max (me@example.com) · via Claude Code · in use
  5 hours: 25% left, resets 14:00 (in 2h). At this pace it runs out 13:00 (in 1h), as soon as 12:30. About 40m of work left (as little as 20m), at 40.9% an hour of work.
  Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset. About 19h of work left (as little as 7h 50m), at 3% an hour of work.

ChatGPT Plus (me@example.com) · via Codex
  Weekly: 80% left, resets Tue 00:00 (in 4d 12h). Lasts, with about 44% left at the reset.

OpenRouter API key · via Pi
  Credits: $35.00 of $50.00 left. Doesn't reset.
  Pi's weekly key limit: $11.00 of $20.00 left, resets Tue 00:00 (in 4d 12h). No forecast yet.
  This month, across its keys: $0.00.

SuperGrok (me@example.com) · via Grok Build
  Its login was refused; open Grok Build to sign in again.
```

"At this pace" uses clock time, breaks included. "Of work" counts only the
hours your agents were working: the work rate, `40.9% an hour of work`, is
how much of the limit an hour of it uses. "As little as" and "as soon as"
are at a pace that went faster only 1 time in 10.
[architecture.md](architecture.md#forecast) says how both are worked out.

With no account to show, it says why in one line: no account's limits have
been read yet, or every account is hidden, and then that `--all` shows
them. With `--json`, `accounts` is empty.

With `--hours`, whether that many more hours of work fit comes first, as
`check_limits` says it ([mcp.md](mcp.md#check_limits)): a line for each
account, named. With `--json`, `fit` is a list of each account's
`account` id and `fit` line.

```console
$ turnscope status --hours 3
- Claude Max (me@example.com): 3 more hours of work don't fit: 5 hours runs out after about 40m of work (as little as 20m); it resets 14:00 (in 2h).
- ChatGPT Plus (me@example.com): whether 3 more hours of work fit can't be told: Weekly has 80% left and lasts at this pace until it resets Tue 00:00 (in 4d 12h), but too little work on it has been measured.
- OpenRouter API key: whether 3 more hours of work fit can't be told: Credits have $35.00 of $50.00 left, but too little work on them has been measured.
- SuperGrok (me@example.com): whether 3 more hours of work fit can't be told: its login was refused.

Claude Max (me@example.com) · via Claude Code · in use
…
```

## Usage

```text
turnscope usage [--since <when>] [--until <when>] [--by <group>] [--agent <id>] [--account <name>] [--top <n>]
turnscope usage --limit <name> [--account <name>] [--since <when>] [--by <group>] [--top <n>]
```

What was used and what it cost. Costs are at list prices: what the same
tokens cost on each provider's API. A response with no known price is
counted as unpriced, not free.

`--since` and `--until` take a [time form](#time-forms), and `--since`
must be earlier. An `--until` past now counts to now. `--account` must
match one account. `--until` and `--agent` don't go with `--limit`.
Grouped by time, `--top` shows the latest rows, with the earlier ones
added up in a first row; otherwise the rows that cost most, with the rest
added up in a last row. One row past the top is shown, not added up.

```console
$ turnscope usage --since 2026-09-29 --by model
Tue Sep 29 00:00 to Thu Oct 1 12:00: 38 responses, $0.53 (+6 unpriced)

  Model             Responses   Cost
  claude-opus-5            27  $0.50
  grok-4.6-build            1  $0.02
  glm-5.3                   2  $0.01
  deepseek-v4               2  $0.00
  gpt-5.5                   4  $0.00 (+4 unpriced)
  gemini-3.8-flash          2  $0.00 (+2 unpriced)

Costs are at list prices: what the same tokens cost on each provider's API.
```

**By session.** A row is a session at the top of its tree: its subagents'
usage is counted in it. A fork, a conversation branched off another, and a
continuation are sessions of their own. A row shows the first 8 characters
of the agent's own id for the session, and its title; with `--json`, `key`
is the id in full, as `claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60`,
and `name` the title. A title is kept to its first 200 characters.

**What used a limit.** With `--limit`, the usage runs from the start of the
limit's current window, by session, and each row gets its share of the
limit, as does the row that adds up the rest. A limit that never resets,
as credits, has no window: the usage is this month's for an API key and
the last 7 days' otherwise, and in place of what's left, the heading says
so and how much of the limit is used. Without `--account`, the account is
the calling agent's if it has the limit, or else the first account shown
that has it. `--since` moves the start, and then rows get no share. A
limit whose window reset since it was read counts from the reset, or from
one window's length ago when the reset is longer ago than that, and its
rows get no share either: how much of the new window is used isn't known.
Nor do they while some of the usage has no known price, since a share is
of cost, or when the account's last read is out of date and your agents
have worked on it since, which that read doesn't hold: a line says so. A
limit in dollars still gives them then, as a share of it is cost over its
size, which needs no read. A key's own limit counts only the usage made
with that key. With `--json`, `sharePercents` holds each row's share in
the rows' order, and `restSharePercent` the share of the row adding up
the rest, each `null` when rows get none.

```console
$ turnscope usage --account claude --limit week
Claude Max (me@example.com) · Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset. About 19h of work left (as little as 7h 50m), at 3% an hour of work.
Mon Sep 28 00:00 to Thu Oct 1 12:00: 27 responses, $0.50

  Session                                                   Responses   Cost  % of Weekly
  7c1d2e3f Add retries with backoff to the HTTP client.            24  $0.47          41%
  0f6e3f6a Fix the CSV parser so quoted commas work. Don'…          3  $0.03           3%

Costs are at list prices: what the same tokens cost on each provider's API.
```

Shares are approximate. Providers say how much of a limit is used, not by
what, so each row's share is its part of the cost. Of a limit in dollars,
as an OpenRouter key's own, a row's share is its cost over the limit's
size. When the rows' cost is less than what's used, a last line says so:
the rest was spent elsewhere with the key. When it's more, as when some
was spent since the key's last read, a last line gives both. A limit on
some models,
such as `Weekly (Opus)` or a [pool](architecture.md#glossary) such as
`Weekly (Claude and GPT)`, counts only the responses of models whose id
names one of them, such as `claude-opus-5`.

## Accounts

```text
turnscope accounts
turnscope accounts hide <name>
turnscope accounts show <name>
```

Lists the accounts, each with its id. `hide` leaves an account out of
lists and alerts; `show` brings it back. Both then print the list. The
name must match one account. Once a newer Turnscope has written to the
data, `hide` and `show` change nothing, and say to update.

```console
$ turnscope accounts
Claude Max (me@example.com) · anthropic:acct-1:org-1
ChatGPT Plus (me@example.com) · openai:acct-2
OpenRouter API key · openrouter:api
SuperGrok (me@example.com) · xai:user-3
```

A hidden account ends with `· hidden`. With no account yet, it says that
no account's limits have been read, as `status` does, and `--json` prints
an empty list. With `--json`, an account's `title` and `label` are apart,
as `status --json` has them: `Claude Max` and `me@example.com`.

## Connect and disconnect

```text
turnscope connect
turnscope connect <agent>
turnscope disconnect <agent>
```

`connect` with no agent lists which agents run Turnscope's MCP server.
`connect <agent>` adds it with the agent's own command, and `disconnect
<agent>` removes it, in each of the agent's folders, a second account's
included. [mcp.md](mcp.md#connect-an-agent) has each agent's command.
Neither writes Turnscope's data, so both work before Turnscope has made
its database, and after a newer Turnscope has changed it.

```console
$ turnscope connect
Claude Code  not connected: `turnscope connect claude-code`
Codex        not connected: `turnscope connect codex`
OpenCode     not connected: `turnscope connect opencode`
Pi           not connected: `turnscope connect pi`
Grok Build   not connected: `turnscope connect grok-build`
Antigravity  not connected: `turnscope connect antigravity`
```

An agent that runs another copy of Turnscope says so, and `connect` points
it at this one. `connect` refuses when Turnscope runs from a disk image or
a drive in `/Volumes`, or from the copy macOS runs a downloaded app from:
the agent would lose it once that's gone.

With `--home`, the agent's command runs with `HOME` set to that folder, so
that it changes the settings there and not your own. The command is still
found on the `PATH` your interactive login shell sets in your own home.
When the agent's command fails or can't be run, both say the command to
run by hand.

## Doctor

```text
turnscope doctor
```

Lists what can't be read and how to fix it: logins that can't be read or
were refused, providers that haven't answered, history files still there
and lines in them that couldn't be read, models with no known price,
prices not read yet, and what was logged in the last day: what went wrong
in the app, why an account's limits couldn't be read, and why models.dev's
catalog was refused. When all is well it prints `Nothing to fix.`

## MCP

```text
turnscope mcp
```

The MCP server on standard input and output, which agents start
([mcp.md](mcp.md)).

## Guard

```text
turnscope guard --limit <name> --below <percent> [--account <name>] [--say <text>]
```

`guard` turns a limit into an exit code, for an agent's hook.

`--limit` is named as in [Names](#names). `--below` is 0 to 100.
`--account` must match one account. `--say` replaces the reason printed
when it blocks.

In Claude Code's `settings.json`, this stops subagents from starting when
the weekly limit has under 10% left:

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "Agent",
      "hooks": [{"type": "command", "command": "turnscope guard --limit week --below 10"}]
    }]
  }
}
```

| Exit | Means |
|---|---|
| 0 | There's room, or the standing isn't known, such as when there is no calling agent, no account read yet, or none of its limits. A guard never blocks for want of data. |
| 1 | It can't follow its command line: a flag missing or mistyped, or a limit your account doesn't have. It blocks nothing. |
| 2 | Under the threshold. The reason is on standard error, which Claude Code shows the model. |

The reason, from `turnscope guard --account claude --limit "5 hours" --below 50`
in the tests:

```text
Claude Max's 5-hour limit has 25% left, under 50%. It resets 14:00 (in 2h); at this pace it runs out 13:00 (in 1h).
```

A hook waits for the guard, so it reads no history and no prices. It reads
the limits only as they come due, and not while the app runs, which reads
them itself, nor once a newer Turnscope has written to the data: then it
guards by the limits read before. Nor does it make or migrate the
database: before Turnscope has, or after an update until the new version
has opened it, the standing isn't known. Each read of a login's limits gives
up after 15 seconds, whether its provider takes one request or two, or
after 5 if it can't connect.

## Status line

```text
turnscope statusline [--account <name>]
```

`statusline` prints one line for an agent's status line: the account it
draws on, the limits it stands by (those on all its usage, or of the
[pool](architecture.md#glossary) it last drew on, or an API key's
deciding key limit when it has no other), and any other limit
running out or used up. When one it stands by is used up, only those show.
`--account` takes the first account it names.

From the tests:

```text
Claude Max · 5h 25%, out in 1h · weekly 56%
ChatGPT Plus · weekly 80%
SuperGrok · sign in again
```

A limit running out ends with `out` and when: a countdown within 3 hours,
else a clock time. A used-up limit says `used up, back` and when. Limits
from an earlier read end with `as of` and the time. An account never read
says `login expired` when that's why, `signed out` when no agent here is
signed in to it, and `not read yet` otherwise. An API key with no limits shows its spend this month.

In Claude Code's `settings.json`:

```json
{
  "statusLine": {"type": "command", "command": "turnscope statusline"}
}
```

Without `--account`, it takes the account of the session Claude Code names
on standard input (`session_id`), then the calling agent's, then the first
account in use. A new session isn't read yet, so the calling agent's
account shows for it. It answers from the database alone, so it never
waits on the network, and never writes it: before Turnscope has made the
database, or after an update until the new version has opened it, it
prints nothing. It always exits 0: when nothing is known, it prints
nothing.
