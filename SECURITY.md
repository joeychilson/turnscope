# Security

## Report a vulnerability

Report it privately on
[GitHub](https://github.com/joeychilson/turnscope/security/advisories/new),
not in a public issue or pull request.

Include:

- What someone could do with it, and what they need first, such as
  running code on the Mac, or writing a file Turnscope reads.
- How to reproduce it, with what `turnscope --version` prints and your
  macOS version.
- A proof of concept, if you have one.

Use made-up logins, keys and session text, never real ones.

## What's covered

- `turnscope`: the command line, the MCP server, and `serve`, the app's
  daemon, with its socket.
- The menu bar app.
- Updates: the appcast, the signature the app checks, and how it installs
  an update.
- Releases: how they're built, signed and notarized, and the workflows that
  do it.

A bug in a coding agent or in a provider's service belongs to that project.

## What to expect

One person maintains Turnscope, as time allows, so there's no set time for
an answer or a fix. A fix ships in a new release. Only the latest release
gets fixes.
