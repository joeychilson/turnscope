# Turnscope

[![Release](https://img.shields.io/github/v/release/joeychilson/turnscope?label=release)](https://github.com/joeychilson/turnscope/releases/latest)
[![Verify](https://img.shields.io/github/actions/workflow/status/joeychilson/turnscope/ci.yml?branch=main&label=verify)](https://github.com/joeychilson/turnscope/actions/workflows/ci.yml)
[![macOS 26+](https://img.shields.io/badge/macOS-26%2B-lightgrey)](#install)
[![MIT](https://img.shields.io/github/license/joeychilson/turnscope?label=license)](LICENSE)

Know whether your coding agents' limits will last, and what used them.

Turnscope is a macOS menu bar app that reads the history **Claude Code, Codex, OpenCode, Pi and Grok Build** already keep on your Mac, and the limits of every account they're signed into. A glance at the menu bar says whether you can keep going; a notification says when you can't. Over MCP, your agents can pace themselves against your limits, find out what used them, and pick up each other's work. No account, no setup, no telemetry.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset=".github/screenshots/panel-dark.png">
  <img width="340" alt="The menu bar panel: Claude Max runs out in two hours, an hour before its five hours reset; its week lasts with about 43% left; two other accounts folded into one line" src=".github/screenshots/panel-light.png">
</picture>

## Install

Requires macOS 26 or later, on Apple silicon or Intel.

Download the disk image from the [latest release](https://github.com/joeychilson/turnscope/releases/latest) and drag Turnscope to Applications. It is signed and notarized by Apple, so it opens like any other app.

Turnscope keeps itself up to date with [Sparkle](https://sparkle-project.org): it looks for a new release once a day, checks it is signed with Turnscope's key, and asks before installing it. **Check for updates automatically** in Settings › General turns the daily look off, and **Check for Updates…** looks now.

## What you get

- **Calm until it matters.** Everything is gray while your limits will last. Amber means a limit will run out before it resets at the pace you're going; red means it has.
- **Menu bar.** Each account an agent is using, by its logo and what is left: "62%". Once a limit will run out early, it says how long it has instead, "1h 40m", in amber; used up, when it's back, "Back 5:40 AM", in red. Settings › General chooses between every account in use and only the most urgent.
- **Panel.** Click the item for the one thing to know, large: whether you can keep going, or when you'll run out. Under it, each account in use: what is left of the limit that matters, how it stands in a few words, and its bar. Open an account for each of its limits in a sentence, the sessions that used it most, and one tip drawn from how it was used. Your other accounts fold into one line that names the one with most room.
- **Notifications.** When a limit will run out before it resets, when it's used up, when it's back, and when a sign-in you're using expires. If you like, also each quarter of a week or month used, and a recap of last week on Monday morning. Choose which in Settings, and send a test to see how they look.
- **More than one account.** Two Claude accounts, a ChatGPT account that Codex, Pi and OpenCode all use: each account counts on its own. Switch with `/login` and each session goes to the account signed in when it ran; a second account kept in its own folder, as `CLAUDE_CONFIG_DIR` or `CODEX_HOME` points to, is found by itself. Hide an account you don't want to see from its row, or in Settings › Accounts.
- **API keys.** Use of a provider's API with a key is an account of its own, "OpenRouter API key"; an OpenRouter key's own limit is read as it is.
- **Your agents can pace themselves, and pick up each other's work.** Turnscope is an MCP server for the agents you work with. **Settings › Agents** adds it to Claude Code, Codex, OpenCode or Grok in a click, with the agent's own command, and the panel suggests it, in a line you can put away, for an agent you use that isn't connected. See [For your agents](#for-your-agents).

## Supported agents

| Agent       | Reads                                                                               |
| ----------- | ----------------------------------------------------------------------------------- |
| Claude Code | `~/.claude/projects/`                                                               |
| Codex       | `~/.codex/sessions/`, `~/.codex/archived_sessions/`, `~/.codex/session_index.jsonl` |
| OpenCode    | `~/.local/share/opencode/opencode.db`                                               |
| Pi          | `~/.pi/agent/sessions/`                                                             |
| Grok Build  | `~/.grok/sessions/`                                                                 |

Limits are read for Claude, ChatGPT (Codex), Grok and OpenCode Go subscriptions, and for OpenRouter keys that carry one.

## Privacy

- **It only reads.** Agents' files and sign-ins are never changed or renewed. Connecting an agent, when you click Connect, runs that agent's own command for adding an MCP server.
- **Your data stays on your Mac.** Turnscope keeps usage, titles and a search index in `~/Library/Application Support/com.joeychilson.turnscope`, readable by you alone. Conversations stay in the agents' own files, read when an agent asks for one over MCP.
- **Three kinds of network destination.** Each subscription's usage endpoint, and OpenRouter's for the limit an OpenRouter key carries, with the sign-ins and keys your agents already have; [models.dev](https://models.dev) for prices; and GitHub, for Turnscope's own updates. The last two are sent nothing of yours. All go through macOS's `/usr/bin/curl`. There is no telemetry.
- **Connected agents see what you see.** An agent you add over MCP can read your sessions, and sends what it reads to its own model provider, as with anything else it reads.

## Good to know

- **Costs are estimates at list prices** from models.dev, not what your subscription charged. Where a model has no list price, Turnscope uses what the provider billed (Grok) or the agent's own estimate.
- **Unknown is never zero.** Usage with no known price shows as unknown, not $0.00.
- **Limits come from the providers' usage endpoints,** which aren't documented and may change.
- **To open it at login,** turn on **Open at login** in Settings.
- **Your history outlives the agents' files.** Agents delete old transcripts; Turnscope keeps their usage. To back it up, quit Turnscope and copy `ledger.sqlite` from its folder. `cache.sqlite` can always be deleted: it is rebuilt from the ledger.

## For your agents

An agent starts Turnscope's server itself, so the server knows who's asking: the agent that started it, the folder it works in (its latest session there is "this session"), and the account it draws on, even with a second account kept in its own folder (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`) or after a `/login` to another. An account you hide in Settings stays out of what agents are told unless they name it by its id. No ids to pass, nothing to set up. Six tools, each a question, answer with sentences an agent can act on and the exact figures beside them:

| Tool            | Asks                                      | Takes                                                                                          |
| --------------- | ----------------------------------------- | ---------------------------------------------------------------------------------------------- |
| `check_limits`  | How much is left, and will it last?       | `account`, `limit` (`"5 hours"`, `"week"`, `"opus week"`, `"month"`), `below`, `all`           |
| `explain_limit` | What used a limit, and why?               | `account`, `limit`, `by` (sessions, projects, models, agents), `session`                       |
| `find_sessions` | Which sessions?                           | `folder`, `words`, `agent`, `account`, `since`, `until`, `running`, `order`, `limit`, `cursor` |
| `get_session`   | What was it doing, and where did it stop? | nothing (this session), `latest_in` a folder (the latest other session there), or `session`    |
| `read_session`  | What exactly was said and done?           | `session`, `detail`, `find`, `offset`, `limit`, or `entry` to read one entry exactly           |
| `get_usage`     | How much, when, and on what?              | `since`, `until`, `folder`, `account`, `agent`, `model`, `by`                                  |

`check_limits` gives each limit's share left, when it resets, and when it runs out at the recent pace or how much is left at the reset. `explain_limit` gives each session's share of the window with what drove it: its model, responses, how large its context grew, how much was cache reads, and its subagents. `get_session` is a handoff: the first and last requests, the last reply, the plan and how far it got, the files changed with lines added and removed, the commands run and whether they failed, and how to resume it.

`since` and `until` take what an agent naturally writes: today, yesterday, a weekday, this or last week, month or year, a date or a month (as `since` it starts the stretch, as `until` it ends it), spans such as `30m`, `7d`, `3mo` or `past 24 hours`, or a local time. `account` can name several at once, as `claude` for every Claude account. Accounts are named as the app names them, as "OpenRouter API key". `check_limits` gives each limit and account its standing, lasts, running out or used up, and `below` says whether your own account is under the percent. `get_session` given nothing says what this session has used so far, and what it cost.

The server also offers three prompts, which Claude Code lists as slash commands: `/mcp__turnscope__catch-up` continues the latest session in this folder, from any agent; `/mcp__turnscope__pace` fits the task at hand to what's left of your limits; `/mcp__turnscope__what-used` explains what used your week, and what you could change.

To make a limit a rule rather than a request, a hook can ask Turnscope before a costly call. This Claude Code hook refuses to start a subagent while less than half the week is left, and tells the agent why:

```json
{ "hooks": { "PreToolUse": [{
  "matcher": "Task|Agent",
  "hooks": [{ "type": "command",
    "command": "turnscope guard --limit week --below 50 --say 'Under half the week is left. Do this yourself instead of starting a subagent.'" }]
}]}}
```

## Command line

The command line is in the app, at `/Applications/Turnscope.app/Contents/Helpers/turnscope`. To run it as `turnscope`, link it into a folder on your `PATH`, such as `sudo ln -s /Applications/Turnscope.app/Contents/Helpers/turnscope /usr/local/bin/turnscope`:

| Command                                                                              | What it does                                                                                        |
| ------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| `turnscope mcp`                                                                      | Serve the MCP tools and prompts on standard input and output                                        |
| `turnscope connect [<agent>]`                                                        | Add the MCP server to an agent with its own command, or with none, say which agents have it         |
| `turnscope watch`                                                                    | Keep history current and stream what the menu bar app shows, as JSON lines                          |
| `turnscope call <tool> ['<JSON>']`                                                   | Run one MCP tool and print its answer, as from a script                                             |
| `turnscope guard --limit <name> --below <percent> [--say <text>] [--account <name>]` | For a hook: exit 2, with why on standard error, when your account's limit is under the percent left |
| `turnscope doctor [--strict]`                                                        | Read every agent's history and report what was found and understood                                 |
| `turnscope catalog <api.json> <catalog.json>`                                        | Turn models.dev's `api.json` into the price catalog the app ships                                   |

`guard` exits 0 when the limit is at or above the percent, and also whenever how it stands isn't known, such as a stale reading, a limit no account has, or an account it can't tell: it never blocks a call for want of data. A command line it can't follow exits 1, which Claude Code shows you without blocking anything.

Every command but `catalog` and `connect` takes `--data <dir>` for where the ledger is kept and `--home <dir>` for whose history is read.

## Building

Requires Xcode 26; `rustup` installs the Rust toolchain `rust-toolchain.toml` names.

```sh
git clone https://github.com/joeychilson/turnscope.git
cd turnscope
scripts/build.sh                  # target/xcode/Build/Products/Debug/Turnscope.app
open target/xcode/Build/Products/Debug/Turnscope.app --args --data /tmp/turnscope-dev
```

Or open `macos/Turnscope.xcodeproj` and run it. The engine and command line are Rust, in `crates/`; the menu bar app is Swift, in `macos/`, and runs the command line as `turnscope watch` for what it shows (`contract/` holds what they say to each other). `scripts/release.sh` builds for Apple silicon and Intel with a disk image, as a release does.

## Contributing

Issues and pull requests are welcome. [CONTRIBUTING.md](CONTRIBUTING.md) says how to build and test a change, and [AGENTS.md](AGENTS.md), for people and coding agents alike, covers how the code is organized and the rules it follows. Report a security problem privately, as [SECURITY.md](SECURITY.md) says. What changed in each release is in [CHANGELOG.md](CHANGELOG.md).

## Credits

Prices and provider logos from [models.dev](https://github.com/anomalyco/models.dev) (MIT). Updates by [Sparkle](https://sparkle-project.org) (MIT). Agent and provider names and marks belong to their owners; Turnscope isn't affiliated with or endorsed by any of them.

## License

[MIT](LICENSE)
