# Changelog

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
