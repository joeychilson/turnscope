# Changelog

What changed in each release of Turnscope. The release workflow publishes a
version's section as its release notes (`scripts/notes.sh`), which is what an
update shows before it installs. A change people will notice adds a line
under Unreleased, which becomes the next version's section when it is
released.

## [Unreleased]

## [0.1.0]

The first release.

- A menu bar item that shows what is left of each account's limits, in gray
  while they will last, amber when one will run out before it resets, and red
  when one is used up.
- A panel that says whether you can keep going, with every account in use:
  each limit in a sentence, the sessions that used it most, and a tip drawn
  from how it was used. Accounts not in use fold into one line; any account
  can be hidden from its row.
- Reads the history of Claude Code, Codex, OpenCode, Pi and Grok Build, and
  the limits of Claude, ChatGPT, Grok and OpenCode Go subscriptions and of
  OpenRouter keys.
- Notifications when a limit will run out, is used up or is back, and when a
  sign-in expires; optionally each quarter of a week or month, and a recap on
  Monday.
- Settings in three tabs: General, Agents and Accounts. Agents connects
  Turnscope's MCP server to Claude Code, Codex, OpenCode or Grok in a click,
  and the panel suggests connecting an agent in use, in a line you can put
  away.
- An MCP server with six tools (`check_limits`, `explain_limit`,
  `find_sessions`, `get_session`, `read_session`, `get_usage`) and three
  prompts, so agents can pace themselves, find what used a limit, and pick up
  each other's work.
- A command line in the app bundle: `mcp`, `connect`, `watch`, `call`,
  `guard` for hooks, `doctor` and `catalog`.
- Updates with Sparkle, signed with Turnscope's key, from GitHub releases.

[0.1.0]: https://github.com/joeychilson/turnscope/releases/tag/v0.1.0
