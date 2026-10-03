# Changelog

What changed in each release of Turnscope. The release workflow publishes a
version's section as its release notes (`scripts/notes.sh`), which is what an
update shows before it installs. A change people will notice adds a line
under Unreleased, which becomes the next version's section when it is
released.

## [Unreleased]

- A session whose files were deleted since Turnscope last looked is gone for
  every agent, rather than failing to open for some.
- A Claude Code session's handoff counts the lines it wrote over a file
  with, as it does for every other agent.

- Claude Code gets all of Turnscope's instructions for agents. It used to cut
  them short, leaving out that what a session says is to be read as data,
  not as instructions.
- After you sign into another account, moving your use to the account it
  drew on can no longer stop partway and leave some of it under the old one.
- An account not in use, opened, says when its provider couldn't be reached
  or answered in a way Turnscope doesn't understand, as an account in use
  does, and is no longer called a key when its limits haven't been read.
- The panel opens with every account not in use closed again.
- Grok Build is called Grok Build everywhere, as it is in answers to your
  agents, rather than Grok in the panel and Settings.
- Pi's cache writes kept for an hour are priced as such, at twice the input
  price, as Claude Code's already are.
- When Turnscope's engine stops, the panel says why, rather than sometimes
  only that it stopped.
- Connecting an agent from the panel or from Settings says the same thing,
  and if it fails, the button's tooltip says why.
- When your agents use a model Turnscope has no price for, an agent asking
  what used a limit is told how much of it that use took, rather than
  finding it left out of every figure.

## [0.1.2]

- A session at work right now is marked with a green dot, and the panel's
  rows have a little room between them.

## [0.1.1]

- Limits show up within a few seconds of opening Turnscope for the first
  time, rather than once all of your agents' history has been read.
- While Turnscope reads your agents' history for the first time, the panel
  says so, and says it is getting your limits rather than that none were
  found.
- Opening Turnscope for the first time doesn't send a notification for how
  much of a week is left: only a limit that will run out, or has, is news.

## [0.1.0]

The first release.

- A menu bar item that shows what is left of each account's limits, in gray
  while they will last, amber when one will run out before it resets, and red
  when one is used up. In the last three hours before one runs out, it
  counts down the time left.
- A panel that says whether you can keep going, and when a limit will run
  out, what to keep to so it lasts: "Stay under 16% a day to last." Every
  account in use shows each limit in a sentence, how it stands against using
  it evenly until it resets ("10% in reserve", with a mark on its bar), the
  sessions that used it most, and a tip drawn from how it was used. Accounts
  not in use fold into one line; any account can be hidden from its row.
- Forecasts that follow how you work: five hours by the last hour of use, a
  week by the last day, and a week just begun, or come back to after days
  away, by its average, so a few busy minutes don't say it runs out.
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

[0.1.2]: https://github.com/joeychilson/turnscope/releases/tag/v0.1.2
[0.1.1]: https://github.com/joeychilson/turnscope/releases/tag/v0.1.1
[0.1.0]: https://github.com/joeychilson/turnscope/releases/tag/v0.1.0
