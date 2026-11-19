//! `file retype` — re-run the classifier over recorded files and report
//! what `project_files.file_type_id` gets wrong.
//!
//! The read half. `--apply` is **not** ported; see *The half that is
//! not here* below, which is the one divergence this module has.
//!
//! # Why the dry form is the valuable half, and it is not a consolation
//!
//! `project_files.file_type_id` is set once, when a file is first
//! recorded, by whatever classifier was in play then — so it is a
//! snapshot of a past decision rather than a derived value. Python's
//! docstring names two sources of wrongness that accumulated behind
//! that: `_get_file_type_id` used to carry its own 14-entry extension
//! map and fall back to `SELECT id FROM file_types LIMIT 1` for
//! anything outside it, and `file_types` was missing roughly half the
//! names the scanner emits, so even a correct classification had
//! nowhere to land.
//!
//! The command is **dry by default** and `--apply` writes. That makes
//! the default form a pure read with a diffable stdout — the only
//! command in the `file` family where the interesting behaviour is
//! reachable without writing anything. Every other write this port has
//! landed had to be verified by cross-read or by the copy technique,
//! because a write has no diffable stdout. This one's whole answer is
//! stdout, so it goes in the parity sweep and is byte-diffed against
//! Python on live data like a read.
//!
//! That is why the read half landed alone rather than waiting for the
//! write: the classifier is the substance of the command, and the dry
//! form is the only way to check 2,413 of its answers against Python at
//! once.
//!
//! # The half that is not here, and why the flag is absent rather than inert
//!
//! `--apply` resolves or creates a `file_types` row per target name and
//! then issues one `UPDATE project_files SET file_type_id = ?,
//! updated_at = datetime('now') WHERE id = ?` per change. It is not
//! ported, and the blocker is the create arm rather than the update:
//! `file_types.category` is `TEXT NOT NULL` with no default, and this
//! port mirrors `file_types` at **two of five columns** — `id` and
//! `type_name`. Python inserts `(type_name, 'uncategorized',
//! 'Auto-registered by file retype')`. So a faithful insert needs the
//! subset to grow two columns, which needs a `pg-reschema` and a full
//! re-mirror, and an *unfaithful* one would write a row this port alone
//! could read back.
//!
//! The flag is therefore **absent from the parser** rather than
//! accepted and ignored, and that is a deliberate choice between two
//! precedents this README already carries. `file set --no-fts` is
//! accepted and ignored under the rule that a flag switching *off*
//! behaviour the port does not have changes nothing. `--apply` is the
//! opposite shape: it switches *on* the only behaviour that writes, so
//! accepting it would mean `file retype --apply` exiting 0 having
//! written nothing, which is the silent divergence this project treats
//! as its one real defect. Refusing at the parser costs one
//! `FLAGS_EXPECTED` entry and exits 2 loudly.
//!
//! # One deliberate divergence: `ORDER BY pf.id`
//!
//! Python's query has **no `ORDER BY`**, and the order is not cosmetic
//! here — it decides the answer. `Counter.most_common(25)` breaks ties
//! by first-encounter order, which is row order, and the output is cut
//! at 25 kinds of change.
//!
//! Measured on the live database 2026-11-19: the unfiltered form finds
//! **26 kinds of change, 11 of them at a count of 1**, and the 25th and
//! 26th entries both have count 1. So *which* kind is printed and which
//! becomes `… and 1 more kinds of change` is whatever the planner
//! returned first. This port adds `ORDER BY pf.id`.
//!
//! `pf.id` rather than `(slug, file_path)` for `search files`'s reason:
//! it is the key that reproduces Python. Measured over all three forms
//! — unfiltered (2,413 rows), `-p templedb-rust` (54) and `--from-type
//! ansible_inventory` (44) — SQLite returns exactly the `pf.id`
//! ascending sequence today, so the added clause changes no current
//! output. Any other total key would reorder every move group for no
//! gain.
//!
//! The decisive argument is the one `intent list` and `search files`
//! made: this port has two backends and the sweep compares them against
//! each other. Without a total key, SQLite's `project_files`-by-rowid
//! scan and a Postgres sequential scan in heap order are both correct
//! and free to disagree — and because the cut falls inside a tie group
//! today, they would disagree about *which rows appear* rather than
//! only about their order.
//!
//! # What is reproduced rather than tidied
//!
//! - **A NULL `current_type` prints as the word `None`.** The join is
//!   a `LEFT JOIN`, Python compares `want == r['current_type']` — so a
//!   NULL is never equal to a target and the row is always a change —
//!   and the move label is an f-string, so `str(None)` reaches stdout.
//!   The fourth `str(None)` in this port after `format_table`'s three.
//!   Measured: **0 of the 2,413 active rows** have a NULL type today,
//!   so the branch is unreachable on live data and is pinned by a unit
//!   test instead.
//! - **The debris block counts every ignored row and lists ten.**
//!   Measured: **0 of 2,413** active paths match `SKIP_FILE_PATTERNS`
//!   today, so this block is also unreachable live and also unit
//!   tested. Its own `… and N more` arithmetic is `len - 10`, which is
//!   a different expression from the move list's `len - 25`, and the
//!   two are off-by-one in opposite directions if copied from each
//!   other.
//! - **The counts are of rows the command *checked*, not of rows the
//!   project has.** `len(rows)` is after the `status = 'active'` filter
//!   and after `--from-type`, which is why `--from-type
//!   ansible_inventory` prints `44 of 44` — a line that reads like a
//!   bug and is arithmetic.
//! - **An ignored row is excluded from the denominator's numerator but
//!   not from the denominator.** `len(rows)` counts it, `changes` does
//!   not. So a project of pure debris would print `0 of N`, reach the
//!   `not changes` branch, and say `Nothing to re-type (N active
//!   file(s) checked)` *after* having listed the debris.
//! - **`unresolved` counts paths the classifier cannot name**, which
//!   then become `text_file` — and the count is of *rows considered*,
//!   not of rows changed: a path that is unresolvable and already
//!   `text_file` increments `unresolved` and produces no change. So the
//!   trailing line can report paths that do not appear in any move
//!   above it. Reproduced; measured at 9 on the live database against
//!   88 changes.

use anyhow::{Context, Result};

use crate::classify::{classify_path, is_ignored_path, GENERIC_TEXT_TYPE};
use crate::store::Store;
use crate::value::Value;

/// Python's two optional narrowings, which are a positional and a flag.
#[derive(Debug, Default, Clone)]
pub struct RetypeFilter<'a> {
    /// `projects.id`, already resolved. Resolution is the caller's, the
    /// same split every other read here uses.
    pub project_id: Option<i64>,
    /// `--from-type`, compared against `file_types.type_name`.
    ///
    /// Note that this is an **inner** condition on a `LEFT JOIN`ed
    /// column, so passing it silently drops every row whose
    /// `file_type_id` is NULL or dangling — the rows most likely to
    /// need re-typing. Python's, and reproduced: the clause is appended
    /// to the same `WHERE` rather than moved into the `ON`.
    pub from_type: Option<&'a str>,
}

/// One row the classifier disagrees with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// `project_files.id`, which `--apply` would write.
    pub file_id: i64,
    pub slug: String,
    pub file_path: String,
    /// The `file_types.type_name` the row holds, or `None` for a NULL
    /// or dangling `file_type_id`. Printed as the word `None`.
    pub from: Option<String>,
    /// What the classifier says, which is [`GENERIC_TEXT_TYPE`] when it
    /// cannot name the path at all.
    pub to: &'static str,
}

/// An active row whose path the scanner would now refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Debris {
    pub slug: String,
    pub file_path: String,
}

/// Everything the renderer needs, with no clock and no formatting.
///
/// The facts/rendering split `summary.rs` established: this is
/// assertable without a terminal and the renderer is assertable without
/// a database.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RetypeProposal {
    /// `len(rows)` — rows the command considered, after the status
    /// filter and after both narrowings.
    pub checked: usize,
    /// In `pf.id` order, which is the module header's divergence.
    pub changes: Vec<Change>,
    /// Also in `pf.id` order; the renderer prints the first ten.
    pub debris: Vec<Debris>,
    /// Paths [`classify_path`] returned `None` for. Not the same as the
    /// number of changes to `text_file`; see the module header.
    pub unresolved: usize,
}

/// Read every active row the filter admits and ask the classifier about
/// each one.
///
/// Pure read: no clock, no session, no transaction. The one thing it
/// does that Python's does not is order the rows.
pub fn propose(store: &dyn Store, filter: &RetypeFilter<'_>) -> Result<RetypeProposal> {
    // Python's `where, params = "1=1", []` and two appends, kept as a
    // clause list for `doctor history`'s reason: a nullable predicate
    // on `pf.project_id` cannot use `idx_project_files_project_id`,
    // which is the index the mirror carries for this lookup.
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(id) = filter.project_id {
        params.push(Value::from(id));
        clauses.push(format!("pf.project_id = ?{}", params.len()));
    }
    if let Some(name) = filter.from_type {
        params.push(Value::from(name));
        clauses.push(format!("ft.type_name = ?{}", params.len()));
    }
    clauses.push("pf.status = 'active'".to_string());

    let sql = format!(
        "SELECT pf.id, pf.file_path, pr.slug, ft.type_name
           FROM project_files pf
           JOIN projects pr ON pr.id = pf.project_id
           LEFT JOIN file_types ft ON ft.id = pf.file_type_id
          WHERE {}
          ORDER BY pf.id",
        clauses.join(" AND ")
    );

    let rows = store
        .query(&sql, &params)
        .context("reading active files to re-type")?;

    let mut proposal = RetypeProposal {
        checked: rows.len(),
        ..Default::default()
    };
    for row in &rows {
        let file_id = row.get_i64(0)?;
        let file_path = row.get_string(1)?;
        let slug = row.get_string(2)?;
        let current = row.get_opt_string(3)?;

        // A path the scanner would now refuse has no right type. Say so
        // instead of assigning one — Python's comment is that these are
        // debris recorded before the filter existed, and giving them a
        // tidy type would make them look deliberate.
        if is_ignored_path(&file_path) {
            proposal.debris.push(Debris { slug, file_path });
            continue;
        }
        let want = match classify_path(&file_path) {
            Some(name) => name,
            None => {
                proposal.unresolved += 1;
                GENERIC_TEXT_TYPE
            }
        };
        // `want == r['current_type']`, where a NULL compares unequal to
        // everything and is therefore always a change.
        if current.as_deref() == Some(want) {
            continue;
        }
        proposal.changes.push(Change {
            file_id,
            slug,
            file_path,
            from: current,
            to: want,
        });
    }
    Ok(proposal)
}

/// How many kinds of change the renderer prints before it summarises
/// the rest. Python's `most_common(25)`.
const MOVE_LIMIT: usize = 25;

/// How many debris paths the renderer lists. Python's `ignored[:10]`,
/// and a *different* number from [`MOVE_LIMIT`] — the two `… and N
/// more` lines are separate arithmetic and the module header says why
/// that matters.
const DEBRIS_LIMIT: usize = 10;

/// The move label and its count, most common first.
///
/// Python is `Counter(f"{current} -> {want}").most_common(25)`, and the
/// two properties that a reimplementation gets wrong are both about
/// ties:
///
/// 1. **Counts descending**, and
/// 2. **first-encounter order within a count.** `Counter.most_common`
///    has guaranteed that since CPython 3.7, and `heapq.nlargest`
///    preserves it for the `n`-argument form too.
///
/// So this is a *stable* sort by count descending over labels in the
/// order they were first seen, which is `pf.id` order because of the
/// `ORDER BY`. `sort_by_key` is stable in Rust, and `Reverse` on the
/// count keeps the label order untouched rather than sorting labels
/// alphabetically as a tie-break — which is what a `BTreeMap` would
/// have done, and what the first draft of this function did.
fn move_counts(changes: &[Change]) -> Vec<(String, usize)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for change in changes {
        // `f"{r['current_type']} -> {want}"`. `str(None)` is the literal
        // word `None`, which is the whole reason this is built as a
        // string rather than as a pair.
        let label = format!(
            "{} -> {}",
            change.from.as_deref().unwrap_or("None"),
            change.to
        );
        let seen = counts.entry(label.clone()).or_insert(0);
        if *seen == 0 {
            order.push(label);
        }
        *seen += 1;
    }
    let mut out: Vec<(String, usize)> = order
        .into_iter()
        .map(|label| {
            let n = counts[&label];
            (label, n)
        })
        .collect();
    out.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    out
}

/// Python's stdout for a proposal, line by line.
///
/// Every string here is `print(f"…")` from `cli/commands/file.py:752`.
/// The two that are easy to get wrong are the right-aligned count —
/// `f"  {n:5}  {move}"`, which is two spaces, a width-5 right-aligned
/// integer, then **two** more spaces — and the blank line that is part
/// of the header's own f-string (`…disagrees with:\n`) rather than a
/// separate `print()`.
pub fn render(proposal: &RetypeProposal) -> Vec<String> {
    let mut lines = Vec::new();

    if !proposal.debris.is_empty() {
        lines.push(format!(
            "{} active file(s) match a path the scanner now ignores — debris, not content. \
             Not re-typed; `templedb file rm` is the fix:",
            proposal.debris.len()
        ));
        for d in proposal.debris.iter().take(DEBRIS_LIMIT) {
            lines.push(format!("    {}/{}", d.slug, d.file_path));
        }
        if proposal.debris.len() > DEBRIS_LIMIT {
            lines.push(format!(
                "    … and {} more",
                proposal.debris.len() - DEBRIS_LIMIT
            ));
        }
        // Python's bare `print()`. An empty line, not an absent one —
        // the same shape `intent create`'s line six has.
        lines.push(String::new());
    }

    if proposal.changes.is_empty() {
        lines.push(format!(
            "Nothing to re-type ({} active file(s) checked).",
            proposal.checked
        ));
        return lines;
    }

    let moves = move_counts(&proposal.changes);
    // The `\n` inside Python's f-string, which makes the blank line
    // part of this `print` rather than a following one.
    lines.push(format!(
        "{} of {} active file(s) are recorded with a type the classifier disagrees with:",
        proposal.changes.len(),
        proposal.checked
    ));
    lines.push(String::new());
    for (label, n) in moves.iter().take(MOVE_LIMIT) {
        lines.push(format!("  {n:5}  {label}"));
    }
    if moves.len() > MOVE_LIMIT {
        lines.push(format!(
            "  … and {} more kinds of change",
            moves.len() - MOVE_LIMIT
        ));
    }
    if proposal.unresolved > 0 {
        // Python's f-string opens with `\n`, so the blank line belongs
        // to this `print` too.
        lines.push(String::new());
        lines.push(format!(
            "  {} path(s) the classifier cannot name at all would become '{GENERIC_TEXT_TYPE}'.",
            proposal.unresolved
        ));
    }
    lines
}

/// The last two lines of the dry form, which are Python's
/// `print("\nDry run. Re-run with --apply to write these.")`.
///
/// Separate from [`render`] rather than appended to it, because Python
/// prints it only when there *are* changes — the `not changes` branch
/// returns before reaching it — and because `--apply` would replace it
/// with `Re-typed N file(s).` A renderer that owned both would have to
/// take a flag for a half of the command this port does not have.
pub const DRY_RUN_NOTICE: &str = "Dry run. Re-run with --apply to write these.";

#[cfg(test)]
mod tests {
    use super::*;

    fn change(id: i64, path: &str, from: Option<&str>, to: &'static str) -> Change {
        Change {
            file_id: id,
            slug: "p".to_string(),
            file_path: path.to_string(),
            from: from.map(str::to_string),
            to,
        }
    }

    /// The counts are descending and ties keep first-encounter order.
    ///
    /// The property `Counter.most_common` guarantees and a `BTreeMap`
    /// does not. Written so that alphabetical order and
    /// first-encounter order disagree: `z -> a` is seen before
    /// `a -> b`, both at count 1, and `z -> a` must print first.
    #[test]
    fn move_counts_are_descending_with_first_encounter_ties() {
        let changes = vec![
            change(1, "a", Some("z"), "a"),
            change(2, "b", Some("a"), "b"),
            change(3, "c", Some("m"), "n"),
            change(4, "d", Some("m"), "n"),
        ];
        let moves = move_counts(&changes);
        assert_eq!(
            moves,
            vec![
                ("m -> n".to_string(), 2),
                ("z -> a".to_string(), 1),
                ("a -> b".to_string(), 1),
            ],
            "ties must hold insertion order, not alphabetical order"
        );
    }

    /// A NULL `file_type_id` prints as the word `None`.
    ///
    /// Unreachable on the live database — 0 of 2,413 active rows — and
    /// reachable from the table, which is why it is pinned here rather
    /// than left to the sweep. The fourth `str(None)` in this port.
    #[test]
    fn a_null_current_type_prints_the_word_none() {
        let moves = move_counts(&[change(1, "a.rs", None, "rust")]);
        assert_eq!(moves, vec![("None -> rust".to_string(), 1)]);
    }

    /// The count column is width 5, right-aligned, with two spaces on
    /// each side of it.
    ///
    /// `f"  {n:5}  {move}"`. Asserted as exact bytes because every
    /// plausible mistake here — one space, width 4, left alignment —
    /// produces output that looks correct in isolation.
    #[test]
    fn the_count_column_is_pythons_width_and_padding() {
        let proposal = RetypeProposal {
            checked: 10,
            changes: vec![change(1, "a.rs", Some("api_route"), "rust")],
            ..Default::default()
        };
        let lines = render(&proposal);
        assert_eq!(
            lines,
            vec![
                "1 of 10 active file(s) are recorded with a type the classifier disagrees with:"
                    .to_string(),
                String::new(),
                "      1  api_route -> rust".to_string(),
            ]
        );
        // Two spaces, then the integer right-aligned in five columns —
        // so a one-digit count sits in column 7.
        assert_eq!(&lines[2][..8], "      1 ");
    }

    /// Nothing to do prints one line and nothing else, and the count is
    /// of rows *checked*.
    #[test]
    fn an_empty_proposal_is_one_line_naming_the_rows_checked() {
        let lines = render(&RetypeProposal {
            checked: 2413,
            ..Default::default()
        });
        assert_eq!(
            lines,
            vec!["Nothing to re-type (2413 active file(s) checked).".to_string()]
        );
        // No thousands separator: Python's is `{len(rows)}` on an int.
        assert!(!lines[0].contains("2,413"));
    }

    /// The unresolved line is preceded by a blank line and names
    /// [`GENERIC_TEXT_TYPE`] in single quotes.
    #[test]
    fn the_unresolved_line_carries_its_own_blank_line() {
        let proposal = RetypeProposal {
            checked: 3,
            changes: vec![change(1, "Session.vim", Some("api_route"), "text_file")],
            unresolved: 9,
            ..Default::default()
        };
        let lines = render(&proposal);
        assert_eq!(lines[lines.len() - 2], "");
        assert_eq!(
            lines[lines.len() - 1],
            "  9 path(s) the classifier cannot name at all would become 'text_file'."
        );
        // Absent rather than zero when nothing was unresolved.
        let none = render(&RetypeProposal {
            unresolved: 0,
            ..proposal.clone()
        });
        assert!(!none.iter().any(|l| l.contains("cannot name")));
    }

    /// Past 25 kinds the tail is summarised, and the arithmetic is
    /// `len - 25`.
    #[test]
    fn the_move_list_is_cut_at_twenty_five_kinds() {
        // 26 distinct labels, every one at count 1, so the cut lands
        // inside a tie group — which is the live situation the module
        // header measures.
        let changes: Vec<Change> = (0..26)
            .map(|i| Change {
                file_id: i,
                slug: "p".to_string(),
                file_path: format!("f{i}"),
                from: Some(format!("t{i}")),
                to: "rust",
            })
            .collect();
        let proposal = RetypeProposal {
            checked: 26,
            changes,
            ..Default::default()
        };
        let lines = render(&proposal);
        assert_eq!(lines.last().unwrap(), "  … and 1 more kinds of change");
        // Header, blank, 25 moves, the tail line.
        assert_eq!(lines.len(), 2 + MOVE_LIMIT + 1);
        // And at exactly 25 the tail line is absent.
        let mut trimmed = proposal.clone();
        trimmed.changes.pop();
        assert!(!render(&trimmed)
            .iter()
            .any(|l| l.contains("more kinds of change")));
    }

    /// The debris block counts everything, lists ten, and is followed
    /// by a blank line.
    ///
    /// Unreachable on the live database — 0 of 2,413 active paths match
    /// `SKIP_FILE_PATTERNS` — so this is the only check the sweep can
    /// never make. Its `… and N more` is `len - 10`, a different
    /// expression from the move list's `len - 25`.
    #[test]
    fn the_debris_block_lists_ten_and_counts_all() {
        let debris: Vec<Debris> = (0..13)
            .map(|i| Debris {
                slug: "p".to_string(),
                file_path: format!("f{i}.orig"),
            })
            .collect();
        let lines = render(&RetypeProposal {
            checked: 13,
            debris,
            ..Default::default()
        });
        assert!(lines[0].starts_with("13 active file(s) match a path the scanner now ignores"));
        assert!(lines[0].ends_with("`templedb file rm` is the fix:"));
        assert_eq!(lines[1], "    p/f0.orig");
        assert_eq!(lines[DEBRIS_LIMIT], "    p/f9.orig");
        assert_eq!(lines[DEBRIS_LIMIT + 1], "    … and 3 more");
        assert_eq!(lines[DEBRIS_LIMIT + 2], "");
        // Then the no-changes line, because debris is excluded from
        // `changes` and included in `checked` — so a project of pure
        // debris prints both blocks.
        assert_eq!(
            lines[DEBRIS_LIMIT + 3],
            "Nothing to re-type (13 active file(s) checked)."
        );
        // Exactly ten paths, not eleven.
        assert_eq!(lines.iter().filter(|l| l.starts_with("    p/")).count(), 10);
    }

    /// At exactly ten debris paths there is no `… and N more`.
    #[test]
    fn ten_debris_paths_need_no_tail_line() {
        let debris: Vec<Debris> = (0..10)
            .map(|i| Debris {
                slug: "p".to_string(),
                file_path: format!("f{i}.orig"),
            })
            .collect();
        let lines = render(&RetypeProposal {
            checked: 10,
            debris,
            ..Default::default()
        });
        assert!(!lines.iter().any(|l| l.contains("and 0 more")));
        assert!(!lines.iter().any(|l| l.trim_start().starts_with("… and")));
    }

    /// The dry-run notice is its own constant and is not part of
    /// [`render`].
    #[test]
    fn the_dry_run_notice_is_not_printed_for_an_empty_proposal() {
        let lines = render(&RetypeProposal {
            checked: 1,
            ..Default::default()
        });
        assert!(!lines.iter().any(|l| l.contains("Dry run")));
        assert_eq!(
            DRY_RUN_NOTICE,
            "Dry run. Re-run with --apply to write these."
        );
    }
}
