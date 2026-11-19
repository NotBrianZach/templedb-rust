//! `file mark` — the seventh write, and the first that needs no clock.
//!
//! One column, one row: `project_files.edit_mode`, which migration 106
//! added to say how a file is meant to be *edited* rather than what it
//! holds. Three values, of which the default is spelled by a NULL.
//!
//! # Zero schema cost, and a reader that was already there
//!
//! No new table, no new column, no new view — the fifth step running at
//! zero. `project_files.edit_mode` has been mirrored since `file ls -l`
//! needed it: `file ls -l` prints it, with a NULL rendered as the word
//! `immutable`, and `file ls --edit-mode` filters on it. So this write
//! is the rarest shape available here — a column this port already
//! byte-diffs a *reader* against Python on, which means the cross-read
//! runs through output the sweep verifies rather than through a query
//! written for the test.
//!
//! # The first write with no clock, and why that is the finding
//!
//! Every write before this one takes a [`Clock`](crate::Clock): step 13
//! built the seam precisely so a write's timestamps would be values a
//! test can assert rather than columns a state diff has to exclude.
//! This one takes none, and the reason is not that it writes no
//! timestamp — it is that the timestamp it moves is moved by something
//! this port does not have.
//!
//! Python's statement is `UPDATE project_files SET edit_mode = ? WHERE
//! id = ?` and names one column. The live SQLite database then fires
//! `update_project_files_updated_at`, one of the three triggers on that
//! table that are *not* cr-sqlite's:
//!
//! ```sql
//! CREATE TRIGGER update_project_files_updated_at
//! AFTER UPDATE ON project_files FOR EACH ROW BEGIN
//!     UPDATE project_files SET updated_at = datetime('now') WHERE id = NEW.id;
//! END
//! ```
//!
//! Measured on a copy of the live database 2026-11-18: before
//! `templedb file mark templedb-rust .gitignore --edit-mode hot-reload`
//! the row's `updated_at` was `2026-10-16 15:27:09`, and after it was
//! `2026-11-19 03:33:59`. Python's code never names the column.
//!
//! **This port does not reproduce that bump, and cannot without a
//! schema change.** `project_files` is mirrored at seven columns and
//! `updated_at` is not one of them, because the subset follows the
//! readers and no reader in either implementation reads it — the only
//! two `SELECT`s naming `pf.updated_at` are in `services/
//! component_service.py`, which nothing imports and which also selects
//! two columns the table does not have. So the divergence is latent in
//! the strongest available sense: there is no query on either side that
//! could observe it.
//!
//! It is recorded here, and in *Known gaps*, for two reasons rather
//! than one. The first is the project's rule — a silent divergence is
//! the defect. The second is that **it is not new**: `file set`'s
//! un-delete arm is `UPDATE project_files SET status = 'active'`, which
//! has fired the same trigger since the first write in step 15, and
//! nothing noticed for a month. Adding the column for `file mark` alone
//! would fix the newer writer and leave the older one wrong, which is a
//! worse state than two writers diverging identically and in writing.
//! See *Known gaps* for what closing it would take.
//!
//! The other two triggers do not fire. `file_contents_fts_file_update`
//! is `AFTER UPDATE OF file_path, status`, and `edit_mode` is in
//! neither list, so this is the first `project_files` write in this port
//! that touches **no** full-text index — which is what makes it the
//! clean case for the column question rather than one tangled up in the
//! FTS one.
//!
//! # The divergence that is Python's output rather than this port's
//!
//! Python's success line interpolates `args.project` — *the string the
//! caller typed* — and resolves the row through `fuzzy_match_project`.
//! The two are not the same thing, and the gap is observable:
//! `templedb file mark templedb-RUST .gitignore --edit-mode hot-reload`
//! printed
//!
//! ```text
//! ✓ templedb-RUST/.gitignore: edit_mode immutable → hot-reload
//! ```
//!
//! and wrote the `templedb-rust` row. There is no project called
//! `templedb-RUST`. Measured on the copy 2026-11-18, and reported
//! upstream rather than fixed here.
//!
//! This port is byte-identical anyway, for a reason worth stating
//! because it looks like luck: [`resolve_project_exact`] accepts only
//! the exact slug, so the string the caller typed and the slug of the
//! row written are the same string by construction. The divergence is
//! confined to inputs this port refuses outright, which is the shape
//! every write's project resolution has here.
//!
//! It is the mirror image of `file rm`'s finding one command over.
//! There the line carries **no** project prefix and a caller cannot
//! tell which project was staged; here it carries one and the one it
//! carries can name a project that does not exist. Neither is a rule,
//! which is the point: each of Python's `file` writes interpolates
//! whatever that function happened to have in scope.
//!
//! # Check-then-act, measured rather than tightened on principle
//!
//! Python reads the row, decides, and then issues an unconditional
//! `UPDATE`. This port does the lookup, the read of the old value and
//! the write in **one statement**, with a self-join in `FROM` supplying
//! the pre-command value to `RETURNING`:
//!
//! ```sql
//! UPDATE project_files AS pf SET edit_mode = $1
//!   FROM project_files AS was
//!  WHERE was.id = pf.id AND pf.project_id = $2 AND pf.file_path = $3
//! RETURNING pf.id, was.edit_mode
//! ```
//!
//! Two round trips become one and the missing-file refusal falls out of
//! `RETURNING` returning no row, which is the smaller half of why. The
//! larger half is that the old value is what the command *prints*, and
//! printing a value another transaction has already replaced is a
//! command reporting a transition that did not happen.
//!
//! Unlike step 20's branch-head CAS and step 25's cancel this is **not**
//! a compare-and-swap, and the distinction is load-bearing rather than
//! pedantic: `mark` has no precondition on the old value, so there is
//! nothing to refuse. The window being closed is a reporting one, not a
//! lost update.
//!
//! Measured by `a_racing_mark_reports_the_value_it_actually_replaced` in
//! `write_parity.rs`: a transaction that reads the row, has it changed
//! under it, then marks it reports the value it **overwrote**, where
//! Python's shape would report the one its own earlier `SELECT`
//! returned. That test also names the case it does not reach — a commit
//! landing while this statement is blocked on the row lock, which goes
//! through Postgres's `EvalPlanQual` recheck and needs two threads to
//! produce. Nothing here claims anything about that interleaving.
//!
//! # What is reproduced that a tidier port would have changed
//!
//! - **An unconditional write, including when nothing changes.**
//!   Marking a `hot-reload` file `hot-reload` still issues the `UPDATE`
//!   and still prints `edit_mode hot-reload → hot-reload`. Measured on
//!   the copy; the `updated_at` trigger fired for it too.
//! - **A soft-deleted row is markable.** Python's lookup has no
//!   `status` filter, so `file mark` succeeds on a `status = 'deleted'`
//!   row — measured: `templedb-rust/src/main.py` took `live-symlink`
//!   and exited 0. It is the one success this command has that **no
//!   reader can see**, because `file ls` filters on `status` and
//!   therefore lists neither the file nor its new mode. Reproduced
//!   rather than refused: the row exists, the column is real, and
//!   `file set`'s un-delete would make it visible again.
//! - **`immutable` is stored as NULL**, not as the string. So
//!   `file mark … --edit-mode immutable` followed by
//!   `file ls --edit-mode immutable` lists nothing, because Python's
//!   filter compares the raw column and NULL is not that string. Both
//!   halves are already in this port — the filter since `file ls`, the
//!   write here — and the pair is worth naming because it reads like a
//!   bug in whichever one you look at alone.
//! - **No session, no intent, no stage.** Measured on the copy across
//!   six invocations: `vcs_sessions` stayed at 1,005 rows,
//!   `edit_intents` at 1,655 and `vcs_working_state` at 1,950. This is
//!   therefore the first write here with **no session divergence to
//!   argue** — `file rm` refuses where Python creates, and this one
//!   never asks.

use anyhow::{bail, Context, Result};

/// The three values `file mark` accepts, which are Python's `choices=`
/// list verbatim.
///
/// A separate type from [`EditModeFilter`](crate::EditModeFilter), which
/// has four, because Python's two `choices=` lists are different lists:
/// `file ls --edit-mode` additionally takes `non-immutable`, a predicate
/// that is not a value the column can hold. Collapsing them would make
/// `file mark --edit-mode non-immutable` expressible, which is a write
/// with no meaning and a flag surface this port would be alone in
/// having.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    /// The default, and the one stored as NULL.
    Immutable,
    HotReload,
    LiveSymlink,
}

impl EditMode {
    /// The word on the right of Python's arrow, which is `args.edit_mode`
    /// and therefore the word the caller typed — *not* the value the
    /// column takes. For `Immutable` those two differ; see
    /// [`EditMode::column`].
    pub fn as_str(self) -> &'static str {
        match self {
            EditMode::Immutable => "immutable",
            EditMode::HotReload => "hot-reload",
            EditMode::LiveSymlink => "live-symlink",
        }
    }

    /// The value written to `project_files.edit_mode`.
    ///
    /// `None` for `immutable`, which is Python's
    /// `None if args.edit_mode == 'immutable' else args.edit_mode` and
    /// not a shortcut: the column has no CHECK constraint and no
    /// DEFAULT, every one of the 2,785 live rows holds NULL, and
    /// `file ls -l` renders NULL as the word. Writing the string would
    /// produce a row that printed the same and filtered differently.
    pub fn column(self) -> Option<&'static str> {
        match self {
            EditMode::Immutable => None,
            other => Some(other.as_str()),
        }
    }
}

/// Python's success line, byte for byte.
///
/// Measured rather than read off the source, by step 31's technique —
/// `TEMPLEDB_PATH` pointed at a copy of the live 494 MB SQLite file, so
/// a write's *output* is diffable even though its effect is not. Through
/// `cat -A`, for `file mark templedb-rust .gitignore --edit-mode
/// hot-reload`:
///
/// ```text
/// M-bM-^\M-^S templedb-rust/.gitignore: edit_mode immutable M-bM-^FM-^R hot-reload$
/// ```
///
/// 65 bytes with the newline. Two multibyte characters, and they are
/// different ones: U+2713 CHECK MARK opens the line, as in every other
/// `file` write here, and U+2192 RIGHTWARDS ARROW separates the two
/// modes — which no other line in this port uses, and which is *not*
/// the `→` of `intent apply`'s second line. That one is the same
/// codepoint reached by a different `print`, so a shared constant would
/// couple two renderers that happen to agree.
///
/// `from` is the raw column, and Python renders it with
/// `row['edit_mode'] or 'immutable'`. That is an `or` and not a null
/// check, so **an empty string prints as `immutable` too** — Python's
/// falsiness, reproduced here with an explicit `is_empty` rather than
/// with `unwrap_or`, which would have printed the empty string and
/// looked right in every test this port can reach from its own writes.
/// The column is nullable text with no CHECK constraint, so `''` is a
/// value the live table can hold and a reader here can be handed.
pub fn mark_line(project: &str, file_path: &str, from: Option<&str>, to: EditMode) -> String {
    format!(
        "\u{2713} {project}/{file_path}: edit_mode {} \u{2192} {}",
        from.filter(|s| !s.is_empty()).unwrap_or("immutable"),
        to.as_str()
    )
}

/// What a mark did, for the caller and for the tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkOutcome {
    /// The `project_files.id` that was written.
    pub file_id: i64,
    /// The raw `edit_mode` the row held before, which is `None` both
    /// for a file that was never marked and for one marked back to
    /// `immutable`. The two are the same row state and Python prints
    /// them identically.
    pub previous: Option<String>,
}

/// Set `file_path`'s `edit_mode` in `project_id`.
///
/// No clock parameter, which is the module header's subject. No session
/// either: Python resolves none here and writes no owned row, so unlike
/// [`remove_file`](crate::remove_file) there is nothing for *resolve
/// never, accept always* to cost.
///
/// The one refusal is a plain `bail!`, which is [`USAGE`](crate::USAGE)
/// — exit 1, and Python's for all three of this command's failures that
/// reach its body: an unresolved project, a missing file, and an
/// `--edit-mode` outside the three (which is dead code over there,
/// because argparse's `choices=` rejects it first and exits 2). Measured
/// on the copy 2026-11-18: missing file 1, unresolved project 1, bad
/// mode 2, absent `--edit-mode` 2.
pub fn mark_file(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    file_path: &str,
    mode: EditMode,
) -> Result<MarkOutcome> {
    // The lookup, the read of the replaced value and the write in one
    // command. `was` is the same table joined on the primary key, so
    // `RETURNING` can name the pre-command value of a column the same
    // statement is overwriting. See the module header for what this
    // does and does not claim about a race.
    //
    // No `status` filter, which is Python's and is a decision here: see
    // the module note on soft-deleted rows.
    let row = tx
        .query_opt(
            "UPDATE project_files AS pf
                SET edit_mode = $1
               FROM project_files AS was
              WHERE was.id = pf.id
                AND pf.project_id = $2
                AND pf.file_path = $3
          RETURNING pf.id, was.edit_mode",
            &[&mode.column(), &project_id, &file_path],
        )
        .context("setting a file's edit mode")?;

    match row {
        Some(row) => Ok(MarkOutcome {
            file_id: row.get(0),
            previous: row.get(1),
        }),
        // Python's `File not found: {args.project}/{args.file_path}`,
        // which carries the typed project string for the same reason its
        // success line does. This port's names the path it was given and
        // not the project, because the project it would name is the one
        // the caller typed and this resolver already refused anything
        // else.
        None => bail!(
            "no file {file_path:?} in this project.\n\
             `file mark` sets a column on a row that already exists; it does not \
             create one. List them: templedb-rs file ls <project>"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes Python printed on a copy of the live database,
    /// 2026-11-18, for each of the four transitions that were run.
    ///
    /// Pinned rather than described, which is step 31's rule: the one
    /// thing a renderer written from `print(f"...")` cannot be checked
    /// against is the `f"..."` it was written from. The byte counts are
    /// what `wc -c` reported for Python's stdout, newline included.
    #[test]
    fn every_measured_transition_is_pythons_bytes() {
        let cases = [
            // NULL → hot-reload, 65 bytes.
            (
                None,
                EditMode::HotReload,
                "immutable \u{2192} hot-reload",
                65,
            ),
            // The no-op, 66 bytes. Python still writes.
            (
                Some("hot-reload"),
                EditMode::HotReload,
                "hot-reload \u{2192} hot-reload",
                66,
            ),
            // 68 bytes.
            (
                Some("hot-reload"),
                EditMode::LiveSymlink,
                "hot-reload \u{2192} live-symlink",
                68,
            ),
            // Back to the default, 67 bytes, and the one whose write is
            // a NULL rather than the word it prints.
            (
                Some("live-symlink"),
                EditMode::Immutable,
                "live-symlink \u{2192} immutable",
                67,
            ),
        ];
        for (from, to, transition, bytes) in cases {
            let line = mark_line("templedb-rust", ".gitignore", from, to);
            assert_eq!(
                line,
                format!("\u{2713} templedb-rust/.gitignore: edit_mode {transition}")
            );
            // `len()` is bytes, and the `println!` newline is the one
            // Python's `print` adds.
            assert_eq!(line.len() + 1, bytes, "{line}");
        }
    }

    /// The arrow is U+2192 and the tick is U+2713, and neither is an
    /// ASCII lookalike.
    ///
    /// Worth its own test because `->` and `->` read identically in a
    /// diff of this file at a glance, and the only thing that would
    /// catch the substitution is a byte count four screens up.
    #[test]
    fn the_two_multibyte_characters_are_the_measured_codepoints() {
        let line = mark_line("p", "f", None, EditMode::HotReload);
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(chars[0], '\u{2713}');
        assert!(line.contains('\u{2192}'), "{line}");
        assert!(!line.contains("->"), "{line}");
        // U+2713 plus U+2192, three bytes each, over a line that is
        // otherwise ASCII.
        assert_eq!(line.len(), line.chars().count() + 4);
    }

    /// The line names the project the caller typed, where `file rm`'s
    /// names no project at all.
    ///
    /// The pair is the finding: these two commands are forty lines apart
    /// in the same Python file and do not agree, so a port that shared a
    /// renderer between them would be wrong on one.
    #[test]
    fn the_line_carries_a_project_prefix_where_file_rm_carries_none() {
        let line = mark_line("templedb-rust", "crates/x.rs", None, EditMode::LiveSymlink);
        assert!(
            line.starts_with("\u{2713} templedb-rust/crates/x.rs:"),
            "{line}"
        );
        assert!(!crate::rm_line("crates/x.rs").contains("templedb-rust"));
    }

    /// `immutable` is the only mode whose printed word and stored value
    /// differ.
    ///
    /// The asymmetry `file ls --edit-mode immutable` already depends on:
    /// the filter compares the raw column, so a row this command marked
    /// `immutable` is not matched by the filter of the same name.
    #[test]
    fn immutable_prints_a_word_and_stores_a_null() {
        assert_eq!(EditMode::Immutable.as_str(), "immutable");
        assert_eq!(EditMode::Immutable.column(), None);
        for mode in [EditMode::HotReload, EditMode::LiveSymlink] {
            assert_eq!(mode.column(), Some(mode.as_str()));
        }
    }

    /// The `from` side is the raw column, with one special case that is
    /// Python's `or` rather than a null check.
    ///
    /// An unrecognised value prints verbatim — the column has no CHECK
    /// constraint, so the live table can hold one and this port has no
    /// business tidying it. An **empty** string prints as `immutable`,
    /// because `'' or 'immutable'` is `'immutable'` in Python. Neither
    /// case is reachable from this port's own writes, since
    /// [`EditMode::column`] yields NULL or one of two non-empty
    /// strings; both are reachable from the table.
    #[test]
    fn the_from_side_is_the_raw_column_except_where_pythons_or_is_falsy() {
        let odd = mark_line("p", "f", Some("whatever"), EditMode::Immutable);
        assert!(
            odd.contains("edit_mode whatever \u{2192} immutable"),
            "{odd}"
        );
        let empty = mark_line("p", "f", Some(""), EditMode::HotReload);
        assert_eq!(
            empty,
            mark_line("p", "f", None, EditMode::HotReload),
            "`'' or 'immutable'` is `'immutable'`, so NULL and the empty string \
             print the same line"
        );
    }
}
