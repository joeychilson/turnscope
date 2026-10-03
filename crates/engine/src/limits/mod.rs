//! Subscription limits, read with the sign-ins the agents already keep.
//!
//! A subscription is not an agent: one ChatGPT plan can be signed into from
//! Codex, OpenCode and Pi at once. So each subscription lists every place a
//! sign-in to it can be kept, looks for it in each of the agents' folders
//! ([`crate::Folder`]), groups the sign-ins it finds by the account they
//! belong to, and reads each account with whichever of its sign-ins works. The
//! same account in several agents is one account; different accounts of one
//! subscription stand apart, as a second Claude account kept in a folder of
//! its own, `CLAUDE_CONFIG_DIR`, is.
//!
//! An account stays once found. Signed out of every agent, as when an agent
//! that holds one sign-in is switched to another account, it keeps its last
//! limits, which refill as their windows reset, until an agent is signed into
//! it again.
//!
//! Sign-ins are only ever read. Renewing one would rotate the token its app
//! holds and sign that app out, so an expired sign-in waits for its app to
//! renew it.
//!
//! **Whose use.** Each read of a subscription also records what every place
//! a sign-in to it is kept held, and what an agent used drew on the account
//! signed in where and when it was made ([`attribution`]). That use is what
//! makes an account in use, and what a limit's rises are shared among
//! ([`share`]).
//!
//! **API keys.** Use no subscription covers is an account too, one for each
//! provider, [`Subscription::ApiKey`], whose limits are those its keys carry
//! of their own ([`api`]). Whether the person hid an account is kept in the
//! ledger and given with the account.
//!
//! Every reading is kept in the ledger, so how fast a limit is rising is
//! known as soon as the app opens, and alerts are sent once per window however
//! often the app restarts.
//!
//! **Alerts** are worked out on each new reading: a limit projected to run
//! out before it resets, one reached, and one back, which is news only when
//! the last alert about it said it was reached. Beside them go milestones of
//! a week or a month: each quarter of it used, and a subscription's that
//! resets within a day with more than half of it left. An account's first
//! reading, as every account's is when Turnscope first runs, says only that
//! a limit runs out or ran out: how much is left is where it stands, not
//! news, and its milestones are kept as told. So is the weekly
//! recap ([`recap`]): from Monday morning, how each account's week that
//! ended went. Only the process that keeps the data directory works them
//! out, the app, which shows them: limits another process reads, as an MCP
//! server does while no app runs, are recorded without them, so no alert is
//! recorded as sent that no one saw.
//!
//! The tests read answers shaped like the providers', never new requests to
//! them.

pub(crate) mod api;
pub(crate) mod attribution;
mod chatgpt;
mod claude;
mod grok;
mod history;
mod opencode;
mod openrouter;
mod pace;
pub(crate) mod recap;
pub(crate) mod share;
pub use history::PastWindow;
pub use recap::WeekEnded;
pub use share::{LimitTrack, LimitWindow};
mod sign_in;

pub(crate) use self::attribution::{Held, Place, Seen, Span, Timeline};

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde_json::Value;

use self::sign_in::{FNV, accounts, claims, fnv, sign_ins};
pub(crate) use self::sign_in::{fingerprint, stamp};
use crate::agent::Agent;
use crate::error::Result;
use crate::folders::Folder;
use crate::ledger::{Ledger, Reading};
use crate::net::{self, Destination, NetError};
use crate::time::Instant;

/// How near two readings' resets can be and still name one window: providers
/// that give a countdown rather than an instant move it by a few seconds from
/// read to read.
pub(crate) const SAME_WINDOW: i64 = 5 * 60 * 1000;

/// An hour, a day and a week, in milliseconds.
const HOUR: i64 = 60 * 60 * 1000;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// How soon after a window ends that a limit of an account signed in nowhere
/// is back is still news: an hour.
const BACK_WITHIN: i64 = HOUR;

/// How recently an agent must have used a provider for an account signed
/// into it to be in use: half an hour.
const IN_USE: i64 = 30 * 60 * 1000;

/// How recent a reading must be to say how much of a limit is left as news:
/// half an hour. Readings are taken every five minutes while an account is
/// signed in.
const FRESH: i64 = 30 * 60 * 1000;

/// An account's limits, as the panel, the menu bar and the MCP server give
/// them.
#[derive(Clone, Debug, PartialEq)]
pub struct AccountLimits {
    /// The account's stable id.
    pub id: String,
    /// Its subscription.
    pub subscription: Subscription,
    /// What tells it apart from other accounts of the subscription, such as
    /// an email address.
    pub label: Option<String>,
    /// Its plan, where known.
    pub plan: Option<String>,
    /// The agents signed into it, in the order the subscription lists them;
    /// signed in nowhere now, those it was last found in.
    pub agents: Vec<Agent>,
    /// Whether an agent on this Mac is signed into it now. One that isn't
    /// keeps its last limits, refilled as their windows reset.
    pub signed_in: bool,
    /// Its limits, as last read.
    pub limits: Vec<LimitState>,
    /// When its limits were last read.
    pub read_at: Option<Instant>,
    /// When it was last read or tried, whether or not that worked: later
    /// than `read_at` when the latest try failed. For an account signed in
    /// nowhere, and for an API-key account whose keys carry no limit read,
    /// when it was last looked for.
    pub checked_at: Option<Instant>,
    /// Why the latest read of it failed, if it did; the last limits read
    /// stand. `None` for an account signed in nowhere, which isn't read.
    pub problem: Option<LimitProblem>,
    /// Whether it is in use: a response of the last half hour drew on it,
    /// made where and when it was signed in, or its limits rose between the
    /// last reads, as use elsewhere does.
    pub in_use: bool,
    /// Whether the person hid it. A hidden account is still given, for the
    /// interface to leave out, and gives rise to no alerts.
    pub hidden: bool,
    /// For an API-key account, [`Subscription::ApiKey`], the provider whose
    /// API it is, as its agents' usage names it, such as `anthropic` or
    /// `openrouter`; `None` for a subscription's.
    pub provider: Option<String>,
    /// Where it is signed in: each agent and the folder of its
    /// ([`crate::Folder`]) that last held a sign-in to it, by agent and then
    /// folder; signed in nowhere now, where it last was. None for an API-key
    /// account, whose use comes from wherever a key is.
    pub folders: Vec<(Agent, PathBuf)>,
}

/// One limit, as last read, with how fast it is rising.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitState {
    /// Its stable key within the account.
    pub key: String,
    /// What to call it, such as `5 hours` or `Weekly`.
    pub name: String,
    /// The one model it applies to, when not all.
    pub scope: Option<String>,
    /// How much is used, in percent, as last read: `None` when its window
    /// has reset since, so that how much of the new one is used is unknown
    /// until it is read again ([`LimitState::refilled`] says why).
    pub used: Option<f64>,
    /// When its window began, when known.
    pub starts: Option<Instant>,
    /// When its window resets, when known.
    pub resets: Option<Instant>,
    /// When it was read.
    pub read_at: Instant,
    /// How fast it is rising, in percentage points an hour: over the hour
    /// up to its latest reading for a window of a day or less, once readings
    /// span fifteen minutes of it; for a longer one, over the day, or its
    /// average since it began while readings cover too little of the day
    /// (`limits::pace`).
    pub pace: Option<f64>,
    /// Whether its window has reset since it was read, so it is full again.
    pub refilled: bool,
}

impl LimitState {
    /// When it reaches `share` percent at its pace, as of its latest reading:
    /// `None` when it isn't rising, is there already, or would take beyond a
    /// thousand years.
    pub fn reaches(&self, share: f64) -> Option<Instant> {
        pace::reaches(self.used?, self.pace?, self.read_at, share)
    }

    /// How much of it is left, in percent: what its provider says is used,
    /// taken from the whole. `None` when its window has reset since it was
    /// read, until it is read again.
    pub fn left(&self) -> Option<f64> {
        self.used.map(|used| (100.0 - used).clamp(0.0, 100.0))
    }

    /// How far it is from an even pace, in points, as of its latest reading:
    /// the share of its window gone by then, as a percent, less the percent
    /// used. Above zero is in reserve, what spending the rest of it evenly
    /// until it resets has to spare; below, used ahead of an even pace.
    /// `None` when its window or how much is used isn't known.
    ///
    /// Unlike its outlook, it needs no pace: it holds from the first reading
    /// and doesn't swing with an hour of use.
    pub fn reserve(&self) -> Option<f64> {
        let (starts, resets) = (self.starts?, self.resets?);
        let length = resets.millis().saturating_sub(starts.millis());
        if length <= 0 {
            return None;
        }
        let gone = self.read_at.millis().saturating_sub(starts.millis()) as f64 / length as f64;
        Some(gone.clamp(0.0, 1.0) * 100.0 - self.used?)
    }

    /// The fastest it can rise and still last until it resets, in points an
    /// hour, as of its latest reading: what is left over the time to its
    /// reset. `None` when its reset or how much is used isn't known, or its
    /// reset is past.
    pub fn lasting_pace(&self) -> Option<f64> {
        let millis = self.resets?.millis().saturating_sub(self.read_at.millis());
        if millis <= 0 {
            return None;
        }
        Some(self.left()? / (millis as f64 / HOUR as f64))
    }

    /// When it runs out at its pace, if it is rising.
    fn runs_out_at(&self) -> Option<Instant> {
        self.reaches(100.0)
    }

    /// How it stands: the one rule the app, the MCP server and alerts share.
    ///
    /// It runs out only when its pace has it run out a meaningful while
    /// before it resets, a fiftieth of its window and a quarter of an hour
    /// at least: a forecast is a guide, and one that has a week run out ten
    /// minutes before its reset says nothing worth hurrying for.
    pub fn standing(&self) -> Standing {
        if self.used.is_some_and(|used| used >= 100.0) {
            return Standing::UsedUp;
        }
        match (self.runs_out_at(), self.resets) {
            (Some(runs_out), Some(resets))
                if runs_out.millis() < resets.millis().saturating_sub(margin(self)) =>
            {
                Standing::RunningOut
            }
            // With no reset to run out before, as a key's lifetime limit,
            // only running out within a day is worth hurrying for: a trickle
            // that empties it in months isn't.
            (Some(runs_out), None)
                if runs_out.millis().saturating_sub(self.read_at.millis()) <= DAY =>
            {
                Standing::RunningOut
            }
            _ => Standing::Lasts,
        }
    }

    /// Where its recent pace leads: when it runs out, while it is running
    /// out, or else how much is left at the reset. Neither while its pace
    /// isn't known or once it is used up.
    pub fn outlook(&self) -> Outlook {
        let none = Outlook {
            runs_out: None,
            left_at_reset: None,
        };
        let (Some(used), Some(pace)) = (self.used, self.pace) else {
            return none;
        };
        match self.standing() {
            Standing::UsedUp => return none,
            Standing::RunningOut => {
                return Outlook {
                    runs_out: self.runs_out_at(),
                    ..none
                };
            }
            Standing::Lasts => {}
        }
        let Some(resets) = self.resets else {
            return Outlook {
                left_at_reset: (pace <= 0.0).then_some(100.0 - used),
                ..none
            };
        };
        let hours =
            resets.millis().saturating_sub(self.read_at.millis()).max(0) as f64 / HOUR as f64;
        Outlook {
            left_at_reset: Some((100.0 - used - pace.max(0.0) * hours).clamp(0.0, 100.0)),
            ..none
        }
    }
}

/// How a limit stands, from calmest to most urgent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    /// At its pace, it lasts until it resets.
    Lasts,
    /// At its pace, it runs out a meaningful while before it resets.
    RunningOut,
    /// It is used up.
    UsedUp,
}

/// Where a limit's recent pace leads ([`LimitState::outlook`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outlook {
    /// When it runs out, while it is running out.
    pub runs_out: Option<Instant>,
    /// How much will be left at the reset, in percent, while it lasts.
    pub left_at_reset: Option<f64>,
}

/// How much earlier than its reset a limit must run out to be running out:
/// a fiftieth of its window, and a quarter of an hour at least. That is 3.4
/// hours of a week and 15 minutes of five hours.
fn margin(limit: &LimitState) -> i64 {
    const LEAST: i64 = 15 * 60 * 1000;
    let window = limit
        .starts
        .zip(limit.resets)
        .map_or(0, |(starts, resets)| {
            resets.millis().saturating_sub(starts.millis())
        });
    (window / 50).max(LEAST)
}

impl AccountLimits {
    /// The limit that matters most now: of its limits on all its usage, or
    /// of every limit when it has none on all of it, one used up, then the
    /// one that runs out soonest, then the one with least left at its reset,
    /// or now. `None` when no limit's share is known.
    pub fn deciding(&self) -> Option<&LimitState> {
        let known = |limit: &&LimitState| limit.used.is_some();
        let whole: Vec<&LimitState> = self
            .limits
            .iter()
            .filter(known)
            .filter(|limit| limit.scope.is_none())
            .collect();
        let candidates = if whole.is_empty() {
            self.limits.iter().filter(known).collect()
        } else {
            whole
        };
        candidates.into_iter().min_by(|a, b| {
            let rank = |limit: &LimitState| {
                let outlook = limit.outlook();
                (
                    Reverse(limit.standing()),
                    outlook.runs_out.map_or(i64::MAX, Instant::millis),
                )
            };
            let least = |limit: &LimitState| {
                limit
                    .outlook()
                    .left_at_reset
                    .or(limit.left())
                    .unwrap_or(100.0)
            };
            rank(a).cmp(&rank(b)).then(least(a).total_cmp(&least(b)))
        })
    }

    /// How the account stands: as the limit that matters most does
    /// ([`AccountLimits::deciding`]), so its color and its headline never
    /// disagree. A limit on one model alone, used up, stops only that model,
    /// and says so on its own line.
    pub fn standing(&self) -> Standing {
        self.deciding()
            .map_or(Standing::Lasts, LimitState::standing)
    }

    /// What the account is called before anything tells it apart, the one
    /// name the app and the MCP server both call it by: its subscription and
    /// plan, "Claude Max", or its provider's API, "OpenRouter API key". What
    /// tells two of one subscription apart is its [`AccountLimits::label`].
    pub fn title(&self) -> String {
        if self.subscription == Subscription::ApiKey {
            return self.provider.as_deref().map_or_else(
                || "API key".to_owned(),
                |provider| format!("{} API key", provider_name(provider)),
            );
        }
        let mut title = self.subscription.name().to_owned();
        // A plan the subscription is named for, as OpenCode Go's `go`, is
        // said once.
        if let Some(plan) = self.plan.as_deref().filter(|plan| {
            !plan.is_empty() && !title.to_lowercase().ends_with(&plan.to_lowercase())
        }) {
            title.push(' ');
            title.push_str(&plan_name(plan));
        }
        title
    }
}

/// What a provider is called, from the id its agents' usage names it by:
/// "anthropic" is Anthropic. An id not known here has its words
/// capitalized.
fn provider_name(id: &str) -> String {
    let known = match id {
        "anthropic" => "Anthropic",
        "openai" => "OpenAI",
        "openrouter" => "OpenRouter",
        "xai" => "xAI",
        "google" => "Google",
        "opencode" => "OpenCode",
        "opencode-go" => "OpenCode Go",
        "deepseek" => "DeepSeek",
        "moonshotai" => "Moonshot AI",
        "zai" => "Z.ai",
        "togetherai" => "Together AI",
        "fireworks-ai" => "Fireworks AI",
        "github-copilot" => "GitHub Copilot",
        "amazon-bedrock" => "Amazon Bedrock",
        "huggingface" => "Hugging Face",
        _ => return titled(id),
    };
    known.to_owned()
}

/// `key`'s words, however it joins them, each begun with a capital:
/// `super_heavy` is Super Heavy.
fn titled(key: &str) -> String {
    key.split(['_', '-', ' '])
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut letters = word.chars();
            letters
                .next()
                .map(|first| first.to_uppercase().chain(letters).collect())
                .unwrap_or_default()
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// What an alert says of a limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlertKind {
    /// At the pace it is rising, it runs out before its window resets.
    RunningOut,
    /// It is used up.
    Reached,
    /// Its window has reset since it was used up, or, for a week or a
    /// month, since it was running out.
    Available,
    /// A week or a month of a subscription resets within a day with more
    /// than half of it left.
    Unused,
    /// A week or a month is down to three quarters of it left.
    ThreeQuartersLeft,
    /// A week or a month is down to half of it left.
    HalfLeft,
    /// A week or a month is down to a quarter of it left.
    QuarterLeft,
}

impl AlertKind {
    /// Every kind.
    const ALL: [AlertKind; 7] = [
        AlertKind::RunningOut,
        AlertKind::Reached,
        AlertKind::Available,
        AlertKind::Unused,
        AlertKind::ThreeQuartersLeft,
        AlertKind::HalfLeft,
        AlertKind::QuarterLeft,
    ];

    /// The kind as stored.
    pub(crate) fn key(self) -> &'static str {
        match self {
            AlertKind::RunningOut => "running-out",
            AlertKind::Reached => "reached",
            AlertKind::Available => "available",
            AlertKind::Unused => "unused",
            AlertKind::ThreeQuartersLeft => "left-75",
            AlertKind::HalfLeft => "left-50",
            AlertKind::QuarterLeft => "left-25",
        }
    }

    /// The kind with `key`.
    pub(crate) fn from_key(key: &str) -> Option<AlertKind> {
        AlertKind::ALL.into_iter().find(|kind| kind.key() == key)
    }

    /// Whether it says how much of a week or a month is left, a quarter
    /// at a time.
    fn quarter(self) -> bool {
        matches!(
            self,
            AlertKind::ThreeQuartersLeft | AlertKind::HalfLeft | AlertKind::QuarterLeft
        )
    }

    /// Whether it says a limit runs out, ran out or is back, which decides
    /// whether a reset is news; the milestones said beside them don't.
    pub(crate) fn of_running_out(self) -> bool {
        matches!(
            self,
            AlertKind::RunningOut | AlertKind::Reached | AlertKind::Available
        )
    }
}

/// An alert about a limit, sent once per window.
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    /// The account.
    pub account: String,
    /// Its subscription.
    pub subscription: Subscription,
    /// What tells the account apart from others of the subscription.
    pub label: Option<String>,
    /// The limit's name.
    pub limit: String,
    /// The one model the limit applies to, when not all.
    pub scope: Option<String>,
    /// What the alert says.
    pub kind: AlertKind,
    /// When it runs out, for [`AlertKind::RunningOut`]; when it resets, for
    /// [`AlertKind::Reached`], [`AlertKind::Unused`] and the quarters left.
    pub when: Option<Instant>,
    /// How much of it was used, in percent, by the reading the alert was
    /// worked out from; `None` where that is unknown, as for a window that
    /// has reset since.
    pub used: Option<f64>,
}

/// A subscription whose limits Turnscope reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Subscription {
    /// A Claude plan.
    Claude,
    /// A ChatGPT plan, as Codex and other agents use it.
    ChatGpt,
    /// A SuperGrok plan.
    SuperGrok,
    /// An OpenCode Go plan.
    OpenCodeGo,
    /// No plan: a provider's API, paid for as it is used, with a key. Each
    /// provider's is an account of its own ([`AccountLimits::provider`]),
    /// whose limits are those its keys carry of their own.
    ApiKey,
}

impl Subscription {
    /// Every subscription.
    pub(crate) const ALL: [Subscription; 5] = [
        Subscription::Claude,
        Subscription::ChatGpt,
        Subscription::SuperGrok,
        Subscription::OpenCodeGo,
        Subscription::ApiKey,
    ];

    /// The subscription's stable key.
    pub fn key(self) -> &'static str {
        match self {
            Subscription::Claude => "claude",
            Subscription::ChatGpt => "chatgpt",
            Subscription::SuperGrok => "supergrok",
            Subscription::OpenCodeGo => "opencode-go",
            Subscription::ApiKey => "api",
        }
    }

    /// The id of its account `key`: the subscription's key, a colon, and
    /// the account's, as the ledger keeps accounts by.
    pub(crate) fn account_id(self, key: &str) -> String {
        format!("{}:{key}", self.key())
    }

    /// The subscription with `key`.
    pub(crate) fn from_key(key: &str) -> Option<Subscription> {
        Subscription::ALL
            .into_iter()
            .find(|subscription| subscription.key() == key)
    }

    /// The subscription's name, as people know it.
    pub fn name(self) -> &'static str {
        match self {
            Subscription::Claude => "Claude",
            Subscription::ChatGpt => "ChatGPT",
            Subscription::SuperGrok => "SuperGrok",
            Subscription::OpenCodeGo => "OpenCode Go",
            Subscription::ApiKey => "API key",
        }
    }

    /// The providers, by the catalog's id for each, whose usage draws on
    /// the subscription, when an agent signed into it asked them; none for
    /// [`Subscription::ApiKey`], whose accounts are each of one provider
    /// ([`AccountLimits::provider`]).
    pub fn providers(self) -> &'static [&'static str] {
        match self {
            Subscription::Claude => &["anthropic"],
            Subscription::ChatGpt => &["openai"],
            Subscription::SuperGrok => &["xai"],
            Subscription::OpenCodeGo => &["opencode-go"],
            Subscription::ApiKey => &[],
        }
    }

    /// How the subscription is read.
    fn reader(self) -> Reader {
        match self {
            Subscription::Claude => claude::READER,
            Subscription::ChatGpt => chatgpt::READER,
            Subscription::SuperGrok => grok::READER,
            Subscription::OpenCodeGo => opencode::READER,
            Subscription::ApiKey => api::READER,
        }
    }
}

/// What a plan is called, from the key its provider reports it by, as
/// [`AccountLimits::plan`] holds it: `max` is Max, `prolite` Pro Lite, in any
/// case. A key not known here is called by its words, however it joins
/// them, each begun with a capital: `super_heavy` is Super Heavy.
fn plan_name(key: &str) -> String {
    let known = match key.to_ascii_lowercase().as_str() {
        "prolite" | "pro_lite" | "pro-lite" => Some("Pro Lite"),
        "max" => Some("Max"),
        "pro" => Some("Pro"),
        "plus" => Some("Plus"),
        "team" => Some("Team"),
        "business" => Some("Business"),
        "enterprise" => Some("Enterprise"),
        "free" => Some("Free"),
        "heavy" => Some("Heavy"),
        _ => None,
    };
    known.map_or_else(|| titled(key), str::to_owned)
}

/// Why an account's limits could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitProblem {
    /// Its sign-ins were refused, or have expired until their agent renews
    /// them.
    SignIn,
    /// The provider could not be reached, or was busy.
    Unavailable,
    /// The provider answered in a way this version does not understand.
    Unrecognized,
    /// The request could not be made on this Mac, since `/usr/bin/curl`
    /// could not be run: no fault of the provider's.
    Unsent,
}

impl LimitProblem {
    /// The problem as stored.
    pub(crate) fn key(self) -> &'static str {
        match self {
            LimitProblem::SignIn => "sign-in",
            LimitProblem::Unavailable => "unavailable",
            LimitProblem::Unrecognized => "unrecognized",
            LimitProblem::Unsent => "unsent",
        }
    }

    /// The problem with `key`.
    pub(crate) fn from_key(key: &str) -> Option<LimitProblem> {
        [
            LimitProblem::SignIn,
            LimitProblem::Unavailable,
            LimitProblem::Unrecognized,
            LimitProblem::Unsent,
        ]
        .into_iter()
        .find(|problem| problem.key() == key)
    }

    /// Why a request for usage got no answer, as a problem of the account's.
    fn of(error: &NetError) -> LimitProblem {
        match error {
            NetError::Spawn(_) => LimitProblem::Unsent,
            // A sign-in that would add headers of its own is refused before
            // it is sent, as the provider would refuse it.
            NetError::Header => LimitProblem::SignIn,
            // curl ran and got no answer, as offline, or when the provider
            // is down.
            NetError::Failed { .. } => LimitProblem::Unavailable,
            NetError::Answer(_) => LimitProblem::Unrecognized,
        }
    }
}

/// One limit, as a provider reported it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reported {
    /// The limit's stable key within its account, such as `five_hour`.
    pub key: String,
    /// What to call it, such as `5 hours` or `Weekly`.
    pub name: String,
    /// The one model it applies to, when it does not apply to all.
    pub scope: Option<String>,
    /// How much of it is used, in percent. Above 100 once exceeded.
    pub used: f64,
    /// When its window began, when the provider says or implies it.
    pub starts: Option<Instant>,
    /// When its window resets.
    pub resets: Option<Instant>,
}

/// An account and what was read of it.
#[derive(Clone, Debug)]
pub(crate) struct AccountRead {
    /// The account's stable id: its subscription and the identity its tokens
    /// share.
    pub id: String,
    /// What tells it apart from other accounts of the subscription.
    pub label: Option<String>,
    /// Its plan, where known.
    pub plan: Option<String>,
    /// The agents signed into it.
    pub agents: Vec<Agent>,
    /// Its limits, or why they could not be read.
    pub limits: Result<Vec<Reported>, LimitProblem>,
}

/// How one subscription is read.
struct Reader {
    /// Every place a sign-in to it can be kept.
    sources: &'static [Source],
    /// The account a token belongs to.
    identity: fn(&str) -> Identity,
    /// Ask the provider for an account's limits with one of its tokens.
    fetch: fn(&str, &Identity, Instant) -> Result<PlanLimits, LimitProblem>,
}

/// One place an agent keeps a sign-in to a subscription, in each of its
/// folders.
struct Source {
    /// The agent.
    agent: Agent,
    /// The provider the agent's usage names when it uses the sign-in, as
    /// Pi's names `openai-codex` for its ChatGPT sign-in: what tells its use
    /// of the subscription from its use of an API key.
    provider: &'static str,
    /// Where it keeps it in a folder of its.
    location: Location,
    /// JSON pointer to the token.
    token: &'static str,
    /// JSON pointer to when the token expires, where the agent records it.
    expires: Option<&'static str>,
    /// JSON pointer to the plan, where the agent records it.
    plan: Option<&'static str>,
    /// Where the agent records whose sign-in it holds, for tokens that don't
    /// say.
    whose: Option<Whose>,
}

/// Where an agent records the account it is signed into, beside the
/// sign-in itself.
struct Whose {
    /// The JSON file it records it in for a folder of its, given the home
    /// directory.
    file: fn(&Folder, &Path) -> PathBuf,
    /// JSON pointers to what identifies the account, joined in order; any
    /// the file leaves out are left out.
    id: &'static [&'static str],
    /// JSON pointer to what tells the account apart, such as its email
    /// address.
    label: &'static str,
}

/// Where an app keeps a sign-in, in a folder of its.
enum Location {
    /// A JSON file in the folder.
    File(&'static str),
    /// The login Keychain item, its password JSON, Claude Code keeps its
    /// sign-in for the folder in, named as [`claude::service`] names it.
    ClaudeKeychain,
    /// OpenCode's database in the folder, which keeps a row in `credential`
    /// for each sign-in to the provider named, its `value` JSON. Of several
    /// to one provider, OpenCode uses those marked `active`, and so do
    /// limits.
    OpenCode(&'static str),
}

/// A sign-in found on this Mac.
struct SignIn {
    agent: Agent,
    /// The agent's folder it was found in.
    folder: PathBuf,
    /// The provider the agent's usage names when it uses it.
    provider: &'static str,
    token: String,
    expires: Option<Instant>,
    plan: Option<String>,
    /// Whose it is, where the agent records it.
    whose: Option<Identity>,
}

/// The account a token belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    /// The same for every token to the account; empty when tokens do not say.
    key: String,
    /// What tells the account apart from others of the subscription.
    label: Option<String>,
}

/// The account of a token that doesn't say whose it is: the same for every
/// such token, as for Claude's without Claude Code's record of whose they
/// are, and for a provider's keys, which draw on its one API-key account.
fn one_account(_token: &str) -> Identity {
    Identity {
        key: String::new(),
        label: None,
    }
}

/// What a provider answered for one account: its plan, when the answer
/// names one, and the limits on it.
struct PlanLimits {
    plan: Option<String>,
    limits: Vec<Reported>,
}

/// An instant a sign-in or answer gives as RFC 3339 text, or as Unix seconds
/// or milliseconds, told apart by size.
fn instant_of(value: &Value) -> Option<Instant> {
    match value {
        Value::String(text) => Instant::parse(text),
        Value::Number(number) => {
            let number = number.as_i64()?;
            // Seconds since the epoch pass ten billion only in the year 2286.
            if number.unsigned_abs() < 10_000_000_000 {
                Instant::from_seconds(number)
            } else {
                Instant::from_millis(number)
            }
        }
        _ => None,
    }
}

/// Read every account of `subscription` signed in on this Mac, in any of
/// the agents' `folders`, under `home`, and say what each place a sign-in
/// to it is kept held ([`attribution`]).
///
/// An account's sign-ins are tried freshest first, and one known to have
/// expired is never sent. A sign-in the provider refuses gives way to the
/// next; any other failure is the account's answer.
///
/// # Errors
///
/// Returns an error, having asked no provider, when a place that keeps
/// sign-ins to `subscription` is there but can't be read.
pub(crate) fn read_subscription(
    subscription: Subscription,
    folders: &[Folder],
    home: &Path,
    now: Instant,
) -> Result<(Vec<AccountRead>, Vec<Seen>)> {
    let reader = subscription.reader();
    let found = sign_ins(subscription, folders, home)?;
    // Keys are read each for its own limit, as their provider's account,
    // and what each place held is told apart from sign-ins by its kind.
    if subscription == Subscription::ApiKey {
        let seen = api::seen(folders)?;
        let identity = (reader.identity)("");
        let reads = api::read(&found, now, |key, now| (reader.fetch)(key, &identity, now));
        return Ok((reads, seen));
    }
    let seen = attribution::seen(subscription, &reader, folders, &found);
    let reads = accounts(&reader, found)
        .into_iter()
        .map(|(identity, mut held)| {
            // Each agent once, in the order the subscription lists them.
            let mut agents: Vec<Agent> = Vec::new();
            for sign_in in &held {
                if !agents.contains(&sign_in.agent) {
                    agents.push(sign_in.agent);
                }
            }
            held.sort_by_key(|sign_in| Reverse(sign_in.expires));
            let mut answer = Err(LimitProblem::SignIn);
            for sign_in in held
                .iter()
                .filter(|sign_in| sign_in.expires.is_none_or(|at| at > now))
            {
                answer = (reader.fetch)(&sign_in.token, &identity, now);
                if answer.as_ref().err() != Some(&LimitProblem::SignIn) {
                    break;
                }
            }
            let plan = answer
                .as_ref()
                .ok()
                .and_then(|answer| answer.plan.clone())
                .or_else(|| held.iter().find_map(|sign_in| sign_in.plan.clone()));
            AccountRead {
                id: subscription.account_id(&identity.key),
                label: identity.label,
                plan,
                agents,
                limits: answer.map(|answer| answer.limits),
            }
        })
        .collect();
    Ok((reads, seen))
}

/// Every account's limits as of `now`, from `ledger`, with how fast each is
/// rising, and whether it is in use by what `cache` says agents used: each
/// subscription's accounts, and each API-key account; with whether the
/// person hid each, and where each is signed in.
///
/// # Errors
///
/// Returns an error when the ledger or the cache cannot be read.
pub(crate) fn current(
    ledger: &Ledger,
    cache: &Connection,
    now: Instant,
) -> Result<Vec<AccountLimits>> {
    let since = Instant::from_millis(now.millis() - IN_USE).unwrap_or(now);
    let recent = recent_use(cache, since)?;
    let hidden = ledger.hidden_accounts()?;
    let mut accounts = state(ledger, &recent, now)?;
    let spans = ledger.sign_ins()?;
    // When sign-ins and keys were last looked for, which is when an API-key
    // account whose keys aren't read was last looked for.
    let looked = spans
        .iter()
        .map(|(_, span)| span.last)
        .max()
        .and_then(Instant::from_millis);
    api::accounts(&mut accounts, cache, &recent, looked)?;
    let mut folders = attribution::folders(&spans);
    for account in &mut accounts {
        account.hidden = hidden.contains(&account.id);
        if account.subscription != Subscription::ApiKey {
            account.folders = folders.remove(&account.id).unwrap_or_default();
        }
    }
    accounts
        .sort_by(|a, b| (a.subscription, &a.label, &a.id).cmp(&(b.subscription, &b.label, &b.id)));
    Ok(accounts)
}

/// Each account a response since `since` drew on, from the cache's
/// `connection`.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the cache cannot be read.
fn recent_use(connection: &Connection, since: Instant) -> Result<HashSet<String>> {
    // Without the index named, SQLite reads every response for a span open
    // at its end: 20 ms where the index takes 1 (2026-09-24, 132K responses).
    let mut statement = connection.prepare_cached(
        "SELECT DISTINCT account FROM usage INDEXED BY usage_at
         WHERE at >= ?1 AND kind = 'response' AND account IS NOT NULL",
    )?;
    let used = statement
        .query_map([since.millis()], |row| row.get(0))?
        .collect::<rusqlite::Result<HashSet<String>>>()?;
    Ok(used)
}

/// Every account's limits as of `now`, with how fast each is rising.
/// `recent` holds each account a response of the last half hour drew on.
///
/// # Errors
///
/// Returns an error when the ledger cannot be read.
fn state(ledger: &Ledger, recent: &HashSet<String>, now: Instant) -> Result<Vec<AccountLimits>> {
    let since = Instant::from_millis(now.millis() - DAY).unwrap_or(now);
    let mut accounts = Vec::new();
    for account in ledger.accounts()? {
        let readings = ledger.readings(&account.id, since)?;
        let mut rising = false;
        let limits = ledger
            .latest_readings(&account.id)?
            .into_iter()
            .map(|latest| {
                // This window's recent readings: those of the limit whose
                // reset is the latest's, give or take a countdown's drift.
                let window: Vec<(Instant, f64)> = readings
                    .iter()
                    .filter(|reading| reading.key == latest.key && same_window(reading, &latest))
                    .map(|reading| (reading.at, reading.used))
                    .collect();
                // Rising now: its last two readings of the last hour.
                let last_hour = window.partition_point(|(at, _)| now.millis() - at.millis() > HOUR);
                rising |= window[last_hour..]
                    .windows(2)
                    .last()
                    .is_some_and(|pair| pair[1].1 > pair[0].1);
                limit_state(latest, &window, now)
            })
            .collect();
        // Use here counts against the account it drew on, the one signed in
        // where and when it was made, which keeps two accounts of one
        // subscription apart.
        accounts.push(AccountLimits {
            in_use: rising || recent.contains(&account.id),
            id: account.id,
            subscription: account.subscription,
            label: account.label,
            plan: account.plan,
            agents: account.agents,
            signed_in: account.signed_in,
            limits,
            read_at: account.read_at,
            checked_at: Some(account.checked_at),
            problem: account.problem,
            hidden: false,
            provider: None,
            folders: Vec::new(),
        });
    }
    Ok(accounts)
}

/// The longest account id taken, in bytes.
const LONGEST_ID: usize = 512;

/// `account` checked as an account's id: a subscription's key, a colon, and
/// at most 512 bytes in all.
///
/// # Errors
///
/// Returns [`crate::Error::Invalid`] when it is not.
pub(crate) fn checked_account(account: &str) -> Result<Subscription> {
    let subscription = account
        .split_once(':')
        .and_then(|(key, _)| Subscription::from_key(key))
        .filter(|_| account.len() <= LONGEST_ID);
    subscription.ok_or_else(|| crate::Error::Invalid {
        what: "account",
        detail: format!("{account:?} is no account's id"),
    })
}

/// Whether `reading` is of the window `latest` is of.
fn same_window(reading: &Reading, latest: &Reading) -> bool {
    match (reading.resets, latest.resets) {
        (Some(a), Some(b)) => (a.millis() - b.millis()).abs() <= SAME_WINDOW,
        (None, None) => true,
        _ => false,
    }
}

/// A limit's state from its latest reading and its window's recent readings.
fn limit_state(latest: Reading, window: &[(Instant, f64)], now: Instant) -> LimitState {
    let refilled = latest.resets.is_some_and(|resets| resets <= now);
    let pace = if refilled {
        None
    } else {
        let length = latest
            .starts
            .zip(latest.resets)
            .map(|(starts, resets)| resets.millis() - starts.millis());
        pace::pace(window, latest.starts, length)
    };
    LimitState {
        key: latest.key,
        name: latest.name,
        scope: latest.scope,
        used: (!refilled).then_some(latest.used),
        starts: if refilled { None } else { latest.starts },
        resets: if refilled { None } else { latest.resets },
        read_at: latest.at,
        pace,
        refilled,
    }
}

/// The alerts `accounts` give rise to that have not been sent for their
/// windows, recorded as sent. An account the person hid gives rise to none.
///
/// # Errors
///
/// Returns an error when the ledger cannot be written.
pub(crate) fn alerts(
    ledger: &mut Ledger,
    accounts: &[AccountLimits],
    now: Instant,
) -> Result<Vec<Alert>> {
    let mut alerts = Vec::new();
    // An account the person hid is not to be heard of.
    for account in accounts.iter().filter(|account| !account.hidden) {
        // Read for the first time, as every account is when Turnscope first
        // runs, how much of a week is left is where it stands, not news: its
        // milestones are kept as told, and only that it runs out, ran out or
        // is back is said.
        let first = ledger.read_once(&account.id)?;
        for limit in &account.limits {
            // A week or a month someone was warned of is worth saying is
            // full again; five hours, which reset within the day, aren't.
            let long = limit
                .starts
                .zip(limit.resets)
                .is_some_and(|(starts, resets)| resets.millis() - starts.millis() > DAY);
            let due = if limit.refilled {
                // An account signed in nowhere isn't read again, so that a
                // limit it used up is back is known only from its window
                // ending: said once, soon after, and not days late.
                if account.signed_in {
                    None
                } else {
                    warned_before(ledger, &account.id, &limit.key, now.millis(), false)?
                        .filter(|window| now.millis() - window <= BACK_WITHIN)
                        .map(|window| (AlertKind::Available, None, window))
                }
            } else if let Some(resets) = limit.resets {
                let window = resets.millis();
                // Windows before this one, told apart from it by more than a
                // countdown's drift.
                let earlier = window - SAME_WINDOW;
                if limit.used.is_some_and(|used| used >= 100.0) {
                    Some((AlertKind::Reached, Some(resets), window))
                } else if let (Standing::RunningOut, Some(runs_out)) =
                    (limit.standing(), limit.runs_out_at())
                {
                    Some((AlertKind::RunningOut, Some(runs_out), window))
                } else if warned_before(ledger, &account.id, &limit.key, earlier, long)?.is_some() {
                    Some((AlertKind::Available, None, window))
                } else {
                    None
                }
            } else {
                // A limit its provider gives no reset for, as Claude gives
                // none for five hours not yet begun, has no window to name
                // an alert by. That it ran out is said once, named by when,
                // until the last thing said of it is that it is back, which
                // is said once, named as the alert it answers.
                let reached = warned_before(ledger, &account.id, &limit.key, i64::MAX, false)?;
                match (limit.used.is_some_and(|used| used >= 100.0), reached) {
                    (true, None) => Some((AlertKind::Reached, None, now.millis())),
                    (false, Some(window)) => Some((AlertKind::Available, None, window)),
                    _ => None,
                }
            };
            // A warning that it runs out, or ran out, says all a quarter
            // crossed in the same read would: the quarter is kept as told,
            // and not said.
            let mut warned = false;
            for (kind, when, window) in due.into_iter().chain(milestones(account, limit, long, now))
            {
                if ledger.send_alert(&account.id, &limit.key, kind, window, now)? {
                    if (warned || first) && kind.quarter() || first && kind == AlertKind::Unused {
                        continue;
                    }
                    warned |= matches!(kind, AlertKind::RunningOut | AlertKind::Reached);
                    alerts.push(Alert {
                        account: account.id.clone(),
                        subscription: account.subscription,
                        label: account.label.clone(),
                        limit: limit.name.clone(),
                        scope: limit.scope.clone(),
                        kind,
                        when,
                        used: limit.used,
                    });
                }
            }
        }
    }
    Ok(alerts)
}

/// The milestones of `account`'s `limit`, a week or a month if `long`, due
/// at `now`, beside anything said of its running out, each as its kind,
/// when the limit resets, and the window, named by that reset:
///
/// - the quarter of it last used, as [`AlertKind::ThreeQuartersLeft`], then
///   [`AlertKind::HalfLeft`] and [`AlertKind::QuarterLeft`]; crossing
///   several between two readings says only the last, and a limit used up
///   says none, as that it ran out says it;
/// - and, of a subscription's, not an API key's, whose limit left isn't
///   lost, that it resets within a day with more than half of it left and
///   won't run out first, [`AlertKind::Unused`].
///
/// Only a reading of the last half hour says either, as an older one's
/// account may have been used since, or signed out and not read again.
fn milestones(
    account: &AccountLimits,
    limit: &LimitState,
    long: bool,
    now: Instant,
) -> Vec<(AlertKind, Option<Instant>, i64)> {
    let (Some(used), Some(resets)) = (limit.used, limit.resets) else {
        return Vec::new();
    };
    let fresh = now.millis() - limit.read_at.millis() <= FRESH;
    if !long || limit.refilled || !fresh || resets <= now {
        return Vec::new();
    }
    let mut due = Vec::new();
    let quarter = if used >= 100.0 {
        None
    } else if used >= 75.0 {
        Some(AlertKind::QuarterLeft)
    } else if used >= 50.0 {
        Some(AlertKind::HalfLeft)
    } else if used >= 25.0 {
        Some(AlertKind::ThreeQuartersLeft)
    } else {
        None
    };
    due.extend(quarter.map(|kind| (kind, Some(resets), resets.millis())));
    let unused = account.subscription != Subscription::ApiKey
        && used < 50.0
        && resets.millis() - now.millis() <= DAY
        && limit.standing() != Standing::RunningOut;
    if unused {
        due.push((AlertKind::Unused, Some(resets), resets.millis()));
    }
    due
}

/// The window of the alert last sent about `account`'s limit `key` for a
/// window resetting before `before`, when that alert said the limit was
/// reached, or, `long` as a week or a month is, that it was running out: a
/// limit is back only when the last thing said of it was that it ran out,
/// or of a long one, that it would.
fn warned_before(
    ledger: &Ledger,
    account: &str,
    key: &str,
    before: i64,
    long: bool,
) -> Result<Option<i64>> {
    Ok(ledger
        .last_alert(account, key, before)?
        .filter(|(_, kind)| *kind == AlertKind::Reached || (long && *kind == AlertKind::RunningOut))
        .map(|(window, _)| window))
}

/// Ask a provider for usage with `token` and `headers`, and read the limits out
/// of its answer with `parse`: an answer with none is one not understood.
fn get(
    url: &str,
    token: &str,
    headers: &[&str],
    parse: impl FnOnce(&Value) -> Option<PlanLimits>,
) -> Result<PlanLimits, LimitProblem> {
    parse(&ask(url, token, headers)?)
        .filter(|answer| !answer.limits.is_empty())
        .ok_or(LimitProblem::Unrecognized)
}

/// Ask a provider with `token` and `headers` at `url`, a usage endpoint, and
/// give back its answer's JSON.
fn ask(url: &str, token: &str, headers: &[&str]) -> Result<Value, LimitProblem> {
    let mut lines = vec![format!("Authorization: Bearer {token}")];
    lines.extend(headers.iter().map(|header| (*header).to_owned()));
    let answer =
        net::get(Destination::Usage, url, &lines).map_err(|error| LimitProblem::of(&error))?;
    match answer.status {
        200..=299 => serde_json::from_slice(&answer.body).map_err(|_| LimitProblem::Unrecognized),
        401 | 403 => Err(LimitProblem::SignIn),
        408 | 429 | 500..=599 => Err(LimitProblem::Unavailable),
        _ => Err(LimitProblem::Unrecognized),
    }
}

/// Whether `window`, as a provider answers with it, is no window: null, or
/// an object whose every value is null, as providers write a window the plan
/// doesn't have.
fn absent(window: &Value) -> bool {
    match window {
        Value::Null => true,
        Value::Object(fields) => fields.values().all(Value::is_null),
        _ => false,
    }
}

/// An instant a window gives, as [`instant_of`] reads one: `Some(None)` when
/// it gives none, and `None` when what it gives is not an instant.
fn optional_instant(value: &Value) -> Option<Option<Instant>> {
    match value {
        Value::Null => Some(None),
        value => instant_of(value).map(Some),
    }
}

/// A limit, when `used` is a percentage from zero up.
fn limit(
    key: &str,
    name: &str,
    scope: Option<String>,
    used: Option<f64>,
    window: (Option<Instant>, Option<Instant>),
) -> Option<Reported> {
    Some(Reported {
        key: key.to_owned(),
        name: name.to_owned(),
        scope,
        used: used.filter(|used| used.is_finite() && *used >= 0.0)?,
        starts: window.0,
        resets: window.1,
    })
}

/// A window of `length` milliseconds ending at `resets`: when it began, and
/// when it resets.
fn ending(resets: Option<Instant>, length: i64) -> (Option<Instant>, Option<Instant>) {
    (
        resets.and_then(|end| Instant::from_millis(end.millis().checked_sub(length)?)),
        resets,
    )
}

/// A window's length, said the way a person would say it.
fn window_name(seconds: i64) -> String {
    const HOUR_S: i64 = HOUR / 1_000;
    const DAY_S: i64 = DAY / 1_000;
    if seconds < 23 * HOUR_S {
        return match ((seconds + HOUR_S / 2) / HOUR_S).max(1) {
            1 => "Hourly".to_owned(),
            hours => format!("{hours} hours"),
        };
    }
    // Rounded to whole days, because a window that crosses a clock change is
    // an hour long or short.
    match seconds.saturating_add(DAY_S / 2) / DAY_S {
        1 => "Daily".to_owned(),
        7 => "Weekly".to_owned(),
        28..=31 => "Monthly".to_owned(),
        days => format!("{days} days"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use serde_json::json;

    use super::{
        AccountLimits, AccountRead, AlertKind, LimitProblem, LimitState, Reported, Standing,
        Subscription, alerts, get, instant_of, milestones, plan_name, recent_use, state,
        window_name,
    };
    use crate::agent::Agent;
    use crate::ledger::Ledger;
    use crate::ledger::tests::scratch;
    use crate::net::NetError;
    use crate::time::Instant;

    fn minute(minutes: i64) -> Instant {
        Instant::from_millis(1_789_000_000_000 + minutes * 60_000).unwrap()
    }

    /// A read of one Claude account whose five-hour limit is `used` percent
    /// full, in the window that resets at minute `resets`.
    fn read(used: f64, resets: i64) -> AccountRead {
        AccountRead {
            id: "claude:".into(),
            label: None,
            plan: Some("max".into()),
            agents: vec![Agent::ClaudeCode],
            limits: Ok(vec![Reported {
                key: "five_hour".into(),
                name: "5 hours".into(),
                scope: None,
                used,
                starts: Some(minute(resets - 300)),
                resets: Some(minute(resets)),
            }]),
        }
    }

    #[test]
    fn a_pace_stands_as_of_its_latest_reading_while_no_other_comes() {
        let (_dir, mut ledger) = scratch();
        // 30% at minute 0, 40% at 45 and 50% at 60: in hours 0, 3/4 and 1,
        // mean 7/12, and mean use 40. The hours' deviations squared sum to
        // (49 + 4 + 25)/144 = 13/24, and their products with use's to
        // (70 + 0 + 50)/12 = 10: 10 / (13/24) = 240/13 points an hour.
        for (at, used) in [(0, 30.0), (45, 40.0), (60, 50.0)] {
            ledger
                .record_limits(Subscription::Claude, &[read(used, 300)], minute(at))
                .unwrap();
        }
        // A minute on, with no reading since, the first is more than an
        // hour old, but still within the hour before the latest.
        for now in [minute(60), minute(61)] {
            let accounts = state(&ledger, &HashSet::new(), now).unwrap();
            let pace = accounts[0].limits[0].pace.unwrap();
            assert!((pace - 240.0 / 13.0).abs() < 1e-9, "{pace} at {now}");
        }
    }

    #[test]
    fn a_rising_limit_warns_once_before_it_runs_out_and_once_when_it_does() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        // 60% to 90% over half an hour: 60 points an hour, so the rest runs out
        // ten minutes after the last reading, long before the reset at 300.
        for (at, used) in [(0, 60.0), (10, 70.0), (20, 80.0), (30, 90.0)] {
            ledger
                .record_limits(Subscription::Claude, &[read(used, 300)], minute(at))
                .unwrap();
        }
        let now = minute(30);
        let accounts = state(&ledger, &quiet, now).unwrap();
        let limit = &accounts[0].limits[0];
        assert!((limit.pace.unwrap() - 60.0).abs() < 1e-6);
        assert_eq!(limit.reaches(100.0), Some(minute(40)));
        assert!(accounts[0].in_use, "a limit that rose is in use");

        let sent = alerts(&mut ledger, &accounts, now).unwrap();
        assert_eq!(
            sent.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::RunningOut]
        );
        assert!(
            alerts(&mut ledger, &accounts, now).unwrap().is_empty(),
            "once per window"
        );

        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(40))
            .unwrap();
        let full = state(&ledger, &quiet, minute(40)).unwrap();
        let sent = alerts(&mut ledger, &full, minute(40)).unwrap();
        assert_eq!(
            sent.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Reached]
        );

        // After the reset, how much of the new window is used is unknown until
        // it is read, not nothing; then it is available again, once.
        let after = state(&ledger, &quiet, minute(301)).unwrap();
        assert!(after[0].limits[0].refilled);
        assert_eq!(after[0].limits[0].used, None);
        ledger
            .record_limits(Subscription::Claude, &[read(2.0, 600)], minute(305))
            .unwrap();
        let fresh = state(&ledger, &quiet, minute(305)).unwrap();
        let sent = alerts(&mut ledger, &fresh, minute(305)).unwrap();
        assert_eq!(
            sent.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Available]
        );
    }

    #[test]
    fn a_week_warned_of_is_full_again_when_it_resets_and_five_hours_are_not() {
        // What is said of running out; the milestones beside it are
        // another test's.
        let said = |reads| -> Vec<(i64, AlertKind)> {
            alerted(reads)
                .into_iter()
                .filter(|(_, kind)| kind.of_running_out())
                .collect()
        };
        // 60% of the week as it begins: over a day at least, 2.5 points an
        // hour, so the 40 left run out 16 hours on, days before its reset,
        // said once. The next week, read with 1% used, is full again, once.
        assert_eq!(
            said(vec![
                (0, weekly(60.0, 0)),
                (30, weekly(70.0, 0)),
                (WEEK + 5, weekly(1.0, WEEK)),
                (WEEK + 5, weekly(1.0, WEEK)),
            ]),
            [(0, AlertKind::RunningOut), (WEEK + 5, AlertKind::Available)]
        );
        // Five hours warned of, at 60 points an hour, but never used up say
        // nothing when they reset.
        assert_eq!(
            said(vec![
                (0, read(60.0, 300)),
                (10, read(70.0, 300)),
                (20, read(80.0, 300)),
                (305, read(2.0, 600)),
            ]),
            [(20, AlertKind::RunningOut)]
        );
    }

    #[test]
    fn an_account_signed_out_keeps_its_last_limits_and_says_so() {
        let (_dir, mut ledger) = scratch();
        ledger
            .record_limits(Subscription::Claude, &[read(40.0, 300)], minute(0))
            .unwrap();
        // Claude Code switched to another account: this one is found nowhere.
        ledger
            .record_limits(Subscription::Claude, &[], minute(5))
            .unwrap();
        let accounts = state(&ledger, &HashSet::new(), minute(5)).unwrap();
        let account = &accounts[0];
        assert!(!account.signed_in);
        assert_eq!(account.agents, [Agent::ClaudeCode], "where it was last");
        assert_eq!(account.problem, None, "being signed out is no failure");
        assert_eq!(account.limits[0].used, Some(40.0));
        assert_eq!(account.read_at, Some(minute(0)));
        // Once its window resets, it is known to be full again.
        let later = state(&ledger, &HashSet::new(), minute(301)).unwrap();
        assert!(later[0].limits[0].refilled);
        // Signed in again, it is the same account.
        ledger
            .record_limits(Subscription::Claude, &[read(10.0, 600)], minute(320))
            .unwrap();
        let back = state(&ledger, &HashSet::new(), minute(320)).unwrap();
        assert_eq!(back.len(), 1);
        assert!(back[0].signed_in);
    }

    #[test]
    fn a_limit_its_provider_stops_reporting_is_no_longer_shown() {
        let (_dir, mut ledger) = scratch();
        let weekly = Reported {
            key: "seven_day".into(),
            name: "Weekly".into(),
            scope: None,
            used: 70.0,
            starts: None,
            resets: Some(minute(7 * 24 * 60)),
        };
        let mut both = read(40.0, 300);
        if let Ok(limits) = &mut both.limits {
            limits.push(weekly);
        }
        let shown = |ledger: &Ledger, at: i64| -> Vec<(String, Instant)> {
            state(ledger, &HashSet::new(), minute(at)).unwrap()[0]
                .limits
                .iter()
                .map(|limit| (limit.key.clone(), limit.read_at))
                .collect()
        };
        ledger
            .record_limits(Subscription::Claude, &[both], minute(0))
            .unwrap();
        assert_eq!(
            shown(&ledger, 0),
            [
                ("five_hour".to_owned(), minute(0)),
                ("seven_day".to_owned(), minute(0))
            ]
        );
        // The next read gives the five hours alone: the weekly window is
        // gone from the plan, not a week at 70% for good.
        ledger
            .record_limits(Subscription::Claude, &[read(45.0, 300)], minute(5))
            .unwrap();
        assert_eq!(shown(&ledger, 5), [("five_hour".to_owned(), minute(5))]);
        // A read that fails leaves what the last one found.
        let refused = AccountRead {
            limits: Err(LimitProblem::SignIn),
            ..read(45.0, 300)
        };
        ledger
            .record_limits(Subscription::Claude, &[refused], minute(10))
            .unwrap();
        assert_eq!(shown(&ledger, 10), [("five_hour".to_owned(), minute(5))]);
        // It was tried since it was last read, and says why that failed.
        let account = &state(&ledger, &HashSet::new(), minute(10)).unwrap()[0];
        assert_eq!(
            (account.read_at, account.checked_at, account.problem),
            (
                Some(minute(5)),
                Some(minute(10)),
                Some(LimitProblem::SignIn)
            )
        );
    }

    #[test]
    fn a_limit_used_up_on_an_account_signed_out_is_back_when_its_window_ends() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        // Used up in the window that resets at minute 300, then signed out
        // of, as when Claude Code is switched to another account.
        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(0))
            .unwrap();
        let full = state(&ledger, &quiet, minute(0)).unwrap();
        let used_up = alerts(&mut ledger, &full, minute(0)).unwrap();
        assert_eq!(
            used_up.iter().map(|alert| alert.kind).collect::<Vec<_>>(),
            [AlertKind::Reached]
        );
        ledger
            .record_limits(Subscription::Claude, &[], minute(5))
            .unwrap();
        let before = state(&ledger, &quiet, minute(200)).unwrap();
        assert!(
            alerts(&mut ledger, &before, minute(200))
                .unwrap()
                .is_empty()
        );
        // Its window ends: it is back, once.
        let after = state(&ledger, &quiet, minute(305)).unwrap();
        assert_eq!(
            alerts(&mut ledger, &after, minute(305))
                .unwrap()
                .iter()
                .map(|alert| alert.kind)
                .collect::<Vec<_>>(),
            [AlertKind::Available]
        );
        assert!(alerts(&mut ledger, &after, minute(310)).unwrap().is_empty());
        // Signed into again in the next window: that it is back was said.
        ledger
            .record_limits(Subscription::Claude, &[read(5.0, 600)], minute(320))
            .unwrap();
        let again = state(&ledger, &quiet, minute(320)).unwrap();
        assert!(alerts(&mut ledger, &again, minute(320)).unwrap().is_empty());
    }

    #[test]
    fn a_limit_back_long_ago_is_no_news() {
        let (_dir, mut ledger) = scratch();
        let quiet = HashSet::new();
        ledger
            .record_limits(Subscription::Claude, &[read(100.0, 300)], minute(0))
            .unwrap();
        let full = state(&ledger, &quiet, minute(0)).unwrap();
        alerts(&mut ledger, &full, minute(0)).unwrap();
        ledger
            .record_limits(Subscription::Claude, &[], minute(5))
            .unwrap();
        // The app was closed when the window ended, and opened a day later.
        let later = state(&ledger, &quiet, minute(300 + 24 * 60)).unwrap();
        assert!(
            alerts(&mut ledger, &later, minute(300 + 24 * 60))
                .unwrap()
                .is_empty()
        );
    }

    /// `read`, its reset moved by `seconds`, as a countdown moves it from
    /// read to read.
    fn drifting(mut read: AccountRead, seconds: i64) -> AccountRead {
        for limit in read.limits.iter_mut().flatten() {
            limit.resets = limit
                .resets
                .and_then(|resets| Instant::from_millis(resets.millis() + seconds * 1_000));
        }
        read
    }

    /// The alerts `reads` give rise to, each recorded at its minute: the
    /// minute and the kind of each.
    fn alerted(reads: Vec<(i64, AccountRead)>) -> Vec<(i64, AlertKind)> {
        let (_dir, mut ledger) = scratch();
        let mut sent = Vec::new();
        for (at, read) in reads {
            ledger
                .record_limits(Subscription::Claude, &[read], minute(at))
                .unwrap();
            let accounts = state(&ledger, &HashSet::new(), minute(at)).unwrap();
            let alerts = alerts(&mut ledger, &accounts, minute(at)).unwrap();
            sent.extend(alerts.into_iter().map(|alert| (at, alert.kind)));
        }
        sent
    }

    #[test]
    fn a_limit_used_up_is_said_once_however_its_reset_drifts() {
        // ChatGPT gives some resets only as a countdown, so each read of one
        // window finds its reset a second or so from the last.
        let sent = alerted(vec![
            (0, drifting(read(100.0, 300), 0)),
            (5, drifting(read(100.0, 300), 1)),
            (10, drifting(read(100.0, 300), 2)),
            // Just under full in the same window is not back.
            (15, drifting(read(99.0, 300), 3)),
            // The next window is.
            (305, read(2.0, 600)),
        ]);
        assert_eq!(sent, [(0, AlertKind::Reached), (305, AlertKind::Available)]);
    }

    #[test]
    fn a_limit_is_back_when_the_last_alert_sent_said_it_ran_out() {
        // A drifting reset records one window's alerts against resets
        // seconds apart, here the warning's later than the one it ran out.
        let sent = alerted(vec![
            (0, drifting(read(60.0, 300), 10)),
            (10, drifting(read(70.0, 300), 9)),
            // 60 points an hour: it runs out at minute 40.
            (20, drifting(read(80.0, 300), 8)),
            (30, drifting(read(100.0, 300), 7)),
            (305, read(2.0, 600)),
        ]);
        assert_eq!(
            sent,
            [
                (20, AlertKind::RunningOut),
                (30, AlertKind::Reached),
                (305, AlertKind::Available)
            ]
        );
    }

    /// A read of the account of [`read`] whose five-hour limit is `used`
    /// percent full, in a window whose reset its provider doesn't give.
    fn unset(used: f64) -> AccountRead {
        let mut read = read(used, 300);
        for limit in read.limits.iter_mut().flatten() {
            (limit.starts, limit.resets) = (None, None);
        }
        read
    }

    #[test]
    fn a_limit_without_a_reset_runs_out_and_is_back_once_each_time() {
        let sent = alerted(vec![
            (0, unset(100.0)),
            (5, unset(100.0)),
            (10, unset(40.0)),
            (15, unset(40.0)),
            // Used up again, and back again.
            (20, unset(100.0)),
            (25, unset(100.0)),
            (30, unset(10.0)),
        ]);
        assert_eq!(
            sent,
            [
                (0, AlertKind::Reached),
                (10, AlertKind::Available),
                (20, AlertKind::Reached),
                (30, AlertKind::Available)
            ]
        );
    }

    #[test]
    fn a_limit_used_up_is_back_when_its_next_window_gives_no_reset_yet() {
        // Used up in the window resetting at minute 300; after it, the five
        // hours haven't begun, so Claude gives no reset.
        let sent = alerted(vec![(0, read(100.0, 300)), (305, unset(0.0))]);
        assert_eq!(sent, [(0, AlertKind::Reached), (305, AlertKind::Available)]);
    }

    #[test]
    fn recent_use_is_the_accounts_responses_since_then_drew_on() {
        let cache = rusqlite::Connection::open_in_memory().unwrap();
        cache
            .execute_batch(
                "CREATE TABLE usage (account TEXT, kind TEXT, at INTEGER);
                 CREATE INDEX usage_at ON usage (at);
                 INSERT INTO usage VALUES
                     ('chatgpt:a', 'response', 10),
                     ('chatgpt:a', 'response', 12),
                     ('chatgpt:b', 'response', 1),
                     ('claude:c', 'outside', 10),
                     (NULL, 'response', 10);",
            )
            .unwrap();
        // chatgpt:b's response came before, what is outside the conversation
        // is no response, and a response of no account known is no one's.
        assert_eq!(
            recent_use(&cache, Instant::from_millis(5).unwrap()).unwrap(),
            HashSet::from(["chatgpt:a".to_owned()])
        );
    }

    #[test]
    fn use_counts_against_the_account_it_drew_on() {
        let (_dir, mut ledger) = scratch();
        let chatgpt = |id: &str, agents: Vec<Agent>| AccountRead {
            id: id.into(),
            label: None,
            plan: None,
            agents,
            limits: Ok(vec![Reported {
                key: "primary_window".into(),
                name: "5 hours".into(),
                scope: None,
                used: 10.0,
                starts: None,
                resets: Some(minute(300)),
            }]),
        };
        ledger
            .record_limits(
                Subscription::ChatGpt,
                &[
                    chatgpt("chatgpt:a", vec![Agent::Codex]),
                    chatgpt("chatgpt:b", vec![Agent::OpenCode, Agent::Pi]),
                ],
                minute(0),
            )
            .unwrap();
        // A response of the last half hour drew on the first account; none
        // on the second, though agents signed into it were used.
        let recent = HashSet::from(["chatgpt:a".to_owned()]);
        let in_use: Vec<(String, bool)> = state(&ledger, &recent, minute(1))
            .unwrap()
            .into_iter()
            .map(|account| (account.id, account.in_use))
            .collect();
        assert_eq!(
            in_use,
            [
                ("chatgpt:a".to_owned(), true),
                ("chatgpt:b".to_owned(), false)
            ]
        );
    }

    #[test]
    fn a_request_that_gets_no_answer_says_why() {
        // A sign-in that would add a header of its own is refused before
        // anything is sent, and the next is tried.
        let refused = get(
            "https://example.invalid/usage",
            "token\r\nX-Injected: yes",
            &[],
            |_| None,
        );
        assert_eq!(refused.err(), Some(LimitProblem::SignIn));
        // curl not run is no fault of the provider's; curl run with no
        // answer, as offline, is the provider out of reach; an answer
        // whose status doesn't read is not understood.
        let missing = NetError::Spawn(std::io::Error::from(std::io::ErrorKind::NotFound));
        let offline = NetError::Failed {
            code: 6,
            message: "curl: (6) Could not resolve host: api.anthropic.com".into(),
        };
        let garbled = NetError::Answer("status \"2x0\"".into());
        assert_eq!(
            [&missing, &offline, &garbled].map(LimitProblem::of),
            [
                LimitProblem::Unsent,
                LimitProblem::Unavailable,
                LimitProblem::Unrecognized
            ]
        );
        for problem in [
            LimitProblem::SignIn,
            LimitProblem::Unavailable,
            LimitProblem::Unrecognized,
            LimitProblem::Unsent,
        ] {
            assert_eq!(LimitProblem::from_key(problem.key()), Some(problem));
        }
    }

    /// A day in minutes.
    const DAY: i64 = 24 * 60;
    /// A week in minutes.
    const WEEK: i64 = 7 * DAY;

    /// A read of the account of [`read`] whose weekly limit is `used`
    /// percent full, in the window from minute `starts`, a week long.
    fn weekly(used: f64, starts: i64) -> AccountRead {
        AccountRead {
            limits: Ok(vec![Reported {
                key: "seven_day".into(),
                name: "Weekly".into(),
                scope: None,
                used,
                starts: Some(minute(starts)),
                resets: Some(minute(starts + WEEK)),
            }]),
            ..read(0.0, 300)
        }
    }

    #[test]
    fn a_week_says_each_quarter_used_once_and_only_the_last_crossed() {
        // From 10% a day in to 55% four days in: past a quarter used and
        // half, said as half left. 55 points in 96 hours, 0.573 an hour,
        // last the 45 left 78.5 hours, past the reset 72 hours on. An hour
        // on, 62%: 62 points in 97 hours, 0.639 an hour, run the 38 left
        // out in 59.5 hours, before it, which is said on its own. A day on,
        // 80%, a quarter left, is said though a warning came before. Used
        // up, it is said as reached, not as a quarter left. The next week,
        // 30% three days in, 0.417 an hour, lasts: its first quarter is
        // said again.
        let milestones: Vec<(i64, AlertKind)> = alerted(vec![
            (DAY, weekly(10.0, 0)),
            (4 * DAY, weekly(55.0, 0)),
            (4 * DAY + 60, weekly(62.0, 0)),
            (5 * DAY, weekly(80.0, 0)),
            (6 * DAY, weekly(100.0, 0)),
            (WEEK + 3 * DAY, weekly(30.0, WEEK)),
        ])
        .into_iter()
        .filter(|(_, kind)| !kind.of_running_out())
        .collect();
        assert_eq!(
            milestones,
            [
                (4 * DAY, AlertKind::HalfLeft),
                (5 * DAY, AlertKind::QuarterLeft),
                (WEEK + 3 * DAY, AlertKind::ThreeQuartersLeft)
            ]
        );
    }

    #[test]
    fn an_account_read_for_the_first_time_says_only_that_it_runs_out() {
        // First read four days in at 30%, 0.3125 points an hour, lasting: a
        // quarter used is where it stands, not news. An hour on, 51%, 0.526
        // an hour, the 49 left lasting 93 hours, past the reset 71 hours
        // off: half left is news.
        assert_eq!(
            alerted(vec![
                (4 * DAY, weekly(30.0, 0)),
                (4 * DAY + 60, weekly(51.0, 0)),
            ]),
            [(4 * DAY + 60, AlertKind::HalfLeft)]
        );
        // First read six days in at 90%, 0.625 an hour: the 10 left run out
        // in 16 hours, before the reset a day off, which is said.
        assert_eq!(
            alerted(vec![(6 * DAY, weekly(90.0, 0))]),
            [(6 * DAY, AlertKind::RunningOut)]
        );
    }

    #[test]
    fn a_quarter_crossed_as_a_week_starts_running_out_is_not_said_beside_it() {
        // 10% a day in, then 30% an hour later: 20 points an hour, so the
        // 70 left last three and a half hours, far before the week resets
        // six days on. That it runs out is said; the quarter used, crossed
        // in the same read, is not, then or later, as it was kept as told.
        let sent = alerted(vec![
            (DAY, weekly(10.0, 0)),
            (DAY + 60, weekly(30.0, 0)),
            (DAY + 70, weekly(33.0, 0)),
        ]);
        assert_eq!(sent, [(DAY + 60, AlertKind::RunningOut)]);
    }

    #[test]
    fn a_week_mostly_left_a_day_before_it_resets_is_said_once() {
        let sent = alerted(vec![
            // Read before, so what follows is news.
            (DAY, weekly(10.0, 0)),
            // 36 hours before the reset, a day and a half: not yet.
            (WEEK - 36 * 60, weekly(30.0, 0)),
            // 18 hours before it, with 69% left.
            (WEEK - 18 * 60, weekly(31.0, 0)),
            (WEEK - 6 * 60, weekly(32.0, 0)),
        ]);
        assert_eq!(
            sent,
            [
                (WEEK - 36 * 60, AlertKind::ThreeQuartersLeft),
                (WEEK - 18 * 60, AlertKind::Unused)
            ]
        );
    }

    #[test]
    fn only_a_fresh_reading_of_a_subscriptions_week_says_it_is_mostly_left() {
        let now = minute(WEEK - 18 * 60);
        let limit = LimitState {
            key: "seven_day".into(),
            name: "Weekly".into(),
            scope: None,
            used: Some(30.0),
            starts: Some(minute(0)),
            resets: Some(minute(WEEK)),
            read_at: now,
            pace: None,
            refilled: false,
        };
        let account = AccountLimits {
            id: "claude:".into(),
            agents: vec![Agent::ClaudeCode],
            read_at: Some(now),
            checked_at: Some(now),
            in_use: false,
            ..account(Vec::new())
        };
        let kinds = |account: &AccountLimits, limit: &LimitState, long: bool| -> Vec<AlertKind> {
            milestones(account, limit, long, now)
                .into_iter()
                .map(|(kind, _, _)| kind)
                .collect()
        };
        assert_eq!(
            kinds(&account, &limit, true),
            [AlertKind::ThreeQuartersLeft, AlertKind::Unused]
        );
        // Read 31 minutes ago, it may have been used since.
        let stale = LimitState {
            read_at: minute(WEEK - 18 * 60 - 31),
            ..limit.clone()
        };
        assert_eq!(kinds(&account, &stale, true), []);
        // Five hours are no week.
        assert_eq!(kinds(&account, &limit, false), []);
        // An API key's limit left isn't lost at its window's end.
        let key = AccountLimits {
            subscription: Subscription::ApiKey,
            ..account.clone()
        };
        assert_eq!(kinds(&key, &limit, true), [AlertKind::ThreeQuartersLeft]);
        // Rising 5 points an hour from 30%, it runs out in 14 hours, before
        // the reset 18 hours off.
        let rising = LimitState {
            pace: Some(5.0),
            ..limit.clone()
        };
        assert_eq!(
            kinds(&account, &rising, true),
            [AlertKind::ThreeQuartersLeft]
        );
    }

    #[test]
    fn a_milestone_said_after_a_warning_does_not_keep_a_week_from_being_back() {
        let sent = alerted(vec![
            // Read before, so what follows is news.
            (12 * 60, weekly(5.0, 0)),
            // 26% two days in, 13 points a day: the 74 left last 137 hours,
            // past the reset five days on.
            (2 * DAY, weekly(26.0, 0)),
            // A day on, 45%: fitted over the day, 19 points in 24 hours, the
            // 55 left last 69 hours, days before the reset.
            (3 * DAY, weekly(45.0, 0)),
            (3 * DAY + 240, weekly(50.0, 0)),
            (WEEK + 10, weekly(1.0, WEEK)),
        ]);
        assert_eq!(
            sent,
            [
                (2 * DAY, AlertKind::ThreeQuartersLeft),
                (3 * DAY, AlertKind::RunningOut),
                (3 * DAY + 240, AlertKind::HalfLeft),
                (WEEK + 10, AlertKind::Available)
            ]
        );
    }

    #[test]
    fn a_plan_is_named_as_it_calls_itself() {
        assert_eq!(plan_name("prolite"), "Pro Lite");
        assert_eq!(plan_name("PRO"), "Pro");
        assert_eq!(plan_name("Go"), "Go");
        // One not known by name, its words each begun with a capital.
        assert_eq!(plan_name("super_heavy"), "Super Heavy");
        assert_eq!(plan_name(""), "");
    }

    #[test]
    fn a_window_is_named_as_a_person_would_say_it() {
        assert_eq!(window_name(5 * 3_600), "5 hours");
        assert_eq!(window_name(3_600), "Hourly");
        assert_eq!(window_name(7 * 86_400), "Weekly");
        // A week that crosses a clock change is an hour long or short.
        assert_eq!(window_name(7 * 86_400 + 3_600), "Weekly");
        assert_eq!(window_name(30 * 86_400), "Monthly");
    }

    #[test]
    fn instants_are_read_as_text_seconds_or_milliseconds() {
        let at = Instant::parse("2026-09-14T08:09:23Z").unwrap();
        assert_eq!(instant_of(&json!("2026-09-14T08:09:23Z")), Some(at));
        assert_eq!(instant_of(&json!(1_789_373_363)), Some(at));
        assert_eq!(instant_of(&json!(1_789_373_363_000_i64)), Some(at));
        // Numbers too large to be any instant are none, either side of zero.
        assert_eq!(instant_of(&json!(i64::MIN)), None);
        assert_eq!(instant_of(&json!(i64::MAX)), None);
    }

    /// A Claude account in use, `a`, holding `limits`.
    fn account(limits: Vec<LimitState>) -> AccountLimits {
        AccountLimits {
            id: "a".into(),
            subscription: Subscription::Claude,
            label: None,
            plan: None,
            agents: Vec::new(),
            signed_in: true,
            limits,
            read_at: None,
            checked_at: None,
            problem: None,
            in_use: true,
            hidden: false,
            provider: None,
            folders: Vec::new(),
        }
    }

    /// A limit read at minute 0, `used` percent used and rising `pace`
    /// points an hour, in a window of `hours` that started then.
    fn paced(used: f64, pace: f64, hours: i64) -> LimitState {
        LimitState {
            key: "k".into(),
            name: "n".into(),
            scope: None,
            used: Some(used),
            starts: Some(minute(0)),
            resets: Some(minute(hours * 60)),
            read_at: minute(0),
            pace: Some(pace),
            refilled: false,
        }
    }

    #[test]
    fn a_pace_says_when_a_limit_runs_out_or_what_is_left_at_its_reset() {
        // Read at minute 0. 82% used, rising 12 points an hour: the 18 left
        // last 1.5 hours, to minute 90, before the reset 3 hours on.
        let five = LimitState {
            key: "five_hour".into(),
            name: "5 hours".into(),
            ..paced(82.0, 12.0, 3)
        };
        assert_eq!(five.outlook().runs_out, Some(minute(90)));
        // 38% used, rising half a point an hour, 40 hours to its reset: 20
        // more points, so 42% left at it.
        let week = LimitState {
            key: "seven_day".into(),
            name: "Weekly".into(),
            ..paced(38.0, 0.5, 40)
        };
        assert_eq!(week.outlook().left_at_reset, Some(42.0));
        assert_eq!(week.outlook().runs_out, None);
        // Not rising: what is left now is left at the reset.
        assert_eq!(paced(38.0, 0.0, 40).outlook().left_at_reset, Some(62.0));
        // No pace yet: neither.
        let new = LimitState {
            pace: None,
            ..paced(38.0, 0.0, 40)
        };
        assert_eq!(new.outlook().left_at_reset, None);
        // The tightest is the one that runs out first.
        assert_eq!(
            account(vec![week, five]).deciding().unwrap().name,
            "5 hours"
        );
    }

    #[test]
    fn a_limit_is_in_reserve_by_what_an_even_pace_would_have_used() {
        // A week read 84 hours in, half of it gone: 42% used is 8 points in
        // reserve, 60% used 10 ahead of an even pace.
        let half = |used: f64| LimitState {
            read_at: minute(84 * 60),
            ..paced(used, 0.0, 168)
        };
        assert_eq!(half(42.0).reserve(), Some(8.0));
        assert_eq!(half(60.0).reserve(), Some(-10.0));
        // Read before its window began, none of it is gone; after its reset,
        // all of it.
        assert_eq!(paced(3.0, 0.0, 168).reserve(), Some(-3.0));
        let after = LimitState {
            read_at: minute(200 * 60),
            ..paced(90.0, 0.0, 168)
        };
        assert_eq!(after.reserve(), Some(10.0));
        // With no start known, or no use, none.
        let unknown = LimitState {
            starts: None,
            ..half(42.0)
        };
        assert_eq!(unknown.reserve(), None);
        let refilled = LimitState {
            used: None,
            ..half(42.0)
        };
        assert_eq!(refilled.reserve(), None);
    }

    #[test]
    fn a_limit_lasts_under_what_is_left_spread_to_its_reset() {
        // A week read 84 hours in, 58% used: its 42 left over the 84 hours
        // to its reset are half a point an hour.
        let half = LimitState {
            read_at: minute(84 * 60),
            ..paced(58.0, 1.0, 168)
        };
        assert_eq!(half.lasting_pace(), Some(0.5));
        // Read at or after its reset, or with no reset or use known, none.
        let past = LimitState {
            read_at: minute(168 * 60),
            ..half.clone()
        };
        assert_eq!(past.lasting_pace(), None);
        let unknown = LimitState {
            resets: None,
            ..half.clone()
        };
        assert_eq!(unknown.lasting_pace(), None);
        let refilled = LimitState { used: None, ..half };
        assert_eq!(refilled.lasting_pace(), None);
    }

    #[test]
    fn a_limit_runs_out_only_a_meaningful_while_before_it_resets() {
        // A week, 32% used, rising 0.4043 points an hour: its 68 left last
        // 68 / 0.4043 = 168.19 hours, past the 168-hour reset. It lasts.
        assert_eq!(paced(32.0, 0.4043, 168).standing(), Standing::Lasts);
        // Rising 0.405: 167.9 hours, 6 minutes before the reset, within a
        // fiftieth of the week (3.36 hours). Still it lasts.
        assert_eq!(paced(32.0, 0.405, 168).standing(), Standing::Lasts);
        // Rising 0.42: 161.9 hours, 6.1 hours early. It runs out.
        assert_eq!(paced(32.0, 0.42, 168).standing(), Standing::RunningOut);
        // Five hours, 50% used, rising 11 an hour: 4.55 hours, 27 minutes
        // before the reset, beyond the least margin of 15 minutes.
        assert_eq!(paced(50.0, 11.0, 5).standing(), Standing::RunningOut);
        // Rising 10.5: 4.76 hours, 14 minutes early. It lasts.
        assert_eq!(paced(50.0, 10.5, 5).standing(), Standing::Lasts);
        // Used up is used up, whatever the pace.
        assert_eq!(paced(100.0, 0.0, 5).standing(), Standing::UsedUp);
    }

    #[test]
    fn a_limit_with_no_reset_runs_out_only_within_a_day() {
        // A key's lifetime limit, 50% used, rising 1 point an hour from the
        // 12:00 reading: its 50 left last 50 hours, beyond a day. It lasts.
        let slow = LimitState {
            resets: None,
            starts: None,
            ..paced(50.0, 1.0, 5)
        };
        assert_eq!(slow.standing(), Standing::Lasts);
        // Rising 3 an hour: 16.7 hours. It runs out.
        let fast = LimitState {
            resets: None,
            starts: None,
            ..paced(50.0, 3.0, 5)
        };
        assert_eq!(fast.standing(), Standing::RunningOut);
    }

    #[test]
    fn an_account_stands_as_the_limit_that_matters() {
        // The week on all usage lasts; the Opus week, on one model, is used
        // up. What matters is the week, and so the account lasts.
        let opus = LimitState {
            key: "opus".into(),
            scope: Some("Opus".into()),
            ..paced(100.0, 0.0, 168)
        };
        let both = account(vec![paced(40.0, 0.0, 168), opus]);
        assert_eq!(both.deciding().map(|l| l.key.as_str()), Some("k"));
        assert_eq!(both.standing(), Standing::Lasts);
    }

    #[test]
    fn the_limit_that_matters_is_used_up_then_soonest_out_then_least_left() {
        let named = |name: &str, limit: LimitState| LimitState {
            key: name.into(),
            name: name.into(),
            ..limit
        };
        // 90% left and lasting, against 60% left and lasting: 60%.
        let calm = account(vec![
            named("five", paced(10.0, 0.0, 5)),
            named("week", paced(40.0, 0.0, 168)),
        ]);
        assert_eq!(calm.deciding().map(|l| l.key.as_str()), Some("week"));
        // A week running out beats five hours with less left that lasts.
        let out = account(vec![
            named("five", paced(80.0, 0.0, 5)),
            named("week", paced(40.0, 1.0, 168)),
        ]);
        assert_eq!(out.deciding().map(|l| l.key.as_str()), Some("week"));
        assert_eq!(out.standing(), Standing::RunningOut);
        // Used up beats running out.
        let spent = account(vec![
            named("five", paced(100.0, 0.0, 5)),
            named("week", paced(40.0, 1.0, 168)),
        ]);
        assert_eq!(spent.deciding().map(|l| l.key.as_str()), Some("five"));
        assert_eq!(spent.standing(), Standing::UsedUp);
    }

    #[test]
    fn an_account_is_titled_by_its_plan_or_its_providers_api() {
        let title = |subscription, plan: Option<&str>, provider: Option<&str>| {
            AccountLimits {
                subscription,
                label: Some("joey@example.com".into()),
                plan: plan.map(str::to_owned),
                in_use: false,
                provider: provider.map(str::to_owned),
                ..account(Vec::new())
            }
            .title()
        };
        assert_eq!(title(Subscription::Claude, Some("max"), None), "Claude Max");
        assert_eq!(title(Subscription::Claude, None, None), "Claude");
        // A plan the subscription is named for is said once.
        assert_eq!(
            title(Subscription::OpenCodeGo, Some("go"), None),
            "OpenCode Go"
        );
        assert_eq!(
            title(Subscription::ApiKey, None, Some("openrouter")),
            "OpenRouter API key"
        );
        assert_eq!(
            title(Subscription::ApiKey, None, Some("new-provider")),
            "New Provider API key"
        );
    }
}
