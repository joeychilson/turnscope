# Changelog

## 0.1.5

- **Hide and Show** name the account with its label when its row shows
  one, for VoiceOver, in the Hide button's help and in the right-click
  menu. Two accounts with the same title were both "Hide Claude Max".

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
