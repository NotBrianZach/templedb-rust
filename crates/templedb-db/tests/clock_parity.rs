//! Differential test of [`templedb_db::Clock`] against the thing it
//! reproduces: CPython running SQLite's `julianday`, then Python's own
//! `_age_hint` arithmetic on the result.
//!
//! # Why an oracle and not a table
//!
//! The unit tests in `clock.rs` pin the boundaries someone thought to
//! write down. What they cannot pin is the part that makes this module
//! hard — a Julian Day is 2.46 million, an `f64` resolves it to about
//! 47 microseconds, and `_age_hint` **truncates**. The two meet at
//! every boundary, and which side of it a given pair of timestamps
//! lands on is not predictable by reading the code:
//!
//! ```text
//! (julianday('2026-10-17 06:30:00') - julianday('2026-10-17 05:30:00')) * 24
//!   -> 1.0000000074505806      # above an hour
//! (julianday('2026-10-17 12:00:00') - julianday('2026-10-17 11:00:00')) * 24
//!   -> 0.9999999962747097      # below it, same gap
//! ```
//!
//! So an implementation that is *correct* to a microsecond still
//! prints a different string from Python's for some inputs. The only
//! way to know this one does not is to ask, over enough pairs that the
//! boundaries get hit, which is what this does: 2,000 of them, half
//! landing on an exact minute, hour or day boundary on purpose.
//!
//! # Why it is `#[ignore]`d
//!
//! It shells out to `python3`, and `cargo test` must pass on a machine
//! without an interpreter. Same treatment as `difflib_parity.rs`:
//!
//! ```bash
//! cargo test -p templedb-db --test clock_parity -- --ignored
//! ```
//!
//! `just check` and `nix flake check` both run the ignored set.
//!
//! One thing deliberately *not* delegated: the oracle reads
//! `julianday` out of the Python process's own `sqlite3` module, which
//! is the system SQLite (3.53.3 here) rather than the one this crate
//! bundles (3.50.2). That is the pairing that matters — the question
//! is whether the port agrees with *Python*, and Python is linked
//! against the system library. A disagreement traceable to the version
//! gap would be a real finding rather than a flaw in the harness, and
//! it is the only place in the tree where the two versions are
//! compared at all.

use std::io::Write;
use std::process::{Command, Stdio};

use templedb_db::Clock;

/// The oracle. `_age_hint`'s arithmetic, transcribed from
/// `src/cli/commands/summary.py`, over `julianday` from the same
/// `sqlite3` the Python implementation uses.
///
/// Returns one line per case: the hours as a `repr` (exact, unlike
/// `%.17g` through the `sqlite3` CLI — which misprints the last digit
/// and would make this harness lie), then the rendered age and its
/// freshness class.
const ORACLE: &str = r#"
import sys, sqlite3
db = sqlite3.connect(":memory:")

def age_hint(now, ts, threshold_hours):
    # summary.py:450, with julianday('now') replaced by an injected
    # instant so the comparison is deterministic. Everything else is
    # character for character.
    if not ts:
        return ('never', 'never')
    row = db.execute(
        "SELECT (julianday(?) - julianday(?)) * 24 AS hours", (now, ts)
    ).fetchone()
    hours = row[0] if row else None
    if hours is None:
        return ('unknown', 'stale')
    if hours < 1:
        return (f"{int(hours * 60)}min ago", 'ok')
    if hours < threshold_hours:
        return (f"{int(hours)}h ago", 'ok')
    if hours < threshold_hours * 24:
        return (f"{int(hours)}h ago", 'stale')
    return (f"{int(hours / 24)}d ago", 'ancient')

for line in sys.stdin:
    now, ts, threshold = line.rstrip("\n").split("\t")
    row = db.execute(
        "SELECT (julianday(?) - julianday(?)) * 24", (now, ts)
    ).fetchone()
    hours = row[0]
    text, freshness = age_hint(now, ts, float(threshold))
    print(f"{hours!r}\t{text}\t{freshness}")
"#;

/// Deterministic PRNG, same reasoning as `difflib_parity.rs`: a fixed
/// seed makes a failure reproducible, and `rand` would be a dependency
/// for sixteen bits of arithmetic.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

/// Seconds since 1970 as `YYYY-MM-DD HH:MM:SS`, built with the module's
/// own conversion. Using it here rather than a second implementation
/// is deliberate: if it were wrong, the round-trip unit test would
/// fail first, and a second copy would only ever agree with itself.
fn stamp(unix_seconds: i64) -> String {
    templedb_db::timestamp_of_ms(210_866_760_000_000 + unix_seconds * 1000)
}

/// Pairs of `(now, then, threshold)`, half of them landing exactly on
/// a minute, hour or day boundary.
///
/// The exact ones are the point. Random timestamps are microseconds
/// away from a truncation boundary essentially never, so a harness
/// built only from them would pass on an implementation that computes
/// the age a slightly different way — which is the one failure mode
/// this module has.
fn cases() -> Vec<(String, String, f64)> {
    let mut rng = Lcg(0x5EED_C10C);
    // 2026-10-17 12:00:00 UTC, within the range the live data covers.
    const BASE: i64 = 1_792_756_800;
    let thresholds = [1.0, 24.0, 168.0];
    let mut out = Vec::new();

    for i in 0..2000u64 {
        let now = BASE + rng.below(400 * 86_400) as i64 - 200 * 86_400;
        let gap = match i % 4 {
            // Exactly n minutes, hours, days: the boundaries.
            0 => 60 * (1 + rng.below(240)) as i64,
            1 => 3_600 * (1 + rng.below(100)) as i64,
            2 => 86_400 * (1 + rng.below(400)) as i64,
            // And an arbitrary offset, including the occasional
            // future timestamp — the live database has those, because
            // ingest stamps UTC.
            _ => rng.below(400 * 86_400) as i64 - 10 * 86_400,
        };
        let threshold = thresholds[(rng.below(3)) as usize];
        out.push((stamp(now), stamp(now - gap), threshold));
    }
    out
}

fn freshness_name(f: templedb_db::Freshness) -> &'static str {
    match f {
        templedb_db::Freshness::Ok => "ok",
        templedb_db::Freshness::Stale => "stale",
        templedb_db::Freshness::Ancient => "ancient",
        templedb_db::Freshness::Never => "never",
    }
}

#[test]
#[ignore = "needs python3 as the reference oracle"]
fn age_strings_match_cpython_driving_sqlite() {
    let cases = cases();
    let mut input = String::new();
    for (now, then, threshold) in &cases {
        input.push_str(&format!("{now}\t{then}\t{threshold}\n"));
    }

    let mut child = Command::new("python3")
        .arg("-c")
        .arg(ORACLE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 must be on PATH for this test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = String::from_utf8(output.stdout).unwrap();
    let expected: Vec<&str> = expected.lines().collect();
    assert_eq!(
        expected.len(),
        cases.len(),
        "oracle returned the wrong count"
    );

    let mut exact_hits = 0usize;
    for ((now, then, threshold), want) in cases.iter().zip(expected) {
        let (want_hours, want_text, want_fresh) = {
            let mut it = want.split('\t');
            (it.next().unwrap(), it.next().unwrap(), it.next().unwrap())
        };
        let clock = Clock::at(now).unwrap();
        let hours = clock.hours_since(then).unwrap();
        let age = clock.age_hint(Some(then), *threshold);

        // The float first, because if the arithmetic diverges the age
        // string will too and the hours say *why*. Compared through
        // Python's repr, which round-trips, so this is a bit-for-bit
        // comparison of the two doubles.
        let want_hours: f64 = want_hours.parse().unwrap();
        assert_eq!(
            hours.to_bits(),
            want_hours.to_bits(),
            "hours differ for {now} - {then}: {hours:.17} vs {want_hours:.17}"
        );
        assert_eq!(age.text, want_text, "age differs for {now} - {then}");
        assert_eq!(
            freshness_name(age.freshness),
            want_fresh,
            "freshness differs for {now} - {then}"
        );
        if hours.fract() == 0.0 || (hours * 60.0).fract() == 0.0 {
            exact_hits += 1;
        }
    }

    // A harness that never lands on a boundary would pass against a
    // wrong implementation, so assert it landed on some. This is the
    // mutation check built in: it fails if the generator is ever
    // changed in a way that stops producing exact gaps.
    assert!(
        exact_hits > 100,
        "only {exact_hits} cases landed on an exact boundary — the \
         generator is no longer testing what this file is for"
    );
}

/// The other direction: every timestamp in the five tables `summary`
/// reads must parse to the same instant SQLite gives it.
///
/// Formats rather than values, so it is cheap and needs no database:
/// the point is that the parser accepts what SQLite accepts and
/// rejects what it rejects, including the zone suffixes no column here
/// carries yet.
#[test]
#[ignore = "needs python3 as the reference oracle"]
fn julianday_agrees_on_every_format_sqlite_accepts() {
    const FORMATS: &[&str] = &[
        "2026-10-17 06:30:00",
        "2026-10-17T06:30:00",
        "2026-10-17 06:30:00.500",
        "2026-10-17 06:30:00.123",
        "2026-10-17 06:30:00Z",
        "2026-10-17 06:30:00+02:00",
        "2026-10-17 06:30:00-05:30",
        "1970-01-01 00:00:00",
        "2000-02-29 12:00:00",
        "1900-03-01 00:00:00",
        "2100-12-31 23:59:59",
        // Rejected by both, which is as much of the contract as the
        // accepted ones: these come back NULL and render `unknown`.
        "",
        "not a date",
        "2026-13-01 00:00:00",
        "20x6-10-17",
    ];

    let input = FORMATS.join("\n");
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(
            r#"
import sys, sqlite3
db = sqlite3.connect(":memory:")
for line in sys.stdin.read().split("\n"):
    v = db.execute("SELECT julianday(?)", (line,)).fetchone()[0]
    print("NULL" if v is None else repr(v))
"#,
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 must be on PATH for this test");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let expected = String::from_utf8(output.stdout).unwrap();

    for (format, want) in FORMATS.iter().zip(expected.lines()) {
        match templedb_db::julian_day(format) {
            None => assert_eq!(want, "NULL", "{format:?} should not parse"),
            Some(jd) => {
                let want: f64 = want
                    .parse()
                    .unwrap_or_else(|_| panic!("{format:?}: SQLite said {want}, port said {jd}"));
                assert_eq!(
                    jd.to_bits(),
                    want.to_bits(),
                    "{format:?}: {jd:.17} vs {want:.17}"
                );
            }
        }
    }
}
