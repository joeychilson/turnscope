//! What share of a limit usage took.
//!
//! A limit's readings say how much of it is used, and nothing of who used
//! it. So each rise, from one reading to the next of the same window, is
//! shared out among the responses of that stretch that could have drawn on
//! it, in proportion to what each cost at list prices. That makes a share
//! approximate, and it says so: a provider weighs usage its own way, and
//! reads a little behind it.
//!
//! **Rises.** A window whose start is known rises from none at its start
//! to its first reading, so a share of it is whole. One whose start isn't
//! known has no rise before its first reading, and a share of usage spent
//! before then says it isn't whole. A limit rises only past the most it
//! was read at: a reading a point lower, as a provider's rounding gives,
//! and the reading back up from it share out nothing, as nothing was used.
//! A fall of more, as when a provider gives use back, starts it again from
//! there ([`super::pace`]). A window ends at its latest reading: what was
//! spent since has no rise yet.
//!
//! **Who could have drawn on it.** Responses that drew on the account, the
//! one signed in where and when each was made ([`super::attribution`]),
//! and, for a limit on one model (its scope, as "Opus" or "gpt-6-astra"),
//! of a model whose key holds the scope, ignoring case. So two accounts of
//! one subscription, kept in two folders or signed into one after the
//! other, each share their rises among their own responses. A response with
//! no price takes no share.
//!
//! **Elsewhere.** A rise while nothing here spent is use off this Mac, as
//! on the provider's website: it is no response's, and a share leaves it
//! out; a window's reckoning ([`window`]) says how much of it there was.
//! A rise while only responses with no price were made here is this Mac's
//! use, not use elsewhere, but nothing says how to share it: a window's
//! reckoning says how much of it there was apart.
//!
//! **A whole window** is reckoned in one pass: every response that could
//! have drawn on it, oldest first, and each rise shared among those of its
//! stretch, found by where the stretch starts and ends among them, so the
//! work grows with the responses and the readings, not with their product.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::error::{Error, Result};
use crate::ledger::{Ledger, Reading};
use crate::limits::pace::GIVEN_BACK;
use crate::limits::{DAY, SAME_WINDOW};
use crate::model::ModelKey;
use crate::session::SessionKey;
use crate::time::Instant;

/// How far back readings are looked for a window a response fell in: the
/// longest window a subscription has, a month, and a day beside.
const LONGEST: i64 = 32 * DAY;

/// A response whose share of limits is asked.
#[derive(Clone, Debug)]
pub(crate) struct Spent {
    pub(crate) at: Option<Instant>,
    /// The account it drew on, where known.
    pub(crate) account: Option<String>,
    pub(crate) model_key: String,
    /// What it cost at list prices; `None` where that isn't known.
    pub(crate) cost: Option<i64>,
}

/// One window of one limit that responses asked of took a share of.
#[derive(Clone, Debug)]
pub(crate) struct Shared {
    pub(crate) account: String,
    pub(crate) key: String,
    pub(crate) name: String,
    pub(crate) scope: Option<String>,
    pub(crate) starts: Option<Instant>,
    pub(crate) resets: Option<Instant>,
    /// The latest reading the share goes up to.
    pub(crate) through: Instant,
    /// Whether every rise the responses could have taken a share of is
    /// known: false when some of them came before the first reading of a
    /// window whose start isn't known.
    pub(crate) whole: bool,
    /// Each response's share, in points of the limit's percent, in the order
    /// asked.
    pub(crate) each: Vec<f64>,
}

/// Every window of every limit that `spent`, oldest first and those at no
/// time known last, took a share of, from the ledger's readings and what
/// everything else in the cache spent beside it.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the ledger or the cache cannot be
/// read.
pub(crate) fn shares(ledger: &Ledger, cache: &Connection, spent: &[Spent]) -> Result<Vec<Shared>> {
    let Some(first) = spent
        .iter()
        .filter_map(|spent| spent.at.map(Instant::millis))
        .min()
    else {
        return Ok(Vec::new());
    };
    let drew: HashSet<&str> = spent
        .iter()
        .filter_map(|spent| spent.account.as_deref())
        .collect();
    let Some(since) = Instant::from_millis(first.saturating_sub(LONGEST).max(0)) else {
        return Ok(Vec::new());
    };
    let mut shared = Vec::new();
    for account in ledger.accounts()? {
        if !drew.contains(account.id.as_str()) {
            continue;
        }
        for window in windows(ledger.readings(&account.id, since)?) {
            let Some(latest) = window.last() else {
                continue;
            };
            let points = points(&window);
            let (Some(&(from, _)), Some(&(through, _))) = (points.first(), points.last()) else {
                continue;
            };
            let drawing = |spent: &Spent| {
                spent.account.as_deref() == Some(account.id.as_str())
                    && latest
                        .scope
                        .as_deref()
                        .is_none_or(|scope| holds(&spent.model_key, scope))
            };
            // Only a window the responses spent in is theirs to share.
            if !spent.iter().any(|spent| {
                drawing(spent) && spent.at.is_some_and(|at| at > from && at <= through)
            }) {
                continue;
            }
            // What each response priced spent, as its time and cost: those
            // with no price take no share.
            let everyone: Vec<(i64, i64)> = spending_by(
                cache,
                &account.id,
                latest.scope.as_deref(),
                from.millis(),
                through.millis(),
            )?
            .rows
            .iter()
            .filter_map(|row| Some((row.at, row.cost?)))
            .collect();
            let each = shared_out(&points, &everyone, spent, &drawing);
            let whole = latest.starts.is_some()
                || !spent.iter().any(|spent| {
                    drawing(spent)
                        && spent
                            .at
                            .is_some_and(|at| at <= from && at.millis() > from.millis() - LONGEST)
                });
            shared.push(Shared {
                account: account.id.clone(),
                key: latest.key.clone(),
                name: latest.name.clone(),
                scope: latest.scope.clone(),
                starts: latest.starts,
                resets: latest.resets,
                through,
                whole,
                each,
            });
        }
    }
    // Windows well before or after all of it took nothing from it.
    shared.retain(|shared| shared.each.iter().any(|share| *share > 0.));
    Ok(shared)
}

/// Whether `model_key` is of the model a limit's `scope` names.
fn holds(model_key: &str, scope: &str) -> bool {
    model_key.to_lowercase().contains(&scope.to_lowercase())
}

/// `readings`, by limit and oldest first as the ledger gives them, split
/// into their windows: a limit's readings belong together while their
/// resets agree, give or take a countdown's drift, and without one while
/// they don't fall.
pub(super) fn windows(readings: Vec<Reading>) -> Vec<Vec<Reading>> {
    let mut windows: Vec<Vec<Reading>> = Vec::new();
    for reading in readings {
        let same = windows
            .last()
            .and_then(|window| window.last())
            .is_some_and(|last| {
                last.key == reading.key
                    && match (last.resets, reading.resets) {
                        (Some(a), Some(b)) => (a.millis() - b.millis()).abs() <= SAME_WINDOW,
                        (None, None) => reading.used >= last.used - GIVEN_BACK,
                        _ => false,
                    }
            });
        match (same, windows.last_mut()) {
            (true, Some(window)) => window.push(reading),
            _ => windows.push(vec![reading]),
        }
    }
    windows
}

/// The windows of `account`'s limit `key` read since `since`, oldest first,
/// each its readings, as [`windows`] tells them apart.
///
/// # Errors
///
/// Returns [`crate::Error::Ledger`] when the ledger cannot be read.
pub(super) fn windows_of(
    ledger: &Ledger,
    account: &str,
    key: &str,
    since: Instant,
) -> Result<Vec<Vec<Reading>>> {
    let readings = ledger
        .readings(account, since)?
        .into_iter()
        .filter(|reading| reading.key == key)
        .collect();
    Ok(windows(readings))
}

/// Each stretch between two of `points` that a limit rose in, as when it
/// starts and ends, in milliseconds, as spending is kept, and how much it
/// rose: past the most it was read at before, or, after a fall of more than
/// a step of rounding, past where it fell to.
fn rises(points: &[(Instant, f64)]) -> Vec<(i64, i64, f64)> {
    let mut rises = Vec::new();
    let Some(&(_, first)) = points.first() else {
        return rises;
    };
    let mut most = first;
    for pair in points.windows(2) {
        let ((from, before), (until, after)) = (pair[0], pair[1]);
        if after < before - GIVEN_BACK {
            most = after;
        } else if after > most {
            rises.push((from.millis(), until.millis(), after - most));
            most = after;
        }
    }
    rises
}

/// A window's rise over time: from none at its start, where that is known
/// and comes before its first reading, and then each reading, as instants
/// and percents used.
fn points(window: &[Reading]) -> Vec<(Instant, f64)> {
    let mut points = Vec::with_capacity(window.len() + 1);
    let starts = window.last().and_then(|latest| latest.starts);
    if let (Some(starts), Some(first)) = (starts, window.first())
        && starts < first.at
    {
        points.push((starts, 0.0));
    }
    points.extend(window.iter().map(|reading| (reading.at, reading.used)));
    points
}

/// Each of `spent`'s share of the rises between `points`: each rise shared
/// out among `everyone` who spent in its stretch, by cost; `spent` being
/// among them, those that `drawing` admits take theirs.
fn shared_out(
    points: &[(Instant, f64)],
    everyone: &[(i64, i64)],
    spent: &[Spent],
    drawing: &dyn Fn(&Spent) -> bool,
) -> Vec<f64> {
    let mut each = vec![0.0; spent.len()];
    // `spent` is oldest first, those at no time known last, which no
    // stretch holds.
    let within =
        |at: i64| spent.partition_point(|spent| spent.at.is_some_and(|t| t.millis() <= at));
    for (from, until, rise) in rises(points) {
        // Everything spent in the stretch, whole dollars being far below
        // where a sum of nanodollars as f64 loses a cent.
        let start = everyone.partition_point(|(at, _)| *at <= from);
        let end = everyone.partition_point(|(at, _)| *at <= until);
        let whole: f64 = everyone[start..end]
            .iter()
            .map(|(_, cost)| *cost as f64)
            .sum();
        if whole <= 0.0 {
            continue;
        }
        for index in within(from)..within(until) {
            let spent = &spent[index];
            if let (true, Some(cost)) = (drawing(spent), spent.cost) {
                each[index] += rise * cost.max(0) as f64 / whole;
            }
        }
    }
    each
}

/// A limit's current window as its readings draw it.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitTrack {
    /// When the window started, where known.
    pub starts: Option<Instant>,
    /// When it resets, where known.
    pub resets: Option<Instant>,
    /// How much of it was used, in percent, oldest first: none at its start,
    /// where that is known, and then each reading.
    pub points: Vec<(Instant, f64)>,
}

/// A limit's current window, and what of it each session, project and model
/// took: approximately, each rise between readings shared out by cost.
#[derive(Clone, Debug, PartialEq)]
pub struct LimitWindow {
    /// The window, as its readings draw it.
    pub track: LimitTrack,
    /// What each session took of it, its subagents' within it, most first,
    /// in points of the limit's percent.
    pub sessions: Vec<(SessionKey, f64)>,
    /// What each project took of it, by its root directory, and `None` for
    /// sessions in none, most first.
    pub projects: Vec<(Option<String>, f64)>,
    /// What each model took of it, most first.
    pub models: Vec<(ModelKey, f64)>,
    /// The rises nothing on this Mac spent in: use elsewhere, as on the
    /// provider's website.
    pub elsewhere: f64,
    /// The rises only responses with no price spent in here: this Mac's
    /// use, which nothing tells how to share out, so no one's in
    /// particular, and none of it use elsewhere.
    pub unpriced: f64,
}

/// A limit's current window as read: the latest reading, and the points its
/// readings draw.
struct Current {
    latest: Reading,
    points: Vec<(Instant, f64)>,
}

/// The current window of `account`'s limit `key`, as of `now`; `None` when
/// the ledger holds no reading of it.
fn current(ledger: &Ledger, account: &str, key: &str, now: Instant) -> Result<Option<Current>> {
    let Some(since) = Instant::from_millis(now.millis().saturating_sub(LONGEST).max(0)) else {
        return Ok(None);
    };
    let Some(current) = windows_of(ledger, account, key, since)?.pop() else {
        return Ok(None);
    };
    let Some(latest) = current.last().cloned() else {
        return Ok(None);
    };
    let points = points(&current);
    if points.is_empty() {
        return Ok(None);
    }
    Ok(Some(Current { latest, points }))
}

impl Current {
    fn track(self) -> LimitTrack {
        LimitTrack {
            starts: self.latest.starts,
            resets: self.latest.resets,
            points: self.points,
        }
    }
}

/// The current window of `account`'s limit `key`, as of `now`, and what
/// used it; `None` when the ledger holds no reading of it. It reads every
/// response that could have drawn on it in the window.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the ledger or the cache cannot be read,
/// and [`Error::Corrupt`] when a session's key in the cache doesn't parse.
pub(crate) fn window(
    ledger: &Ledger,
    cache: &Connection,
    account: &str,
    key: &str,
    now: Instant,
) -> Result<Option<LimitWindow>> {
    let Some(current) = current(ledger, account, key, now)? else {
        return Ok(None);
    };
    let points = &current.points;
    let (Some(&(from, _)), Some(&(through, _))) = (points.first(), points.last()) else {
        return Ok(None);
    };
    let spending = spending_by(
        cache,
        account,
        current.latest.scope.as_deref(),
        from.millis(),
        through.millis(),
    )?;
    let spent = &spending.rows;
    let mut sessions: HashMap<i64, f64> = HashMap::new();
    // Each model's part, as each session's: of those that took one, however
    // small, as a free model's.
    let mut models: Vec<Option<f64>> = vec![None; spending.models.len()];
    let mut elsewhere = 0.0;
    let mut unpriced = 0.0;
    for (start, until, rise) in rises(points) {
        let first = spent.partition_point(|row| row.at <= start);
        let last = spent.partition_point(|row| row.at <= until);
        let stretch = &spent[first..last];
        let whole: f64 = stretch
            .iter()
            .filter_map(|row| row.cost)
            .map(|cost| cost as f64)
            .sum();
        if whole <= 0.0 {
            if stretch.is_empty() {
                elsewhere += rise;
            } else {
                unpriced += rise;
            }
            continue;
        }
        for row in stretch {
            if let Some(cost) = row.cost {
                let part = rise * cost as f64 / whole;
                *sessions.entry(row.session).or_default() += part;
                *models[row.model].get_or_insert(0.0) += part;
            }
        }
    }
    let mut taken = Taken::default();
    for (id, part) in sessions {
        let (root, project) = counts_in(cache, id)?;
        *taken.sessions.entry(root).or_default() += part;
        *taken.projects.entry(project).or_default() += part;
    }
    taken.models = spending
        .models
        .into_iter()
        .zip(models)
        .filter_map(|(model, part)| Some((model, part?)))
        .collect();
    taken.window(current.track(), elsewhere, unpriced).map(Some)
}

/// The key and project of the session the cache's session `id` counts in:
/// its own, or the one its subagent ran within.
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read.
pub(super) fn counts_in(cache: &Connection, id: i64) -> Result<(String, Option<String>)> {
    Ok(cache
        .prepare_cached(
            "SELECT r.key, r.project FROM session s JOIN session r ON r.key = s.root
             WHERE s.id = ?1",
        )?
        .query_row([id], |row| Ok((row.get(0)?, row.get(1)?)))?)
}

/// What each session, project and model took of a window, in points of its
/// limit's percent, by the keys the cache keeps them by.
#[derive(Default)]
struct Taken {
    sessions: HashMap<String, f64>,
    projects: HashMap<Option<String>, f64>,
    models: Vec<(String, f64)>,
}

impl Taken {
    /// The window `track` draws, with what each took, most first, and
    /// `elsewhere` and `unpriced` beside.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Corrupt`] when a session's key in the cache doesn't
    /// parse.
    fn window(self, track: LimitTrack, elsewhere: f64, unpriced: f64) -> Result<LimitWindow> {
        let most = |a: &f64, b: &f64| b.total_cmp(a);
        let mut sessions = self
            .sessions
            .into_iter()
            .map(|(key, share)| {
                SessionKey::parse(&key)
                    .map(|key| (key, share))
                    .ok_or_else(|| Error::corrupt("session key", key))
            })
            .collect::<Result<Vec<_>>>()?;
        sessions
            .sort_by(|a, b| most(&a.1, &b.1).then_with(|| a.0.to_string().cmp(&b.0.to_string())));
        let mut projects: Vec<(Option<String>, f64)> = self.projects.into_iter().collect();
        projects.sort_by(|a, b| most(&a.1, &b.1).then_with(|| a.0.cmp(&b.0)));
        let mut models: Vec<(ModelKey, f64)> = self
            .models
            .into_iter()
            .map(|(key, share)| (ModelKey::stored(key), share))
            .collect();
        models.sort_by(|a, b| most(&a.1, &b.1).then_with(|| a.0.as_str().cmp(b.0.as_str())));
        Ok(LimitWindow {
            track,
            sessions,
            projects,
            models,
            elsewhere,
            unpriced,
        })
    }
}

/// A response that could have drawn on a limit, as a window's reckoning
/// takes it.
pub(super) struct Row {
    pub(super) at: i64,
    /// What it cost at list prices, in nano-dollars; `None` with no price.
    pub(super) cost: Option<i64>,
    /// The cache's id of its own session.
    pub(super) session: i64,
    /// Its model, by its place among [`Spending::models`].
    model: usize,
}

/// Every response that drew on a limit over a stretch of time, oldest first,
/// and the models they were of.
pub(super) struct Spending {
    pub(super) rows: Vec<Row>,
    models: Vec<String>,
}

/// Every response that drew on `account`'s limit, of the model its `scope`
/// names where it names one, between `from` and `through`, oldest first,
/// with what it cost and the session it is of.
///
/// It reads the index alone, and each model's key once: reading each
/// response's session as its key and project, and its model as text, from
/// the table took 20 ms of a week's 56K responses; this takes 3.5
/// (2026-09-30).
///
/// # Errors
///
/// Returns [`Error::Ledger`] when the cache cannot be read.
pub(super) fn spending_by(
    cache: &Connection,
    account: &str,
    scope: Option<&str>,
    from: i64,
    through: i64,
) -> Result<Spending> {
    let mut statement = cache.prepare_cached(
        "SELECT u.at, u.cost, u.session, u.model_key FROM usage u INDEXED BY usage_account
         WHERE u.account = ?1 AND u.at > ?2 AND u.at <= ?3 ORDER BY u.at",
    )?;
    let mut rows = statement.query(params![account, from, through])?;
    let mut spending = Spending {
        rows: Vec::new(),
        models: Vec::new(),
    };
    // Each model's place among those read, and whether the scope holds it.
    let mut places: HashMap<String, Option<usize>> = HashMap::new();
    while let Some(row) = rows.next()? {
        let model_key = row.get_ref(3)?.as_str().map_err(rusqlite::Error::from)?;
        let place = match places.get(model_key) {
            Some(place) => *place,
            None => {
                let place = scope
                    .is_none_or(|scope| holds(model_key, scope))
                    .then_some(spending.models.len());
                if place.is_some() {
                    spending.models.push(model_key.to_owned());
                }
                places.insert(model_key.to_owned(), place);
                place
            }
        };
        let Some(model) = place else {
            continue;
        };
        spending.rows.push(Row {
            at: row.get(0)?,
            cost: row.get::<_, Option<i64>>(1)?.map(|cost| cost.max(0)),
            session: row.get(2)?,
            model,
        });
    }
    Ok(spending)
}

#[cfg(test)]
mod tests {
    use super::{Reading, Spent, points, shared_out, windows};
    use crate::time::Instant;

    const HOUR: i64 = 3_600_000;
    /// 2026-09-27T00:00:00Z.
    const DAY: i64 = 1_790_467_200_000;

    fn at(hours: i64) -> Instant {
        Instant::from_millis(DAY + hours * HOUR).unwrap()
    }

    fn reading(hours: i64, used: f64, starts: Option<i64>, resets: i64) -> Reading {
        Reading {
            key: "seven_day".to_owned(),
            name: "Weekly".to_owned(),
            scope: None,
            at: at(hours),
            used,
            starts: starts.map(at),
            resets: Some(at(resets)),
        }
    }

    fn spent(hours: i64, cost: Option<i64>) -> Spent {
        Spent {
            at: Some(at(hours)),
            account: Some("claude:work".to_owned()),
            model_key: "claude-opus-5-5".to_owned(),
            cost,
        }
    }

    #[test]
    fn each_rise_is_shared_by_what_each_response_spent_in_its_stretch() {
        // The window starts at hour 0 and is read at 2 (10%) and 4 (30%).
        // Hours 0-2 rose 10: ours spent 1 at hour 1 of the 4 spent then, so
        // 10 x 1/4 = 2.5. Hours 2-4 rose 20: ours spent 3 at hour 3 of the 3
        // spent then, so all 20. Ours come to 22.5.
        let window = vec![
            reading(2, 10.0, Some(0), 168),
            reading(4, 30.0, Some(0), 168),
        ];
        let points = points(&window);
        assert_eq!(points.first(), Some(&(at(0), 0.0)));
        let everyone = vec![(DAY + HOUR, 1), (DAY + HOUR, 3), (DAY + 3 * HOUR, 3)];
        let ours = vec![spent(1, Some(1)), spent(3, Some(3))];
        let each = shared_out(&points, &everyone, &ours, &|_| true);
        assert_eq!(each, vec![2.5, 20.0]);
    }

    #[test]
    fn a_fall_a_rise_with_no_one_spending_and_an_unpriced_response_share_nothing() {
        // 0-2 rose 5 while only an unpriced response of ours spent: nothing
        // here explains it. 2-4 fell by a point of rounding: nothing to
        // share. 4-6 rose to 10, 5 past the most read before, all of it
        // ours at 1 of 1 spent.
        let window = vec![
            reading(2, 5.0, Some(0), 168),
            reading(4, 4.0, Some(0), 168),
            reading(6, 10.0, Some(0), 168),
        ];
        let everyone = vec![(DAY + 5 * HOUR, 1)];
        let ours = vec![spent(1, None), spent(5, Some(1))];
        let each = shared_out(&points(&window), &everyone, &ours, &|_| true);
        assert_eq!(each, vec![0.0, 5.0]);
    }

    #[test]
    fn a_step_of_rounding_down_and_back_shares_nothing_and_use_given_back_starts_again() {
        // From none at hour 0: 10% at 2, a point lower at 4 and back at 6,
        // which is no use, 13% at 8, 3 more; then given back to 5% at 10,
        // and 8% at 12, 3 more. Ours spent all there was in each stretch:
        // 10 + 3 = 13 in hours 0-8, where each reading's rise from the one
        // before comes to 14, and 3 after.
        let window = vec![
            reading(2, 10.0, Some(0), 168),
            reading(4, 9.0, Some(0), 168),
            reading(6, 10.0, Some(0), 168),
            reading(8, 13.0, Some(0), 168),
            reading(10, 5.0, Some(0), 168),
            reading(12, 8.0, Some(0), 168),
        ];
        let everyone: Vec<(i64, i64)> = [1, 3, 5, 7, 11]
            .iter()
            .map(|hours| (DAY + hours * HOUR, 1))
            .collect();
        let ours: Vec<Spent> = [1, 3, 5, 7, 11]
            .iter()
            .map(|hours| spent(*hours, Some(1)))
            .collect();
        let each = shared_out(&points(&window), &everyone, &ours, &|_| true);
        assert_eq!(each, vec![10.0, 0.0, 0.0, 3.0, 3.0]);
    }

    #[test]
    fn a_window_whose_start_is_unknown_rises_only_from_its_first_reading() {
        let window = vec![reading(2, 10.0, None, 168), reading(4, 30.0, None, 168)];
        assert_eq!(points(&window).first(), Some(&(at(2), 10.0)));
    }

    #[test]
    fn readings_part_into_windows_where_their_resets_part() {
        // A countdown drifts by seconds within one window; the next window
        // resets a week later.
        let mut drifted = reading(3, 20.0, Some(0), 168);
        drifted.resets = Some(Instant::from_millis(at(168).millis() + 4_000).unwrap());
        let windows = windows(vec![
            reading(2, 10.0, Some(0), 168),
            drifted,
            reading(170, 1.0, Some(168), 336),
        ]);
        let sizes: Vec<usize> = windows.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![2, 1]);
    }
}
