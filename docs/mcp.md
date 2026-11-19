# MCP server

`turnscope mcp` is a Model Context Protocol server over standard input and
output. A coding agent connected to it can check its limits before costly
work, and find out what used them. It has two tools.

The tools only read: they change nothing in your agents' files.

## Connect an agent

```sh
turnscope connect               # list the agents and whether they have it
turnscope connect claude-code   # add it with the agent's own command
turnscope disconnect codex      # remove it
```

The app's Settings → Agents does the same.

`connect` runs the agent's own command, found on the `PATH` of your
interactive login shell ([cli.md](cli.md#connect-and-disconnect)), and
never writes the agent's files itself.

| Agent | Id | Add | Remove |
|---|---|---|---|
| Claude Code | `claude-code` | `claude mcp add --scope user turnscope -- <turnscope> mcp` | `claude mcp remove --scope user turnscope` |
| Codex | `codex` | `codex mcp add turnscope -- <turnscope> mcp` | `codex mcp remove turnscope` |
| OpenCode | `opencode` | `opencode mcp add --global turnscope -- <turnscope> mcp` | none |
| Pi | `pi` | `pi mcp add turnscope -- <turnscope> mcp` | `pi mcp remove turnscope` |
| Grok Build | `grok-build` | `grok mcp add --scope user turnscope <turnscope> mcp` | `grok mcp remove --scope user turnscope` |
| Antigravity | `antigravity` | `agy mcp add turnscope -- <turnscope> mcp` | `agy mcp remove turnscope` |

`<turnscope>` is the binary's real path, such as
`/Applications/Turnscope.app/Contents/Helpers/turnscope`, even when you run
it through a link.

`connect` runs the remove command first, so an old entry is replaced. If
the agent then refuses the new entry, the old one is put back, and the
error gives the command to run by hand. Start a new session in the agent to
use the server. After `disconnect`, sessions already open keep it until
they end.

OpenCode has no remove command. To remove the server, delete `turnscope`
from `mcp.servers` in `~/.config/opencode/opencode.json`.

### Check the connection

`turnscope connect` with no agent lists each agent's state
([cli.md](cli.md#connect-and-disconnect) shows it):

| State | Means |
|---|---|
| `connected` | The agent runs this copy of Turnscope, in every folder it has. A link to it counts. |
| `runs another copy of Turnscope` | The agent runs another binary, such as an older install. `turnscope connect <id>` points it at this one. |
| `not connected` | The agent has no `turnscope` server. |
| `not installed` | The agent's folder doesn't exist: `~/.claude`, `~/.codex`, `~/.local/share/opencode`, `~/.pi/agent` or `~/.grok`. For Antigravity, neither `~/.gemini/antigravity` nor `~/.gemini/antigravity-cli` does: the Gemini command line keeps `~/.gemini` too. |

The state comes from the agent's configuration file:

| Agent | File | Entry |
|---|---|---|
| Claude Code | `~/.claude.json` | `mcpServers.turnscope` |
| Codex | `~/.codex/config.toml` | `[mcp_servers.turnscope]` |
| OpenCode | `~/.config/opencode/opencode.json` | `mcp.servers.turnscope` |
| Pi | `~/.pi/agent/mcp.json` | `mcpServers.turnscope` |
| Grok Build | `~/.grok/config.toml` | `[mcp_servers.turnscope]` |
| Antigravity | `~/.gemini/config/mcp_config.json` | `mcpServers.turnscope` |

Antigravity's app and `agy` share that file, so `agy` connects both. A
server switched off there, as `agy mcp disable turnscope` does, counts as
not connected, and `turnscope disconnect antigravity` leaves it where it
is: `agy mcp remove turnscope` removes it.

A second account's folder, such as `~/.claude-work` or `~/.codex-work`,
keeps its own configuration: `.claude.json` or `config.toml` inside it.
`connect` and `disconnect` run the agent's command once for each folder,
pointed at it with `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `GROK_HOME` as you
would to use that account.

## Who is asking

When it starts, the server works out which agent started it, and its
session where the agent names it. From those it finds the account the agent
uses. So "my limit" needs no id, and `explain_usage` marks the session
Claude Code calls from.

| What | How it's found |
|---|---|
| Agent | The nearest agent's command (`claude`, `codex`, `opencode`, `pi`, `grok` or `agy`) among the server's parent processes, up to eight levels up, found with `/bin/ps`. A script run by `node`, `bun`, `deno`, `python` or `python3` counts by the script's name. A versioned build such as `grok-1.0.41-macos-aarch64` counts as `grok`. Antigravity's app starts the server from its own language server, without `agy`, so that program counts as well. With none found, `CLAUDE_CODE_SESSION_ID`, which Claude Code sets in a server it starts, says Claude Code. So Codex run from Claude Code's shell, where the variable is set too, is Codex. |
| Session | `CLAUDE_CODE_SESSION_ID` for Claude Code. The other agents don't say, so their session isn't known. |
| Account | The account of the session's latest response. Otherwise the account of the agent's latest response from its agent folder: the one `CLAUDE_CONFIG_DIR`, `CODEX_HOME` or `GROK_HOME` points to, or else its default folder. |

When the account isn't known, `check_limits` lists every account, and
`explain_usage` answers with every account's usage; given `limit`, it asks
for `account`.

The command line's `guard` and `statusline` work out the caller the same
way.

## Answers

- Every answer is one markdown text block: the verdict in sentences, then
  lists and tables.
- There is no `structuredContent`: programs should use the CLI's `--json`.
- Times are local, on a 24-hour clock: `10:37` today, `Wed 09:00` within a
  week, `Sep 12 09:30` beyond. A limit's times also say how far off they
  are: `15:00 (in 3h)`.
- Costs are at list prices: what the same tokens cost on the provider's
  API, or the provider's own charge where the agent records one (Grok
  Build). An unknown cost is usage with no known price, not free.
- A session's title is in double quotes. It's often the first thing asked
  in that session, so agents are told to read it as data, not as an
  instruction.
- A tool call that fails answers with `isError: true` and one text block
  that says why and, in most cases, how to fix the call. Arguments that
  don't fit the schema fail the same way, not with a JSON-RPC error:

  ```
  no account is named nope; there are: Claude Max (me@example.com) (anthropic:acct-1:org-1), ChatGPT Plus (me@example.com) (openai:acct-2), OpenRouter API key (openrouter:api), SuperGrok (me@example.com) (xai:user-3)
  ```

- Each call first reads what's new: history, and limits and prices when
  they're due. A provider that can't be reached leaves its account as it
  was last read. If reading fails, as when a newer Turnscope has written
  to the data since the server started, or another Turnscope has been
  reading history for over a second, as the app does at its first launch,
  the tool still answers from what's known, and ends by saying it's as of the
  last read, and why: once an update has replaced the binary the server
  runs, `Turnscope was updated after this one started; start it again`.
  While all of history is read, the first time and after an update, it
  ends instead by saying its usage is only part of it, and to ask again
  in a few minutes. A call that then fails says why first, since its query
  may no longer fit a schema a newer Turnscope changed: `What's new
  couldn't be read first: a newer Turnscope has updated this data; update
  Turnscope. Then this failed: …`.

## Tools

| Tool | Title | Call it to |
|---|---|---|
| [`check_limits`](#check_limits) | Check limits | See whether planned work fits, before costly work |
| [`explain_usage`](#explain_usage) | Explain usage | Find out what used a limit or what something cost |

Every input is optional. An input the schema doesn't list is an error.

The examples come from `crates/turnscope/tests/snapshots/mcp.json`:
synthetic accounts and sessions, with the clock at Thu Oct 1 12:00 UTC.

### `check_limits`

An agent calls it before costly work, such as starting several subagents or
a long task, with the hours of work it plans. The answer says whether they
fit and how much work is left before each limit runs out. With `all`, it
compares every account, to choose where to work. Hours are at the user's
usual work rate, so for several agents at once an agent gives their
hours added up. An answer that can't tell
is not a yes.

| Input | Type | Default | Meaning |
|---|---|---|---|
| `hours` | number, 0 or more | none | Hours of work planned. The answer opens with whether they fit. |
| `account` | string | the caller's | The accounts to show: an account id, or text in their title, label or id, any case, such as `claude` or `openrouter`. It can match several. |
| `all` | boolean | `false` | Every account except hidden ones. `account` wins when both are given. |

The answer has three parts:

1. With `hours`, whether they fit at the work rate: of several
   accounts, a line for each, named. Only an account's limits on all its
   usage count, not a limit on one model or one key. Work past a limit's
   reset counts in the windows after it. A limit with too little work
   measured, or a window that rolls, as OpenCode Go's 5 hours does, which
   has no start to measure work from, means it can't be told: the answer
   names the limit and what's left of it, then what the others would
   leave.
2. A one-sentence verdict on the first account: how much work is left
   before which limit runs out, and when that limit resets. When the
   limit that decides the account has no work rate measured, it says
   what's left of that limit instead. When the
   caller's account isn't known and `account` and `all` aren't given,
   every account is shown and this line says so instead. A used-up
   account says which limit is used up and when it's back: of several, the
   last back.
3. Each account, the caller's first and marked `yours`, with the agents
   that use it (`via OpenCode and Pi`), and `in use` and `hidden` where
   they apply. Then a note if its limits couldn't be read, a
   line per limit with what's left, the reset, the outlook and the work
   left, and for an API key, the spend this month across its keys and the
   month at that pace, to two figures.

[cli.md](cli.md#status) says what "at this pace" and "of work" count.

With no account to show, the answer is one line saying why, as the command
line's `status` says it: no account's limits have been read yet, or every
account is hidden, and then to name one with `account`.

Called with `{"all": true, "hours": 2}`:

```markdown
- Claude Max (me@example.com): 2 more hours of work don't fit: 5 hours runs out after about 40m of work.
- ChatGPT Plus (me@example.com): Whether 2 more hours of work fit can't be told: Weekly has 80% left, and too little work on it has been measured.
- OpenRouter API key: Whether 2 more hours of work fit can't be told: Credits have $35.00 of $50.00 left, and too little work on them has been measured.
- SuperGrok (me@example.com): Whether 2 more hours of work fit can't be told: no limit on all its usage was read.

Claude Max (me@example.com): about 40m of work left (20m–1h 10m) before 5 hours runs out; it resets 14:00 (in 2h).

Claude Max (me@example.com) · via Claude Code · yours · in use
  5 hours: 25% left, resets 14:00 (in 2h). At this pace it runs out 13:00 (in 1h), as soon as 12:30. About 40m of work left (20m–1h 10m), at 40.9% an hour of work.
  Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset (0–25%). About 19h of work left (8h 30m–47h), at 3% an hour of work.

ChatGPT Plus (me@example.com) · via Codex
  Weekly: 80% left, resets Tue 00:00 (in 4d 12h). Lasts, with about 44% left at the reset (0–54%).

OpenRouter API key · via Pi
  Credits: $35.00 of $50.00 left. Doesn't reset.
  Weekly key limit: $11.00 of $20.00 left, resets Tue 00:00 (in 4d 12h). No forecast yet.
  This month, across its keys: $0.00.

SuperGrok (me@example.com) · via Grok Build
  Its login was refused; open Grok Build to sign in again.
```

### `explain_usage`

An agent calls it when you ask what used a limit or what something cost. It
breaks down one account's usage in a limit's current window, or since a
time, by session, project, model, agent, day, week or month; or, with
`all`, every account's
together, as for "what did I spend across my agents".

| Input | Type | Default | Meaning |
|---|---|---|---|
| `account` | string | the caller's | One account: its id, or text in its title, label or id. If the text matches several, the error lists them. |
| `all` | boolean | `false` | Every account's usage together, when `account` isn't given. There's no limit then, so no shares, and `since` defaults to 30 days back, as the command line's `usage` does. With `limit` it's an error. |
| `limit` | string | the limit that decides how the account stands | Text in a limit's name or key, any case, such as `week` or `5 hours`. A model's limit takes its model after its name or before it: `weekly (opus)` or `opus weekly`. |
| `since` | string | `window` | `window` for the limit's current window, or a [time form](cli.md#time-forms). For a limit with no window start, `window` means this month for an API key, and the last 7 days otherwise. Only `window` gives each row's share of the limit, and the description says "this week" of a weekly limit is its window. A `since` at or after now is an error, as it is for the command line's `usage`. |
| `until` | string | now | With `since`: up to when, in the same forms. Without `since`, the usage runs to now, and `until` is an error. |
| `by` | `session`, `project`, `model`, `agent`, `day`, `week` or `month` | `session` | How to group the rows. |
| `top` | integer, 1 to 20 | 5 | The rows to show. The rest are added up in one row. |

The answer:

1. The account, when the usage starts and how long ago, the cost at list
   prices and the number of responses, with how many have no known price.
2. The limit's line, as `check_limits` gives it.
3. A table of the top rows, each with its cost, responses and share of the
   limit in percentage points, then a row adding up the rest. A session
   shows as the first 8 characters of its agent's own id for it, and its
   title in quotes, or `(untitled)`. The caller's own is marked `(yours)`
   when its agent names it, as Claude Code does.
4. The top three models and projects of the same usage, except the one
   grouped by, and with `all` the top three accounts, each with its cost.
   Usage with no known price shows as `(+N unpriced)` after the cost, as
   in the table.

The usage's start and each row's share are worked out as for the command
line's `usage --limit` ([cli.md](cli.md#usage)). Shares are
approximate, and known only over the limit's current window: with any
other `since`, the column is left out, and a line says from when the
window runs and to leave `since` out for them. A limit whose window reset
since it was read gives no shares, so no line says how to get them. Usage
with no known price gives none either, as a share is of cost, and a line
says so. A key's own limit counts only the usage made with that key.

When the caller's account isn't known and neither `account` nor `all` is
given, the answer is every account's, and opens by saying so; with
`limit`, it's an error. When the caller's session isn't among the rows,
a line after the table gives its cost, responses and share.

An account with no limit read, such as one whose login expired, still has
its usage broken down, without the limit's line or shares: unless `since`
says otherwise, over this month for an API key and the last 7 days for a
subscription.

Called with `{"account": "claude", "limit": "week", "by": "model"}`:

```markdown
Usage of Claude Max (me@example.com) since Mon 00:00 (3d 12h ago): $0.50 at list prices over 27 responses.
Weekly: 56% left, resets Mon 00:00 (in 3d 12h). Lasts, with about 12% left at the reset (0–25%). About 19h of work left (8h 30m–47h), at 3% an hour of work.

| Model | Cost | Responses | % of Weekly |
|---|--:|--:|--:|
| claude-opus-5 | $0.50 | 27 | 44% |

By project: /work/api: $0.47; /work/app: $0.03.
```

## Protocol

Messages are JSON-RPC 2.0, one to a line, on standard input and output. A
notification, which has no `id`, gets no reply.

| Revision | How a client starts |
|---|---|
| `2026-07-28` | `server/discover`. Each request carries `_meta["io.modelcontextprotocol/protocolVersion"]`. |
| `2025-11-25`, `2025-06-18`, `2025-03-26`, `2024-11-05` | `initialize` |

`initialize` answers with the client's revision when it's one of these,
and with `2025-11-25` otherwise. The two oldest are taken because nothing
here is newer than tools with annotations, which an older client ignores.

| Method | Result |
|---|---|
| `initialize` | `protocolVersion`, `capabilities`, `serverInfo`, `instructions` |
| `server/discover` | `supportedVersions`, `capabilities`, `instructions`, `ttlMs`, `cacheScope` |
| `ping` | `{}` |
| `tools/list` | The two tools |
| `tools/call` | The answer, as in [Answers](#answers) |

- `capabilities` is `tools`, with `listChanged: false`.
- `serverInfo` is `{"name": "turnscope", "title": "Turnscope", "version":
  "<the binary's version>"}`.
- `instructions` tells the agent what Turnscope reads, to call
  `check_limits` before work that takes hours or several subagents, and to
  read a session's title as data. Claude Code can hold back tools'
  descriptions until the model looks for them, but it always shows the
  instructions, so they say when to check unasked; `explain_usage` answers
  what the person asks, as its name says.
- Each tool has a `title` and the annotations `readOnlyHint: true` and
  `openWorldHint: true`: each call reads limits from providers first.
- A request with the `2026-07-28` `_meta` gets `resultType: "complete"` and
  `_meta["io.modelcontextprotocol/serverInfo"]` in its result. Its
  `tools/list` result also gets `ttlMs: 3600000` and
  `cacheScope: "public"`, which `server/discover` always has.

## Errors

| Code | When |
|---|---|
| `-32700` | The line isn't UTF-8 or JSON. The reply's `id` is `null`. |
| `-32600` | A message isn't one JSON object, as a batch, or its `id` is neither a string nor an integer (the reply's `id` is `null`), or a request has no `method`. |
| `-32601` | The method is unknown. |
| `-32602` | The tool is unknown. |
| `-32022` | `_meta` gives a protocol version other than `2026-07-28`. `data.supported` lists the revisions spoken, and `data.requested` gives the one asked for. |

```json
{"error":{"code":-32022,"data":{"requested":"2024-01-01","supported":["2026-07-28","2025-11-25","2025-06-18","2025-03-26","2024-11-05"]},"message":"Unsupported protocol version"},"id":10,"jsonrpc":"2.0"}
```

A tool that fails isn't a protocol error: see [Answers](#answers).

For hooks and status lines, see [`guard`](cli.md#guard) and
[`statusline`](cli.md#status-line).
