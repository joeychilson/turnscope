//! `turnscope guard`: a limit as a verdict, for a hook to turn into an exit
//! code.
//!
//! A hook asks whether the caller's account's limit is under a percent
//! left, as `check_limits` with `below` answers it. Under it, the call the
//! hook guards is refused, with why. Anything else lets the call go on: a
//! limit known not to be under it, and every limit whose standing isn't
//! known, such as one whose reading is stale, one no account has, or an
//! account that can't be told. A guard never blocks on missing data, since
//! a guard that fails closed would stop an agent for want of a reading, and
//! says in a note what it couldn't tell.

use crate::accounts;
use crate::limits::{self, Status};
use crate::prose;
use crate::tools::{Failure, Server};

/// What a guard says of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Guard {
    /// Let it go on, with a note of what couldn't be told, if anything.
    Pass(Option<String>),
    /// Refuse it: the limit is under the percent, as this says.
    Block(String),
}

impl Guard {
    /// The code a hook exits with: 2 to refuse the call, which a Claude Code
    /// `PreToolUse` hook exiting so does, showing the model what it wrote to
    /// standard error; 0 to let it go on.
    pub fn exit_code(&self) -> u8 {
        match self {
            Guard::Pass(_) => 0,
            Guard::Block(_) => 2,
        }
    }
}

/// Whether the limit `limit` of `account`, or of the caller's account, is
/// under `below` percent left, as a verdict on the call a hook guards.
///
/// # Errors
///
/// Returns a failure when `below` isn't more than 0 and at most 100, or
/// Turnscope's data can't be read.
pub fn guard(
    server: &Server,
    limit: &str,
    below: f64,
    account: Option<&str>,
) -> Result<Guard, Failure> {
    if !(below > 0.0 && below <= 100.0) {
        return Err(Failure("--below takes more than 0 and at most 100.".into()));
    }
    server.bring_up_to_date()?;
    let (all, _) = server.accounts()?;
    let chosen = match account {
        Some(asked) => match accounts::every_named(asked, &all) {
            Ok(named) => named,
            Err(why) => return Ok(Guard::Pass(Some(why.0))),
        },
        None => match accounts::asking(server, &all)?.1 {
            Ok(yours) => vec![yours],
            Err(why) => return Ok(Guard::Pass(Some(why))),
        },
    };
    let watched: Vec<_> = chosen
        .iter()
        .flat_map(|account| {
            account
                .limits
                .iter()
                .filter(|state| limits::matches(state, limit))
                .map(move |state| (*account, state))
        })
        .collect();
    if watched.is_empty() {
        return Ok(Guard::Pass(Some(
            limits::no_limit(limit, chosen.iter().copied()).0,
        )));
    }
    let now = turnscope_engine::Instant::now();
    let mut unknown = Vec::new();
    for (account, state) in watched {
        let name = format!("{}, {}", accounts::name(account), limits::spoken(state));
        match limits::status(account, state, below, now) {
            Status::Under => {
                let left = state.left().unwrap_or(0.0);
                let resets = state.resets.map_or_else(String::new, |at| {
                    format!(" It resets {}.", server.clock(at))
                });
                return Ok(Guard::Block(format!(
                    "{name}: {} left, under {}.{resets}",
                    prose::left(left),
                    prose::percent(below)
                )));
            }
            Status::NotUnder => {}
            Status::Unknown => unknown.push(name),
        }
    }
    Ok(Guard::Pass((!unknown.is_empty()).then(|| {
        format!(
            "Whether {} is under {} left isn't known now, so the call goes on.",
            prose::list(&unknown),
            prose::percent(below)
        )
    })))
}
