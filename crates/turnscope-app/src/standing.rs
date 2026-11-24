//! How a limit or an account stands, which sets its color.

use serde::Serialize;
use turnscope::status::{Account, Limit, Outlook};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
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
}

impl AccountStanding for Account {
    fn standing(&self) -> Standing {
        self.deciding_limit()
            .map_or(Standing::Lasts, LimitStanding::standing)
    }
}
