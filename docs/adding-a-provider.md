# Adding a provider

A provider sells model usage and reports an account's limits. Each provider
is one file in `src/providers/` and one line in `providers::ALL`. Besides
the agents that hold its logins (step 5), only that file knows the
provider. The app needs nothing but a logo: it takes the account's title
from the status.

Prices need no code when responses name the provider by its models.dev id:
they come from models.dev's catalog. A response whose provider or model
models.dev doesn't list costs what the agent recorded, if anything, and
`turnscope doctor` names its model.

The steps use a provider called Acme, with id `acme`.

| Step | File |
|---|---|
| [1. Measure its answers](#1-measure-its-answers) | The module comment of `src/providers/acme.rs` |
| [2. Describe it in `Info`](#2-describe-it-in-info) | `src/providers/acme.rs` |
| [3. Implement `Provider`](#3-implement-provider) | `src/providers/acme.rs` |
| [4. Register it](#4-register-it) | `src/providers/mod.rs` |
| [5. Have an agent hold its logins](#5-have-an-agent-hold-its-logins) | `src/agents/<agent>.rs` |
| [6. Redact its keys](#6-redact-its-keys) | `src/redact.rs` |
| [7. Test it](#7-test-it) | `src/providers/acme.rs` |
| [8. Add its logo](#8-add-its-logo) | `macos/Logos/providers/acme.svg` |
| [9. Update the docs](#9-update-the-docs) | `README.md`, `docs/architecture.md`, `CHANGELOG.md` |

## 1. Measure its answers

Find the endpoint that reports usage for a login the agents hold, and read
real answers from it: each limit's fields, what a plan without a window
sends, how times are written, and what a refused login gets. Write what you
measured, with dates, in the module comment at the top of
`src/providers/acme.rs`, as the other providers do.

## 2. Describe it in `Info`

The file is named by the provider's id, with underscores for dashes:
`opencode-go` is `opencode_go.rs`.

```rust
use serde_json::Value;

use super::{Account, Info, Kind, Limits, Problem, Provider, Reading};
use crate::agents::Credential;

pub struct Acme;

static INFO: Info = Info {
    id: "acme",
    name: "Acme",
    subscription: Some("Acme"),
};
```

| Field | Type | What it is |
|---|---|---|
| `id` | `&'static str` | As stored, as agents' logins name it, as the logo's file name, and as models.dev names it, so its models are priced. |
| `name` | `&'static str` | As people know it. An API-key account is titled `<name> API key`. |
| `subscription` | `Option<&'static str>` | What its plans are called, before the plan. A subscription account is titled `<subscription> <plan>`, such as `Claude Max`, or `<subscription>` alone when there's no plan or the name already ends with it, as `OpenCode Go` does. `None` for a provider that sells no plans: every account is then titled `<name> API key`. |

## 3. Implement `Provider`

```rust
impl Provider for Acme {
    fn info(&self) -> &'static Info {
        &INFO
    }

    fn account(&self, credential: &Credential) -> Option<Account> {
        if credential.key {
            return Some(super::api_account("acme"));
        }
        let (id, label) = credential.identity.clone()?;
        Some(Account {
            id: format!("acme:{id}"),
            kind: Kind::Subscription,
            label,
        })
    }

    fn limits(&self, credential: &Credential, _now: i64) -> Result<Limits, Problem> {
        if credential.key {
            return Ok(Limits {
                plan: None,
                readings: Vec::new(),
            });
        }
        let answer = super::ask("https://api.acme.example/v1/usage", credential, &[])?;
        Ok(Limits {
            plan: credential.plan.clone(),
            readings: parse(&answer).ok_or(Problem::Unavailable)?,
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
        let seconds = window["length_seconds"].as_i64()?;
        let resets = match &window["resets_at"] {
            Value::Null => None,
            at => Some(super::time(at)?),
        };
        readings.push(Reading {
            key: window["id"].as_str()?.to_owned(),
            name: super::window_name(seconds),
            scope: None,
            used: window["used_percent"].as_f64()?,
            size: None,
            starts: resets.map(|resets| resets - seconds * 1000),
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

`limits` reads the account's limits now. `now` is Unix milliseconds, for an
answer that gives a reset as time from now. It returns a `Limits`: the
`plan` as the provider reports it (`max`, `plus`), and a `Reading` per
limit:

| Field | What it is |
|---|---|
| `key` | The same at every read: it names the limit in readings, alerts and `limit.breakdown`. A limit that belongs to one API key is keyed `key-<fingerprint>`, from `credential.secret.fingerprint()`: the status then shows the agents holding that key, and the limit never decides how the account stands. |
| `name` | As the user knows it: `5 hours`, `Weekly`, `Credits`. |
| `scope` | The one model it covers, when not all: `Opus`. |
| `used` | Percent used. For a limit of money, what was spent divided by the limit, times 100. |
| `size` | Dollars, for a limit of money. |
| `starts` | When its window started, in Unix milliseconds. `None` for a window that rolls, which then has no outlook or work left. |
| `resets` | When its window resets. `None` for a limit that never resets, such as credits. |

If the provider's keys don't say which plan they're to, implement
`plan_mark`: a value read from `Limits` that tells one plan from another.
Accounts of the provider whose limits give the same mark are read as one
account, and the folders and usage of the rest move to it. OpenCode Go's is
the instant its monthly window resets, which keys to one plan report to the
second. The default, `None`, never merges.

A failure is a `Problem`:

- `Problem::SignIn`: the provider refused the login. The user signs in
  again in the agent, and an account in use raises a `signIn` alert.
- `Problem::Unavailable`: the provider is out of reach or busy, or its
  answer isn't understood.

Helpers in `src/providers/mod.rs`:

| Helper | Does |
|---|---|
| `ask(url, credential, headers)` | GETs `url` with `Authorization: Bearer <secret>` and `headers`, for JSON. A 2xx status gives the JSON, 401 or 403 `Problem::SignIn`, and anything else `Problem::Unavailable`. |
| `ask_refused_by(url, credential, headers, refused)` | As `ask`, with `refused` the statuses that mean a refused login, where a 403 can mean something else, as at xAI. |
| `get(url, headers)` | The request itself, giving the status and body, for a provider whose token goes in another header. |
| `time(value)` | A time given as RFC 3339 text, or as Unix seconds or milliseconds. |
| `absent(window)` | Whether a window is null or all nulls, as providers send one a plan doesn't have. |
| `window_name(seconds)` | A window's name by its length: `Hourly`, `5 hours`, `Daily`, `Weekly`, `Monthly`, or `<n> days`. |
| `words(key)` | A provider's key as words: `super_heavy` is `Super Heavy`. |
| `api_account(provider)` | The one account, `<provider>:api`, that all of a provider's API keys draw on. |

How Turnscope calls it, every five minutes:

- Every login the agents hold goes to `account`. Logins to one account are
  read together.
- A subscription is read with its freshest login, by `expires`, that
  works. An API-key account is read with every key, and their readings are
  merged by `key`.
- A login past its `expires` is never sent.

Rules:

- **Never refresh a token** or write a login. Turnscope waits for the agent
  to refresh it.
- **Requests** go only through `ask`, `ask_refused_by` or `get`: curl with
  the credential on standard input, Turnscope's only way to the network.
- **An answer that isn't understood** is `Problem::Unavailable`, not an
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
  sign-in's token, except OpenRouter's, which is a key. If the new
  provider's sign-in gives a key too, add it beside `openrouter` in their
  `logins`.
- Where Pi names the provider its own way, add the pair to `ALIASES` in
  `src/agents/pi.rs`, as `("openai-codex", "openai")`.
- Claude Code, Codex and Grok Build read only their own provider's login,
  so holding another there takes code in that agent's `logins`
  ([adding-an-agent.md](adding-an-agent.md#logins)).

## 6. Redact its keys

If the provider's API keys start with a prefix not in `PREFIXES` in
`src/redact.rs`, add it, with the least number of key characters that must
follow it, and a case to the test there. `sk-` already covers keys like
`sk-ant-`, `sk-or-` and `sk-proj-`.

## 7. Test it

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

Run what CI runs ([development.md](development.md)):

```sh
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

Adding a provider changes no CLI snapshot.

## 8. Add its logo

Add a one-color SVG (`currentColor`) at `macos/Logos/providers/acme.svg`.
models.dev has most providers' logos, at `https://models.dev/logos/<id>.svg`.
It must be shapes, not an embedded picture, and not models.dev's placeholder
([macos/Logos/README.md](../macos/Logos/README.md)). Without one, the app
draws a generic mark.

## 9. Update the docs

Name the provider in:

- `README.md`: the supported providers, with what its plans are called.
- `docs/architecture.md`: the glossary and the code map.
- `CHANGELOG.md`.

## Done when

- [ ] `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings`
  and `cargo test --locked` pass, and no snapshot changed.
- [ ] `macos/Logos/providers/acme.svg` exists.
- [ ] `README.md`, `docs/architecture.md` and `CHANGELOG.md` name the
  provider.
- [ ] With an agent signed in to it, `turnscope status` shows the account
  and its limits, and `turnscope doctor` reports nothing about it.
