//! `file rm` — the sixth write, and the one that gives an existing
//! reader its missing writer.
//!
//! It stages a deletion: one `vcs_working_state` row flipped to
//! `state = 'deleted'` and owned by a session. Nothing is removed. The
//! `project_files` row stays `status = 'active'`, `file cat` still
//! answers and `file ls` still lists it, until
//! [`commit_staged`](crate::commit_staged) hard-deletes the row — which
//! is the one place in the whole system that does.
//!
//! # Why this one, and what it closes
//!
//! `vcs commit-staged` has handled `state = 'deleted'` since step 20
//! and nothing in this port could produce such a row. The branch was
//! reachable only from `Fixture::stage`, whose own doc comment says so:
//! *"still SQL, and still needed, for the states no writer produces: a
//! `deleted` row comes from a workspace scan"*. That sentence is now
//! two-thirds true — the hash-naming-no-blob and file_id-naming-no-file
//! corruptions still need SQL, and the deletion does not.
//!
//! So the deletion loop closes the way the stage did in step 21 and the
//! cancel in step 25: `file set` → `file rm` → `vcs commit-staged` →
//! `file cat`, with no SQL in between, and the hard delete asserted
//! through a reader rather than through a query written for the test.
//!
//! # Zero schema cost
//!
//! No new table, no new column, no new view. `vcs_working_state`
//! arrived in step 19 at 9 of its 11 columns and this writes four of
//! them — `state`, `staged_by_session_id`, `last_modified` and, on the
//! insert arm only, the three keys. Every column was already mirrored,
//! already written by the auto-stage and already read by the commit.
//!
//! # Two divergences, both inherited
//!
//! **No session means a refusal**, where Python creates one. Measured
//! on a copy of the live database 2026-11-18: with neither variable
//! set, `templedb file rm templedb-rust .gitignore` printed its success
//! line, exited 0 and left `vcs_sessions` one row longer — id 1331,
//! minted to own a staged deletion. That is *resolve never, accept
//! always* costing a capability, and it is the same cost
//! `file set --stage` and `vcs commit-staged` already pay. Unlike
//! `file set`, there is no "write it and stage nothing" half to fall
//! back on: this command's entire effect *is* the stage, so there is
//! one outcome and no flag to make it conditional.
//!
//! **The slug is exact where Python's is fuzzy**, which is
//! [`resolve_project_exact`](crate::resolve_project_exact)'s
//! divergence and is argued there.
//!
//! # What is reproduced that a tidier port would have changed
//!
//! - **`content_hash` is left exactly as it was.** Python's
//!   `DO UPDATE SET` names three columns and not this one, so a file
//!   staged `modified` by an earlier `file set` keeps that write's hash
//!   under a `deleted` state. Verified on the copy: the row for
//!   `.gitignore` held `c20683…` before and after. It reads like an
//!   oversight and it is not — the commit records that hash in
//!   `vcs_file_states.content_hash`, so the history of a committed
//!   deletion names the bytes that were deleted instead of the
//!   `DELETED` sentinel. Clearing it would lose that.
//! - **`intent_id` is left alone too**, for a consequence of the
//!   above rather than for its own reason: migration 088's link is
//!   supposed to name the intent whose hash the row holds, and since
//!   the hash does not move the link stays correct. A `DO UPDATE SET
//!   intent_id = NULL` would break the invariant this port fixed in
//!   step 21.
//! - **No `edit_intents` row.** Every `file set` records provenance and
//!   a deletion records none, on either side. Confirmed on the copy:
//!   1,647 intents before and 1,647 after. Reproduced rather than
//!   improved — inventing a `'deleted'` status would put a value in the
//!   column that nothing in either implementation writes, and the
//!   column's CHECK is not mirrored, so this port would be the only
//!   thing that could read it back.
//! - **A soft-deleted file can be staged for deletion.** Python's
//!   `get_file_by_path` returns rows of any status, and its own doc
//!   comment says so. A `status = 'deleted'` row has no content to
//!   remove, but the `project_files` row is still there and the commit
//!   is what removes it, so staging one is not meaningless.
//!
//! # The one place the shared branch lookup is stricter
//!
//! Python has **three** copies of the active-branch lookup now and they
//! are not the same query. `vcs commit`'s joins `vcs_branches`; the
//! auto-stage's does not; and this one does not either —
//! `SELECT active_branch_id AS id FROM projects WHERE id = ? AND
//! active_branch_id IS NOT NULL`, with the join only in the
//! `is_default = 1` fallback below it. So an `active_branch_id` naming
//! no branch row stages against a branch that does not exist, and
//! `vcs commit`, which joins, then finds nothing staged.
//!
//! This calls [`active_or_default_branch`] — `vcs commit`'s form, the
//! one `file set` already shares — which joins and additionally refuses
//! a pointer into *another project's* branch. Two divergences in the
//! stricter direction, both latent on current data: measured
//! 2026-11-18, **no live project has an `active_branch_id` that names a
//! missing or foreign branch**. The sharing is the point rather than a
//! tidy-up, and it is the same argument the auto-stage records: a
//! deletion staged against one branch and a commit reading another is a
//! loop that reports success at both ends and moves nothing.

use anyhow::{bail, Context, Result};

use crate::clock::Clock;
use crate::commit::active_or_default_branch;
use crate::session::SessionId;

/// Python's success line, byte for byte.
///
/// Measured rather than read off the source, by step 31's technique:
/// Python run against a copy of the live database, stdout captured and
/// `cat -A`'d. It is `✓ Staged .gitignore for deletion` plus a newline,
/// 35 bytes for that path — U+2713, one space, the word `Staged`, the
/// path **with no project prefix**, and `for deletion`.
///
/// The missing prefix is the part a renderer written from the source
/// gets right by accident and a renderer written from the *sibling
/// commands* gets wrong: `file set`'s line is `✓ Set <path>`,
/// `intent create`'s first line carries `<slug>/<path>`, and
/// `intent apply`'s carries the bare path. Python's `rm` interpolates
/// `args.file_path` alone, so a caller who typed a fuzzy project name
/// cannot tell from stdout which project was staged.
pub fn rm_line(file_path: &str) -> String {
    format!("✓ Staged {file_path} for deletion")
}

/// What a staged deletion did, for the caller and for the tests.
///
/// The caller prints none of it — [`rm_line`] is the whole of stdout,
/// and adding the branch to it would break the byte parity this command
/// has. It exists because the two facts worth asserting about this
/// write are not in its output: *which* branch the row landed on, and
/// whether there was a row there before.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RmOutcome {
    /// The `vcs_branches.branch_name` the staged row belongs to.
    pub branch: String,
    /// The `project_files.id` that was staged.
    pub file_id: i64,
    /// The `state` the row held before, or `None` when this insert
    /// created it. `Some("deleted")` means the command was a no-op,
    /// which Python reports as success too.
    pub previous_state: Option<String>,
}

/// Stage `file_path` for deletion on the project's active branch.
///
/// `session` is a [`SessionId`] rather than an `Option`, so the type
/// carries the requirement: there is no shape of this call that stages
/// a row nobody owns. The refusal lives at the CLI edge instead of
/// here, for the reason `vcs commit-staged`'s does — its message wants
/// [`ignored_name`](crate::ignored_name), which reads the environment,
/// and a query module that reads the environment cannot be tested at a
/// fixed value.
///
/// Both refusals below are plain `bail!`, which is [`USAGE`](crate::USAGE)
/// — exit 1. That is Python's for all four of this command's failures:
/// an unresolved project, a missing file, a project with no branch and
/// a bad `TEMPLEDB_SESSION_ID` all leave `rm()` through a `return 1`,
/// the first three by `logger.error` and the last through the
/// `except Exception` wrapper. Measured on the copy 2026-11-18, all
/// four at 1.
pub fn remove_file(
    tx: &mut postgres::Transaction<'_>,
    clock: &Clock,
    project_id: i64,
    file_path: &str,
    session: SessionId,
) -> Result<RmOutcome> {
    // Any status, which is `get_file_by_path`'s documented behaviour and
    // not an omission here — see the module note on soft-deleted rows.
    let file_id: i64 = match tx
        .query_opt(
            "SELECT id FROM project_files WHERE project_id = $1 AND file_path = $2",
            &[&project_id, &file_path],
        )
        .context("looking up the file to stage")?
    {
        Some(row) => row.get(0),
        None => bail!(
            "no file {file_path:?} in this project.\n\
             `file rm` stages a row that already exists; it does not create one. \
             List them: templedb-rs file ls <project>"
        ),
    };

    let Some(branch) = active_or_default_branch(tx, project_id)? else {
        bail!(
            "this project has no active branch and no default branch, so there \
             is nowhere to stage a deletion.\n\
             Python's message here is `No active branch found for project \
             '<name>'` and its status is the same 1."
        )
    };

    // Python's `datetime('now')` becomes the injected clock, for the
    // reason step 13 came before any write: a timestamp a test can
    // assert instead of a column a state diff has to exclude.
    let now = clock.now_timestamp();
    let previous: Option<String> = tx
        .query_opt(
            "SELECT state FROM vcs_working_state
              WHERE project_id = $1 AND branch_id = $2 AND file_id = $3",
            &[&project_id, &branch.id, &file_id],
        )
        .context("reading the state this deletion replaces")?
        .map(|row| row.get(0));

    // Python's statement, column for column — including the two it
    // leaves out of `DO UPDATE SET`. See the module note: `content_hash`
    // and `intent_id` surviving is what makes a committed deletion name
    // the bytes it removed.
    tx.execute(
        "INSERT INTO vcs_working_state
             (project_id, branch_id, file_id, state, staged_by_session_id,
              last_modified)
         VALUES ($1, $2, $3, 'deleted', $4, $5)
         ON CONFLICT (project_id, branch_id, file_id) DO UPDATE SET
             state                = 'deleted',
             staged_by_session_id = excluded.staged_by_session_id,
             last_modified        = excluded.last_modified",
        &[&project_id, &branch.id, &file_id, &session.get(), &now],
    )
    .context("staging the deletion")?;

    Ok(RmOutcome {
        branch: branch.name,
        file_id,
        previous_state: previous,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes Python printed on a copy of the live database,
    /// 2026-11-18, for `file rm templedb-rust .gitignore`.
    ///
    /// Pinned rather than described, which is step 31's rule: the one
    /// thing a renderer written from `print(f"...")` cannot be checked
    /// against is the `f"..."` it was written from.
    #[test]
    fn the_success_line_is_pythons_measured_bytes() {
        assert_eq!(
            rm_line(".gitignore"),
            "\u{2713} Staged .gitignore for deletion"
        );
        // 34 on the wire plus the newline `println!` adds, which is the
        // 35 `wc -c` reported for Python's stdout.
        assert_eq!(rm_line(".gitignore").len() + 1, 35);
    }

    /// No project prefix, where `intent create`'s first line has one.
    /// The two commands are four screens apart in the same Python file
    /// and they do not agree, so a port that shared a renderer would be
    /// wrong on one of them.
    #[test]
    fn the_line_names_the_path_alone_and_not_the_project() {
        let line = rm_line("crates/templedb-db/src/filerm.rs");
        assert!(line.ends_with("src/filerm.rs for deletion"), "{line}");
        assert!(!line.contains("templedb-rust"), "{line}");
    }
}
