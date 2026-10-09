# Command line

`turnscope` is one binary: the command line, the menu bar app's daemon
(`serve`) and the MCP server (`mcp`). Inside the app it is
`/Applications/Turnscope.app/Contents/Helpers/turnscope`; the
[README](../README.md#install) shows how to put it on your `PATH`.

## Commands

| Command | What it does |
|---|---|
| [`status`](#status) | Every account's limits: what's left, when each resets, its outlook and the work left |
| [`usage`](#usage) | What was used and what it cost, or what used a limit |
| [`sessions`](#sessions) | Find sessions, show one, or read its conversation |
| [`handoff`](#handoff) | Everything another agent needs to finish a session's work |
| [`accounts`](#accounts) | List accounts, or hide or show one |
| [`config`](#config) | Show the settings, or turn a notification on or off |
| [`connect`, `disconnect`](#connect-and-disconnect) | See which agents run Turnscope's MCP server, add it to one, or remove it |
| [`doctor`](#doctor) | What can't be read, and how to fix it |
| [`serve`, `mcp`](#serve-and-mcp) | The app's daemon, and the MCP server |
| [`guard`](#guard) | For an agent's hook: exit 2 when a limit is under a percent left |
| [`statusline`](#status-line) | One line for an agent's status line |

`turnscope <command> --help` prints a command's flags.

The examples below are the output of the snapshot tests
(`tests/snapshots/`), run on synthetic accounts and sessions at Thu Oct 1
12:00 UTC.

## Global options

| Option | Default | |
|---|---|---|
| `--data <dir>` | `~/Library/Application Support/com.joeychilson.turnscope` | Where Turnscope keeps its data |
| `--home <dir>` | `$HOME` | The home folder to read the agents' history and logins from |
| `--json` | | Print JSON instead of text |
| `-h`, `--help` | | Print help |
| `-V`, `--version` | | Print the version |

Every command that prints data takes `--json`. Examples of the JSON are in
`tests/snapshots/*.json`.

Before answering, `status`, `usage`, `sessions`, `handoff`, `accounts` and
`doctor` bring the database up to date. They read what's new in the
agents' history, the limits when five minutes have passed since any process
read them, and the prices when six hours have. If that fails, they answer
from what's known and say why on standard error.

## Names

- **Accounts** are named by id, or by part of a title, label or id, in any
  case: `claude`, `openrouter`, `me@example.com`. Commands that act on one
  account need the name to match exactly one. When it matches several, the
  message lists them.
- **Limits** are named by part of a name or key, in any case: `week` names
  Weekly (`seven_day`), and `"5 hours"` names 5 hours.
- **Agents** are named by id: `claude-code`, `codex`, `opencode`, `pi`,
  `grok-build`.
- **Sessions** are named by id, as `sessions` lists them:
  `claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60`. A unique start of the
  id, or of the agent's own part of it, works too when it's 4 characters
  or more: `0f6e3f6a`.
- **Settings** are `notify.runningOut`, `notify.usedUp`, `notify.reset` and
  `notify.signIn`.

`handoff` leaves out the calling agent's own session, and `usage --limit`,
`guard` and `statusline` default to its account. They find the calling
agent the way the MCP server does ([who is asking](mcp.md#who-is-asking)).
Run from a plain terminal, there is no calling agent.

## Time forms

`usage --since` and `--until`, `sessions --since`, and the MCP tools'
`since` take:

| Form | Example | Means |
|---|---|---|
| A day | `today`, `yesterday` | From midnight |
| A period | `week`, `month` | Since Monday, or since the 1st, at midnight |
| A count back | `24h`, `7d`, `4w` | That many hours, days or weeks before now |
| A date | `2026-10-01` | From midnight that day |
| A time | `2026-10-01T09:00:00Z` | RFC 3339 |
| Now | `now` | Now |

Times are read and shown in the Mac's time zone, or the one `TZ` names.

## Exit codes

| Code | Means |
|---|---|
| 0 | Done |
| 1 | Failed; the message says how to fix it |
| 2 | Bad usage: an unknown flag or value, a time Turnscope can't read, a name that matches several |
| 3 | What you named doesn't exist: an account, a limit, a session |

A search that finds nothing is not an error: it exits 0 and says so.
[`guard`](#guard) and [`statusline`](#status-line) have exit codes of their
own.

## Environment

| Variable | Effect |
|---|---|
| `TURNSCOPE_OFFLINE` | Set to anything but `0`: no provider or models.dev is asked; the database alone answers. |
| `TURNSCOPE_NOW` | Fixes the clock, in UTC milliseconds, so output can be reproduced. |
| `HOME` | The default for `--home`. |
| `TZ` | The time zone times are read and shown in. |
| `SHELL` | `connect` and `disconnect` find the agent's command on the `PATH` this shell sets as a login shell. Default `/bin/zsh`. |
| `CLAUDE_CODE_SESSION_ID`, `CLAUDE_PROJECT_DIR` | Set by Claude Code: the calling session and the folder it works in. |
| `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GROK_HOME` | The agent folder the calling agent uses, when it isn't its own: which account is yours. |

Variables used only by tests are in [development.md](development.md#test).

## Status

```text
turnscope status [--account <name>] [--all] [--hours <n>]
```

Every account's limits: what's left, when each resets, its outlook at this
pace, and the hours of work left. An API key also shows its spend this
month.

| Flag | |
|---|---|
| `--account <name>` | Only the accounts this names. It may name several. |
| `--all` | Hidden accounts too. |
| `--hours <n>` | Whether n more hours of work fit the first account shown, at its pace of work. Zero or more; decimals work. |

```console
$ turnscope status
Claude Max (me@example.com) · via Claude Code · in use
  5 hours: 25% left, resets 14:00 (in 2h). At this pace it runs out 13:00 (in 1h), as soon as 12:30. About 40m of work left (20m–1h 10m), at 40.9% an hour of work.
  Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset (0–25%). About 19h of work left (8h 30m–47h), at 3% an hour of work.

ChatGPT Plus (me@example.com) · via Codex
  Weekly: 80% left, resets Tue 00:00 (in 4d 12h). Lasts, with about 44% left at the reset (0–54%).

OpenRouter API key · via Pi
  Credits: $35.00 of $50.00 left. Doesn't reset.
  Weekly key limit: $11.00 of $20.00 left, resets Tue 00:00 (in 4d 12h). No forecast yet.
  This month, across its keys: $0.00.

SuperGrok (me@example.com) · via Grok Build
  Its login was refused: sign in again in Grok Build.
```

"At this pace" uses clock time, breaks included. "Of work" counts only the
hours your agents were working. Ranges are 80% ranges.
[architecture.md](architecture.md#forecast) says how both are worked out.

With `--hours`, the answer comes first:

```console
$ turnscope status --account claude --hours 3
3 more hours of work don't fit: 5 hours runs out after about 1h 40m of work.

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

| Flag | Default | |
|---|---|---|
| `--since <when>` | 30 days back | A [time form](#time-forms). |
| `--until <when>` | now | A time form. Not with `--limit`. |
| `--by <group>` | `day`, or `session` with `--limit` | `day`, `week`, `month`, `model`, `project`, `agent`, `account` or `session`. |
| `--agent <id>` | every agent | Only this agent's usage. Not with `--limit`. |
| `--account <name>` | every account | Only this account's usage. The name must match one account. |
| `--limit <name>` | | What used this limit (below). |
| `--top <n>` | 20 | How many rows to show. By time, the latest n, with the earlier ones added up in a first row. Otherwise the n that cost most, with the rest added up in a last row. |

```console
$ turnscope usage --since 2026-09-29 --by model
Tue Sep 29 00:00 to Thu Oct 1 12:00: 27 responses, $0.50

  Model          Responses   Cost
  claude-opus-5         27  $0.50

Costs are at list prices: what the same tokens cost on each provider's API.
```

**What used a limit.** With `--limit`, the usage runs from the start of the
limit's current window (for a limit with no window, this month for an API
key and the last 7 days otherwise), by
session, and each row gets its share of the limit. Without `--account`, the
account is the calling agent's if it has the limit, or else the first
account shown that has it. `--since` moves the start, and then rows get no
share.

```console
$ turnscope usage --account claude --limit week
Claude Max (me@example.com) · Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset (0–25%). About 19h of work left (8h 30m–47h), at 3% an hour of work.
Mon Sep 28 00:00 to Thu Oct 1 12:00: 27 responses, $0.50

  Session                                                   Responses   Cost  % of Weekly
  7c1d2e3f Add retries with backoff to the HTTP client.            24  $0.47          41%
  0f6e3f6a Fix the CSV parser so quoted commas work. Don'…          3  $0.03           3%

Costs are at list prices: what the same tokens cost on each provider's API.
```

Shares are approximate. Providers say how much of a limit is used, not by
what, so each row's share is its part of the cost.

## Sessions

```text
turnscope sessions [--search <words>] [--folder <dir>] [--agent <id>] [--since <when>] [--running] [--count <n>] [--cursor <id>]
turnscope sessions show <session>
turnscope sessions read <session> [--from <n>] [--count <n>] [--kinds <kind,…>] [--search <words>] [--failed]
```

`sessions` finds sessions, most recently active first, a page at a time.
Each line is a session at the top of its tree: its subagents are counted in
it, not listed. A fork, a conversation branched off another, and a
continuation are sessions of their own. Run by an agent, the agent's own
session ends with `· yours`. Usage with no known price shows as
`(+N unpriced)` after the cost.

| Flag | Default | |
|---|---|---|
| `--search <words>` | | Sessions where all these words were said, by you or the model, in any of its messages. Whole words, in any case. Those with all of them in the most messages come first, and the first three show the latest such message. |
| `--folder <dir>` | every folder | Sessions of this folder's project (its git repository), or run in it or under it. |
| `--agent <id>` | every agent | Only this agent's sessions. |
| `--since <when>` | | Active since this [time](#time-forms). |
| `--running` | | Only sessions active in the last 5 minutes. |
| `-n`, `--count <n>` | 20 | How many on a page, 1 to 100. |
| `--cursor <id>` | | The next page, from the cursor the page before printed. |

```console
$ turnscope sessions
2 found, most recently active first.

- `claude-code:7c1d2e3f-4a5b-4c6d-8e9f-0a1b2c3d4e5f` "Add retries with backoff to the HTTP client." · /work/api (retries) · running · 24 responses · $0.47
- `claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60` "Fix the CSV parser so quoted commas work. Don't change the public API." · /work/app (main) · active 1d 2h ago · 3 responses · $0.03 · yours
```

**`sessions show`** prints one session in full, with the command that
resumes it:

```console
$ turnscope sessions show 0f6e3f6a
claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60  Fix the CSV parser so quoted commas work. Don't change the public API.
  active 1d 2h ago · 3 responses · $0.03
  folder: /work/app
  project: /work/app
  branch: main
  started: Wed 09:00
  models: claude-opus-5
  account: Claude Max (me@example.com)
  resume: cd /work/app && claude --resume 0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60
```

**`sessions read`** prints a session's conversation, a page at a time,
read from the agent's files with secrets redacted. What was said comes in
full. Tool input and output, reasoning and system notes are cut past 2,000
characters, or 40,000 with `--count 1`, and the entry says how many were
left out. Tool output is cut from the middle, keeping its end. A tool call's input
shows a field a line, one-line fields first, and text of several lines as
it is.

| Flag | Default | |
|---|---|---|
| `--from <n>` | 0 | The first entry, by number. Negative counts back from the end over the entries `--kinds`, `--search` and `--failed` keep: `--from -10` is the last ten of them. |
| `-n`, `--count <n>` | 30 | How many entries, 1 to 100. A page also ends at about 3,000 tokens, after one entry at least. |
| `--kinds <kind,…>` | every kind | Only these kinds, separated by commas (below). |
| `--search <words>` | | Only entries holding all these words, in any case, in what was said or in a tool call's name, input or output. |
| `--failed` | | Only tool calls that failed, as the agent recorded them. With `--from -1`, the last. |

| Kind | Is |
|---|---|
| `user` | What you typed |
| `assistant` | What the agent said |
| `reasoning` | The model's reasoning, where the agent keeps it |
| `tool` | A tool call, with its input and output |
| `system` | What the agent added itself: instructions, reminders, notices |
| `summary` | The summary the agent wrote when it compacted the conversation |
| `task` | What a subagent was asked to do |

```console
$ turnscope sessions read claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60 --from 1 --count 3
3 of 7 entries of `claude-code:0f6e3f6a-713c-4d1e-9a6b-2b8c1d4e5f60`, from entry 1. Times are local; it's now Thu Oct 1 12:00.

#1 assistant · Wed 09:00
I decided to rewrite the tokenizer rather than patch the splitter, because quotes nest.

#2 tool `Bash` (failed) · Wed 09:00
description: Run tests
command: cargo test
out: error: test csv::quoted failed

#3 assistant · Wed 09:02
The tokenizer is done; the quoted-comma test still fails. Should escaped quotes follow RFC 4180?

More from entry 4: add `--from 4`.
```

## Handoff

```text
turnscope handoff [<session>] [--folder <dir>]
```

Prints a handoff: everything another agent needs to pick up a session and
finish its work, in at most 16,000 tokens. It is the same markdown as the
MCP tool [`handoff`](mcp.md#handoff), but it ends with `turnscope sessions
read` commands instead of tool calls. [handoff.md](handoff.md) says what it
holds and how it's built, with an example.

| Argument | Default | |
|---|---|---|
| `<session>` | The latest session in the folder, other than the caller's own | The session to hand off. Taken by default, the handoff first lists the two sessions before it in the folder; `--json` gives them as `others`. |
| `--folder <dir>` | The current folder | The folder to take the latest session of: its project's sessions, or those run in it or under it. |

Only Claude Code says which session is calling. From another agent, its
own session is taken to be that agent's latest in the folder.

## Accounts

```text
turnscope accounts
turnscope accounts hide <name>
turnscope accounts show <name>
```

Lists the accounts, each with its id. `hide` leaves an account out of
lists and alerts; `show` brings it back. Both then print the list. The
name must match one account.

```console
$ turnscope accounts
Claude Max (me@example.com) · anthropic:acct-1:org-1
ChatGPT Plus (me@example.com) · openai:acct-2
OpenRouter API key · openrouter:api
SuperGrok (me@example.com) · xai:user-3
```

A hidden account ends with `· hidden`.

## Config

```text
turnscope config
turnscope config set <key> on|off
```

Shows the settings, or turns a notification on or off. All are on by
default.

| Key | Notifies when |
|---|---|
| `notify.runningOut` | A limit in use will run out before its reset at this pace |
| `notify.usedUp` | A limit in use is used up |
| `notify.reset` | A limit that was used up is back |
| `notify.signIn` | An account in use needs signing in again |

```console
$ turnscope config
notify.runningOut on
notify.usedUp on
notify.reset on
notify.signIn on
```

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

```console
$ turnscope connect
Claude Code  not connected: `turnscope connect claude-code`
Codex        not installed
OpenCode     not installed
Pi           not installed
Grok Build   not installed
```

An agent that runs another copy of Turnscope says so, and `connect` points
it at this one.

## Doctor

```text
turnscope doctor
```

Lists what can't be read and how to fix it: logins that can't be read or
were refused, providers that haven't answered, history lines that couldn't
be read, models with no known price, prices not read yet, and `serve` not
running. When all is well it prints `Nothing to fix.`

## Serve and MCP

```text
turnscope serve [--socket] [--linger <seconds>]
turnscope mcp
```

`serve` is the daemon the menu bar app talks to, over a Unix socket
([protocol.md](protocol.md)). The app starts it. Only one runs per data
directory; a second one exits at once.

| Flag | Default | |
|---|---|---|
| `--socket` | | Print the socket's path and exit. |
| `--linger <seconds>` | 60 | How long to keep running with no client connected. |

`mcp` is the MCP server on standard input and output, which agents start
([mcp.md](mcp.md)).

## Guard

```text
turnscope guard --limit <name> --below <percent> [--account <name>] [--say <text>]
```

`guard` turns a limit into an exit code, for an agent's hook.

| Flag | Default | |
|---|---|---|
| `--limit <name>` | required | The limit, by part of its name, such as `week` or `"5 hours"`. |
| `--below <percent>` | required | Block when the limit has less than this percent left, 0 to 100. |
| `--account <name>` | The calling agent's | The account. The name must match one account. |
| `--say <text>` | The reason | What to print on standard error when it blocks. |

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
| 0 | There's room, or the standing isn't known, such as when there is no calling agent. A guard never blocks for want of data. |
| 1 | It can't follow its command line: a flag missing or mistyped, or a limit your account doesn't have. It blocks nothing. |
| 2 | Under the threshold. The reason is on standard error, which Claude Code shows the model. |

The reason, from `turnscope guard --account claude --limit "5 hours" --below 50`
in the tests:

```text
Claude Max's 5-hour limit has 25% left, under 50%. It resets 14:00 (in 2h); at this pace it runs out 13:00 (in 1h).
```

A hook waits for the guard, so it reads no history and no prices. It reads
the limits only when five minutes have passed since any process did. While
the app runs, its daemon always has.

## Status line

```text
turnscope statusline [--account <name>]
```

`statusline` prints one line for an agent's status line: the account it
draws on, its limits on all its usage, and any other limit running out or
used up. When a limit on all its usage is used up, only those show.

| Flag | Default | |
|---|---|---|
| `--account <name>` | Below | The account. Of several it names, the first. |

From the tests:

```text
Claude Max · 5h 25%, out in 1h · weekly 56%
ChatGPT Plus · weekly 80%
SuperGrok · sign in again
```

A limit running out ends with `out` and when: a countdown within 3 hours,
else a clock time. A used-up limit says `used up, back` and when. Limits
from an earlier read end with `as of` and the time. An account never read
says `login expired` when that's why, and `not read yet` otherwise. An API key with no limits shows its spend this month.

In Claude Code's `settings.json`:

```json
{
  "statusLine": {"type": "command", "command": "turnscope statusline"}
}
```

Without `--account`, it takes the account of the session Claude Code names
on standard input (`session_id`, or else the latest session in
`workspace.current_dir` or `cwd`), then the calling agent's, then the first
account in use. It answers from the database alone, so it never waits on
the network. It always exits 0: when nothing is known, it prints nothing.
