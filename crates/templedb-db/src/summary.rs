//! `summary`'s observation layer: the facts, and nothing that
//! interprets them.
//!
//! # Why this module is shaped differently from the others
//!
//! Every other query module here returns what its command prints.
//! This one deliberately does not. The reasoning is in the TempleDB
//! report *summary is not a function of the database* (2026-10-18),
//! written while porting it; the short version is that `summary` is
//! the first command in the port with inputs that are not rows — the
//! wall clock, `os.getppid()`, the hostname, systemd, and
//! `TEMPLEDB_SESSION_ID` — and that the byte-diff method which has
//! verified 49 invocations is, without having been designed as one, a
//! test for exactly that property.
//!
//! So the command is split three ways:
//!
//! | layer | depends on | lives in |
//! |---|---|---|
//! | observation | the database alone | here |
//! | interpretation — ages, freshness, colour | the facts **plus an instant passed in** | the CLI |
//! | environment — systemd, session identity | the machine | the CLI |
//!
//! What that buys is not tidiness. It is that this half is a function,
//! so it can be diffed across backends, mirrored, and tested without a
//! clock; and the other half is small enough to test at a fixed
//! instant. The two queries below that *do* need the clock take it as
//! a parameter rather than reading it, which is the same rule the
//! write path will need when every insert stamps `datetime('now')`.
//!
//! # Absolute timestamps, every time
//!
//! Every struct here carries the timestamp, never an age. Python's
//! doctor and reconcile panes render `(15h ago)` and `idle 0d` with no
//! absolute anywhere, which is lossy in a system that stored the exact
//! instant and whose agents' output is itself ingested back into the
//! graph. The port prints both; see the README's divergence list.

use anyhow::{Context, Result};

use crate::clock::Clock;
use crate::store::Store;
use crate::value::Value;
use crate::Dialect;

/// Entity and relation totals, plus the four largest kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphFacts {
    pub entities: i64,
    pub relations: i64,
    /// `(kind, count)`, largest first. Four of them, as Python's
    /// `LIMIT 4`.
    pub top_kinds: Vec<(String, i64)>,
}

/// One ingest adapter's history, collapsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterRuns {
    pub adapter: String,
    /// `MAX(started_at)`. `None` only if every row for this adapter
    /// has a NULL — the column is `NOT NULL`, so in practice never.
    pub last_run: Option<String>,
    pub ok_count: i64,
    pub err_count: i64,
}

/// The latest result for one invariant check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantResult {
    pub check_name: String,
    /// `ok` | `violated` | `error`. Kept as the string rather than an
    /// enum: the CHECK constraint is in SQLite and a value outside it
    /// should render as itself rather than become `Unknown`.
    pub status: String,
    pub ran_at: Option<String>,
    pub issue_count: i64,
}

impl InvariantResult {
    /// A check that **errored** did not run, so its `issue_count` is
    /// whatever the last successful run left behind.
    ///
    /// Python keeps these separate for a reason it records: a check
    /// dead for days went on displaying a stale `389 issue(s)` from
    /// its last good run, counted among the violations.
    pub fn could_not_run(&self) -> bool {
        self.status == "error"
    }

    pub fn violated(&self) -> bool {
        self.status != "ok" && self.status != "error"
    }
}

/// One fleet machine and its most recent reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineReconcile {
    pub machine_name: String,
    /// `NULL` for a machine never deployed through TempleDB. Those are
    /// listed separately, because only a deployed machine has a
    /// reconcile baseline to be measured against.
    pub last_deployed_at: Option<String>,
    pub last_run: Option<String>,
    pub last_status: Option<String>,
}

impl MachineReconcile {
    pub fn deployed(&self) -> bool {
        self.last_deployed_at.is_some()
    }
}

/// Dead-import candidates for one project slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HygieneRow {
    pub slug: String,
    pub total_imports: i64,
    pub dead_candidates: i64,
}

/// The reconciliation status histogram.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReconciliationCounts {
    pub open: i64,
    pub in_progress: i64,
    pub accepted: i64,
    /// Accepted rows whose `review_after` has arrived. The one count
    /// that needs a clock, which is why [`reconciliation_counts`]
    /// takes a date rather than reading one.
    pub review_due: i64,
}

impl ReconciliationCounts {
    pub fn any_live(&self) -> bool {
        self.open > 0 || self.in_progress > 0 || self.accepted > 0
    }
}

/// One open or in-progress reconciliation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    pub kind: String,
    pub subject_ref: String,
    pub status: String,
    pub item_count: i64,
    pub owner: Option<String>,
    /// Absolute, not an idle count. Python computes
    /// `CAST(julianday('now') - julianday(last_observed_at) AS INT)` in
    /// SQL; here the column comes back as it is stored and
    /// [`Clock::days_since`] does the arithmetic, so this function
    /// stays one.
    pub last_observed_at: Option<String>,
    pub first_detected_at: Option<String>,
}

/// Unacked handoff notes, for one session identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffInbox {
    pub direct: i64,
    pub broadcast: i64,
}

impl HandoffInbox {
    pub fn is_empty(&self) -> bool {
        self.direct == 0 && self.broadcast == 0
    }
}

/// A one-row, one-column `COUNT(*)`.
///
/// `Store` carries `query` and `query_opt` and nothing else, so the
/// "this aggregate always returns a row" reasoning lives here rather
/// than being asserted at five call sites. An aggregate with no
/// `GROUP BY` returns exactly one row on both engines even over an
/// empty table; the `None` arm is therefore unreachable and says so
/// rather than silently reading as zero.
fn count(store: &dyn Store, sql: &str, params: &[Value], what: &str) -> Result<i64> {
    store
        .query_opt(sql, params)
        .with_context(|| format!("counting {what}"))?
        .with_context(|| format!("counting {what}: aggregate returned no row"))?
        .get_i64(0)
}

/// `COUNT(*)` on `entities` and `relations`, and the top four kinds.
pub fn graph_facts(store: &dyn Store) -> Result<GraphFacts> {
    let entities = count(store, "SELECT COUNT(*) FROM entities", &[], "entities")?;
    let relations = count(store, "SELECT COUNT(*) FROM relations", &[], "relations")?;
    // `ORDER BY n DESC` alone is Python's, and it ties: two kinds with
    // the same count come back in planner order. `kind` breaks it,
    // which costs nothing — on live data the top four counts are
    // 21,770 / 21,322 / 13,817 / 2,709 and no tie is close — and makes
    // the row set a sequence rather than a set.
    let top_kinds = store
        .query(
            "SELECT kind, COUNT(*) AS n FROM entities
              GROUP BY kind ORDER BY n DESC, kind LIMIT 4",
            &[],
        )
        .context("counting entity kinds")?
        .into_iter()
        .map(|r| Ok((r.get_string(0)?, r.get_i64(1)?)))
        .collect::<Result<_>>()?;

    Ok(GraphFacts {
        entities,
        relations,
        top_kinds,
    })
}

/// Last run and ok/error tallies per ingest adapter, newest first.
pub fn ingest_adapters(store: &dyn Store) -> Result<Vec<AdapterRuns>> {
    // Every column here is either grouped or aggregated, so this one
    // is portable as written — unlike the two below.
    store
        .query(
            "SELECT adapter,
                    MAX(started_at) AS last_run,
                    SUM(CASE WHEN status = 'ok' THEN 1 ELSE 0 END) AS ok_count,
                    SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) AS err_count
               FROM ingestion_runs
              GROUP BY adapter
              ORDER BY last_run DESC, adapter",
            &[],
        )
        .context("reading ingestion runs")?
        .into_iter()
        .map(|r| {
            Ok(AdapterRuns {
                adapter: r.get_string(0)?,
                last_run: r.get_opt_string(1)?,
                ok_count: r.get_i64(2)?,
                err_count: r.get_i64(3)?,
            })
        })
        .collect()
}

/// The latest result per invariant check, newest first.
///
/// # The one query in the port that cannot be written once
///
/// Python's is:
///
/// ```sql
/// SELECT check_name, status, MAX(ran_at) AS ran_at, issue_count
///   FROM invariant_checks GROUP BY check_name ORDER BY ran_at DESC
/// ```
///
/// `status` and `issue_count` are neither grouped nor aggregated,
/// which is an error in Postgres (`42803`) and in standard SQL. It
/// works on SQLite because of a documented extension: with exactly one
/// `min()` or `max()` aggregate, the bare columns take their values
/// **from the row that produced the extreme**. So it means "the status
/// and issue count of each check's most recent run", and that is
/// exactly what the pane needs.
///
/// The tempting repair — wrapping the bare columns in `MAX()` —
/// compiles on both engines and is wrong: it would return the
/// alphabetically largest status rather than the latest one, and on
/// today's data `violated` > `ok` would hide a check that has since
/// started passing.
///
/// So this is the second place in the crate where one question needs
/// two SQL texts, after `search content`. The difference is written
/// out here rather than hidden behind the placeholder rewriter,
/// because `dialect.rs` exists for differences with no behavioural
/// content and this one has plenty.
///
/// **A tie-break Python does not have.** `ORDER BY ran_at DESC` alone
/// ties constantly here: one `doctor entities` run stamps every check
/// within the same second, so 42 checks routinely share a timestamp
/// and the planner picks their order. That matters because the pane
/// prints only the first three problems, so *which three* is
/// unspecified. `(ran_at DESC, check_name)` makes it a sequence. The
/// same reasoning, and the same kind of divergence, as `entity trace`.
pub fn invariant_results(store: &dyn Store) -> Result<Vec<InvariantResult>> {
    let sql = match store.dialect() {
        Dialect::Sqlite => {
            "SELECT check_name, status, MAX(ran_at) AS ran_at, issue_count
               FROM invariant_checks
              GROUP BY check_name
              ORDER BY ran_at DESC, check_name"
        }
        Dialect::Postgres => {
            "SELECT check_name, status, ran_at, issue_count FROM (
                 SELECT DISTINCT ON (check_name)
                        check_name, status, ran_at, issue_count
                   FROM invariant_checks
                  ORDER BY check_name, ran_at DESC, id DESC
             ) latest
              ORDER BY ran_at DESC, check_name"
        }
    };
    store
        .query(sql, &[])
        .context("reading invariant checks")?
        .into_iter()
        .map(|r| {
            Ok(InvariantResult {
                check_name: r.get_string(0)?,
                status: r.get_string(1)?,
                ran_at: r.get_opt_string(2)?,
                issue_count: r.get_i64(3)?,
            })
        })
        .collect()
}

/// Every fleet machine with its most recent reconcile, by name.
///
/// The same SQLite-only shape as [`invariant_results`]: Python selects
/// a bare `fm.last_deployed_at` while grouping on `fm.machine_name`,
/// with `MAX(rr.ran_at)` as the single aggregate. On Postgres that is
/// an error even though `machine_name` is unique per network, because
/// the functional dependency the planner accepts is on the primary
/// key, and this groups by a different column.
///
/// Python's `last_status` comes from a correlated subquery rather than
/// from the grouped row — `ORDER BY ran_at DESC LIMIT 1` — which is a
/// second, independent "latest row" with its own untied ordering. Both
/// are reproduced, the subquery with a tie-break added.
pub fn fleet_reconciles(store: &dyn Store) -> Result<Vec<MachineReconcile>> {
    let sql = match store.dialect() {
        Dialect::Sqlite => {
            "SELECT fm.machine_name,
                    fm.last_deployed_at,
                    MAX(rr.ran_at) AS last_run,
                    (SELECT status FROM reconcile_runs rr2
                      WHERE rr2.machine_name = fm.machine_name
                      ORDER BY rr2.ran_at DESC, rr2.id DESC LIMIT 1) AS last_status
               FROM fleet_machines fm
               LEFT JOIN reconcile_runs rr ON rr.machine_name = fm.machine_name
              GROUP BY fm.machine_name
              ORDER BY fm.machine_name"
        }
        Dialect::Postgres => {
            "SELECT fm.machine_name,
                    MAX(fm.last_deployed_at) AS last_deployed_at,
                    MAX(rr.ran_at) AS last_run,
                    (SELECT status FROM reconcile_runs rr2
                      WHERE rr2.machine_name = fm.machine_name
                      ORDER BY rr2.ran_at DESC, rr2.id DESC LIMIT 1) AS last_status
               FROM fleet_machines fm
               LEFT JOIN reconcile_runs rr ON rr.machine_name = fm.machine_name
              GROUP BY fm.machine_name
              ORDER BY fm.machine_name"
        }
    };
    store
        .query(sql, &[])
        .context("reading fleet reconciles")?
        .into_iter()
        .map(|r| {
            Ok(MachineReconcile {
                machine_name: r.get_string(0)?,
                last_deployed_at: r.get_opt_string(1)?,
                last_run: r.get_opt_string(2)?,
                last_status: r.get_opt_string(3)?,
            })
        })
        .collect()
}

/// Per-slug dead-import candidates, worst first, top five.
///
/// The same shape as `entity dead-imports` aggregated by slug, and the
/// slug is extracted from a `File` entity's `external_ref` — which is
/// `<slug>/<path>` — rather than joined, because the entity graph has
/// no project id on it.
pub fn hygiene(store: &dyn Store) -> Result<Vec<HygieneRow>> {
    // `substr`/`instr` are SQLite spellings. Postgres has `substr` but
    // calls the other one `strpos`, with the arguments the other way
    // round, so this is a third dialect split — mechanical, unlike the
    // two above, because the two expressions mean the same thing.
    let slug_expr = match store.dialect() {
        Dialect::Sqlite => "substr(fe.external_ref, 1, instr(fe.external_ref, '/') - 1)",
        Dialect::Postgres => "substr(fe.external_ref, 1, strpos(fe.external_ref, '/') - 1)",
    };
    let sql = format!(
        "WITH imports AS (
           SELECT {slug_expr} AS slug, fe.id AS from_id, te.id AS to_id
             FROM relations r
             JOIN entities fe ON fe.id = r.from_entity_id
             JOIN entities te ON te.id = r.to_entity_id
            WHERE r.kind = 'imports'
              AND fe.kind = 'File' AND te.kind = 'File'
         ),
         bridges AS (
           SELECT imp.slug, imp.from_id, imp.to_id,
                  SUM(CASE WHEN dr_to.id IS NOT NULL THEN 1 ELSE 0 END)
                      AS bridge_count
             FROM imports imp
             LEFT JOIN relations dr_from
               ON dr_from.from_entity_id = imp.from_id
              AND dr_from.kind = 'defines'
             LEFT JOIN entities fsym
               ON fsym.id = dr_from.to_entity_id AND fsym.kind = 'Symbol'
             LEFT JOIN relations cr
               ON cr.from_entity_id = fsym.id
              AND cr.kind IN ('calls', 'inherits', 'uses')
             LEFT JOIN entities tsym
               ON tsym.id = cr.to_entity_id AND tsym.kind = 'Symbol'
             LEFT JOIN relations dr_to
               ON dr_to.from_entity_id = imp.to_id
              AND dr_to.kind = 'defines'
              AND dr_to.to_entity_id = tsym.id
            GROUP BY imp.slug, imp.from_id, imp.to_id
         )
         SELECT slug,
                COUNT(*) AS total_imports,
                SUM(CASE WHEN bridge_count = 0 THEN 1 ELSE 0 END)
                    AS dead_candidates
           FROM bridges
          GROUP BY slug
         HAVING COUNT(*) > 0
          ORDER BY dead_candidates DESC, total_imports DESC, slug
          LIMIT 5"
    );
    store
        .query(&sql, &[])
        .context("reading python hygiene")?
        .into_iter()
        .map(|r| {
            Ok(HygieneRow {
                slug: r.get_string(0)?,
                total_imports: r.get_i64(1)?,
                dead_candidates: r.get_i64(2)?,
            })
        })
        .collect()
}

/// The reconciliation status histogram.
///
/// `today` is `date('now')` — passed in rather than read, because that
/// is the whole point of this module. [`Clock::today`] produces it.
pub fn reconciliation_counts(store: &dyn Store, today: &str) -> Result<ReconciliationCounts> {
    let mut counts = ReconciliationCounts::default();
    for row in store
        .query(
            "SELECT status, COUNT(*) FROM reconciliations GROUP BY status",
            &[],
        )
        .context("counting reconciliations")?
    {
        let n = row.get_i64(1)?;
        match row.get_string(0)?.as_str() {
            "open" => counts.open = n,
            "in_progress" => counts.in_progress = n,
            "accepted" => counts.accepted = n,
            // `settled` and anything a future producer invents are
            // counted by the query and not displayed, exactly as in
            // Python: its `out` dict is pre-seeded with the known
            // statuses and the pane reads three of them.
            _ => {}
        }
    }
    counts.review_due = count(
        store,
        "SELECT COUNT(*) FROM reconciliations
          WHERE status = 'accepted' AND review_after IS NOT NULL
            AND review_after <= ?1",
        &[Value::from(today)],
        "reviews due",
    )?;
    Ok(counts)
}

/// Open, in-progress and accepted reconciliations, worst first.
///
/// Python's `LIVE_STATUSES` and its `ORDER BY CASE status` are
/// reproduced; `datetime(first_detected_at)` is dropped to a plain
/// column comparison, because every value in the column is already
/// `YYYY-MM-DD HH:MM:SS` and `datetime()` on it is the identity.
pub fn live_reconciliations(store: &dyn Store, limit: i64) -> Result<Vec<Reconciliation>> {
    store
        .query(
            "SELECT kind, subject_ref, status, item_count, owner,
                    last_observed_at, first_detected_at
               FROM reconciliations
              WHERE status IN ('open', 'in_progress', 'accepted')
              ORDER BY CASE status WHEN 'open' THEN 0
                                   WHEN 'in_progress' THEN 1
                                   WHEN 'accepted' THEN 2 ELSE 3 END,
                       first_detected_at, kind, subject_ref
              LIMIT ?1",
            &[Value::from(limit)],
        )
        .context("reading reconciliations")?
        .into_iter()
        .map(|r| {
            Ok(Reconciliation {
                kind: r.get_string(0)?,
                subject_ref: r.get_string(1)?,
                status: r.get_string(2)?,
                item_count: r.get_i64(3)?,
                owner: r.get_opt_string(4)?,
                last_observed_at: r.get_opt_string(5)?,
                first_detected_at: r.get_opt_string(6)?,
            })
        })
        .collect()
}

/// Unacked notes addressed to `session_id`, and unacked broadcasts.
///
/// `session_id` is an argument for the same reason `today` is: Python
/// builds it from `TEMPLEDB_SESSION_ID` or `(hostname, getppid())` at
/// the point of use, which makes the pane a function of which shell
/// invoked the command. Resolving it at the edge at least puts the
/// dependency somewhere a caller can see and a test can fix.
pub fn handoff_inbox(store: &dyn Store, session_id: &str) -> Result<HandoffInbox> {
    let direct = count(
        store,
        "SELECT COUNT(*) FROM handoff_notes
          WHERE to_session = ?1 AND acked_at IS NULL",
        &[Value::from(session_id)],
        "direct handoffs",
    )?;
    let broadcast = count(
        store,
        "SELECT COUNT(*) FROM handoff_notes
          WHERE to_session IS NULL AND to_topic IS NULL
            AND acked_at IS NULL",
        &[],
        "broadcast handoffs",
    )?;
    Ok(HandoffInbox { direct, broadcast })
}

/// Everything the database can say, in one call.
///
/// A struct rather than eight calls at the call site so that the CLI
/// cannot accidentally interleave a query between two panes and read a
/// database that moved underneath it. The live file is written by the
/// Python implementation while this runs; an ingest landing between
/// the adapter pane and the doctor pane would make the output describe
/// two different instants.
#[derive(Debug, Clone)]
pub struct SummaryFacts {
    pub graph: GraphFacts,
    pub adapters: Vec<AdapterRuns>,
    pub invariants: Vec<InvariantResult>,
    pub reconciliation_counts: ReconciliationCounts,
    pub reconciliations: Vec<Reconciliation>,
    pub machines: Vec<MachineReconcile>,
    pub hygiene: Vec<HygieneRow>,
    pub handoffs: HandoffInbox,
}

/// Collect every pane's facts.
///
/// Takes the clock only to derive `date('now')` for the review-due
/// count, and the session id for the inbox. Neither is read here.
pub fn observe(store: &dyn Store, clock: &Clock, session_id: &str) -> Result<SummaryFacts> {
    Ok(SummaryFacts {
        graph: graph_facts(store)?,
        adapters: ingest_adapters(store)?,
        invariants: invariant_results(store)?,
        reconciliation_counts: reconciliation_counts(store, &clock.today())?,
        reconciliations: live_reconciliations(store, 50)?,
        machines: fleet_reconciles(store)?,
        hygiene: hygiene(store)?,
        handoffs: handoff_inbox(store, session_id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    #[test]
    fn graph_facts_rank_kinds_and_break_ties_by_name() {
        let c = testdb::fixture();
        let facts = graph_facts(&c).unwrap();
        assert!(facts.entities > 0);
        assert!(facts.relations > 0);
        assert!(facts.top_kinds.len() <= 4);
        // Descending by count, and alphabetical within a tie — which
        // the fixture has, since most kinds appear once or twice.
        for pair in facts.top_kinds.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(a.1 > b.1 || (a.1 == b.1 && a.0 < b.0), "{a:?} then {b:?}");
        }
    }

    #[test]
    fn adapters_collapse_to_one_row_each_with_tallies() {
        let c = testdb::fixture();
        let rows = ingest_adapters(&c).unwrap();
        let git = rows.iter().find(|r| r.adapter == "git").unwrap();
        assert_eq!(git.last_run.as_deref(), Some("2026-10-17 12:00:00"));
        assert_eq!((git.ok_count, git.err_count), (2, 1));
        // Newest adapter first.
        assert_eq!(rows[0].adapter, "git");
        // One row per adapter, never one per run.
        assert_eq!(rows.len(), 2);
    }

    /// The bare-column `GROUP BY` is load-bearing: the status and
    /// issue count must come from the *latest* row, not from an
    /// arbitrary one and not from the alphabetically largest. The
    /// fixture gives `no_orphans` an older `violated` run with 9
    /// issues and a newer `ok` run with 0, so wrapping the columns in
    /// `MAX()` — the repair that compiles on both engines — would
    /// report it as still violated.
    #[test]
    fn the_latest_run_wins_not_the_largest_status() {
        let c = testdb::fixture();
        let rows = invariant_results(&c).unwrap();
        let fixed = rows.iter().find(|r| r.check_name == "no_orphans").unwrap();
        assert_eq!(fixed.status, "ok");
        assert_eq!(fixed.issue_count, 0);
        assert_eq!(fixed.ran_at.as_deref(), Some("2026-10-17 12:00:00"));

        let broken = rows.iter().find(|r| r.check_name == "no_cycles").unwrap();
        assert_eq!(broken.status, "violated");
        assert_eq!(broken.issue_count, 3);
        assert!(broken.violated() && !broken.could_not_run());

        let dead = rows
            .iter()
            .find(|r| r.check_name == "needs_python")
            .unwrap();
        assert!(dead.could_not_run() && !dead.violated());
    }

    /// Newest first, and ties broken by name rather than by the
    /// planner — one `doctor entities` run stamps every check with the
    /// same second, so the tie is the normal case rather than an edge.
    #[test]
    fn invariants_are_ordered_newest_first_then_by_name() {
        let c = testdb::fixture();
        let rows = invariant_results(&c).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.check_name.as_str()).collect();
        // All three latest runs share one timestamp — which is what a
        // single `doctor entities` produces — so this is entirely the
        // tie-break talking. Under Python's `ORDER BY ran_at DESC`
        // alone, these three come back in whatever order the planner
        // picks, and the pane shows the first three problems.
        assert_eq!(names, vec!["needs_python", "no_cycles", "no_orphans"]);
        assert!(rows
            .iter()
            .all(|r| r.ran_at.as_deref() == Some("2026-10-17 12:00:00")));
    }

    #[test]
    fn machines_carry_their_latest_reconcile_and_the_undeployed_are_visible() {
        let c = testdb::fixture();
        let rows = fleet_reconciles(&c).unwrap();
        let names: Vec<_> = rows.iter().map(|r| r.machine_name.as_str()).collect();
        assert_eq!(names, vec!["box1", "neverdeployed", "unreconciled"]);

        let box1 = &rows[0];
        assert!(box1.deployed());
        assert_eq!(box1.last_run.as_deref(), Some("2026-10-17 11:00:00"));
        // The newest run's status, not the first inserted.
        assert_eq!(box1.last_status.as_deref(), Some("ok"));

        // Deployed, never reconciled: a row with no runs survives the
        // LEFT JOIN rather than vanishing.
        let never = rows
            .iter()
            .find(|r| r.machine_name == "unreconciled")
            .unwrap();
        assert!(never.deployed());
        assert_eq!(never.last_run, None);
        assert_eq!(never.last_status, None);

        let undeployed = rows
            .iter()
            .find(|r| r.machine_name == "neverdeployed")
            .unwrap();
        assert!(!undeployed.deployed());
    }

    #[test]
    fn reconciliation_counts_depend_on_the_date_they_are_given() {
        let c = testdb::fixture();
        let before = reconciliation_counts(&c, "2026-10-01").unwrap();
        assert_eq!(
            (before.open, before.in_progress, before.accepted),
            (2, 1, 1)
        );
        // `settled` is counted by the query and displayed nowhere.
        assert_eq!(before.review_due, 0);

        // The same database, a later date: the accepted row's review
        // has come due. Nothing about the data changed — which is the
        // argument for the date being an argument.
        let after = reconciliation_counts(&c, "2026-12-01").unwrap();
        assert_eq!(after.review_due, 1);
        assert!(after.any_live());
    }

    #[test]
    fn live_reconciliations_sort_open_first_and_exclude_settled() {
        let c = testdb::fixture();
        let rows = live_reconciliations(&c, 50).unwrap();
        let statuses: Vec<_> = rows.iter().map(|r| r.status.as_str()).collect();
        assert_eq!(
            statuses,
            vec!["open", "open", "in_progress", "accepted"],
            "{rows:#?}"
        );
        assert!(rows.iter().all(|r| r.status != "settled"));
        // Absolute, never an age: the interpretation layer's job.
        assert_eq!(
            rows[0].last_observed_at.as_deref(),
            Some("2026-10-10 09:00:00")
        );

        assert_eq!(live_reconciliations(&c, 2).unwrap().len(), 2);
    }

    #[test]
    fn the_inbox_is_scoped_to_the_session_it_is_given() {
        let c = testdb::fixture();
        let mine = handoff_inbox(&c, "box1-123").unwrap();
        assert_eq!((mine.direct, mine.broadcast), (1, 1));
        assert!(!mine.is_empty());

        // Same database, different shell: Python's inbox changes with
        // the ppid, and reproducing that faithfully means the count
        // really does move.
        let theirs = handoff_inbox(&c, "box1-999").unwrap();
        assert_eq!((theirs.direct, theirs.broadcast), (0, 1));

        // An acked note and a topic-addressed one are both excluded
        // from the broadcast count.
        assert_eq!(mine.broadcast, 1);
    }

    #[test]
    fn observe_collects_every_pane() {
        let c = testdb::fixture();
        let clock = Clock::at("2026-10-17 12:00:00").unwrap();
        let o = observe(&c, &clock, "box1-123").unwrap();
        assert_eq!(o.graph.entities, graph_facts(&c).unwrap().entities);
        assert_eq!(o.adapters.len(), 2);
        assert_eq!(o.invariants.len(), 3);
        assert_eq!(o.machines.len(), 3);
        assert_eq!(o.handoffs.direct, 1);
        // 2026-10-17 is before the fixture's review_after of
        // 2026-11-01, so this is the date doing its job.
        assert_eq!(o.reconciliation_counts.review_due, 0);
    }
}
