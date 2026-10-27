//! `vcs commit-staged` — the database half of Python's `vcs commit`.
//!
//! # Why this is not called `vcs commit`
//!
//! The design document (*vcs commit: the widest door, measured*,
//! 2026-10-24) measured the door at seven tables, nine guards and one
//! trigger, and recommended against porting it: **five of the nine
//! guards reconcile a filesystem against the database**, and the port
//! has no concept of a checkout by design. Porting them means porting
//! the workspace scanner, the checkout-role resolver and the
//! mirror-to-disk step, which is a different project rather than a
//! harder version of this one.
//!
//! So the half that is a database operation is here, under a name that
//! does not promise the other half. That was the document's one open
//! question and it was settled on 2026-10-25 by the `--no-fts`
//! precedent: a flag — or a name — is only harmless when the thing it
//! names is present. `vcs commit` over there means *scan the tree,
//! reconcile it, commit*; this cannot scan, so sharing the name would
//! promise a workflow that is refused rather than merely absent.
//!
//! The name also says what the operation *is*, which is not "a reduced
//! commit": it is what `publish` already performs. Its guard is
//! `COUNT(*) … WHERE staged_by_session_id IS NOT NULL` and it commits
//! without scanning anything.
//!
//! A side effect worth stating: because the name is new, the sweep's
//! flag-surface stage has nothing to compare it against — there is no
//! `templedb vcs commit-staged --help` on the other side. That is the
//! honest outcome. Sharing the name would have reported `--amend`,
//! `--force`, `--strategy` and the rest as divergences on a command
//! that cannot have them.
//!
//! # What it does
//!
//! | ported | refused |
//! |---|---|
//! | the staged-rows write across six tables | workspace scanning — there is no workspace |
//! | the parent edge | guards 4, 5, 6, 8, which all compare against a tree |
//! | the branch-head advance, as a compare-and-swap | `--strategy` and its resolution strategies |
//! | guard 2 (orphan reap), guard 3 (content-hash refusal) | `--amend`, which is a different command's job |
//! | guard 9's *shape*: session resolved at the edge, never created | reading a missing blob off disk |
//!
//! # The trigger, which is the sharpest porting problem
//!
//! `update_branch_head_on_commit` is an `AFTER INSERT` trigger on
//! `vcs_commits` that sets `vcs_branches.head_commit_id = NEW.id`. This
//! port has none of the live database's 61 triggers, so a faithful port
//! of the *code* leaves the branch head frozen forever — the one case
//! where reproducing Python's statements and reproducing Python's
//! behaviour come apart.
//!
//! Doing the `UPDATE` in application code is the obvious fix, and
//! Python's own comment says why that was a trap over there:
//!
//! ```text
//! -- Branch head is auto-advanced by the `update_branch_head_on_commit`
//! -- trigger ... so no manual UPDATE is required here. The previous
//! -- app-level compare-and-swap was buggy: it always fired a
//! -- false-positive HEAD_RACE error because the trigger had already set
//! -- head_commit_id to the new commit id by the time the CAS's
//! -- WHERE head_commit_id IS <old> ran.
//! ```
//!
//! That is a bug *caused by the trigger existing*. An implementation
//! with no trigger can do the compare-and-swap correctly, because
//! nothing has moved the column underneath it — see
//! [`advance_branch_head`]. This is the first place the port is better
//! rather than merely faithful, and it is not a liberty taken: without
//! it the command would not work at all.
//!
//! It does cost something measurable. Every cross-backend comparison so
//! far has compared SQLite against Postgres *after a copy*;
//! `vcs_branches.head_commit_id` is now the first column whose contents
//! are produced by different mechanisms on the two sides.
//!
//! # Verification
//!
//! Cross-read, as `file set`'s is: a write cannot be byte-diffed
//! against Python, because Python writes SQLite and this writes its own
//! Postgres, so there is no shared input to diff. What *is* available is
//! reading the result back through the read layer the sweep already
//! verifies byte-for-byte — `vcs log`, `vcs show`, `file cat`. A commit
//! those three cannot see is a commit that did not happen.

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::pgtext;
use crate::session::SessionId;

/// The sentinel `vcs_file_states.content_hash` carries for a state that
/// names no content.
///
/// Only honest for an actual deletion, which is what guard 3 is about —
/// see [`check_committable`].
const DELETED: &str = "DELETED";

/// What a caller may vary about a commit.
///
/// `session` is not an `Option`: this command cannot run without one.
/// Python resolves through seven steps and two of them insert a row, so
/// the port accepts an explicit id and declines to guess — and a commit
/// with no session would have nothing to select. See `session.rs`.
#[derive(Debug, Clone, Copy)]
pub struct CommitOptions<'a> {
    pub message: &'a str,
    /// `vcs_commits.author`, NOT NULL and printed by `vcs log`. Resolved
    /// at the entry point, as the clock and the session are.
    pub author: &'a str,
    /// Branch by exact name. `None` takes the project's active branch,
    /// then its default — Python's fallback, in Python's order.
    pub branch: Option<&'a str>,
    /// Whose staged rows to commit. Nobody else's: the isolation is the
    /// point of the column.
    pub session: SessionId,
}

/// What a commit did, for the caller to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitOutcome {
    /// `sha256(…)[:16].upper()`, the native id `vcs log` displays.
    pub hash: String,
    pub commit_id: i64,
    pub branch_name: String,
    /// `vcs_file_states` rows written — Python's `Files:` line.
    pub files: usize,
    /// How many of those were deletions, which are the rows that
    /// hard-delete a `project_files` row.
    pub deletions: usize,
    /// Staged in *other* sessions and therefore not included. Python
    /// prints this and so should anything built on it: it is the
    /// footgun where an agent stages in one shell and commits in
    /// another.
    pub other_sessions_staged: i64,
    /// `vcs_working_state` rows reaped by guard 2.
    pub reaped: u64,
    /// The head this commit was appended to, `None` for a root commit.
    pub previous_head: Option<i64>,
}

/// A branch, resolved.
///
/// `pub(crate)` for `file set`'s auto-stage, which resolves the branch
/// through [`active_or_default_branch`] below.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Branch {
    pub(crate) id: i64,
    pub(crate) name: String,
    pub(crate) head_commit_id: Option<i64>,
}

/// One staged row, joined to what the commit needs to record it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Staged {
    file_id: i64,
    /// From `project_files`, by LEFT JOIN — `None` when the row is gone,
    /// which guard 2 should already have reaped.
    file_path: Option<String>,
    state: String,
    /// `vcs_working_state.content_hash`: the hash of the file as the
    /// workspace scan last saw it. This is what gets committed, and the
    /// hash in `file_contents` is not.
    ws_hash: Option<String>,
    /// `file_contents.file_size_bytes` for the current revision, which
    /// is the fallback size for a state that names no new content.
    current_size: Option<i64>,
    current_lines: Option<i64>,
}

/// The first twelve *characters* of a hash, for a message.
///
/// By character and not by byte, which is the difference between a
/// truncation and a panic. Every surface that shows a hash truncates it
/// — `file where`, `source snapshot --meta`, doctor output — so the
/// length is the convention; the char-boundary care is because
/// `vcs_working_state.content_hash` is a `text` column with no CHECK on
/// either side, so the value a staged row carries is whatever wrote it.
/// A writer that panicked while explaining why it was refusing would be
/// a poor sort of guard.
fn short(hash: &str) -> String {
    hash.chars().take(12).collect()
}

/// Python's float rendering, which is an input to the commit id.
///
/// `f"{…}:{time.time()}"` interpolates through `str()`, and `str` of a
/// float is `repr`: the shortest decimal that round-trips, with one
/// extra rule Rust does not share — a float with no fractional part
/// still shows `.0`. `str(1761393045.0)` is `'1761393045.0'` and Rust's
/// `{}` prints `1761393045`.
///
/// That case is not exotic here, it is the common one under a pinned
/// clock: [`Clock::at`] parses a whole-second timestamp, so every test
/// and every `TEMPLEDB_RS_NOW` run lands exactly on it. Without this the
/// port and Python would compute different ids for the same stated
/// instant, which is the one thing about this hash that *is* checkable.
///
/// The exponent forms Python switches to (`1e+16` and above, `1e-05` and
/// below) are not reproduced and cannot be reached: the argument is
/// seconds since 1970.
fn python_float(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('.') || s.contains('e') || s.contains("inf") || s.contains("NaN") {
        s
    } else {
        format!("{s}.0")
    }
}

/// The commit id: `sha256(f"{slug}:{branch}:{message}:{time}")[:16]`,
/// uppercased.
///
/// Sixteen hex characters of a sha256 over four fields, and **not**
/// content-addressed: the same tree committed twice gives different ids,
/// and the clock is in the input precisely so that it does. That is why
/// `--amend` is safe over there and why this is a native id rather than
/// a git SHA — migration 126 separated the two namespaces after
/// git-history import had been writing real 40-character SHAs into the
/// same column.
///
/// Uppercase is load-bearing rather than cosmetic. `_amend_message`
/// refuses to touch a commit whose hash is 40 characters *or lowercase*,
/// on the grounds that either shape means the row might be published
/// history — so a port that minted lowercase ids would produce commits
/// Python refuses to amend.
pub fn commit_hash(slug: &str, branch: &str, message: &str, unix_seconds: f64) -> String {
    let input = format!("{slug}:{branch}:{message}:{}", python_float(unix_seconds));
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    // Hex by hand for the reason `fileset::content_hash` gives: sha2
    // 0.11 returns a `hybrid_array::Array`, which has no `LowerHex`.
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    hex[..16].to_ascii_uppercase()
}

/// Guard 3: refuse a staged non-deleted file with no content hash.
///
/// Such a row can only record the [`DELETED`] sentinel, which says *this
/// state names no content* — true for a deletion and a lie for anything
/// else. The lie is unrecoverable, because no blob was ever stored under
/// the hash that is missing: 124 rows in the live database were written
/// this way before the guard existed (83 added, 33 modified, 8
/// unmodified), and they feed 501 `source_snapshots` rows pointing at a
/// fabricated blob that claims 8,059 bytes and holds nothing.
///
/// Refusing is the right failure because the caller has a remedy —
/// `vcs status --refresh` computes the hash — so taking it costs a
/// re-run instead of a permanent hole in history. Exit 3, the same
/// status the mojibake guard uses and for the same reason: a well-formed
/// request, deliberately answered no.
fn check_committable(file_path: &str, state: &str, ws_hash: Option<&str>) -> Result<()> {
    // `'conflict'` is refused ahead of the hash check, because it is
    // refused whatever the hash is. `detect_changes` can produce it, and
    // the live `vcs_file_states.change_type` CHECK allows only
    // ('added','modified','deleted','renamed','unmodified') — so
    // Python's own INSERT would be rejected by the constraint. The
    // subset schema carries no CHECK, following the rule that a mirror
    // must accept what the live database holds, which means this port
    // would *silently* write a value the live schema forbids. A row that
    // can never go home is worse than a refusal.
    if state == "conflict" {
        return Err(anyhow::Error::new(crate::Coded::refused(format!(
            "refusing to commit {file_path}: its staged state is 'conflict'.\n\
             \x20 Resolving a conflict means comparing the workspace against the \
             database, which this command cannot do — and the live \
             `vcs_file_states.change_type` CHECK would reject the row anyway.\n\
             \x20 Fix: resolve it with the Python `templedb vcs commit`."
        ))));
    }
    if ws_hash.is_some() || state == "deleted" {
        return Ok(());
    }
    Err(anyhow::Error::new(crate::Coded::refused(format!(
        "refusing to commit {file_path}: state is '{state}' but the working \
         state has no content hash, so the commit could only record the \
         '{DELETED}' sentinel and the content would be unrecoverable.\n\
         \x20 Fix: templedb vcs status <project> --refresh"
    ))))
}

/// The project's active branch, then its default — or `None` when it
/// has neither.
///
/// Python's active-branch join does not check that the branch it finds
/// belongs to the project: it reads `projects.active_branch_id` and
/// follows it. Reproduced, and then checked — a writer that followed a
/// cross-project pointer would append a commit to another project's
/// history, which is the same class of mistake `resolve_project_exact`
/// refuses fuzzy matching for. Reading that way is harmless; writing
/// that way is not.
///
/// Shared with `file set`'s auto-stage, and the sharing is the point
/// rather than a tidy-up. A stage written against one branch and a
/// commit reading another is a loop that reports success at both ends
/// and moves nothing. Python has **two** copies of this lookup and they
/// are not the same query: the auto-stage's reads
/// `projects.active_branch_id` without joining `vcs_branches` at all, so
/// it will stage against a branch id naming no row — and then
/// `vcs commit`, which does join, finds nothing staged.
pub(crate) fn active_or_default_branch(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
) -> Result<Option<Branch>> {
    let active = tx
        .query_opt(
            "SELECT vb.id, vb.branch_name, vb.head_commit_id, vb.project_id
               FROM projects p
               JOIN vcs_branches vb ON p.active_branch_id = vb.id
              WHERE p.id = $1",
            &[&project_id],
        )
        .context("looking up the active branch")?;
    if let Some(row) = &active {
        let owner: i64 = row.get(3);
        if owner != project_id {
            bail!(
                "project {project_id}'s active_branch_id names branch {} \
                 of project {owner}.\n\
                 Python's active-branch join does not check this and would \
                 write there; touching another project's history is not a \
                 mistake worth reproducing.\n\
                 Pass --branch to say which branch you meant.",
                row.get::<_, i64>(0)
            );
        }
    }
    let row = match active {
        Some(row) => Some(row),
        // Python's fallback, in Python's order.
        None => tx
            .query_opt(
                "SELECT id, branch_name, head_commit_id
                   FROM vcs_branches
                  WHERE project_id = $1 AND is_default = 1
                  LIMIT 1",
                &[&project_id],
            )
            .context("looking up the default branch")?,
    };
    Ok(row.map(|row| Branch {
        id: row.get(0),
        name: row.get(1),
        head_commit_id: row.get(2),
    }))
}

/// The branch to commit to: an exact name, or the active one.
///
/// Python's `--branch` path lists the project's branches and picks by
/// name, so it is project-scoped; the no-name path is
/// [`active_or_default_branch`].
fn resolve_branch(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    name: Option<&str>,
) -> Result<Branch> {
    let found = match name {
        Some(name) => tx
            .query_opt(
                "SELECT id, branch_name, head_commit_id
                   FROM vcs_branches
                  WHERE project_id = $1 AND branch_name = $2",
                &[&project_id, &name],
            )
            .context("looking up the named branch")?
            .map(|row| Branch {
                id: row.get(0),
                name: row.get(1),
                head_commit_id: row.get(2),
            }),
        None => active_or_default_branch(tx, project_id)?,
    };
    let Some(branch) = found else {
        match name {
            Some(name) => bail!(
                "no branch {name:?} in this project.\n\
                 List them: templedb vcs branch <project>"
            ),
            None => bail!(
                "this project has no active branch and no default branch, so \
                 there is nowhere to commit.\n\
                 Create one: templedb vcs branch <project> main"
            ),
        }
    };
    Ok(branch)
}

/// The branch's current head, read inside the transaction.
///
/// Separate from [`resolve_branch`] so a test can interleave two
/// transactions around it — see [`advance_branch_head`], whose whole
/// contract is about the value read here having moved.
pub fn branch_head(tx: &mut postgres::Transaction<'_>, branch_id: i64) -> Result<Option<i64>> {
    let row = tx
        .query_opt(
            "SELECT head_commit_id FROM vcs_branches WHERE id = $1",
            &[&branch_id],
        )
        .context("reading the branch head")?;
    match row {
        Some(row) => Ok(row.get(0)),
        None => bail!("branch {branch_id} no longer exists"),
    }
}

/// Advance `vcs_branches.head_commit_id` from `expected` to `new`, or
/// refuse.
///
/// The one place this port is deliberately better than Python rather
/// than merely faithful. Over there the column is advanced by the
/// `update_branch_head_on_commit` trigger, and the app-level
/// compare-and-swap that preceded it *could not work*: the trigger had
/// already written the new id by the time the CAS's `WHERE` ran, so it
/// reported `HEAD_RACE` on every commit. The comment recording that is
/// still in `vcs.py` and it ends "if cross-transaction race protection
/// is needed, drop the trigger and do the UPDATE-with-CAS inside
/// create_commit()" — which is exactly the situation an implementation
/// with no triggers is already in.
///
/// `IS NOT DISTINCT FROM` and not `=`: the expected head is NULL for a
/// branch with no commits, and `NULL = NULL` is unknown, so `=` would
/// refuse every root commit.
///
/// Under Postgres's default `READ COMMITTED` this is a genuine CAS and
/// not an optimistic guess. A concurrent transaction that has advanced
/// the head blocks this `UPDATE` on the row lock; when it commits, the
/// predicate is re-evaluated against the new row version and matches
/// nothing, so the refusal below fires rather than the two commits
/// silently interleaving.
pub fn advance_branch_head(
    tx: &mut postgres::Transaction<'_>,
    branch_id: i64,
    expected: Option<i64>,
    new: i64,
) -> Result<()> {
    let moved = tx
        .execute(
            "UPDATE vcs_branches
                SET head_commit_id = $1
              WHERE id = $2
                AND head_commit_id IS NOT DISTINCT FROM $3",
            &[&new, &branch_id, &expected],
        )
        .context("advancing the branch head")?;
    if moved == 1 {
        return Ok(());
    }
    // Report what the head actually holds, because "someone else
    // committed" is only actionable if you can see where they landed.
    let found = branch_head(tx, branch_id).ok().flatten();
    Err(anyhow::Error::new(crate::Coded::conflict(format!(
        "HEAD_RACE on branch {branch_id}: this commit was built on head {}, \
         but the branch now points at {}.\n\
         \x20 Another session committed while this one was working. Nothing \
         has been written — re-stage against the new head and commit again.",
        expected.map_or_else(|| "none".to_string(), |h| h.to_string()),
        found.map_or_else(|| "none".to_string(), |h| h.to_string())
    ))))
}

/// Guard 2: the defensive orphan reap, which is pure SQL.
///
/// `vcs_working_state.file_id` has an `ON DELETE CASCADE` FK to
/// `project_files` in the live schema, but `sync_engine` and some
/// migrations bulk-delete with FK enforcement off — so rows pointing at
/// a file that no longer exists are a state the live database reaches.
/// When the commit then tries to record one in `vcs_file_states`, the FK
/// *there* fires and the whole commit aborts. That happened on
/// 2026-09-19.
///
/// Session-wide and not scoped to the project or the branch, which is
/// Python's query exactly. Widening it would be tidier and would reap
/// rows this commit was never going to touch.
///
/// Python wraps this in `except Exception: pass` — "orphan reap skipped
/// (non-fatal)". Not reproduced: the swallow is there because the
/// statement runs outside the commit's own transaction over there, and
/// here it is inside one, so a failure that is ignored would be a
/// failure that rolls the commit back anyway. A loud error beats a debug
/// line.
fn reap_orphans(tx: &mut postgres::Transaction<'_>, session: SessionId) -> Result<u64> {
    // The count is the return value, which is the half Python got wrong:
    // `execute` returns `cursor.lastrowid`, meaningless after a DELETE,
    // so `if orphaned:` was true on essentially every commit and the
    // debug line "reaped N orphaned working_state row(s)" invented the
    // very condition it was added to observe. `execute` here returns the
    // row count, so there is nothing to get wrong.
    tx.execute(
        "DELETE FROM vcs_working_state
          WHERE staged_by_session_id = $1
            AND file_id NOT IN (SELECT id FROM project_files)",
        &[&session.get()],
    )
    .context("reaping orphaned working-state rows")
}

/// This session's staged rows on this branch, joined to what the commit
/// records.
///
/// `ORDER BY ws.id` is this port's addition — Python's query has none.
/// It changes no output: `vcs show` orders its file list by path and
/// nothing reads `vcs_file_states.id`. What it buys is that the ids a
/// commit mints are a function of its input, so a write test can assert
/// on them and two runs of the same fixture produce the same rows in the
/// same order.
fn staged_rows(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    branch_id: i64,
    session: SessionId,
) -> Result<Vec<Staged>> {
    let rows = tx
        .query(
            "SELECT ws.file_id, pf.file_path, ws.state, ws.content_hash,
                    fc.file_size_bytes, fc.line_count
               FROM vcs_working_state ws
               LEFT JOIN file_contents fc
                      ON fc.file_id = ws.file_id AND fc.is_current = 1
               LEFT JOIN project_files pf ON pf.id = ws.file_id
              WHERE ws.project_id = $1 AND ws.branch_id = $2
                AND ws.staged_by_session_id = $3
              ORDER BY ws.id",
            &[&project_id, &branch_id, &session.get()],
        )
        .context("reading the staged rows")?;
    Ok(rows
        .iter()
        .map(|r| Staged {
            file_id: r.get(0),
            file_path: r.get(1),
            state: r.get(2),
            ws_hash: r.get(3),
            current_size: r.get(4),
            current_lines: r.get(5),
        })
        .collect())
}

/// The size and line count to record for a staged hash, from the blob.
///
/// Python re-derives both from `content_blobs` rather than trusting the
/// `file_contents` join, and it has to: that join is the *current*
/// revision, and a modified file's staged hash is by definition not
/// that. The arithmetic is Python's, quirks included —
///
/// - size is the UTF-8 byte length of the text, and the length of the
///   binary blob when there is no text;
/// - an **empty** `content_text` is falsy over there, so it takes the
///   binary branch for the size and yields a NULL line count rather
///   than 0 or 1.
///
/// The text is unescaped first. `mirror` and `file set` both store
/// content through `pgtext::escape`, so the stored string is not the
/// bytes — measuring the escaped form would overcount every file holding
/// a NUL, and one live file does.
///
/// # Where the tree half is refused
///
/// A missing blob is where Python leaves the database: it resolves the
/// file's path, joins it onto `~/.config/templedb/checkouts/<slug>`,
/// reads the bytes off disk and stores them. That is the mirror-to-disk
/// dependency the design document says settles the question — this port
/// has no checkout, so it refuses here instead, by name and with the
/// hash it could not find.
fn blob_metrics(
    tx: &mut postgres::Transaction<'_>,
    file_path: &str,
    ws_hash: &str,
) -> Result<(i64, Option<i64>)> {
    let row = tx
        .query_opt(
            "SELECT content_text, content_blob FROM content_blobs WHERE hash_sha256 = $1",
            &[&ws_hash],
        )
        .context("reading the staged blob")?;
    let Some(row) = row else {
        return Err(anyhow::Error::new(crate::Coded::refused(format!(
            "refusing to commit {file_path}: its staged hash {} names no blob.\n\
             \x20 The Python implementation reads the file off disk here, from \
             the project's checkout, and stores the blob before committing. \
             This port has no checkout — it reads a database and writes one — \
             so there is nothing to read.\n\
             \x20 Fix: templedb vcs status <project> --refresh, which stores \
             the blob, and then commit again.",
            short(ws_hash)
        ))));
    };
    let text: Option<String> = row.get(0);
    let blob: Option<Vec<u8>> = row.get(1);
    let text = text.map(|t| pgtext::unescape(&t).into_owned());
    match text.as_deref().filter(|t| !t.is_empty()) {
        Some(t) => Ok((t.len() as i64, Some(t.matches('\n').count() as i64 + 1))),
        // Python's `if content_text` is false for both NULL and `''`,
        // and both land here.
        None => Ok((blob.map_or(0, |b| b.len() as i64), None)),
    }
}

/// Commit this session's staged rows.
///
/// One transaction, as Python's is, and the order is Python's: the
/// commit row, the parent edge, the per-file states, the deletions, then
/// the unstage. The branch-head compare-and-swap is inserted where the
/// trigger would have fired — immediately after the commit row — so the
/// `HEAD_RACE` refusal costs nothing and happens before any file work,
/// the same reason `--if-match` is checked first in `file set`.
///
/// # What it does not write
///
/// - **`vcs_commits.parent_commit_id`.** Not in the mirrored subset: no
///   reader names it, `vcs show` walks `vcs_commit_parents` instead, and
///   Python's own `create_commit` docstring records the column drifting
///   out of sync with the join table (`vcs merge` wrote the rows and not
///   the column, so merge commits read as roots). One declarer per fact,
///   and the join table is the declarer.
/// - **`vcs_commit_metadata`.** The design document lists it among the
///   tables commit writes; measured, `vcs commit` never touches it.
///   Only `templedb commit <slug> <dir>` does, through
///   `create_commit_metadata`. Same for `vcs_session_heads`.
/// - **`vcs_commits.session_id`.** Python's `vcs commit` leaves it NULL
///   too — migration 107's writer side was only ever wired into the
///   other commit command — so commits land already published, which is
///   also what makes them visible to `vcs log`'s default view for the
///   cross-read. 73 of 1,444 live commits carry a session.
/// - **`deploy_stage_runs`.** Chain observability, outside the subset.
pub fn commit_staged(
    tx: &mut postgres::Transaction<'_>,
    clock: &Clock,
    project_id: i64,
    slug: &str,
    opts: &CommitOptions<'_>,
) -> Result<CommitOutcome> {
    let branch = resolve_branch(tx, project_id, opts.branch)?;
    let reaped = reap_orphans(tx, opts.session)?;
    let staged = staged_rows(tx, project_id, branch.id, opts.session)?;

    let other_sessions_staged: i64 = tx
        .query_one(
            "SELECT COUNT(*) FROM vcs_working_state
              WHERE project_id = $1 AND branch_id = $2
                AND staged_by_session_id IS NOT NULL
                AND staged_by_session_id != $3",
            &[&project_id, &branch.id, &opts.session.get()],
        )
        .context("counting other sessions' staged rows")?
        .get(0);

    if staged.is_empty() {
        // Python prints this on stdout and returns 1, following git:
        // non-zero when nothing was committed, including when there was
        // simply nothing to commit. Exit 1 is what a bare `bail!`
        // produces here, so the status matches without a `Coded`.
        //
        // The other-sessions line is Python's too, and it is the one
        // piece of guidance that has earned its place: staging in one
        // shell and committing in another is how an agent loses a
        // commit, because every tool call gets its own session leader.
        let scattered = if other_sessions_staged > 0 {
            format!(
                "\n  {other_sessions_staged} file(s) are staged in other \
                 sessions and were not considered. Pin one identity for both \
                 steps: TEMPLEDB_SESSION_ID, or --session on each call."
            )
        } else {
            String::new()
        };
        bail!(
            "nothing staged for commit in session {} on branch {}.{scattered}",
            opts.session,
            branch.name
        );
    }

    // Guard 3 and the conflict refusal run over the whole set *before*
    // anything is written. Python checks inside the per-file loop, after
    // the commit row exists — which is harmless there because the
    // transaction rolls back, but it means the refusal it prints is
    // about file four while files one to three looked fine. Checking
    // first is the same information, earlier.
    for file in &staged {
        check_committable(
            file.file_path.as_deref().unwrap_or("?"),
            &file.state,
            file.ws_hash.as_deref(),
        )?;
    }

    // Refuse a head that names no commit, rather than discovering it as
    // a foreign-key violation three statements later.
    //
    // `vcs_branches.head_commit_id` carries no FK — in this subset by
    // decision and in the live schema by omission — so a pruned commit
    // leaves a dangling head, which `vcs branch` renders as a NULL
    // message and reads fine. `vcs_commit_parents` *does* reference
    // `vcs_commits`, so the parent edge below would be the first thing
    // to notice, with an error about a constraint rather than about a
    // branch.
    if let Some(head) = branch.head_commit_id {
        let exists = tx
            .query_opt("SELECT 1 FROM vcs_commits WHERE id = $1", &[&head])
            .context("checking the branch head resolves")?;
        if exists.is_none() {
            bail!(
                "branch {} has head_commit_id {head}, which names no commit — \
                 the row was pruned and the head was never updated.\n\
                 A commit appended here could record no parent edge, so the \
                 history would fork silently.",
                branch.name
            );
        }
    }

    let now = clock.now_timestamp();
    let hash = commit_hash(slug, &branch.name, opts.message, clock.unix_seconds());

    let commit_id: i64 = tx
        .query_one(
            "INSERT INTO vcs_commits
                 (project_id, branch_id, commit_hash, author, commit_message,
                  commit_timestamp)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id",
            &[
                &project_id,
                &branch.id,
                &hash,
                &opts.author,
                &opts.message,
                &now,
            ],
        )
        .context("inserting the commit")?
        .get(0);

    // Where the trigger would have fired.
    advance_branch_head(tx, branch.id, branch.head_commit_id, commit_id)?;

    if let Some(parent) = branch.head_commit_id {
        // Python writes this edge twice — once inside `create_commit`
        // from a parent resolved by *hash*, and once here from the
        // branch head — with `INSERT OR IGNORE` making the second a
        // no-op. Written once, from the id, because the hash round-trip
        // can only resolve to the same row or to nothing, and the
        // nothing case is already refused above.
        //
        // `ON CONFLICT DO NOTHING` is kept anyway: `parent_order` is 0
        // on all 718 live rows because no merge has ever been
        // committed, and the day a second parent is added by something
        // else, a re-run must not fail on the first edge.
        tx.execute(
            "INSERT INTO vcs_commit_parents (commit_id, parent_commit_id, parent_order)
             VALUES ($1, $2, 0)
             ON CONFLICT (commit_id, parent_commit_id) DO NOTHING",
            &[&commit_id, &parent],
        )
        .context("recording the parent edge")?;
    }

    let mut deletions = 0usize;
    for file in &staged {
        let ws_hash = file.ws_hash.as_deref().unwrap_or(DELETED);
        let names_content =
            ws_hash != DELETED && matches!(file.state.as_str(), "modified" | "added");

        let (size, lines) = if names_content {
            blob_metrics(tx, file.file_path.as_deref().unwrap_or("?"), ws_hash)?
        } else {
            // Python's fallback: the current revision's numbers, with a
            // missing size read as 0 and a missing line count left NULL.
            (file.current_size.unwrap_or(0), file.current_lines)
        };

        // A plain INSERT where Python has `ON CONFLICT (commit_id,
        // file_id) DO UPDATE`. That arm is unreachable from here: the
        // commit id was minted three statements ago, so it owns no rows
        // yet, and `vcs_working_state`'s `UNIQUE (project_id, branch_id,
        // file_id)` means one staged row per file. Python carries the
        // upsert because `record_file_change` annotates rows its caller
        // already inserted. Keeping dead statements for fidelity would
        // also turn a duplicate — which could only be a bug in the
        // query above — into a silent overwrite.
        tx.execute(
            "INSERT INTO vcs_file_states
                 (commit_id, file_id, file_path, content_hash, file_size,
                  line_count, change_type)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                &commit_id,
                &file.file_id,
                &file.file_path,
                &ws_hash,
                &size,
                &lines,
                &file.state,
            ],
        )
        .context("recording a file state")?;

        if names_content {
            // So materialisation and `file cat` see the committed
            // content. `updated_at` is supplied on both arms, where
            // Python supplies it on neither: SQLite defaults the column
            // to `datetime('now')` and the subset schema has no DEFAULT,
            // because it was written to carry mirrored rows that already
            // had one. The clock is the port's answer to the same
            // question, and an injected one is what lets a write test
            // assert the value instead of excluding it.
            tx.execute(
                "INSERT INTO file_contents
                     (file_id, content_hash, file_size_bytes, line_count,
                      is_current, updated_at)
                 VALUES ($1, $2, $3, $4, 1, $5)
                 ON CONFLICT (file_id, is_current) DO UPDATE SET
                     content_hash    = excluded.content_hash,
                     file_size_bytes = excluded.file_size_bytes,
                     line_count      = excluded.line_count,
                     updated_at      = excluded.updated_at",
                &[&file.file_id, &ws_hash, &size, &lines, &now],
            )
            .context("advancing the current content")?;
        }

        if file.state == "deleted" {
            deletions += 1;
            // The one place in the whole system that hard-deletes a
            // `project_files` row — everything else soft-deletes by
            // flipping `status`, which is why `file set` needs an
            // un-delete guard at all. `file_contents` goes with it by
            // cascade; `vcs_file_states` does not, which is what the
            // denormalised `file_path` above is for.
            tx.execute("DELETE FROM project_files WHERE id = $1", &[&file.file_id])
                .context("deleting a committed file")?;
            tx.execute(
                "DELETE FROM vcs_working_state
                  WHERE file_id = $1 AND project_id = $2 AND branch_id = $3",
                &[&file.file_id, &project_id, &branch.id],
            )
            .context("retiring a deleted file's working state")?;
        }
    }

    // Unstage what was committed, and only this session's rows: other
    // sessions' staged rows survive, which is the whole point of the
    // column. The deletions' rows are already gone.
    tx.execute(
        "UPDATE vcs_working_state
            SET state = 'unmodified', staged_by_session_id = NULL
          WHERE project_id = $1 AND branch_id = $2
            AND staged_by_session_id = $3",
        &[&project_id, &branch.id, &opts.session.get()],
    )
    .context("clearing the staging flags")?;

    Ok(CommitOutcome {
        hash,
        commit_id,
        branch_name: branch.name,
        files: staged.len(),
        deletions,
        other_sessions_staged,
        reaped,
        previous_head: branch.head_commit_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The hash is sixteen uppercase hex characters of a sha256 over the
    /// four fields joined by colons.
    ///
    /// The expected value is an oracle: computed by CPython, which is
    /// the only way to check that the *input string* is assembled the
    /// same way rather than merely that sha256 works.
    ///
    /// ```text
    /// >>> import hashlib
    /// >>> s = f"bza:main:fix the button:{1761393045.0}"
    /// >>> s
    /// 'bza:main:fix the button:1761393045.0'
    /// >>> hashlib.sha256(s.encode()).hexdigest()[:16].upper()
    /// 'D0B74D456C6AFC2E'
    /// ```
    #[test]
    fn the_commit_hash_matches_cpythons() {
        assert_eq!(
            commit_hash("bza", "main", "fix the button", 1_761_393_045.0),
            "D0B74D456C6AFC2E"
        );
    }

    /// Sixteen characters, uppercase hex, and nothing else. Lowercase
    /// would mint commits Python's `--amend` refuses to touch, because
    /// it reads a lowercase hash as possibly-published history.
    #[test]
    fn the_hash_is_sixteen_uppercase_hex_characters() {
        let h = commit_hash("p", "main", "m", 1.5);
        assert_eq!(h.len(), 16);
        assert!(
            h.chars()
                .all(|c| c.is_ascii_digit() || ('A'..='F').contains(&c)),
            "{h}"
        );
    }

    /// The clock is in the input, so the same tree committed twice gets
    /// different ids — and the same *stated* instant gets the same one,
    /// which is what makes the write tests deterministic.
    #[test]
    fn the_instant_is_part_of_the_id() {
        let a = commit_hash("p", "main", "m", 1_761_393_045.0);
        let b = commit_hash("p", "main", "m", 1_761_393_045.001);
        assert_ne!(a, b);
        assert_eq!(a, commit_hash("p", "main", "m", 1_761_393_045.0));
    }

    /// Every field reaches the digest. A port that dropped one would
    /// still produce a plausible id, and two branches committing the
    /// same message at the same instant would collide on
    /// `commit_hash`'s UNIQUE constraint.
    #[test]
    fn every_field_changes_the_id() {
        let base = commit_hash("p", "main", "m", 1.0);
        assert_ne!(base, commit_hash("q", "main", "m", 1.0));
        assert_ne!(base, commit_hash("p", "dev", "m", 1.0));
        assert_ne!(base, commit_hash("p", "main", "n", 1.0));
    }

    /// Twelve characters, and never a panic. The input is a `text`
    /// column with no constraint on either side, so a multibyte value
    /// is reachable — and `&s[..12]` on one panics *inside a refusal*,
    /// which is the worst place for it.
    #[test]
    fn a_hash_is_shortened_by_character_and_not_by_byte() {
        assert_eq!(short("abcdef1234567890"), "abcdef123456");
        assert_eq!(short("abc"), "abc");
        assert_eq!(short(""), "");
        // Three bytes per character: a byte slice at 12 would land
        // mid-character.
        assert_eq!(short("✓✓✓✓✓"), "✓✓✓✓✓");
        assert_eq!(short("✓✓✓✓✓✓✓✓✓✓✓✓✓✓"), "✓✓✓✓✓✓✓✓✓✓✓✓");
    }

    /// Python's `str(float)`, which is `repr`: shortest round-trip, with
    /// a mandatory `.0` on an integral value. The last one is the case
    /// Rust gets different, and it is the case a pinned clock always
    /// produces.
    #[test]
    fn floats_render_the_way_python_renders_them() {
        assert_eq!(python_float(1_761_393_045.0), "1761393045.0");
        assert_eq!(python_float(0.0), "0.0");
        assert_eq!(python_float(1_761_393_045.25), "1761393045.25");
        // Shortest round-trip, not seventeen digits.
        assert_eq!(python_float(0.1), "0.1");
        assert_eq!(python_float(1.5), "1.5");
    }

    /// Guard 3: a staged non-deleted file with no hash is refused, and a
    /// deletion with no hash is fine — that is what the sentinel means.
    #[test]
    fn a_missing_hash_is_refused_unless_the_file_was_deleted() {
        for state in ["added", "modified", "unmodified", "renamed"] {
            let err = check_committable("src/a.rs", state, None).unwrap_err();
            assert_eq!(crate::code_of(&err), crate::NO_PATH, "{state}");
            let msg = format!("{err:#}");
            assert!(msg.contains("src/a.rs"), "{msg}");
            assert!(msg.contains("DELETED"), "{msg}");
            assert!(msg.contains("--refresh"), "{msg}");
        }
        assert!(check_committable("src/a.rs", "deleted", None).is_ok());
    }

    /// A hash present means nothing to check, whatever the state.
    #[test]
    fn a_hash_is_always_committable() {
        for state in ["added", "modified", "unmodified", "deleted", "renamed"] {
            assert!(
                check_committable("a", state, Some("abc123")).is_ok(),
                "{state}"
            );
        }
    }

    /// `'conflict'` is refused with or without a hash. Python's INSERT
    /// would be rejected by the live CHECK; this subset has no CHECK, so
    /// the refusal has to be in the code or the row goes in silently.
    #[test]
    fn a_conflicted_state_is_refused_either_way() {
        for hash in [None, Some("abc123")] {
            let err = check_committable("src/c.rs", "conflict", hash).unwrap_err();
            assert_eq!(crate::code_of(&err), crate::NO_PATH);
            let msg = format!("{err:#}");
            assert!(msg.contains("'conflict'"), "{msg}");
            assert!(msg.contains("CHECK"), "{msg}");
        }
    }
}
