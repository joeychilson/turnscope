//! How large sessions' contexts grew.
//!
//! A response's context is the prompt it was given, its input however it
//! was cached, as pricing measures it for a provider's long-context tier
//! ([`crate::price`]): for usage an agent counts over several model calls
//! together, one call's, the largest of Claude Code's iterations, the
//! average of a Grok Build turn's, or the latest request of an older Codex
//! running total. A session's largest context is its largest response's.
//! Its subagents' responses are theirs, each with a context of its own, and
//! usage outside the conversation is no response's.
//!
//! **Why it matters.** Every response sends its whole context again, most
//! of it read from the cache, so a context that grows is paid for on every
//! response after: what drives a long session's share of a limit more than
//! the work it was asked to do.

use std::collections::HashMap;

use rusqlite::{Connection, params_from_iter};

use crate::error::{Error, Result};
use crate::ledger::unsigned;
use crate::session::SessionKey;

/// How many sessions one statement asks about: far fewer than the values
/// SQLite takes in one, 32,766.
const AT_ONCE: usize = 500;

/// The largest context of each of `sessions` that has a response in the
/// cache, in tokens. A session with none, as one whose only usage is
/// outside the conversation, is left out: its context is unknown.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read, and
/// [`Error::Corrupt`] when a stored count is out of range.
pub(crate) fn largest(
    cache: &Connection,
    sessions: &[SessionKey],
) -> Result<HashMap<SessionKey, u64>> {
    let mut largest = HashMap::new();
    for chunk in sessions.chunks(AT_ONCE) {
        let marks = vec!["?"; chunk.len()].join(", ");
        let mut statement = cache.prepare(&format!(
            "SELECT s.key, max(u.prompt) FROM usage u JOIN session s ON s.id = u.session
             WHERE s.key IN ({marks}) AND u.kind = 'response' GROUP BY s.key"
        ))?;
        let mut rows = statement.query(params_from_iter(chunk.iter().map(ToString::to_string)))?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let key = SessionKey::parse(&key)
                .ok_or_else(|| Error::corrupt("session key", key.clone()))?;
            largest.insert(key, unsigned(row.get(1)?, "prompt")?);
        }
    }
    Ok(largest)
}
