//! Sessions, as usage counts them: each at the top of its tree, with its
//! subagents' usage counted in. A fork or a continuation is a session of its
//! own, with no parent.
//!
//! Of a session, Turnscope keeps its agent, its project, its title, the
//! session it's a subagent of, when it was last active, and its responses'
//! counts. Of what was said in it, only its title is kept: the first
//! request, where its agent gave it no title.

use rusqlite::{Connection, OptionalExtension};

use crate::time::MINUTE;
use crate::{Error, Result};

/// How recently a session, or one within it, was active to be running. One
/// active after now, by a clock ahead of this one, is running.
const RUNNING: i64 = 5 * MINUTE;

/// Each session at the top of its tree with the sessions in its tree, its
/// subagents, theirs, and so on, as `tree (root, id)`. A session whose
/// parent was never read is a top of its own.
pub(crate) const TREES: &str = "
WITH RECURSIVE tree (root, id) AS (
    SELECT id, id FROM session s
    WHERE parent IS NULL OR NOT EXISTS (SELECT 1 FROM session p WHERE p.id = s.parent)
    UNION ALL
    SELECT tree.root, s.id FROM session s JOIN tree ON s.parent = tree.id
)";

/// The session `?1` and its subagents, theirs, and so on, as `tree (id)`.
const SUBTREE: &str = "
WITH RECURSIVE tree (id) AS (
    SELECT ?1 UNION ALL
    SELECT s.id FROM session s JOIN tree ON s.parent = tree.id
)";

/// The project and agent of the session `id`, and whether it or a subagent
/// of it runs now: what the app lists of a session that used a limit, with
/// the title its usage by session gives.
pub fn brief(db: &Connection, id: &str, now: i64) -> Result<(Option<String>, String, bool)> {
    db.prepare_cached(&format!(
        "{SUBTREE} SELECT project, agent, (SELECT max(last) FROM session WHERE id IN tree)
         FROM session WHERE id = ?1"
    ))?
    .query_row([id], |row| {
        let last: Option<i64> = row.get(2)?;
        Ok((
            row.get(0)?,
            row.get(1)?,
            last.is_some_and(|last| now - last <= RUNNING),
        ))
    })
    .optional()?
    .ok_or_else(|| Error::NotFound(format!("no session is {id}")))
}

/// The account session `id` drew on last, when one is known.
pub fn account(db: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    db.query_row(
        "SELECT account FROM response WHERE session = ?1 AND account IS NOT NULL ORDER BY at DESC LIMIT 1",
        [id],
        |row| row.get(0),
    )
    .optional()
}
