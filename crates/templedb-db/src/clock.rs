//! The clock seam: one injectable source of "now", and the age
//! arithmetic Python does in SQL.
//!
//! # Why this exists before the command that needs it
//!
//! The write-path design document (*The write path: what porting it
//! actually requires*, 2026-10-15) names this as the first of three
//! pieces of work, ahead of any write. Two separate things need it:
//!
//! - **`summary` cannot be verified without it.** Its output carries
//!   relative ages computed from the clock, so two runs a minute apart
//!   disagree with each other and a byte diff against Python is
//!   meaningless. The README has deferred the command for exactly that
//!   reason since the porting order was written.
//! - **A state-diff harness cannot verify a write without it.**
//!   `commit_hash` is `sha256(f"{id}-{message}-{time.time()}")`, and
//!   every write stamps `datetime('now')`. With a real clock on both
//!   sides, the columns most likely to be wrong are the ones a diff has
//!   to exclude.
//!
//! The document's advice is to build the seam against a *read* command,
//! where being wrong is free, which is what pins it here rather than in
//! the first writer.
//!
//! # Why the arithmetic is reimplemented rather than delegated
//!
//! Python asks the database:
//!
//! ```sql
//! SELECT (julianday('now') - julianday(?)) * 24 AS hours
//! ```
//!
//! That is a SQLite function. Postgres has no `julianday`, so keeping
//! the computation in SQL would mean two expressions that have to agree
//! about leap years and about what an unparseable timestamp does — the
//! same shape as the `search content` problem, where two engines'
//! answers to one question differed and the fix was to stop asking
//! them. The arithmetic is 20 lines and has no dialect.
//!
//! It is also the one piece here that *must* be exact rather than
//! merely reasonable, because every age string in `summary` is derived
//! from it. So it is checked against CPython driving SQLite, as an
//! `#[ignore]`d oracle test in the same style as the `difflib` one —
//! see `tests/clock_parity.rs`.
//!
//! # What a timestamp is in this database
//!
//! Every column this reads is written by SQLite's `CURRENT_TIMESTAMP`
//! or `datetime('now')`, both of which produce `YYYY-MM-DD HH:MM:SS`
//! in **UTC**. Measured across the five tables `summary` reads: 19
//! characters, no exceptions, no offsets. The parser accepts the `T`
//! separator, fractional seconds, a trailing `Z` and a `±HH:MM` offset
//! as well, because SQLite does and because a column this code does
//! not read yet may carry one. The offset is applied rather than
//! ignored — `06:30:00+02:00` is 04:30 UTC to SQLite, a *different
//! instant* and not a decoration — which is the only reading that
//! keeps a future caller from getting a two-hour error silently.
//!
//! An unparseable timestamp yields `None`, which is what SQLite's
//! `julianday` returns for one it cannot read. Python turns that NULL
//! into `('unknown', 'stale')` and so does [`Clock::age_hint`].
//!
//! # The float error is part of the answer
//!
//! A Julian Day is about 2.46 million, so an `f64` resolves it to
//! roughly 47 microseconds — and `_age_hint` truncates. The two meet
//! at every exact boundary, and the result is visible in the output:
//!
//! ```text
//! sqlite> SELECT CAST((julianday('2026-10-17 12:00:00')
//!                    - julianday('2026-10-17 11:59:00')) * 24 * 60 AS INTEGER);
//! 0
//! sqlite> SELECT CAST((julianday('2026-10-17 12:00:00')
//!                    - julianday('2026-10-17 11:00:00')) * 24 AS INTEGER);
//! 0
//! ```
//!
//! So a row stamped exactly one minute ago renders `0min ago`, and one
//! stamped exactly an hour ago does not reach the `hours < 1` test's
//! other side — it renders `59min ago`, classified fresh. Both are
//! what `templedb summary` prints today.
//!
//! That is why this module holds the instant as **integer
//! milliseconds** and divides by 86,400,000 to get a Julian Day, which
//! is what SQLite does internally (`sqlite3_int64 iJD`). Computing the
//! same quantity a more natural way — add the day fraction to the
//! epoch constant — lands a few ulp off, which is invisible everywhere
//! except at exactly the boundaries where truncation makes it a
//! different string. Reproducing the arithmetic is the only way to
//! reproduce the output.

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

/// Override "now" with a fixed `YYYY-MM-DD HH:MM:SS` UTC timestamp.
///
/// `TEMPLEDB_RS_`-prefixed rather than `TEMPLEDB_`, because it must not
/// be mistaken for something the Python implementation reads: a
/// variable that silently pins one binary's clock and not the other's
/// would make a parity run look like agreement.
pub const NOW_ENV: &str = "TEMPLEDB_RS_NOW";

/// Milliseconds from the start of the Julian era to the Unix epoch:
/// `2440587.5 * 86_400_000`, exactly, and well inside an `i64`.
const JD_EPOCH_MS: i64 = 210_866_760_000_000;

const MS_PER_DAY: i64 = 86_400_000;

/// How fresh something is, as Python's `_age_hint` classifies it.
///
/// The thresholds are a caller's argument rather than a constant here,
/// because the two callers disagree by two orders of magnitude: the
/// doctor pane considers an hour stale and the reconcile pane allows a
/// week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Ok,
    Stale,
    Ancient,
    Never,
}

/// A rendered age and its classification: `("3h ago", Stale)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Age {
    pub text: String,
    pub freshness: Freshness,
}

/// One instant, injectable.
///
/// Held as integer milliseconds since the start of the Julian era,
/// which is SQLite's own representation — see the note on float error
/// above. Converting to a Julian Day happens at the subtraction, once,
/// so the rounding is in one place and is the same rounding SQLite
/// performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    ms: i64,
}

impl Clock {
    /// The wall clock, as `julianday('now')` would read it.
    pub fn system() -> Clock {
        let ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            // Before 1970 the only honest answer is a negative offset,
            // and `duration_since` cannot express one. A clock set that
            // far back is not a case worth a branch at every call site.
            .map_or(0, |d| d.as_millis() as i64);
        Clock {
            ms: JD_EPOCH_MS + ms,
        }
    }

    /// A fixed instant, from a `YYYY-MM-DD HH:MM:SS` UTC timestamp.
    pub fn at(timestamp: &str) -> Result<Clock> {
        let ms =
            julian_day_ms(timestamp).with_context(|| format!("not a timestamp: {timestamp:?}"))?;
        Ok(Clock { ms })
    }

    /// This instant as a Julian Day, the way `julianday()` returns it.
    fn jd(&self) -> f64 {
        self.ms as f64 / MS_PER_DAY as f64
    }

    /// [`Clock::at`] the value of `TEMPLEDB_RS_NOW`, or
    /// [`Clock::system`] when it is unset.
    ///
    /// An empty value falls back rather than erroring, matching how
    /// `conn.rs` treats an empty `TEMPLEDB_PATH`: an exported-but-empty
    /// variable is how a shell says "unset", and reading it as a
    /// malformed timestamp would fail every command in a directory
    /// whose `.envrc` mentions it.
    pub fn from_env() -> Result<Clock> {
        match std::env::var(NOW_ENV) {
            Ok(v) if !v.trim().is_empty() => Clock::at(v.trim()),
            _ => Ok(Clock::system()),
        }
    }

    /// This instant as `YYYY-MM-DD HH:MM:SS`, the format every
    /// timestamp column in the database is written in.
    ///
    /// Seconds are truncated, not rounded, because `datetime('now')`
    /// truncates.
    pub fn now_timestamp(&self) -> String {
        timestamp_of_ms(self.ms)
    }

    /// This instant as `time.time()` would return it: seconds since the
    /// Unix epoch, fractional.
    ///
    /// The one place a Python *float* rather than a SQLite timestamp is
    /// the thing being reproduced. `vcs commit`'s id is
    /// `sha256(f"{slug}:{branch}:{message}:{time.time()}")[:16].upper()`,
    /// so the float's decimal rendering is an input to a hash and a
    /// digit's difference changes every character of the answer.
    ///
    /// Resolution is milliseconds, not microseconds, because that is
    /// what this type holds — see the note at the top of the module on
    /// why the representation is SQLite's and not a more natural one.
    /// `time.time()` is finer, so the two implementations cannot produce
    /// the same id for the same *wall clock* instant; they produce the
    /// same id for the same *stated* instant, which is what
    /// [`NOW_ENV`] pins and what a test can assert.
    pub fn unix_seconds(&self) -> f64 {
        (self.ms - JD_EPOCH_MS) as f64 / 1000.0
    }

    /// `(julianday('now') - julianday(ts)) * 24`, to the bit.
    ///
    /// Both operands are rounded to `f64` *before* the subtraction,
    /// because that is the order the SQL does it in and the order is
    /// what decides the last digit.
    ///
    /// `None` for a timestamp SQLite could not parse either.
    pub fn hours_since(&self, timestamp: &str) -> Option<f64> {
        julian_day(timestamp).map(|then| (self.jd() - then) * 24.0)
    }

    /// `CAST(julianday('now') - julianday(ts) AS INT)`, which is what
    /// `reconciliation_service.listing` computes for its idle column.
    ///
    /// Not `hours_since(..) / 24.0`. The SQL subtracts Julian Days and
    /// truncates; multiplying by 24 and dividing again is two more
    /// roundings, and on a boundary those decide the digit. The same
    /// reasoning as [`Clock::hours_since`] — see the note on float
    /// error at the top of this module.
    pub fn days_since(&self, timestamp: &str) -> Option<i64> {
        julian_day(timestamp).map(|then| (self.jd() - then) as i64)
    }

    /// This instant as `date('now')` renders it: `YYYY-MM-DD`, UTC.
    ///
    /// Compared as a string against `review_after`, which is how the
    /// SQL does it (`review_after <= date('now')`) — so the comparison
    /// is lexicographic and this must be zero-padded, which
    /// [`timestamp_of_ms`] guarantees.
    pub fn today(&self) -> String {
        self.now_timestamp()[..10].to_string()
    }

    /// Python's `_age_hint`, reproduced branch for branch.
    ///
    /// Two details are load-bearing and neither is obvious from the
    /// output:
    ///
    /// - **`int()` truncates toward zero**, so a timestamp in the
    ///   future — which this database does contain, since ingest rows
    ///   are stamped in UTC and can be ahead of a local reading — gives
    ///   `-4min ago` rather than `-5min ago`. Rust's `as i64` truncates
    ///   the same way, which is why there is no `floor` here.
    /// - **The `ancient` cutoff is `threshold * 24`, not 24.** With the
    ///   reconcile pane's threshold of 168 hours that is 4,032 hours —
    ///   168 *days* — so a machine unreconciled for five months still
    ///   renders in hours rather than days. It reads like a bug and it
    ///   is on stdout, so it is reproduced rather than fixed.
    pub fn age_hint(&self, timestamp: Option<&str>, threshold_hours: f64) -> Age {
        let Some(timestamp) = timestamp.filter(|t| !t.is_empty()) else {
            return Age {
                text: "never".to_string(),
                freshness: Freshness::Never,
            };
        };
        let Some(hours) = self.hours_since(timestamp) else {
            return Age {
                text: "unknown".to_string(),
                freshness: Freshness::Stale,
            };
        };
        if hours < 1.0 {
            Age {
                text: format!("{}min ago", (hours * 60.0) as i64),
                freshness: Freshness::Ok,
            }
        } else if hours < threshold_hours {
            Age {
                text: format!("{}h ago", hours as i64),
                freshness: Freshness::Ok,
            }
        } else if hours < threshold_hours * 24.0 {
            Age {
                text: format!("{}h ago", hours as i64),
                freshness: Freshness::Stale,
            }
        } else {
            Age {
                text: format!("{}d ago", (hours / 24.0) as i64),
                freshness: Freshness::Ancient,
            }
        }
    }
}

/// SQLite's `julianday()`, as an exact count of milliseconds since the
/// start of the Julian era — SQLite's own `iJD`.
///
/// `YYYY-MM-DD` with an optional ` HH:MM[:SS[.fff]]`, the separator
/// being a space or `T`, and an optional `Z` or `±HH:MM` suffix.
/// Anything else is `None`, which is what SQLite returns rather than
/// raising.
///
/// The suffixes are here because SQLite reads them as a *different
/// instant*, not as decoration: `julianday('… 06:30:00+02:00')` is
/// 04:30 UTC. Rejecting them would have been defensible — no column
/// this port reads carries one — but a parser that silently differs
/// from the one it is reproducing is the kind of gap that surfaces
/// years later through a column nobody checked.
pub fn julian_day_ms(timestamp: &str) -> Option<i64> {
    let s = timestamp.trim();
    let bytes = s.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let year: i64 = field(s.get(0..4)?)?;
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let month: i64 = field(s.get(5..7)?)?;
    let day: i64 = field(s.get(8..10)?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let (mut hour, mut minute, mut ms_of_second) = (0i64, 0i64, 0i64);
    let mut offset_minutes = 0i64;
    if bytes.len() > 10 {
        if bytes[10] != b' ' && bytes[10] != b'T' {
            return None;
        }
        let mut time = s.get(11..)?;

        // Trailing zone, stripped before the time is read so the
        // seconds field does not have to know about it.
        if let Some(rest) = time.strip_suffix('Z').or_else(|| time.strip_suffix('z')) {
            time = rest;
        } else if let Some(at) = time.rfind(['+', '-']) {
            let (rest, zone) = time.split_at(at);
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let zone = &zone[1..];
            let (zh, zm) = zone.split_once(':')?;
            offset_minutes = sign * (field(zh)? * 60 + field(zm)?);
            time = rest;
        }

        let tb = time.as_bytes();
        if tb.len() < 5 || tb[2] != b':' {
            return None;
        }
        hour = field(time.get(0..2)?)?;
        minute = field(time.get(3..5)?)?;
        if tb.len() > 5 {
            if tb[5] != b':' {
                return None;
            }
            // Fractional seconds included: SQLite accepts them, keeps
            // milliseconds, and the subtraction is in days — dropping
            // them would shift an age by up to a second.
            let seconds: f64 = time.get(6..)?.parse().ok()?;
            if !(0.0..60.0).contains(&seconds) {
                return None;
            }
            ms_of_second = (seconds * 1000.0).round() as i64;
        }
        if hour > 24 || minute > 59 {
            return None;
        }
    }

    let days = days_from_civil(year, month, day);
    Some(
        JD_EPOCH_MS
            + days * MS_PER_DAY
            + (hour * 60 + minute - offset_minutes) * 60_000
            + ms_of_second,
    )
}

/// [`julian_day_ms`] as the `f64` `julianday()` hands back.
pub fn julian_day(timestamp: &str) -> Option<f64> {
    julian_day_ms(timestamp).map(|ms| ms as f64 / MS_PER_DAY as f64)
}

/// Milliseconds since the Julian epoch back to `YYYY-MM-DD HH:MM:SS`,
/// truncating sub-second precision the way `datetime('now')` does.
pub fn timestamp_of_ms(ms: i64) -> String {
    let unix_ms = ms - JD_EPOCH_MS;
    let days = unix_ms.div_euclid(MS_PER_DAY);
    let rem = unix_ms.rem_euclid(MS_PER_DAY) / 1000;
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Days since 1970-01-01 for a proleptic Gregorian date.
///
/// Howard Hinnant's `days_from_civil`, which is the algorithm SQLite's
/// own date code is equivalent to over this range. Written out rather
/// than pulled in: `chrono` would be the port's first dependency beyond
/// `anyhow`, the two drivers and `clap`, for twenty lines that have an
/// exact test oracle.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A fixed-width numeric field: digits only, so `julian_day` rejects
/// `20x6-01-01` rather than reading it as 20.
fn field(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchored against values SQLite prints, so this test fails if the
    /// epoch constant or the noon offset is ever "simplified".
    #[test]
    fn julian_day_matches_known_values() {
        assert_eq!(julian_day("1970-01-01 00:00:00"), Some(2_440_587.5));
        assert_eq!(julian_day("2000-01-01 12:00:00"), Some(2_451_545.0));
        assert_eq!(julian_day("2026-10-17 00:00:00"), Some(2_461_330.5));
        // Leap day, and the century rule either side of it.
        assert_eq!(julian_day("2000-02-29 00:00:00"), Some(2_451_603.5));
        assert_eq!(julian_day("1900-03-01 00:00:00"), Some(2_415_079.5));
    }

    #[test]
    fn the_separator_may_be_a_t_and_seconds_may_be_fractional() {
        assert_eq!(
            julian_day("2026-10-17T06:30:00"),
            julian_day("2026-10-17 06:30:00")
        );
        let half = julian_day_ms("2026-10-17 06:30:00.500").unwrap();
        let whole = julian_day_ms("2026-10-17 06:30:00").unwrap();
        assert_eq!(half - whole, 500);
    }

    /// SQLite reads a zone suffix as a different instant, so this is
    /// not cosmetic: `+02:00` is two hours *earlier* in UTC.
    #[test]
    fn a_zone_suffix_moves_the_instant() {
        let utc = julian_day_ms("2026-10-17 06:30:00").unwrap();
        assert_eq!(julian_day_ms("2026-10-17 06:30:00Z"), Some(utc));
        assert_eq!(
            julian_day_ms("2026-10-17 06:30:00+02:00"),
            Some(utc - 2 * 3_600_000)
        );
        assert_eq!(
            julian_day_ms("2026-10-17 06:30:00-05:30"),
            Some(utc + 5 * 3_600_000 + 1_800_000)
        );
    }

    /// Returning `None` rather than erroring is the contract: SQLite's
    /// `julianday` yields NULL for these and Python renders that as
    /// `unknown`.
    #[test]
    fn unparseable_timestamps_are_none_not_errors() {
        for bad in [
            "",
            "not a date",
            "2026-13-01 00:00:00",
            "2026-10-32 00:00:00",
            "20x6-10-17",
            "2026-10-17X06:30:00",
            "2026-10-17 06:30:99",
        ] {
            assert_eq!(julian_day(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_timestamp_survives_the_round_trip() {
        for ts in [
            "1970-01-01 00:00:00",
            "1999-12-31 23:59:59",
            "2000-02-29 12:00:00",
            "2026-10-17 06:30:00",
            "2100-03-01 00:00:01",
        ] {
            assert_eq!(timestamp_of_ms(julian_day_ms(ts).unwrap()), ts);
        }
    }

    #[test]
    fn a_fixed_clock_reads_back_as_itself() {
        let c = Clock::at("2026-10-17 06:30:00").unwrap();
        assert_eq!(c.now_timestamp(), "2026-10-17 06:30:00");
        assert_eq!(c.hours_since("nonsense"), None);
        // Not `== 1.0`, and the sign of the error is not a constant
        // either — which is why this is pinned to the literal double
        // rather than to a tolerance. Through CPython's sqlite3, whose
        // `repr` round-trips exactly:
        //
        //   (julianday('2026-10-17 06:30:00')
        //  - julianday('2026-10-17 05:30:00')) * 24  ->  1.0000000074505806
        //   (julianday('2026-10-17 12:00:00')
        //  - julianday('2026-10-17 11:00:00')) * 24  ->  0.9999999962747097
        //
        // One is above an hour and the other below it, for the same
        // gap — so a tolerance assertion would pass on an
        // implementation that rounds differently and renders a
        // different string at the boundary.
        //
        // Read through the `sqlite3` CLI instead, the first of those
        // prints as `1.000000007450581` under `printf('%.17g')`: its
        // float formatter is not exact at the last digit. The oracle
        // goes through Python for that reason.
        assert_eq!(
            c.hours_since("2026-10-17 05:30:00"),
            Some(1.0000000074505806)
        );
    }

    /// Every branch of `_age_hint`, at the boundaries, with the values
    /// SQLite actually produces.
    ///
    /// The two surprises are both float truncation and both are on
    /// stdout today: **exactly one minute ago renders `0min`**, and
    /// **exactly one hour ago renders `59min` and is classified
    /// fresh** — because `(julianday(a) - julianday(b)) * 24` for a
    /// one-hour gap is `0.99999999…`, which fails `hours < 1` on the
    /// low side. Verified against `sqlite3` directly, and against
    /// CPython in `tests/clock_parity.rs`.
    #[test]
    fn age_hint_reproduces_pythons_branches() {
        let c = Clock::at("2026-10-17 12:00:00").unwrap();
        let hint = |ts: &str| c.age_hint(Some(ts), 1.0);

        assert_eq!(hint("2026-10-17 11:59:00").text, "0min ago");
        assert_eq!(hint("2026-10-17 11:59:00").freshness, Freshness::Ok);
        assert_eq!(hint("2026-10-17 11:00:30").text, "59min ago");
        assert_eq!(hint("2026-10-17 11:00:00").text, "59min ago");
        assert_eq!(hint("2026-10-17 11:00:00").freshness, Freshness::Ok);
        // A hair over the hour does reach the second branch, and at
        // threshold 1 that is `stale` rather than `ok`.
        assert_eq!(hint("2026-10-17 10:59:59").text, "1h ago");
        assert_eq!(hint("2026-10-17 10:59:59").freshness, Freshness::Stale);
        assert_eq!(hint("2026-10-16 12:59:59").text, "23h ago");
        assert_eq!(hint("2026-10-16 12:59:59").freshness, Freshness::Stale);
        assert_eq!(hint("2026-10-16 11:59:59").text, "1d ago");
        assert_eq!(hint("2026-10-16 11:59:59").freshness, Freshness::Ancient);

        // Threshold 168 (the reconcile pane). `ok` for a week, and the
        // `ancient` cutoff is 168 *days* rather than 168 hours,
        // because the branch multiplies the threshold by 24. So a
        // machine unreconciled for three months still renders in
        // hours — 2,184 of them — and only passes 4,032 hours in April
        // of the following year.
        let week = c.age_hint(Some("2026-10-11 12:00:01"), 168.0);
        assert_eq!(
            (week.text.as_str(), week.freshness),
            ("143h ago", Freshness::Ok)
        );
        let months = c.age_hint(Some("2026-07-18 12:00:00"), 168.0);
        assert_eq!(
            (months.text.as_str(), months.freshness),
            ("2184h ago", Freshness::Stale)
        );
        let year = c.age_hint(Some("2026-04-01 12:00:00"), 168.0);
        assert_eq!(
            (year.text.as_str(), year.freshness),
            ("199d ago", Freshness::Ancient)
        );
    }

    #[test]
    fn a_missing_timestamp_is_never_and_a_bad_one_is_unknown() {
        let c = Clock::at("2026-10-17 12:00:00").unwrap();
        assert_eq!(c.age_hint(None, 1.0).freshness, Freshness::Never);
        assert_eq!(c.age_hint(None, 1.0).text, "never");
        assert_eq!(c.age_hint(Some(""), 1.0).freshness, Freshness::Never);
        assert_eq!(c.age_hint(Some("nope"), 1.0).text, "unknown");
        assert_eq!(c.age_hint(Some("nope"), 1.0).freshness, Freshness::Stale);
    }

    /// The live database holds timestamps ahead of a local reading,
    /// because ingest stamps UTC. Python's `int()` truncates toward
    /// zero and so must this: `-4min`, never `-5min`.
    #[test]
    fn a_future_timestamp_truncates_toward_zero() {
        let c = Clock::at("2026-10-17 12:00:00").unwrap();
        assert_eq!(
            c.age_hint(Some("2026-10-17 12:04:30"), 1.0).text,
            "-4min ago"
        );
        assert_eq!(
            c.age_hint(Some("2026-10-17 12:04:30"), 1.0).freshness,
            Freshness::Ok
        );
    }

    #[test]
    fn the_env_override_wins_and_an_empty_one_does_not() {
        // Serialised by construction: one test owns the variable.
        std::env::set_var(NOW_ENV, "2026-01-02 03:04:05");
        assert_eq!(
            Clock::from_env().unwrap().now_timestamp(),
            "2026-01-02 03:04:05"
        );
        std::env::set_var(NOW_ENV, "  ");
        assert!(Clock::from_env().is_ok());
        std::env::set_var(NOW_ENV, "halfway through tuesday");
        let err = Clock::from_env().unwrap_err().to_string();
        assert!(err.contains("not a timestamp"), "{err}");
        std::env::remove_var(NOW_ENV);
    }

    /// The system clock is not asserted against a value — it is
    /// asserted to be a clock: it parses back, and it moves forward.
    #[test]
    fn the_system_clock_produces_a_parseable_timestamp() {
        let ts = Clock::system().now_timestamp();
        assert_eq!(ts.len(), 19, "{ts}");
        let round_tripped = Clock::at(&ts).unwrap();
        assert!(round_tripped.hours_since(&ts).unwrap().abs() < 1e-9);
    }
}
