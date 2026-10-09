# Adding an agent

An agent is a coding tool whose history and logins Turnscope reads. Each
agent is one file in `src/agents/` and one line in `agents::ALL`. Only that
file knows the agent's formats. The app needs nothing but a logo: it takes
the agent's name from the status.

The steps use an agent called Acme, with id `acme`. Pi
(`src/agents/pi.rs`) is the shortest complete agent to read alongside.

| Step | File |
|---|---|
| [1. Measure the agent's files](#1-measure-the-agents-files) | The module comment of `src/agents/acme.rs` |
| [2. Describe it in `Info`](#2-describe-it-in-info) | `src/agents/acme.rs` |
| [3. Implement `Agent`](#3-implement-agent) | `src/agents/acme.rs` |
| [4. Register it](#4-register-it) | `src/agents/mod.rs` |
| [5. Test it](#5-test-it) | `src/agents/acme.rs`, `tests/snapshots/` |
| [6. Add its logo](#6-add-its-logo) | `macos/Logos/agents/acme.svg`, `macos/Logos/README.md` |
| [7. Update the docs](#7-update-the-docs) | `README.md`, `docs/cli.md`, `docs/mcp.md`, `docs/architecture.md`, `CHANGELOG.md` |

## 1. Measure the agent's files

Before writing code, look at real history: where the files are, what each
record holds, and how usage, sessions, subagents, forks and logins are
written. Read only what the agent writes today.

Write what you measured, with numbers and dates, in the module comment at
the top of `src/agents/acme.rs`, as the other agents do. The reader's rules
rest on it.

## 2. Describe it in `Info`

In `src/agents/acme.rs`:

```rust
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Entry, Info, Login, Read, Secret};
use crate::Result;

pub struct Acme;

static INFO: Info = Info {
    id: "acme",
    name: "Acme",
    command: "acme",
    resume: "acme --resume {id}",
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: Some("ACME_HOME"),
    charges: false,
    session_var: None,
    cwd_var: None,
};
```

| Field | Type | What it is |
|---|---|---|
| `id` | `&'static str` | The agent's id: as stored, as `--agent` takes it, as the logo's file name, and at the start of its session ids, `acme:<its own id>`. |
| `name` | `&'static str` | The agent's name as people know it. |
| `command` | `&'static str` | The program `connect` runs. The MCP server also finds the agent calling it by this name among its parent processes: `acme`, `acme-<anything>` for a versioned build, or a script of that name run by `node`, `bun`, `deno`, `python` or `python3`. |
| `resume` | `&'static str` | The command that resumes a session, `{id}` being the agent's own id. `sessions show` puts `cd <folder> &&` before it, and `folder_var` for a session in a second account's folder. |
| `mcp_add` | `&'static [&'static str]` | The arguments to `command` that add an MCP server. `connect` runs `<command> <mcp_add> <path to turnscope> mcp`. |
| `mcp_remove` | `Option<&'static [&'static str]>` | The arguments that remove it, if the agent has a command for that. `connect` runs them first, so a refused add can put back the server that was there. With `None`, `disconnect` tells the user to remove it by hand. |
| `folder_var` | `Option<&'static str>` | The environment variable that points the agent at a folder other than its own, such as `CODEX_HOME`. Needed when `folders` can find more than one. |
| `charges` | `bool` | The cost the agent records is what the provider charged, so it's used before list prices. `false` when it's the agent's estimate. |
| `session_var` | `Option<&'static str>` | The variable holding its session's id that it sets in an MCP server it starts. With it, the MCP server knows the calling session; without it, the calling session is taken to be the agent's latest in the folder. |
| `cwd_var` | `Option<&'static str>` | The variable holding the folder it works in, set the same way. |

## 3. Implement `Agent`

```rust
impl Agent for Acme {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".acme", ".acme-", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> { … }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> { … }

    fn transcript(&self, id: &str, files: &[PathBuf]) -> Result<Vec<Entry>> { … }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> { … }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::json_mcp_command(&folder.join("config.json"), "/mcpServers/turnscope/command")
    }
}
```

`Result` is `crate::Result`. Its errors are `Error::Failed` and
`Error::NotFound`, each with a message saying what went wrong.

| Method | Returns |
|---|---|
| `info` | `&INFO`. |
| `folders` | The folders the agent keeps history and logins in under `home`, its own first. The first is taken as its own: the agent is installed when it exists, and `resume` adds `folder_var` for the others. `own_and_found(home, own, prefix, signed)` gives `home/own`, then each folder beside it whose name starts with `prefix` and that holds the file `signed`, such as `~/.acme-work/auth.json`: how a second account's folder is kept. |
| `files` | Its history files in a folder. It runs at every catch-up, so it lists and doesn't read. A file that doesn't exist is passed over, so one the agent may keep can be listed without looking. |
| `read` | What a file holds past `cursor` (empty: from the start), and the cursor to read on from ([below](#reading-history)). An error leaves the cursor as it was, so the file is tried again at the next catch-up. |
| `transcript` | The conversation of the session `id`, the agent's own id without `acme:`, from the files that hold it ([below](#transcripts)). |
| `logins` | One `Login` per provider the agent signs in to ([below](#logins)). |
| `mcp_command` | The command that the configuration `folder` uses runs for the `turnscope` MCP server, if any. An agent with `folder_var` keeps a configuration per folder: `connect` runs its command once for each folder, pointed at it with `folder_var`, so read the file that command writes for that folder. `json_mcp_command(path, pointer)` reads it at a JSON pointer in a JSON file; `toml_mcp_command(path)` reads `command` under `[mcp_servers.turnscope]` in a TOML file. The status compares it with this binary to show the agent as connected, outdated or available. |

### Reading history

A reader keeps its own cursor as a string. The readers here keep a serde
struct as JSON. For a JSON Lines file, `lines(path, from, visit)` passes
each complete line from byte `from` to `visit`, and returns where the next
read starts: past the last line that ends with a newline, since one
without is still being written.

```rust
#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
}

fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
    let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Value>(bytes) else {
            read.skipped += 1;
            return;
        };
        // What `line` says: read.session(…), read.respond(…), read.say(…).
    })?;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}
```

An agent that keeps a SQLite database, as OpenCode does, is opened read-only
and read on from the latest `time_updated` seen (`src/agents/opencode.rs`).

What a read returns, in `Read`:

| Field or method | What it's for |
|---|---|
| `read.session(id)` | The `Session` for a session id, `acme:<its own id>`: its `cwd`, `branch` and `parent`. `saw(at)` notes a time it was active. `title(kind, text)` offers a title. |
| `read.respond(response)` | Adds a report of a response, merged with any other report of it by `Response::merge`. |
| `read.say(session, text)` | Notes what the user typed or a model replied, for search. It redacts secrets. |
| `read.skipped` | How many lines or rows couldn't be read. `doctor` names files with any. |
| `read.cursor` | Where the next read starts. |

A session's `parent` is a `Parent { id, link }`, `link` being
`Link::Subagent` (counted in its parent's usage and listed under it),
`Link::Fork` (the conversation branched off by the user) or
`Link::Continuation` (the same conversation carried on in a new session). A better kind of
title replaces a worse one, and a later one of the same kind an earlier
one. From worst to best, a `Title` is `Prompt` (the user's first real
request), `Task` (a subagent's task, as its parent described it),
`Generated` (the agent's) or `Named` (the user's).

A `Response`:

| Field | What it is |
|---|---|
| `id` | Unique within the agent, and the same in every report of the response. |
| `session` | The id of the session that made it. |
| `copy` | The report may be of another session's response, as a fork's copy of its parent's history. |
| `at` | When it was made, in Unix milliseconds. |
| `provider` | The provider as the agent names it. It's matched with the agent folder's sign-in to find the account, and with models.dev's provider ids for the price. |
| `model` | The model as the agent names it. |
| `tokens` | Its `Tokens`: `input` (neither read from the cache nor written to it), `cache_read`, `cache_write_5m` (kept five minutes, or a provider's only kind), `cache_write_1h`, `output` (reasoning included) and `reasoning`. |
| `prompt` | The largest prompt of the model calls it covers: input with cache reads and writes. It sets the price tier where long prompts cost more. |
| `web_searches` | How many web searches it made. |
| `priority` | It was served at a faster, dearer tier, as fast mode is. |
| `cost` | What the agent says it cost, if it says. |

Rules:

- **Tokens.** If the agent's input count includes cache reads and writes,
  subtract them. If its output count leaves reasoning out, add it in. Skip
  a response whose counts are all zero, such as one aborted before it
  wrote anything.
- **One response, many reports.** A response can be written several times:
  a line rewritten as it streams, a fork's copy of its parent's history, a
  continuation's. Give every report the same `id` and pass each to
  `read.respond`. Set `copy` on a report that may be another session's. A
  report that isn't a copy decides which session owns the response.
- **Text said.** Pass `read.say` only what the user typed and what the
  model replied, not what the agent adds itself. `without_tags(text, tags)`
  takes the agent's blocks out of a message sent as the user's, and
  `inner(text, tag)` gives what one tag holds. A command of the agent's
  own, such as `/clear`, asks for no work, so it's no title.
- **Growing files.** Reading a file in two parts, the second from the
  cursor the first returned, gives what reading it whole does, and says
  nothing twice.
- **Changing a reader later.** If a change would read a file already read
  differently, raise `READERS` in `src/ingest.rs`, so every file is read
  again.

### Transcripts

`transcript` builds the conversation with a `Conversation` and returns its
`entries`:

- `say(kind, at, text)` adds an entry. Empty text is passed over.
- `call(id, at, name, input)` adds a tool call, and `answer(id, output,
  failed)` puts its result in it.
- `changed(files)` notes the files the call made last changed, as the agent
  names them: absolute, or within the session's folder. `patched(text)`
  gives the files of an `apply_patch` patch. Handoffs list these files.

| `Kind` | For |
|---|---|
| `User` | What the user typed |
| `Assistant` | What the model said |
| `Reasoning` | The model's reasoning |
| `Tool` | A tool call, from `call` |
| `System` | What the agent added itself: instructions, reminders, notices |
| `Summary` | The summary the agent wrote when it compacted the conversation |
| `Task` | What a subagent was asked to do, which arrives as the user's |

Handoffs depend on these kinds: every request is a `User` entry, and the
latest `Summary` stands for everything before it. They also show a tool
call by what it does: implement `role(name)` to say which of the agent's
tools read (`Role::Read`), ask the user (`Role::Ask`) or run a subagent
(`Role::Subagent`); any other is `Role::Work`. `transcript` doesn't
redact: Turnscope redacts what it returns before showing it.

### Logins

`logins(folder, home)` returns a `Login` for each provider the folder
holds a login for; one with no credential, or none at all, is signed out
of. It fails when a place that keeps logins can't be read, such as a file
being written: taking that for no login would sign its accounts out.

```rust
fn logins(&self, folder: &Path, _home: &Path) -> Result<Vec<Login>> {
    let kept = super::json_file(&folder.join("auth.json"))?.unwrap_or_default();
    let token = kept["access_token"]
        .as_str()
        .filter(|token| !token.is_empty());
    Ok(vec![Login {
        provider: "acme".to_owned(),
        credential: token.map(|token| Credential {
            provider: "acme",
            secret: Secret::new(token),
            key: false,
            expires: kept["expires_at"].as_i64(),
            identity: None,
            plan: None,
        }),
    }])
}
```

- `Login.provider` is the provider as the agent's responses name it. It
  must match `Response.provider` for usage to count toward the account.
- `Credential.provider` is the id of the provider that reads it, from
  `providers::ALL`. `secret` is a `Secret`, which can't be printed,
  serialized or cloned. `key` is true for an API key, false for a sign-in's
  token. `expires` is when the token expires, in Unix milliseconds: an
  expired token is never sent. `identity` is the account's id and label
  (such as an email), and `plan` the plan, where the agent's files say.
- `json_file(path)` reads a JSON file: `Ok(None)` when there's none, an
  error when it doesn't parse. `crate::output_within(command, wait)` runs a
  command with nothing on its standard input and stops it after `wait`, as
  Claude Code's reader does for the Keychain. `claims(token)` reads a JSON
  Web Token's claims, such as its expiry.
- A provider the folder held a login for and no longer lists is taken as
  signed out of, so `logins` only lists what the folder holds.

## 4. Register it

In `src/agents/mod.rs`:

```rust
mod acme;

pub static ALL: &[&dyn Agent] = &[
    // …
    &acme::Acme,
];
```

The order of `ALL` is the order agents are listed in.

## 5. Test it

In a `#[cfg(test)]` module in `src/agents/acme.rs`, test on small synthetic
files shaped like the agent's own, written to a `tempfile::tempdir()`, with
totals worked out by hand. Use no real session text, ids, emails or
secrets. Cover:

- each record that carries usage, with its tokens and cost;
- each rule above: merged reports, copies, titles, text said;
- reading a growing file on from its cursor, which reads nothing twice;
- the transcript's kinds, tool output and changed files;
- logins signed in and signed out.

Run what CI runs ([development.md](development.md)):

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Three CLI snapshots list every agent: `tests/snapshots/connect.txt`,
`connect.json`, and `mcp.json` (the MCP server's instructions). `cargo test`
names them as stale. Rewrite them, and check that the new agent is the only
change:

```sh
TURNSCOPE_BLESS=1 cargo test --test cli
```

## 6. Add its logo

Add a one-color SVG (`currentColor`) at `macos/Logos/agents/acme.svg`, and
say where it came from in `macos/Logos/README.md`. Without one, the app
draws a generic mark.

## 7. Update the docs

Name the agent in:

- `README.md`: the supported agents.
- `docs/cli.md`: the agent ids, and the `connect` output, copied from
  `tests/snapshots/connect.txt`.
- `docs/mcp.md`: the table of `connect` commands, the `connect` output,
  the agents' folders and configuration files, the commands "Who is
  asking" looks for and any `session_var`, `cwd_var` or `folder_var`, and
  the agent ids `find_sessions` takes.
- `docs/architecture.md`: the agent list, the glossary and the code map.
- `CHANGELOG.md`.

## Done when

- [ ] `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`
  and `cargo test --locked` pass.
- [ ] The rewritten snapshots differ only by the new agent.
- [ ] `macos/Logos/agents/acme.svg` exists, and `macos/Logos/README.md`
  says where it came from.
- [ ] `README.md`, `docs/cli.md`, `docs/mcp.md`, `docs/architecture.md` and
  `CHANGELOG.md` name the agent.
- [ ] On a Mac with the agent, `turnscope sessions --agent acme` lists its
  sessions, `turnscope sessions read` reads one, and `turnscope doctor`
  reports nothing about it.
