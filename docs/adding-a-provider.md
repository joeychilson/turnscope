# Adding a provider

A provider sells model usage and reports an account's limits. Each provider
is one file in `src/providers/` and one line in `providers::ALL`. Besides
the agents that hold its logins (step 5), only that file knows the
provider. The app needs nothing but a logo: it takes the account's title
from the status. Paths are in the engine's crate, `crates/turnscope/`, but
for the logo.

Prices need no code when responses name the provider by its models.dev id:
they come from models.dev's catalog. A response whose provider or model
models.dev doesn't list costs what the agent recorded, if anything, and
`turnscope doctor` names its model.

The steps use a provider called Acme, with id `acme`.

## 1. Measure its answers

Find the endpoint that reports usage for a login the agents hold, and read
real answers from it: each limit's fields, what a plan without a window
sends, how times are written, and what a refused login gets: its status,
and whether its body is an HTML page, which `ask` doesn't count as refused.
The module comment of `src/providers/acme.rs` says how its API answers.
Each decision you measured goes on the code it applies to, with its
numbers and dates.

## 2. Describe it in `Info`

The file is named by the provider's id, with underscores for dashes:
`opencode-go` is `opencode_go.rs`.

```rust
use std::time::Instant;

use serde_json::Value;

use super::{Account, Info, Limits, Problem, Provider, Reading};
use crate::agents::Credential;

pub struct Acme;

static INFO: Info = Info {
    id: "acme",
    name: "Acme",
    subscription: Some("Acme"),
    key_limits: false,
};
```

| Field | Type | What it is |
|---|---|---|
| `id` | `&'static str` | As stored, as agents' logins name it, as the logo's file name, and as models.dev names it, so its models are priced. |
| `name` | `&'static str` | As people know it. An API-key account is titled `<name> API key`. |
| `subscription` | `Option<&'static str>` | What its plans are called, before the plan. A subscription account is titled `<subscription> <plan>`, such as `Claude Max`, or `<subscription>` alone when no plan is known, as `OpenCode Go` is. `None` for a provider that sells no plans: every account is then titled `<name> API key`. |
| `key_limits` | `bool` | Whether an API key an agent holds reads any limit, as OpenRouter's does. Where it doesn't, as Anthropic's, an API-key account is listed with no limits, and `limits` is never given a key. |

## 3. Implement `Provider`

```rust
const USAGE: &str = "https://api.acme.example/v1/usage";

impl Provider for Acme {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        super::account(INFO.id, credential, || credential.identity.clone())
    }

    fn limits(&self, credential: &Credential, _now: i64, by: Instant) -> Result<Limits, Problem> {
        let answer = super::ask(USAGE, credential, &[], by)?;
        Ok(Limits {
            // The plan, when the answer names it; the login's is used
            // otherwise (`limits::title`).
            plan: None,
            readings: parse(&answer).ok_or_else(|| Problem::not_understood(USAGE))?,
        })
    }
}

/// `None` for an answer that isn't one, such as an error sent with 200.
fn parse(answer: &Value) -> Option<Vec<Reading>> {
    let mut readings = Vec::new();
    for window in answer.get("windows")?.as_array()? {
        if super::absent(window) {
            continue;
        }
        let length = window["length_seconds"].as_i64()? * 1000;
        let resets = super::reset(&window["resets_at"])?;
        readings.push(Reading {
            key: window["id"].as_str()?.to_owned(),
            name: super::window_name(length),
            scope: None,
            used: window["used_percent"].as_f64()?,
            size: None,
            starts: resets.map(|resets| resets - length),
            resets,
        });
    }
    Some(readings)
}
```

`account` says which account a credential is to, from the credential alone,
or `None` when it can't tell: that login's usage is then no account's. It
reads `credential.identity`, which the agent's files gave, or the token's
claims (`crate::agents::claims`). An `Account`:

| Field | What it is |
|---|---|
| `id` | `acme:<the provider's id for it>`. Never put a secret in it: an account known only by its key uses `key-` and `credential.secret.fingerprint()`, as OpenCode Go's does. |
| `kind` | `Kind::Subscription`, a plan with percent limits that reset, or `Kind::ApiKey`, paid as used, whose limits, where it has any, are money. |
| `label` | What tells it from another of its kind, such as an email address. |

`limits` reads what a login reads now: a sign-in's account's limits, or a
key's own. `now` is Unix milliseconds, for an answer that gives a reset as
time from now. `by` is when the account's read must end: pass it to every
request, which asks nothing once it's past. It returns a `Limits`: the
`plan` as the provider reports it (`max`, `plus`), and a `Reading` per
limit:

| Field | What it is |
|---|---|
| `key` | The same at every read: it names the limit in readings, alerts and what used it most. A limit that belongs to one API key is keyed `key-<fingerprint>`, from `credential.secret.fingerprint()`: the status then shows the agents holding that key, and the limit never decides how the account stands. |
| `name` | As the user knows it: `5 hours`, `Weekly`, `Credits`. A limit of money whose name ends in `s` is said as plural, "Credits are used up", by the command line, the MCP server and the app alike. |
| `scope` | The model it covers, when not all: `Opus`. Join several with "and", as `Claude and GPT`: a breakdown counts the responses of models whose id names any of them. A limit on use no agent here makes, as Anthropic's on other apps, gets a scope that names no model, `Other apps`, so it counts none. A limit with a scope never decides how its account stands while one without does. |
| `used` | Percent used. For a limit of money, what was spent divided by the limit, times 100. |
| `size` | Dollars, for a limit of money. For one that never resets, all that was ever bought, as OpenRouter's credits are, so it grows when more is: a `usedUp` alert is raised once per size. |
| `starts` | When its window started, in Unix milliseconds. `None` for a window that rolls, which then has no outlook or work left. |
| `resets` | When its window resets. `None` for a limit that never resets, such as credits. |

Where every key of an API-key account reads the same limits of the
account itself, as OpenRouter's credits, implement `account_limits` for
them, and keep them out of `limits`: they're asked once a read, with one
key after another until one isn't refused, as a key may have no access to
them.

For a window that rolls, implement `rolls`: given the limit's `key`, how
long the window is. What used the limit is then the use of the last so
long, as OpenCode Go's 5 hours is. The default, `None`, counts a limit
with no start over the last week.

If the provider's keys don't say which plan they're to, implement
`plan_mark`: a value read from `Limits` that tells one plan from another.
Accounts of the provider whose limits give the same mark are read as one
account, and the folders and usage of the rest move to it. OpenCode Go's is
the instant its monthly window resets, which keys to one plan report to the
second. The default, `None`, never merges.

If another provider serves the provider's models under its own id, as
Google serves Claude to Antigravity, implement `makes`: a model a provider
serves but models.dev doesn't list under it takes its maker's list price.

A failure is a `Problem`:

- `Problem::SignIn`: the provider refused the login. Refused at two reads
  in a row, the user is asked to sign in again in the agent, and an
  account in use raises a `signIn` alert.
- `Problem::Unavailable`: the provider is out of reach or busy, or its
  answer isn't understood. It says why, in words that are the same each
  time it fails the same way, as the log adds a line only when they
  change, how long the provider asked to be left (`Retry-After`), and
  whether it answered at all. `ask` and `asked` fill these in. For an
  answer `parse` doesn't understand, use `Problem::not_understood(url)`;
  for anything else, `Problem::unavailable(why)`.

The helpers in `src/providers/mod.rs` each have a doc comment:
`account(provider, credential, identity)` and `api_account(provider)`;
`ask(url, credential, headers, by)`, `asked(url, credential, headers,
body, refused, by)`, `asked_bearer(url, token, headers, body, refused,
by)`, `post_form(url, fields, by)` and
`request(url, headers, body, seconds)`; and
`absent(window)`, `reset(value)`, `window_name(length)` and `words(key)`
for reading an answer.

How Turnscope calls it, every five minutes, or less often while it
answers but is unavailable (`limits::backoff`):

- Every login the agents hold goes to `account`. Logins to one account are
  read together, in 15 s all told.
- A subscription is read with its freshest login, by `expires`. A refused
  login moves on to the next; an answer out of reach or not understood
  ends the read, as the next would most likely fare the same.
- An API-key account is read with every key at once, and
  `account_limits` beside them. Their readings are put together. A key
  that can't be read is the account's problem, beside what the others
  read.
- A login within a minute of its `expires` is never sent unless the
  provider sets `refreshes()` and the login carries a `refresh` token.

Rules:

- **Never rotate a refresh token or write a login.** Turnscope never
  writes to an agent's files or Keychain. A provider whose refresh tokens
  do not rotate (Google) may exchange one in memory (`refreshes()`),
  caching the short-lived access token in memory only; every other
  provider waits for the agent to refresh it.
- **Requests** go only through `ask`, `asked`, `asked_bearer`,
  `post_form` or `request`: curl with credentials on standard input,
  Turnscope's only way to the network. A request changes nothing at the
  provider.
- **An answer that isn't understood** is `Problem::not_understood`, not an
  empty list of readings, which would read as a plan without limits. A
  window whose time won't parse fails the whole answer, as the rest would
  pass for the plan's full set.
- **Parsing is a function of the answer**, as `parse` above, so tests can
  feed it answers without a request.

## 4. Register it

In `src/providers/mod.rs`:

```rust
mod acme;

pub static ALL: &[&dyn Provider] = &[
    // …
    &acme::Acme,
];
```

## 5. Have an agent hold its logins

Turnscope reads a provider's limits only once an agent's `logins` returns a
`Credential` naming it, and counts that agent folder's usage toward the
account through it.

- OpenCode and Pi name providers by their ids, so they pick up a new one
  when they keep its login under its id. They take an OAuth login for a
  sign-in's token, except to a provider with no `subscription`, as
  OpenRouter, whose sign-in gives a key (`kept_login` in
  `src/agents/mod.rs`).
- Where Pi names the provider its own way, add an arm to the `match` in
  `logins` in `src/agents/pi.rs`, as `"openai-codex" => "openai"`.
- Claude Code, Codex, Grok Build and Antigravity read only their own
  provider's login, so holding another there takes code in that agent's
  `logins`
  ([adding-an-agent.md](adding-an-agent.md#logins)).

## 6. Test it

In a `#[cfg(test)]` module in `src/providers/acme.rs`, feed `parse` answers
shaped like the provider's, written by hand, with no real ids, emails or
tokens. Cover what each field becomes, a window the plan doesn't have, and
an answer that isn't one. Tests never make requests.

```rust
#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn a_plans_windows_are_read_and_an_error_is_not_an_answer() {
        let answer = json!({"windows": [
            {"id": "five_hour", "length_seconds": 18000, "used_percent": 20.0, "resets_at": "2026-10-01T15:00:00Z"},
            null
        ]});
        let readings = super::parse(&answer).unwrap();
        assert_eq!(
            (readings[0].name.as_str(), readings[0].used),
            ("5 hours", 20.0)
        );
        assert_eq!(readings.len(), 1);
        assert!(super::parse(&json!({"error": "unauthorized"})).is_none());
    }
}
```

Run what CI runs ([development.md](development.md#test)). Adding a
provider changes no CLI snapshot.

## 7. Add its logo

Add a logo at `crates/turnscope-macos/assets/logos/providers/acme.svg`, as
[its README](../crates/turnscope-macos/assets/logos/README.md) says. Without one, the
app draws a generic mark.

## 8. Update the docs

Name the provider in:

- `README.md`: the supported providers, with what its plans are called.
- `docs/architecture.md`: the glossary and the code map.
- `CHANGELOG.md`.

## Done when

With an agent signed in to it, `turnscope status` shows the account and its
limits, and `turnscope doctor` reports nothing about it.
