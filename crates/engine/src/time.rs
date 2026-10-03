//! Instants, stretches of time, and the quarter hours and local hours, days,
//! weeks and months usage is totalled over.

use std::fmt;

use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned};

/// A moment, as milliseconds since the Unix epoch in UTC.
///
/// Agents write instants as RFC 3339 text or as Unix times in seconds or
/// milliseconds; every one becomes this. Local days and hours are worked out
/// from it only when a query asks for them, in the zone in effect then, so an
/// instant never depends on where or when it was read.
///
/// Every instant lies within the range `jiff` represents, from early in the
/// year -9999 to late in 9999, so any two can be subtracted without overflow.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(i64);

impl Instant {
    /// The instant `millis` after the epoch, when it lies within the years
    /// -9999 to 9999 as `jiff` bounds them.
    pub fn from_millis(millis: i64) -> Option<Instant> {
        jiff::Timestamp::from_millisecond(millis)
            .ok()
            .map(Instant::from)
    }

    /// The instant `seconds` after the epoch, when it lies within the years
    /// -9999 to 9999 as `jiff` bounds them.
    pub(crate) fn from_seconds(seconds: i64) -> Option<Instant> {
        jiff::Timestamp::from_second(seconds)
            .ok()
            .map(Instant::from)
    }

    /// Read RFC 3339 text such as `2026-09-14T08:09:23.404Z`, keeping whole
    /// milliseconds. Text with no offset names no instant and is refused.
    pub fn parse(text: &str) -> Option<Instant> {
        text.parse::<jiff::Timestamp>().ok().map(Instant::from)
    }

    /// The start of a UTC day written `2026-07-24`, or of a month written
    /// `2026-07`, as models.dev dates its entries.
    pub(crate) fn from_date(text: &str) -> Option<Instant> {
        let date: jiff::civil::Date = match text.len() {
            10 => text.parse().ok()?,
            7 => format!("{text}-01").parse().ok()?,
            _ => return None,
        };
        Some(Instant::from(
            date.to_zoned(jiff::tz::TimeZone::UTC).ok()?.timestamp(),
        ))
    }

    /// Now, by the system clock.
    pub fn now() -> Instant {
        Instant::from(jiff::Timestamp::now())
    }

    /// Milliseconds since the epoch.
    pub const fn millis(self) -> i64 {
        self.0
    }

    /// This instant as `jiff` keeps one, which every instant can be, as
    /// each lies within its range.
    pub fn timestamp(self) -> Timestamp {
        Timestamp::from_millisecond(self.0).expect("every instant lies within jiff's range")
    }
}

impl From<Timestamp> for Instant {
    /// The instant `at` is, to the whole millisecond toward the epoch.
    fn from(at: Timestamp) -> Instant {
        Instant(at.as_millisecond())
    }
}

impl fmt::Display for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.timestamp())
    }
}

/// A time zone, which decides where local days and hours begin.
#[derive(Clone, Debug)]
pub struct Zone(TimeZone);

impl Zone {
    /// This Mac's zone.
    pub fn system() -> Zone {
        Zone(TimeZone::system())
    }

    /// The zone the IANA database names `name`, such as `America/New_York`.
    pub fn named(name: &str) -> Option<Zone> {
        TimeZone::get(name).ok().map(Zone)
    }

    /// The local moment `at` falls in.
    fn local(&self, at: Instant) -> Zoned {
        at.timestamp().to_zoned(self.0.clone())
    }
}

impl From<TimeZone> for Zone {
    /// The zone `zone` is, for a caller that also tells times in it.
    fn from(zone: TimeZone) -> Zone {
        Zone(zone)
    }
}

/// A stretch of time: from `from`, inclusive, until `until`, exclusive. An
/// open end reaches as far as there is usage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    /// The first instant within it.
    pub from: Option<Instant>,
    /// The first instant after it.
    pub until: Option<Instant>,
}

/// Local lengths of time usage can be totalled over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bucket {
    /// Local hours.
    Hour,
    /// Local days.
    Day,
    /// Local weeks, from Monday.
    Week,
    /// Local months.
    Month,
}

/// How long ago the local clock last read a whole hour at `local`, if its
/// offset has stood since: its minutes, seconds and milliseconds.
fn past_the_hour(local: &Zoned) -> i64 {
    i64::from(local.minute()) * 60_000
        + i64::from(local.second()) * 1_000
        + i64::from(local.millisecond())
}

/// Whether `zone`'s offset changes at `change`, as a transition can also
/// change only a name or whether it is summer time.
fn offset_changes(zone: &Zone, change: &jiff::tz::TimeZoneTransition) -> bool {
    change
        .timestamp()
        .checked_sub(1.nanosecond())
        .is_ok_and(|before| zone.0.to_offset(before) != change.offset())
}

/// The start of the local `every` that `at` falls in.
///
/// A local hour starts where the clock reads a whole hour, and where its
/// offset changes: a change of half an hour, as Lord Howe Island's, leaves
/// the clock reading an hour from its half past, and one back by a whole
/// hour reads the same hour again, an hour of its own each time. Hours and
/// the buckets [`next`] steps through come from this one rule, so every
/// instant falls in a bucket.
pub(crate) fn start_of(every: Bucket, at: Instant, zone: &Zone) -> Option<Instant> {
    let local = zone.local(at);
    let start = match every {
        Bucket::Hour => {
            let whole = at.millis() - past_the_hour(&local);
            // The last change of offset at `at` or before it, when it falls
            // after that whole hour.
            let after = Timestamp::from_millisecond(at.millis().checked_add(1)?).ok()?;
            let changed = zone
                .0
                .preceding(after)
                .take_while(|change| change.timestamp().as_millisecond() > whole)
                .find(|change| offset_changes(zone, change))
                .map(|change| change.timestamp().as_millisecond());
            return Instant::from_millis(changed.map_or(whole, |changed| changed.max(whole)));
        }
        Bucket::Day => local.start_of_day().ok()?,
        Bucket::Week => {
            let back = i64::from(local.weekday().to_monday_zero_offset());
            local.checked_sub(back.days()).ok()?.start_of_day().ok()?
        }
        Bucket::Month => local.first_of_month().ok()?.start_of_day().ok()?,
    };
    Some(Instant::from(start.timestamp()))
}

/// The start of the local `every` after the one starting at `start`.
pub(crate) fn next(every: Bucket, start: Instant, zone: &Zone) -> Option<Instant> {
    let step = match every {
        Bucket::Hour => {
            // The next whole hour on the clock as it reads now, or the next
            // change of offset before it.
            let whole = start.millis() - past_the_hour(&zone.local(start)) + 3_600_000;
            let changed = zone
                .0
                .following(start.timestamp())
                .take_while(|change| change.timestamp().as_millisecond() < whole)
                .find(|change| offset_changes(zone, change))
                .map(|change| change.timestamp().as_millisecond());
            return Instant::from_millis(changed.unwrap_or(whole));
        }
        Bucket::Day => 1.day(),
        Bucket::Week => 1.week(),
        Bucket::Month => 1.month(),
    };
    let later = zone.local(start).checked_add(step).ok()?;
    start_of(every, Instant::from(later.timestamp()), zone)
}

/// The latest local Monday at 9 AM at or before `at`, in `zone`: when the
/// weekly recap is due. Where the clocks skip 9 AM that Monday, the moment
/// they read after it.
pub(crate) fn monday_morning(at: Instant, zone: &Zone) -> Option<Instant> {
    let local = zone.local(at);
    let back = i64::from(local.weekday().to_monday_zero_offset());
    let monday = local.date().checked_sub(back.days()).ok()?;
    let morning = |date: jiff::civil::Date| {
        let zoned = date.at(9, 0, 0, 0).to_zoned(zone.0.clone()).ok()?;
        Some(Instant::from(zoned.timestamp()))
    };
    let this = morning(monday)?;
    if this <= at {
        Some(this)
    } else {
        morning(monday.checked_sub(1.week()).ok()?)
    }
}

#[cfg(test)]
mod tests {
    use super::{Bucket, Instant, Zone, monday_morning, next, start_of};

    fn at(text: &str) -> Instant {
        Instant::parse(text).unwrap()
    }

    /// The start of every local `every` from the one `from` falls in to the
    /// one before `until`, each the [`next`] after the one before.
    fn buckets(every: Bucket, from: Instant, until: Instant, zone: &Zone) -> Vec<Instant> {
        let mut starts = Vec::new();
        let mut current = start_of(every, from, zone);
        while let Some(start) = current.filter(|start| *start < until) {
            starts.push(start);
            current = next(every, start, zone).filter(|next| *next > start);
        }
        starts
    }

    #[test]
    fn rfc3339_text_names_an_instant_to_the_millisecond_only_with_its_offset() {
        // 2026-09-14T08:09:23Z is 1,789,373,363 seconds after the epoch.
        let at = Instant::parse("2026-09-14T08:09:23.404Z").unwrap();
        assert_eq!(at.millis(), 1_789_373_363_404);
        let offset = Instant::parse("2026-09-14T10:09:23.404+02:00").unwrap();
        assert_eq!(offset, at);
        assert_eq!(Instant::parse("2026-09-14T08:09:23"), None);
        assert_eq!(Instant::parse("yesterday"), None);
    }

    #[test]
    fn a_catalog_date_is_the_start_of_its_day_or_month_in_utc() {
        // 2026-07-24T00:00:00Z is 1,784,851,200 seconds after the epoch, and
        // 2026-07-01 is 23 days earlier.
        assert_eq!(
            Instant::from_date("2026-07-24").unwrap().millis(),
            1_784_851_200_000
        );
        assert_eq!(
            Instant::from_date("2026-07").unwrap().millis(),
            1_784_851_200_000 - 23 * 86_400_000
        );
        assert_eq!(Instant::from_date("24 July"), None);
    }

    #[test]
    fn unix_times_beyond_the_year_9999_are_refused() {
        // 253,402,300,800 seconds is 10000-01-01T00:00:00Z, and
        // 253,399,622,400 is 9999-12-01T00:00:00Z.
        assert_eq!(Instant::from_seconds(253_402_300_800), None);
        assert!(Instant::from_seconds(253_399_622_400).is_some());
        assert_eq!(Instant::from_millis(i64::MAX), None);
    }

    #[test]
    fn local_days_follow_the_clocks_through_their_changes() {
        let zone = Zone::named("America/New_York").unwrap();
        // Clocks went back at 2:00 on 2026-11-01, making that day 25 hours.
        let days = buckets(
            Bucket::Day,
            at("2026-10-31T12:00:00Z"),
            at("2026-11-03T05:00:00Z"),
            &zone,
        );
        let starts: Vec<i64> = days.iter().map(|day| day.millis()).collect();
        assert_eq!(
            starts,
            [
                at("2026-10-31T04:00:00Z").millis(),
                at("2026-11-01T04:00:00Z").millis(),
                at("2026-11-02T05:00:00Z").millis(),
            ]
        );
        assert_eq!(starts[2] - starts[1], 25 * 3_600_000);
    }

    #[test]
    fn a_local_hour_starts_at_minute_zero_in_a_zone_off_the_hour() {
        let zone = Zone::named("Asia/Kathmandu").unwrap();
        // Kathmandu is 5:45 ahead: 10:20 UTC is 16:05 there, in the hour from 16:00.
        let hour = start_of(Bucket::Hour, at("2026-09-23T10:20:00Z"), &zone).unwrap();
        assert_eq!(hour, at("2026-09-23T10:15:00Z"));
    }

    /// The hours `buckets` gives from `from` until `until` in `zone`, and
    /// the check that every quarter hour between falls in one of them: the
    /// hour `start_of` puts it in is the last given at or before it.
    fn hours_holding_every_quarter(zone: &Zone, from: &str, until: &str) -> Vec<Instant> {
        let (from, until) = (at(from), at(until));
        let hours = buckets(Bucket::Hour, from, until, zone);
        let mut quarter = from.millis();
        while quarter < until.millis() {
            let within = Instant::from_millis(quarter).unwrap();
            let hour = start_of(Bucket::Hour, within, zone).unwrap();
            let last = hours.iter().rev().find(|start| **start <= within);
            assert_eq!(Some(&hour), last, "the hour of {within}");
            quarter += 900_000;
        }
        hours
    }

    #[test]
    fn a_half_hour_clock_change_starts_a_local_hour_of_its_own() {
        let zone = Zone::named("Australia/Lord_Howe").unwrap();
        // Lord Howe is 10:30 ahead, and 11 in summer. On 2026-10-04 at 2:00
        // the clock went on to 2:30, at 15:30 UTC: the hour it then read,
        // from 2:30, lasted until 3:00, at 16:00 UTC.
        assert_eq!(
            hours_holding_every_quarter(&zone, "2026-10-03T13:30:00Z", "2026-10-03T18:00:00Z"),
            [
                at("2026-10-03T13:30:00Z"), // 0:00
                at("2026-10-03T14:30:00Z"), // 1:00
                at("2026-10-03T15:30:00Z"), // 2:30
                at("2026-10-03T16:00:00Z"), // 3:00
                at("2026-10-03T17:00:00Z"), // 4:00
            ]
        );
        // Use at 16:15 UTC, 3:15 there, is in the hour from 3:00.
        assert_eq!(
            start_of(Bucket::Hour, at("2026-10-03T16:15:00Z"), &zone).unwrap(),
            at("2026-10-03T16:00:00Z")
        );
        // On 2026-04-05 at 2:00 the clock went back to 1:30, at 15:00 UTC:
        // the half hour from 1:30 came round again, an hour of its own.
        assert_eq!(
            hours_holding_every_quarter(&zone, "2026-04-04T13:00:00Z", "2026-04-04T17:30:00Z"),
            [
                at("2026-04-04T13:00:00Z"), // 0:00
                at("2026-04-04T14:00:00Z"), // 1:00
                at("2026-04-04T15:00:00Z"), // 1:30 again
                at("2026-04-04T15:30:00Z"), // 2:00
                at("2026-04-04T16:30:00Z"), // 3:00
            ]
        );
    }

    #[test]
    fn a_whole_hour_clock_change_keeps_whole_local_hours() {
        let zone = Zone::named("America/New_York").unwrap();
        // On 2026-03-08 at 2:00 the clock went on to 3:00, at 7:00 UTC.
        assert_eq!(
            hours_holding_every_quarter(&zone, "2026-03-08T05:00:00Z", "2026-03-08T09:00:00Z"),
            [
                at("2026-03-08T05:00:00Z"), // 0:00
                at("2026-03-08T06:00:00Z"), // 1:00
                at("2026-03-08T07:00:00Z"), // 3:00
                at("2026-03-08T08:00:00Z"), // 4:00
            ]
        );
        // On 2026-11-01 at 2:00 it went back to 1:00, at 6:00 UTC, and the
        // hour from 1:00 came round again.
        assert_eq!(
            hours_holding_every_quarter(&zone, "2026-11-01T04:00:00Z", "2026-11-01T08:00:00Z"),
            [
                at("2026-11-01T04:00:00Z"), // 0:00
                at("2026-11-01T05:00:00Z"), // 1:00
                at("2026-11-01T06:00:00Z"), // 1:00 again
                at("2026-11-01T07:00:00Z"), // 2:00
            ]
        );
    }

    #[test]
    fn weeks_start_on_monday_and_months_on_the_first() {
        let zone = Zone::named("UTC").unwrap();
        // 2026-09-23 was a Wednesday.
        assert_eq!(
            start_of(Bucket::Week, at("2026-09-23T15:00:00Z"), &zone).unwrap(),
            at("2026-09-21T00:00:00Z")
        );
        assert_eq!(
            start_of(Bucket::Month, at("2026-09-23T15:00:00Z"), &zone).unwrap(),
            at("2026-09-01T00:00:00Z")
        );
    }

    #[test]
    fn the_weekly_recap_is_due_from_nine_on_monday_morning_where_the_clock_is() {
        // Monday 2026-09-28. Los Angeles is seven hours behind UTC then, so
        // 9 AM there is 16:00 UTC; Tokyo is nine ahead, so 9 AM there is
        // midnight UTC.
        let los_angeles = Zone::named("America/Los_Angeles").unwrap();
        let tokyo = Zone::named("Asia/Tokyo").unwrap();
        let due = |now: &str, zone: &Zone| monday_morning(at(now), zone);
        // A minute before 9 AM in Los Angeles, the last one due was the
        // Monday before's.
        assert_eq!(
            due("2026-09-28T15:59:00Z", &los_angeles),
            Some(at("2026-09-21T16:00:00Z"))
        );
        assert_eq!(
            due("2026-09-28T16:00:00Z", &los_angeles),
            Some(at("2026-09-28T16:00:00Z"))
        );
        // All week, until the next Monday's.
        assert_eq!(
            due("2026-10-05T15:59:00Z", &los_angeles),
            Some(at("2026-09-28T16:00:00Z"))
        );
        // At 03:00 UTC it is noon on Monday in Tokyo, but 8 PM on Sunday in
        // Los Angeles.
        assert_eq!(
            due("2026-09-28T03:00:00Z", &tokyo),
            Some(at("2026-09-28T00:00:00Z"))
        );
        assert_eq!(
            due("2026-09-28T03:00:00Z", &los_angeles),
            Some(at("2026-09-21T16:00:00Z"))
        );
        assert_eq!(
            due("2026-09-27T23:59:00Z", &tokyo),
            Some(at("2026-09-21T00:00:00Z"))
        );
    }
}
