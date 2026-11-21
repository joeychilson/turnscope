//! The engine's status, as the app reads it: the engine's own types, with what the app adds
//! to them: how a limit or an account stands, which sets its color.

pub use turnscope::status::{
    Account, Agent, Connection, Horizon, Limit, Moment, Outlook, Spend, Stale, State, Status,
    Unknown,
};

/// How a limit stands, from calmest to most urgent. It sets the color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Lasts,
    RunningOut,
    UsedUp,
}

pub trait LimitStanding {
    fn standing(&self) -> Standing;
}

impl LimitStanding for Limit {
    fn standing(&self) -> Standing {
        match self.outlook {
            Outlook::UsedUp { .. } => Standing::UsedUp,
            Outlook::RunsOut { .. } => Standing::RunningOut,
            Outlook::Lasts { .. } | Outlook::Unknown { .. } => Standing::Lasts,
        }
    }
}

pub trait AccountStanding {
    fn standing(&self) -> Standing;
    fn refilled_at(&self) -> Option<i64>;
    fn twinned<'a>(&self, among: impl IntoIterator<Item = &'a Account>) -> bool;
}

impl AccountStanding for Account {
    fn standing(&self) -> Standing {
        self.deciding_limit()
            .map_or(Standing::Lasts, LimitStanding::standing)
    }

    fn refilled_at(&self) -> Option<i64> {
        self.deciding_limit()
            .filter(|l| l.left_percent.is_some())
            .and_then(|l| l.reset_since)
    }

    fn twinned<'a>(&self, among: impl IntoIterator<Item = &'a Account>) -> bool {
        among
            .into_iter()
            .any(|a| a.id != self.id && !a.hidden && a.title == self.title)
    }
}
