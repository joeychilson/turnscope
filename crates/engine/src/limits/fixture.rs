//! Limits recorded as if read, for synthetic history: the engine's and the
//! MCP server's tests, and the README's screenshots, whose subscriptions no
//! provider could be asked about. Only with the `fixture` feature.

use std::path::PathBuf;

use crate::Engine;
use crate::agent::Agent;
use crate::error::Result;
use crate::folders;
use crate::limits::{self, AccountLimits, Subscription};
use crate::runtime::Change;
use crate::time::Instant;

impl Engine {
    /// Record `accounts`, each of `subscription`, as read at `at`: their
    /// labels, plans and agents, and each limit's use and window, as a read
    /// of their provider would, and each signed in at `at` where its
    /// [`AccountLimits::folders`] say, or else in its agents' own folders,
    /// as a look for sign-ins would find it. For synthetic
    /// history, whose subscriptions no provider could be asked about. What
    /// is worked out from readings and sign-ins, whether an account is in
    /// use, its pace and when it runs out, and which account each response
    /// drew on, is worked out from these as from any others, so an account
    /// read twice, its use risen between, is in use.
    ///
    /// # Errors
    ///
    /// Returns an error when the ledger cannot be written.
    pub fn record_fixture_limits(
        &self,
        subscription: Subscription,
        accounts: &[AccountLimits],
        at: Instant,
    ) -> Result<()> {
        let reads: Vec<limits::AccountRead> = accounts
            .iter()
            .map(|account| limits::AccountRead {
                id: account.id.clone(),
                label: account.label.clone(),
                plan: account.plan.clone(),
                agents: account.agents.clone(),
                // A limit whose share is unknown has no reading to record.
                limits: Ok(account
                    .limits
                    .iter()
                    .filter_map(|limit| {
                        Some(limits::Reported {
                            key: limit.key.clone(),
                            name: limit.name.clone(),
                            scope: limit.scope.clone(),
                            used: limit.used?,
                            starts: limit.starts,
                            resets: limit.resets,
                        })
                    })
                    .collect()),
            })
            .collect();
        let seen: Vec<limits::Seen> = accounts
            .iter()
            .flat_map(|account| {
                // Where it says it is signed in, or else in each of its
                // agents' own folders.
                let signed: Vec<(Agent, PathBuf)> = if account.folders.is_empty() {
                    account
                        .agents
                        .iter()
                        .map(|agent| (*agent, folders::own(*agent, &self.home)))
                        .collect()
                } else {
                    account.folders.clone()
                };
                signed.into_iter().flat_map(move |(agent, folder)| {
                    limits::attribution::places(subscription, agent, &folder)
                        .into_iter()
                        .map(|place| limits::Seen {
                            place,
                            held: limits::Held::Account(account.id.clone()),
                        })
                })
            })
            .collect();
        let mut ledger = self.writing()?;
        ledger.record_limits(subscription, &reads, at)?;
        if ledger.record_sign_ins(&seen, at)? {
            self.catch_up(&mut ledger)?;
        }
        drop(ledger);
        self.publish(Change::Limits);
        Ok(())
    }
}
