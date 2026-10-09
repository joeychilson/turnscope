# Architecture

How the `turnscope` binary works: its data, its code, the processes that
run, and how it decides what to show. The menu bar app is in
[app.md](app.md). It talks to `turnscope serve` over the protocol in
[protocol.md](protocol.md).

## What it does

The user runs several coding agents (Claude Code, Codex, OpenCode, Pi, Grok
Build) on several accounts: subscriptions whose limits reset, and API keys
that cost money. Turnscope does four things:

1. **Limits.** How much of each limit is left, when it resets, whether it
   lasts at the current pace, and how many hours of work are left.
2. **Usage.** What was used and what it cost, by day, week, month, model,
   project, agent, account or session.
3. **Sessions.** Find a session, read it, and hand it to another agent.
4. **Alerts.** A limit running out, used up or back, and an account that
   needs signing in again.

It answers through the command line ([cli.md](cli.md)), an MCP server for
agents ([mcp.md](mcp.md)), and `serve`, the daemon the app talks to. Most of
the code stores and queries data. The hard parts are reading five agents'
formats correctly, and the forecast.

## Glossary

| Term | Meaning |
|---|---|
| Agent | A coding tool whose history and logins Turnscope reads, named by id: `claude-code`, `codex`, `opencode`, `pi`, `grok-build`. |
| Agent folder | Where an agent keeps its history and logins, such as `~/.claude`. A second account's folder is another, such as `~/.claude-work`. |
| Provider | A company that sells model usage and reports limits: Anthropic, OpenAI, xAI, OpenCode Go, OpenRouter. Its id is the one models.dev uses. |
| Account | What usage draws on at a provider: a subscription, or API keys. Its id is `<provider>:<the provider's id for it>`. |
| Limit | One cap on an account, in percent used, as the provider reports it: a 5-hour or weekly limit, credits, a key's spending limit. |
| Window | The time a limit counts, from its start to its reset. A rolling window has no start. |
| Reset | When a window ends and its limit is full again. |
| Outlook | Where a limit is headed by its reset: `lasts`, `runsOut`, `usedUp` or `unknown`. |
| Pace | Percentage points of a limit used per hour of clock time in the current window. |
| Work left | Hours of agent work a limit has left, from the points it used per hour that agents worked. |
| Session | One conversation in an agent, with id `<agent>:<the agent's own id>`. A subagent, a fork (a conversation branched off another) or a continuation is a session with a parent. Only a subagent counts toward its parent. |
| Entry | One item of a session's conversation: a message, reasoning, a tool call with its output, a summary, a system note or a subagent's task. |
| Response | The usage of one model call as the agent records it: tokens, cost, and the account it drew on. Grok Build records one per model per turn. |
| Reading | A limit as its provider reported it at one time. |
| Sign-in | What an agent folder held for a provider from a given time. It says which account a response drew on. |
| Handoff | Where a session stands, for another agent to finish its work ([handoff.md](handoff.md)). |
| Data directory | Where Turnscope keeps its database, locks and socket: `~/Library/Application Support/com.joeychilson.turnscope`, or `--data`. |

## Data

| Source | Gives |
|---|---|
| Agents' history files | Responses and sessions |
| Agents' login files and the Keychain | Which account each agent folder is signed in to, and the logins to read limits with |
| Providers' APIs | Readings |
| models.dev | Prices |

Everything is kept in one SQLite file, `turnscope.sqlite`, in the data
directory. Times are UTC milliseconds and money is dollars. The schema is
`db::REVISIONS` in `src/db.rs`:

| Table | Holds |
|---|---|
| `file` | Each history file: its size, modification time and inode when last read, its reader's cursor, and how many lines it couldn't read |
| `session` | Each session: agent, agent folder, working folder, project, branch, title, parent and link, first and last activity |
| `session_file` | Which files hold each session's conversation |
| `response` | One row per response: tokens, prompt size, the cost the agent recorded, its cost, its account, and the file that last reported it |
| `said` | A full-text index (FTS5) of what the user and the models said. The text is indexed, not kept. |
| `account` | Each account: provider, kind, title, label, whether it's hidden, the last read that worked, and what went wrong with the last read |
| `reading` | Each limit at each read: name, scope, percent used, size in dollars, the window's start and reset |
| `sign_in` | What each agent folder held for each provider, from when: the account, and an API key's fingerprint |
| `alert` | Each alert raised, what it says, and whether a client acknowledged it |
| `price` | models.dev's prices for each model, each from when it was read |
| `state` | One row: the reader version the history was read with, when history, limits and prices were last read, and the settings |

`db::open` creates or migrates the database. Each revision is one script: a
shipped script is never edited, and a change is a new script at the end.
Before creating its tables in a file that has none of Turnscope's yet,
`db::open` checks the file's SQLite application id, so it never writes
into another program's database.

- **History is read incrementally.** Each file keeps its reader's cursor,
  so only what was appended is read, and an unchanged file is only
  stat'ed. A file that was replaced or cut short is read from the start.
- **A response is one row**, however many files report it: a line
  rewritten as it streams, a fork's copy, a continuation's.
  `agents::Response::merge` combines reports the same way in any order: the
  largest of each count, the latest time, and the session that owns the
  response over one holding a copy.
- **Cost and account are set when a response is written**: its cost at the
  price in force when it ran, and its account from its agent folder's
  sign-in in force then (before the first sign-in recorded, the first).
  `ingest::derive` works them out again only for the responses a change
  touches: a model priced for the first time, or an agent folder whose
  sign-in to a provider changed.
- **Some tables can't be rebuilt.** Accounts, readings, sign-ins, past
  prices, alerts and settings exist only here, so they're never deleted.
  History can be read again. When a reader changes, `ingest::READERS` is
  raised, and every file still on disk is read again from the start.
  Responses and sessions from files the agent has since deleted stay. An
  older Turnscope, such as an agent's MCP server started before an update,
  doesn't read history a newer one read: it says to update instead, as it
  does for a newer schema.
- **Transcripts aren't stored.** They're read from the agent's files when
  asked for, with secrets redacted.

## Code

One crate and one binary. Each module holds its own data, SQL and text.
There's no engine object, store layer or view types.

```
src/
  main.rs             the command line: commands, catch_up, guard, statusline, doctor, exit codes
  db.rs               the schema, opening and migrating
  ingest.rs           agents' files → responses, sessions and search; each response's cost and account
  limits.rs           logins → accounts, sign-ins and readings
  prices.rs           models.dev's prices over time, and what a response cost
  forecast.rs         pace, outlook, ranges and work left: pure functions over readings
  status.rs           every account and limit, as every front end shows them
  status/text.rs      the status in words, for the command line and MCP
  alerts.rs           which alerts a status raises, which wait to be shown, and the settings for them
  usage.rs            usage grouped by time, model, project, agent, account or session; what used a limit
  sessions.rs         find, show and read sessions
  sessions/handoff.rs hand a session to another agent
  mcp.rs              the MCP server: who is asking, the tools, the prompt, the stdio loop
  serve.rs            the app's daemon: JSON-RPC on a Unix socket, keeping the data current
  connect.rs          adding and removing the MCP server with the agent's own command, and whether it's there
  time.rs             local time as people read and write it
  redact.rs           secrets out of transcript text
  agents/
    mod.rs            the Agent trait, agents::ALL, what readers return, shared helpers
    claude_code.rs    Claude Code
    codex.rs          Codex
    opencode.rs       OpenCode
    pi.rs             Pi
    grok_build.rs     Grok Build
  providers/
    mod.rs            the Provider trait, providers::ALL, what providers return, curl, shared helpers
    anthropic.rs      Anthropic: Claude plans, and API keys
    openai.rs         OpenAI: ChatGPT plans, and API keys
    xai.rs            xAI: SuperGrok plans, and API keys
    opencode_go.rs    OpenCode Go plans
    openrouter.rs     OpenRouter: a key's spending limit, and credits
```

## Extension points

Each kind of extension lives in one place.

| To add | Write | Also |
|---|---|---|
| An agent | `src/agents/<agent>.rs`, implementing `Agent` | A line in `agents::ALL` ([adding-an-agent.md](adding-an-agent.md)) |
| A provider | `src/providers/<provider>.rs`, implementing `Provider` | A line in `providers::ALL` ([adding-a-provider.md](adding-a-provider.md)) |
| An MCP tool | A function `fn(&mut Server, Value) -> Result<String>` in `mcp.rs`, answering in markdown | An entry in `mcp::TOOLS`: name, title, description, input JSON Schema, and the function |
| A CLI command | A variant of `main::Command` | Its arm in `main::answer`, and whether it catches up first |
| A `serve` method | Its arm in `serve::Server::answer` | Its example in `contract/` ([protocol.md](protocol.md)) |
| A database change | A new script at the end of `db::REVISIONS` | |

The two traits, from `src/agents/mod.rs` and `src/providers/mod.rs`:

```rust
pub trait Agent: Sync {
    fn info(&self) -> &'static Info;
    fn folders(&self, home: &Path) -> Vec<PathBuf>;
    fn files(&self, folder: &Path) -> Vec<PathBuf>;
    fn read(&self, file: &Path, cursor: &str) -> Result<Read>;
    fn transcript(&self, id: &str, files: &[PathBuf]) -> Result<Vec<Entry>>;
    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>>;
    fn mcp_command(&self, folder: &Path, home: &Path) -> Option<String>;
    fn role(&self, _name: &str) -> Role { Role::Work }
}

pub trait Provider: Sync {
    fn info(&self) -> &'static Info;
    fn account(&self, credential: &Credential) -> Option<Account>;
    fn limits(&self, credential: &Credential, now: i64) -> std::result::Result<Limits, Problem>;
    fn plan_mark(&self, _limits: &Limits) -> Option<i64> { None }
}
```

## Processes

### Catching up

`main::catch_up` brings the database up to date, in this order:

1. Prices, when six hours have passed since any process last asked
   models.dev, or ten minutes after a failed try.
2. Limits, when five minutes have passed since any process last read them.
3. The agents' history, every time.

With `TURNSCOPE_OFFLINE` set to anything but `0`, it skips prices and
limits. A provider out of reach leaves things as they were.

| Process | Catches up |
|---|---|
| A CLI command that reports data | Before it answers. `config`, `connect`, `disconnect`, and `accounts hide` and `show` don't. |
| `guard` | Limits only, when due. A hook waits for it, so it reads no history or prices. |
| `statusline` | Never. It reads the database alone. |
| `mcp` | Before each tool call, and before the `continue` prompt. |
| `serve` | On a thread of its own: at start, 250 ms after the agents' folders stop changing (a second at most), and at least every minute. |

While `serve` runs, the other processes rarely find anything new to read.

### Locks

Three files in the data directory keep processes apart:

- `turnscope.lock` while the database is created or migrated.
- `ingest.lock` while history is read, or costs and accounts are worked out
  again, so a first run isn't read twice at once and no response is written
  with stale prices or sign-ins.
- `serve.lock`, so only one `serve` runs per data directory. A second one
  says so and exits.

Limits and prices are claimed with one `UPDATE` of `state`, so no two
processes ask a provider at once.

### serve

`turnscope serve` listens on a Unix socket: `serve.sock` in the data
directory, or one under `$TMPDIR` when that path is too long for macOS
([protocol.md](protocol.md)).

- The main thread answers requests one at a time and pushes the status. One
  thread accepts connections, and one per client reads its lines.
- One thread keeps the data current: it watches every agent folder and
  catches up as above. After each catch-up, the main thread works out the
  status. If anything it shows changed, it raises alerts and sends `status`,
  then each new `alert`, to every client that said `hello`.
- `settings.set`, `account.hide`, and an `agent.connect` or
  `agent.disconnect` that worked push the status too. Connecting runs the
  agent's own command on a thread of its own, as it can take a minute.
- If the thread that keeps the data current stops, `serve` exits with an
  error, so the app starts one that works.
- It exits `--linger` seconds (default 60) after its last client leaves.

## Status

`status::status` works out everything the app, the CLI and the MCP server
show, with every decision already made. Its fields are in
[protocol.md](protocol.md#status).

- A status is worked out as of the minute, so between readings it changes
  at most once a minute.
- Percent left is rounded down, so it's never overstated. A limit whose
  window reset since it was read counts as full.
- Each time to come is rounded to the minute and carries how soon it is (a
  `Moment`): `soon` within 3 hours, `today`, `thisWeek` within 7 days, or
  `later`.
- An account is **in use** when a response drew on it in the last 30
  minutes, or a limit rose between its last two readings in the last 30
  minutes, as use elsewhere makes it. It's **recent** when in use or drawn
  on in the last 7 days.
- An account's **state** says how its limits were last read: `live`;
  `asOf` an earlier read; or `unread`, never read. The last two say why:
  no agent here is signed in to it (`signedOut`), its login was refused
  (`signIn`), every login to it expired (`expired`), or it couldn't be read
  (`readFailed`): its provider failed, or nothing has read it for 30
  minutes.
- A limit's outlook is `unknown` (`stale`) once its latest reading is over
  30 minutes old.
- The **deciding limit** says how an account stands. It's chosen from the
  limits not reset since they were read, and of those, from the limits on
  all of the account's usage (not one model's or one key's) where there are
  any. The most urgent wins (used up, then running out), then the soonest
  out, then the least left (at the reset for a limit that lasts, where
  that's forecast). When every limit reset since, it's the last to reset.
- **Accounts are listed** in use first (the most urgent, soonest out and
  least left first), then the rest (the most left first), then those whose
  login was refused, then those with nothing to show.
- Each **agent** is `installed` when its own folder exists. Its connection
  is `connected` when the MCP configuration of every one of its folders
  runs this binary, `outdated` when one runs another copy, and `available`
  otherwise.

## Prices

A response costs, in this order:

1. What the provider charged, where the agent records it (Grok Build).
2. Otherwise, models.dev's list price for its provider and model.
3. Otherwise, the agent's own estimate (OpenCode, Pi).

A recorded cost of zero is usage a subscription covered, so it's passed
over. A response with none of these has an unknown cost, never zero, and
`doctor` lists its model.

models.dev's catalog (`https://models.dev/api.json`) is read when six hours
have passed since any process last did, or ten minutes after a failure. A
catalog with fewer than 1,000 priced models is refused. A price that
changed is kept from then on beside the one before, so each response keeps
the price in force when it ran. Usage from before a model was first priced
takes that first price.

A list price is in dollars per million tokens (`prices::Prices::cost`):

- A prompt larger than a tier's size takes that tier's rates (`tiers`, or
  `context_over_200k` for a model that lists no tiers).
- A response at priority takes the rates of the model's fast mode, where
  models.dev lists one.
- Cache reads and writes take their own rates, or the input rate where none
  is listed. A cache write kept an hour costs twice the input rate.
- Reasoning takes its own rate, or the output rate.
- A web search costs $0.01.

## Forecast

The method was chosen by replaying this Mac's real limit history
(`forecast::tests::backtest`; [development.md](development.md) says how to
run it). Its tuning is in `forecast.rs`, and `status.rs` applies it.

- **Pace.** Points used so far divided by hours so far in the window,
  counted from the window's start, or from when use was last given back (a
  drop of more than a point, as at an early reset). Time with no new
  reading counts as time nothing more was used. Least squares, recent 1-,
  3- and 24-hour rates, weighted rates, blends and a duty cycle did no
  better on windows of a day or less, and worse on longer ones, where they
  raised false alarms.
- **Not enough data.** The outlook is `unknown` (`notEnoughData`) when the
  window has no reading, less than a twentieth of it has passed (15 minutes
  at least), or more than 0 but fewer than 2 points were used. Providers
  report whole percents, so one step of rounding would read as a pace. It's
  also `unknown` when the pace runs out more than a quarter of the window
  from now (`forecast::told`), as after a burst at a window's start:
  replayed on four weeks of this Mac's work, run-outs told that early came
  true 56–76% of the time, and within a quarter 75–100%.
- **Outlook.** The pace is run forward to the reset.
  - `usedUp`: 100% used, with when it's back.
  - `runsOut`: it reaches 100% at least a fiftieth of the window (and at
    least 15 minutes) before the reset, and within a quarter of the window
    from now. The time is shown to 10 minutes
    within a day and to the hour beyond, and never before now.
  - `lasts`: with what will be left at the reset, as a range, or nothing
    when none of it has been used.
  - `unknown`: not enough data, `stale` (read over 30 minutes ago, or the
    window reset since), `rolling` (no start to pace from) or `noReset`
    (credits).
- **Range.** The pace is scaled by the 10th and 90th percentiles of how the
  pace that followed compared with it in the backtest (`BANDS`), by window
  length and the share of the window left. The ranges held 82% of the
  time. The fast end gives the soonest run-out and the least left; the slow
  end, the latest and the most.

  | Share of the window left | Day or less | Longer |
  |---|---|---|
  | Under 10% | 0.05–2.5 | 0–3.4 |
  | 10% to 25% | 0.05–2.5 | 0–3.4 |
  | 25% to 50% | 0.25–1.8 | 0–3.4 |
  | 50% or more | 0.35–1.6 | 0.7–3.4 |

- **Work left.** The rate is the points a limit rose per hour of work. An
  hour of work is twelve 5-minute slots that each hold a response on the
  account. The rate walks back over the limit's readings, across windows
  and up to 14 days, until it has 3 hours of work (5 for windows longer
  than a day), and needs at least an hour of work and 2 points. Work counts
  only from when the account's first sign-in was read, so work on another
  account before Turnscope ran isn't taken for this one's. Hours of work
  left are what's left divided by the rate. The rate's 80% range is 0.5 to
  1.6 times it for windows of a day or less, and 0.4 to 2.2 for longer
  ones. When the likely hours reach the reset, the limit lasts even with
  work all the time, and no hours are given. Work left isn't given for a
  rolling window. On weekly limits, the clock's pace, which counts nights
  and breaks, understated what more hours of work use by about half; the
  rate per hour of work showed no bias.

## Alerts

| Kind | Raised when |
|---|---|
| `runningOut` | A limit of an account in use runs out before its reset at its pace, as the status shows it |
| `usedUp` | A limit of an account in use is used up |
| `reset` | A limit that was used up is back: its window ended in the last day, or use was given back early |
| `signIn` | An account in use can't be read because its login was refused |

- `serve` raises alerts (`alerts::raise`) each time it pushes a changed
  status. The CLI and the MCP server don't.
- Each is raised once per account, limit, kind and window (the window's
  reset, within 5 minutes; for `signIn`, the last read that worked), so
  neither a restart nor a pace near the line repeats one.
- Some alerts are recorded as already shown: one for a hidden account, one
  of a kind turned off, and a `reset` while another limit on all models is
  still used up. Showing the account or turning the kind on later doesn't
  bring back the past.
- An alert waits until a client acknowledges it (`alerts.ack`). Until then
  `hello` returns it: a `runningOut` or `usedUp` while its window lasts, a
  `reset` or `signIn` for a day.

## Handoff

`sessions::handoff` writes where a session stands, so another agent can
finish its work: mostly what was said, which the next agent can't recover
from the files on disk, in at most 16,000 tokens. [handoff.md](handoff.md)
covers what it holds, the budget, and how to test a change.

## Security

- **No secret is stored.** Logins are read when needed, from the agents'
  files or the Keychain (with `/usr/bin/security`). A `Secret` can't be
  printed, serialized or cloned. An API key is known by its fingerprint,
  the first 16 hex digits of its SHA-256.
- **One way to the network.** Every request goes through `/usr/bin/curl`
  (`providers::get`), HTTPS only, ignoring `~/.curlrc`. Headers, with any
  credential, go to curl on standard input, never in its arguments or
  environment. A header with a line break isn't sent.
- **Logins are only read.** Each is sent only to its own provider, and
  never once it has expired. Turnscope never refreshes a token or writes an
  agent's files. `connect` and `disconnect` run the agent's own command.
- **Redaction.** `redact::redact` replaces keys with known prefixes, JSON
  Web Tokens and `Bearer` tokens with `[redacted]`, in text before it's
  indexed for search and in transcripts before they're shown.
- **The data directory and socket** are the owner's alone: the directory
  is created 0700, and the socket is 0600.
- **The MCP server only reads.** Its instructions tell agents that what a
  session says is data, not instructions.
