# Changelog

## Unreleased

- **Hidden accounts and limits that have recovered aren't alerted on
  launch.** Unacknowledged alerts from before hiding an account or before
  use was given back early were still sent when the app reopened.
- **A second open copy of the app sends no alerts.** Only the copy keeping
  the data current raises and sends notifications, so alerts aren't sent
  twice.
- **Opening the panel or pressing ⌘R shows fresh limits right away**,
  without waiting for history to finish reading.
- **Responses priced from their maker's catalog replace earlier estimates
  once that model is first listed.** Usage recorded under another
  provider, such as Claude on Antigravity, kept an agent's own estimate
  instead of taking the maker's price.
- **`connect` and `disconnect` show an agent's error when it writes to
  standard output**, instead of a blank message, and aren't thrown off
  when a login shell prints text on exit.
- **Codex's responses after fast mode is turned off cost the standard
  rate**, not the fast one. The history is read again once, to price them
  anew.
- **`turnscope guard` passes while the standing isn't known**: before any
  account is read, while none of its limits are, or for a key with none,
  as an Anthropic API key has. It exited 1 with an error, which a hook
  shows at every tool call.
- **A notification clicked as the panel opens shows its account once the
  limits are read**, even if Settings was opened meanwhile. The account
  opened on its own later instead.
- **An API key whose limits were never read doesn't say it has none.** The
  panel says they couldn't be read yet, as `turnscope status` does, not
  "No limit" and "You can keep going".
- **Settings shows the notification switches once it knows them.** Before
  the first read, all four showed on, whatever you had set.
- **A key's pace isn't said once it rounds below what's spent**, late in a
  month: "$155 this month" no longer comes with "On pace for about $150".
- **Hours of work left are told once an hour of work is measured.** Work
  in several stretches could add up to just under an hour, and say there
  was too little to tell.
- **Credits used up say so** in their row, not "Credits back later": they
  come back only when bought.
- **A limit used up again after its use was given back is told again**,
  and told back when its window ends. Only the first time was.
- **"Refreshing limits…" shows until the limits are read.** Hiding an
  account, changing a setting or connecting an agent meanwhile ended it
  early, and ⌘R could start another refresh.
- **Hours of work left are rounded once**, so 33 minutes reads "about
  30m", not "about 40m". `status --json` gives them as `leftMinutes` and
  `workedMinutes`, in minutes as they're said, in place of `leftHours` and
  `workedHours`.
- **Copy Diagnostics lists each line of `app.log` once.** The latest were
  in doctor's notes, then again after them.
- **A key's limit says whose key it is** in `turnscope status` and agents'
  answers: "Pi's weekly key limit", not "Weekly key limit", which two keys
  with the same period both said. `--limit pi` names it.
- **A login refused or expired names its agents as a list**: "open Claude
  Code, OpenCode or Pi to sign in again", not "open Claude Code or
  OpenCode or Pi".
- **A ChatGPT plan is read even when one of its model limits isn't
  understood.** That limit is left out, instead of the whole plan reading
  as failed.
- **A refused login is told again after a read that worked in between**,
  however soon. One within 5 minutes of the last read that worked before
  was taken for the same refusal.
- **`turnscope disconnect` says the command to run by hand** when it
  can't run the agent's own, as `connect` does.
- **The window that offers an update shows what's new in it**, instead
  of loading its whole GitHub release page.
- **A limit in dollars is shared by dollars.** Of an OpenRouter key's
  limit, a session's share, in the panel's Used most and in answers, is its
  cost over the limit's size, not its part of the cost here, and a last
  line of an answer says how much of what's used was spent elsewhere with
  the key.
- **No shares of a limit are given from a read that's out of date** while
  your agents have worked on it since, in `usage --limit`,
  `explain_usage` and the panel's Used most, as `check_limits` already
  can't tell then. The answers say why, unless nothing was used. Before,
  the last read's use was spread over work it doesn't hold. A limit in
  dollars still gives them: a share of it is cost over its size.
- **A key's sessions that cost more than its last read says is used say
  both**: "Your agents here account for $12.00; the last read, at 11:50,
  says $9.00 is used." So do they from a read that's out of date, while
  your agents have worked since.
- **Whether more hours fit isn't said of a read that's out of date** when
  your agents have worked on the account since it: `check_limits` and
  `status --hours` say what was left then, and how long they've worked.
- **A limit answers to the name the status line shows**, such as `5h`, in
  `guard`, `usage --limit` and `explain_usage`.
- **An MCP input given as `""` is taken as not given**, as `null` is.
- **Of one account, whether more hours fit and how it stands are one
  line**, with the range and reset the verdict gives; a refused login is
  said once.
- **`explain_usage` since a time leaves out the limit of now** unless one
  is named: its line and shares were of a window that time isn't in.
- **Usage of credits says which it is and how much of them is used**:
  this month's for an API key, since credits never reset.
- **A date in another year shows its year**, and a span of a year or more
  is in years: `since Jan 1 2020 00:00 (6y ago)`. A table no longer adds
  up a lone row as "1 more": it's shown by name.
- **An answer from ChatGPT or Claude that isn't understood keeps the
  last limits**, shown as of their read, instead of a plan without limits
  or without its 5-hour window, as a renamed field read.
- **OpenRouter's credits stay shown when only they couldn't be read**,
  as of their last read, instead of going missing until the next read.
- **Refresh Limits reads a provider that keeps failing** at once, instead
  of waiting up to 30 minutes for its next try. One that asked to wait is
  still left until then.
- **A forecast says how bad it could get, not a range.** A limit that
  lasts says "about 44% left at the reset (as little as 30%)", and one that
  runs out says how soon, not how late. On real limit history, the old 80%
  ranges held 62–71% of the time; the new bound holds 85–89%. A least of
  0% isn't said: it was half the forecasts of weeks and months, and 21% of
  those ran out against 16% of the rest.
- **Work left says how little it could be, not a range**: "About 19h of
  work left (as little as 7h 50m)". On real limit history, that held 90–92%
  of the time for Claude's limits; the range it replaces, as "(20m–1h
  10m)", held 65–78%.

## 0.4.1

- **The data directory no longer keeps hundreds of MB of write-ahead
  log.** `turnscope.db-wal` is cut back to 16 MB once SQLite checkpoints
  it; before, one large write left it that size while the app ran.
- **`app.log` says once why the data can't be kept current**, not at
  every try: a database that couldn't be opened added a line every 5
  seconds and pushed out what was logged before.
- **`doctor` says why models.dev's catalog was refused**, as when it
  answers with an error, and no longer lists unreadable lines of history
  files since deleted.
- **Data from Turnscope 0.1 and 0.2 is no longer carried over.** Updating
  straight from them starts anew: the agents' history is read again, but
  usage older than the agents keep (Claude Code keeps 30 days), past
  limits, alerts and settings aren't. `turnscope.sqlite`, which 0.3 and 0.4
  copied, can be deleted.
- **Opening the panel or refreshing limits while a `turnscope` command
  reads them** waits for that read instead of asking each provider again
  at once, which could count a login refused once as refused twice and
  ask you to sign in again.
- **`turnscope usage --until` past now counts to now**, as `explain_usage`
  does, so the heading no longer names an end that hasn't come.
- **`turnscope usage` says "1 response"**, not "1 responses", and tells
  its times by the minute they're in, as every other answer does.
- **`check_limits` says why hours of work can't be told.** An account not
  read says why, such as "its login was refused". A limit with too little
  work measured says whether it lasts at its pace until it resets. An API
  key with no limit read says what it cost this month.
- **`check_limits` with `all` names the account with the most room**, to
  choose where to work, in place of the verdict on yours. Given `hours`,
  it leaves out an account they don't fit.
- **`explain_usage` groups by account**, as `turnscope usage --by account`
  does.
- **An MCP input that doesn't fit says what the input takes**, such as
  `hours is a number of 0 or more, not "two"`. A `top` outside 1 to 20 is
  refused instead of moved into range. A `since` at or after `until` or
  now says which.
- **`turnscope status --hours` says whether the hours fit each account
  shown**, not only the first, and with `--json`, `fit` is a list of them.
- **Refresh Limits no longer spins forever while Turnscope can't read its
  data**, and is greyed out in the right-click menu while a newer
  Turnscope has written to the data.
- **Why a connect failed is no longer said once the agent's connection
  changes**, as when it's connected from the command line.
- **VoiceOver tells apart two accounts with the same title in the menu
  bar,** and no longer reads a dash on a row with no figure, whose line
  already says why, such as "Sign in again".

## 0.4.0

- **The macOS app is now native SwiftUI and AppKit**, under half the size
  (27 MB instead of 60 MB). The menu bar item opens on mouse-down, and
  right-clicking it opens a menu with Refresh Limits, Settings, Check for
  Updates and Quit.
- **Press ⌘R (or choose Refresh Limits from the menu bar)** to refresh
  your accounts' limits right away without waiting for them to go stale,
  while still respecting when a provider asks Turnscope to wait.
- **Fixed-window limit bars mark how much time is left**, so a bar shorter
  than its tick is running ahead of steady pace.
- **When an account is running out or used up, Turnscope names another
  live account with room.** The panel says so under its headline, and
  `check_limits` adds a final line naming the best alternate (preferring
  one your agent can use directly, or saying which agent uses it).
- **The panel works from the keyboard, and sessions under "Used most" can
  be copied or opened.** Up and Down move through rows and folds, and ⌘1,
  ⌘2, ⌘3 or Left and Right switch Settings tabs. Clicking a session copies
  its project path (or title), and right-clicking offers Open Project in
  Finder, Copy Project Path and Copy Session ID.
- **Hovering "Uses another Turnscope" in Settings → Agents** shows the
  path the agent is configured to run.
- **Right-clicking the version in Settings → General** (or Option-clicking
  it) copies a diagnostics report or reveals `app.log` in Finder.
- **Edge-case fixes:** older costs update when prices change, `app.log`
  keeps the previous 1 MiB in `app.log.1` instead of clearing it, Codex
  and Grok Build's connected status is read when `config.toml` has
  comments or single-quoted paths, and Antigravity installed in a path with
  spaces is still recognized when it calls the MCP server.

## 0.3.2

- **Antigravity's limits are read whether its app is open or not.**
  Antigravity 2.33.1 and later don't save the login they renew, so
  Turnscope now renews it with Google itself and keeps it in memory, as
  the app does. 0.3.1 asked the open app, which stopped working in 2.34.0.
- **Codex's limits update as Codex works**, offline too: Turnscope takes in
  the limits Codex writes with each response. A limit Codex doesn't write,
  such as one on some models, stays as last read.
- **An account not in use is read every 20 minutes**, not every 5, and so
  is one whose limits Codex writes as it works. Any other in use is still
  read every 5, and any account right after a limit resets.
- **Opening the panel reads limits more than 5 minutes old right away**,
  and those whose read failed. So does an agent writing a new login file. If that
  read finds no network, as right after the Mac wakes, the account stays
  as it was.
- **An account whose login can't be read**, as while the Keychain is
  locked, says it couldn't be read since its last read, and `app.log` says
  why. Before, it kept saying what it said last, such as to renew its
  login.
- **An expired login no longer asks you to open its agent**, which renews
  it when it runs. The account keeps showing its limits as last read, and
  says when that was.
- **Claude Code's login is also read from `.credentials.json`**, where
  Claude Code keeps it when it doesn't use the Keychain. The copy that
  expires later is used.
- **`guard` and the MCP server leave limits and prices to the app** while
  it runs, so a hook or a tool call doesn't wait on a provider.
- **A Google AI plan goes by the pool it last drew on.** While you work on
  Claude or GPT, the panel, the status line, `status --hours` and
  `check_limits` go by the Claude and GPT limits, and say when they're used
  up. Before, they went by Gemini's and said there was room. Gemini's
  limits are now named `(Gemini)`, and their work rate and what used them
  count Gemini usage alone. A row in the panel names a limit on some
  models by them alone, as "Claude and GPT reset Sat 12 PM" or "Opus
  resets Sun 12 AM", so it isn't cut short.
- **A model used through a provider that lists no price for it has a
  cost**, at its maker's list price, earlier usage included: Claude
  through Antigravity, or through a gateway set up in OpenCode.
- **The app shows an amount that rounds to no cents as `$0`**, not
  `$0.00`.
- **A session's title and project are read better**, in history read
  before too, which is read again once after updating: a title cut to
  length no longer ends in a space, a Pi session's title skips a blank
  first part, and a session's project is no longer lost to an empty
  folder its agent wrote first.

## 0.3.1

- **Resets and run-outs a week or more off show the date instead of a
  weekday.** Right after a weekly reset, the panel and notifications named
  today's weekday for next week's reset, and a monthly run-out a week or
  more ahead read in the headline as this week's weekday. In
  `status --json`, such a moment's `horizon` is `later`, not `thisWeek`.
- **`explain_usage` by day, week or month** puts the row adding up earlier
  periods first and labels it by the period (`1 earlier day`, `2 earlier
  weeks`). It put that row last and wrote `1 earlier days` or `2 earlier
  week ofs`.
- **An OpenCode Go plan stays one account when one key is refused while
  another key works.** The new key made a second account instead of keeping
  the plan's history, and an answer with no windows cleared the account's
  limits instead of failing the read. A second account made this way joins
  the plan's again, which stays shown if only one of them was hidden.
- **A forecast after midnight names the right night when the clocks have
  just sprung forward.** In the hour after midnight on the Monday after, it
  said "Saturday night" for "Sunday night".
- **One request that crashes no longer stops the panel's others.** What
  used a limit most, hiding an account and saving settings went unanswered
  until Turnscope was started again; connecting an agent too. The crash is
  noted in `app.log`.
- **Antigravity's limits stay current while its app is open.** Antigravity
  2.33.1 stopped saving the login it renews, so the one Turnscope read
  expired: the limits stopped updating, and Turnscope said to open
  Antigravity while it was open. Turnscope now asks the open app, on your
  Mac, finding it and its token in the list of running programs.
- **An agent whose folder is gone is no longer signed in.** Turnscope
  stops asking it for logins and `doctor` stops reporting on it. An empty
  Claude Code login counts as signed out too, instead of asking you to sign
  in again.

## 0.3.0

The app is rebuilt in Rust, with the engine inside it. It looks and works
as before, but for what's below.

- **One process instead of two.** The app runs the engine itself, so it
  no longer starts `turnscope serve`.
- **Removed: `turnscope serve`.** A script that runs it now fails with
  exit code 2.
- **A new database, `turnscope.db`.** The first time 0.3.0 opens your
  data, it takes in everything in `turnscope.sqlite`: history, limits,
  accounts and which are hidden, alerts and settings. The old file is
  left as it was, and a later version deletes it. `serve.lock`,
  `serve.log` and `serve.sock`, which `turnscope serve` kept beside it,
  are removed, though a `serve` still running writes `serve.log` again
  as it stops. Then all of history is read again once, which can take a
  few minutes. A Turnscope 0.2 still running, such as an agent's MCP
  server started before the update, then writes nothing more and says
  why. One from 0.1 goes on writing to the old file, which nothing reads
  now: start its agent again.
- **Once a newer Turnscope has written to the data, the panel's footer says
  to update Turnscope,** in each copy of the app open: "Not up to date ·
  update Turnscope", or to start it again when it was updated while it ran.
  It said it was trying again, with why only on hover, and a second copy
  open beside the first went on for good, logging a failure every 5 seconds.
  A change asked for then says so at once, and Connect is off, as Hide is:
  it went back with no word why, for up to a minute.
- **Turnscope keeps a log:** `app.log` in the data directory, with why it
  couldn't keep the data current or read an account's limits. `turnscope
  doctor` shows what it logged in the last day.
- **An agent installed while the app runs** is watched from the next
  minute. Its sessions showed only each minute until the app started
  again.
- **An MCP tool call waits at most a second for another Turnscope reading
  history,** then answers from what was read before and says so. It waited
  out the whole read, minutes at the app's first launch. While all of
  history is read, at the first launch and after this update, it says its
  usage is only part of it.
- **`check_limits` with `hours` and several accounts gives a line for
  each,** named, so one call compares them. It judged the first account
  alone, and with no account known it left `hours` out.
- **When the calling agent's account isn't known,** `check_limits` and
  `explain_usage` say so and show every account. `explain_usage` failed.
- **`explain_usage` takes `until`, and `by` takes `day`, `week` and
  `month`.** When your session isn't among the rows shown, a line after
  the table gives its cost and share.
- **A `|` in a session's title stays in its cell** of `explain_usage`'s
  table. It ended the cell, so a title could add cells of its own.
- **The MCP server answers a message it can't take with an error and goes
  on:** a line that isn't UTF-8 ended it, and a batch or an id of `null`
  got no reply. It now opens its database at the first tool call, so the
  handshake works when the database can't be opened, and the call says
  why.
- **The MCP server accepts protocol versions `2025-03-26` and
  `2024-11-05`,** and both tools are marked `openWorldHint: true`, as
  each reads limits from providers.
- **`turnscope statusline` only reads.** It created the database, or took
  in the old one, when it ran before anything else had; it now prints
  nothing then.
- **`usage --limit --json` gives `restSharePercent`,** the share of the
  row adding up the rest. **`usage --top 0`** is refused.
- **Usage on no account, in no project, or of no model named** is labelled
  so: `no account`. It was `(none)`. In `explain_usage`, the lines after
  the table read `By project: /work/api: $0.47; /work/app: $0.03`.
- **When an agent's sign-in changes, its usage since moves to the new
  account in the same write that records the change.** Done after, it
  could be lost for good when a Turnscope quit between the two, and a
  `guard` hook waited out another Turnscope's read of history. The same
  holds for pricing a model models.dev lists for the first time.
- **Reading OpenCode's history leaves nothing in its folder.** With
  OpenCode closed, a read left an empty `opencode.db-wal` and
  `opencode.db-shm` beside its database, and read it again a minute later.
- **`check_limits` and `status --hours` count work past a limit's reset
  in the window after it.** 6 hours of work on a 5 hours resetting in 30
  minutes were said to fit; the window after runs out 4 hours into them.
- **A window that rolls, as OpenCode Go's 5 hours, means whether hours of
  work fit can't be told,** with what's left of it. They were said to fit,
  with a note, at 4% left. When such a limit, or one with too little work
  measured, decides the account, the verdict says what's left of it, not
  another limit's hours of work.
- **A limit is back as soon as a provider resets it early.** Turnscope
  waited for the old window's reset, days later, to say so, and kept
  showing it used up.
- **A key's own limit is counted by that key's usage alone** in what used
  it, and in its work rate. The usage of every key to the provider
  counted, so another agent's key showed among what used it.
- **A limit on some models is paced by work on those models,** as Weekly
  (Opus) by Opus. Work on every model counted, so after hours of Sonnet,
  an hour of Opus that used 8 points read as 2% an hour.
- **No shares of a limit are given while some of its usage has no known
  price,** and the answer says so. A model not yet priced took `<1%` and
  the priced rows its points, at each model's launch.
- **An OpenCode Go key swapped for a new one to the same plan stays one
  account,** with its history, alerts and whether it's hidden. The new key
  made a second account, and the first showed as signed out.
- **`turnscope doctor` before prices are read** says only that. It also
  listed each model as having no known price.

- **The panel works from the keyboard.** Tab reaches the rows, Up and
  Down move between them, and Return or Space opens one. Left and Right
  switch Settings' tabs. VoiceOver reads a row as one button, with its
  name, line and figure, and says whether it's open.
- **Increase Contrast strengthens the grays, the panel's edge, an open row
  and a bar's track,** as well as amber and red.
- **A closed row darkens while pressed. A status that comes while the
  panel is open moves into place:** figures fade to their new values, and
  a row moving between the accounts in use and "Used this week" slides
  there.
- **A click just beside or below the open panel closes it, and goes no
  further.** It went through to the window under it. A click anywhere else
  closes the panel and reaches what it was on, as before.
- **An account's label, such as an email, sits on its title's line,** and
  reads a little darker. A Settings tab that scrolls fades at its edge.
- **An expired login says to open its agent to renew it,** in the app and
  on the command line. The offer to connect an agent reads as a row, its
  name over what's wrong, and its button says Reconnect for an agent that
  uses another copy of Turnscope. A refused login says to open its agent
  to sign in again everywhere too.
- **Claude's "Other apps weekly" counts only what other apps used.** Used
  up, it decided the account, held back the other limits' "back"
  notifications, and listed Claude Code's sessions as what used it.
- **"Sign in again" waits for a second refused read in a row,** 5 minutes
  after the first. A login its agent was renewing mid-read asked you to
  sign in again for nothing.
- **A provider that keeps failing is asked less often:** after 5, 10, 20,
  then every 30 minutes, or when its `Retry-After` says, up to 30. One
  that answers is asked every 5 minutes again. One that doesn't answer at
  all, as while the Mac is offline, is still asked every 5 minutes, so
  limits are back within 5 minutes of the network.
- **Prices are asked for again 2 minutes later** when Turnscope quits
  while models.dev is asked, as when the app is quit in its first minute.
  Every cost was unknown for 6 hours.
- **An account's API keys are read at once, in 15 seconds all told,** and
  OpenRouter's credits once. A key that can't be read shows the account as
  not read, with the other keys' limits; it was left out without a word.
- **A Claude plan whose answer has a window Turnscope can't read** is read
  without it. The whole plan went unread.
- **A Grok Build session is counted right when its subagent works across
  several of its turns,** and a subagent stopped before it finished is no
  longer counted twice. A subagent's history that can't be read is tried
  again, not counted twice.
- **Codex usage missing a count shows as a skipped line in `turnscope
  doctor`,** not as a response with no tokens. A response a subagent's
  history repeats from its parent is counted for the parent.
- **A response with no tokens and no cost isn't counted,** for every
  agent.
- **An OpenCode session titled `New session - <time>` keeps that title.**
  Turnscope titled such sessions, written by older OpenCode versions, by
  their first message.
- **A session from a git worktree since removed keeps its repository as
  its project** when its history is read again.
- **`accounts --json` gives `label` apart from `title`,** as `status
  --json` does: `title` no longer ends with the label.
- **An API key's forecast for the month has two figures:** "on pace for
  about $62". When what's spent already reads the same, it's left out.
- **`turnscope guard`, `connect` and `disconnect` never create the database
  or take in the old one.** Until something else has, `guard` blocks
  nothing, as its standing isn't known. When reading the limits again fails,
  `guard` guards by those read before: it failed with exit code 1.
- **Credits used up or running out are said as plural** after a limit
  that lasts: "Credits are used up". The panel's countdowns round to the
  nearest minute, as the command line's do.

## 0.2.1

- **When Turnscope's engine stops or can't start, the app says why:**
  `Turnscope's engine stopped: a newer Turnscope has updated this data;
  update Turnscope`. It said only that the engine stopped.
- **The engine stops once a newer Turnscope has written to its data,** so
  the app says to update. It went on, and the app showed what it had read
  before as current.
- **A request the engine doesn't answer within 30 seconds,** such as the
  sessions shown under an open account, marks what's shown as out of date
  and connects again: `Turnscope's engine didn't answer`. The app waited
  forever, showing old figures as current.
- **The engine has up to 30 seconds to start, not 5,** as it now opens its
  database before the app can connect, and a new version may update the
  database first.
- **The engine keeps a log:** `serve.log` in its data directory, with why
  it couldn't start or stopped, and each error it meets keeping the data
  current.
- **When the app can't read what its engine sent,** it says so in one
  sentence, and the details go to the unified log. It showed Swift's
  error.
- **Two versions of the app at once:** the newer one connects. From this
  version on, the older one says `Turnscope 0.2.2 is also running; update
  this one to use it`, and no longer shows the other version's figures,
  which it could misread.
- **An older Turnscope still running writes nothing once a newer one has
  written,** as an agent's Turnscope MCP server started before an update.
  It read limits and prices its way among the newer one's, and could hide
  or show an account. Until the agent starts it again, it answers from
  what's there and says `Turnscope was updated after this one started;
  start it again`. An MCP server of 0.2.0 left running through this update
  says `a newer Turnscope has updated this data; update Turnscope`
  instead: a new session in its agent starts the new one. A guard hook
  still guards, by the limits read before.

## 0.2.0

Turnscope is now about limits and usage alone. It no longer finds, reads or
hands off sessions.

- **Removed: `turnscope sessions`,** with `sessions show` and
  `sessions read`, **and `turnscope handoff`.** A script that runs one now
  fails with exit code 2.
- **Removed: the MCP tools `find_sessions`, `handoff` and `read_session`,
  and the `continue` prompt.** The server has two tools, `check_limits`
  and `explain_usage`. An agent that calls a removed tool is told it's
  unknown. Start a new session in each connected agent to get the new
  list.
- **What was said in a session is no longer kept.** The search index is
  deleted from Turnscope's database the first time this version opens it,
  which takes a few seconds. The file keeps its size, and the space is
  used again as history is read. Of what was said, Turnscope keeps only
  the title: the one you or the agent gave it, or else the first thing you
  asked. It no longer reads Grok Build's `chat_history.jsonl`.
- **Less is kept of each session:** its agent, project and title, and the
  session it's a subagent of. Its folder, its branch, when it started,
  when it was last active and the session it was forked or continued from
  are deleted in that first open.
- **A title is kept to its first 200 characters,** and longer ones stored
  before are cut in that first open. A title is often your whole first
  request. No list shows more than 80 characters of one;
  `usage --by session --json` gave it whole in `name`, and now gives 200.
- **`explain_usage` shows a session by the first 8 characters of its id,**
  as `usage --by session` does: `` `7c1d2e3f` "Add retries" ``. It showed
  the whole id, about 45 characters a row.
- **`explain_usage` marks a session `(yours)` only where the agent names
  its session,** which Claude Code does. For Codex, OpenCode, Pi, Grok
  Build and Antigravity it was a guess, the agent's latest session in the
  folder, and that guess is gone. Each still gets its own account.
- **A key in a session's title is no longer replaced with `[redacted]`.**
  The title is kept as written, and `usage --by session`, the app and
  `explain_usage` show it so. A title stored before keeps its `[redacted]`
  until a title for its session is read again.
- **A Turnscope left running through an update,** such as an agent's MCP
  server, answers from what it read before until it's restarted. One from
  0.1.8 goes on until a read fails: it can't read new history into this
  version's database, and its `explain_usage` fails when it lists sessions,
  as it does unless given another `by`. From this version on, once a newer
  one changes the database, one left running reads no more history, limits
  or prices, and says a newer Turnscope has updated the data.
- **`status --hours` and `check_limits` name the limit that runs out
  first.** They named the first limit in key order whose work would run
  out. With 5 hours at 1h 15m of work left and Weekly at 40m,
  `check_limits` said 5 hours runs out after about 1h 20m, beside a
  verdict naming Weekly. The verdict now also counts a limit that reset
  since it was read, with a new window's work left, where it could name a
  later limit or say there was room to keep going.
- **`status --hours` counts a limit that reset since it was read only to
  the end of its new window.** At 10% an hour, 6 hours of work left about
  40% of 5 hours, it said, where the new window ends with 50%.
- **Settings → Agents, with no agent installed,** says "No agent Turnscope
  reads is installed." It said "serves".
- **Removed: the green dot for a session active now** in the app's list
  of what used a limit most.
- **`serve` speaks protocol 3.** A session in a `limit.breakdown` answer
  no longer has `running`, and an alert no longer has `at`, which no
  client read. A client of protocol 2 is refused.

## 0.1.8

- **A key that starts a line a diff removes,** as after deleting a file
  that holds a token, is redacted, also with the line's number before it,
  as Claude Code's `Read` shows a patch. The `-` before the key counted as
  part of a word, so reading a session, search and handoffs showed the
  key. A private key on such a line was already redacted.
- **More keys are redacted:** Stripe's (`sk_live_`, `sk_test_` and
  `rk_live_`), Slack's (`xoxb-` and `xoxp-`), Hugging Face's (`hf_`) and
  Google's access tokens (`ya29.`). Reading a session, search and handoffs
  showed one unless it followed `Bearer `.
- **A handoff** says when its session was last active, also for one active
  in the last 5 minutes: `last active 11:57 (3m ago)`. It said `running
  now`, so the handoff of a session that had just stopped, as when its
  limit ran out, told the next agent that another agent was at work in the
  folder.
- **What failed**, in a handoff, names a call by the commands it ran, such
  as `"cargo test --locked"`, and a command of several lines by its first:
  `"cargo test …"`. It named it by the description the agent gave them,
  such as `"Run tests"`, and the next agent had to read the entry to learn
  what was run. A call that ran no command is still named by what it was
  for.
- **A fork's handoff** names the session it was forked from, which holds
  what came before the fork, or says that can't be read once that
  session's files are gone. Claude Code's, Codex's, OpenCode's and Pi's
  forks start where they branched off, so their handoffs began
  mid-conversation and didn't say where the start was.
- **On a first launch** the app shows your limits as soon as they're read,
  while the footer says it's reading your agents' history. It said
  "Getting your limits" and showed no account until all of the history was
  read, which can take minutes. Until then it can't tell which accounts
  are in use, so it shows every account as one in use, the most urgent
  first: in the headline, the menu bar and the panel's rows.
- **Settings → Agents and Accounts** say what the panel says before the
  first status arrives: "Reading your agents' history…", or why Turnscope
  can't read its data. With no account yet, Accounts says what the panel's
  headline does, such as "Getting your limits". Agents said no agent
  Turnscope serves is installed, and Accounts was empty.
- **Why the app's engine couldn't start** is said in the panel, as when a
  newer Turnscope has updated the data: "turnscope: a newer Turnscope has
  updated this data; update Turnscope". The panel said only that the engine
  didn't start.
- **Switches in Settings** that change something through the app's engine
  are grayed out while it's away: the notification switches, and those
  that connect an agent or show an account. A change made then moved the
  switch and moved it back, and for an agent said the engine isn't
  running.
- **Reduce Motion:** with it on, no spinner turns. The ones shown before
  the first status and while the panel connects an agent kept turning.
  They're now the footer's small arc.
- **VoiceOver** reads the figure on an account's row and on a limit's line
  as what's left: "87% left". It read the figure alone.
- **A command that reads all of your agents' history** says so first, on
  standard error: `Reading your agents' history…`. That's the first
  command you run, and the first after an update that changes how history
  is read. A command that waits for another Turnscope reading history,
  such as the app's, says so after a second:
  `Waiting while another Turnscope reads your agents' history…`. Both
  waited in a blank terminal.
- **`serve`** no longer takes `--linger`. It exits 60 seconds after its
  last client leaves, as it did without the flag. The app never passed it.
- **A request that names a tag** its agent uses is read whole, such as
  `<skill>` or `<image>` with Codex and `<system-reminder>` with Claude
  Code. It was cut at the tag: `Add a <skill> element to the manifest
  schema and document it.` became `Add a` in the session's title, in
  search, in `sessions read` and `read_session`, and in the requests a
  handoff quotes.
- **Search** quotes the message that holds a word with punctuation in it,
  such as `csv::quoted`, or a word said with accents and searched for
  without, as `cafe` finds `café`. The message quoted is found by the same
  rules as the session. A session found by such a word was listed with no
  message under it.
- **A Pi fork** shows only its own conversation in `sessions read`,
  `read_session` and handoffs. They opened with the fork's copy of its
  parent's entries, and a handoff listed the parent's requests as the
  fork's.
- **OpenCode:** a session or message saved while the Mac's clock was ahead
  no longer stops later ones being read. Until the clock passed the time it
  was stamped, no new OpenCode session or usage was read, and what was
  saved in that time stayed unread afterwards. `doctor` said nothing.
- **History whose files were all replaced at once**, as when a home folder
  is restored from a backup or moved to a new Mac, is read again as fast as
  it was first read. With 3,000 sessions, the first command after took 3 to
  4 minutes and takes 13 seconds.
- **`doctor`** says why a Keychain item couldn't be read, in the words of
  macOS's `security` and with its exit status, such as `User interaction is
  not allowed. (exit status: 36)` for a locked Keychain. It said `security
  exited with Some(36)`.
- **A session whose files are gone** says so one way, whichever of them is
  left: `codex:019a…'s files are gone: Codex deleted or moved them, so its
  conversation can't be read`. A named Codex thread whose rollouts were
  deleted said `the thread's rollouts are gone`, naming no session.
- **A folder** names its sessions with `..`, `.` or a slash at its end:
  for `sessions --folder`, `handoff --folder`, and the `folder` of
  `find_sessions` and `handoff`. `..` was kept as written, so `sessions
  --folder ..` looked for `/work/app/..` and found no session. With a
  slash at its end, `--folder` left out the sessions of the folder's
  project run outside it, as in a worktree kept beside it. The folder is
  folded as it's written, without asking the disk, so a folder that's gone
  still names its sessions, and a link isn't followed.
- **The calling agent's account** is found when `CLAUDE_CONFIG_DIR`,
  `CODEX_HOME` or `GROK_HOME` is written with a slash at its end or with
  `..`. None was found: `check_limits` listed every account, `guard`
  blocked nothing, and `statusline` showed the first account in use.
- **`statusline`** shows the calling agent's own account for a session
  Turnscope hasn't read yet, as a new one, and the first account in use
  with no calling agent. It showed the account of the latest session in
  the folder, of any agent: after Codex ran last in a folder, a new Claude
  Code session's status line showed the Codex account's limits until the
  session was read.
- **What used a limit** whose window reset since it was read counts from
  the reset, with no shares: in `usage --limit`, in `explain_usage`, and in
  the sessions the app lists under "Used most". They listed the sessions of
  the window before, each at `<1%` of a limit shown as 100% left. It showed
  for an account not read since a window ended, as one whose login expired
  overnight, or a Google AI plan an hour after Antigravity was last open.
- **`explain_usage`** shows usage with no known price as `(+N unpriced)` in
  its lines by model, project and account, as its table does. They showed
  its cost as `$0.00` alone.
- **`usage --limit`** gives the row that adds up the rest its share of the
  limit, as `explain_usage` does. It left that row's share out. With
  `--json`, `sharePercents` is `null` when rows get no share, where it held
  a `null` for each row.
- **`usage --agent`** refuses an id that names no agent and lists the ids,
  as `sessions --agent` does. It said nothing was used.
- **`usage --limit --since`** refuses a time at or after now, as `usage`
  and `explain_usage` do. It printed a span that ran backwards, with
  nothing used.
- **An account with more than one limit used up** is back when the last of
  them is. With 5 hours back in an hour and Weekly back in three days, the
  app and `check_limits` say it's back in three days, and `--hours` says
  Weekly is what's used up. They went by the first limit listed, so the
  account read as back in an hour. A used-up limit with no reset counts as
  the last back. Under the panel's headline, the app names another limit
  that's used up before one running out, and of several used up the last
  back. It named the first listed that doesn't last, so with Claude and
  GPT's weekly limit used up it could say their 5 hours runs out in 2h.
- **Under ten minutes of work left** reads `Under 10m of work left`. It
  read `About under 10m of work left (under 10m–under 10m)`. The range is
  left out when both its ends read the same, and when it's `under 10m or
  more`, which is any amount.
- **A limit whose use was given back**, as by an early reset, with none
  used since, says it lasts with what it has now: `Lasts, with about 60%
  left at the reset.` It said `No use shows in its reading yet.` beside 60%
  left. Only a limit with nothing used says that.
- **Credits used up** raise a notification, "Credits are used up", and
  another once more were bought and used up too. When more are bought,
  "Credits are back" says how much is left. So does a key's own limit that
  never resets. A limit that never resets raised no notification.
  `check_limits` and `status --hours` say "Credits are used up" and
  "Credits have $45.00 of $50.00 left" too, where they said "is" and "has".
- **A second account's folder that's gone**, such as `~/.codex-work` once
  deleted, is signed out of, and so is one without the file its agent
  writes once signed in. Its account stayed signed in through that agent,
  and read as one that couldn't be read.
- **A key kept under two providers** is read as each provider's own
  account. It took the account it was last signed in to under either, so
  one provider's plan could be read as the other's account.
- **OpenRouter's two requests** share the 15 seconds a provider's limits
  are waited for, as Google's do. Each got 15, so `guard` could hold an
  agent's hook for 30.
- **`status` and `accounts`** say why when there's no account to show, as
  `check_limits` does: no account's limits have been read yet, or every
  account is hidden, with how to see them. They printed nothing, and
  `check_limits` said no account had been read when every one was hidden.
- **`turnscope config`** is removed. It showed the four notification
  settings and turned one on or off. Only the app sends notifications, and
  its Settings → General has a switch for each.
- **`--limit` and `explain_usage`'s `limit`** take a model's limit with its
  model first, as the app names it: `"opus weekly"` as well as
  `"weekly (opus)"`. Named model first, no limit was found.
- **An API key that has spent nothing this month** says `This month,
  across its keys: $0.00.` After the month's first day it added `on pace
  for about $0.00`. The pace is left out whenever it reads the same as
  what was spent.
- **`connect` and `disconnect` with `--home`** run the agent's command
  with `HOME` set to that folder, so that it changes the settings there.
  The command ran in your own home: with `--home` at a copy of a home,
  Turnscope read the copy, found no server there, and had the agent add
  one to your own settings. The command an error says to run by hand
  names that home too.
- **Connecting or disconnecting an agent** is done once the agent's
  command ends. When the command, or your login shell's startup files,
  left a process running, Turnscope went on waiting: 10 seconds for the
  shell, and 60 for each command it ran.
- Your agents' history is read again once after updating, so these fixes
  apply to what was read before.

## 0.1.7

- **The app's countdowns** count from the minute it is, as the command
  line's do. They read a minute short: 40 seconds into 12:00, a reset at
  14:00 was in 1h 59m, and a limit's last minute before it ran out read
  0m.
- **Clicking a notification** while the app is quit opens the panel under
  the menu bar item, and the notification's account once the first status
  arrives. The panel opened in the bottom-left corner of the screen, with
  no account open, since the item had no place in the menu bar yet and no
  account was known.
- **A notification asking to sign in again** is removed as soon as the
  account can be read, also one left from before the app started. It could
  stay until the status next changed. One the app couldn't post at first,
  as while notifications were off, is no longer posted later for an
  account that can be read by then.
- **`explain_usage`** marks the caller's own session `(yours)` for every
  agent, as `find_sessions` does. It marked it only for Claude Code, the
  one agent that names its session. A session with no title yet shows as
  its id and `(untitled)`, where it showed its id alone.
- **Times** are told as of the minute in every answer. `explain_usage`
  told them from the second: 40 seconds into 12:00, a reset at 14:00 was in
  1h 59m, where `check_limits` said in 2h. And a time gone by was counted
  from its second everywhere: at 12:00:40, a reset at 11:00:40 read 11:00
  (59m ago), an hour by the clock.
- **`explain_usage`** refuses a `since` at or after now, as the command
  line's `usage` does. It said there was no usage since then, and a time
  ahead read as gone by: `since Fri 00:00 (12h ago)` on Thursday at noon.
- **A time with an offset**, such as `2026-10-01T09:00:00-05:00`, is read
  at that time by `--since`, `--until` and the MCP tools' `since`. It was
  read as midnight of its date, as was a time with no offset, which is now
  refused, since it could be any time zone's.
- **An agent's own session**, for an agent other than Claude Code, is its
  latest run in the folder it works in, then its latest in the folder's
  project. With two of its sessions in one project, as in two worktrees, it
  took the later for its own, wherever it ran: `find_sessions`,
  `explain_usage` and `sessions` marked that one, and `handoff` with no
  session left it out and handed the caller its own.
- **Finding sessions** in one folder or of one agent is faster: among
  3,000 sessions, `sessions --folder` took 0.5 s and takes 0.06 s. So are
  `find_sessions` and `handoff` given a folder, and `explain_usage` by
  session for an agent other than Claude Code.

## 0.1.6

- **Antigravity** is a supported agent. Turnscope reads the conversations
  of its app and of its command line, `agy`, with their usage.
  `turnscope connect antigravity` adds the MCP server to both. It runs
  `agy` to do it, so the command line has to be installed. In the app,
  Turnscope knows that Antigravity is asking but not which folder it works
  in, since the app starts the server outside any: there `handoff` with no
  `session` asks for `folder`, and the `continue` prompt tells the agent to
  give its own.
- **Google AI plans** are read, with the sign-in Antigravity keeps in the
  Keychain: a Google AI Pro or Ultra plan's 5-hour and weekly limits on
  Gemini models, and its limits on Claude and GPT models. Antigravity's
  sign-in lasts an hour and Turnscope never renews it, so an hour after
  Antigravity was last used the account says to open it to update. Claude
  and GPT models used through Antigravity have no price yet: their usage
  shows as unpriced, with no share of a limit.
- **A Gemini API key** that OpenCode or Pi holds is an account too, titled
  Gemini API key. Its spend counts from the update on: what the key spent
  before stays under no account.
- **`usage --limit` and `explain_usage`** list, for a limit on several
  models, such as Weekly (Claude and GPT), the responses of any of them.
- **A limit** is named by part of its name as shown, with its models:
  `"weekly (claude"` names Weekly (Claude and GPT). A name that matches no
  limit lists the limits as shown, so two aren't both "Weekly".
- **`handoff` and `find_sessions`**, from an MCP server that an agent
  started in `/`, as an app does, no longer take `/` for the folder the
  agent works in. `handoff` with no `session` asks for `folder`, where it
  took the latest session of any project. A relative `folder` asks for its
  full path, where it was read under `/`. The caller's own session is its
  agent's latest anywhere.
- **`handoff`** leaves out the caller's own session, the one in the folder
  it works in, also when it's asked for another folder. It left out its
  agent's latest session in the folder asked for, which could be the one
  wanted.
- **Handoffs** read a tool's `AbsolutePath`, `Path` and `TargetFile` as a
  file, its `CommandLine` as a command and its `toolSummary` as what the
  call was for, as Antigravity's tools name them, in any agent's session.
- **The notification that a limit is back** waits while another limit on
  the same models is used up, as it does for one on the whole account. A
  plan's 5-hour limit on a model was said to be back while its weekly limit
  on that model was used up.
- **A login with under a minute left** isn't sent to its provider. One that
  expired on the way was refused, and the account said to sign in again
  where the login only needed renewing.
- **`serve`** starts no catch-up when only a SQLite database's `-shm` file
  was written, which Turnscope writes itself by opening the database. A
  database it couldn't read, with a log left beside it, had it read
  history again every 5 seconds.

## 0.1.5

- **The app** shows OpenCode's logo for OpenCode and OpenCode Go. OpenCode
  had OpenCode Zen's logo, and OpenCode Go had its own.
- **Hide and Show** name the account with its label when its row shows
  one, for VoiceOver, in the Hide button's help and in the right-click
  menu. Two accounts with the same title were both "Hide Claude Max".
- **The app** says it's reading your agents' history until the first read
  is done, also when it connects while that read runs. It stopped saying so
  once the read had begun.
- **`serve`** speaks protocol 2: the status's `historyReadAt` time is now
  `historyRead`, true once the first read of the agents' history is done.
  A client of protocol 1 is refused, as the protocol says.
- **The notification that a limit is back** comes for one OpenRouter key's
  limit while another key's limit is used up. A key's limit stops only that
  key, as the status line already took it, but the notification was dropped
  as if the whole account were still stopped.
- **`status`, `check_limits` and `doctor`** say why an account can't be
  read in the same words. `check_limits`' verdict on the first account
  says how to fix it, as the others do, not only that its limits can't be
  read. `status` writes $1,000 or more with a comma, as `statusline` does.
- **`status --account`** shows a hidden account it names, as
  `check_limits` does. It left it out unless `--all` was given too.
- **`check_limits`' verdict, `statusline` and `usage --limit`'s first
  line** count time from the minute the status is as of, as `status`
  does. Late in a minute, `check_limits` could say a limit resets "14:00
  (in 1h 59m)" above a line that says "14:00 (in 2h)", and `statusline`
  could say "out in 59m" while `status` said "13:00 (in 1h)".
- **`sessions --folder` and `handoff --folder`** take a relative folder,
  such as `.`, from the current folder. They found no sessions.
- **`sessions`**, run by any agent, ends the agent's own session with
  `· yours`, as `find_sessions` does. It marked only Claude Code's.
- **`sessions` and `sessions read`** refuse a `--count` outside 1 to 100,
  with exit code 2, instead of using the nearest count. With `--count 0`,
  `sessions read` showed one entry with its tool output cut at 2,000
  characters, not at 40,000 as with `--count 1`. `read_session` now takes a
  `count` of 0 as 1.
- **A Codex script** is named by the tools it calls, in `sessions read`,
  `read_session` and the handoff, not by every tool its text names. One
  that ran `rg tools.apply_patch src` was named `exec: exec_command,
  apply_patch`, as if it changed files.
- **Handoff:** a read by OpenCode's `read` or Claude Code's `NotebookRead`
  is named by its file, not by its input as JSON. So is a failed OpenCode
  `edit` or `write`, or Claude Code's `NotebookEdit`, under "What failed",
  and it no longer shows there once a later one of the same file passed,
  as a failed Claude Code `Edit` already didn't.
- **Handoff:** a command that only reads is a read though it names one of
  Codex's tools, as `rg tools.apply_patch src` does. It showed as work,
  with its input and output under "Where it stopped", and under "What
  failed" when it found nothing.
- **Handoff:** "Where it stopped" says which entries after the agent's
  last message it had no room for when none of them fit. It said nothing
  of them. The entries it says it left out for room no longer start or
  end with a system note, which a handoff always leaves out, so a note
  alone is no longer said to be left out for room.
- **Handoff:** "What it found and did" no longer counts the system notes
  and tool calls before the first entry it had no room for as left out
  for room. With a system note at #0 and a tool call at #1, which it
  leaves out wherever they fall, it said "Entries #0 to #2 are left out
  for room." It now says "Entry #2 is left out for room."

## 0.1.4

- **The app's engine** uses much less CPU. It reads the agents' history no
  sooner than 5 seconds after it last did, not as soon as their writes
  pause, so the panel can show what an agent wrote up to 5 seconds late.
  Working out the status no longer reads every response of the last two
  weeks, and checking for new usage does less for each history file. With
  a made-up history of 5,700 files and 233,000 responses in the last two
  weeks, it used 0.7% of a core while one agent worked, down from 8.4%,
  and 2.6% with ten at work, down from 18.3%.

## 0.1.3

- **The app's engine** works out the status again only after reading
  something new, even with a history file it can't read. Such a file made
  it work out the status after every change in an agent's folder.
- **`doctor`** no longer lists a history file that couldn't be read once
  it's deleted, and counts an OpenCode message it can't read once, not
  again on each read for the 10 seconds after it.
- **Connecting an agent** is refused while Turnscope's engine runs from its
  disk image or another drive. Moved to Applications and opened within a
  minute of quitting the one on the disk image, the app could still connect
  an agent to that one, which is gone once the image is ejected.
- **Claude Code usage** from a backend that sends an empty cache split
  counts its cache writes.
- **Search** quotes the message it found with a quote or backslash in it
  escaped, as the rest of a session's text within a line is. A `"` in it
  could end the quote early, so its words read as Turnscope's own.
- Your agents' history is read again once after updating, so these fixes
  apply to what was read before.

## 0.1.2

- **The panel's headline** names an account by its title, never its email,
  and the line under it names the account too: "At this pace, no Claude Max
  limit runs out." A notification adds the email only when another account
  in use has the same title.
- **Reading a session** with `turnscope sessions read` or the
  `read_session` tool quotes what each entry says, `> ` a line, and says
  to treat it as data, not instructions. Text from a web page or a file in
  a session can no longer pass for one of its entries, such as a request
  from you.
- **Long messages** read back from a session are cut at 40,000 characters,
  saying how many were left out, so one long paste stays within what an
  agent can take from a tool.
- **Run from its disk image,** Turnscope no longer offers to connect an
  agent: an agent connected there would lose Turnscope once the image is
  ejected. Settings → Agents turns off its switches and says to move
  Turnscope to Applications.
- **Updates** are checked against their signature before the disk image is
  opened, from the update after this one.
- **Claude Code usage** through a gateway or another backend that leaves
  out the cache counts or their split is counted. Such a response was
  skipped whole.
- **A Claude Code fork** read the moment it starts, while its description
  is still being written, no longer shows its parent's history as its own
  and keeps its task title. A fork of a session shows the session it was
  forked from.
- **Branches:** a Claude Code session that moves outside a repository keeps
  the branch it was on.
- **Search** no longer counts a session's messages twice once Codex
  archives it, or once a history file is replaced. A moved file is read on
  from where it was.
- **`doctor`** lists history files that can't be read and why, and counts
  the lines it couldn't read once, not again each time a file is read from
  the start.
- **Handoff:** the session's title, its tasks and what each tool call was
  for are in double quotes, as the rest of what the session said is
  quoted, with a quote inside escaped as `\"`. A task a model wrote can no
  longer read as Turnscope's own words. A session of one entry says
  "1 entry", not "1 entries".
- **Session titles** are in quotes wherever a line shows one: `turnscope
  sessions show` and `explain_usage` by session quote them now, as the
  list of sessions does. A session with no title shows as (untitled),
  without quotes.
- **What used a limit on one model,** such as Claude's Opus weekly, counts
  only that model's responses. Other models' usage was given shares of it.
  Its column reads "% of Weekly (Opus)".
- **Hours that fit** (`status --hours`, `check_limits`) leave out a window
  that rolls, as OpenCode Go's 5 hours, and say so, instead of saying too
  little work was measured. One hour reads "1 more hour of work fits", not
  "fit".
- **An API key** held by the same agent in two folders names that agent
  once.
- **`--since`** no longer takes a negative count, which meant a time ahead,
  and `usage` says when `--since` isn't before `--until`.
- **Limits** are read with a shorter wait. A request to a provider gives up
  after 15 seconds instead of 60, and any request gives up after 5 if it
  can't connect. On a network that drops what's sent, `guard` in an agent's
  hook could hold the agent for a minute or more.
- **"Sign in again"** is no longer said when a web page in front of a
  provider, such as a bot check, refuses the request. The account reads as
  one that couldn't be read, and it's tried again in 5 minutes.
- **OpenCode Go:** a limit whose reset time can't be read fails the read,
  instead of showing as a limit that never resets.
- **A key held by two agents**, such as one OpenRouter key in OpenCode and
  Pi, is sent once each read instead of twice.
- **Not up to date:** when the engine sends a status or alert the app can't
  read, or doesn't answer within 30 seconds, the panel says "Not up to date ·
  trying again", the menu bar item dims, and the app connects again. Before
  the first status, the panel says "Turnscope's engine didn't answer". It
  used to keep showing the last status as current, or "Reading your
  agents' history…" with no end.
- **Settings → General:** the notification switches are grayed out until
  the first status arrives. Before, clicking them did nothing.
- **Reading a long history,** as on a first run, no longer makes other
  commands fail with "database is locked". `config set`, `accounts hide`,
  reading limits and raising alerts used to wait up to 10 s for it and
  then fail. Now they wait a second or two at most.
- **Costs:** when models.dev first listed a model while another process
  was reading history, the responses that process wrote could stay
  without a cost for good. They're now priced.
- **Codex forks** that open with a copy of the parent thread's history no
  longer take that copy as their own. A fork's title is its own first
  request, search finds the parent's words only in the parent,
  `sessions read` and handoffs show only the fork's conversation, and the
  parent's usage isn't counted a second time.
- **A Codex thread rewound** to an earlier turn no longer shows the turns
  it went back past in `sessions read` and handoffs, and search no longer
  finds the thread by their words. A fork that points at its parent's
  history instead of copying it is titled by its own first request, not
  "(untitled)".
- **OpenCode:** a reply another OpenCode process saved late is no longer
  missed. A session OpenCode left with a placeholder title, such as
  "New session - 2026-07-30T18:45:03.662Z", is titled by its first request.
- **Handoff:** a failed command run again under a reworded description no
  longer shows under "What failed". A `grep -i` that found nothing is a
  read, not a failure; `sed -i.bak`, `find -delete` and `git branch -D` are
  work; and a command after `cd v2;` counts as its own. A request is left
  out as sent again only if the agent hadn't answered it.
- **Handoff:** a file a Codex script patched counts as changed when the
  tests the script ran next failed. It was left out, so a session whose
  edits all came in such scripts had no list of files at all.
- **`statusline` and `guard`** no longer read every agent's MCP settings.
  With a 5.8 MB `~/.claude.json`, each run takes a quarter of the work and
  8 MB of memory instead of 38 MB.
- **The app's engine** works out the status again only after reading
  something new, in a new minute, or after another Turnscope process wrote.
  While an agent writes files other than its history, it uses about a
  twentieth of the CPU it did.
- **Connecting or disconnecting an agent** no longer hangs when your
  shell's startup files leave a program running. It waits no longer than
  its time limit.
- **The app's engine** answers a line that isn't UTF-8 with a "not JSON"
  error instead of closing the connection, and keeps running when it runs
  out of file descriptors for a new connection.
- **Colors:** amber and red are darker in light mode, and red is lighter in
  dark mode, so text in them reads at 4.5:1 or more against the panel.
  Amber text in light mode was 2.4:1 on white. With Increase Contrast they
  reach 7:1.
- **Reduce Motion:** with it on, pages fade instead of sliding, a limit's
  bar doesn't grow into place, and the footer's spinner holds still.
- **VoiceOver** reads the menu bar item as it is now. A change only it
  would read, such as an API key's cost becoming partly unpriced, used to
  wait until the figures shown changed too.
- **Dates** are on the Gregorian calendar on a Mac set to another one, such
  as the Hebrew or Islamic calendar: "Oct 18", not "Heshvan 7".
- **With no forecast yet,** the line under the panel's headline says only
  what's left, such as "Claude Max has 99% left of 5 hours". The row below
  already says when it resets.
- **VoiceOver and notifications** spell out the day and month: "Back
  Saturday 9 AM", "October 18". The panel and the menu bar keep them short.
- **Private keys** are redacted in a JSON string, such as a service
  account's file, in a diff that removes one, and cut short with no END
  line, as `head` prints one, even encrypted or with spaces after its
  lines. Search and reading a session showed them.
- **Keys in a tool call's input,** such as a token at the start of a line
  in a file an agent wrote, or after a tab, are redacted. The input is kept
  as JSON, where the `\n` or `\t` before the token hid it, so reading a
  session and handoffs showed it.
- Your agents' history is read again once after updating, so these fixes
  apply to what was read before.

## 0.1.1

- **Settings** shows its settings in cards, with larger switches.
- **Agents** connect with a switch in Settings → Agents, instead of a
  Connect or Disconnect button. "Connecting…" shows beside the switch until
  it's done. An agent that uses another copy of Turnscope says so under its
  name, and turning its switch on points it at this copy.
- **Menu bar:** with no account in use, it shows Turnscope's logo instead
  of a gauge. Account logos are larger, and the figure beside them is the
  size of the menu bar's clock and sits on its baseline.
- **Panel:** account logos are larger, and the Settings and Quit buttons
  line up with the rows. An account that can't be read now says when it was
  last read, in one line: "Last read 4:50 AM. Open Grok Build to update."
  The command line says "open Grok Build to update" for an expired login
  too.

## 0.1.0

The first release.

- **Limits** of every account your coding agents sign in to: Claude,
  ChatGPT, SuperGrok and OpenCode Go plans, and OpenRouter keys. What's
  left, when each resets, and when it runs out at your pace.
- **Work left:** how many hours of agent work each limit has left, measured
  over the hours your agents were working rather than clock time.
- **Usage and cost** by day, week, month, model, project, agent, account or
  session, and what used each limit. Each response is priced at the list
  price when it ran.
- **Sessions** from Claude Code, Codex, OpenCode, Pi and Grok Build:
  searchable by text, folder, agent or time, readable a page at a time with
  secrets redacted, and handed off to another agent.
- **MCP server** for agents to check their limits, see what used them and
  pick up other sessions' work, plus `guard` and `statusline` commands for
  Claude Code's hooks and status line.
- **Menu bar app:** the accounts in use, most urgent first, and Settings to
  connect agents, hide accounts and choose notifications. It checks for
  updates.
- **Notifications** when a limit is about to run out, is used up or is back,
  and when an account needs signing in again.
