//! `summary`'s interpretation layer: ages, freshness, colour, columns.
//!
//! The second of the three layers the TempleDB report *`summary` is not
//! a function of the database* (2026-10-18) splits the command into.
//! `templedb-db/src/summary.rs` holds the first — facts with absolute
//! timestamps and no clock — and this holds everything that turns them
//! into bytes.
//!
//! # Why it is a function returning lines
//!
//! [`render`] takes the facts, an instant, a session identity and a
//! colour flag, and returns the lines rather than printing them. That
//! is the only reason this file can be tested at all: `summary` is the
//! first command in the port that the byte-diff sweep cannot verify,
//! because Python's reads five things that are not rows. Pinning the
//! rendering at a fixed instant against hand-built facts is what
//! replaces the sweep here, and a function that printed could not be
//! pinned without capturing stdout.
//!
//! # Two panes are missing, and they say so
//!
//! Python prints nine panes; this prints seven, and emits a `(not
//! ported)` line in place of each of the other two, in the position
//! Python puts them:
//!
//! - **Agent fleet** has no observation layer. Python's reads
//!   `agent.store.fleet_rollup()`, whose tables are outside the
//!   seventeen the mirror covers, so there is nothing to observe yet.
//! - **Systemd units** reads `systemctl`, not the database. It belongs
//!   to the third layer — environment — which is not built.
//!
//! The alternative was to omit them silently, and that is worse than a
//! wrong number: a missing systemd pane reads as "no failed units" and
//! a missing fleet pane reads as "nothing waiting on you". Python's
//! fleet pane does vanish when the agent tables are absent, so silence
//! there would even have been faithful — which is exactly why it is not
//! safe.
//!
//! # The escape codes are inside the column width, deliberately
//!
//! Python pads several coloured strings — `f"{_c(ts, colour):<40}"` —
//! which pads a string whose nine characters of ANSI escape count
//! toward the forty. The columns do not line up, and they do not line
//! up on screen either. It is on stdout, so it is reproduced. The
//! consequence worth knowing is that `--no-color` output is *not* the
//! coloured output with the escapes stripped: see
//! `no_color_is_not_the_painted_output_with_escapes_removed`.

use templedb_db::{
    Clock, Freshness, InvariantResult, MachineReconcile, Reconciliation, SummaryFacts,
};

use crate::{py_str_opt, thousands};

/// How recent an invariant result has to be before the doctor pane will
/// present it without a date. Python's `DOCTOR_FRESH_HOURS`, matched to
/// the hourly ingest cadence rather than the flat 24h it replaced.
const DOCTOR_FRESH_HOURS: f64 = 1.0;

/// A week, in hours, which is the reconcile pane's staleness threshold.
///
/// Note that `Clock::age_hint` reads the `ancient` cutoff as
/// `threshold * 24` — so 168 hours here means a machine renders its age
/// in hours until it is 168 *days* stale. That is Python's arithmetic
/// and the note on it is in `clock.rs`.
const FLEET_THRESHOLD_HOURS: f64 = 168.0;

/// Python's `RECONCILE_STALE_DAYS`: how long an open reconciliation has
/// to sit untouched before the pane calls it out separately.
const RECONCILE_STALE_DAYS: i64 = 7;

/// Python's `max_rows` for the reconciliations pane.
const RECONCILE_MAX_ROWS: usize = 4;

/// The five colours Python's `_c` knows, and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Colour {
    Green,
    Yellow,
    Red,
    Muted,
    Accent,
}

impl Colour {
    fn code(self) -> u8 {
        match self {
            Colour::Green => 32,
            Colour::Yellow => 33,
            Colour::Red => 31,
            Colour::Muted => 90,
            Colour::Accent => 35,
        }
    }
}

/// Python's `_c`, with the on/off switch Python does not have.
///
/// Python always colours — there is no flag — so the painted form is
/// the one that corresponds to its output and is therefore the default.
/// `--no-color` exists because every other command in this port has it
/// and because a terminal is not the only thing that reads stdout.
#[derive(Debug, Clone, Copy)]
struct Paint {
    color: bool,
}

impl Paint {
    /// Wrap `text` in the colour, or return it untouched.
    ///
    /// An empty `text` still produces `\x1b[90m\x1b[0m` when painting,
    /// which looks like a bug and is not one: Python's doctor pane
    /// writes `_c(age, 'muted')` with `age` set to `''` for a fresh
    /// check, so those nine bytes really are on its stdout.
    fn c(self, text: &str, colour: Colour) -> String {
        if self.color {
            format!("\x1b[{}m{text}\x1b[0m", colour.code())
        } else {
            text.to_string()
        }
    }
}

/// Python's freshness-to-colour mapping, which folds `ancient` and
/// `never` together: `green if ok else yellow if stale else red`.
fn freshness_colour(freshness: Freshness) -> Colour {
    match freshness {
        Freshness::Ok => Colour::Green,
        Freshness::Stale => Colour::Yellow,
        Freshness::Ancient | Freshness::Never => Colour::Red,
    }
}

/// Python's truthiness for a nullable text column: `NULL` and `''` are
/// both falsy, and `x or 'fallback'` takes the fallback for either.
fn or_else<'a>(value: Option<&'a str>, fallback: &'a str) -> &'a str {
    value.filter(|s| !s.is_empty()).unwrap_or(fallback)
}

/// Render every pane, at the instant given.
pub fn render(facts: &SummaryFacts, clock: &Clock, addressee: &str, color: bool) -> Vec<String> {
    let p = Paint { color };
    let mut out = Vec::new();

    out.push(String::new());
    out.push(p.c("═════ TempleDB Summary ═════", Colour::Accent));
    out.push(String::new());

    entity_graph(&mut out, facts, p);
    agent_fleet(&mut out, p);
    ingestion(&mut out, facts, clock, p);
    systemd(&mut out, p);
    doctor(&mut out, facts, clock, p);
    reconciliations(&mut out, facts, clock, p);
    fleet(&mut out, facts, clock, p);
    hygiene(&mut out, facts, p);
    handoffs(&mut out, facts, addressee, p);

    out.push(String::new());
    out
}

fn entity_graph(out: &mut Vec<String>, facts: &SummaryFacts, p: Paint) {
    let top = facts
        .graph
        .top_kinds
        .iter()
        .map(|(kind, n)| format!("{kind}={}", thousands(*n)))
        .collect::<Vec<_>>()
        .join(", ");
    out.push(format!(
        "  {:<18} {} entities, {} relations",
        "Entity graph",
        p.c(&thousands(facts.graph.entities), Colour::Green),
        p.c(&thousands(facts.graph.relations), Colour::Green),
    ));
    // `top: , ...` when the graph is empty, which is what Python's
    // unconditional join produces. Not worth a branch the original
    // does not have.
    out.push(format!(
        "  {:<18} {}",
        "",
        p.c(&format!("top: {top}, ..."), Colour::Muted),
    ));
}

/// The pane with no observation layer. See the module note.
fn agent_fleet(out: &mut Vec<String>, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Agent fleet (per project) ──", Colour::Accent));
    out.push(format!(
        "  {}",
        p.c(
            "(not ported — the agent tables are outside the mirrored subset)",
            Colour::Muted
        ),
    ));
}

fn ingestion(out: &mut Vec<String>, facts: &SummaryFacts, clock: &Clock, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Ingestion (per adapter) ──", Colour::Accent));
    if facts.adapters.is_empty() {
        out.push(format!(
            "  {}",
            p.c(
                "(no ingests recorded — run `templedb ingest all`)",
                Colour::Muted
            ),
        ));
        return;
    }
    for a in &facts.adapters {
        let age = clock.age_hint(a.last_run.as_deref(), DOCTOR_FRESH_HOURS);
        // The colour is the *timestamp's*, and the timestamp is padded
        // after being painted — so this column is nine characters
        // narrower than it looks. Python's, reproduced.
        out.push(format!(
            "  {:<10} last {:<14} {:<40} {} ok, {} err",
            a.adapter,
            age.text,
            p.c(
                or_else(a.last_run.as_deref(), "(never)"),
                freshness_colour(age.freshness)
            ),
            a.ok_count,
            a.err_count,
        ));
    }
}

/// The pane that reads the machine rather than the database. See the
/// module note.
fn systemd(out: &mut Vec<String>, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Systemd units ──", Colour::Accent));
    out.push(format!(
        "  {}",
        p.c(
            "(not ported — reads systemd, not the database)",
            Colour::Muted
        ),
    ));
}

fn doctor(out: &mut Vec<String>, facts: &SummaryFacts, clock: &Clock, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Doctor invariants (latest results) ──", Colour::Accent));
    let checks = &facts.invariants;
    if checks.is_empty() {
        out.push(format!(
            "  {}",
            p.c(
                "(no doctor runs — try `templedb doctor entities`)",
                Colour::Muted
            ),
        ));
        return;
    }

    // Errored and violated are counted apart because a check that
    // errored did not run, so its `issue_count` is left over from the
    // last run that did. Python records the bug this prevented: a check
    // dead for days kept reporting `389 issue(s)`.
    let errored: Vec<&InvariantResult> = checks.iter().filter(|c| c.could_not_run()).collect();
    let violated: Vec<&InvariantResult> = checks.iter().filter(|c| c.violated()).collect();
    let summary_colour = if violated.is_empty() && errored.is_empty() {
        Colour::Green
    } else {
        Colour::Red
    };
    let mut counts = format!("{} violated", violated.len());
    if !errored.is_empty() {
        counts.push_str(&format!(", {} could not run", errored.len()));
    }

    // The *oldest* of the per-check latest results, not the newest:
    // `doctor entities --check X` persists only X, so one check run a
    // minute ago must not let the header claim the pane is current.
    // Lexicographic `min` over `YYYY-MM-DD HH:MM:SS` is chronological,
    // which is what Python's `min()` over the same strings relies on.
    let oldest = checks.iter().filter_map(|c| c.ran_at.as_deref()).min();
    let oldest_age = clock.age_hint(oldest, DOCTOR_FRESH_HOURS);
    let stale_note = if oldest_age.freshness == Freshness::Ok {
        String::new()
    } else {
        p.c(
            &format!(
                "  — stalest {}, re-run `templedb doctor entities`",
                oldest_age.text
            ),
            Colour::Yellow,
        )
    };
    out.push(format!(
        "  {}{}{}",
        p.c(
            &format!("{} invariants tracked; ", checks.len()),
            Colour::Muted
        ),
        p.c(&counts, summary_colour),
        stale_note,
    ));

    // Problems first. Slicing `checks[:3]` by recency — which an
    // earlier revision of Python did — hides a violated check behind
    // three that merely ran later.
    let problems: Vec<&InvariantResult> = errored.iter().chain(violated.iter()).copied().collect();
    let spotlight: Vec<&InvariantResult> = if problems.is_empty() {
        checks.iter().take(3).collect()
    } else {
        problems.iter().take(3).copied().collect()
    };
    for c in spotlight {
        let (marker, marker_colour, detail) = if c.status == "ok" {
            ("✓", Colour::Green, "OK".to_string())
        } else if c.could_not_run() {
            (
                "!",
                Colour::Yellow,
                "could not run (see `templedb doctor entities`)".to_string(),
            )
        } else {
            ("✗", Colour::Red, format!("{} issue(s)", c.issue_count))
        };
        // Per-check age as well as the pane's, because `--check X`
        // persists only X and one fresh check can sit beside twenty a
        // day old. Fresh checks print the empty string — through `_c`,
        // escapes and all.
        let age = clock.age_hint(c.ran_at.as_deref(), DOCTOR_FRESH_HOURS);
        let age = if age.freshness == Freshness::Ok {
            String::new()
        } else {
            format!("  ({})", age.text)
        };
        out.push(format!(
            "    {} {:<45} {}{}",
            p.c(marker, marker_colour),
            c.check_name,
            detail,
            p.c(&age, Colour::Muted),
        ));
    }
    let problem_count = errored.len() + violated.len();
    if problem_count > 3 {
        out.push(format!(
            "    {}",
            p.c(
                &format!(
                    "... and {} more — `templedb doctor entities`",
                    problem_count - 3
                ),
                Colour::Muted
            ),
        ));
    }
}

fn reconciliations(out: &mut Vec<String>, facts: &SummaryFacts, clock: &Clock, p: Paint) {
    let counts = &facts.reconciliation_counts;
    let rows = &facts.reconciliations;
    // Python returns before printing a header when there is nothing
    // live *and* no rows, so the pane is absent rather than empty. The
    // whole pane is optional, blank line included.
    if rows.is_empty() && !counts.any_live() {
        return;
    }
    out.push(String::new());
    out.push(p.c(
        "── Reconciliations (discrepancies being worked on) ──",
        Colour::Accent,
    ));
    let head = format!(
        "{} open, {} in progress, {} accepted",
        counts.open, counts.in_progress, counts.accepted
    );
    let colour = if counts.open != 0 {
        Colour::Red
    } else if counts.in_progress != 0 {
        Colour::Yellow
    } else {
        Colour::Green
    };
    let extra = if counts.review_due == 0 {
        String::new()
    } else {
        p.c(
            &format!(
                "  — {} acceptance(s) past their review date",
                counts.review_due
            ),
            Colour::Yellow,
        )
    };
    out.push(format!("  {}{}", p.c(&head, colour), extra));

    // The idle column is the point of this pane: a violated invariant
    // that has been red for nine days is invisible in the pane above,
    // which reports only the latest result. Python computes this in SQL
    // as `CAST(julianday('now') - julianday(last_observed_at) AS INT)`;
    // here the column arrives absolute and the clock does the
    // subtraction, which is what keeps the observation layer a
    // function.
    let idle = |r: &Reconciliation| {
        r.last_observed_at
            .as_deref()
            .and_then(|t| clock.days_since(t))
    };
    let stale: Vec<&Reconciliation> = rows
        .iter()
        .filter(|r| {
            (r.status == "open" || r.status == "in_progress")
                && idle(r).unwrap_or(0) >= RECONCILE_STALE_DAYS
        })
        .collect();
    if !stale.is_empty() {
        out.push(format!(
            "  {}",
            p.c(
                &format!("{} untouched for {RECONCILE_STALE_DAYS}+ days", stale.len()),
                Colour::Red
            ),
        ));
    }
    // `stale or rows` — the stale set *replaces* the listing rather
    // than preceding it, so a pane with anything stale shows only stale
    // rows.
    let shown: Vec<&Reconciliation> = if stale.is_empty() {
        rows.iter().take(RECONCILE_MAX_ROWS).collect()
    } else {
        stale.iter().take(RECONCILE_MAX_ROWS).copied().collect()
    };
    for r in shown {
        let (marker, colour) = if r.status == "open" {
            ("!", Colour::Red)
        } else {
            ("~", Colour::Yellow)
        };
        let owner = match r.owner.as_deref().filter(|o| !o.is_empty()) {
            Some(o) => format!("  {o}"),
            None => String::new(),
        };
        // `subject_ref[:40]` slices characters, not bytes — the refs are
        // file paths and entity addresses, which are not all ASCII.
        let subject: String = r.subject_ref.chars().take(40).collect();
        out.push(format!(
            "    {} {}/{:<40} {:>4} item(s), idle {}d{}",
            p.c(marker, colour),
            r.kind,
            subject,
            r.item_count,
            // A NULL `last_observed_at` renders `idle Noned`, because
            // Python interpolates the NULL the SQL CAST returned.
            py_str_opt(idle(r)),
            p.c(&owner, Colour::Muted),
        ));
    }
    // Against `rows`, not against `shown`: Python compares the full
    // listing to `max_rows`, so a pane showing two stale rows out of
    // fifty still says "... 46 more".
    if rows.len() > RECONCILE_MAX_ROWS {
        out.push(format!(
            "    {}",
            p.c(
                &format!(
                    "... {} more — `templedb reconcile list`",
                    rows.len() - RECONCILE_MAX_ROWS
                ),
                Colour::Muted
            ),
        ));
    }
}

fn fleet(out: &mut Vec<String>, facts: &SummaryFacts, clock: &Clock, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Reconcile (per fleet machine) ──", Colour::Accent));
    if facts.machines.is_empty() {
        out.push(format!(
            "  {}",
            p.c("(no fleet_machines registered)", Colour::Muted),
        ));
        return;
    }
    // Deployed and never-deployed are split because only a machine
    // TempleDB deployed has a reconcile baseline to be measured
    // against. The second group collapses to one muted line.
    let undeployed: Vec<&MachineReconcile> =
        facts.machines.iter().filter(|m| !m.deployed()).collect();
    for m in facts.machines.iter().filter(|m| m.deployed()) {
        let Some(last_run) = m.last_run.as_deref().filter(|s| !s.is_empty()) else {
            out.push(format!(
                "  {} {:<20} {}",
                p.c("?", Colour::Yellow),
                m.machine_name,
                p.c("never reconciled", Colour::Yellow),
            ));
            continue;
        };
        let age = clock.age_hint(Some(last_run), FLEET_THRESHOLD_HOURS);
        let status = m.last_status.as_deref();
        let (marker, colour) = match status {
            Some("ok") => ("✓", Colour::Green),
            Some("drift") => ("✗", Colour::Red),
            _ => ("?", Colour::Yellow),
        };
        out.push(format!(
            "  {} {:<20} last {:<14} {}",
            p.c(marker, colour),
            m.machine_name,
            age.text,
            p.c(or_else(status, "?"), colour),
        ));
    }
    if !undeployed.is_empty() {
        let names = undeployed
            .iter()
            .map(|m| m.machine_name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(format!(
            "  {}",
            p.c(
                &format!(
                    "({} never deployed via templedb, skipped: {names})",
                    undeployed.len()
                ),
                Colour::Muted
            ),
        ));
    }
}

fn hygiene(out: &mut Vec<String>, facts: &SummaryFacts, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Python hygiene ──", Colour::Accent));
    if facts.hygiene.is_empty() {
        out.push(format!(
            "  {}",
            p.c(
                "(no python imports observed — run `templedb ingest python`)",
                Colour::Muted
            ),
        ));
        return;
    }
    for row in &facts.hygiene {
        let pct = if row.total_imports == 0 {
            0.0
        } else {
            100.0 * row.dead_candidates as f64 / row.total_imports as f64
        };
        let colour = if pct < 10.0 {
            Colour::Green
        } else if pct < 30.0 {
            Colour::Yellow
        } else {
            Colour::Red
        };
        out.push(format!(
            "  {:<28} {}/{:<4} candidate dead ({})",
            row.slug,
            p.c(&format!("{:>3}", row.dead_candidates), colour),
            row.total_imports,
            p.c(&format!("{pct:.0}%"), colour),
        ));
    }
    out.push(format!(
        "  {}",
        p.c(
            "detail: templedb entity dead-imports --slug <slug>",
            Colour::Muted
        ),
    ));
}

/// `addressee` is the handoff *address* — the string the notes were
/// sent to — and `inbox` below is the counts. Two names because the
/// parameter was `session_id` and the local was `inbox`, which made a
/// `{inbox}` in the format string below silently mean the wrong one;
/// the compiler caught it, which it would not have done had both been
/// strings.
fn handoffs(out: &mut Vec<String>, facts: &SummaryFacts, addressee: &str, p: Paint) {
    out.push(String::new());
    out.push(p.c("── Handoff inbox ──", Colour::Accent));
    let inbox = &facts.handoffs;
    if inbox.is_empty() {
        out.push(format!(
            "  {}",
            p.c("(no unacked handoffs for this session)", Colour::Muted),
        ));
        return;
    }
    if inbox.direct != 0 {
        out.push(format!(
            "  {} unacked note(s) for session {addressee}",
            p.c(&inbox.direct.to_string(), Colour::Yellow),
        ));
    }
    if inbox.broadcast != 0 {
        out.push(format!(
            "  {} unacked broadcast(s)",
            p.c(&inbox.broadcast.to_string(), Colour::Yellow),
        ));
    }
    out.push(format!(
        "  {}",
        p.c(
            &format!("view: templedb handoff list --for {addressee}"),
            Colour::Muted
        ),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use templedb_db::{AdapterRuns, GraphFacts, HandoffInbox, HygieneRow, ReconciliationCounts};

    /// The instant every test below renders at. Chosen so the fixture's
    /// timestamps land on both sides of each threshold.
    const NOW: &str = "2026-10-17 12:00:00";

    fn clock() -> Clock {
        Clock::at(NOW).unwrap()
    }

    fn invariant(name: &str, status: &str, issues: i64, ran_at: &str) -> InvariantResult {
        InvariantResult {
            check_name: name.into(),
            status: status.into(),
            ran_at: Some(ran_at.into()),
            issue_count: issues,
        }
    }

    fn reconciliation(
        kind: &str,
        subject: &str,
        status: &str,
        items: i64,
        observed: Option<&str>,
    ) -> Reconciliation {
        Reconciliation {
            kind: kind.into(),
            subject_ref: subject.into(),
            status: status.into(),
            item_count: items,
            owner: None,
            last_observed_at: observed.map(str::to_string),
            first_detected_at: Some("2026-09-01 00:00:00".into()),
        }
    }

    /// Everything empty: no adapters, no checks, no machines, no rows.
    fn empty() -> SummaryFacts {
        SummaryFacts {
            graph: GraphFacts {
                entities: 0,
                relations: 0,
                top_kinds: vec![],
            },
            adapters: vec![],
            invariants: vec![],
            reconciliation_counts: ReconciliationCounts::default(),
            reconciliations: vec![],
            machines: vec![],
            hygiene: vec![],
            handoffs: HandoffInbox {
                direct: 0,
                broadcast: 0,
            },
        }
    }

    /// One row per rendering branch worth pinning.
    fn populated() -> SummaryFacts {
        SummaryFacts {
            graph: GraphFacts {
                entities: 61_234,
                relations: 128_900,
                top_kinds: vec![
                    ("Symbol".into(), 21_770),
                    ("File".into(), 13_817),
                    ("Commit".into(), 2_709),
                    ("Machine".into(), 6),
                ],
            },
            adapters: vec![
                // 30 minutes old: fresh, so green and rendered in
                // minutes.
                AdapterRuns {
                    adapter: "git".into(),
                    last_run: Some("2026-10-17 11:30:00".into()),
                    ok_count: 12,
                    err_count: 1,
                },
                // Nine days old, past `1 * 24` hours: ancient, so red
                // and rendered in days.
                AdapterRuns {
                    adapter: "python".into(),
                    last_run: Some("2026-10-08 12:00:00".into()),
                    ok_count: 3,
                    err_count: 0,
                },
                // Never run at all.
                AdapterRuns {
                    adapter: "nix".into(),
                    last_run: None,
                    ok_count: 0,
                    err_count: 0,
                },
            ],
            invariants: vec![
                // Fresh and passing, which is the branch that prints an
                // empty age through `_c`.
                invariant("no_orphans", "ok", 0, "2026-10-17 11:59:00"),
                invariant("no_cycles", "violated", 3, "2026-10-16 09:00:00"),
                invariant("needs_python", "error", 389, "2026-10-16 09:00:00"),
                invariant("no_dangling_refs", "violated", 12, "2026-10-16 09:00:00"),
                invariant("no_dupe_paths", "violated", 1, "2026-10-16 09:00:00"),
            ],
            reconciliation_counts: ReconciliationCounts {
                open: 2,
                in_progress: 1,
                accepted: 1,
                review_due: 1,
            },
            reconciliations: vec![
                reconciliation("Machine", "box1", "open", 9, Some("2026-10-01 09:00:00")),
                reconciliation("File", "templedb/x.py", "in_progress", 1, Some(NOW)),
                reconciliation("Symbol", "templedb/y.py:f", "accepted", 4, None),
            ],
            machines: vec![
                MachineReconcile {
                    machine_name: "box1".into(),
                    last_deployed_at: Some("2026-09-01 00:00:00".into()),
                    last_run: Some("2026-10-17 10:00:00".into()),
                    last_status: Some("ok".into()),
                },
                MachineReconcile {
                    machine_name: "drifted".into(),
                    last_deployed_at: Some("2026-09-01 00:00:00".into()),
                    last_run: Some("2026-10-10 10:00:00".into()),
                    last_status: Some("drift".into()),
                },
                MachineReconcile {
                    machine_name: "unreconciled".into(),
                    last_deployed_at: Some("2026-09-01 00:00:00".into()),
                    last_run: None,
                    last_status: None,
                },
                MachineReconcile {
                    machine_name: "neverdeployed".into(),
                    last_deployed_at: None,
                    last_run: None,
                    last_status: None,
                },
            ],
            hygiene: vec![
                HygieneRow {
                    slug: "templedb".into(),
                    total_imports: 1200,
                    dead_candidates: 480,
                },
                HygieneRow {
                    slug: "bza".into(),
                    total_imports: 100,
                    dead_candidates: 15,
                },
                HygieneRow {
                    slug: "emacs-config".into(),
                    total_imports: 40,
                    dead_candidates: 1,
                },
            ],
            handoffs: HandoffInbox {
                direct: 2,
                broadcast: 1,
            },
        }
    }

    fn lines(facts: &SummaryFacts, color: bool) -> Vec<String> {
        render(facts, &clock(), "box1-123", color)
    }

    fn find(lines: &[String], needle: &str) -> String {
        lines
            .iter()
            .find(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("no line containing {needle:?} in {lines:#?}"))
            .clone()
    }

    /// One pane's lines: its header, and everything up to the next
    /// header.
    ///
    /// Panes have to be isolated before their rows can be asserted on,
    /// because the row *shapes* repeat across them — `    ! box1` is a
    /// doctor spotlight line and also a reconciliation line, and a
    /// whole-output `find` for either picks up whichever comes first.
    /// Two of these tests were wrong in exactly that way before this
    /// existed. Unpainted output only; a painted header does not start
    /// with its box-drawing run.
    fn pane(lines: &[String], header: &str) -> Vec<String> {
        let start = lines
            .iter()
            .position(|l| l.starts_with(header))
            .unwrap_or_else(|| panic!("no pane {header:?} in {lines:#?}"));
        let mut out = vec![lines[start].clone()];
        for l in &lines[start + 1..] {
            if l.starts_with("──") || l.starts_with("══") {
                break;
            }
            out.push(l.clone());
        }
        out
    }

    /// The pane order is Python's, and the two unported panes hold
    /// their places rather than being dropped — so adding them later
    /// does not move anything.
    #[test]
    fn the_panes_come_in_pythons_order() {
        let out = lines(&populated(), false);
        let headers: Vec<&str> = out
            .iter()
            .filter(|l| l.starts_with("══") || l.starts_with("──"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            headers,
            vec![
                "═════ TempleDB Summary ═════",
                "── Agent fleet (per project) ──",
                "── Ingestion (per adapter) ──",
                "── Systemd units ──",
                "── Doctor invariants (latest results) ──",
                "── Reconciliations (discrepancies being worked on) ──",
                "── Reconcile (per fleet machine) ──",
                "── Python hygiene ──",
                "── Handoff inbox ──",
            ]
        );
    }

    /// An unported pane says so. Silence would read as "no failed
    /// units" and "nothing waiting on you", which are claims this port
    /// cannot make.
    #[test]
    fn the_unported_panes_are_announced_not_omitted() {
        let out = lines(&populated(), false);
        assert!(find(&out, "Agent fleet").starts_with("── Agent fleet"));
        assert_eq!(
            find(&out, "not ported — the agent tables"),
            "  (not ported — the agent tables are outside the mirrored subset)"
        );
        assert_eq!(
            find(&out, "not ported — reads systemd"),
            "  (not ported — reads systemd, not the database)"
        );
    }

    /// Counts are grouped with Python's `{:,}`, and the top-kind line
    /// is indented to the same column as the counts it describes.
    #[test]
    fn the_graph_pane_groups_its_digits() {
        let out = lines(&populated(), false);
        assert_eq!(
            out[3],
            "  Entity graph       61,234 entities, 128,900 relations"
        );
        assert_eq!(
            out[4],
            "                     top: Symbol=21,770, File=13,817, Commit=2,709, Machine=6, ..."
        );
    }

    /// The three freshness classes and their colours, on one pane.
    /// `(never)` is red rather than yellow, because Python's ternary
    /// has no `never` arm and falls through to the `else`.
    #[test]
    fn ingest_ages_pick_their_colour_from_freshness() {
        let out = lines(&populated(), true);
        const GREEN: &str = "\x1b[32m";
        const YELLOW: &str = "\x1b[33m";
        const RED: &str = "\x1b[31m";

        let git = find(&out, "git  ");
        assert!(git.contains("last 30min ago"), "{git:?}");
        assert!(
            git.contains(&format!("{GREEN}2026-10-17 11:30:00")),
            "{git:?}"
        );

        // Nine days: `hours < threshold * 24` is false at a threshold of
        // one hour, so this is `ancient` and renders in days.
        let python = find(&out, "python ");
        assert!(python.contains("last 9d ago"), "{python:?}");
        assert!(
            python.contains(&format!("{RED}2026-10-08 12:00:00")),
            "{python:?}"
        );

        let nix = find(&out, "nix  ");
        assert!(nix.contains("last never"), "{nix:?}");
        assert!(nix.contains(&format!("{RED}(never)")), "{nix:?}");
        assert!(!nix.contains(YELLOW), "{nix:?}");
    }

    /// Python pads the *painted* timestamp to 40, so the nine bytes of
    /// escape are inside the column and the `N ok, N err` tail sits
    /// nine characters early. Reproducing a misalignment needs a test,
    /// because the obvious implementation is the one that fixes it.
    #[test]
    fn the_ingest_timestamp_column_counts_its_escape_codes() {
        let out = lines(&populated(), true);
        let git = find(&out, "git  ");
        // 19 characters of timestamp + 9 of escape = 28, padded to 40:
        // twelve spaces of padding, then the literal one in the format.
        assert!(git.contains("\x1b[0m             12 ok, 1 err"), "{git:?}");
    }

    /// Stripping the escapes from the painted output does not give the
    /// `--no-color` output, because the padding was computed over a
    /// different length. Worth pinning so nobody implements `--no-color`
    /// as a post-processing pass.
    #[test]
    fn no_color_is_not_the_painted_output_with_escapes_removed() {
        let painted = find(&lines(&populated(), true), "git  ");
        let plain = find(&lines(&populated(), false), "git  ");
        let stripped = strip_ansi(&painted);
        assert_ne!(stripped, plain);
        // The plain form is the one whose columns line up: 19 of
        // timestamp padded to 40 leaves 21 spaces before the tail.
        assert!(plain.contains("2026-10-17 11:30:00                      12 ok"));
        // And the stripped form is nine characters short of it.
        assert_eq!(stripped.len() + 9, plain.len());
    }

    fn strip_ansi(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// The header counts errored and violated separately, and dates the
    /// pane by its *stalest* check rather than its newest.
    #[test]
    fn the_doctor_header_separates_errors_from_violations() {
        let out = lines(&populated(), false);
        // `1d ago`, not `27h`: the `ancient` cutoff is
        // `threshold * 24`, and the doctor pane's threshold is one
        // hour, so anything past a day renders in days. The reconcile
        // pane's threshold of 168 makes the same arithmetic read very
        // differently — see `the_fleet_pane_splits_deployed_from_never_deployed`.
        assert_eq!(
            find(&out, "invariants tracked"),
            "  5 invariants tracked; 3 violated, 1 could not run  \
             — stalest 1d ago, re-run `templedb doctor entities`"
        );
    }

    /// Problems first, and the fresh passing check is *not* shown even
    /// though it is the most recent — which is the bug the ordering
    /// change fixed.
    #[test]
    fn the_doctor_spotlight_shows_problems_not_the_newest_three() {
        let out = lines(&populated(), false);
        let doctor = pane(&out, "── Doctor invariants");
        let spotlight: Vec<&String> = doctor
            .iter()
            .filter(|l| l.starts_with("    ✓") || l.starts_with("    ✗") || l.starts_with("    !"))
            .collect();
        assert_eq!(spotlight.len(), 3, "{doctor:#?}");
        // The errored check leads, because `errored + violated` puts it
        // first regardless of when it ran.
        assert_eq!(
            spotlight[0],
            &"    ! needs_python                                  \
              could not run (see `templedb doctor entities`)  (1d ago)"
        );
        // Its stale 389 issues are never printed: an errored check did
        // not run, so the count is left over from the last run that did.
        assert!(!out.iter().any(|l| l.contains("389")), "{out:#?}");
        // The fresh `ok` check is absent despite being the newest of
        // the five — which is the whole point of sorting problems
        // first rather than slicing by recency.
        assert!(!out.iter().any(|l| l.contains("no_orphans")), "{out:#?}");
        // Four problems, three shown.
        assert_eq!(
            find(&doctor, "and 1 more"),
            "    ... and 1 more — `templedb doctor entities`"
        );
    }

    /// A fresh check prints an empty age *through* `_c`, so the line
    /// ends in a muted escape pair wrapping nothing. Python's output
    /// really does carry those nine bytes.
    #[test]
    fn a_fresh_check_still_emits_the_muted_escape_pair() {
        let mut facts = empty();
        facts.invariants = vec![invariant("no_orphans", "ok", 0, "2026-10-17 11:59:00")];
        let out = lines(&facts, true);
        let line = find(&out, "no_orphans");
        assert!(line.ends_with("OK\x1b[90m\x1b[0m"), "{line:?}");
        // And nothing at all when the paint is off.
        let plain = find(&lines(&facts, false), "no_orphans");
        assert!(plain.ends_with("OK"), "{plain:?}");
    }

    /// `stale or rows`: once anything is stale the listing is replaced,
    /// not prefixed — so the in-progress row from today disappears.
    /// The "... N more" count still measures the full listing.
    #[test]
    fn stale_reconciliations_replace_the_listing() {
        let mut facts = populated();
        facts.reconciliations = vec![
            reconciliation("Machine", "box1", "open", 9, Some("2026-10-01 09:00:00")),
            reconciliation("File", "a.py", "in_progress", 1, Some(NOW)),
            reconciliation("File", "b.py", "accepted", 1, Some(NOW)),
            reconciliation("File", "c.py", "accepted", 1, Some(NOW)),
            reconciliation("File", "d.py", "accepted", 1, Some(NOW)),
        ];
        let out = lines(&facts, false);
        assert_eq!(find(&out, "untouched for"), "  1 untouched for 7+ days");
        let rows: Vec<&String> = out.iter().filter(|l| l.contains("item(s), idle")).collect();
        assert_eq!(rows.len(), 1, "{rows:#?}");
        assert!(rows[0].contains("Machine/box1"), "{rows:#?}");
        // Five rows, four of them not shown by `max_rows` — Python
        // compares the listing length, not the shown length.
        assert_eq!(
            find(&out, "more — `templedb reconcile list`"),
            "    ... 1 more — `templedb reconcile list`"
        );
    }

    /// A reconciliation never observed renders `idle Noned`, because
    /// Python interpolates the NULL its SQL `CAST` returned. It is on
    /// stdout, so it is reproduced rather than tidied to `-`.
    #[test]
    fn a_never_observed_reconciliation_prints_pythons_none() {
        let mut facts = empty();
        facts.reconciliation_counts.open = 1;
        facts.reconciliations = vec![reconciliation("Symbol", "x", "accepted", 4, None)];
        let out = lines(&facts, false);
        let row = find(&out, "item(s), idle");
        assert!(row.ends_with("item(s), idle Noned"), "{row:?}");
    }

    /// `subject_ref[:40]` slices characters and then pads to 40, so a
    /// multibyte ref must not be cut mid-character or counted in bytes.
    #[test]
    fn a_long_subject_ref_is_truncated_by_characters() {
        let mut facts = empty();
        facts.reconciliation_counts.open = 1;
        let long = "é".repeat(50);
        facts.reconciliations = vec![reconciliation("File", &long, "open", 1, Some(NOW))];
        let out = lines(&facts, false);
        let row = find(&out, "item(s), idle");
        let subject: String = "é".repeat(40);
        assert!(row.contains(&format!("File/{subject} ")), "{row:?}");
        assert!(!row.contains(&"é".repeat(41)), "{row:?}");
        // Exactly 40 characters, so the pad adds nothing.
        assert!(row.contains(&format!("{subject}    1 item(s)")), "{row:?}");
    }

    /// A machine deployed but never reconciled takes its own line and
    /// prints no age; a machine never deployed is not listed at all,
    /// only counted.
    #[test]
    fn the_fleet_pane_splits_deployed_from_never_deployed() {
        let out = pane(
            &lines(&populated(), false),
            "── Reconcile (per fleet machine)",
        );
        assert_eq!(
            find(&out, "box1"),
            "  ✓ box1                 last 2h ago         ok"
        );
        // 168 hours of threshold means `ancient` starts at 168 *days*,
        // so a week-old reconcile is still counted in hours — 170 of
        // them. Python's arithmetic; see `clock.rs`.
        assert_eq!(
            find(&out, "drifted"),
            "  ✗ drifted              last 170h ago       drift"
        );
        assert_eq!(
            find(&out, "unreconciled"),
            "  ? unreconciled         never reconciled"
        );
        assert_eq!(
            find(&out, "never deployed"),
            "  (1 never deployed via templedb, skipped: neverdeployed)"
        );
        // The undeployed machine is never given a row of its own.
        assert_eq!(
            out.iter().filter(|l| l.contains("neverdeployed")).count(),
            1
        );
    }

    /// The percentage picks the colour, rounds to whole percent, and
    /// the counts keep Python's `{:>3}` / `{:<4}` widths.
    #[test]
    fn hygiene_rows_colour_by_percentage() {
        let out = lines(&populated(), true);
        const GREEN: &str = "\x1b[32m";
        const YELLOW: &str = "\x1b[33m";
        const RED: &str = "\x1b[31m";

        // 480/1200 = 40%: red.
        let worst = find(&out, "templedb  ");
        assert!(worst.contains(&format!("{RED}480")), "{worst:?}");
        assert!(worst.contains(&format!("{RED}40%")), "{worst:?}");
        // 15/100 = 15%: yellow, and the count keeps its right-aligned
        // width of three.
        let mid = find(&out, "bza  ");
        assert!(mid.contains(&format!("{YELLOW} 15\x1b[0m/100 ")), "{mid:?}");
        // 1/40 = 2.5%, which `{:.0}` renders as `2` — half-to-even, the
        // same rule Python's format spec uses.
        let best = find(&out, "emacs-config");
        assert!(
            best.contains(&format!("{GREEN}  1\x1b[0m/40   ")),
            "{best:?}"
        );
        assert!(best.contains(&format!("{GREEN}2%")), "{best:?}");
    }

    /// The inbox is a function of which shell invoked the command, so
    /// the identity is threaded in and appears in two places.
    #[test]
    fn the_inbox_names_the_session_it_was_scoped_to() {
        let out = lines(&populated(), false);
        assert_eq!(
            find(&out, "unacked note(s)"),
            "  2 unacked note(s) for session box1-123"
        );
        assert_eq!(find(&out, "broadcast(s)"), "  1 unacked broadcast(s)");
        assert_eq!(
            find(&out, "view:"),
            "  view: templedb handoff list --for box1-123"
        );
    }

    /// Every pane's empty branch, in one pass — and the reconciliations
    /// pane absent entirely rather than empty, header included.
    #[test]
    fn an_empty_database_renders_every_placeholder() {
        let out = lines(&empty(), false);
        assert_eq!(out[3], "  Entity graph       0 entities, 0 relations");
        assert_eq!(out[4], "                     top: , ...");
        for needle in [
            "(no ingests recorded — run `templedb ingest all`)",
            "(no doctor runs — try `templedb doctor entities`)",
            "(no fleet_machines registered)",
            "(no python imports observed — run `templedb ingest python`)",
            "(no unacked handoffs for this session)",
        ] {
            assert_eq!(find(&out, needle), format!("  {needle}"));
        }
        // The whole pane is gone, not merely empty.
        assert!(
            !out.iter().any(|l| l.contains("Reconciliations")),
            "{out:#?}"
        );
    }

    /// One accepted row and no listing still draws the pane, because
    /// `any_live()` is true — the branch that is easy to get backwards.
    #[test]
    fn a_live_count_alone_is_enough_to_draw_the_reconciliation_pane() {
        let mut facts = empty();
        facts.reconciliation_counts.accepted = 1;
        let out = lines(&facts, false);
        assert!(
            out.iter().any(|l| l.contains("Reconciliations")),
            "{out:#?}"
        );
        assert_eq!(
            find(&out, "accepted"),
            "  0 open, 0 in progress, 1 accepted"
        );
        // Green: nothing open and nothing in progress.
        let painted = find(&lines(&facts, true), "accepted");
        assert!(painted.contains("\x1b[32m0 open"), "{painted:?}");
    }

    /// A review that has come due appends to the head line rather than
    /// recolouring it.
    #[test]
    fn a_due_review_is_appended_to_the_head_line() {
        let out = lines(&populated(), false);
        assert_eq!(
            find(&out, "open, 1 in progress"),
            "  2 open, 1 in progress, 1 accepted  \
             — 1 acceptance(s) past their review date"
        );
    }

    /// The output opens and closes with a blank line, as Python's
    /// leading and trailing `print()` do.
    #[test]
    fn the_output_is_bracketed_by_blank_lines() {
        let out = lines(&populated(), false);
        assert_eq!(out[0], "");
        assert_eq!(out[1], "═════ TempleDB Summary ═════");
        assert_eq!(out[2], "");
        assert_eq!(out.last().unwrap(), "");
    }
}
