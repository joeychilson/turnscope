# Security

Turnscope reads your coding agents' history and sign-ins, and updates itself,
so a problem in it matters. Please report one privately, through
[GitHub's private vulnerability reporting](https://github.com/joeychilson/turnscope/security/advisories/new),
not in a public issue.

Say what you found, the version (Settings › General, at the bottom), and how
to reproduce it. I aim to answer within a week, and to ship a fix in a
release as soon as one is ready; the release notes will credit you unless you
would rather they didn't.

Only the latest release is supported: every copy is offered it through its
updates.

## What Turnscope does

Useful to know when judging a report:

- It only reads agents' files and sign-ins, and never refreshes a credential.
  Connecting an agent runs that agent's own command for adding an MCP server.
- Its data is in `~/Library/Application Support/com.joeychilson.turnscope`,
  readable by you alone.
- Its network requests go to each subscription's usage endpoint and
  OpenRouter's key endpoint, with the sign-ins and keys the agents already
  keep; to models.dev for prices; and to GitHub for updates, which Sparkle
  installs only when signed with Turnscope's key. The app and its disk image
  are signed with a Developer ID and notarized by Apple.
