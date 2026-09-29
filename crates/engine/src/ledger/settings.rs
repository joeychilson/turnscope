//! The person's choices: which accounts are hidden. They are what the
//! person said, not anything read or worked out, so they live in the ledger
//! beside what was observed.

use std::collections::HashSet;

use rusqlite::params;

use super::Ledger;
use crate::error::Result;

impl Ledger {
    /// Every account the person hid, by its id.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Ledger`] when the ledger cannot be read.
    pub(crate) fn hidden_accounts(&self) -> Result<HashSet<String>> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT account FROM account_setting WHERE hidden")?;
        let hidden = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<HashSet<String>>>()?;
        Ok(hidden)
    }

    /// Hide `account`, or show it again.
    ///
    /// # Errors
    ///
    /// Returns [`crate::Error::Ledger`] when the ledger cannot be written.
    pub(crate) fn set_hidden(&mut self, account: &str, hidden: bool) -> Result<()> {
        self.connection.execute(
            "INSERT INTO account_setting (account, hidden) VALUES (?1, ?2)
             ON CONFLICT (account) DO UPDATE SET hidden = excluded.hidden",
            params![account, hidden],
        )?;
        Ok(())
    }
}
