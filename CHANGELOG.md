# Changelog

## 0.1.8

- **A key that starts a line a diff removes,** as after deleting a file
  that holds a token, is redacted. The `-` before the key counted as part
  of a word, so reading a session, search and handoffs showed the key. A
  private key on such a line was already redacted.
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
  as `"cargo test --locked"`. It named it by the description the agent gave
  them, such as `"Run tests"`, and the next agent had to read the entry to
  learn what was run. A call that ran no command is still named by what it
  was for.
- **A fork's handoff** names the session it was forked from, which holds
  what came before the fork. Claude Code's, Codex's and OpenCode's forks
  start where they branched off, so their handoffs began mid-conversation
  and didn't say where the start was.
- **On a first launch** the app shows your limits as soon as they're read,
  while the footer says it's reading your agents' history. It said
  "Getting your limits" and showed no account until all of the history was
  read, which can take minutes.
- **A command that reads all of your agents' history** says so first, on
  standard error: `Reading your agents' history…`. That's the first
  command you run, and the first after an update that changes how history
  is read. A command that waits for another Turnscope reading history,
  such as the app's, says so after a second:
  `Waiting while another Turnscope reads your agents' history…`. Both
  waited in a blank terminal.
- **A request that names a tag** its agent uses is read whole, such as
  `<skill>` or `<image>` with Codex and `<system-reminder>` with Claude
  Code. It was cut at the tag: `Add a <skill> element to the manifest
  schema and document it.` became `Add a` in the session's title, in
  search, in `sessions read` and `read_session`, and in the requests a
  handoff quotes.
- **A Pi fork** shows only its own conversation in `sessions read`,
  `read_session` and handoffs. They opened with the fork's copy of its
  parent's entries, and a handoff listed the parent's requests as the
  fork's.
- **OpenCode:** a session or message saved while the Mac's clock was ahead
  no longer stops later ones being read. Until the clock passed the time it
  was stamped, no new OpenCode session or usage was read, and what was
  saved in that time stayed unread afterwards. `doctor` said nothing.
- **`doctor`** says why a Keychain item couldn't be read, in the words of
  macOS's `security` and with its exit status, such as `User interaction is
  not allowed. (exit status: 36)` for a locked Keychain. It said `security
  exited with Some(36)`.
- **A session whose files are gone** says so one way, whichever of them is
  left: `codex:019a…'s files are gone: Codex deleted or moved them, so its
  conversation can't be read`. A named Codex thread whose rollouts were
  deleted said `the thread's rollouts are gone`, naming no session.
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
  the last back. Under the panel's headline, of two used-up limits on the
  same models, the app names the last back too.
- **Under ten minutes of work left** reads `Under 10m of work left`. It
  read `About under 10m of work left (under 10m–under 10m)`. The range is
  left out when both its ends read the same, and when it's `under 10m or
  more`, which is any amount.
- **A limit whose use was given back**, as by an early reset, with none
  used since, says it lasts with what it has now: `Lasts, with about 60%
  left at the reset.` It said `No use shows in its reading yet.` beside 60%
  left. Only a limit with nothing used says that.
- **`status` and `accounts`** say why when there's no account to show, as
  `check_limits` does: no account's limits have been read yet, or every
  account is hidden, with how to see them. They printed nothing, and
  `check_limits` said no account had been read when every one was hidden.
- **`--limit` and `explain_usage`'s `limit`** take a model's limit with its
  model first, as the app names it: `"opus weekly"` as well as
  `"weekly (opus)"`. Named model first, no limit was found.
- **An API key that has spent nothing this month** says `This month,
  across its keys: $0.00.` After the month's first day it added `on pace
  for about $0.00`. The pace is left out whenever it reads the same as
  what was spent.
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
