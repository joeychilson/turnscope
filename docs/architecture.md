# Architecture

How Turnscope works: its data, its code, the processes that run, and how
it decides what to show. The menu bar app is in [app.md](app.md).

## What it does

The user runs several coding agents (Claude Code, Codex, OpenCode, Pi, Grok
Build, Antigravity) on several accounts: subscriptions whose limits reset, and API keys
that cost money. Turnscope does three things:

1. **Limits.** How much of each limit is left, when it resets, whether it
   lasts at the current pace, and how many hours of work are left.
2. **Usage.** What was used and what it cost, by day, week, month, model,
   project, agent, account or session.
3. **Alerts.** A limit running out, used up or back, and an account that
   needs signing in again.

It answers through the command line ([cli.md](cli.md)), an MCP server for
agents ([mcp.md](mcp.md)), and the menu bar app ([app.md](app.md)), which
runs the engine in its own process. Most of the code stores and queries
data. The hard parts are reading six agents'
formats correctly, and the forecast.

## Glossary

| Term | Meaning |
|---|---|
| Agent | A coding tool whose history and logins Turnscope reads, named by id: `claude-code`, `codex`, `opencode`, `pi`, `grok-build`, `antigravity`. |
| Agent folder | Where an agent keeps its history and logins, such as `~/.claude`. A second account's folder is another, such as `~/.claude-work`. |
| Provider | A company that sells model usage and reports limits: Anthropic, OpenAI, xAI, Google, OpenCode Go, OpenRouter. Its id is the one models.dev uses. |
| Account | What usage draws on at a provider: a subscription, or API keys. Its id is `<provider>:<the provider's id for it>`. |
| Login | What an agent keeps to reach a provider: an OAuth token or an API key, in its files or the Keychain. Turnscope reads it when needed and never stores it. |
| Limit | One cap on an account, in percent used, as the provider reports it: a 5-hour or weekly limit, credits, a key's spending limit. |
| Pool | A group of models whose limits an account has in place of limits on all its usage, as a Google AI plan has a pool for Gemini and one for Claude and GPT. |
| Deciding limit | The limit that says how an account stands: of its limits on all its usage, or of the pool it last drew on, the most urgent. [Status](#status) says how it's chosen. |
| Window | The time a limit counts, from its start to its reset. A rolling window has no start. |
| Reset | When a window ends and its limit is full again. |
| Outlook | Where a limit is headed by its reset: `lasts`, `runsOut`, `usedUp` or `unknown`. |
| Standing | How the app shows an outlook: lasts (gray), running out (amber) or used up (red). An `unknown` outlook stands as lasts. An account's standing is its deciding limit's. |
| Pace | Percentage points of a limit used per hour of clock time in the current window. |
| Work rate | Percentage points of a limit used per hour that agents worked, as `40.9% an hour of work`. Pace counts hours of the clock instead. |
| Work left | Hours of agent work a limit has left, at its work rate. |
| Session | One conversation in an agent, with id `<agent>:<the agent's own id>`. A subagent is a session with a parent, and its usage counts toward that parent. A fork (a conversation branched off another) or a continuation is a session of its own. Turnscope keeps a session's title, project and usage, not what was said in it. |
| Response | The usage of a model call as the agent records it: tokens, cost, and the account it drew on. Some cover several calls: Grok Build records one per model per turn, Claude Code one with its calls listed in `iterations`, and Codex one for each growth of its running total beyond its records. |
| Reading | A limit as its provider reported it at one time. |
| Sign-in | What an agent folder held for a provider from a given time. It says which account a response drew on. |
| In use | An account a response drew on in the last 30 minutes, or whose limit rose between its last two readings in that time, as use elsewhere makes it. |
| Catch-up | Bringing the database up to date: prices and limits as they come due, and what's new in the agents' history ([Catching up](#catching-up)). |
| Data directory | Where Turnscope keeps its database, its locks and `app.log`: `~/Library/Application Support/com.joeychilson.turnscope`, or `--data`. |
| Engine | The `turnscope` crate: it reads history, logins, limits and prices, and works out what every front end shows. The command line, the MCP server and each app run it. |

## Data

| Source | Gives |
|---|---|
| Agents' history files | Responses, sessions, and the limits Codex writes as it works |
| Agents' login files and the Keychain | Which account each agent folder is signed in to, and the logins to read limits with |
| Providers' APIs | Readings |
| models.dev | Prices |

Everything is kept in one SQLite file, `turnscope.db`, in the data
directory. Times are UTC milliseconds and money is dollars. The schema is
`db::REVISIONS` in `crates/turnscope/src/db.rs`:

| Table | Holds |
|---|---|
| `file` | Each history file: its size, modification time and inode when last read, its reader's cursor, how many lines it couldn't read, why it couldn't be read the last time it failed, and the reader version it was last read with |
| `session` | Each session: agent, project, title, and the session it's a subagent of |
| `session_file` | Which files tell of each session |
| `response` | One row per response: tokens, prompt size, the cost the agent recorded, its cost, its account, and the file that last reported it |
| `account` | Each account: provider, kind, title, label, whether it's hidden, its last reading, the last read of all its limits that worked, when its provider was last asked, and what went wrong then |
| `failing` | Each account whose last read failed: how many reads in a row failed that way, what the last met, and when it's read next |
| `reading` | Each limit at each read: name, scope, percent used, size in dollars, the window's start and reset |
| `sign_in` | What each agent folder held for each provider, from when: the account, and an API key's fingerprint |
| `alert` | Each alert raised, what it says, and whether the app acknowledged it |
| `price` | models.dev's prices for each model, each from when it was read |
| `state` | One row: the latest reader version that read the history, the newest Turnscope version that has written, when its first read finished, when limits and prices were last read, and the settings |

`db::open` creates or migrates the database. Each revision is one script: a
shipped script is never edited, and a change is a new script at the end.
Before creating its tables in a file that has none of Turnscope's yet,
`db::open` checks the file's SQLite application id, so it never writes
into another program's database.

Turnscope 0.1 and 0.2 kept `turnscope.sqlite`, with six revisions of their
own. A new `turnscope.db` takes in all of it once, by its columns' names
(`db::import`), and leaves it as it was. `serve.lock`, `serve.log`
and `serve.sock`, which their `serve` kept beside it, are removed then. A later release
removes the import, and the old file with it.

- **History is read incrementally.** Each file keeps its reader's cursor,
  so only what was appended is read, and an unchanged file is only
  stat'ed. A file that was replaced or cut short is read from the start.
  A moved file is read on from where it was. A file is taken as moved
  when its old path is gone and its inode turns up at a new path, as when
  an agent archives it.
- **A response is one row**, however many files report it: a line
  rewritten as it streams, a fork's copy, a continuation's.
  `agents::Response::merge` combines reports the same way in any order: the
  largest of each count, the latest time, and the session that owns the
  response over one holding a copy.
- **Cost and account are set when a response is written**: its cost at the
  price in force when it ran, and its account from its agent folder's
  sign-in in force then (before the first sign-in recorded, the first).
  They're worked out again only for the responses a change touches, in the
  write that records the change, so a process that ends between the two
  can't lose it: a model priced for the first time (`ingest::price_again`),
  or an agent folder whose sign-in to a provider changed
  (`limits::record`). A read of history takes in the prices and sign-ins
  at each of its writes, so it writes none gone stale, and neither a price
  refresh nor a hook reading limits waits out a read of history.
- **Some tables can't be rebuilt.** Accounts, readings, sign-ins, past
  prices, alerts and settings exist only here, so they're never deleted.
  History can be read again.
- **When a reader changes, `ingest::READERS` is raised.** Each file keeps
  the version it was last read with, and one read with an older one is
  read again from the start when it's next found: where it was, moved, or
  back after it was gone. Its responses, and which sessions it holds, are
  taken out first. The sessions only such files hold lose their title and
  parent, and keep their project, worked out from the folders when the
  session was first seen: a worktree may be gone since. Responses and
  sessions from files the agent has since deleted stay.
- **An older Turnscope still running**, such as an agent's MCP server
  started before an update, writes nothing once a newer one has written:
  `state` holds the newest version that has, and each function that
  writes checks it first (`db::writing`). It answers from what's there,
  where its queries still fit the schema. If an update has replaced its
  binary since it started, it says to start it again; otherwise, to update
  Turnscope. An app's engine stops keeping the data current instead, and
  the app says why. `guard` guards by the limits read before. Each write
  of more than one statement checks again inside it (`db::write`), as one
  may have waited, for `ingest.lock` or for a provider. One started after
  the schema changed doesn't open the data, and says to update
  (`db::open`).

## Code

Two crates. `crates/turnscope` is the engine, a library every front end
uses, and the `turnscope` binary: the command line and the MCP server.
`crates/turnscope-macos` is the menu bar app ([app.md](app.md)); an app
for another platform would be a crate beside it. Each module holds its own
data, SQL and text. There's no central object, store layer or view types.

```
crates/turnscope/src/            the engine, a module per job (ingest, limits, prices,
                                 forecast, status, usage, alerts, engine); the command
                                 line (main.rs) and the MCP server (mcp.rs)
crates/turnscope/src/agents/     one file per agent, and agents::ALL
crates/turnscope/src/providers/  one file per provider, and providers::ALL
crates/turnscope-macos/src/      the menu bar app
```

## Extension points

Each kind of extension lives in one place.

| To add | Write | Also |
|---|---|---|
| An agent | `crates/turnscope/src/agents/<agent>.rs`, implementing `Agent` | A line in `agents::ALL` ([adding-an-agent.md](adding-an-agent.md)) |
| A provider | `crates/turnscope/src/providers/<provider>.rs`, implementing `Provider` | A line in `providers::ALL` ([adding-a-provider.md](adding-a-provider.md)) |
| An MCP tool | A function `fn(&Ask, Value) -> Result<String>` in `mcp.rs`, answering in markdown | An entry in `mcp::TOOLS`: name, title, description, input JSON Schema, and the function |
| A CLI command | A variant of `main::Command` | Its arm in `main::answer`, and whether it catches up first |
| Something an app shows or asks | An `engine::Event`, or a method of `engine::Engine` | Its handling in each app's `engine.rs` |
| A database change | A new script at the end of `db::REVISIONS` | |

Each trait's methods, `Agent` in `agents/mod.rs` and `Provider` in
`providers/mod.rs`, have doc comments.

## Processes

### Catching up

`turnscope::catch_up` brings the database up to date, in this order:

1. Prices, when six hours have passed since any process last took
   models.dev's catalog in, ten minutes after models.dev couldn't be read,
   or two minutes after a process asked it and ended before the answer.
2. Limits, of each account as they come due (`limits::Schedule`): 5
   minutes after its last reading while it's in use, 20 while it isn't,
   and right after one of its windows resets. All of an account's limits
   are read at least every 20 minutes, whatever an agent writes of some of
   them meanwhile. The agents' logins are gathered for each read, and
   every 5 minutes otherwise: an account first signed in to then is read
   at once. An account whose
   provider answered but was busy or not understood waits longer: 5
   minutes, then 10, 20 and 30 at most, or as long as the provider asks,
   up to 30 (`limits::backoff`). One that didn't answer, as while the Mac
   is offline, doesn't. A read that fails differently from the one before
   is added to `app.log`.
3. The agents' history, every time, with the limits Codex writes as it
   works (`limits::record_in_band`).

With `TURNSCOPE_OFFLINE` set to anything but `0`, it skips prices and
limits. A provider out of reach leaves things as they were.

| Process | Catches up |
|---|---|
| A CLI command that reports data | Before it answers. `connect`, `disconnect`, and `accounts hide` and `show` don't. |
| `guard` | Limits only, when due, and not while an app runs. A hook waits for it, so it reads no history or prices. |
| `statusline` | Never. It reads the database alone. |
| `mcp` | Before each tool call, history only while an app runs. |
| An app | On a thread of its own (`engine`): at start, with the limits first; 250 ms after the agents' folders stop changing (a second at most) but no sooner than 5 s after the last catch-up; and at least every minute. As the panel opens, or an agent writes a login file (`Info::login_file`; a login in the Keychain is found at the next 5-minute gather), it reads the limits more than 5 minutes old, and those whose read failed, right away, at most every 30 s (`limits::refresh`). |

While an app runs, the other processes rarely find anything new to read.

A catch-up reads all of history the first time, and again after
`ingest::READERS` is raised. That can take minutes. A CLI command says so
on standard error, and says when it waits for another process that reads
history (`ingest::waits`), as [cli.md](cli.md#global-options) shows. `mcp`
and the app say nothing there, since no one reads their standard error. A
tool call waits a second at most for another process's read
(`mcp::PATIENCE`), then answers from what was read and says so: while
all of history is read, that its usage is only part of it
(`ingest::all_read`). When no
other process reads, the call reads history itself, all of it the first
time, and answers after. The panel says the history is being read.

### Locks

Three files in the data directory keep processes apart:

- `turnscope.lock` while the database is created or migrated.
- `ingest.lock` while history is read, so a first run isn't read twice at
  once.
- `app.lock`, while an app keeps the data current. A second app shows what
  the first writes, and takes over once it's gone.

Limits and prices are claimed with one `UPDATE` of `state`, so no two
processes ask a provider at once.

SQLite lets one connection write at a time, and another waits up to 10 s
for its turn. History is written a second at a time, with 110 ms between,
so a long read never makes another process's write wait more than a second
or so.

### The app's engine

`engine::Engine` is what an app runs ([app.md](app.md#how-data-flows)).
It's in the engine crate, so an app for another platform only draws.

- One thread keeps the data current while it holds `app.lock`: it watches
  every agent folder, an agent's own only once the agent is installed, and
  catches up as above. A write to a SQLite database's `-shm` file alone
  starts no catch-up: Turnscope writes it itself by opening the database.
- After a catch-up that read anything new, ran in a new minute, or
  followed another process's write to the database, it works out the
  status. If anything it shows changed, it raises alerts and tells the app
  the status with the alerts (`Event::Shown`). A status that differs from
  the last told only in the minute it's as of isn't told: the app counts
  down on its own clock. Each agent's MCP settings are read once a minute
  and after a connect, not with every status.
- At start, it reads the limits, when they're due, before its first
  catch-up, and tells the status then too. On a first launch the catch-up
  reads all of history, so the app shows the accounts while it does, and
  the status says the history isn't read yet (`historyRead`).
- Requests (hiding an account, saving the settings, acknowledging alerts,
  asking what used a limit) run one at a time, in the order asked, on a
  thread of their own; connecting an agent, on another, as the agent's
  command can take a minute. The status is told again after a hide, a
  save of the settings and a connect, changed or not, so a change the app
  showed at once goes back if the request failed. Whichever thread works
  out a status, statuses are told in the order they're worked out.
- The data is said to be current (`Event::Kept`) once a catch-up has read
  every agent. If one fails, it tells the app why and tries again: at the
  next change in the agents' folders, or the next minute. If keeping the
  data current can't start, it tries again every 5 s.
- Once a newer Turnscope has written to the data ([Data](#data)), it
  stops for good: it writes nothing and raises no alert, a hide or a save
  of the settings tells the last status again, and a connect runs no
  command and says why.
- What goes wrong is added to `app.log` in the data directory, which
  `turnscope doctor` shows: an app is started from its icon, where no one
  reads its standard error.

## Status

`status::status` works out everything the app, the CLI and the MCP server
show, with every decision already made. Its fields are documented on its
types. The agents are worked out apart, by `status::agents`, for the app
and `turnscope connect` only: whether an
agent is connected is read from its MCP settings, and Claude Code's can be
megabytes.

- A status is worked out as of the minute, so between readings it changes
  at most once a minute.
- Percent left is rounded down, so it's never overstated. A limit whose
  window reset since it was read counts as full.
- Each time to come is rounded to the minute and carries how soon it is, a
  `Moment`.
- An account is **recent** when it's [in use](#glossary) or was drawn on
  in the last 7 days.
- An account's **state** says whether its limits were read at the last
  try, and if not, why (`status::State`).
- A limit's outlook is `unknown` (`stale`) once its latest reading is over
  30 minutes old, unless it was used up: it stays `usedUp` until its
  reset, as nothing gives use back sooner but an early reset, which a read
  shows.
- The **deciding limit** says how an account stands. It's chosen from the
  limits not reset since they were read, and of those, from the limits on
  all of the account's usage (not one model's, other apps', or one key's)
  where there are any. An account with none, as a Google AI plan with a
  pool for Gemini and one for Claude and GPT, stands by one pool: the one
  its latest response drew on, or with none, the first listed
  (`status::pool`). Its deciding limit is that pool's. The most urgent
  wins (used up, then running out), then the soonest out, then the least
  left (at the reset for a limit that lasts, where that's forecast, or else
  now). Of several used up, the last back wins, since the account can't be
  used until then; one with no reset counts as later than any. When every
  limit reset since, it's the last to reset.
- **Accounts are listed** in use first (the most urgent, soonest out and
  least left first), then the rest (the most left first), then those whose
  login was refused, then those with nothing to show. Until the first read
  of history ends, every account is listed as one in use, since which are
  isn't known yet.
- A **fit** (`status --hours`, `check_limits` with `hours`) and
  `check_limits`' verdict go by the limit the account stands by (on all its
  usage, or of its pool) with the fewest likely hours of work left. A limit
  that reset since it was read counts, with a new window's work left. A fit
  counts work past a limit's reset in the windows after it, and can't be
  told while a limit it stands by has no work rate measured. The verdict
  says what's left of the deciding limit when that one has none.
- Each **agent** is `installed` when its own folder exists, and its
  connection says whether each of its folders runs this binary's MCP
  server (`status::Connection`).

## Prices

A response costs, in this order:

1. What the provider charged, where the agent records it (Grok Build).
2. Otherwise, models.dev's list price for its provider and model, or
   where models.dev doesn't list the model under the provider serving it,
   its maker's (`Provider::makes`), as for Claude served to Antigravity.
3. Otherwise, the agent's own estimate (OpenCode, Pi).

A recorded cost of zero is usage a subscription covered, so it's passed
over. A response with none of these has an unknown cost, never zero, and
`doctor` lists its model.

models.dev's catalog is `https://models.dev/api.json`, read as [Catching
up](#catching-up) says. A catalog with fewer than 1,000 priced models is
refused. A price that changed is kept from then on beside the one before,
so each response keeps the price in force when it ran. Usage from before a
model was first priced takes that first price, and usage with no known
price is priced again each time the catalog is taken in.

`prices::Prices::cost` says how tiers, fast mode, cache and reasoning
are priced.

## Forecast

`forecast.rs` holds the method, what was chosen over what, and what the
backtest measured ([development.md](development.md#forecast-backtest)
says how to run it). `status.rs` applies it.

- **Pace** is the points used so far divided by the hours so far in the
  window, run forward to the reset to give the outlook.
- **Not enough data** gives `unknown` early in a window, after little use,
  or when the run-out is too far off to tell.
- **Ranges** are 80% ranges, scaled by how the pace that followed compared
  with it in the backtest.
- **Work left** divides what's left by the work rate, measured over recent
  hours of work on the account.

## Alerts

| Kind | Raised when |
|---|---|
| `runningOut` | A limit of an account in use runs out before its reset at its pace, as the status shows it |
| `usedUp` | A limit of an account in use is used up: one with a window, or money that never resets, such as credits |
| `reset` | A limit that was used up is back: its window ended in the last day, use was given back early, or money that never resets has some left |
| `signIn` | An account in use can't be read because its login was refused at two reads in a row |

- An app's engine raises alerts (`alerts::raise`) each time it tells a
  changed status. The CLI and the MCP server don't.
- Each is raised once per account, limit, kind and window (the window's
  reset, within 5 minutes; for `signIn`, the last read that worked), so
  neither a restart nor a pace near the line repeats one.
- Money that never resets has no window, so its size stands in for one:
  credits used up are told once, and again when more were bought and used
  up too. A limit with neither a window nor a size, a window whose reset
  its provider didn't say, raises nothing.
- Some alerts are recorded as already shown: one for a hidden account, one
  of a kind turned off, and a `reset` while another limit on the same models or on the whole
  account is still used up. Showing the account or turning the kind on
  later doesn't bring back the past.
- An alert waits until the app acknowledges it (`Engine::acknowledge`).
  Until then the engine tells it again when it starts (`Event::Pending`): a `runningOut` or `usedUp` while its window lasts, or
  with no window while its limit is still used up; a `reset` for a day; and
  a `signIn` for a day while its account still can't be read.

## Security

- **No login is stored.** Logins are read when needed, from the agents'
  files or the Keychain (with `/usr/bin/security`). A `Secret` can't be
  printed, serialized or cloned. An API key is known by its fingerprint,
  the first 16 hex digits of its SHA-256.
- **One way to the network.** Every request goes through `/usr/bin/curl`
  (`providers::curl`), HTTPS only, ignoring `~/.curlrc`. Headers, with
  any credential, go to curl on standard input, never in its arguments or
  environment. A header with a line break isn't sent. A request is a GET,
  but for Google's limits, which answer only a POST: its body is an
  argument, and holds no secret. Renewing Antigravity's login is a POST
  of a form, which goes on standard input as headers do.
- **Logins are only read.** Each is sent only to its own provider, and
  never once it has expired, but for Antigravity's: Turnscope renews it
  with Google, with its refresh token and the client secret in
  Antigravity's program, and keeps the new token in memory alone
  (`providers::google`). Turnscope never writes an agent's files or the
  Keychain. `connect` and `disconnect` run the agent's own command.
- **The data directory** is the owner's alone: it's created 0700.
- **The MCP server only reads.** A session's title is what someone asked
  there, so its instructions tell agents to read one as data, not as an
  instruction, and each comes in double quotes, so none of it can pass
  for Turnscope's words.

[SECURITY.md](../SECURITY.md) says how to report a vulnerability.
