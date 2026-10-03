//! Which account is which: what an account is called, which one an argument
//! names, which one is the caller's, and which usage draws on each.
//!
//! Every question of this kind is answered here, so that when the engine
//! knows accounts better only this module changes.
//!
//! **Names.** An account is called as the app calls it: its title
//! ([`AccountLimits::title`]), its subscription and plan or its provider's
//! API, and what tells it apart, its label, as `Claude Max ·
//! joey@example.com` or `OpenRouter API key`. A list, where the account is
//! said again on every line, calls it by its title alone, unless another
//! account shown has that title too ([`short`]). An argument names an
//! account by its id, either name, its label, an API key's provider, or its
//! subscription alone, ignoring case.
//!
//! **Hidden.** An account the person hid in Turnscope is left out of every
//! list an agent is given, never named in a sentence, and never taken to
//! be the caller's. Where a session drew on it, it is given by its id
//! alone, marked hidden; named by its id, it is answered as any other.
//!
//! **Yours.** The engine puts each response down to the account signed in
//! where and when it was made ([`turnscope_engine::SessionRow::account`]).
//! The caller's account is the one its session drew on, as that says,
//! unless where the caller works has been signed into another account of
//! what it drew on since, as after a `/login`, when it is that one. With no
//! session of its to go by, it is the account signed in where the caller
//! works: the folder its environment points its agent at, as a second
//! account's is ([`crate::Caller::config`]), or the agent's own; and of
//! several signed in there, as an agent signed into several subscriptions
//! is, the one in use. Where that leaves none or several, or the folder is
//! one Turnscope doesn't read, which account is unknown, and the answer
//! says so rather than guess.
//!
//! **Usage.** What drew on an account is what the engine puts down to it,
//! response by response ([`turnscope_engine::Filter::accounts`]), so a
//! session made under one account and then another after a switch counts in
//! each for its part; a session itself is given as the account most of its
//! responses drew on ([`of_session`]).

use std::path::Path;

use turnscope_engine::{AccountLimits, Folder, FolderOrigin, SessionRow, Subscription};

use crate::caller::Caller;
use crate::sessions;
use crate::tools::{Failure, Server};

/// What `account` is called, as the app calls it: its title and its label,
/// as `Claude Max · joey@example.com`, or `OpenRouter API key`.
pub(crate) fn name(account: &AccountLimits) -> String {
    let title = account.title();
    match account.label.as_deref().filter(|label| !label.is_empty()) {
        Some(label) => format!("{title} \u{b7} {label}"),
        None => title,
    }
}

/// What a list calls `account`, of those `accounts` shown: its title alone,
/// as `Claude Max`, unless another shown has that title too, when it is its
/// [`name`]. An argument names it either way.
pub(crate) fn short(account: &AccountLimits, accounts: &[AccountLimits]) -> String {
    let title = account.title();
    let shared = shown(accounts).any(|other| other.id != account.id && other.title() == title);
    if shared { name(account) } else { title }
}

/// What an answer calls the account `id`: its name, or, hidden, only that
/// it is; `None` when no account has that id.
pub(crate) fn called(id: &str, accounts: &[AccountLimits]) -> Option<String> {
    let account = accounts.iter().find(|account| account.id == id)?;
    Some(if account.hidden {
        "an account hidden in Turnscope".to_owned()
    } else {
        name(account)
    })
}

/// The accounts of `accounts` the person hasn't hidden.
pub(crate) fn shown(accounts: &[AccountLimits]) -> impl Iterator<Item = &AccountLimits> {
    accounts.iter().filter(|account| !account.hidden)
}

/// The accounts `asked` names: by id exactly, hidden or not; or, of those
/// shown, by name, its label, an API key's provider, or subscription,
/// ignoring case. None when it names none.
pub(crate) fn named<'a>(asked: &str, accounts: &'a [AccountLimits]) -> Vec<&'a AccountLimits> {
    let asked = asked.trim();
    if let Some(account) = accounts.iter().find(|account| account.id == asked) {
        return vec![account];
    }
    let lower = asked.to_lowercase();
    let is = |text: &str| text.to_lowercase() == lower;
    let by_name: Vec<&AccountLimits> = shown(accounts)
        .filter(|account| {
            is(&name(account))
                || is(&account.title())
                || [&account.label, &account.provider]
                    .into_iter()
                    .any(|apart| apart.as_deref().is_some_and(&is))
        })
        .collect();
    if !by_name.is_empty() {
        return by_name;
    }
    shown(accounts)
        .filter(|account| is(account.subscription.key()) || is(account.subscription.name()))
        .collect()
}

/// The one account `asked` names, as [`named`] finds it, or why there isn't
/// one.
pub(crate) fn one<'a>(
    asked: &str,
    accounts: &'a [AccountLimits],
) -> Result<&'a AccountLimits, Failure> {
    match named(asked, accounts).as_slice() {
        [] => Err(no_account(asked, accounts)),
        [one] => Ok(one),
        several => Err(Failure(format!(
            "{asked:?} names {} accounts: {}. Name one by its id.",
            several.len(),
            several
                .iter()
                .map(|account| format!("{} ({})", name(account), account.id))
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The ids of every account `asked` names, as [`named`] finds them, for a
/// question of usage, which draws on them all alike: `claude` names each
/// Claude account, and an email each account signed in with it. Why there
/// is none when it names none.
pub(crate) fn ids(asked: &str, accounts: &[AccountLimits]) -> Result<Vec<String>, Failure> {
    Ok(every_named(asked, accounts)?
        .into_iter()
        .map(|account| account.id.clone())
        .collect())
}

/// Every account `asked` names, as [`named`] finds them, or why there is
/// none.
pub(crate) fn every_named<'a>(
    asked: &str,
    accounts: &'a [AccountLimits],
) -> Result<Vec<&'a AccountLimits>, Failure> {
    let named = named(asked, accounts);
    if named.is_empty() {
        return Err(no_account(asked, accounts));
    }
    Ok(named)
}

/// Why no account is what `asked` names, with those there are that the
/// person hasn't hidden.
fn no_account(asked: &str, accounts: &[AccountLimits]) -> Failure {
    let names: Vec<String> = shown(accounts)
        .map(|account| format!("{} ({})", name(account), account.id))
        .collect();
    Failure(if names.is_empty() {
        format!("No account is {asked:?}; Turnscope knows no account yet.")
    } else {
        format!(
            "No account is {asked:?}; the accounts are {}.",
            names.join(", ")
        )
    })
}

/// The providers whose usage draws on `account`, by the catalog's id for
/// each: an API key's own, or those its subscription reaches.
fn providers(account: &AccountLimits) -> Vec<&str> {
    match (account.subscription, account.provider.as_deref()) {
        (Subscription::ApiKey, Some(provider)) => vec![provider],
        (subscription, _) => subscription.providers().to_vec(),
    }
}

/// The folder, of the `folders` read, that `caller`'s agent keeps its
/// sign-in and history in: the one its environment points it at, or its
/// own; or why that isn't known.
fn callers_folder<'a>(caller: &Caller, folders: &'a [Folder]) -> Result<&'a Folder, String> {
    let Some(agent) = caller.agent else {
        return Err(
            "Turnscope can't tell which agent started it, so which account is yours isn't known."
                .to_owned(),
        );
    };
    let Some((variable, given)) = &caller.config else {
        return folders
            .iter()
            .find(|folder| folder.agent == agent && folder.origin == FolderOrigin::Own)
            .ok_or_else(|| {
                format!(
                    "{agent}'s own folder was removed from those Turnscope reads, so which \
                     account is yours isn't known."
                )
            });
    };
    let given = Path::new(given);
    let same = |path: &Path| {
        path == given
            || std::fs::canonicalize(path)
                .is_ok_and(|path| std::fs::canonicalize(given).is_ok_and(|given| path == given))
    };
    folders
        .iter()
        .find(|folder| folder.agent == agent && same(&folder.path))
        .ok_or_else(|| {
            format!(
                "{agent} runs with {variable}={}, a folder Turnscope doesn't read, so which \
                 account is yours isn't known.",
                given.display()
            )
        })
}

/// The account `caller` draws on, among `accounts`, from its `session`
/// where one is known and the `folders` read, as the module says; or why
/// that isn't known.
fn yours<'a>(
    caller: &Caller,
    folders: &[Folder],
    session: Option<&SessionRow>,
    accounts: &'a [AccountLimits],
) -> Result<&'a AccountLimits, String> {
    let folder = callers_folder(caller, folders)?;
    let agent = folder.agent;
    let there: Vec<&'a AccountLimits> = accounts
        .iter()
        .filter(|account| {
            account.signed_in
                && account
                    .folders
                    .iter()
                    .any(|(held_by, path)| *held_by == agent && *path == folder.path)
        })
        .collect();
    let drew = session.and_then(|session| of_session(session, accounts));
    let chosen: &'a AccountLimits = match drew {
        Some(drew) if there.is_empty() || there.iter().any(|account| account.id == drew.id) => drew,
        Some(drew) => {
            // Signed in since to another account of what it drew on.
            let since: Vec<&'a AccountLimits> = there
                .iter()
                .copied()
                .filter(|account| {
                    providers(account)
                        .iter()
                        .any(|provider| providers(drew).contains(provider))
                })
                .collect();
            match since.as_slice() {
                [one] => one,
                _ => drew,
            }
        }
        None => match there.as_slice() {
            [] => {
                return Err(format!(
                    "{agent} isn't signed into a subscription Turnscope reads the limits of, in \
                     {}.",
                    folder.path.display()
                ));
            }
            [one] => one,
            several => {
                let in_use: Vec<&'a AccountLimits> = several
                    .iter()
                    .copied()
                    .filter(|account| account.in_use)
                    .collect();
                match in_use.as_slice() {
                    [one] => one,
                    _ => {
                        return Err(format!(
                            "{agent} is signed into {} accounts ({}), and which it is using \
                             isn't known.",
                            several.len(),
                            several
                                .iter()
                                .map(|account| name(account))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                }
            }
        },
    };
    if chosen.hidden {
        return Err(format!(
            "{agent} draws on an account hidden in Turnscope ({}); name it by that id to see it.",
            chosen.id
        ));
    }
    Ok(chosen)
}

/// This session, the caller's, and the caller's account or why that isn't
/// known, among `accounts`, as [`yours`] finds it.
pub(crate) fn asking<'a>(
    server: &Server,
    accounts: &'a [AccountLimits],
) -> Result<(Option<SessionRow>, Result<&'a AccountLimits, String>), Failure> {
    let folders = server.engine.folders();
    let folder = callers_folder(&server.caller, &folders).ok();
    let session = sessions::this(server, folder)?;
    let account = yours(&server.caller, &folders, session.as_ref(), accounts);
    Ok((session, account))
}

/// The account the session `row` drew on, among `accounts`: the one most
/// of its responses did, as the engine puts them down. `None` when that
/// isn't known.
pub(crate) fn of_session<'a>(
    row: &SessionRow,
    accounts: &'a [AccountLimits],
) -> Option<&'a AccountLimits> {
    let id = row.account.as_deref()?;
    accounts.iter().find(|account| account.id == id)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use turnscope_engine::{
        AccountLimits, Agent, Folder, FolderOrigin, SessionKey, SessionRow, Subscription, Totals,
    };

    use super::{name, named, yours};
    use crate::caller::Caller;

    const HOME: &str = "/Users/joey";

    fn folder(agent: Agent, path: &str, origin: FolderOrigin) -> Folder {
        Folder {
            agent,
            path: PathBuf::from(HOME).join(path),
            origin,
        }
    }

    /// Every agent's own folder, and Claude Code's `~/.claude-work`.
    fn folders() -> Vec<Folder> {
        vec![
            folder(Agent::ClaudeCode, ".claude", FolderOrigin::Own),
            folder(Agent::ClaudeCode, ".claude-work", FolderOrigin::Found),
            folder(Agent::Codex, ".codex", FolderOrigin::Own),
            folder(Agent::OpenCode, ".local/share/opencode", FolderOrigin::Own),
            folder(Agent::Pi, ".pi/agent", FolderOrigin::Own),
        ]
    }

    /// An account of `subscription`, signed into each of `signed`, an agent
    /// and its folder under the home folder.
    fn account(id: &str, subscription: Subscription, signed: &[(Agent, &str)]) -> AccountLimits {
        AccountLimits {
            id: id.to_owned(),
            subscription,
            label: None,
            plan: None,
            agents: signed.iter().map(|(agent, _)| *agent).collect(),
            signed_in: true,
            limits: Vec::new(),
            read_at: None,
            checked_at: None,
            problem: None,
            in_use: false,
            hidden: false,
            provider: None,
            folders: signed
                .iter()
                .map(|(agent, path)| (*agent, PathBuf::from(HOME).join(path)))
                .collect(),
        }
    }

    fn api(provider: &str, agents: &[Agent]) -> AccountLimits {
        AccountLimits {
            provider: Some(provider.to_owned()),
            agents: agents.to_vec(),
            ..account(&format!("api:{provider}"), Subscription::ApiKey, &[])
        }
    }

    /// A session of `agent` that drew mostly on `account`.
    fn session(agent: Agent, account: Option<&str>) -> SessionRow {
        SessionRow {
            key: SessionKey::new(agent, "s"),
            title: None,
            project: None,
            cwd: None,
            branch: None,
            started: None,
            active: None,
            parent: None,
            present: true,
            totals: Totals::default(),
            with_subagents: Totals::default(),
            models: Vec::new(),
            subagents: 0,
            folder: None,
            account: account.map(str::to_owned),
        }
    }

    fn caller(agent: Agent, config: Option<(&str, &str)>) -> Caller {
        Caller {
            agent: Some(agent),
            config: config.map(|(variable, folder)| (variable.to_owned(), folder.to_owned())),
            ..Caller::default()
        }
    }

    #[test]
    fn an_account_is_named_by_its_plan_and_what_tells_it_apart() {
        let mut work = account("claude:a:o", Subscription::Claude, &[]);
        work.plan = Some("max".into());
        work.label = Some("joey@example.com".into());
        assert_eq!(name(&work), "Claude Max \u{b7} joey@example.com");
        // A plan the subscription is named for is said once.
        let mut go = account("opencode-go:1", Subscription::OpenCodeGo, &[]);
        go.plan = Some("go".into());
        assert_eq!(name(&go), "OpenCode Go");
        // An API key's provider tells it apart.
        let router = api("openrouter", &[Agent::Pi]);
        assert_eq!(name(&router), "OpenRouter API key");
        let mut hidden = account("claude:b:o", Subscription::Claude, &[]);
        hidden.label = Some("old@example.com".into());
        hidden.hidden = true;
        let accounts = [work, go, router, api("anthropic", &[Agent::Pi]), hidden];
        let ids = |asked: &str| {
            named(asked, &accounts)
                .iter()
                .map(|account| account.id.as_str())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids("claude:a:o"), ["claude:a:o"]);
        assert_eq!(ids("CLAUDE MAX · joey@example.com"), ["claude:a:o"]);
        assert_eq!(ids("joey@example.com"), ["claude:a:o"]);
        assert_eq!(ids("opencode-go"), ["opencode-go:1"]);
        assert_eq!(ids("openrouter"), ["api:openrouter"]);
        assert_eq!(ids("api"), ["api:openrouter", "api:anthropic"]);
        assert!(ids("gemini").is_empty());
        // A hidden account is named only by its id.
        assert_eq!(ids("claude"), ["claude:a:o"]);
        assert!(ids("old@example.com").is_empty());
        assert_eq!(ids("claude:b:o"), ["claude:b:o"]);
    }

    #[test]
    fn yours_is_the_account_signed_in_where_you_work_or_unknown() {
        let personal = account(
            "claude:personal",
            Subscription::Claude,
            &[(Agent::ClaudeCode, ".claude")],
        );
        let work = account(
            "claude:work",
            Subscription::Claude,
            &[(Agent::ClaudeCode, ".claude-work")],
        );
        let chatgpt = account(
            "chatgpt:w",
            Subscription::ChatGpt,
            &[
                (Agent::Codex, ".codex"),
                (Agent::OpenCode, ".local/share/opencode"),
            ],
        );
        let mut go = account(
            "opencode-go:1",
            Subscription::OpenCodeGo,
            &[(Agent::OpenCode, ".local/share/opencode")],
        );
        let accounts = [personal.clone(), work.clone(), chatgpt.clone(), go.clone()];
        let folders = folders();
        let id = |caller: &Caller, accounts: &[AccountLimits]| {
            yours(caller, &folders, None, accounts).map(|account| account.id.clone())
        };
        assert_eq!(
            id(&caller(Agent::ClaudeCode, None), &accounts),
            Ok(personal.id.clone())
        );
        // Pointed at the second folder, the account signed in there.
        let at_work = caller(
            Agent::ClaudeCode,
            Some(("CLAUDE_CONFIG_DIR", "/Users/joey/.claude-work/")),
        );
        assert_eq!(id(&at_work, &accounts), Ok(work.id.clone()));
        // Pointed at a folder Turnscope doesn't read: not known.
        let elsewhere = caller(
            Agent::ClaudeCode,
            Some(("CLAUDE_CONFIG_DIR", "/Users/joey/.claude-other")),
        );
        assert!(id(&elsewhere, &accounts).unwrap_err().contains(
            "CLAUDE_CONFIG_DIR=/Users/joey/.claude-other, a folder Turnscope doesn't read"
        ));
        assert_eq!(
            id(&caller(Agent::Codex, None), &accounts),
            Ok(chatgpt.id.clone())
        );
        assert!(id(&Caller::default(), &accounts).is_err());
        assert!(id(&caller(Agent::Pi, None), &accounts).is_err());
        // OpenCode is signed into two there; the one in use is its.
        assert!(id(&caller(Agent::OpenCode, None), &accounts).is_err());
        go.in_use = true;
        let accounts = [personal.clone(), work.clone(), chatgpt.clone(), go.clone()];
        assert_eq!(
            id(&caller(Agent::OpenCode, None), &accounts),
            Ok(go.id.clone())
        );
        // Hidden, it isn't taken for yours.
        let mut hidden = personal.clone();
        hidden.hidden = true;
        assert!(
            id(&caller(Agent::ClaudeCode, None), &[hidden])
                .unwrap_err()
                .contains("hidden in Turnscope (claude:personal)")
        );
    }

    #[test]
    fn your_session_says_which_account_unless_you_signed_into_another_since() {
        let folders = folders();
        let opencode = caller(Agent::OpenCode, None);
        let chatgpt = account(
            "chatgpt:w",
            Subscription::ChatGpt,
            &[(Agent::OpenCode, ".local/share/opencode")],
        );
        let go = account(
            "opencode-go:1",
            Subscription::OpenCodeGo,
            &[(Agent::OpenCode, ".local/share/opencode")],
        );
        let key = api("anthropic", &[Agent::OpenCode]);
        let accounts = [chatgpt, go, key];
        let drew = |account: &str| {
            yours(
                &opencode,
                &folders,
                Some(&session(Agent::OpenCode, Some(account))),
                &accounts,
            )
            .map(|account| account.id.clone())
        };
        // Signed into two, it drew on one of them, or on a key.
        assert_eq!(drew("opencode-go:1"), Ok("opencode-go:1".to_owned()));
        assert_eq!(drew("api:anthropic"), Ok("api:anthropic".to_owned()));
        // A /login to another Claude account in the same folder since the
        // session drew on the first: the one signed in there now.
        let claude = caller(Agent::ClaudeCode, None);
        let mut before = account(
            "claude:personal",
            Subscription::Claude,
            &[(Agent::ClaudeCode, ".claude")],
        );
        before.signed_in = false;
        let now = account(
            "claude:work",
            Subscription::Claude,
            &[(Agent::ClaudeCode, ".claude")],
        );
        let accounts = [before, now];
        let session = session(Agent::ClaudeCode, Some("claude:personal"));
        assert_eq!(
            yours(&claude, &folders, Some(&session), &accounts).map(|account| &account.id),
            Ok(&"claude:work".to_owned())
        );
    }
}
