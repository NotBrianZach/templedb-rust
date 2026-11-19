//! `templedb doctor history` — the read half of the doctor family.
//!
//! # Why only the read half, with numbers
//!
//! `doctor` has two subcommands. The README's porting order said for a
//! long time that both sat behind the write-path design document, on
//! the grounds that `doctor entities` inserts into `invariant_checks`.
//! That reasoning expired when the write path landed, so the entry was
//! re-measured on 2026-10-29 and the real obstacle is a different one.
//!
//! **`doctor entities` is 44 checks**, not a command. Classified by what
//! each one reads. Re-derived from the 44 method bodies on 2026-11-05,
//! because the previous version of this table was classified by name
//! and two of its entries were wrong in the direction that matters —
//! see *What the re-derivation moved* below:
//!
//! | | checks |
//! |---|---|
//! | **A** — reachable from the mirrored 21-table subset | 17 |
//! | **B** — need tables outside it | 10 |
//! | **C** — read SQLite *engine* internals: `pragma_foreign_key_check`, `pragma_function_list`, `sqlite_master`, the `-wal` sidecar | 8 |
//! | **D** — read a filesystem | 8 |
//! | **E** — read the Python implementation's own argparse tree | 1 |
//!
//! Of the 8 in D, only **4 read a checkout tree** —
//! `checkout_matches_db`, `checkout_files_are_mode_locked`,
//! `checkout_roles_are_unambiguous` and
//! `generated_file_matches_generator`. The other four read roots that
//! are not checkouts and that no tree fixture covers:
//! `~/.nix-profile` resolved into the store (`deployed_build_matches_db`),
//! `/nix/var/nix/profiles/system-*-link`
//! (`generation_log_covers_profile_links`),
//! `~/.local/share/templedb/backups` (`local_backups_within_budget`),
//! and `~/.local/share/templedb/fhs-deployments` plus both TempleDB
//! roots for `st_mode` (`exec_targets_are_executable`).
//!
//! # What the re-derivation moved
//!
//! The old table had one bucket of 15 called "read something that is
//! not a database" and it was doing two jobs. Splitting it reports a
//! blocker the single bucket hid:
//!
//! **C is blocked by the verification backend, not by the tree.** Those
//! eight checks ask SQLite about itself. A mirrored Postgres has no
//! `sqlite_master`, no `pragma_function_list` and no `-wal` sidecar, so
//! there is nothing for the cross-backend digest to compare and nothing
//! for the byte-diff sweep to run against. That is a harder constraint
//! than the tree-pinning harness and it was not visible while the two
//! categories shared a row.
//!
//! **Two checks the old table counted as filesystem reads are pure
//! SQL.** `working_state_references_valid_files` is a `LEFT JOIN`
//! orphan check over `vcs_working_state` and `project_files`, both
//! already mirrored, and it is portable today.
//! `checkout_roles_are_unambiguous` is a query over `checkouts` plus
//! exactly one `Path(...).is_dir()` liveness filter — it reads no file
//! content and needs no pinning, only directory existence.
//!
//! **One check is unportable in principle, not yet.**
//! `cli_help_is_populated` imports `cli.core` and walks argparse's
//! `_choices_actions`; its subject is the Python CLI's own parser tree.
//! There is no Rust statement of that invariant, so "arrives whole,
//! late" is not reachable for `doctor entities` either — at 43 of 44 the
//! exit status still cannot be reproduced.
//!
//! **One is portable but is a project.** `no_undefined_names` reads
//! templedb's Python source *out of `content_blobs`*, not off the disk,
//! so it is in category A — but running it means a Python parser and
//! scope analyser in Rust. The substrate is mirrored; the analysis is
//! not written.
//!
//! So a ported `doctor entities` would run 17 of 44 and print
//! `(not ported)` 27 times. That is the shape `summary` already has —
//! seven panes of nine — at a ratio that inverts the argument: `summary`
//! is 78% ported and names what is missing, and this would be 39% and
//! would mostly be naming. Its exit status is `0 if not problems else 1`
//! over the *whole* set, so a partial run cannot even reproduce the
//! status, which is the one part of a diagnostic command a script reads.
//!
//! It also writes three things, not one. `invariant_checks` is mirrored;
//! the claims it asserts on success and the `reconciliations` episodes
//! it opens and settles are a second and third write path, and the
//! claims tables are outside the subset entirely.
//!
//! None of that is an argument about difficulty. It is the boundary rule
//! the subset has followed since it was built — follow the readers — and
//! `doctor entities` is on the far side of it for the same reason the
//! systemd pane is.
//!
//! # `doctor history`, which is none of that
//!
//! A single `SELECT` over `invariant_checks`, already the seventeenth
//! mirrored table, and the projection turns out to be **exactly the five
//! columns already carried**. The query names seven: `duration_ms` and
//! `sample_issues_json` are selected and never printed — the same shape
//! as `vcs log`'s `files_changed`, a value computed and thrown away, and
//! the reason the mirror does not need to grow for this command.
//!
//! So this port is a new command that adds no table, no column and no
//! new verification machinery. It is byte-diffable, which is the
//! property `doctor entities` could never have.

use anyhow::{Context, Result};

use crate::store::Store;
use crate::value::Value;

/// One row of invariant-check history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRun {
    pub id: i64,
    pub check_name: String,
    pub ran_at: String,
    /// `'ok'`, `'violated'` or `'error'` in practice. Not an enum: the
    /// column has no CHECK on either side, the renderer has a `?`
    /// fallback for anything else, and inventing a variant for a value
    /// the live database does not hold would be the port deciding what
    /// a status is.
    pub status: String,
    pub issue_count: i64,
}

/// What a caller may vary.
#[derive(Debug, Clone, Copy, Default)]
pub struct HistoryFilter<'a> {
    /// One invariant by exact name. Python's `--check`.
    pub check: Option<&'a str>,
    /// Only rows whose status is not `'ok'`.
    pub violated_only: bool,
}

/// Recent invariant-check results, newest first.
///
/// # `id DESC` is a tie-break and it is load-bearing
///
/// Reproduced from Python, including the comment's reasoning, because
/// an implementation that dropped it would pass every small fixture and
/// be wrong on the live data. `ran_at` has second granularity and a
/// `doctor entities` pass inserts one row per check in well under a
/// second, so a full run leaves ~44 rows sharing a timestamp. `ORDER BY
/// ran_at DESC LIMIT 20` then returns an arbitrary 20 of those ties —
/// neither engine promises an order among equal keys — so the newest run
/// is reported as a random subset of itself, and which subset moves with
/// physical row layout.
///
/// That is the wrong answer to the one question the command exists to
/// answer, and over there it made a test pass or fail depending on what
/// had run before it. `id` is monotonic per insert, so within a tied
/// second it recovers the order the checks actually ran in.
///
/// This is also the one ordering in the port that is right on both
/// backends for the same reason rather than by coincidence: the tie-break
/// makes the key total, so there are no equal keys left for SQLite and
/// Postgres to break differently.
pub fn check_history(
    store: &dyn Store,
    filter: &HistoryFilter<'_>,
    limit: i64,
) -> Result<Vec<CheckRun>> {
    // Built the way Python builds it — a clause list joined with AND —
    // rather than as one query with `(?1 IS NULL OR check_name = ?1)`.
    // The second form reads better and plans worse: a nullable
    // predicate on `check_name` cannot use `invariant_checks_name_time`,
    // which is the index the mirror carries for exactly this lookup.
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(name) = filter.check {
        params.push(Value::from(name));
        clauses.push(format!("check_name = ?{}", params.len()));
    }
    if filter.violated_only {
        clauses.push("status != 'ok'".to_string());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    params.push(Value::from(limit));
    let sql = format!(
        "SELECT id, check_name, ran_at, status, issue_count
           FROM invariant_checks
           {where_sql}
          ORDER BY ran_at DESC, id DESC
          LIMIT ?{}",
        params.len()
    );

    let rows = store
        .query(&sql, &params)
        .context("reading invariant check history")?;
    rows.iter()
        .map(|r| {
            Ok(CheckRun {
                id: r.get_i64(0)?,
                check_name: r.get_string(1)?,
                ran_at: r.get_string(2)?,
                status: r.get_string(3)?,
                issue_count: r.get_i64(4)?,
            })
        })
        .collect()
}

/// The marker character for a status — Python's
/// `{'ok': '✓', 'violated': '✗', 'error': '!'}.get(status, '?')`.
pub fn marker(status: &str) -> char {
    match status {
        "ok" => '✓',
        "violated" => '✗',
        "error" => '!',
        _ => '?',
    }
}

/// The right-hand summary column.
///
/// Note what `'error'` does *not* print: Python has the row's
/// `sample_issues_json`, which holds the exception text for an errored
/// check, and prints the bare word `error` anyway. So the one column
/// that would explain a failure is in the table, selected by the query,
/// and discarded — which is why the mirror does not carry it.
pub fn summary(status: &str, issue_count: i64) -> String {
    match status {
        "ok" => "OK".to_string(),
        "violated" => format!("{issue_count} issue(s)"),
        _ => "error".to_string(),
    }
}

/// One rendered line, which is Python's
/// `f"  {m} #{r['id']:<5} {r['ran_at']}  {r['check_name']:<40} {summary}"`.
///
/// Both widths are minimums and neither truncates, so a check name past
/// 40 characters pushes the column rather than being cut —
/// `machines_with_generations_are_registered` is exactly 40 and
/// `every_generation_with_commit_has_relation` is 41, so the live data
/// reaches the overflow on every full run.
///
/// Padding by *character* and not by byte, which costs nothing today and
/// is the bug `summary`'s `subject_ref[:40]` slice already had to have
/// pointed out once: `{:<40}` in Rust counts chars, as Python's does,
/// and a byte-based pad would differ the first time a check name is not
/// ASCII.
pub fn render(run: &CheckRun) -> String {
    format!(
        "  {} #{:<5} {}  {:<40} {}",
        marker(&run.status),
        run.id,
        run.ran_at,
        run.check_name,
        summary(&run.status, run.issue_count)
    )
}

/// What Python prints when the table is empty.
pub const NO_HISTORY: &str =
    "(no invariant check history yet — run `templedb doctor entities` first)";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    fn run(id: i64, name: &str, ran_at: &str, status: &str, issues: i64) -> CheckRun {
        CheckRun {
            id,
            check_name: name.to_string(),
            ran_at: ran_at.to_string(),
            status: status.to_string(),
            issue_count: issues,
        }
    }

    /// The three markers Python defines, and the fallback for anything
    /// else. The column has no CHECK on either side, so the fallback is
    /// reachable rather than defensive.
    #[test]
    fn markers_are_pythons_three_plus_a_fallback() {
        assert_eq!(marker("ok"), '✓');
        assert_eq!(marker("violated"), '✗');
        assert_eq!(marker("error"), '!');
        assert_eq!(marker("running"), '?');
        assert_eq!(marker(""), '?');
    }

    /// `OK` for ok, a count for violated, and the bare word `error`
    /// otherwise — including for a status Python has no marker for,
    /// because its `if/elif/else` has no third test.
    #[test]
    fn the_summary_column_is_pythons_three_way_expression() {
        assert_eq!(summary("ok", 0), "OK");
        // An `ok` row with a non-zero count still says OK: Python
        // branches on the status, not on the number.
        assert_eq!(summary("ok", 7), "OK");
        assert_eq!(summary("violated", 1), "1 issue(s)");
        // No plural agreement, which is Python's — `issue(s)` always.
        assert_eq!(summary("violated", 2), "2 issue(s)");
        assert_eq!(summary("violated", 0), "0 issue(s)");
        assert_eq!(summary("error", 0), "error");
        assert_eq!(summary("something-new", 3), "error");
    }

    /// The exact line, against a hand-built row. Two leading spaces,
    /// the id left-padded to five *after* the `#`, two spaces after the
    /// timestamp, and the name left-padded to forty.
    #[test]
    fn a_line_matches_pythons_format_string() {
        let r = run(7061, "no_undefined_names", "2026-10-28 21:49:31", "ok", 0);
        assert_eq!(
            render(&r),
            "  ✓ #7061  2026-10-28 21:49:31  no_undefined_names                       OK"
        );
    }

    /// A violated row, and a name at exactly the column width — the
    /// boundary where a `<` pad adds nothing and a `truncate` would
    /// remove something.
    #[test]
    fn a_name_of_exactly_forty_characters_is_neither_padded_nor_cut() {
        let name = "machines_with_generations_are_registered";
        assert_eq!(name.len(), 40);
        let r = run(12, name, "2026-10-28 21:49:30", "violated", 3);
        assert_eq!(
            render(&r),
            format!("  ✗ #12    2026-10-28 21:49:30  {name} 3 issue(s)")
        );
    }

    /// Past the width the column grows, because Python's `{:<40}` is a
    /// minimum. The live data reaches this on every full run.
    #[test]
    fn a_longer_name_pushes_the_column_rather_than_truncating() {
        let name = "every_generation_with_commit_has_relation";
        assert_eq!(name.len(), 41);
        let line = render(&run(1, name, "2026-10-28 00:00:00", "ok", 0));
        assert!(line.contains(name), "{line}");
        assert!(line.ends_with(" OK"), "{line}");
    }

    /// A six-digit id overflows its own column the same way. `#{:<5}`
    /// stops padding rather than cutting, so the row stays readable and
    /// the columns after it shift — which is what Python does and what
    /// the table will do as soon as the id passes 99,999. It is at
    /// 7,061 today.
    #[test]
    fn an_id_past_the_column_width_shifts_the_row() {
        let line = render(&run(1_234_567, "c", "t", "ok", 0));
        assert!(line.starts_with("  ✓ #1234567 t  c"), "{line}");
    }

    /// Newest first, with `id DESC` deciding the ties — the property
    /// the whole command rests on.
    ///
    /// The fixture was built for `summary` and happens to be the right
    /// shape for this: three of its five rows share
    /// `2026-10-17 12:00:00`, because one `doctor entities` stamps
    /// every check in the same second, and their ids are 5, 3 and 2 —
    /// not contiguous, so an implementation that returned them in
    /// insertion order rather than by the tie-break would still look
    /// plausible and be wrong.
    #[test]
    fn ties_on_the_timestamp_are_broken_by_id_descending() {
        let c = testdb::fixture();
        let rows = check_history(&c, &HistoryFilter::default(), 100).unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![5, 3, 2, 1, 4]);
        // And the timestamps really are descending, which is the half
        // the tie-break does not cover.
        let times: Vec<&str> = rows.iter().map(|r| r.ran_at.as_str()).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted);
    }

    /// `--limit` cuts from the newest end, which is the only cut that
    /// answers "what happened most recently" — and it cuts *inside*
    /// the tied second, which is the case the tie-break exists for.
    #[test]
    fn the_limit_takes_the_newest_rows() {
        let c = testdb::fixture();
        let rows = check_history(&c, &HistoryFilter::default(), 2).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 3]);
    }

    /// `--check` is an exact name match, not a prefix and not a LIKE.
    #[test]
    fn the_check_filter_matches_one_name_exactly() {
        let c = testdb::fixture();
        let f = HistoryFilter {
            check: Some("no_orphans"),
            violated_only: false,
        };
        let rows = check_history(&c, &f, 100).unwrap();
        // Both of that name's runs, newest first.
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![2, 1]);
        // A prefix of a real name selects nothing, which is what
        // `= ?` means and what a LIKE would get wrong. The fixture
        // carries a `mod_a` / `modxa` pair for this question one table
        // over; here the point is that the predicate is not a pattern
        // at all.
        for near_miss in ["no_orph", "no_orphans ", "NO_ORPHANS", "%orphans"] {
            let f = HistoryFilter {
                check: Some(near_miss),
                violated_only: false,
            };
            assert!(
                check_history(&c, &f, 100).unwrap().is_empty(),
                "{near_miss:?} should select nothing"
            );
        }
    }

    /// `--violated-only` is `status != 'ok'`, so it keeps errors as
    /// well as violations. A filter that kept only `'violated'` would
    /// hide exactly the rows a drift hunt is looking for.
    #[test]
    fn violated_only_keeps_errors_too() {
        let c = testdb::fixture();
        let f = HistoryFilter {
            check: None,
            violated_only: true,
        };
        let rows = check_history(&c, &f, 100).unwrap();
        // ids 5 (error), 3 (violated), 1 (violated) — the two `ok`
        // rows are gone and the error is not.
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 3, 1]);
        let statuses: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
        assert!(statuses.contains(&"violated"), "{statuses:?}");
        assert!(statuses.contains(&"error"), "{statuses:?}");
    }

    /// Both filters at once are ANDed, which is the combination the
    /// clause-list build could get wrong by emitting `WHERE` twice or
    /// by numbering the placeholder against the wrong position.
    ///
    /// `no_orphans` is the right name to ask it with: it has one `ok`
    /// run and one `violated` one, so a build that dropped either
    /// clause returns two rows instead of one.
    #[test]
    fn the_two_filters_combine_with_and() {
        let c = testdb::fixture();
        let f = HistoryFilter {
            check: Some("no_orphans"),
            violated_only: true,
        };
        let rows = check_history(&c, &f, 100).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(rows[0].status, "violated");
    }

    /// A filter selecting nothing returns no rows rather than
    /// everything — the failure a mis-built `WHERE` produces, and the
    /// one a non-empty fixture hides when the assertion is only
    /// `is_empty() == false`.
    #[test]
    fn an_unknown_check_name_selects_nothing() {
        let c = testdb::fixture();
        let f = HistoryFilter {
            check: Some("no_such_invariant"),
            violated_only: false,
        };
        assert!(check_history(&c, &f, 100).unwrap().is_empty());
    }
}
