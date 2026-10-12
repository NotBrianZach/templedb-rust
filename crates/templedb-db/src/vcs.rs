//! `templedb vcs ...` reads.
//!
//! # Why only part of `vcs log` is here
//!
//! The Python command has three visibility modes and its default is the
//! one this port cannot reproduce. `--published` shows commits with
//! `session_id IS NULL` — the shared branch view; `--all-sessions`
//! shows everything; and the default shows the published set *plus the
//! commits owned by your current session*.
//!
//! That last one resolves the session through
//! `VCSService.get_current_session()`, which is six fallbacks deep —
//! a `--session` override, then `TEMPLEDB_SESSION_ID`, then an
//! instance cache, then `TEMPLEDB_SESSION`, then a lookup keyed on the
//! session-leader PID within the last 24 hours, then one keyed on the
//! PPID — and if none of them hit, it **creates a session row**. So the
//! default view is both environment-dependent and a write path. Neither
//! belongs in this crate.
//!
//! So [`commit_log`] takes the visibility explicitly and the CLI
//! defaults to [`Visibility::Published`], which is the reproducible
//! shared view. The command prints which view it used, exactly as the
//! Python one does, so the output says what it is rather than leaving
//! the reader to assume it matches a `templedb vcs log` run.

use crate::like::escape_like;
use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};

/// Which commits a log should include.
///
/// Python's default — published plus the caller's own session — is
/// absent on purpose; see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    /// `session_id IS NULL`: the shared branch view.
    Published,
    /// No session filter at all.
    All,
}

impl Visibility {
    /// The label the Python command prints in its header. Reproduced
    /// character for character because it is on stdout.
    pub fn label(self) -> &'static str {
        match self {
            Visibility::Published => "published only",
            Visibility::All => "all commits (no session filter)",
        }
    }
}

/// One line of `vcs log`.
#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub commit_hash: String,
    pub branch_name: String,
    pub author: String,
    pub commit_timestamp: String,
    pub commit_message: String,
    /// `None` for a published commit. The Python command appends
    /// `(unpublished)` to the hash when this is set, which is the only
    /// thing it uses the column for.
    pub session_id: Option<i64>,
}

/// Commit history for one project, newest first.
///
/// # What is deliberately not computed
///
/// The Python query selects a `files_changed` column:
///
/// ```sql
/// COALESCE((SELECT COUNT(*) FROM vcs_file_states WHERE commit_id = c.id),
///          (SELECT COUNT(*) FROM commit_files    WHERE commit_id = c.id))
/// ```
///
/// It is omitted here for two independent reasons, and both are worth
/// recording because the expression looks load-bearing.
///
/// First, **nothing reads it.** It is not printed by the human
/// formatter and not placed in the dict the JSON formatter emits. It is
/// a correlated subquery evaluated once per returned commit whose
/// result is discarded.
///
/// Second, **the `COALESCE` fallback is unreachable.** `COUNT(*)` over
/// an empty set returns `0`, not `NULL`, so the first branch always
/// wins and the `commit_files` lookup never runs. The intent was
/// evidently "use the newer table, fall back to the legacy one", and
/// what it does is "use the newer table, and report 0 when it has no
/// rows". 354 commits in the live database have no `vcs_file_states`
/// rows at all; none of them currently has `commit_files` rows either,
/// so the mistake is latent rather than visible — but `commit_files`
/// holds 6,146 rows, so it is a live table being silently ignored.
///
/// Reproducing a discarded value would mean mirroring `commit_files`
/// for nothing. If a caller ever needs the count, it should be written
/// as a real fallback and tested against the 354 commits that exercise
/// it.
///
/// # Ordering
///
/// `(commit_timestamp DESC, id ASC)`. Python orders on the timestamp
/// alone and the timestamps tie — 1,196 commits across 1,186 distinct
/// values, with three commits sharing `2026-02-23 19:00:24` — so with
/// `-n` the planner decides which of a tied group you see.
///
/// The tie-break is `ASC`, and getting here took being wrong first.
/// `DESC` looks right by symmetry with the column above it: the log is
/// newest-first, so a tie should be too. The argument fails on two
/// counts.
///
/// The first is that it buys nothing. The defect in Python's ordering is
/// that it is *unspecified* — a tie is left to the planner. `ASC` and
/// `DESC` are equally total, so both fix it completely, and between two
/// fixes the one that also agrees byte for byte with the implementation
/// being replaced is strictly better. The divergences this port does
/// carry (`entity search`, `entity trace`) exist because there was no
/// ordering that was both stable and faithful; here there is.
///
/// The second is that the data disagrees. `slimevr_fitness_arena` has
/// two commits at `2026-05-30 19:54:32`: id 80 is "Initial commit:
/// SlimeVR Fitness Arena project" and id 81 is "Scaffold source code".
/// `id` is monotonic, so ascending shows them in the order they
/// happened and descending shows a project being scaffolded before it
/// was created. Within a tie the rows are a sequence, not a set — the
/// same reason `entity observations` sorts ascending, arrived at from
/// the opposite direction.
pub fn commit_log(
    store: &dyn Store,
    project_id: i64,
    branch: Option<&str>,
    visibility: Visibility,
    limit: i64,
) -> Result<Vec<CommitEntry>> {
    // `JOIN vcs_branches`, not `LEFT JOIN`: Python's is an inner join,
    // so a commit whose `branch_id` does not resolve is dropped rather
    // than listed with an empty branch. `branch_id` is NOT NULL and
    // declares the foreign key, so this should be unreachable —
    // reproduced rather than improved because the two are diffed.
    let visibility_clause = match visibility {
        Visibility::Published => " AND c.session_id IS NULL",
        Visibility::All => "",
    };
    // `CAST(?2 AS TEXT) IS NULL` rather than `?2 IS NULL`: Postgres
    // infers a parameter's type from its use and `IS NULL` tells it
    // nothing. Same fix as `search.rs` and `dead_imports`.
    let sql = format!(
        "SELECT c.commit_hash, b.branch_name, c.author,
                c.commit_timestamp, c.commit_message, c.session_id
           FROM vcs_commits c
           JOIN vcs_branches b ON b.id = c.branch_id
          WHERE c.project_id = ?1
            AND (CAST(?2 AS TEXT) IS NULL OR b.branch_name = ?2){visibility_clause}
          ORDER BY c.commit_timestamp DESC, c.id ASC
          LIMIT ?3"
    );

    let rows = store
        .query(
            &sql,
            &[
                Value::from(project_id),
                Value::from(branch),
                Value::from(limit),
            ],
        )
        .context("reading commit history")?;

    rows.iter()
        .map(|r| {
            Ok(CommitEntry {
                commit_hash: r.get_string(0)?,
                branch_name: r.get_string(1)?,
                author: r.get_string(2)?,
                commit_timestamp: r.get_string(3)?,
                commit_message: r.get_string(4)?,
                session_id: r.get_opt_i64(5)?,
            })
        })
        .collect()
}

/// One commit's metadata, as `vcs show` heads its output with.
#[derive(Debug, Clone)]
pub struct CommitDetail {
    /// Needed by [`parent_hash`], which keys on the id rather than the
    /// hash because `vcs_commit_parents` stores ids.
    pub id: i64,
    pub commit_hash: String,
    pub branch_name: String,
    pub author: String,
    pub commit_timestamp: String,
    pub commit_message: String,
}

/// One row of `vcs show`'s changed-file list.
#[derive(Debug, Clone)]
pub struct FileChange {
    pub file_path: String,
    pub change_type: String,
    pub file_size: Option<i64>,
    pub line_count: Option<i64>,
}

impl FileChange {
    /// The parenthesised size the Python command prints.
    ///
    /// Reproduces a Python truthiness check rather than a null check:
    /// `f"({line_count} lines)" if fs['line_count'] else f"({file_size}
    /// bytes)"`. `0` is falsy in Python, so a file recorded with zero
    /// lines reports its *byte* count, not "0 lines" — and a deletion,
    /// which has NULL for both, reports "(None bytes)". Both are
    /// reproduced because this is on stdout.
    pub fn size_info(&self) -> String {
        match self.line_count {
            Some(n) if n != 0 => format!("({n} lines)"),
            _ => match self.file_size {
                Some(b) => format!("({b} bytes)"),
                // Python interpolates `None` into the f-string.
                None => "(None bytes)".to_string(),
            },
        }
    }

    /// The marker Python prefixes each line with.
    pub fn marker(&self) -> &'static str {
        match self.change_type.as_str() {
            "added" => "✨",
            "modified" => "📝",
            "deleted" => "🗑️",
            "renamed" => "📋",
            _ => "❓",
        }
    }
}

/// Resolve a commit by hash or hash prefix, with its changed files.
///
/// # The prefix is escaped, and that is a fix
///
/// Python looks the commit up with
/// `c.commit_hash = ? OR c.commit_hash LIKE ?`, where the second
/// parameter is `f"{args.commit}%"` — built by interpolation, with no
/// escaping and no `ESCAPE` clause. So a `%` or `_` in the argument is
/// a wildcard, and this one needs no coincidence in the data to reach:
///
/// ```text
/// $ templedb vcs show templedb '%' --no-diff
/// commit E067215FD8DAA5D38CA956734405C574594D3E8F7DB5EDF914D80EDBB6400F83
/// ```
///
/// That is an arbitrary commit presented as the one you asked for, and
/// `_` does the same. It is the fifth instance of this mistake in the
/// codebase and the first reachable by typing one character; the others
/// needed either an underscore in real data or a specific pair of
/// slugs. Escaped here, so `%` matches nothing and the command says the
/// commit was not found.
///
/// # And the lookup is ordered
///
/// Python's query has no `ORDER BY` and takes the first row, so among
/// several commits sharing a prefix the answer is the planner's choice.
/// Ordering by `commit_hash` makes an ambiguous prefix resolve the same
/// way on every run. It does not *report* the ambiguity — git would
/// refuse the prefix outright — because that would change the output
/// shape rather than make it deterministic, and determinism is what was
/// missing.
pub fn commit_detail(
    store: &dyn Store,
    project_id: i64,
    commit: &str,
) -> Result<Option<(CommitDetail, Vec<FileChange>)>> {
    let row = store
        .query_opt(
            "SELECT c.id, c.commit_hash, b.branch_name, c.author,
                    c.commit_timestamp, c.commit_message
               FROM vcs_commits c
               JOIN vcs_branches b ON b.id = c.branch_id
              WHERE c.project_id = ?1
                AND (c.commit_hash = ?2
                     OR c.commit_hash LIKE ?3 || '%' ESCAPE '\\')
              ORDER BY c.commit_hash
              LIMIT 1",
            &[
                Value::from(project_id),
                Value::from(commit),
                Value::from(escape_like(commit)),
            ],
        )
        .context("resolving commit")?;

    let Some(row) = row else {
        return Ok(None);
    };
    let commit_id = row.get_i64(0)?;
    let detail = CommitDetail {
        id: commit_id,
        commit_hash: row.get_string(1)?,
        branch_name: row.get_string(2)?,
        author: row.get_string(3)?,
        commit_timestamp: row.get_string(4)?,
        commit_message: row.get_string(5)?,
    };

    // `JOIN project_files`, inner, as Python has it: a file state whose
    // `file_id` no longer resolves is dropped rather than listed with an
    // empty path. `vcs_file_states.file_id` has no foreign key — the
    // row outlives the file — so this is reachable, not theoretical.
    let files = store
        .query(
            "SELECT pf.file_path, vfs.change_type, vfs.file_size, vfs.line_count
               FROM vcs_file_states vfs
               JOIN project_files pf ON pf.id = vfs.file_id
              WHERE vfs.commit_id = ?1
              ORDER BY pf.file_path",
            &[Value::from(commit_id)],
        )
        .context("reading changed files")?;

    let files = files
        .iter()
        .map(|r| {
            Ok(FileChange {
                file_path: r.get_string(0)?,
                change_type: r.get_string(1)?,
                file_size: r.get_opt_i64(2)?,
                line_count: r.get_opt_i64(3)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(Some((detail, files)))
}

/// The hash of a commit's first parent, or `None` for a root commit.
///
/// `vcs show` diffs a commit against this, which is `git show`'s
/// semantics. A root commit has no parent and is diffed against the
/// empty string, so every line reads as an addition.
///
/// `ORDER BY parent_order LIMIT 1` is Python's, and "first parent" is
/// doing real work in principle: `vcs_commit_parents` is a join table
/// built for merge commits, so a merge has two rows and only the first
/// is diffed against. In the live database there are no merges yet —
/// all 718 rows have `parent_order = 0`, which migration 123 records as
/// a known-constant column — so the ordering is currently untested by
/// the data and is reproduced rather than relied on.
///
/// The `id` tie-break is this port's. `parent_order` is not declared
/// unique, and a merge commit with two parents sharing an order would
/// otherwise leave the choice to the planner — the same defect as
/// `vcs log`'s timestamps, and reachable the moment merges start being
/// recorded.
pub fn parent_hash(store: &dyn Store, commit_id: i64) -> Result<Option<String>> {
    let row = store
        .query_opt(
            "SELECT c.commit_hash
               FROM vcs_commit_parents p
               JOIN vcs_commits c ON c.id = p.parent_commit_id
              WHERE p.commit_id = ?1
              ORDER BY p.parent_order, c.id
              LIMIT 1",
            &[Value::from(commit_id)],
        )
        .context("resolving the parent commit")?;
    row.map(|r| r.get_string(0)).transpose()
}

/// A file's text at one commit, or `None` when there is no text to show.
///
/// `None` covers six distinct situations, and Python collapses all of
/// them to the empty string at the call site — an unresolvable commit, a
/// path that is not in `project_files`, a file the commit does not
/// touch, a NULL `content_hash`, a `content_blobs` row that is not
/// there, and a blob that is not text. They are collapsed here too,
/// because reproducing the stdout means reproducing that: a binary file
/// in a commit diffs as empty-against-empty and prints
/// `No differences`, which is wrong but is what the command being
/// replaced says.
///
/// A deletion returns `Some("")` rather than `None`, which is Python's
/// behaviour and is the one case where the empty string is meant: the
/// file's content at that commit genuinely is nothing, so the diff
/// correctly shows every line being removed.
///
/// # The commit reference is escaped
///
/// Python resolves it with `commit_hash = ? OR commit_hash LIKE ?`,
/// building the second parameter as `f"{commit_ref}%"` with no escaping
/// — the same unescaped-LIKE mistake as [`commit_detail`], in a second
/// place. It is unreachable from `vcs show`, which passes full hashes it
/// read out of the database, and reachable from `vcs diff`, which passes
/// what the caller typed. Escaped here so it is not reachable from
/// either.
pub fn content_at_commit(
    store: &dyn Store,
    project_id: i64,
    file_path: &str,
    commit_ref: &str,
) -> Result<Option<String>> {
    // `change_type` and `content_hash` in one go, with the file
    // resolved by path. Python issues three queries to get here; the
    // joins are the same ones, and all three are inner, so a missing
    // row at any level drops out as no row at all.
    let state = store
        .query_opt(
            "SELECT vfs.change_type, vfs.content_hash
               FROM vcs_commits c
               JOIN vcs_file_states vfs ON vfs.commit_id = c.id
               JOIN project_files pf ON pf.id = vfs.file_id
              WHERE c.project_id = ?1
                AND pf.project_id = ?1
                AND pf.file_path = ?2
                AND (c.commit_hash = ?3
                     OR c.commit_hash LIKE ?4 || '%' ESCAPE '\\')
              ORDER BY c.commit_hash
              LIMIT 1",
            &[
                Value::from(project_id),
                Value::from(file_path),
                Value::from(commit_ref),
                Value::from(escape_like(commit_ref)),
            ],
        )
        .context("resolving the file state at a commit")?;

    let Some(state) = state else {
        return Ok(None);
    };
    if state.get_string(0)? == "deleted" {
        return Ok(Some(String::new()));
    }
    let Some(hash) = state.get_opt_string(1)? else {
        return Ok(None);
    };

    let blob = store
        .query_opt(
            "SELECT content_text, content_type FROM content_blobs
              WHERE hash_sha256 = ?1",
            &[Value::from(hash)],
        )
        .context("reading a content blob")?;
    let Some(blob) = blob else {
        return Ok(None);
    };
    // Python tests `content_type != 'text'` and returns no content for
    // anything else, so a NULL type is also "not text".
    if blob.get_opt_string(1)?.as_deref() != Some("text") {
        return Ok(None);
    }
    blob.get_opt_string(0)
}

/// One row of `vcs branch`'s listing.
#[derive(Debug, Clone)]
pub struct BranchEntry {
    pub branch_name: String,
    pub is_default: bool,
    /// Whether this is the project's active branch.
    pub is_active: bool,
    pub commit_count: i64,
    /// The head commit's message, if the branch has a head and that
    /// commit still exists.
    pub last_message: Option<String>,
}

/// Branches for one project, as `vcs branch` lists them.
///
/// Only the listing path. Creating and deleting branches are writes and
/// stay with the rest of the write paths.
///
/// # Resolving the active branch
///
/// Python tries `projects.active_branch_id` first and falls back to
/// whichever branch has `is_default = 1`. Both are reproduced,
/// including the fallback's `LIMIT 1` — nothing stops two branches of
/// one project from both being flagged default, and in that case Python
/// takes the planner's pick. The fallback here orders by `id` so the
/// pick is at least the same on every run.
///
/// # Ordering
///
/// `ORDER BY is_default DESC, branch_name`, as Python has it, and this
/// time no tie-break is needed: `UNIQUE(project_id, branch_name)` makes
/// the second key total on its own.
pub fn branches(store: &dyn Store, project_id: i64) -> Result<Vec<BranchEntry>> {
    // `active_branch_id` first, then the default-branch fallback.
    let active = store
        .query_opt(
            "SELECT vb.id
               FROM projects p
               JOIN vcs_branches vb ON vb.id = p.active_branch_id
              WHERE p.id = ?1",
            &[Value::from(project_id)],
        )
        .context("resolving the active branch")?;
    let active_id = match active {
        Some(r) => Some(r.get_i64(0)?),
        None => store
            .query_opt(
                "SELECT id FROM vcs_branches
                  WHERE project_id = ?1 AND is_default = 1
                  ORDER BY id LIMIT 1",
                &[Value::from(project_id)],
            )
            .context("resolving the default branch")?
            .map(|r| r.get_i64(0))
            .transpose()?,
    };

    let rows = store
        .query(
            "SELECT vb.id, vb.branch_name, vb.is_default,
                    (SELECT COUNT(*) FROM vcs_commits WHERE branch_id = vb.id),
                    (SELECT commit_message FROM vcs_commits WHERE id = vb.head_commit_id)
               FROM vcs_branches vb
              WHERE vb.project_id = ?1
              ORDER BY vb.is_default DESC, vb.branch_name",
            &[Value::from(project_id)],
        )
        .context("listing branches")?;

    rows.iter()
        .map(|r| {
            let id = r.get_i64(0)?;
            Ok(BranchEntry {
                branch_name: r.get_string(1)?,
                // `is_default` is declared BOOLEAN in SQLite, which
                // stores 1/0, and `integer` in the mirror. The backend
                // narrows a Postgres `bool` to `Int` too, so reading it
                // as an integer works on both. NULL counts as false, as
                // Python's truthiness does.
                is_default: r.get_opt_i64(2)?.unwrap_or(0) != 0,
                is_active: Some(id) == active_id,
                commit_count: r.get_i64(3)?,
                last_message: r.get_opt_string(4)?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    fn hashes(entries: &[CommitEntry]) -> Vec<&str> {
        entries.iter().map(|e| e.commit_hash.as_str()).collect()
    }

    /// Newest first, and the published view hides session-owned
    /// commits. `C3` is unpublished.
    #[test]
    fn published_view_hides_session_commits() {
        let c = testdb::fixture();
        assert_eq!(
            hashes(&commit_log(&c, 1, None, Visibility::Published, 10).unwrap()),
            ["C4", "C2", "C1", "ABCD1234EFFF"]
        );
    }

    #[test]
    fn all_view_includes_them() {
        let c = testdb::fixture();
        let all = commit_log(&c, 1, None, Visibility::All, 10).unwrap();
        assert_eq!(hashes(&all), ["C4", "C2", "C3", "C1", "ABCD1234EFFF"]);
        // And the session id is carried, because the caller marks those
        // lines `(unpublished)`.
        let unpublished = all.iter().find(|e| e.commit_hash == "C3").unwrap();
        assert_eq!(unpublished.session_id, Some(7));
        assert_eq!(all[0].session_id, None);
    }

    /// `C2` and `C3` share a timestamp. `id ASC` shows them in the
    /// order they were committed, which is what Python does and what
    /// reads correctly — a tie is a sequence, not a set.
    #[test]
    fn ties_break_on_id_descending() {
        let c = testdb::fixture();
        let all = commit_log(&c, 1, None, Visibility::All, 10).unwrap();
        let tied: Vec<&str> = all
            .iter()
            .filter(|e| e.commit_timestamp == "2026-02-02 00:00:00")
            .map(|e| e.commit_hash.as_str())
            .collect();
        assert_eq!(tied, ["C2", "C3"]);
    }

    /// The tie-break is what makes `-n` mean something: with two
    /// commits sharing a timestamp at the cutoff, which one survives
    /// has to be the same every run.
    #[test]
    fn limit_cuts_a_tie_deterministically() {
        let c = testdb::fixture();
        assert_eq!(
            hashes(&commit_log(&c, 1, None, Visibility::All, 2).unwrap()),
            ["C4", "C2"]
        );
    }

    #[test]
    fn branch_filter_is_exact_not_a_pattern() {
        let c = testdb::fixture();
        assert_eq!(
            hashes(&commit_log(&c, 1, Some("main"), Visibility::All, 10).unwrap()),
            ["C4", "C2", "C1", "ABCD1234EFFF"]
        );
        assert_eq!(
            hashes(&commit_log(&c, 1, Some("feature"), Visibility::All, 10).unwrap()),
            ["C3"]
        );
        // A branch name is compared with `=`, so a LIKE metacharacter
        // in it is literal and matches nothing.
        assert!(commit_log(&c, 1, Some("mai_"), Visibility::All, 10)
            .unwrap()
            .is_empty());
    }

    /// Another project's commits must not appear, and a project with
    /// none answers empty rather than erroring. Project 2 carries the
    /// three commits the `source` fixtures use plus its own `B1`, which
    /// is the newest of them.
    #[test]
    fn the_log_is_scoped_to_one_project() {
        let c = testdb::fixture();
        assert_eq!(
            hashes(&commit_log(&c, 2, None, Visibility::All, 10).unwrap()),
            ["B1", "DEAD0002", "CAFE0001"]
        );
        // Project 1's commits are absent from that list, and project 3
        // has none at all.
        assert!(commit_log(&c, 3, None, Visibility::All, 10)
            .unwrap()
            .is_empty());
    }

    /// A commit whose branch does not resolve is dropped, because
    /// Python's join is inner. `C9` points at a branch id that is not
    /// there.
    #[test]
    fn a_commit_with_an_unresolvable_branch_is_dropped() {
        let c = testdb::fixture();
        let all = commit_log(&c, 1, None, Visibility::All, 50).unwrap();
        assert!(
            !hashes(&all).contains(&"C9"),
            "an inner join should have dropped C9: {:?}",
            hashes(&all)
        );
    }

    #[test]
    fn labels_match_the_python_header() {
        assert_eq!(Visibility::Published.label(), "published only");
        assert_eq!(Visibility::All.label(), "all commits (no session filter)");
    }

    #[test]
    fn commit_detail_resolves_an_exact_hash_with_its_files() {
        let c = testdb::fixture();
        let (d, files) = commit_detail(&c, 1, "ABCD1234EFFF").unwrap().unwrap();
        assert_eq!(d.commit_hash, "ABCD1234EFFF");
        assert_eq!(d.branch_name, "main");
        assert_eq!(d.author, "zach");
        assert_eq!(d.commit_message, "first");
        // Commit 1 touches alpha's src/main.rs.
        assert_eq!(
            files
                .iter()
                .map(|f| f.file_path.as_str())
                .collect::<Vec<_>>(),
            ["src/main.rs"]
        );
        assert_eq!(files[0].change_type, "modified");
    }

    #[test]
    fn a_hash_prefix_resolves() {
        let c = testdb::fixture();
        let (d, _) = commit_detail(&c, 1, "ABCD").unwrap().unwrap();
        assert_eq!(d.commit_hash, "ABCD1234EFFF");
    }

    /// The injection Python is open to. `%` and `_` must match
    /// literally, so neither resolves to an arbitrary commit.
    #[test]
    fn a_wildcard_in_the_prefix_is_literal() {
        let c = testdb::fixture();
        assert!(commit_detail(&c, 1, "%").unwrap().is_none());
        assert!(commit_detail(&c, 1, "_").unwrap().is_none());
        // And a partial pattern does not widen either: `ABC_` would
        // match `ABCD...` if the underscore were a wildcard.
        assert!(commit_detail(&c, 1, "ABC_").unwrap().is_none());
    }

    #[test]
    fn an_unknown_commit_is_none_not_an_error() {
        let c = testdb::fixture();
        assert!(commit_detail(&c, 1, "deadbeef").unwrap().is_none());
        // A real hash under the wrong project is also absent — the
        // lookup is scoped.
        assert!(commit_detail(&c, 2, "ABCD1234EFFF").unwrap().is_none());
    }

    /// A commit with no file states lists none rather than erroring.
    #[test]
    fn a_commit_with_no_file_states_has_an_empty_list() {
        let c = testdb::fixture();
        let (_, files) = commit_detail(&c, 1, "C4").unwrap().unwrap();
        assert!(files.is_empty());
    }

    /// Python's size line is a truthiness check, not a null check, so
    /// zero lines reports bytes and a deletion reports `None bytes`.
    #[test]
    fn size_info_reproduces_pythons_truthiness() {
        let mk = |line_count, file_size| FileChange {
            file_path: "x".into(),
            change_type: "modified".into(),
            file_size,
            line_count,
        };
        assert_eq!(mk(Some(12), Some(400)).size_info(), "(12 lines)");
        // 0 is falsy in Python, so it falls through to bytes.
        assert_eq!(mk(Some(0), Some(400)).size_info(), "(400 bytes)");
        assert_eq!(mk(None, Some(400)).size_info(), "(400 bytes)");
        // A deletion has neither; Python interpolates the word `None`.
        assert_eq!(mk(None, None).size_info(), "(None bytes)");
        assert_eq!(mk(Some(0), None).size_info(), "(None bytes)");
    }

    /// `alpha` pins `feature` as active via `active_branch_id` while
    /// `main` is the default, so the two markers land on different rows
    /// and a bug that conflated them would show.
    #[test]
    fn branches_mark_active_and_default_separately() {
        let c = testdb::fixture();
        let bs = branches(&c, 1).unwrap();
        // `is_default DESC` puts main first despite `feature` sorting
        // earlier alphabetically.
        assert_eq!(
            bs.iter()
                .map(|b| b.branch_name.as_str())
                .collect::<Vec<_>>(),
            ["main", "feature"]
        );
        let main = &bs[0];
        assert!(main.is_default);
        assert!(!main.is_active);
        let feature = &bs[1];
        assert!(!feature.is_default);
        assert!(feature.is_active);
    }

    /// With `active_branch_id` unset, the active branch is whichever is
    /// flagged default — Python's fallback.
    #[test]
    fn an_unset_active_branch_falls_back_to_the_default() {
        let c = testdb::fixture();
        let bs = branches(&c, 2).unwrap();
        assert_eq!(bs.len(), 1);
        assert!(bs[0].is_default);
        assert!(bs[0].is_active, "the default should be active for beta");
    }

    #[test]
    fn branches_count_commits_and_carry_the_head_message() {
        let c = testdb::fixture();
        let bs = branches(&c, 1).unwrap();
        let main = &bs[0];
        // main holds commits 1, 11, 12 and 14. The dangling C9 names
        // branch 99, so it is not one of main's — which is also why
        // `vcs log` drops it and `project show` counts 5 across both
        // branches rather than 6.
        assert_eq!(main.commit_count, 4);
        // Its head is commit 14, message "four".
        assert_eq!(main.last_message.as_deref(), Some("four"));
        // feature has one commit and no head, so no message.
        let feature = &bs[1];
        assert_eq!(feature.commit_count, 1);
        assert_eq!(feature.last_message, None);
    }

    #[test]
    fn a_project_with_no_branches_lists_none() {
        let c = testdb::fixture();
        assert!(branches(&c, 3).unwrap().is_empty());
    }

    #[test]
    fn a_root_commit_has_no_parent() {
        let c = testdb::fixture();
        // C1 (id 11) has no row in vcs_commit_parents.
        assert_eq!(parent_hash(&c, 11).unwrap(), None);
        // And neither does a commit id that does not exist.
        assert_eq!(parent_hash(&c, 9999).unwrap(), None);
    }

    #[test]
    fn a_commits_parent_resolves_to_its_hash() {
        let c = testdb::fixture();
        assert_eq!(parent_hash(&c, 12).unwrap().as_deref(), Some("C1"));
        assert_eq!(parent_hash(&c, 14).unwrap().as_deref(), Some("C2"));
    }

    /// A merge commit has one row per parent and only the first is
    /// diffed against. C9's rows are inserted with the higher
    /// `parent_order` first, so a query without the `ORDER BY` could
    /// return either and would likely return the inserted-first one.
    #[test]
    fn a_merge_commit_resolves_to_its_first_parent() {
        let c = testdb::fixture();
        assert_eq!(parent_hash(&c, 19).unwrap().as_deref(), Some("C1"));
    }

    #[test]
    fn content_at_commit_reads_the_text_at_that_revision() {
        let c = testdb::fixture();
        // src/main.rs is one line at C1 and three at C2.
        assert_eq!(
            content_at_commit(&c, 1, "src/main.rs", "C1").unwrap(),
            Some("fn main() {}\n".to_string())
        );
        assert_eq!(
            content_at_commit(&c, 1, "src/main.rs", "C2").unwrap(),
            Some("fn main() {}\nsecond\nthird\n".to_string())
        );
    }

    /// A deletion is `Some("")`, not `None`: the file's content at that
    /// commit genuinely is nothing, so the diff should show every line
    /// being removed rather than showing nothing at all.
    #[test]
    fn a_deleted_file_reads_as_empty_rather_than_absent() {
        let c = testdb::fixture();
        assert_eq!(
            content_at_commit(&c, 2, "removed.txt", "DEAD0002").unwrap(),
            Some(String::new())
        );
    }

    /// The four ways there is no text, all of which Python collapses to
    /// the empty string at the call site.
    #[test]
    fn the_absent_cases_are_all_none() {
        let c = testdb::fixture();
        // A commit that does not resolve.
        assert_eq!(
            content_at_commit(&c, 1, "src/main.rs", "nope").unwrap(),
            None
        );
        // A path that is not in project_files.
        assert_eq!(content_at_commit(&c, 1, "no/such.rs", "C1").unwrap(), None);
        // A file the commit does not touch.
        assert_eq!(content_at_commit(&c, 1, "README.md", "C1").unwrap(), None);
        // A content_hash naming a blob that is not there.
        assert_eq!(
            content_at_commit(&c, 2, "orphan.txt", "CAFE0001").unwrap(),
            None
        );
        // A blob that is not text. This is the one that produces a
        // wrong answer downstream — the diff shows "No differences" for
        // a binary file that did change — and it is Python's answer.
        assert_eq!(content_at_commit(&c, 2, "logo.png", "B1").unwrap(), None);
    }

    /// The lookup is scoped to the project on both sides of the join, so
    /// one project's commit cannot read another's file.
    #[test]
    fn content_at_commit_is_scoped_to_the_project() {
        let c = testdb::fixture();
        // src/main.rs is project 1's, C1 is project 1's commit.
        assert!(content_at_commit(&c, 2, "src/main.rs", "C1")
            .unwrap()
            .is_none());
    }

    /// The same unescaped-LIKE injection as `commit_detail`, in a second
    /// place. `%` must resolve nothing rather than an arbitrary commit.
    #[test]
    fn a_wildcard_commit_ref_is_literal() {
        let c = testdb::fixture();
        assert_eq!(content_at_commit(&c, 1, "src/main.rs", "%").unwrap(), None);
        assert_eq!(content_at_commit(&c, 1, "src/main.rs", "_").unwrap(), None);
        // A prefix still works, so the escape has not broken resolution.
        assert_eq!(
            content_at_commit(&c, 1, "src/main.rs", "C2").unwrap(),
            Some("fn main() {}\nsecond\nthird\n".to_string())
        );
    }

    #[test]
    fn markers_match_the_python_mapping() {
        let mk = |t: &str| FileChange {
            file_path: "x".into(),
            change_type: t.into(),
            file_size: None,
            line_count: None,
        };
        assert_eq!(mk("added").marker(), "✨");
        assert_eq!(mk("modified").marker(), "📝");
        assert_eq!(mk("deleted").marker(), "🗑️");
        assert_eq!(mk("renamed").marker(), "📋");
        // `unmodified` is a legal change_type with no mapping, so it
        // takes the fallback rather than being special-cased.
        assert_eq!(mk("unmodified").marker(), "❓");
        assert_eq!(mk("").marker(), "❓");
    }
}
