//! Token counts and money.

use serde::{Deserialize, Serialize};

/// The largest count any one record may hold.
///
/// About 1.1 quadrillion: far beyond any response or session, 110,000 times
/// the largest session's cache reads here (9.92 billion, 2026-09-27). A
/// larger count can only be corruption, and readers refuse it as such. Sums
/// can pass `i64`, how the ledger and cache store counts, after 8,192 counts
/// this large, so sums stop at `i64::MAX` rather than fail ([`Tokens::add`],
/// and the cache's sums alike).
pub(crate) const LARGEST_COUNT: u64 = 1 << 50;

/// The largest sum of counts: `i64::MAX`, where the cache's sums stop too.
const MOST: u64 = i64::MAX.unsigned_abs();

/// Two counts, or sums of them, added: stopping at `i64::MAX`, as the cache's
/// sums do, so a sum comes out the same in any order.
pub(crate) fn add_counts(mine: u64, theirs: u64) -> u64 {
    mine.saturating_add(theirs).min(MOST)
}

/// Token counts for one response, or a sum of them.
///
/// Every agent's figures are brought to this one form, in which **no count
/// includes another**, so a sum means the same thing whichever agent it came
/// from. The one exception is `reasoning`, which says how much of `output` was
/// reasoning and is never added to it.
///
/// A reader records no count above 2^50, as `LARGEST_COUNT` says; a sum of them
/// stops at `i64::MAX`. Neither bound is kept by the type itself, whose
/// fields are public: they are kept where counts are read and added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    /// Input neither read from the cache nor written to it.
    pub input: u64,
    /// Input read from the cache.
    pub cache_read: u64,
    /// Input written to a cache kept for five minutes, or for however long the
    /// provider keeps it when it offers only one duration.
    pub cache_write_5m: u64,
    /// Input written to a cache kept for an hour.
    pub cache_write_1h: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// How much of `output` was reasoning.
    pub reasoning: u64,
}

impl Tokens {
    /// Everything written to the cache.
    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m.saturating_add(self.cache_write_1h)
    }

    /// The prompt a response was given: its input however it was cached. A
    /// provider's long-context prices are set by this.
    pub(crate) fn prompt(&self) -> u64 {
        self.input
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write())
    }

    /// Every token, counting reasoning once, as part of output.
    pub fn total(&self) -> u64 {
        self.prompt().saturating_add(self.output)
    }

    /// Combine two reports of the same response into one.
    ///
    /// Agents write a response more than once — as it streams, and again when a
    /// subagent copies it — and a later report is never smaller than an earlier
    /// one. So the combination keeps the larger of each count, which gives the
    /// same result in any order and however many times a report is repeated.
    pub(crate) fn merge(&mut self, other: &Tokens) {
        self.input = self.input.max(other.input);
        self.cache_read = self.cache_read.max(other.cache_read);
        self.cache_write_5m = self.cache_write_5m.max(other.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.max(other.cache_write_1h);
        self.output = self.output.max(other.output);
        self.reasoning = self.reasoning.max(other.reasoning);
    }

    /// Add another response's counts to these, each stopping at `i64::MAX`,
    /// as the cache's sums do, so a sum comes out the same in any order.
    pub(crate) fn add(&mut self, other: &Tokens) {
        let add = |mine: &mut u64, theirs: u64| *mine = add_counts(*mine, theirs);
        add(&mut self.input, other.input);
        add(&mut self.cache_read, other.cache_read);
        add(&mut self.cache_write_5m, other.cache_write_5m);
        add(&mut self.cache_write_1h, other.cache_write_1h);
        add(&mut self.output, other.output);
        add(&mut self.reasoning, other.reasoning);
    }
}

/// One response, with every report of it combined: what it used, and what
/// is known of what it cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Response {
    /// The agent that recorded it.
    pub agent: crate::agent::Agent,
    /// Its key within the agent.
    pub key: String,
    /// The session it belongs to.
    pub session: crate::session::SessionKey,
    /// When it was made.
    pub at: crate::time::Instant,
    /// The provider that served it.
    pub provider: String,
    /// The model, as the agent recorded it.
    pub model: String,
    /// What it used.
    pub tokens: Tokens,
    /// The largest prompt it covers, which sets its long-context tier.
    pub prompt: u64,
    /// Web searches the provider ran for it.
    pub web_searches: u64,
    /// Whether it was served at priority.
    pub priority: bool,
    /// What the agent recorded it cost.
    pub recorded: Option<Usd>,
}

/// An amount in US dollars, held as whole nano-dollars.
///
/// Sums are of integers, so a total never depends on the order its parts were
/// added in. One record's amount is at least zero and at most a billion
/// dollars; a sum of them can reach `i64::MAX` nano-dollars, about $9.2
/// billion, after ten of the largest, and stops there.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Usd(i64);

impl Usd {
    /// Nano-dollars in a dollar.
    const NANOS: f64 = 1e9;

    /// The largest amount one record may hold, in dollars.
    const LARGEST: f64 = 1e9;

    /// The largest amount one record may hold, in nano-dollars: a billion
    /// dollars, as [`Usd::LARGEST`].
    const LARGEST_NANOS: u64 = 1_000_000_000 * 1_000_000_000;

    /// The largest amount, about $9.2 billion, where sums stop: one there is
    /// at least that much, and how much more isn't known.
    pub(crate) const MOST: Usd = Usd(i64::MAX);

    /// `dollars`, rounded to the nearest nano-dollar, when it is a finite
    /// amount between zero and a billion dollars.
    pub(crate) fn from_dollars(dollars: f64) -> Option<Usd> {
        if !dollars.is_finite() || !(0.0..=Usd::LARGEST).contains(&dollars) {
            return None;
        }
        // In range by the check above, so the conversion neither saturates nor
        // loses anything but the fraction of a nano-dollar being rounded.
        let nanos = (dollars * Usd::NANOS).round() as i64;
        Some(Usd(nanos))
    }

    /// An amount of whole nano-dollars, a record's or a sum's, when it is not
    /// below zero.
    pub fn from_nanos(nanos: i64) -> Option<Usd> {
        (nanos >= 0).then_some(Usd(nanos))
    }

    /// One record's amount of whole nano-dollars, when it is no more than a
    /// billion dollars, the most [`Usd::from_dollars`] takes too.
    pub(crate) fn of_record_nanos(nanos: u64) -> Option<Usd> {
        if nanos > Usd::LARGEST_NANOS {
            return None;
        }
        Usd::from_nanos(i64::try_from(nanos).ok()?)
    }

    /// The amount in nano-dollars.
    pub(crate) const fn nanos(self) -> i64 {
        self.0
    }

    /// The amount in dollars, for display: exact to the nano-dollar up to
    /// 2^53 of them, about $9 million, and within a few past it, far finer
    /// than any amount is shown.
    pub fn dollars(self) -> f64 {
        let nanos = self.0 as f64;
        nanos / Usd::NANOS
    }

    /// The sum of two amounts, stopping at [`Usd::MOST`].
    pub(crate) fn saturating_add(self, other: Usd) -> Usd {
        Usd(self.0.saturating_add(other.0))
    }

    /// The sum of two amounts, or `None` past [`Usd::MOST`].
    pub(crate) fn checked_add(self, other: Usd) -> Option<Usd> {
        self.0.checked_add(other.0).map(Usd)
    }
}

#[cfg(test)]
mod tests {
    use super::{Tokens, Usd};

    fn tokens(input: u64, cache_read: u64, output: u64) -> Tokens {
        Tokens {
            input,
            cache_read,
            output,
            ..Tokens::default()
        }
    }

    #[test]
    fn merging_keeps_the_most_complete_report_in_any_order() {
        // A response streamed as three lines, the first two with partial output.
        let reports = [
            tokens(32, 254_907, 16),
            tokens(32, 254_907, 13_695),
            tokens(32, 254_907, 20),
        ];
        let mut forward = Tokens::default();
        reports.iter().for_each(|report| forward.merge(report));
        let mut backward = Tokens::default();
        reports
            .iter()
            .rev()
            .for_each(|report| backward.merge(report));
        assert_eq!(forward, tokens(32, 254_907, 13_695));
        assert_eq!(backward, forward);
        // Merging a report again changes nothing.
        forward.merge(&reports[1]);
        assert_eq!(forward, backward);
    }

    #[test]
    fn a_prompt_is_its_input_however_it_was_cached() {
        let used = Tokens {
            input: 2,
            cache_read: 30_516,
            cache_write_5m: 0,
            cache_write_1h: 6_261,
            output: 87,
            reasoning: 40,
        };
        assert_eq!(used.prompt(), 36_779);
        // Reasoning is part of the output and is not counted again.
        assert_eq!(used.total(), 36_866);
    }

    #[test]
    fn dollars_round_to_the_nearest_nano_dollar() {
        assert_eq!(
            Usd::from_dollars(42.274_133_749_999_99).unwrap().nanos(),
            42_274_133_750
        );
        assert_eq!(Usd::from_dollars(0.0).unwrap().nanos(), 0);
        assert_eq!(Usd::from_dollars(-0.01), None);
        assert_eq!(Usd::from_dollars(f64::NAN), None);
        assert_eq!(Usd::from_dollars(2e9), None);
    }

    #[test]
    fn counts_add_up_to_the_largest_the_cache_holds_and_stop() {
        // 2^62 twice is 2^63, one past i64::MAX: the sum stops at it, as the
        // cache's sums do, whichever is added to which.
        let half = tokens(1 << 62, 0, 7);
        let mut sum = half;
        sum.add(&half);
        assert_eq!(sum.input, i64::MAX.unsigned_abs());
        assert_eq!(sum.output, 14);
    }
}
