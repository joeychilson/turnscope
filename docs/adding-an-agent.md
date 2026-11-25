# Adding an agent

An agent is a coding tool whose history and logins Turnscope reads. Each
agent is one file in `src/agents/` and one line in `agents::ALL`. Only that
file knows the agent's formats. The app needs nothing but a logo: it takes
the agent's name from the status.

The steps use an agent called Acme, with id `acme`. Pi
(`src/agents/pi.rs`) is a complete agent with JSON Lines history to read
alongside; OpenCode (`src/agents/opencode.rs`) keeps a SQLite database.
Paths are in the engine's crate, `crates/turnscope/`, but for the logo.

## 1. Measure the agent's files

Before writing code, look at real history: where the files are, what each
record holds, and how usage, sessions, subagents, forks and logins are
written. Read only what the agent writes today.

The module comment of `src/agents/acme.rs` says what the agent's files are,
and the agent's version they were read from.

## 2. Describe it in `Info`

In `src/agents/acme.rs`:

```rust
use std::borrow::Cow;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Agent, Credential, Info, Login, Read, Secret};
use crate::Result;

pub struct Acme;

static INFO: Info = Info {
    id: "acme",
    name: "Acme",
    command: "acme",
    app: None,
    mcp_add: &["mcp", "add", "turnscope", "--"],
    mcp_remove: Some(&["mcp", "remove", "turnscope"]),
    folder_var: Some("ACME_HOME"),
    charges: false,
    session_var: None,
    login_file: Some("auth.json"),
};
```

Each field's doc comment in `src/agents/mod.rs` says what it is. Beyond
those:

- `connect` runs `<command> <mcp_add> <path to turnscope> mcp`, with
  `HOME` set to the home Turnscope was given. With no `mcp_remove`,
  `disconnect` tells the user to remove the server by hand.
- `app` names the program in the app that starts an MCP server, not the
  app: otherwise a person's shell in a terminal the app holds counts as
  the agent. To find it, connect the agent, start a session in its app,
  and look at the server's parents with `ps`. It's `None` for an agent
  whose app runs `command`, or that has no app.
- `login_file` names the file the agent keeps its login in: a write to it
  asks for the limits again (`limits::refresh`), as after signing in
  again. An agent with its login in the Keychain alone has `None`.
- `folder_var` is needed when `folders` can find more than one folder.
- [mcp.md](mcp.md#who-is-asking) says how `command`, `app` and
  `session_var` tell the MCP server which agent and session call it.

## 3. Implement `Agent`

```rust
impl Agent for Acme {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn folders(&self, home: &Path) -> Vec<PathBuf> {
        super::own_and_found(home, ".acme", "auth.json")
    }

    fn files(&self, folder: &Path) -> Vec<PathBuf> { … }

    fn read(&self, file: &Path, cursor: &str) -> Result<Read> { … }

    fn logins(&self, folder: &Path, home: &Path) -> Result<Vec<Login>> { … }

    fn mcp_command(&self, folder: &Path, _home: &Path) -> Option<String> {
        super::json_mcp_command(&folder.join("config.json"), "/mcpServers/turnscope/command")
    }
}
```

`Result` is `crate::Result`. Its error here is `Error::Failed`, with a
message saying what went wrong. `reading(path, error)` is the
`Error::Failed` of a file that couldn't be read.

Each method's doc comment on the `Agent` trait says what it returns. The
helpers `own_and_found`, `entries`, `json_mcp_command` and
`toml_mcp_command` in `src/agents/mod.rs` do what most agents need.
Beyond those:

- `folders` lists the agent's own folder first: `installed` is asked of
  it, and it's the agent folder of a caller that names none.
- `files` runs at every catch-up, so it lists and doesn't read.
- An error from `read` leaves the cursor as it was, so the file is tried
  again at the next catch-up.
- An agent with `folder_var` keeps a configuration per folder: `connect`
  runs its command once for each folder, pointed at it with `folder_var`.
  So `mcp_command` reads the file that command writes for that folder.

### Reading history

A reader keeps its own cursor as a string. The readers here keep a serde
struct as JSON. For a JSON Lines file, `lines(path, from, visit)` passes
each complete line from byte `from` to `visit`, and returns where the next
read starts: past the last line that ends with a newline, since one
without is still being written. `INFO.session(native)` is a session's id
as stored, `acme:<native>`.

```rust
#[derive(Default, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
}

#[derive(Deserialize)]
struct Line<'a> {
    #[serde(rename = "type", borrow, default)]
    kind: Cow<'a, str>,
    #[serde(borrow, default, deserialize_with = "super::lenient")]
    timestamp: Option<Cow<'a, str>>,
    // The other fields the reader needs.
}

fn read(&self, file: &Path, cursor: &str) -> Result<Read> {
    let mut cursor: Cursor = serde_json::from_str(cursor).unwrap_or_default();
    let mut read = Read::default();
    cursor.offset = super::lines(file, cursor.offset, |bytes| {
        let Ok(line) = serde_json::from_slice::<Line>(bytes) else {
            read.skipped += 1;
            return;
        };
        // What `line` says: read.session(…), read.respond(…).
    })?;
    read.cursor = serde_json::to_string(&cursor).unwrap_or_default();
    Ok(read)
}
```

A line is parsed into a struct that borrows from it, not a `Value`: for a
file that grows large, that takes less than half the time (`grok_build.rs`
says what was measured). A field the reader can do without is `lenient`, so
a null or a new type there doesn't lose the line.

An agent that keeps a SQLite database, as OpenCode does, is opened read-only
and read on from a little before the latest `time_updated` seen, since a
row can be committed after one stamped later, and from no later than now,
since a row can be stamped ahead of the clock (`src/agents/opencode.rs`).
Rows read again are reported again, which changes nothing. `Stat::of` in
`src/ingest.rs` looks for a write-ahead log only beside a file whose name
ends in `.db`, as OpenCode's does. For a database named otherwise, add its
ending there: until a checkpoint, new rows are only in the log, and the
file itself doesn't change.

The doc comments of `Read`, `Session`, `Title`, `Response` and `Tokens` in
`src/agents/mod.rs` say what each field holds. A response's `at` is in Unix
milliseconds. Its `provider` is matched with the agent folder's sign-in to
find the account, and with models.dev's provider ids for the price.

Rules:

- **Tokens.** If the agent's input count includes cache reads and writes,
  subtract them. If its output count leaves reasoning out, add it in.
  `read.respond` passes over a report with no tokens and no charge, such as
  one stopped before it wrote anything.
- **One response, many reports.** A response can be written several times:
  a line rewritten as it streams, a fork's copy of its parent's history, a
  continuation's. Give every report the same `id` and pass each to
  `read.respond`. Set `copy` on a report that may be another session's. A
  report that isn't a copy decides which session owns the response.
- **Titles.** A session the agent or the user named takes that title.
  Otherwise offer the first thing the user asked, as `Title::Prompt`, and
  read no more of what was said: Turnscope keeps none of it. Offer what
  the user typed, not what the agent adds itself. `texts(content)` gives a
  message's text, whether its content is a string or parts with `text`.
  `without_tags(text, tags)`
  takes the agent's blocks out of a message sent as the user's, and
  `inner(text, tag)` gives what one tag holds. A tag with no closing one is
  taken as the agent's only where the text opens with it: further in, the
  user typed it, and what follows is theirs. A command of the agent's
  own, such as `/clear`, asks for no work, so it's no title.
- **Growing files.** Reading a file in two parts, the second from the
  cursor the first returned, gives what reading it whole does.
- **Changing a reader later.** If a change would read a file already read
  differently, raise `READERS` in `src/ingest.rs`, so every file is read
  again.
- **Limits.** An agent that writes its account's limits as it works, as
  Codex does, passes each set to `read.in_band`, keyed as the provider's
  answer keys them (`providers::window`). They go to the account the folder
  was signed in to then, while it still is, and only those newer than the
  provider's last read are taken (`limits::record_in_band`).

### Logins

`logins(folder, home)` returns a `Login` for each provider the folder
holds a login for, and fails as its doc comment says:

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
            refresh: None,
        }),
    }])
}
```

The doc comments of `Login` and `Credential` say what each field holds.
`Login.provider` must match `Response.provider` for usage to count toward
the account.

The helpers `json_file`, `kept_login` and `claims` in `src/agents/mod.rs`,
and `crate::process::output_within`, read what agents keep. An agent's
SQLite database is read with `opened`, which leaves nothing beside it. `keychain(home, service,
account, parse)` reads a Keychain item with `/usr/bin/security`, as Claude
Code's and Antigravity's readers do. `parse` reads the item's bytes and
returns why it couldn't. Its error names the item, and it's `Ok(None)`
when there's no such item.

A folder that `folders` no longer lists is signed out of, so `folders`
lists a folder by what's on disk alone: not by an environment variable or
a running process, which come and go.

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
- each rule above: merged reports, copies, titles;
- reading a growing file on from its cursor, which reads nothing twice;
- logins signed in and signed out.

Run what CI runs ([development.md](development.md#test)).

Then add a session of Acme's to the fixture home,
`crates/turnscope/tests/fixtures/home`, where the agent keeps it
(`.acme/…`) and in its format: a request, and a reply or two with usage,
made up as the unit tests' are. Date it on Sep 30, a day before the tests'
clock, as the other agents' sessions are. An agent whose history is a
database, as OpenCode's is, gets the SQL that builds it beside where the
database goes, named as the database with `.sql` for `.db`: `home()` in
`crates/turnscope/tests/cli.rs` builds each, as it does `opencode.db` from
`opencode.sql`.

The new agent and its session change the snapshots that list every agent or
its usage, such as `connect`, `usage`, `usage-model`, `status.json` and
`mcp.json` (the MCP server's instructions). `cargo test` names them as
stale. Rewrite them, and check that the new agent and its session are the
only change:

```sh
TURNSCOPE_BLESS=1 cargo test --test cli
```

## 6. Add its logo

Add a one-color SVG (`currentColor`) at
`assets/logos/agents/acme.svg`, and say where it
came from in `assets/logos/README.md`. Without one, the panel shows a
terminal in its place.

## 7. Update the docs

Name the agent in:

- `README.md`: the supported agents.
- `docs/cli.md`: the agent ids, any `folder_var` or `session_var` in
  [Environment](cli.md#environment), and the examples whose snapshots
  changed, such as the `connect` output, copied from
  `crates/turnscope/tests/snapshots/connect.txt`.
- `docs/mcp.md`: the commands "Who is asking" looks for, and any
  `session_var` or `folder_var`.
- `docs/architecture.md`: the agent list and the glossary.
- `CHANGELOG.md`.

## Done when

On a Mac with the agent, `turnscope usage --agent acme --by session` lists
its sessions with their usage, and `turnscope doctor` reports nothing about
it.
