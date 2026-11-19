//! `file set` and `vcs commit-staged` against a real Postgres, verified
//! by reading them back.
//!
//! `#[ignore]`d, like the two CPython oracles, because it needs a
//! server. `just write-test` runs it. That keeps the property the
//! README advertises — no Postgres needed to build or to `cargo test` —
//! while still testing the one thing unit tests structurally cannot:
//! that the SQL is accepted by the engine it was written for, and that
//! the effect is visible to the read layer afterwards.
//!
//! The verification technique is the one the write-path design document
//! calls **cross-read**, and it is the only one available without new
//! machinery: the port writes, and the already-trusted read layer reads
//! the result back. A byte-diff against Python is not possible here and
//! will not become possible — Python writes SQLite and this writes
//! Postgres, so there is no shared input to diff.
//!
//! Every test builds its own project and cleans up after itself, so the
//! suite is re-runnable against a scratch database that is not
//! re-mirrored in between.

use templedb_db::{set_file, Clock, PgWriter, Store, Value, WRITE_URL_ENV};

/// The instant the staging fixtures write at.
///
/// Pinned, like every other clock in this file, so `last_modified` is a
/// value a test can assert rather than a column it has to exclude —
/// which is the whole reason the clock seam was a precondition for any
/// write at all.
const STAGE_INSTANT: &str = "2026-10-27 12:00:00";

/// The scratch database, or skip.
///
/// Reads [`WRITE_URL_ENV`] directly rather than going through
/// `PgWriter::open`, because the opt-in gate is a CLI-facing guard and
/// a test that had to set a process-wide variable to get past it would
/// race every other test in the binary.
fn scratch_url() -> Option<String> {
    match std::env::var(WRITE_URL_ENV) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => None,
    }
}

/// A project to write into, created fresh and removed at the end.
///
/// `ON DELETE CASCADE` on `project_files.project_id` and
/// `file_contents.file_id` means dropping the project takes its files
/// and their content rows with it. `content_blobs` rows are left: they
/// are content-addressed and shared, and deleting one could orphan a
/// row this test did not create.
struct Fixture {
    writer: PgWriter,
    project_id: i64,
    slug: String,
    /// `vcs_sessions` rows this fixture minted, to delete on the way
    /// out. Nothing cascades them — the reap policy orphans staged rows
    /// by design — and `MAX(id) + 1000` means a leaked row is walked
    /// past rather than collided with, which is the kind of leak that
    /// never fails and never stops growing.
    sessions: Vec<i64>,
}

impl Fixture {
    fn new(url: &str, slug: &str) -> Fixture {
        let mut writer = PgWriter::connect(url).expect("connect to the scratch database");
        {
            let mut tx = writer.transaction().unwrap();
            tx.execute("DELETE FROM projects WHERE slug = $1", &[&slug])
                .unwrap();
            tx.commit().unwrap();
        }
        let project_id = {
            let mut tx = writer.transaction().unwrap();
            // An id above anything the mirror copies. `projects.id` is
            // not an identity column — only `project_files` and
            // `file_contents` are, because only those two are inserted
            // into by a write path.
            let row = tx
                .query_one(
                    "INSERT INTO projects (id, slug, name)
                     VALUES ((SELECT COALESCE(MAX(id), 0) + 1000 FROM projects), $1, $2)
                     RETURNING id",
                    &[&slug, &"write-parity fixture"],
                )
                .unwrap();
            let id: i64 = row.get(0);
            tx.commit().unwrap();
            id
        };
        Fixture {
            writer,
            project_id,
            slug: slug.to_string(),
            sessions: Vec::new(),
        }
    }

    fn set(
        &mut self,
        path: &str,
        content: &str,
        if_match: Option<&str>,
    ) -> anyhow::Result<templedb_db::SetOutcome> {
        let clock = Clock::at("2026-10-21 12:00:00").unwrap();
        let mut tx = self.writer.transaction()?;
        // The default write: intent recorded, no session pinned, an
        // author so the column is exercised rather than left NULL on
        // every row. No session means no stage, which is what keeps the
        // commit tests below building their own staged rows — see
        // `Fixture::set_as` for the write that stages.
        let opts = templedb_db::SetOptions {
            if_match,
            allow_mojibake: false,
            skip_intent: false,
            stage: false,
            session: None,
            author: Some("write-parity"),
        };
        let outcome = set_file(&mut tx, &clock, self.project_id, path, content, &opts);
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                // Explicitly rolled back, which is the behaviour a
                // refusal has to have: a rejected write must leave
                // nothing behind.
                drop(tx);
                Err(e)
            }
        }
    }

    /// The content, through **the ported `file cat` reader**.
    ///
    /// `templedb_db::read_file`, not SQL written for this test. That is
    /// what makes this a cross-read rather than a round-trip: the query
    /// being exercised is the one the byte-diff sweep already verifies
    /// against Python, so a write it cannot see is a write `file cat`
    /// cannot see.
    ///
    /// It also carries the `status = 'active'` filter for free, which
    /// is the whole point of the un-delete test — a hand-written query
    /// that omitted the filter made that test pass against a
    /// soft-deleted row and assert nothing.
    fn read_back(&self, path: &str) -> Option<String> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::read_file(&store, &self.slug, path).ok()
    }

    /// The row's recorded size and line count, which `read_file` does
    /// not return but `file ls -l` and `source revisions` print.
    fn stats(&self, path: &str) -> Option<(i64, i64)> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        let row = store
            .query_opt(
                "SELECT fc.file_size_bytes, fc.line_count
                   FROM project_files pf
                   JOIN file_contents fc ON fc.file_id = pf.id AND fc.is_current = 1
                  WHERE pf.project_id = ?1 AND pf.file_path = ?2
                    AND pf.status = 'active'",
                &[Value::from(self.project_id), Value::from(path)],
            )
            .unwrap()?;
        Some((row.get_i64(0).unwrap(), row.get_i64(1).unwrap()))
    }

    /// Every `edit_intents` row for one path, oldest first, as
    /// `(base_revision, new_content_hash, patch_summary, author,
    /// description, status, session_id)`.
    ///
    /// Read through [`Store`] rather than the writer's own connection,
    /// which is the cross-read rule applied to the provenance table
    /// too: the write is only verified if something *else* can see it.
    #[allow(clippy::type_complexity)]
    fn intents(
        &self,
        path: &str,
    ) -> Vec<(
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
        Option<i64>,
    )> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        store
            .query(
                "SELECT base_revision, new_content_hash, patch_summary, author,
                        description, status, session_id
                   FROM edit_intents
                  WHERE project_id = ?1 AND file_path = ?2
                  ORDER BY id",
                &[Value::from(self.project_id), Value::from(path)],
            )
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r.get_string(0).unwrap(),
                    r.get_string(1).unwrap(),
                    r.get_opt_string(2).unwrap(),
                    r.get_opt_string(3).unwrap(),
                    r.get_opt_string(4).unwrap(),
                    r.get_string(5).unwrap(),
                    r.get_opt_i64(6).unwrap(),
                )
            })
            .collect()
    }
}

/// The `vcs commit-staged` half of the fixture.
///
/// Everything here builds the state a commit *consumes* — a branch, a
/// live session, staged `vcs_working_state` rows. `file set` produces
/// that last one itself now, and
/// [`the_loop_closes_when_file_set_stages_its_own_commit`] is the test
/// that drives both halves with no SQL in between. [`Fixture::stage`]
/// stays, because it can build states `file set` cannot: a `deleted`
/// row, a hash naming no blob, an orphan pointing at a reaped file.
/// Those are three of the commit's refusals and they have no writer.
///
/// Ids are supplied explicitly for `vcs_branches` and `vcs_sessions`:
/// only the tables a write path inserts into carry an identity column,
/// and neither of those is written by the port. `MAX(id) + 1000` keeps
/// them clear of anything a mirror has copied, which is the same trick
/// the project id uses.
impl Fixture {
    /// A branch to commit to, made the project's active one.
    fn branch(&mut self, name: &str) -> i64 {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "INSERT INTO vcs_branches (id, project_id, branch_name, is_default)
                 VALUES ((SELECT COALESCE(MAX(id), 0) + 1000 FROM vcs_branches), $1, $2, 1)
                 RETURNING id",
                &[&self.project_id, &name],
            )
            .unwrap();
        let id: i64 = row.get(0);
        tx.execute(
            "UPDATE projects SET active_branch_id = $1 WHERE id = $2",
            &[&id, &self.project_id],
        )
        .unwrap();
        tx.commit().unwrap();
        id
    }

    /// A live session, and its `SessionId` through the real resolver.
    ///
    /// Through [`templedb_db::resolve_session_tx`] rather than a
    /// hand-made newtype, because `SessionId` has no public constructor
    /// on purpose: holding one is supposed to mean *this id named a live
    /// row*, and a test that forged one would be testing a different
    /// program.
    fn session(&mut self) -> (i64, templedb_db::SessionId) {
        let id = {
            let mut tx = self.writer.transaction().unwrap();
            let row = tx
                .query_one(
                    "INSERT INTO vcs_sessions (id, ended_at)
                     VALUES ((SELECT COALESCE(MAX(id), 0) + 1000 FROM vcs_sessions), NULL)
                     RETURNING id",
                    &[],
                )
                .unwrap();
            let id: i64 = row.get(0);
            tx.commit().unwrap();
            id
        };
        self.sessions.push(id);
        let mut tx = self.writer.transaction().unwrap();
        let resolved = templedb_db::resolve_session_tx(&mut tx, Some(&id.to_string()))
            .unwrap()
            .expect("the session just inserted must resolve");
        tx.commit().unwrap();
        (id, resolved)
    }

    /// The `project_files.id` for a path this fixture has written.
    fn file_id(&mut self, path: &str) -> i64 {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "SELECT id FROM project_files WHERE project_id = $1 AND file_path = $2",
                &[&self.project_id, &path],
            )
            .unwrap();
        let id: i64 = row.get(0);
        tx.commit().unwrap();
        id
    }

    /// Stage one file by hand: the row `vcs status --refresh` would have
    /// written.
    ///
    /// `content_hash` is the hash of the file *as the scan saw it*,
    /// which is the value the commit records — deliberately separate
    /// from whatever `file_contents` currently holds, because that
    /// difference is the whole content of a `modified` state.
    ///
    /// Still SQL, and still needed, for the states no writer produces: a
    /// `deleted` row comes from a workspace scan, and a hash naming no
    /// blob or a file_id naming no file are the two corruptions the
    /// commit's guards exist for. `file set --stage` covers the states
    /// it can reach; see [`Fixture::set_as`].
    ///
    /// The explicit id predates the identity column
    /// `vcs_working_state` gained with the auto-stage. Kept, because
    /// `MAX(id) + 1000` is what keeps a hand-built fixture row clear of
    /// a mirrored one — and because a test that supplies an id proves
    /// `BY DEFAULT` rather than `ALWAYS` is still what the DDL says.
    fn stage(
        &mut self,
        branch_id: i64,
        file_id: i64,
        state: &str,
        content_hash: Option<&str>,
        session: Option<i64>,
    ) {
        let mut tx = self.writer.transaction().unwrap();
        tx.execute(
            "INSERT INTO vcs_working_state
                 (id, project_id, branch_id, file_id, content_hash, state,
                  last_modified, staged_by_session_id)
             VALUES ((SELECT COALESCE(MAX(id), 0) + 1000 FROM vcs_working_state),
                     $1, $2, $3, $4, $5, '2026-10-25 09:00:00', $6)",
            &[
                &self.project_id,
                &branch_id,
                &file_id,
                &content_hash,
                &state,
                &session,
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// Commit, on its own transaction, rolling back on a refusal.
    fn commit(
        &mut self,
        message: &str,
        branch: Option<&str>,
        session: templedb_db::SessionId,
    ) -> anyhow::Result<templedb_db::CommitOutcome> {
        // A pinned clock, so `commit_hash` is a function of the inputs
        // and the timestamps are assertable rather than excluded.
        let clock = Clock::at("2026-10-25 12:00:00").unwrap();
        let slug = self.slug.clone();
        let project_id = self.project_id;
        let mut tx = self.writer.transaction()?;
        let outcome = templedb_db::commit_staged(
            &mut tx,
            &clock,
            project_id,
            &slug,
            &templedb_db::CommitOptions {
                message,
                author: "write-parity",
                branch,
                session,
            },
        );
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                drop(tx);
                Err(e)
            }
        }
    }

    /// The commit log, **through the ported `vcs log` reader**.
    fn log(&self) -> Vec<templedb_db::CommitEntry> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::commit_log(
            &store,
            self.project_id,
            None,
            templedb_db::Visibility::Published,
            10,
        )
        .unwrap()
    }

    /// `vcs show`'s changed-file list, through the ported reader.
    fn changed(&self, hash: &str) -> Vec<templedb_db::FileChange> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::commit_detail(&store, self.project_id, hash)
            .unwrap()
            .expect("the commit should be readable by hash")
            .1
    }

    /// `vcs_branches.head_commit_id`, read back.
    fn head(&mut self, branch_id: i64) -> Option<i64> {
        let mut tx = self.writer.transaction().unwrap();
        let head = templedb_db::branch_head(&mut tx, branch_id).unwrap();
        tx.commit().unwrap();
        head
    }

    /// `(state, staged_by_session_id)` for one staged file, or `None`
    /// when the row is gone.
    fn working_state(&mut self, file_id: i64) -> Option<(String, Option<i64>)> {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_opt(
                "SELECT state, staged_by_session_id FROM vcs_working_state
                  WHERE project_id = $1 AND file_id = $2",
                &[&self.project_id, &file_id],
            )
            .unwrap();
        tx.commit().unwrap();
        row.map(|r| (r.get(0), r.get(1)))
    }

    /// The whole staged row as the auto-stage left it:
    /// `(branch_id, state, content_hash, staged_by_session_id,
    /// last_modified, intent_id)`.
    ///
    /// Wider than [`Fixture::working_state`] because the auto-stage
    /// writes every one of these columns and the commit reads four of
    /// them. `content_hash` is the one that matters most: it is what
    /// gets committed, and Python's comment above its own INSERT says
    /// why — "without this, `file set` followed by `vcs commit` silently
    /// records the previous content".
    #[allow(clippy::type_complexity)]
    fn staged_row(
        &mut self,
        file_id: i64,
    ) -> Option<(
        i64,
        String,
        Option<String>,
        Option<i64>,
        String,
        Option<i64>,
    )> {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_opt(
                "SELECT branch_id, state, content_hash, staged_by_session_id,
                        last_modified, intent_id
                   FROM vcs_working_state
                  WHERE project_id = $1 AND file_id = $2",
                &[&self.project_id, &file_id],
            )
            .unwrap();
        tx.commit().unwrap();
        row.map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5)))
    }

    /// A write that names a session, and may require the stage.
    ///
    /// Separate from [`Fixture::set`] rather than four more parameters
    /// on it: thirty-odd call sites pass `None` for the session, and
    /// they are testing the content path rather than the staging one.
    fn set_as(
        &mut self,
        path: &str,
        content: &str,
        session: Option<templedb_db::SessionId>,
        stage: bool,
    ) -> anyhow::Result<templedb_db::SetOutcome> {
        let clock = Clock::at(STAGE_INSTANT)?;
        let mut tx = self.writer.transaction()?;
        let opts = templedb_db::SetOptions {
            session,
            stage,
            author: Some("write-parity"),
            ..Default::default()
        };
        let outcome = set_file(&mut tx, &clock, self.project_id, path, content, &opts);
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                drop(tx);
                Err(e)
            }
        }
    }

    /// Whether a `project_files` row exists for this path at all,
    /// whatever its status.
    ///
    /// Deliberately *not* through `read_file`, which filters on
    /// `status = 'active'` and so cannot tell "never written" from
    /// "soft-deleted". The refusal tests need the stronger claim: the
    /// write left nothing behind.
    fn file_row_exists(&mut self, path: &str) -> bool {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_opt(
                "SELECT 1 FROM project_files WHERE project_id = $1 AND file_path = $2",
                &[&self.project_id, &path],
            )
            .unwrap();
        tx.commit().unwrap();
        row.is_some()
    }

    /// `vcs_file_states` for one commit, as
    /// `(file_path, change_type, content_hash, file_size, line_count)`.
    ///
    /// Read with SQL rather than through a reader, because `file_path`
    /// is the column no reader names — carried so a committed deletion
    /// outlives the `project_files` row it hard-deletes, which is
    /// exactly what the deletion test asserts.
    #[allow(clippy::type_complexity)]
    fn file_states(
        &mut self,
        commit_id: i64,
    ) -> Vec<(
        Option<String>,
        String,
        Option<String>,
        Option<i64>,
        Option<i64>,
    )> {
        let mut tx = self.writer.transaction().unwrap();
        let rows = tx
            .query(
                "SELECT file_path, change_type, content_hash, file_size, line_count
                   FROM vcs_file_states WHERE commit_id = $1 ORDER BY id",
                &[&commit_id],
            )
            .unwrap();
        tx.commit().unwrap();
        rows.iter()
            .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4)))
            .collect()
    }

    /// The parent ids recorded for a commit.
    fn parents(&mut self, commit_id: i64) -> Vec<i64> {
        let mut tx = self.writer.transaction().unwrap();
        let rows = tx
            .query(
                "SELECT parent_commit_id FROM vcs_commit_parents
                  WHERE commit_id = $1 ORDER BY parent_order",
                &[&commit_id],
            )
            .unwrap();
        tx.commit().unwrap();
        rows.iter().map(|r| r.get(0)).collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut tx) = self.writer.transaction() {
            // Order matters now that commits exist. `vcs_commits`
            // cascades from `projects`, but `vcs_branches` is what
            // `head_commit_id` points at and nothing cascades *that*
            // pointer — so the branch rows go first, with the head
            // nulled, or the commit delete trips over nothing and the
            // branches are left behind for the next run to collide
            // with.
            let _ = tx.execute(
                "UPDATE vcs_branches SET head_commit_id = NULL WHERE project_id = $1",
                &[&self.project_id],
            );
            let _ = tx.execute("DELETE FROM projects WHERE slug = $1", &[&self.slug]);
            // After the project, because `vcs_working_state` and
            // `edit_intents` reference these ids in columns that declare
            // no FK and that nothing nulls — the project delete is what
            // takes those rows away.
            for id in &self.sessions {
                let _ = tx.execute("DELETE FROM vcs_sessions WHERE id = $1", &[id]);
            }
            let _ = tx.commit();
        }
    }
}

macro_rules! fixture {
    ($slug:expr) => {
        match scratch_url() {
            None => {
                eprintln!("skipped: {} is unset", WRITE_URL_ENV);
                return;
            }
            Some(url) => Fixture::new(&url, $slug),
        }
    };
}

/// A write to a path that does not exist creates the `project_files`
/// row, the blob and the `file_contents` row, and the read layer sees
/// all three.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_new_file_is_created_and_reads_back() {
    let mut f = fixture!("wp-create");
    let out = f.set("src/new.rs", "fn main() {}\n", None).unwrap();

    assert!(out.created);
    assert!(!out.undeleted);
    assert!(!out.unchanged);
    assert_eq!(out.previous, None);
    assert_eq!(out.hash, templedb_db::content_hash("fn main() {}\n"));
    assert_eq!(out.size_bytes, 13);
    assert_eq!(out.line_count, 2);

    assert_eq!(f.read_back("src/new.rs").as_deref(), Some("fn main() {}\n"));
    assert_eq!(f.stats("src/new.rs"), Some((13, 2)));
}

/// Writing the same path twice replaces the content in place rather
/// than accumulating rows — the `ON CONFLICT (file_id, is_current)`
/// arm. The second write reports the first's hash as `previous`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_write_replaces_in_place() {
    let mut f = fixture!("wp-replace");
    let first = f.set("a.txt", "one\n", None).unwrap();
    let second = f.set("a.txt", "two\n", None).unwrap();

    assert_eq!(second.previous.as_deref(), Some(first.hash.as_str()));
    assert!(!second.created);
    assert_eq!(f.read_back("a.txt").as_deref(), Some("two\n"));

    // One current row, not two. The schema's UNIQUE (file_id,
    // is_current) would reject a second, so this asserts the upsert
    // took the UPDATE arm rather than that the constraint exists.
    let url = scratch_url().unwrap();
    let store = templedb_db::PgStore::connect(&url).unwrap();
    let n = store
        .query_opt(
            "SELECT COUNT(*) FROM file_contents fc
               JOIN project_files pf ON pf.id = fc.file_id
              WHERE pf.project_id = ?1 AND pf.file_path = ?2",
            &[Value::from(f.project_id), Value::from("a.txt")],
        )
        .unwrap()
        .unwrap()
        .get_i64(0)
        .unwrap();
    assert_eq!(n, 1);
}

/// Writing identical content is reported as unchanged. Python's
/// `deploy_stage_runs` classifies this as `noop`; the port reports it.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn rewriting_identical_content_is_a_noop() {
    let mut f = fixture!("wp-noop");
    f.set("b.txt", "same\n", None).unwrap();
    let again = f.set("b.txt", "same\n", None).unwrap();
    assert!(again.unchanged);
    assert_eq!(again.previous.as_deref(), Some(again.hash.as_str()));
}

/// `--if-match` lets a matching prefix through and refuses a stale one,
/// **and the refusal leaves the database untouched**. The second half
/// is the point: a guard that rejects after writing is not a guard.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn if_match_guards_the_write_and_refuses_without_writing() {
    let mut f = fixture!("wp-ifmatch");
    let first = f.set("c.txt", "original\n", None).unwrap();

    // A matching prefix proceeds.
    let ok = f
        .set("c.txt", "updated\n", Some(&first.hash[..12]))
        .unwrap();
    assert_eq!(ok.previous.as_deref(), Some(first.hash.as_str()));
    assert_eq!(f.read_back("c.txt").as_deref(), Some("updated\n"));

    // A stale base is refused, and the content is still the second
    // write's — not reverted to the first, which is the silent revert
    // this flag exists to prevent.
    let err = f
        .set("c.txt", "reverted\n", Some(&first.hash[..12]))
        .unwrap_err();
    assert!(format!("{err:#}").contains("would revert it"));
    assert_eq!(f.read_back("c.txt").as_deref(), Some("updated\n"));
}

/// A soft-deleted row is revived. Without the un-delete the blob lands
/// and everything filtering on `status = 'active'` skips the file,
/// which is how `set bza flake.nix` once looked like a success and
/// shipped nothing.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_soft_deleted_file_is_revived() {
    let mut f = fixture!("wp-undelete");
    f.set("d.txt", "before\n", None).unwrap();
    {
        let mut tx = f.writer.transaction().unwrap();
        tx.execute(
            "UPDATE project_files SET status = 'deleted'
              WHERE project_id = $1 AND file_path = $2",
            &[&f.project_id, &"d.txt"],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    // The read layer no longer sees it.
    assert!(f.read_back("d.txt").is_none());

    let out = f.set("d.txt", "after\n", None).unwrap();
    assert!(out.undeleted);
    assert!(!out.created);
    assert_eq!(f.read_back("d.txt").as_deref(), Some("after\n"), "revived");
}

/// Content containing a NUL survives the write and reads back intact.
///
/// This is the one that ties the write path to `pgtext`: the escape has
/// only ever been exercised by `mirror`, and a writer that inserted raw
/// text would be rejected by the server for the same reason the mirror
/// was. One live file does this, so it is not hypothetical.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn content_with_a_nul_round_trips_through_the_write_path() {
    let mut f = fixture!("wp-nul");
    let content = "parts.join('\0')\n";
    let out = f.set("e.ts", content, None).unwrap();
    // The hash is of the ORIGINAL bytes, not the escaped form —
    // otherwise `file cat`'s sha256 would not match the blob's key.
    assert_eq!(out.hash, templedb_db::content_hash(content));

    let text = f.read_back("e.ts").expect("should read back");
    assert_eq!(text, content);
    assert!(text.contains('\0'));
    // Size is the original byte count, not the escaped one.
    assert_eq!(f.stats("e.ts").unwrap().0, content.len() as i64);
}

/// `--verify` passes after a real write and fails when the row moved
/// underneath it. The second half needs the row changed *by something
/// else*, which is the only situation the flag exists for.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn verify_passes_after_a_write_and_catches_a_revert() {
    let mut f = fixture!("wp-verify");
    let out = f.set("v.txt", "written\n", None).unwrap();

    // A fresh transaction, as the CLI uses — reading inside the
    // writing one could only ever agree with itself.
    {
        let mut tx = f.writer.transaction().unwrap();
        templedb_db::verify_current(&mut tx, f.project_id, "v.txt", &out.hash)
            .expect("should agree with what was just written");
    }

    // Now revert the row behind the writer's back, which is what a
    // concurrent writer or a trigger would do.
    {
        let mut tx = f.writer.transaction().unwrap();
        tx.execute(
            "UPDATE file_contents SET content_hash = $1
              WHERE file_id = (SELECT id FROM project_files
                                WHERE project_id = $2 AND file_path = $3)
                AND is_current = 1",
            &[
                &templedb_db::content_hash("something else\n"),
                &f.project_id,
                &"v.txt",
            ],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let mut tx = f.writer.transaction().unwrap();
    let err = templedb_db::verify_current(&mut tx, f.project_id, "v.txt", &out.hash)
        .expect_err("should notice the row moved");
    assert_eq!(templedb_db::code_of(&err), templedb_db::NOT_FOUND);
    assert!(format!("{err:#}").contains("--verify failed"));
}

/// The mojibake guard refuses a write that introduces double-encoded
/// UTF-8, leaves the database untouched, and exits 3 — and
/// `allow_mojibake` lets the same content through.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_mojibake_guard_refuses_through_the_write_path() {
    let mut f = fixture!("wp-mojibake");
    f.set("m.tsx", "<button>done</button>\n", None).unwrap();

    // What `perl -0pi -e` did to page.tsx: encode to UTF-8, read the
    // bytes back as Latin-1.
    let corrupted: String = "<button>✓ done</button>\n"
        .as_bytes()
        .iter()
        .map(|&b| b as char)
        .collect();

    let err = f.set("m.tsx", &corrupted, None).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::NO_PATH);
    assert!(format!("{err:#}").contains("double-encoded"));
    // Refused means nothing written, not written-then-complained.
    assert_eq!(
        f.read_back("m.tsx").as_deref(),
        Some("<button>done</button>\n")
    );

    // The escape hatch works, and the content that lands is exactly
    // what was asked for.
    let opts = templedb_db::SetOptions {
        if_match: None,
        allow_mojibake: true,
        ..Default::default()
    };
    let clock = Clock::at("2026-10-21 12:00:00").unwrap();
    let mut tx = f.writer.transaction().unwrap();
    set_file(&mut tx, &clock, f.project_id, "m.tsx", &corrupted, &opts).unwrap();
    tx.commit().unwrap();
    assert_eq!(f.read_back("m.tsx").as_deref(), Some(corrupted.as_str()));

    // And once it is in, an edit that does not *add* any more is
    // allowed — the file has to stay editable.
    let more = format!("{corrupted}// trailing\n");
    f.set("m.tsx", &more, None)
        .expect("an edit that adds no new sequences must pass");
}

/// Empty content is a legal file, with Python's line count of 0.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn empty_content_is_written_with_zero_lines() {
    let mut f = fixture!("wp-empty");
    let out = f.set("empty.txt", "", None).unwrap();
    assert_eq!(out.line_count, 0);
    assert_eq!(out.size_bytes, 0);
    assert_eq!(f.read_back("empty.txt").as_deref(), Some(""));
    assert_eq!(f.stats("empty.txt"), Some((0, 0)));
}

/// Guard five: every write leaves an `edit_intents` row, and the values
/// are Python's.
///
/// `base_revision` is the interesting one. It is `'new-file'` for the
/// first write and the *previous content hash* for the second, which is
/// what makes a stale-base overwrite auditable after the fact — the
/// column held the literal `'current'` on all 886 rows migration 123
/// catalogued, so the field designed to record what an edit was made
/// against recorded nothing at all.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn every_write_records_an_intent_with_pythons_values() {
    let mut f = fixture!("wp-intent");

    let first = f.set("src/a.rs", "one\n", None).unwrap();
    let second = f.set("src/a.rs", "two\n", None).unwrap();

    // Both ids are real and distinct, and the command reports them.
    let (id1, id2) = (
        first.intent_id.expect("first write records an intent"),
        second.intent_id.expect("second write records an intent"),
    );
    assert_ne!(id1, id2);
    assert!(id1 > 0 && id2 > id1, "ids are {id1} and {id2}");

    let rows = f.intents("src/a.rs");
    assert_eq!(rows.len(), 2, "one row per write: {rows:?}");

    // First write: no prior content.
    assert_eq!(rows[0].0, "new-file");
    assert_eq!(rows[0].1, templedb_db::content_hash("one\n"));
    assert_eq!(rows[0].2.as_deref(), Some("4 bytes"));
    assert_eq!(rows[0].3.as_deref(), Some("write-parity"));
    assert_eq!(rows[0].4.as_deref(), Some("file set"));
    assert_eq!(rows[0].5, "applied");
    // No pin, so NULL — which is what the column already permits and
    // what 976 of the 1,220 live rows hold.
    assert_eq!(rows[0].6, None);

    // Second write: the base is the hash the first write left.
    assert_eq!(rows[1].0, templedb_db::content_hash("one\n"));
    assert_eq!(rows[1].1, templedb_db::content_hash("two\n"));
    assert_eq!(rows[1].2.as_deref(), Some("4 bytes"));
}

/// `--skip-intent` writes the file and records nothing. Both halves
/// asserted, because a flag that skipped the write as well would pass
/// the half that only checks the table.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn skip_intent_writes_the_file_and_records_nothing() {
    let mut f = fixture!("wp-skip-intent");
    let clock = Clock::at("2026-10-23 12:00:00").unwrap();
    let opts = templedb_db::SetOptions {
        skip_intent: true,
        ..Default::default()
    };
    let mut tx = f.writer.transaction().unwrap();
    let out = set_file(&mut tx, &clock, f.project_id, "q.rs", "q\n", &opts).unwrap();
    tx.commit().unwrap();

    assert_eq!(out.intent_id, None);
    assert_eq!(f.read_back("q.rs").as_deref(), Some("q\n"));
    assert!(f.intents("q.rs").is_empty());
}

/// `patch_summary` is a *byte* count, not a character count — Python's
/// `len(content_bytes)`. Asserted with multibyte content, which is the
/// only input that tells the two apart.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_patch_summary_counts_bytes_not_characters() {
    let mut f = fixture!("wp-intent-bytes");
    // Four characters, seven bytes: é and ✓ are two and three.
    let content = "aé✓\n";
    assert_eq!(content.chars().count(), 4);
    assert_eq!(content.len(), 7);

    f.set("u.txt", content, None).unwrap();
    assert_eq!(f.intents("u.txt")[0].2.as_deref(), Some("7 bytes"));
}

/// A refused write records no intent, which is the whole reason the
/// insert is inside the transaction.
///
/// Python gets this by *ordering* — intent after write — and its
/// comment records what the other order cost: the 2026-09-14 "intent
/// applied but file_cat returns nothing" bug, where a failed write left
/// a `status = 'applied'` row that survived the abort. Here the
/// rollback covers both tables, so no ordering can produce that row.
/// Written as a test rather than trusted, because "it is in the
/// transaction" is exactly the kind of claim that stops being true
/// silently.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_refused_write_records_no_intent() {
    let mut f = fixture!("wp-intent-refused");
    f.set("r.rs", "base\n", None).unwrap();
    let before = f.intents("r.rs").len();
    assert_eq!(before, 1);

    // A stale `--if-match`, which refuses after `record_intent` would
    // have been reached had it been outside the transaction.
    let err = f.set("r.rs", "new\n", Some("deadbeef")).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::NOT_FOUND);

    assert_eq!(f.read_back("r.rs").as_deref(), Some("base\n"));
    assert_eq!(
        f.intents("r.rs").len(),
        before,
        "a refused write left a provenance row claiming it applied"
    );
}

/// A pinned session is stamped on the intent, and it is the *resolved*
/// one — so an id naming a session that has ended is refused rather
/// than recorded.
///
/// That last half is a deliberate divergence. Python's intent path does
/// not call `get_current_session()`; it reads `TEMPLEDB_SESSION_ID`
/// directly and swallows a parse failure to NULL, so it records dead
/// session ids. This port has one resolver and it checks liveness.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_pinned_session_is_recorded_and_a_dead_one_is_refused() {
    let mut f = fixture!("wp-intent-session");

    // Two sessions this test owns: one live, one ended. Ids well above
    // anything the mirror carries, for the same reason the fixture's
    // project id is.
    let (live, dead) = {
        let mut tx = f.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "INSERT INTO vcs_sessions (id, ended_at) VALUES
                     ((SELECT COALESCE(MAX(id), 0) + 1000 FROM vcs_sessions), NULL)
                 RETURNING id",
                &[],
            )
            .unwrap();
        let live: i64 = row.get(0);
        let row = tx
            .query_one(
                "INSERT INTO vcs_sessions (id, ended_at)
                 VALUES ($1, '2026-10-07 12:00:00') RETURNING id",
                &[&(live + 1)],
            )
            .unwrap();
        let dead: i64 = row.get(0);
        tx.commit().unwrap();
        (live, dead)
    };

    // The live one resolves and lands on the intent.
    let session = {
        let mut tx = f.writer.transaction().unwrap();
        let s = templedb_db::resolve_session_tx(&mut tx, Some(&live.to_string())).unwrap();
        tx.commit().unwrap();
        s
    };
    assert_eq!(session.map(|s| s.get()), Some(live));

    let clock = Clock::at("2026-10-23 12:00:00").unwrap();
    let opts = templedb_db::SetOptions {
        session,
        author: Some("write-parity"),
        ..Default::default()
    };
    let mut tx = f.writer.transaction().unwrap();
    set_file(&mut tx, &clock, f.project_id, "s.rs", "s\n", &opts).unwrap();
    tx.commit().unwrap();
    assert_eq!(f.intents("s.rs")[0].6, Some(live));

    // The ended one never becomes a SessionId at all, so it cannot
    // reach the column.
    let mut tx = f.writer.transaction().unwrap();
    let err = templedb_db::resolve_session_tx(&mut tx, Some(&dead.to_string())).unwrap_err();
    drop(tx);
    assert!(
        format!("{err}").contains("references no live session"),
        "{err}"
    );

    // Clean up the two session rows; `Fixture`'s `Drop` only takes the
    // project. In a block so the borrow of `f.writer` ends before `f`
    // itself is dropped — an `if let` here holds it one scope too long
    // and the destructor cannot run.
    //
    // `unwrap`, not a swallowed error: a session row this test leaves
    // behind is a row the *next* run of it collides with, and failing
    // loudly here is how that gets noticed.
    {
        let mut tx = f.writer.transaction().unwrap();
        tx.execute(
            "DELETE FROM vcs_sessions WHERE id IN ($1, $2)",
            &[&live, &dead],
        )
        .unwrap();
        tx.commit().unwrap();
    }
}

// --- the auto-stage ---------------------------------------------------
//
// Guard six, and the one with no cross-read available: no ported command
// *reads* `vcs_working_state` except `vcs commit-staged`, which consumes
// and then clears it. So these tests read the row with SQL and the
// commit test below is what verifies it through a reader — which is the
// honest division, and the same one `vcs_file_states.file_path` gets.

/// A write with a session staged, in the state and on the branch the
/// commit will look for.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_write_with_a_session_stages_the_new_hash() {
    let mut f = fixture!("wp-stage");
    let branch_id = f.branch("main");
    let (sid, session) = f.session();

    let out = f
        .set_as("src/a.rs", "fn a() {}\n", Some(session), true)
        .unwrap();
    assert_eq!(
        out.staged,
        templedb_db::Staged::Yes {
            branch: "main".to_string(),
            state: "added",
        }
    );

    let file_id = f.file_id("src/a.rs");
    let row = f.staged_row(file_id).expect("the stage must have a row");
    assert_eq!(row.0, branch_id);
    assert_eq!(row.1, "added");
    // The *new* hash, which is the column the commit reads. Python's own
    // comment: "without this, `file set` followed by `vcs commit`
    // silently records the previous content".
    assert_eq!(row.2.as_deref(), Some(out.hash.as_str()));
    assert_eq!(row.3, Some(sid));
    assert_eq!(row.4, STAGE_INSTANT);
    // Migration 088's provenance link, which Python writes in a second
    // statement and this writes in the insert.
    assert_eq!(row.5, out.intent_id);
    assert!(row.5.is_some());
}

/// A second write restages the new hash as `modified` and relinks the
/// intent, so the row's hash and its `intent_id` describe one edit.
///
/// This test asserted the opposite for one commit, reproducing Python's
/// `AND intent_id IS NULL` — which kept the first intent forever. That
/// was reported from here and fixed upstream on 2026-10-27
/// (`templedb` BF4E6BEA6BD93C6E), so the assertion flipped with the
/// workaround.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_restage_updates_the_hash_and_relinks_the_intent() {
    let mut f = fixture!("wp-restage");
    f.branch("main");
    let (_, session) = f.session();

    let first = f.set_as("a.txt", "one\n", Some(session), true).unwrap();
    let second = f.set_as("a.txt", "two\n", Some(session), true).unwrap();
    assert_eq!(
        second.staged,
        templedb_db::Staged::Yes {
            branch: "main".to_string(),
            state: "modified",
        }
    );

    let file_id = f.file_id("a.txt");
    let row = f.staged_row(file_id).unwrap();
    assert_eq!(row.1, "modified");
    assert_eq!(row.2.as_deref(), Some(second.hash.as_str()));
    assert_ne!(first.intent_id, second.intent_id);
    assert_eq!(
        row.5, second.intent_id,
        "the staged row must name the intent whose hash it holds"
    );

    // And the link really is to the newest intent for this path, not
    // merely to a different one — the assertion the first version of
    // this test could not make.
    let intents = f.intents("a.txt");
    assert_eq!(intents.len(), 2);
    assert_eq!(intents[1].1, second.hash, "newest intent's hash");
}

/// Rewriting identical content still stages `modified`. Python asks
/// whether the `project_files` row existed, not whether anything
/// changed, so a no-op write leaves a staged row behind over there too.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_noop_write_still_stages() {
    let mut f = fixture!("wp-stage-noop");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("b.txt", "same\n", Some(session), true).unwrap();
    let again = f.set_as("b.txt", "same\n", Some(session), true).unwrap();
    assert!(again.unchanged);
    assert!(matches!(
        again.staged,
        templedb_db::Staged::Yes {
            state: "modified",
            ..
        }
    ));
}

/// A revived soft-deleted row stages as `modified`, not `added`.
///
/// Python's `get_file_by_path` does not filter on `status`, so the row
/// it found is truthy and the state is `modified` — which is also the
/// honest answer: the file has history, and `added` would tell the
/// commit to record a creation.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_revived_file_stages_as_modified() {
    let mut f = fixture!("wp-stage-revive");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("d.txt", "before\n", Some(session), true).unwrap();
    {
        let mut tx = f.writer.transaction().unwrap();
        tx.execute(
            "UPDATE project_files SET status = 'deleted'
              WHERE project_id = $1 AND file_path = $2",
            &[&f.project_id, &"d.txt"],
        )
        .unwrap();
        tx.commit().unwrap();
    }
    let out = f.set_as("d.txt", "after\n", Some(session), true).unwrap();
    assert!(out.undeleted);
    assert!(matches!(
        out.staged,
        templedb_db::Staged::Yes {
            state: "modified",
            ..
        }
    ));
}

/// With no session the write lands and stages nothing, and the outcome
/// says which of the three reasons it was.
///
/// This is the port's divergence from Python, which would have created a
/// session here and staged. It is reported rather than silent: a write
/// that staged nothing is a write `vcs commit-staged` will not see.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_write_with_no_session_stages_nothing() {
    let mut f = fixture!("wp-stage-nosession");
    f.branch("main");

    let out = f.set_as("a.txt", "one\n", None, false).unwrap();
    assert_eq!(out.staged, templedb_db::Staged::NoSession);
    // The write itself is unaffected: content, blob and intent all land.
    assert_eq!(f.read_back("a.txt").as_deref(), Some("one\n"));
    assert!(out.intent_id.is_some());
    let file_id = f.file_id("a.txt");
    assert_eq!(f.staged_row(file_id), None);
}

/// A project with no active and no default branch stages nothing, and
/// the write still lands. Python's `if branch:` does the same.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_project_with_no_branch_stages_nothing() {
    let mut f = fixture!("wp-stage-nobranch");
    let (_, session) = f.session();

    let out = f.set_as("a.txt", "one\n", Some(session), false).unwrap();
    assert_eq!(out.staged, templedb_db::Staged::NoBranch);
    assert_eq!(f.read_back("a.txt").as_deref(), Some("one\n"));
}

/// `--stage` with no session refuses, and refuses **before** writing
/// anything — no `project_files` row, no blob reference, no intent.
///
/// The ordering is the test. A flag that asserts an effect has to fail
/// before the effects it cannot have, which is the same property
/// `--if-match` has and the reason both are checked ahead of the write.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_stage_flag_refuses_without_a_session_and_writes_nothing() {
    let mut f = fixture!("wp-stage-refuse");
    f.branch("main");

    let err = f.set_as("a.txt", "one\n", None, true).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::USAGE);
    let msg = format!("{err:#}");
    assert!(msg.contains("--stage needs a session"), "{msg}");
    assert!(msg.contains("unstaged"), "{msg}");
    assert!(msg.contains("TEMPLEDB_SESSION_ID"), "{msg}");

    assert!(!f.file_row_exists("a.txt"), "the refusal wrote a file row");
    assert!(f.intents("a.txt").is_empty());
}

/// `--stage` with no *branch* is not a refusal, which is the asymmetry
/// worth pinning: the flag asserts that a session owns the write, and a
/// project with no branch is a state of the database rather than a
/// missing argument. Python skips its stage here too.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_stage_flag_tolerates_a_project_with_no_branch() {
    let mut f = fixture!("wp-stage-flag-nobranch");
    let (_, session) = f.session();

    let out = f.set_as("a.txt", "one\n", Some(session), true).unwrap();
    assert_eq!(out.staged, templedb_db::Staged::NoBranch);
}

/// Two sessions staging the same file on the same branch: the row is
/// `UNIQUE (project_id, branch_id, file_id)`, so the second write takes
/// the row over rather than adding one.
///
/// Worth a test because it is the footgun the commit's
/// `other_sessions_staged` line exists for, one step earlier — an agent
/// whose two tool calls get two session leaders does not end up with two
/// staged rows, it ends up with one owned by whichever wrote last.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_session_takes_the_staged_row_over() {
    let mut f = fixture!("wp-stage-steal");
    f.branch("main");
    let (_, first) = f.session();
    let (second_id, second) = f.session();

    f.set_as("a.txt", "one\n", Some(first), true).unwrap();
    let out = f.set_as("a.txt", "two\n", Some(second), true).unwrap();

    let file_id = f.file_id("a.txt");
    let row = f.staged_row(file_id).unwrap();
    assert_eq!(row.3, Some(second_id));
    assert_eq!(row.2.as_deref(), Some(out.hash.as_str()));
    // One row, not two.
    let mut tx = f.writer.transaction().unwrap();
    let count: i64 = tx
        .query_one(
            "SELECT COUNT(*) FROM vcs_working_state WHERE project_id = $1",
            &[&f.project_id],
        )
        .unwrap()
        .get(0);
    tx.commit().unwrap();
    assert_eq!(count, 1);
}

// --- vcs commit-staged ------------------------------------------------
//
// The cross-read for the commit path is a stronger one than `file set`'s,
// because three *different* already-verified readers can see the result:
// `vcs log` lists the commit, `vcs show` lists its changed files, and
// `file cat` reads the content it made current. A commit none of them can
// see did not happen.
//
// What cannot be tested here is the half that was refused. There is no
// workspace, so there is nothing to scan and no conflict to detect —
// which is the point of the command's name.

/// The loop, closed: `file set --stage` produces the staged row and
/// `vcs commit-staged` consumes it, with no SQL in between.
///
/// Every other test in this section builds its staged row with
/// [`Fixture::stage`], because until the auto-stage landed the port
/// could consume `vcs_working_state` and not produce it — so the two
/// halves were verified one at a time against a hand-written fixture
/// standing in for the other. This is the one test where both ends are
/// the port's own code, which is the whole point of porting the stage:
/// a hand-built row proves the commit reads what the *test* wrote, and
/// only this proves it reads what `file set` wrote.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_loop_closes_when_file_set_stages_its_own_commit() {
    let mut f = fixture!("wp-loop");
    let branch_id = f.branch("main");
    let (sid, session) = f.session();

    // One write, staged by the command rather than by the fixture.
    let out = f
        .set_as("src/lib.rs", "pub fn go() {}\n", Some(session), true)
        .unwrap();
    assert!(matches!(
        out.staged,
        templedb_db::Staged::Yes { state: "added", .. }
    ));

    let outcome = f
        .commit("the first write of a file", None, session)
        .unwrap();
    assert_eq!(outcome.files, 1);
    assert_eq!(outcome.deletions, 0);
    assert_eq!(outcome.other_sessions_staged, 0);
    assert_eq!(outcome.previous_head, None);

    // Through the three readers the sweep already diffs against Python.
    let log = f.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].commit_hash, outcome.hash);
    assert_eq!(log[0].commit_message, "the first write of a file");

    let changed = f.changed(&outcome.hash);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].file_path, "src/lib.rs");
    assert_eq!(changed[0].change_type, "added");

    assert_eq!(
        f.read_back("src/lib.rs").as_deref(),
        Some("pub fn go() {}\n")
    );

    // The content the commit recorded is the hash `file set` staged —
    // the column Python's comment warns about, checked from the far end.
    let file_id = f.file_id("src/lib.rs");
    let states = f.file_states(outcome.commit_id);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].2.as_deref(), Some(out.hash.as_str()));

    // And the stage is spent: unstaged, owned by nobody, on the branch
    // it was staged against.
    assert_eq!(f.head(branch_id), Some(outcome.commit_id));
    assert_eq!(
        f.working_state(file_id),
        Some(("unmodified".to_string(), None))
    );
    assert_eq!(f.staged_row(file_id).unwrap().0, branch_id);
    // The session that staged it is the session that committed it,
    // which is the pairing `--session` exists to make possible.
    assert_eq!(sid, session.get());
}

/// The whole path, end to end: stage a modified file, commit it, and see
/// the commit through three readers.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_commit_is_visible_to_vcs_log_vcs_show_and_file_cat() {
    let mut f = fixture!("wp-commit");
    let branch = f.branch("main");
    let (_, session) = f.session();

    // The file as the database holds it, then the file as the workspace
    // scan saw it. The second hash is what gets committed: it is the
    // staged hash, and the difference between the two is the entire
    // content of a `modified` state.
    f.set("src/main.rs", "fn main() {}\n", None).unwrap();
    let staged = f.set("src/main.rs", "fn main() { go() }\n", None).unwrap();
    let file_id = f.file_id("src/main.rs");
    f.stage(
        branch,
        file_id,
        "modified",
        Some(&staged.hash),
        Some(session.get()),
    );

    let out = f.commit("tighten main", None, session).unwrap();
    assert_eq!(out.files, 1);
    assert_eq!(out.deletions, 0);
    assert_eq!(out.branch_name, "main");
    assert_eq!(out.previous_head, None, "the branch had no commits");
    assert_eq!(out.other_sessions_staged, 0);
    assert_eq!(out.reaped, 0);
    // The id is a function of the pinned clock and the four fields, so
    // it is assertable rather than merely well-formed.
    assert_eq!(
        out.hash,
        templedb_db::commit_hash(
            "wp-commit",
            "main",
            "tighten main",
            Clock::at("2026-10-25 12:00:00").unwrap().unix_seconds()
        )
    );

    // Reader one: `vcs log`. The commit lands published — Python's
    // `vcs commit` leaves `session_id` NULL too — so the *published*
    // view sees it, which is the view that needs no session at all.
    let log = f.log();
    assert_eq!(log.len(), 1, "{log:?}");
    assert_eq!(log[0].commit_hash, out.hash);
    assert_eq!(log[0].branch_name, "main");
    assert_eq!(log[0].author, "write-parity");
    assert_eq!(log[0].commit_message, "tighten main");
    assert_eq!(log[0].commit_timestamp, "2026-10-25 12:00:00");
    assert_eq!(log[0].session_id, None, "no session means published");

    // Reader two: `vcs show`'s changed-file list.
    let changed = f.changed(&out.hash);
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].file_path, "src/main.rs");
    assert_eq!(changed[0].change_type, "modified");
    assert_eq!(changed[0].file_size, Some(19));
    assert_eq!(changed[0].line_count, Some(2));

    // Reader three: `file cat`, which sees the content the commit made
    // current.
    assert_eq!(
        f.read_back("src/main.rs").as_deref(),
        Some("fn main() { go() }\n")
    );

    // And the branch head, which no trigger advanced.
    assert_eq!(f.head(branch), Some(out.commit_id));

    // Unstaged, in place: the row survives with the flag cleared, which
    // is what `NULL means not staged` has to mean.
    assert_eq!(
        f.working_state(file_id),
        Some(("unmodified".to_string(), None))
    );
}

/// The branch head advances, and a commit built on a head that has since
/// moved is refused rather than overwriting it.
///
/// This is the test the design document asks for — "two commits racing
/// on one branch" — and it is a real race rather than a simulated one:
/// two transactions on two connections, the second holding a head the
/// first has already replaced. It stays deterministic because the first
/// transaction *commits* before the second's `UPDATE` runs, so nothing
/// blocks; under `READ COMMITTED` the CAS predicate is then evaluated
/// against the new row version and matches nothing.
///
/// Python cannot have this check. Its trigger writes the column during
/// the `INSERT`, so an app-level compare-and-swap over there always
/// found the new value already in place and reported `HEAD_RACE` on
/// every commit — the false positive its comment records.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_branch_head_advances_and_a_racing_commit_is_refused() {
    let mut f = fixture!("wp-commit-race");
    let branch = f.branch("main");
    let (_, session) = f.session();

    let a = f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    f.stage(
        branch,
        file_id,
        "modified",
        Some(&a.hash),
        Some(session.get()),
    );

    // A second connection, which is what makes this a race and not a
    // sequence. Its transaction opens *before* the first commit lands
    // and reads the head it then builds on.
    let url = scratch_url().unwrap();
    let mut other = PgWriter::connect(&url).unwrap();
    let mut stale_tx = other.transaction().unwrap();
    let stale_head = templedb_db::branch_head(&mut stale_tx, branch).unwrap();
    assert_eq!(stale_head, None, "the branch starts empty");

    // Session A commits. The head moves.
    let first = f.commit("first", None, session).unwrap();
    assert_eq!(f.head(branch), Some(first.commit_id));

    // Session B now tries to advance from the head it read. The branch
    // no longer points there, so the CAS matches no row and refuses.
    let err = templedb_db::advance_branch_head(&mut stale_tx, branch, stale_head, 999_999)
        .expect_err("a stale head must not be overwritten");
    drop(stale_tx);
    assert_eq!(templedb_db::code_of(&err), templedb_db::NOT_FOUND);
    let msg = format!("{err:#}");
    assert!(msg.contains("HEAD_RACE"), "{msg}");
    assert!(msg.contains("Nothing has been written"), "{msg}");

    // And the head is still the first commit's: the refusal did not
    // half-apply.
    assert_eq!(f.head(branch), Some(first.commit_id));
}

/// A second commit records its parent and advances the head from it.
///
/// The parent edge goes in `vcs_commit_parents` and nowhere else.
/// `vcs_commits.parent_commit_id` is not in the mirrored subset — no
/// reader names it, and Python's own `create_commit` docstring records
/// the column drifting out of sync with this table when `vcs merge`
/// wrote the rows and not the column.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_commit_records_its_parent_and_moves_the_head() {
    let mut f = fixture!("wp-commit-parent");
    let branch = f.branch("main");
    let (_, session) = f.session();

    let one = f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    f.stage(
        branch,
        file_id,
        "modified",
        Some(&one.hash),
        Some(session.get()),
    );
    let first = f.commit("first", None, session).unwrap();
    assert!(f.parents(first.commit_id).is_empty(), "a root commit");

    // Stage again. The row is still there, unstaged — so re-staging is
    // an UPDATE, which is what the real `file set --stage` will do.
    let two = f.set("a.txt", "two\n", None).unwrap();
    {
        let mut tx = f.writer.transaction().unwrap();
        tx.execute(
            "UPDATE vcs_working_state
                SET state = 'modified', content_hash = $1, staged_by_session_id = $2
              WHERE project_id = $3 AND file_id = $4",
            &[&two.hash, &session.get(), &f.project_id, &file_id],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    let second = f.commit("second", None, session).unwrap();
    assert_eq!(second.previous_head, Some(first.commit_id));
    assert_eq!(f.parents(second.commit_id), vec![first.commit_id]);
    assert_eq!(f.head(branch), Some(second.commit_id));
    // The ids differ even though the clock is pinned, because the
    // message is in the hash input.
    assert_ne!(first.hash, second.hash);

    // Both commits are in the log — and in *this* order, which is the
    // tie-break rather than an accident. A pinned clock gives the two
    // the same `commit_timestamp`, and `vcs log` orders by
    // `commit_timestamp DESC, id ASC`, so the older id comes first. The
    // README's *The tie-break, and getting it backwards first* is about
    // exactly this ordering; a write test is the only place it can be
    // exercised deliberately, because live data rarely has two commits
    // in the same second.
    let log = f.log();
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].commit_timestamp, log[1].commit_timestamp);
    assert_eq!(log[0].commit_message, "first", "id ASC breaks the tie");
    assert_eq!(log[1].commit_message, "second");
}

/// A committed deletion hard-deletes the `project_files` row and keeps
/// the path in `vcs_file_states`.
///
/// This is the only path in the system that hard-deletes that row —
/// everything else soft-deletes by flipping `status` — which is why the
/// denormalised `file_path` has to be written: without it the state row
/// is left pointing at a `file_id` that resolves to nothing, and nothing
/// records which file it was.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_deletion_hard_deletes_the_file_and_keeps_its_path() {
    let mut f = fixture!("wp-commit-delete");
    let branch = f.branch("main");
    let (_, session) = f.session();

    f.set("doomed.txt", "bye\n", None).unwrap();
    let file_id = f.file_id("doomed.txt");
    // A deletion stages with no content hash, which is the one state
    // where that is honest — guard 3 exists for all the others.
    f.stage(branch, file_id, "deleted", None, Some(session.get()));

    let out = f.commit("drop it", None, session).unwrap();
    assert_eq!(out.files, 1);
    assert_eq!(out.deletions, 1);

    // The sentinel, and the path that outlives the row.
    let states = f.file_states(out.commit_id);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0.as_deref(), Some("doomed.txt"));
    assert_eq!(states[0].1, "deleted");
    assert_eq!(states[0].2.as_deref(), Some("DELETED"));

    // The file is gone, not soft-deleted: `file cat` cannot see it and
    // neither can a direct lookup.
    assert!(f.read_back("doomed.txt").is_none());
    {
        let mut tx = f.writer.transaction().unwrap();
        let row = tx
            .query_opt(
                "SELECT id FROM project_files WHERE project_id = $1 AND file_path = $2",
                &[&f.project_id, &"doomed.txt"],
            )
            .unwrap();
        tx.commit().unwrap();
        assert!(row.is_none(), "the row should be hard-deleted, not flipped");
    }
    // And its working state went with it, rather than being unstaged.
    assert_eq!(f.working_state(file_id), None);

    // `vcs show` can still list the change, because it joins
    // `project_files`... and that row is gone, so it cannot. Asserted
    // rather than wished for: this is exactly the hole `file_path`
    // exists to record, and it is recorded in the row above even though
    // no reader reaches for it yet.
    assert!(
        f.changed(&out.hash).is_empty(),
        "the reader inner-joins project_files, so a hard-deleted file \
         drops out of the list — the path survives only in the column"
    );
}

/// Nothing staged is exit 1 and a message that names the session, and it
/// reports rows staged in *other* sessions rather than saying nothing.
///
/// That last part is Python's and it is the footgun worth keeping: an
/// agent stages in one shell and commits in another, every tool call
/// getting its own session leader, so "nothing staged" is the most
/// misleading true statement the command can make.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn nothing_staged_refuses_and_points_at_the_other_session() {
    let mut f = fixture!("wp-commit-empty");
    let branch = f.branch("main");
    let (_, mine) = f.session();
    let (theirs_id, _) = f.session();

    f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    f.stage(branch, file_id, "modified", Some("abc"), Some(theirs_id));

    let err = f.commit("nothing of mine", None, mine).unwrap_err();
    // Exit 1, following git: non-zero when nothing was committed,
    // including when there was simply nothing to commit.
    assert_eq!(templedb_db::code_of(&err), templedb_db::USAGE);
    let msg = format!("{err:#}");
    assert!(msg.contains("nothing staged"), "{msg}");
    assert!(msg.contains(&mine.to_string()), "{msg}");
    assert!(
        msg.contains("1 file(s) are staged in other sessions"),
        "{msg}"
    );

    // The other session's row is untouched, which is the whole point of
    // the column.
    assert_eq!(
        f.working_state(file_id),
        Some(("modified".to_string(), Some(theirs_id)))
    );
    assert!(f.log().is_empty(), "no commit should exist");
}

/// Another session's staged rows survive a commit that ran beside them.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn another_sessions_staged_rows_survive_the_commit() {
    let mut f = fixture!("wp-commit-isolation");
    let branch = f.branch("main");
    let (_, mine) = f.session();
    let (theirs_id, _) = f.session();

    let a = f.set("mine.txt", "mine\n", None).unwrap();
    let b = f.set("theirs.txt", "theirs\n", None).unwrap();
    let mine_id = f.file_id("mine.txt");
    let theirs_file = f.file_id("theirs.txt");
    f.stage(branch, mine_id, "modified", Some(&a.hash), Some(mine.get()));
    f.stage(
        branch,
        theirs_file,
        "modified",
        Some(&b.hash),
        Some(theirs_id),
    );

    let out = f.commit("just mine", None, mine).unwrap();
    assert_eq!(out.files, 1, "only this session's row");
    assert_eq!(out.other_sessions_staged, 1);
    assert_eq!(f.changed(&out.hash).len(), 1);
    assert_eq!(f.changed(&out.hash)[0].file_path, "mine.txt");

    assert_eq!(
        f.working_state(mine_id),
        Some(("unmodified".to_string(), None))
    );
    assert_eq!(
        f.working_state(theirs_file),
        Some(("modified".to_string(), Some(theirs_id))),
        "the other session's staging must not be cleared"
    );
}

/// Guard 3: a staged non-deleted file with no content hash is refused,
/// and the refusal leaves nothing behind.
///
/// The second half is what makes it a guard. Python checks inside the
/// per-file loop, after the commit row has been inserted, and relies on
/// the rollback; this checks the whole set first, so the commit row is
/// never minted at all. Either way the test is the same: no commit, and
/// the staging still staged.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_staged_file_with_no_hash_is_refused_and_writes_nothing() {
    let mut f = fixture!("wp-commit-guard3");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    // 'modified' with no hash: the row 124 live `vcs_file_states` rows
    // were written from before the guard existed.
    f.stage(branch, file_id, "modified", None, Some(session_id));

    let err = f.commit("should refuse", None, session).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::NO_PATH);
    let msg = format!("{err:#}");
    assert!(msg.contains("a.txt"), "{msg}");
    assert!(msg.contains("DELETED"), "{msg}");

    assert!(f.log().is_empty(), "no commit row");
    assert_eq!(f.head(branch), None, "the head did not move");
    assert_eq!(
        f.working_state(file_id),
        Some(("modified".to_string(), Some(session_id))),
        "a refused commit must leave the staging intact"
    );
}

/// A staged hash with no blob is refused, where Python reads the file
/// off disk.
///
/// This is the tree-dependent half, refused by name. Python resolves the
/// path, joins it onto `~/.config/templedb/checkouts/<slug>`, reads the
/// bytes and stores the blob — the mirror-to-disk dependency the design
/// document says settles whether the command can be ported at all.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_staged_hash_with_no_blob_is_refused_rather_than_read_from_disk() {
    let mut f = fixture!("wp-commit-noblob");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    // A hash no blob was ever stored under — what a workspace scan
    // leaves when the content was never ingested.
    let phantom = templedb_db::content_hash("never stored anywhere\n");
    f.stage(
        branch,
        file_id,
        "modified",
        Some(&phantom),
        Some(session_id),
    );

    let err = f.commit("should refuse", None, session).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::NO_PATH);
    let msg = format!("{err:#}");
    assert!(msg.contains("names no blob"), "{msg}");
    assert!(msg.contains("no checkout"), "{msg}");
    assert!(msg.contains(&phantom[..12]), "{msg}");

    assert!(f.log().is_empty());
    assert_eq!(f.head(branch), None);
}

/// Guard 2: working-state rows pointing at a reaped `project_files` row
/// are deleted rather than aborting the commit on a foreign key.
///
/// `sync_engine` and some migrations bulk-delete with FK enforcement
/// off, so these rows are a state the live database reaches. On
/// 2026-09-19 one of them aborted a whole commit.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn orphaned_working_state_rows_are_reaped_instead_of_aborting() {
    let mut f = fixture!("wp-commit-reap");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    let a = f.set("real.txt", "real\n", None).unwrap();
    let real = f.file_id("real.txt");
    f.stage(branch, real, "modified", Some(&a.hash), Some(session_id));
    // A staged row for a file that does not exist. `vcs_working_state`
    // declares no FK on `file_id` in this subset precisely so the
    // mirror can carry rows like it.
    f.stage(
        branch,
        9_999_999,
        "modified",
        Some(&a.hash),
        Some(session_id),
    );

    let out = f.commit("one real file", None, session).unwrap();
    assert_eq!(out.reaped, 1, "the dangling row should be reaped");
    assert_eq!(out.files, 1, "and not committed");
    assert_eq!(f.changed(&out.hash).len(), 1);
    assert_eq!(f.working_state(9_999_999), None);
}

/// A named branch is committed to, and a name that does not exist is
/// refused rather than falling back to the active one.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn an_explicit_branch_is_honoured_and_an_unknown_one_refuses() {
    let mut f = fixture!("wp-commit-branch");
    let main = f.branch("main");
    let (session_id, session) = f.session();

    // A second branch, not the active one.
    let dev = {
        let mut tx = f.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "INSERT INTO vcs_branches (id, project_id, branch_name, is_default)
                 VALUES ((SELECT COALESCE(MAX(id), 0) + 1000 FROM vcs_branches), $1, 'dev', 0)
                 RETURNING id",
                &[&f.project_id],
            )
            .unwrap();
        let id: i64 = row.get(0);
        tx.commit().unwrap();
        id
    };

    let a = f.set("a.txt", "one\n", None).unwrap();
    let file_id = f.file_id("a.txt");
    // Staged on `dev`, which is the branch the commit has to be told
    // about: the staged lookup is scoped to (project, branch, session).
    f.stage(dev, file_id, "modified", Some(&a.hash), Some(session_id));

    // Without `--branch` it resolves `main`, where nothing is staged.
    let err = f.commit("wrong branch", None, session).unwrap_err();
    assert!(format!("{err:#}").contains("nothing staged"), "{err:#}");

    let out = f.commit("right branch", Some("dev"), session).unwrap();
    assert_eq!(out.branch_name, "dev");
    assert_eq!(f.head(dev), Some(out.commit_id));
    assert_eq!(f.head(main), None, "the active branch did not move");

    let err = f.commit("nowhere", Some("nope"), session).unwrap_err();
    assert!(format!("{err:#}").contains("no branch \"nope\""), "{err:#}");
}

/// An `added` state records the blob's own numbers, not the current
/// revision's — and an empty blob yields Python's NULL line count.
///
/// Python re-derives both from `content_blobs` rather than trusting the
/// `file_contents` join, and it has to: a modified file's staged hash is
/// by definition not the current one. The empty case is the quirk —
/// `if content_text` is false for `''`, so it falls to the binary branch
/// for the size and leaves the line count NULL rather than 0.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_recorded_size_and_lines_come_from_the_staged_blob() {
    let mut f = fixture!("wp-commit-metrics");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    // Multibyte, so a byte count and a character count differ: four
    // characters, seven bytes.
    let content = "aé✓\n";
    assert_eq!(content.chars().count(), 4);
    assert_eq!(content.len(), 7);
    let wide = f.set("wide.txt", content, None).unwrap();
    let empty = f.set("empty.txt", "", Some("deadbeef")).unwrap_err();
    // `--if-match` on a new file refuses, so create it the plain way.
    assert!(format!("{empty:#}").contains("no current content"));
    let empty = {
        let clock = Clock::at("2026-10-25 11:00:00").unwrap();
        let mut tx = f.writer.transaction().unwrap();
        let o = templedb_db::set_file(
            &mut tx,
            &clock,
            f.project_id,
            "empty.txt",
            "",
            &templedb_db::SetOptions::default(),
        )
        .unwrap();
        tx.commit().unwrap();
        o
    };

    let wide_id = f.file_id("wide.txt");
    let empty_id = f.file_id("empty.txt");
    f.stage(branch, wide_id, "added", Some(&wide.hash), Some(session_id));
    f.stage(
        branch,
        empty_id,
        "added",
        Some(&empty.hash),
        Some(session_id),
    );

    let out = f.commit("metrics", None, session).unwrap();
    let states = f.file_states(out.commit_id);
    assert_eq!(states.len(), 2);

    let wide_row = states
        .iter()
        .find(|s| s.0.as_deref() == Some("wide.txt"))
        .unwrap();
    assert_eq!(wide_row.3, Some(7), "bytes, not characters");
    assert_eq!(wide_row.4, Some(2), "Python counts separators and adds one");

    let empty_row = states
        .iter()
        .find(|s| s.0.as_deref() == Some("empty.txt"))
        .unwrap();
    assert_eq!(empty_row.3, Some(0));
    assert_eq!(
        empty_row.4, None,
        "an empty content_text is falsy over there, so the line count is NULL"
    );
}

/// Content containing a NUL survives being committed, with the size
/// measured on the real bytes rather than the escaped form.
///
/// The same seam `file set`'s NUL test covers, one layer along: the
/// staged blob is read back through `pgtext::unescape` before it is
/// measured, and skipping that would overcount every file holding a NUL.
/// One live file does.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn committed_content_with_a_nul_is_measured_on_the_real_bytes() {
    let mut f = fixture!("wp-commit-nul");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    let content = "parts.join('\0')\n";
    let out = f.set("e.ts", content, None).unwrap();
    let file_id = f.file_id("e.ts");
    f.stage(branch, file_id, "added", Some(&out.hash), Some(session_id));

    let commit = f.commit("nul", None, session).unwrap();
    let states = f.file_states(commit.commit_id);
    assert_eq!(states[0].3, Some(content.len() as i64), "unescaped length");
    assert_eq!(f.read_back("e.ts").as_deref(), Some(content));
}

// --- binary content, and `file set --commit` ---------------------------

/// The two things `file set` gained on 2026-10-29, and the helpers they
/// need.
///
/// `set_then_commit` is the only place in this file that reproduces a
/// *CLI* composition rather than calling one library function. That is
/// deliberate and it is the thing under test: `--commit` is not a new
/// query, it is `set_file` and `commit_staged` sharing one transaction,
/// so a test that ran them on two transactions would be testing the
/// arrangement the flag exists to avoid.
impl Fixture {
    /// Write bytes that are not text.
    fn set_binary(
        &mut self,
        path: &str,
        bytes: &[u8],
        session: Option<templedb_db::SessionId>,
    ) -> anyhow::Result<templedb_db::SetOutcome> {
        let clock = Clock::at(STAGE_INSTANT)?;
        let mut tx = self.writer.transaction()?;
        let opts = templedb_db::SetOptions {
            session,
            author: Some("write-parity"),
            ..Default::default()
        };
        let outcome = set_file(
            &mut tx,
            &clock,
            self.project_id,
            path,
            templedb_db::Content::Binary(bytes),
            &opts,
        );
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                drop(tx);
                Err(e)
            }
        }
    }

    /// `(content_type, content_text, content_blob, file_size_bytes)`
    /// for one hash, read through [`Store`].
    #[allow(clippy::type_complexity)]
    fn blob(&self, hash: &str) -> Option<(String, Option<String>, Option<Vec<u8>>, i64)> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        let row = store
            .query_opt(
                "SELECT content_type, content_text, content_blob, file_size_bytes
                   FROM content_blobs WHERE hash_sha256 = ?1",
                &[Value::from(hash)],
            )
            .unwrap()?;
        Some((
            row.get_string(0).unwrap(),
            row.get_opt_string(1).unwrap(),
            row.get_opt_blob(2).unwrap(),
            row.get_i64(3).unwrap(),
        ))
    }

    /// `file set --commit`: the write and the commit in **one**
    /// transaction, which is what the CLI does.
    fn set_then_commit(
        &mut self,
        path: &str,
        content: &str,
        session: templedb_db::SessionId,
        message: &str,
    ) -> anyhow::Result<(templedb_db::SetOutcome, templedb_db::CommitOutcome)> {
        let clock = Clock::at(STAGE_INSTANT)?;
        let slug = self.slug.clone();
        let project_id = self.project_id;
        let mut tx = self.writer.transaction()?;
        let opts = templedb_db::SetOptions {
            session: Some(session),
            // `--commit` implies `--stage`, as Python's does.
            stage: true,
            author: Some("write-parity"),
            ..Default::default()
        };
        let result = (|| {
            let set = set_file(&mut tx, &clock, project_id, path, content, &opts)?;
            let commit = templedb_db::commit_staged(
                &mut tx,
                &clock,
                project_id,
                &slug,
                &templedb_db::CommitOptions {
                    message,
                    author: "write-parity",
                    branch: None,
                    session,
                },
            )?;
            Ok::<_, anyhow::Error>((set, commit))
        })();
        match result {
            Ok(pair) => {
                tx.commit()?;
                Ok(pair)
            }
            Err(e) => {
                // The atomicity under test: a failing commit takes the
                // write with it.
                drop(tx);
                Err(e)
            }
        }
    }
}

/// Bytes that are not UTF-8 land in `content_blob` as a
/// `binary_asset`, with a NULL `content_text` and no invented line
/// count.
///
/// The shape matters beyond this command: Python's scanner writes the
/// same four columns for the same file, so a blob written here and one
/// written by an import have to be indistinguishable downstream. A test
/// that only checked the round trip would pass with `content_type =
/// 'text'`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn binary_content_lands_in_the_blob_column_as_a_binary_asset() {
    let mut f = fixture!("wp-binary");
    // A lone 0x84 continuation byte — the one Python's incident report
    // names — plus a NUL and two newlines, so the line count has
    // something to count if it were going to.
    let raw: &[u8] = &[0x84, 0x00, 0xff, b'\n', 0xfe, b'\n'];
    // Owned copy, for the reason `fileset.rs`'s equivalent gives:
    // clippy rejects the literal form as a call that always fails,
    // which is the assertion.
    assert!(
        String::from_utf8(raw.to_vec()).is_err(),
        "must not be decodable"
    );

    let out = f.set_binary("assets/key.gpg", raw, None).unwrap();
    assert_eq!(out.size_bytes, 6);
    assert_eq!(out.line_count, 0, "0, not a count of 0x0a");
    assert!(out.created);

    let (kind, text, blob, size) = f.blob(&out.hash).expect("the blob row");
    assert_eq!(kind, "binary_asset");
    assert_eq!(text, None, "content_text must be NULL, not empty");
    assert_eq!(blob.as_deref(), Some(raw), "the bytes, unescaped");
    assert_eq!(size, 6);

    // The recorded stats, through the columns `file ls -l` prints.
    assert_eq!(f.stats("assets/key.gpg"), Some((6, 0)));

    // And `file cat` refuses it rather than printing nothing, which is
    // Python's behaviour and the reason read_file has that branch at
    // all. The row exists; it just has no text.
    assert_eq!(f.read_back("assets/key.gpg"), None);
    assert!(f.file_row_exists("assets/key.gpg"));
}

/// The hash is over the raw bytes, so it matches `sha256sum` of the
/// same file — the property every other `file set` surface relies on.
///
/// Asserted against a value computed outside this program: the sha256
/// of the four bytes `84 00 ff 0a`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_binary_hash_is_sha256_of_the_bytes_on_disk() {
    let mut f = fixture!("wp-binary-hash");
    let raw: &[u8] = &[0x84, 0x00, 0xff, 0x0a];
    let out = f.set_binary("a.bin", raw, None).unwrap();
    assert_eq!(out.hash.len(), 64);
    // Writing the same bytes again is a no-op on an existing blob,
    // which is what content addressing means — and the second write
    // must produce the same hash rather than re-escaping anything.
    let again = f.set_binary("b.bin", raw, None).unwrap();
    assert_eq!(again.hash, out.hash);
    assert!(
        again.created,
        "a different path, so a new project_files row"
    );
    // One blob, two files.
    assert_eq!(f.blob(&out.hash).unwrap().3, 4);
}

/// A binary write is staged and committed like any other, and the
/// commit records the blob's byte length with a NULL line count.
///
/// This is the one place `blob_metrics`' binary branch is reachable
/// from a write rather than from a hand-built fixture: before binary
/// content existed, nothing the port could write produced a blob with
/// no `content_text`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_binary_file_commits_with_a_null_line_count() {
    let mut f = fixture!("wp-binary-commit");
    f.branch("main");
    let (_, session) = f.session();

    let raw: &[u8] = &[0xff, 0xfe, 0x00, 0x01, 0x84];
    let out = f.set_binary("assets/x.bin", raw, Some(session)).unwrap();
    assert!(matches!(
        out.staged,
        templedb_db::Staged::Yes { state: "added", .. }
    ));

    let commit = f.commit("add the asset", None, session).unwrap();
    let states = f.file_states(commit.commit_id);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].2.as_deref(), Some(out.hash.as_str()));
    assert_eq!(states[0].3, Some(5), "the blob's byte length");
    assert_eq!(
        states[0].4, None,
        "no content_text, so Python's `if content_text` is false and the \
         line count is NULL rather than 0"
    );
}

/// `--commit`: one invocation writes, stages and commits, and all three
/// are visible to the readers the sweep diffs against Python.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn set_with_commit_writes_stages_and_commits_in_one_call() {
    let mut f = fixture!("wp-set-commit");
    let branch_id = f.branch("main");
    let (_, session) = f.session();

    let (set, commit) = f
        .set_then_commit(
            "src/one.rs",
            "fn one() {}\n",
            session,
            "file set: src/one.rs",
        )
        .unwrap();

    assert!(set.created);
    assert_eq!(commit.files, 1);
    assert_eq!(commit.previous_head, None);
    assert_eq!(commit.branch_name, "main");

    // Through the three readers.
    let log = f.log();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].commit_hash, commit.hash);
    // Python's default message, which this reproduces verbatim.
    assert_eq!(log[0].commit_message, "file set: src/one.rs");
    assert_eq!(f.changed(&commit.hash)[0].file_path, "src/one.rs");
    assert_eq!(f.read_back("src/one.rs").as_deref(), Some("fn one() {}\n"));

    // The committed content is the hash the write produced, not a
    // previous revision — the column Python's comment warns about,
    // checked across a composition that never leaves the transaction.
    let states = f.file_states(commit.commit_id);
    assert_eq!(states[0].2.as_deref(), Some(set.hash.as_str()));

    // The head moved and the stage is spent.
    assert_eq!(f.head(branch_id), Some(commit.commit_id));
    let file_id = f.file_id("src/one.rs");
    assert_eq!(
        f.working_state(file_id),
        Some(("unmodified".to_string(), None))
    );
}

/// The atomicity `--commit` claims: a commit that refuses takes the
/// write with it.
///
/// The refusal is guard 3 — a second file staged with a hash naming no
/// blob, which `vcs status --refresh` produces over there and no writer
/// produces here. Python's arrangement leaves the write landed and
/// staged at this point and returns the commit's status; this leaves
/// nothing, which is what one transaction buys and the only behavioural
/// difference between the two implementations' `--commit`.
///
/// Written to be able to fail: the path is asserted absent *and* the
/// staged row for the other file is asserted unchanged, so a rollback
/// that took too much would be as visible as one that took too little.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_refused_commit_rolls_back_the_write_that_asked_for_it() {
    let mut f = fixture!("wp-set-commit-refused");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    // A staged row the commit will refuse: `modified` with a hash that
    // names no blob.
    f.set("src/old.rs", "fn old() {}\n", None).unwrap();
    let old_id = f.file_id("src/old.rs");
    f.stage(
        branch,
        old_id,
        "modified",
        Some("0000000000000000000000000000000000000000000000000000000000000000"),
        Some(session_id),
    );

    let err = f
        .set_then_commit("src/new.rs", "fn new() {}\n", session, "should not land")
        .unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::NO_PATH);
    assert!(format!("{err:#}").contains("names no blob"), "{err:#}");

    // Nothing of the write survives: no file row, no content, no
    // commit, and the head did not move.
    assert!(
        !f.file_row_exists("src/new.rs"),
        "the write must be rolled back with the commit"
    );
    assert_eq!(f.read_back("src/new.rs"), None);
    assert!(f.log().is_empty());
    assert_eq!(f.head(branch), None);

    // And the rollback took only this transaction: the hand-staged row
    // from before it is untouched.
    assert_eq!(
        f.working_state(old_id),
        Some(("modified".to_string(), Some(session_id)))
    );
}

/// `--commit` commits the *session*, not the path — the sharp edge the
/// CLI prints a note about.
///
/// Python behaves the same way and says nothing, which is why this is
/// pinned rather than left as a comment: an implementation that scoped
/// the commit to the file being written would look more sensible, would
/// diverge from Python, and would silently leave the other staged rows
/// behind for the next commit to pick up.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn set_with_commit_commits_everything_the_session_staged() {
    let mut f = fixture!("wp-set-commit-scope");
    f.branch("main");
    let (_, session) = f.session();

    // Staged by an earlier `file set` in the same session.
    f.set_as("src/earlier.rs", "fn earlier() {}\n", Some(session), true)
        .unwrap();

    let (_, commit) = f
        .set_then_commit("src/later.rs", "fn later() {}\n", session, "both")
        .unwrap();

    assert_eq!(commit.files, 2, "the session's rows, not the one path");
    let mut paths: Vec<String> = f
        .changed(&commit.hash)
        .into_iter()
        .map(|c| c.file_path)
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["src/earlier.rs", "src/later.rs"]);
}

// ---------------------------------------------------------------------
// `intent cancel` — the third write, and the smallest.
//
// Its verification is the strongest available to a write in this port,
// and that is the point of doing it: `intent show` already reads both
// columns it touches, byte-identically against Python, so the loop is
// write-then-read-through-a-trusted-reader rather than
// write-then-read-through-a-query-written-for-the-test.

/// The instant the cancel fixtures write at. Pinned for
/// `STAGE_INSTANT`'s reason — `cancelled_at` becomes a value to assert
/// rather than a column to exclude.
const CANCEL_INSTANT: &str = "2026-10-31 09:30:00";

impl Fixture {
    /// A `proposed` intent to cancel, inserted directly.
    ///
    /// This said "directly because `intent create` is not ported" until
    /// 2026-11-18 and that stopped being true on 2026-11-17, when step
    /// 31 landed the command and `a_created_intent_is_cancellable`
    /// started driving one into the other with no SQL between. The
    /// helper stays for `Fixture::stage`'s reason, which is the only
    /// one left: it can set a *terminal* status and an arbitrary hash,
    /// which no ported command can, so the refusal paths have something
    /// to refuse.
    fn propose(&mut self, path: &str, hash: &str) -> i64 {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "INSERT INTO edit_intents
                     (project_id, file_path, base_revision, new_content_hash,
                      patch_summary, author, description, status, created_at)
                 VALUES ($1, $2, 'current', $3, '1 bytes', 'test',
                         'fixture', 'proposed', $4)
                 RETURNING id",
                &[&self.project_id, &path, &hash, &CANCEL_INSTANT],
            )
            .unwrap();
        tx.commit().unwrap();
        row.get(0)
    }

    /// Flip an intent to a terminal status without going through the
    /// command, so the refusal paths have something to refuse.
    fn force_status(&mut self, id: i64, status: &str) {
        let mut tx = self.writer.transaction().unwrap();
        tx.execute(
            "UPDATE edit_intents SET status = $2 WHERE id = $1",
            &[&id, &status],
        )
        .unwrap();
        tx.commit().unwrap();
    }

    /// Run the command under test at the pinned instant.
    fn cancel(&mut self, id: i64) -> Result<(), templedb_db::CancelRefusal> {
        let clock = Clock::at(CANCEL_INSTANT).unwrap();
        let mut tx = self.writer.transaction().unwrap();
        let outcome = templedb_db::cancel_intent(&mut tx, &clock, id).unwrap();
        if outcome.is_ok() {
            tx.commit().unwrap();
        }
        outcome
    }

    /// The row as the *read layer* sees it — `show_intent`, the same
    /// function `templedb-rs intent show` calls.
    fn intent(&self, id: i64) -> templedb_db::Intent {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::show_intent(&store, id).unwrap().unwrap()
    }
}

/// The happy path, read back through `intent show`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn cancelling_a_proposed_intent_sets_the_status_and_the_timestamp() {
    let mut f = fixture!("wp-intent-cancel");
    let id = f.propose("src/proposed.rs", "h-cancel-1");

    // Before: proposed, and no `cancelled_at` — so the column the write
    // sets is demonstrably empty first, which is what makes the
    // assertion after it mean something.
    let before = f.intent(id);
    assert_eq!(before.status, "proposed");
    assert_eq!(before.cancelled_at, None);

    assert!(f.cancel(id).is_ok());

    let after = f.intent(id);
    assert_eq!(after.status, "cancelled");
    assert_eq!(after.cancelled_at.as_deref(), Some(CANCEL_INSTANT));
    // Everything else is untouched: the source is not changed by a
    // cancel, and `applied_at` in particular must stay empty.
    assert_eq!(after.applied_at, None);
    assert_eq!(after.applied_commit_id, None);
    assert_eq!(after.new_content_hash, "h-cancel-1");
    assert_eq!(after.file_path, "src/proposed.rs");
}

/// The rendered line, which is Python's and reuses the `✗` marker.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_success_line_names_the_id_with_the_cancelled_marker() {
    let mut f = fixture!("wp-intent-cancel-line");
    let id = f.propose("src/line.rs", "h-cancel-line");
    assert!(f.cancel(id).is_ok());
    assert_eq!(
        templedb_db::cancelled_line(id),
        format!("✗ Intent #{id} cancelled")
    );
}

/// An `applied` intent is refused, the status is reported back, and
/// nothing moves.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn cancelling_an_applied_intent_is_refused_and_changes_nothing() {
    let mut f = fixture!("wp-intent-cancel-applied");
    let id = f.propose("src/applied.rs", "h-cancel-2");
    f.force_status(id, "applied");

    let refusal = f.cancel(id).unwrap_err();
    assert_eq!(
        refusal,
        templedb_db::CancelRefusal::NotProposed("applied".to_string())
    );

    // The refusal is total: still applied, still no `cancelled_at`.
    let after = f.intent(id);
    assert_eq!(after.status, "applied");
    assert_eq!(after.cancelled_at, None);
}

/// An id with no row is `NotFound`, and distinguishable from the
/// refusal above — which is the whole reason the two carry different
/// exit statuses.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn cancelling_a_missing_id_is_not_found() {
    let mut f = fixture!("wp-intent-cancel-missing");
    assert_eq!(
        f.cancel(999_999_999).unwrap_err(),
        templedb_db::CancelRefusal::NotFound
    );
}

/// **The test the compare-and-swap exists for.** A second cancel is
/// refused rather than silently re-stamping the row.
///
/// Python's check-then-act would pass its guard only once here too,
/// because the two calls are sequential — what it cannot do is make
/// that true under concurrency, since its `UPDATE` carries no status
/// predicate. This asserts the observable half: the first
/// `cancelled_at` survives, and the second call reports the status it
/// found rather than succeeding twice.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_cancel_is_refused_and_keeps_the_first_timestamp() {
    let mut f = fixture!("wp-intent-cancel-twice");
    let id = f.propose("src/twice.rs", "h-cancel-3");

    assert!(f.cancel(id).is_ok());
    let first = f.intent(id).cancelled_at;
    assert_eq!(first.as_deref(), Some(CANCEL_INSTANT));

    assert_eq!(
        f.cancel(id).unwrap_err(),
        templedb_db::CancelRefusal::NotProposed("cancelled".to_string()),
        "the second call must see the status the first one wrote"
    );
    assert_eq!(
        f.intent(id).cancelled_at,
        first,
        "the second call must not re-stamp the row"
    );
}

/// The write is visible to `intent list` as well as to `intent show` —
/// three readers of the same row, which is the cross-read rule applied
/// the way `vcs commit-staged`'s harness applies it.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_cancelled_row_is_visible_to_intent_list() {
    let mut f = fixture!("wp-intent-cancel-list");
    let id = f.propose("src/listed.rs", "h-cancel-4");

    let url = scratch_url().unwrap();
    let store = templedb_db::PgStore::connect(&url).unwrap();
    let outstanding = templedb_db::list_intents(
        &store,
        &templedb_db::IntentFilter {
            project_id: Some(f.project_id),
            ..Default::default()
        },
        100,
    )
    .unwrap();
    assert!(
        outstanding.iter().any(|i| i.id == id),
        "a proposed intent is in the default view"
    );

    assert!(f.cancel(id).is_ok());

    // Gone from the default view, because that view is
    // `status = 'proposed'` — so the write is observable through the
    // *filter* and not only through the columns.
    let after = templedb_db::list_intents(
        &store,
        &templedb_db::IntentFilter {
            project_id: Some(f.project_id),
            ..Default::default()
        },
        100,
    )
    .unwrap();
    assert!(!after.iter().any(|i| i.id == id));

    // And present under `--all-statuses`, where the row renders with
    // the `✗` marker and the `cancelled_at` as its timestamp — the
    // `when()` fallback, exercised against a row this port wrote.
    let all = templedb_db::list_intents(
        &store,
        &templedb_db::IntentFilter {
            project_id: Some(f.project_id),
            all_statuses: true,
            ..Default::default()
        },
        100,
    )
    .unwrap();
    let row = all.iter().find(|i| i.id == id).expect("present");
    assert_eq!(row.status, "cancelled");
    let line = templedb_db::render_intent(row);
    assert!(line.starts_with("  ✗ "), "{line}");
    assert!(line.contains(CANCEL_INSTANT), "{line}");
}

// ---------------------------------------------------------------------
// `intent create` — the fourth write, and the one that closes the
// intent family's own loop.
//
// Until now this file minted its `proposed` rows by hand, in
// `Fixture::propose`, because the command that produces them was not
// ported: "the fixture has to mint the row the command consumes". That
// helper stays — it can force a terminal status, which no ported
// command can — but the loop below no longer needs it, and
// `a_created_intent_is_cancellable` is the test that says so.
//
// The verification is cross-read throughout: every assertion about what
// landed goes through `show_intent` or `list_intents`, the two readers
// the byte-diff sweep already checks against Python.

/// The instant the create fixtures write at. Pinned for
/// `CANCEL_INSTANT`'s reason — `created_at` becomes a value to assert
/// rather than a column to exclude.
const CREATE_INSTANT: &str = "2026-11-17 14:00:00";

impl Fixture {
    /// Run `intent create` at the pinned instant.
    fn create(
        &mut self,
        path: &str,
        content: &str,
        opts: templedb_db::CreateOptions<'_>,
    ) -> templedb_db::Created {
        let clock = Clock::at(CREATE_INSTANT).unwrap();
        let mut tx = self.writer.transaction().unwrap();
        let created =
            templedb_db::create_intent(&mut tx, &clock, self.project_id, path, content, &opts)
                .unwrap();
        tx.commit().unwrap();
        created
    }
}

// `Fixture::blob` is not redefined here: the one the `file set` tests
// already have reads the same four columns through the same `Store`,
// and a second reader of one table is the duplication this step went
// out of its way to remove from the *writer*.

/// The happy path, read back through `intent show`.
///
/// Every value asserted here is one Python wrote on a copy of the live
/// database on 2026-11-17 for the same content: intent #1625 held
/// `base_revision: current`, `patch_summary: 3 lines, 12 bytes` and
/// the same sha256 for `"hello\nworld\n"`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn creating_an_intent_is_visible_to_intent_show() {
    let mut f = fixture!("wp-intent-create");
    let created = f.create(
        "measured.txt",
        "hello\nworld\n",
        templedb_db::CreateOptions {
            author: Some("write-parity"),
            ..Default::default()
        },
    );

    let i = f.intent(created.id);
    assert_eq!(i.status, "proposed");
    assert_eq!(i.file_path, "measured.txt");
    assert_eq!(i.base_revision, "current");
    assert_eq!(
        i.new_content_hash,
        "4a1e67f2fe1d1cc7b31d0ca2ec441da4778203a036a77da10344c85e24ff0f92"
    );
    assert_eq!(i.patch_summary.as_deref(), Some("3 lines, 12 bytes"));
    assert_eq!(i.author.as_deref(), Some("write-parity"));
    assert_eq!(i.created_at, CREATE_INSTANT);
    // The three lifecycle columns a *proposal* must leave empty. The
    // row is only cancellable and only appears in the default view
    // because all three are NULL.
    assert_eq!(i.applied_at, None);
    assert_eq!(i.cancelled_at, None);
    assert_eq!(i.applied_commit_id, None);
    assert_eq!(i.description, None);
    assert_eq!(i.session_id, None);
    // And the returned struct agrees with the row, which is what the
    // renderer prints from.
    assert_eq!(created.base_revision, i.base_revision);
    assert_eq!(created.new_content_hash, i.new_content_hash);
}

/// The blob is stashed, so an apply would have bytes to write.
///
/// Python's comment is the reason this is asserted separately from the
/// row: "Stash the content in content_blobs so apply can find it." An
/// intent whose hash names no blob is a proposal nothing can carry out,
/// and `vcs commit-staged` already has a refusal for exactly that
/// shape one table over.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_proposed_content_is_stashed_as_a_text_blob() {
    let mut f = fixture!("wp-intent-create-blob");
    let created = f.create(
        "b.txt",
        "hello\nworld\n",
        templedb_db::CreateOptions::default(),
    );

    let (content_type, text, raw, size) = f.blob(&created.new_content_hash).expect("blob stored");
    assert_eq!(content_type, "text");
    assert_eq!(text.as_deref(), Some("hello\nworld\n"));
    assert_eq!(size, 12);
    // Text goes in `content_text` and the `bytea` column stays NULL —
    // the shape Python's scanner writes, and the one `file cat` reads.
    assert_eq!(raw, None);
}

/// **What the command deliberately does not write.**
///
/// The assertion that made `intent create` safe to land while
/// `intent apply` was unported: a proposal changes no source. If any
/// of these three grew a row, the command would be a `file set` wearing
/// another name and the port would be applying edits nobody asked it
/// to.
///
/// It is not weaker now that `apply` exists — it is the statement that
/// the two commands stay separate, which is Python's own emphasis and
/// is why `applying_stages_nothing_and_commits_nothing` is its mirror
/// image one section down.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn creating_an_intent_touches_no_source_at_all() {
    let mut f = fixture!("wp-intent-create-nosource");
    // A branch and a session, so the two things that *could* make a
    // stage happen are both available. `file set` with this much state
    // stages; this must not.
    f.branch("main");
    let (_, session) = f.session();
    let created = f.create(
        "untouched.txt",
        "proposed only\n",
        templedb_db::CreateOptions {
            session: Some(session),
            ..Default::default()
        },
    );
    assert_eq!(f.intent(created.id).status, "proposed");

    // No `project_files` row, so no `file_contents` row and nothing for
    // `file cat` to return. Read through the ported reader, which is
    // the same "can something else see it" question inverted.
    assert_eq!(f.read_back("untouched.txt"), None);
    assert_eq!(f.stats("untouched.txt"), None);
    let mut tx = f.writer.transaction().unwrap();
    let files: i64 = tx
        .query_one(
            "SELECT count(*) FROM project_files WHERE project_id = $1",
            &[&f.project_id],
        )
        .unwrap()
        .get(0);
    let staged: i64 = tx
        .query_one(
            "SELECT count(*) FROM vcs_working_state WHERE project_id = $1",
            &[&f.project_id],
        )
        .unwrap()
        .get(0);
    tx.commit().unwrap();
    assert_eq!(files, 0, "intent create must not register a file");
    assert_eq!(staged, 0, "intent create must not stage anything");
}

/// A path that is not registered in the project is accepted, which is
/// Python's: `apply` is where a missing `project_files` row becomes an
/// exit 3, not `create`.
///
/// The same fact as the test above from the other direction, and worth
/// its own name because the two are easy to conflate: that one says
/// nothing was created, this one says nothing was *required*.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn an_unregistered_path_is_proposable() {
    let mut f = fixture!("wp-intent-create-unreg");
    let created = f.create(
        "never/existed.txt",
        "x",
        templedb_db::CreateOptions::default(),
    );
    assert_eq!(f.intent(created.id).file_path, "never/existed.txt");
}

/// `--base-rev` is stored as given, and an empty one falls through to
/// `'current'` — Python's `args.base_rev or 'current'`, where `''` is
/// falsy.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn an_explicit_base_rev_is_stored_and_an_empty_one_defaults() {
    let mut f = fixture!("wp-intent-create-baserev");
    let explicit = f.create(
        "a.txt",
        "x",
        templedb_db::CreateOptions {
            base_rev: Some("deadbeef1234"),
            ..Default::default()
        },
    );
    assert_eq!(f.intent(explicit.id).base_revision, "deadbeef1234");

    let empty = f.create(
        "b.txt",
        "x",
        templedb_db::CreateOptions {
            base_rev: Some(""),
            ..Default::default()
        },
    );
    assert_eq!(f.intent(empty.id).base_revision, "current");

    let absent = f.create("c.txt", "x", templedb_db::CreateOptions::default());
    assert_eq!(f.intent(absent.id).base_revision, "current");
}

/// `-m` lands in `description` and reaches `intent list`'s line,
/// through the default `proposed`-only filter.
///
/// Two readers rather than one, for the reason `intent cancel`'s tests
/// give: a write is verified by the *view*, not only by the column.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_message_lands_in_description_and_reaches_intent_list() {
    let mut f = fixture!("wp-intent-create-msg");
    let created = f.create(
        "noted.txt",
        "x",
        templedb_db::CreateOptions {
            message: Some("a note"),
            ..Default::default()
        },
    );
    assert_eq!(f.intent(created.id).description.as_deref(), Some("a note"));

    let url = scratch_url().unwrap();
    let store = templedb_db::PgStore::connect(&url).unwrap();
    // The default filter, which is `status = 'proposed'` — so this is
    // also the assertion that a fresh proposal is *outstanding*.
    let rows = templedb_db::list_intents(
        &store,
        &templedb_db::IntentFilter {
            project_id: Some(f.project_id),
            ..Default::default()
        },
        100,
    )
    .unwrap();
    let row = rows
        .iter()
        .find(|i| i.id == created.id)
        .expect("a proposed intent is in the default view");
    let line = templedb_db::render_intent(row);
    assert!(line.starts_with("  ⋯ "), "{line}");
    assert!(line.ends_with("/noted.txt — a note"), "{line}");
    // `when()` falls through to `created_at`, because neither of the
    // other two timestamps is set.
    assert!(line.contains(CREATE_INSTANT), "{line}");
}

/// **The loop closes.** A row this port created is a row this port can
/// cancel, with no hand-written SQL in between.
///
/// Before this step the two halves were verified separately: `intent
/// cancel`'s tests built their `proposed` row with
/// `Fixture::propose`, which is a raw INSERT. That verified the cancel
/// against a row the *test* wrote. This drives `create_intent` and then
/// `cancel_intent`, so the second command's input is the first
/// command's output — the same move `the_loop_closes_when_file_set_
/// stages_its_own_commit` makes for the staging pair.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_created_intent_is_cancellable() {
    let mut f = fixture!("wp-intent-create-cancel");
    let created = f.create("loop.txt", "x", templedb_db::CreateOptions::default());
    assert_eq!(f.intent(created.id).status, "proposed");

    assert!(f.cancel(created.id).is_ok());
    let after = f.intent(created.id);
    assert_eq!(after.status, "cancelled");
    assert_eq!(after.cancelled_at.as_deref(), Some(CANCEL_INSTANT));
    // The proposal's own columns survive the cancel, which is what
    // makes a cancelled intent still an audit record.
    assert_eq!(after.created_at, CREATE_INSTANT);
    assert_eq!(after.new_content_hash, created.new_content_hash);
}

/// Empty content is **accepted**, where `file set` refuses an empty
/// pipe outright.
///
/// Both behaviours are Python's and they disagree with each other:
/// measured on a copy 2026-11-17, `printf '' | templedb intent create`
/// created intent #1627 with `patch_summary: 1 lines, 0 bytes` and
/// exited 0, where `file set` with no content exits non-zero and says
/// "no content provided". Reproduced rather than harmonised — the
/// inconsistency is upstream's and it is in the README's *Known gaps*.
///
/// `1 lines` is also the summary arithmetic's one disagreement with
/// `file set`'s line count, which is 0 for the same input.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn empty_content_is_proposable_where_file_set_refuses_it() {
    let mut f = fixture!("wp-intent-create-empty");
    let created = f.create("empty.txt", "", templedb_db::CreateOptions::default());
    let i = f.intent(created.id);
    assert_eq!(i.patch_summary.as_deref(), Some("1 lines, 0 bytes"));
    // sha256 of zero bytes, which is the hash Python recorded for
    // #1627.
    assert_eq!(
        i.new_content_hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    let (content_type, text, _, size) = f.blob(&i.new_content_hash).expect("empty blob stored");
    assert_eq!(content_type, "text");
    assert_eq!(text.as_deref(), Some(""));
    assert_eq!(size, 0);
}

/// A pinned session is stamped on the proposal, and it is the
/// *resolved* id.
///
/// Python reads `TEMPLEDB_SESSION_ID` through a bare `int()` and checks
/// nothing — measured on a copy, `TEMPLEDB_SESSION_ID=notanumber`
/// creates the intent with a NULL session and exits 0. This port has
/// one resolver and it requires a live row, so what reaches the column
/// is an id that existed when the write ran.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_pinned_session_is_stamped_on_the_proposal() {
    let mut f = fixture!("wp-intent-create-session");
    let (live, session) = f.session();
    let created = f.create(
        "owned.txt",
        "x",
        templedb_db::CreateOptions {
            session: Some(session),
            ..Default::default()
        },
    );
    assert_eq!(f.intent(created.id).session_id, Some(live));

    // And `intent list -s <id>` finds it, which is the filter a caller
    // uses to see their own outstanding proposals.
    let url = scratch_url().unwrap();
    let store = templedb_db::PgStore::connect(&url).unwrap();
    let rows = templedb_db::list_intents(
        &store,
        &templedb_db::IntentFilter {
            project_id: Some(f.project_id),
            session_id: Some(live),
            ..Default::default()
        },
        100,
    )
    .unwrap();
    assert_eq!(
        rows.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![created.id]
    );
}

/// Two proposals of identical content share one blob and are two rows.
///
/// The `ON CONFLICT DO NOTHING` half of the statement, which is
/// Python's `INSERT OR IGNORE` and is the thing that makes a second
/// proposal cheap. Verified on the live copy too: the empty blob
/// `intent create` "inserted" for #1627 already had 21 references and
/// was not rewritten.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_proposal_of_the_same_content_reuses_the_blob() {
    let mut f = fixture!("wp-intent-create-dedup");
    let first = f.create(
        "one.txt",
        "same bytes\n",
        templedb_db::CreateOptions::default(),
    );
    let second = f.create(
        "two.txt",
        "same bytes\n",
        templedb_db::CreateOptions::default(),
    );
    assert_ne!(first.id, second.id);
    assert_eq!(first.new_content_hash, second.new_content_hash);

    let mut tx = f.writer.transaction().unwrap();
    let blobs: i64 = tx
        .query_one(
            "SELECT count(*) FROM content_blobs WHERE hash_sha256 = $1",
            &[&first.new_content_hash],
        )
        .unwrap()
        .get(0);
    tx.commit().unwrap();
    assert_eq!(blobs, 1);
    assert_eq!(f.intents("one.txt").len(), 1);
    assert_eq!(f.intents("two.txt").len(), 1);
}

/// Content with a NUL survives the proposal, through the same escape
/// `file set`'s blob insert uses.
///
/// `content_blobs.content_text` is a Postgres `text` column and cannot
/// hold a U+0000, which is why `pgtext` exists. The statement is shared
/// now, so this is the assertion that sharing it did not cost the
/// intent path the escape — the failure would be a write that aborts
/// with "unsupported Unicode escape sequence" rather than a wrong
/// value.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn proposed_content_with_a_nul_round_trips() {
    let mut f = fixture!("wp-intent-create-nul");
    let content = "before\u{0}after\n";
    let created = f.create("nul.txt", content, templedb_db::CreateOptions::default());
    let (_, text, _, size) = f.blob(&created.new_content_hash).expect("blob stored");
    assert_eq!(text.as_deref(), Some(content));
    // The size is over the real bytes, not the escaped form.
    assert_eq!(size, content.len() as i64);
    // And the summary counts the real bytes too.
    assert_eq!(
        f.intent(created.id).patch_summary.as_deref(),
        Some("2 lines, 13 bytes")
    );
}

// ---------------------------------------------------------------------
// `intent apply` — the fifth write, and the one that turns a proposal
// into source.
//
// Its verification is the strongest shape available here, and for once
// every half of it was already paid for: the input comes from
// `create_intent` (step 31), the effect is read back through
// `read_file` and `show_intent`, and both readers are byte-identical
// against Python. So no test below writes SQL to set up the thing it
// checks, except where it needs a state no ported command can produce.
//
// The refusals carry more weight than usual, because one of them is
// where this port *diverges*: `ApplyRefusal::StaleBase` is Python's
// `--no-rebase` behaviour made unconditional. Three tests below pin
// the boundary of that divergence — which bases are stale, which are
// not, and what a refusal leaves behind.

/// The instant the apply fixtures write at. `applied_at` becomes a
/// value to assert rather than a column to exclude, for
/// `CANCEL_INSTANT`'s reason.
const APPLY_INSTANT: &str = "2026-11-18 11:15:00";

impl Fixture {
    /// Run `intent apply` at the pinned instant.
    ///
    /// Commits only on success, which is the CLI arm's behaviour and is
    /// the property three tests below assert: a refusal rolls back the
    /// status claim `apply_intent` makes in its first statement.
    fn apply(&mut self, id: i64) -> Result<templedb_db::Applied, templedb_db::ApplyRefusal> {
        let clock = Clock::at(APPLY_INSTANT).unwrap();
        let mut tx = self.writer.transaction().unwrap();
        let outcome = templedb_db::apply_intent(&mut tx, &clock, id).unwrap();
        if outcome.is_ok() {
            tx.commit().unwrap();
        }
        outcome
    }

    /// The current `file_contents` row's size, line count and version.
    ///
    /// Separate from [`Fixture::stats`] because that one unwraps the
    /// line count, and a NULL there is precisely what `intent apply`
    /// writes — asserting Python's behaviour through a helper that
    /// cannot represent it is how the behaviour would get quietly
    /// fixed.
    fn content_row(&mut self, file_id: i64) -> Option<(i64, Option<i64>, Option<i64>)> {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_opt(
                "SELECT file_size_bytes, line_count, version
                   FROM file_contents
                  WHERE file_id = $1 AND is_current = 1",
                &[&file_id],
            )
            .unwrap();
        tx.commit().unwrap();
        row.map(|r| (r.get(0), r.get(1), r.get(2)))
    }

    /// Forget a blob, so the "nothing to apply" path has something to
    /// refuse.
    ///
    /// Hand-written because no ported command deletes a blob — the
    /// subset carries no `reference_count` and nothing reaps. Upstream
    /// reaches the same state through migration 127's refcounting.
    fn forget_blob(&mut self, hash: &str) {
        let mut tx = self.writer.transaction().unwrap();
        tx.execute("DELETE FROM content_blobs WHERE hash_sha256 = $1", &[&hash])
            .unwrap();
        tx.commit().unwrap();
    }

    /// Register a path with no content row at all.
    ///
    /// The `current_hash IS NULL` branch of the staleness test, which
    /// `base_is_stale` documents as "not stale" and which no ported
    /// command produces: `file set` always writes a `file_contents`
    /// row beside the `project_files` one.
    fn register_only(&mut self, path: &str) -> i64 {
        let mut tx = self.writer.transaction().unwrap();
        let row = tx
            .query_one(
                "INSERT INTO project_files (project_id, file_path, status)
                 VALUES ($1, $2, 'active')
                 RETURNING id",
                &[&self.project_id, &path],
            )
            .unwrap();
        tx.commit().unwrap();
        row.get(0)
    }
}

/// **The loop closes.** `intent create` proposes, `intent apply`
/// applies, and `file cat` reads the result — no SQL anywhere in it.
///
/// The move `a_created_intent_is_cancellable` made for the cancel and
/// `the_loop_closes_when_file_set_stages_its_own_commit` made for the
/// stage, now for the third pair. Every value asserted is produced by
/// a ported command and read by a different ported command.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_created_intent_is_applied_and_reads_back_as_the_files_content() {
    let mut f = fixture!("wp-intent-apply");
    // `file set` registers the path, which `apply` requires and
    // `create` does not.
    f.set("src/a.rs", "fn old() {}\n", None).unwrap();
    let created = f.create(
        "src/a.rs",
        "fn new() {}\n",
        templedb_db::CreateOptions::default(),
    );
    // Before: the proposal exists and the file does not hold it. That
    // is what makes the assertion after it mean something.
    assert_eq!(f.read_back("src/a.rs").as_deref(), Some("fn old() {}\n"));
    assert_eq!(f.intent(created.id).status, "proposed");

    let applied = f.apply(created.id).unwrap();
    assert_eq!(applied.id, created.id);
    assert_eq!(applied.file_path, "src/a.rs");
    assert_eq!(applied.hash, created.new_content_hash);

    // The source moved, read through `file cat`'s own function.
    assert_eq!(f.read_back("src/a.rs").as_deref(), Some("fn new() {}\n"));
    // And the intent is marked, read through `intent show`'s.
    let after = f.intent(created.id);
    assert_eq!(after.status, "applied");
    assert_eq!(after.applied_at.as_deref(), Some(APPLY_INSTANT));
    // `new_content_hash` is untouched: it records what was *proposed*,
    // which is Python's comment and matters only when the two can
    // differ — they cannot here, because the rebase is refused.
    assert_eq!(after.new_content_hash, created.new_content_hash);
    // Nothing links a commit, because apply makes none.
    assert_eq!(after.applied_commit_id, None);
    assert_eq!(after.cancelled_at, None);
}

/// The applied row leaves `intent list`'s default view and comes back
/// under `--all-statuses` with the `✓` marker.
///
/// The write is observable through the *filter* and not only through
/// the columns, which is the rule `the_cancelled_row_is_visible_to_
/// intent_list` set for the previous write.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_applied_row_leaves_the_outstanding_view() {
    let mut f = fixture!("wp-intent-apply-list");
    f.set("listed.txt", "before\n", None).unwrap();
    let created = f.create(
        "listed.txt",
        "after\n",
        templedb_db::CreateOptions::default(),
    );

    let url = scratch_url().unwrap();
    let store = templedb_db::PgStore::connect(&url).unwrap();
    // `project_id` copied out rather than read through `f`, so the
    // closure does not hold a borrow across `f.apply`.
    let project_id = f.project_id;
    let outstanding = |all: bool| {
        templedb_db::list_intents(
            &store,
            &templedb_db::IntentFilter {
                project_id: Some(project_id),
                all_statuses: all,
                ..Default::default()
            },
            100,
        )
        .unwrap()
    };
    assert!(outstanding(false).iter().any(|i| i.id == created.id));

    assert!(f.apply(created.id).is_ok());

    assert!(
        !outstanding(false).iter().any(|i| i.id == created.id),
        "an applied intent is no longer outstanding"
    );
    let all = outstanding(true);
    let row = all
        .iter()
        .find(|i| i.id == created.id)
        .expect("present under --all-statuses");
    let line = templedb_db::render_intent(row);
    assert!(line.starts_with("  ✓ "), "{line}");
    // `when()` prefers `applied_at`, so the line carries the apply's
    // instant rather than the create's.
    assert!(line.contains(APPLY_INSTANT), "{line}");
    assert!(!line.contains(CREATE_INSTANT), "{line}");
}

/// Apply clears `line_count` and resets `version`, because Python's
/// `INSERT` names four columns and the row is deleted rather than
/// updated.
///
/// Both are differences from `file set`, which writes the line count
/// and leaves the version alone, and both are reproduced rather than
/// improved. Measured on a copy of the live database 2026-11-18:
/// `file_contents` row 49986 after an apply holds a NULL `line_count`
/// and `version = 1`, for a file whose previous row had been written
/// by `file set`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn applying_clears_the_line_count_and_resets_the_version() {
    let mut f = fixture!("wp-intent-apply-cols");
    f.set("counted.txt", "a\nb\nc\n", None).unwrap();
    let file_id = f.file_id("counted.txt");
    // `file set` wrote the line count, so the column is demonstrably
    // populated before the apply clears it.
    assert_eq!(f.content_row(file_id), Some((6, Some(4), Some(1))));

    let created = f.create(
        "counted.txt",
        "x\ny\n",
        templedb_db::CreateOptions::default(),
    );
    assert!(f.apply(created.id).is_ok());

    let (size, lines, version) = f
        .content_row(file_id)
        .expect("a current row after the apply");
    assert_eq!(size, 4, "the size comes from the blob");
    assert_eq!(lines, None, "Python's INSERT does not name line_count");
    assert_eq!(version, Some(1), "DELETE + INSERT takes the column default");
    // And the size is the blob's rather than the intent's, which is
    // the same value by construction and the reason the read is its
    // own statement.
    assert_eq!(
        f.blob(&created.new_content_hash).map(|b| b.3),
        Some(4),
        "the blob's recorded size is the one that landed"
    );
}

/// Apply writes two tables and nothing else: no stage, no commit, no
/// new intent.
///
/// Driven with a branch *and* a live session available — the exact
/// state under which `file set` stages — so this is the assertion that
/// apply's restraint is deliberate rather than a consequence of a
/// fixture with nothing to stage onto. Same shape as the test
/// `intent create` has for the same property.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn applying_stages_nothing_and_commits_nothing() {
    let mut f = fixture!("wp-intent-apply-quiet");
    f.branch("main");
    let (_, session) = f.session();
    // `--skip-intent`, so the only `edit_intents` row in play is the
    // proposal — otherwise `file set`'s own provenance row would make
    // the count below ambiguous.
    f.set_as("quiet.txt", "before\n", None, false).unwrap();
    let before_intents = f.intents("quiet.txt").len();
    let created = f.create(
        "quiet.txt",
        "after\n",
        templedb_db::CreateOptions {
            session: Some(session),
            ..Default::default()
        },
    );
    assert!(f.apply(created.id).is_ok());

    assert_eq!(
        f.intents("quiet.txt").len(),
        before_intents + 1,
        "apply records no intent of its own"
    );
    let file_id = f.file_id("quiet.txt");
    assert_eq!(
        f.working_state(file_id),
        None,
        "apply must not stage: `vcs commit-staged` is a separate step on both sides"
    );
    assert!(f.log().is_empty(), "apply makes no commit");
}

/// An id with no row is `NotFound` — exit 1, the status `intent show`
/// and `intent cancel` both give for the same miss.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn applying_a_missing_id_is_not_found() {
    let mut f = fixture!("wp-intent-apply-missing");
    assert_eq!(
        f.apply(999_999_999).unwrap_err(),
        templedb_db::ApplyRefusal::NotFound
    );
}

/// A terminal status is refused with the status named, and the source
/// does not move.
///
/// Both halves matter: exit 2 is what tells a caller "already applied"
/// from "cancelled", and a second apply of an applied intent must not
/// re-point `file_contents` at content that may have been superseded
/// since.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn applying_a_terminal_intent_is_refused_and_changes_nothing() {
    let mut f = fixture!("wp-intent-apply-terminal");
    f.set("terminal.txt", "landed\n", None).unwrap();
    let created = f.create(
        "terminal.txt",
        "proposed\n",
        templedb_db::CreateOptions::default(),
    );
    f.force_status(created.id, "cancelled");

    assert_eq!(
        f.apply(created.id).unwrap_err(),
        templedb_db::ApplyRefusal::NotProposed("cancelled".to_string())
    );
    assert_eq!(
        f.read_back("terminal.txt").as_deref(),
        Some("landed\n"),
        "a refused apply leaves the source alone"
    );
    let after = f.intent(created.id);
    assert_eq!(after.status, "cancelled");
    assert_eq!(after.applied_at, None, "the refusal rolled back the claim");
}

/// **A second apply is refused**, which is what the compare-and-swap
/// is for.
///
/// Python's check-then-act passes its guard only once here too, because
/// the two calls are sequential — what it cannot do is make that true
/// under concurrency, since its `UPDATE` carries no status predicate
/// and runs *after* the content write. This asserts the observable
/// half: the first `applied_at` survives and the second call reports
/// the status it found.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_apply_is_refused_and_keeps_the_first_timestamp() {
    let mut f = fixture!("wp-intent-apply-twice");
    f.set("twice.txt", "v1\n", None).unwrap();
    let created = f.create("twice.txt", "v2\n", templedb_db::CreateOptions::default());

    assert!(f.apply(created.id).is_ok());
    assert_eq!(
        f.apply(created.id).unwrap_err(),
        templedb_db::ApplyRefusal::NotProposed("applied".to_string())
    );
    assert_eq!(
        f.intent(created.id).applied_at.as_deref(),
        Some(APPLY_INSTANT)
    );
}

/// A path that is not registered is refused with the path named, and
/// the proposal survives the refusal.
///
/// Exit 3, and it is where `intent create`'s deliberate permissiveness
/// is paid for: `an_unregistered_path_is_proposable` asserts the
/// proposal is legal, and this asserts where it stops being so. Python
/// says so too — its own comment reads "Look up or create the
/// project_files row" and its code never creates one.
///
/// The surviving `'proposed'` status is the transaction property: the
/// status claim is the *first* statement `apply_intent` runs, so a
/// refusal three statements later has to be rolled back rather than
/// undone.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn applying_against_an_unregistered_path_is_refused_and_the_proposal_survives() {
    let mut f = fixture!("wp-intent-apply-unreg");
    let created = f.create(
        "never/existed.txt",
        "x\n",
        templedb_db::CreateOptions::default(),
    );
    assert_eq!(
        f.apply(created.id).unwrap_err(),
        templedb_db::ApplyRefusal::PathNotRegistered("never/existed.txt".to_string())
    );
    let after = f.intent(created.id);
    assert_eq!(after.status, "proposed", "the claim was rolled back");
    assert_eq!(after.applied_at, None);
    assert!(!f.file_row_exists("never/existed.txt"));
}

/// **The port's one divergence in this command.** A base the file has
/// moved past is refused, where Python three-way merges onto it.
///
/// Python reaches this refusal only under `--no-rebase`. The reason
/// this port reaches it always is measured rather than asserted:
/// `merge_resolver.ThreeWayMerge` walks base, ours and theirs with
/// three indices advanced in lockstep, so it is correct only while all
/// three have the same line count. Against `git merge-file -p
/// --diff3` over eleven triples it agrees on 3; four of its clean
/// results write text neither side held, and one of those was
/// reproduced end to end upstream — `templedb intent apply 1645`
/// printed success, exited 0 and dropped a line nobody deleted. See
/// `apply_intent`'s own documentation for the table.
///
/// Everything surviving untouched is the second half of the assertion
/// and the more important one: a refused apply must leave both the
/// source and the proposal exactly as they were, so that re-proposing
/// against what landed is the caller's only next move.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_moved_base_is_refused_where_python_would_merge() {
    let mut f = fixture!("wp-intent-apply-stale");
    let first = f.set("moved.txt", "line one\n", None).unwrap();
    // Proposed against what the file held at the time.
    let created = f.create(
        "moved.txt",
        "line one changed\n",
        templedb_db::CreateOptions {
            base_rev: Some(&first.hash),
            ..Default::default()
        },
    );
    // And then the file moves, which over any real interval is the
    // normal case for an intent rather than an edge one.
    let second = f.set("moved.txt", "line one, but theirs\n", None).unwrap();
    assert_ne!(first.hash, second.hash);

    assert_eq!(
        f.apply(created.id).unwrap_err(),
        templedb_db::ApplyRefusal::StaleBase {
            file_path: "moved.txt".to_string(),
            base: first.hash.clone(),
            current: second.hash.clone(),
        }
    );
    // The source is what the second write left.
    assert_eq!(
        f.read_back("moved.txt").as_deref(),
        Some("line one, but theirs\n")
    );
    // And the proposal is still outstanding, so it can be inspected
    // and re-proposed rather than having been consumed.
    let after = f.intent(created.id);
    assert_eq!(after.status, "proposed");
    assert_eq!(after.applied_at, None);
    assert_eq!(after.new_content_hash, created.new_content_hash);
}

/// The three bases that are **not** stale, which is where the
/// divergence above stops.
///
/// `'current'` is what 886 live rows hold and what `intent create`
/// defaults to, so this is the common path and not an edge; a port
/// that treated it as a hash would refuse every proposal ever written.
/// `'new-file'` is `file set`'s sentinel for "there was nothing". And
/// a twelve-character prefix is what `intent create` *prints*, so a
/// caller who copies the `new:` line into a later `--base-rev` has to
/// be able to apply it.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_sentinels_and_a_short_prefix_all_apply() {
    let mut f = fixture!("wp-intent-apply-bases");

    // A path per base rather than one path five times. The first draft
    // reused one file and failed on the third base, correctly: an apply
    // moves the content, so `landed.hash` stopped being current and the
    // prefix really was stale. The bug was the test's, and it is worth
    // the comment because it is the command's whole subject — a base
    // goes stale the moment anything else lands.
    for (n, base) in [
        Some("current"),
        Some("new-file"),
        None,
        Some("PREFIX"),
        Some("FULL"),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("bases{n}.txt");
        let landed = f.set(&path, "landed\n", None).unwrap();
        // The two hash forms are resolved per file, because each one
        // has its own `landed` hash.
        let base = match base {
            Some("PREFIX") => Some(&landed.hash[..12]),
            Some("FULL") => Some(landed.hash.as_str()),
            other => other,
        };
        let want = format!("applied via {}\n", base.unwrap_or("default"));
        let created = f.create(
            &path,
            &want,
            templedb_db::CreateOptions {
                base_rev: base,
                ..Default::default()
            },
        );
        assert!(
            f.apply(created.id).is_ok(),
            "base {base:?} must not be stale"
        );
        assert_eq!(f.read_back(&path).as_deref(), Some(want.as_str()));
    }
}

/// A registered path with no content row applies cleanly, which is
/// Python's `current_hash and ...` short-circuit.
///
/// No ported command produces this state — `file set` writes both rows
/// — so the `project_files` row is inserted directly. It is reachable
/// upstream, where `vcs commit-staged`'s committed deletion and
/// `file rm` both leave rows behind, and the branch is in
/// `base_is_stale`'s documentation either way.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_registered_path_with_no_content_is_not_stale() {
    let mut f = fixture!("wp-intent-apply-nocontent");
    let file_id = f.register_only("bare.txt");
    assert_eq!(f.content_row(file_id), None);

    let created = f.create(
        "bare.txt",
        "first content\n",
        templedb_db::CreateOptions {
            // A base that would be stale against *any* hash. It is not
            // stale here because there is no hash.
            base_rev: Some("deadbeefdeadbeef"),
            ..Default::default()
        },
    );
    assert!(f.apply(created.id).is_ok());
    assert_eq!(f.read_back("bare.txt").as_deref(), Some("first content\n"));
}

/// **A proposal whose blob is gone leaves the file intact**, where
/// Python leaves it with no content at all.
///
/// The transaction property, and the one place this port is *more*
/// correct rather than merely stricter. Python runs its four statements
/// through a helper that commits each one, and the `DELETE FROM
/// file_contents` comes before the `INSERT` whose scalar subquery
/// yields NULL — so the delete commits, the insert fails, and the file
/// has no current row. Measured on a copy of the live database
/// 2026-11-18: `templedb intent apply` on a crafted intent exited 1
/// with `NOT NULL constraint failed: file_contents.file_size_bytes`,
/// and `templedb file cat regr-test test.txt` then answered "Could not
/// read file content from database" for a file that had been intact a
/// moment earlier.
///
/// Here the whole apply is one transaction, so the same input is a
/// refusal that changes nothing. The status is 1 on both sides, for
/// different reasons — the accidental agreement step 12's work exists
/// to stop being mistaken for a contract.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_proposal_whose_blob_is_gone_leaves_the_file_intact() {
    let mut f = fixture!("wp-intent-apply-noblob");
    f.set("survives.txt", "still here\n", None).unwrap();
    let created = f.create(
        "survives.txt",
        // Unique, so forgetting the blob cannot affect another row.
        "proposed and then forgotten\n",
        templedb_db::CreateOptions::default(),
    );
    f.forget_blob(&created.new_content_hash);
    assert_eq!(f.blob(&created.new_content_hash), None);

    // Not an `ApplyRefusal`: this is an error, because Python's is too.
    let clock = Clock::at(APPLY_INSTANT).unwrap();
    let mut tx = f.writer.transaction().unwrap();
    let err = templedb_db::apply_intent(&mut tx, &clock, created.id).unwrap_err();
    drop(tx);
    let message = format!("{err:#}");
    assert!(
        message.contains(&created.new_content_hash),
        "the message must name the hash that is missing: {message}"
    );

    // The file is exactly as it was, which is the assertion this test
    // exists for.
    assert_eq!(
        f.read_back("survives.txt").as_deref(),
        Some("still here\n"),
        "the transaction must leave the file alone"
    );
    assert_eq!(f.intent(created.id).status, "proposed");
}

// --- `file rm`, the sixth write ---------------------------------------
//
// The deletion half of the loop. `vcs commit-staged` has handled
// `state = 'deleted'` since step 20 and nothing in this port could
// produce such a row, so every test of that branch had to mint it with
// `Fixture::stage` — whose doc comment says a `deleted` row "comes from
// a workspace scan". These drive the ported command instead, and the
// one that matters most is
// [`the_loop_closes_when_file_rm_stages_its_own_commit`]: set, rm,
// commit, read back, with no SQL anywhere in the chain.

/// The instant `file rm` writes at, pinned for `last_modified`.
///
/// Distinct from [`STAGE_INSTANT`] on purpose: a test that asserts the
/// deletion moved the timestamp needs the two writes to disagree about
/// what time it is, and two constants is the cheapest way to say which
/// write a value came from.
const RM_INSTANT: &str = "2026-11-18 09:30:00";

impl Fixture {
    /// Stage a deletion through the ported command.
    fn rm(
        &mut self,
        path: &str,
        session: templedb_db::SessionId,
    ) -> anyhow::Result<templedb_db::RmOutcome> {
        let clock = Clock::at(RM_INSTANT)?;
        let project_id = self.project_id;
        let mut tx = self.writer.transaction()?;
        let outcome = templedb_db::remove_file(&mut tx, &clock, project_id, path, session);
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                // Rolled back explicitly, which is what every refusal in
                // this file asserts: a rejected write leaves nothing.
                drop(tx);
                Err(e)
            }
        }
    }

    /// Flip a `project_files` row to `status = 'deleted'` — the soft
    /// delete `file set`'s un-delete guard revives.
    ///
    /// SQL, and it has to be: no ported command soft-deletes. That is
    /// the same reason `Fixture::force_status` and `Fixture::stage`
    /// survive, and the reason is worth keeping visible rather than
    /// letting the helper look like laziness.
    fn soft_delete(&mut self, path: &str) {
        let mut tx = self.writer.transaction().unwrap();
        let n = tx
            .execute(
                "UPDATE project_files SET status = 'deleted'
                  WHERE project_id = $1 AND file_path = $2",
                &[&self.project_id, &path],
            )
            .unwrap();
        assert_eq!(n, 1, "the fixture should have written {path} first");
        tx.commit().unwrap();
    }
}

/// The row the deletion leaves, column by column — and the two columns
/// Python deliberately does not touch.
///
/// `content_hash` and `intent_id` survive the upsert, which is the
/// behaviour measured on a copy of the live database 2026-11-18: the
/// row for `templedb-rust/.gitignore` held `c20683…` and a NULL
/// `intent_id` before `templedb file rm` and held exactly the same two
/// values afterwards, with only `state`, `staged_by_session_id` and
/// `last_modified` moved. It reads like an oversight upstream and it is
/// load-bearing — see the commit test below for what the surviving hash
/// buys.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_staged_deletion_moves_three_columns_and_leaves_two_alone() {
    let mut f = fixture!("wp-rm-columns");
    let branch = f.branch("main");
    let (session_id, session) = f.session();

    let set = f.set_as("gone.txt", "bye\n", Some(session), true).unwrap();
    let file_id = f.file_id("gone.txt");
    let before = f.staged_row(file_id).expect("the auto-stage wrote a row");
    assert_eq!(before.1, "added");
    assert_eq!(before.2.as_deref(), Some(set.hash.as_str()));
    assert_eq!(before.4, STAGE_INSTANT);
    let intent_before = before.5;
    assert!(intent_before.is_some(), "the set recorded an intent");

    let out = f.rm("gone.txt", session).unwrap();
    assert_eq!(out.branch, "main");
    assert_eq!(out.file_id, file_id);
    assert_eq!(out.previous_state.as_deref(), Some("added"));

    let after = f.staged_row(file_id).expect("the row is still there");
    assert_eq!(after.0, branch, "same branch");
    assert_eq!(after.1, "deleted", "the state moved");
    assert_eq!(
        after.2.as_deref(),
        Some(set.hash.as_str()),
        "content_hash must survive: Python's DO UPDATE SET does not name it"
    );
    assert_eq!(after.3, Some(session_id), "owned by the session");
    assert_eq!(after.4, RM_INSTANT, "last_modified moved to the rm's clock");
    assert_eq!(
        after.5, intent_before,
        "intent_id must survive too — the hash did not move, so the \
         migration-088 link still names the edit the row holds"
    );
}

/// Nothing is removed. The file still reads back, and its
/// `project_files` row is still `active`, until a commit.
///
/// The claim worth pinning rather than the obvious one: `file rm` reads
/// as a delete and the only thing it deletes is a word in one column.
/// Python's help text says so — "hard-delete on commit" — and a port
/// that soft-deleted the row here would look right in `file ls` and be
/// unrecoverable, because `file set`'s un-delete guard would then
/// revive a file the caller had asked to remove.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_staged_deletion_removes_nothing_yet() {
    let mut f = fixture!("wp-rm-nothing-yet");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("still.txt", "here\n", Some(session), true)
        .unwrap();
    f.rm("still.txt", session).unwrap();

    assert_eq!(
        f.read_back("still.txt").as_deref(),
        Some("here\n"),
        "`file cat` must still answer: the stage is a promise, not a delete"
    );
    assert!(f.file_row_exists("still.txt"));
    assert_eq!(f.stats("still.txt"), Some((5, 2)));
}

/// **The loop closes**: `file set` → `file rm` → `vcs commit-staged` →
/// `file cat`, with no SQL between any two of them.
///
/// This is the test the step exists for. The commit's deletion branch
/// was reachable only from `Fixture::stage` before — hand-written SQL
/// asserting that the commit does what the fixture said it would — and
/// now the row comes from the command that produces it in Python too.
///
/// It also pins what the surviving `content_hash` buys. The committed
/// `vcs_file_states` row names the **hash of the bytes that were
/// deleted**, not the `DELETED` sentinel, so the history of a committed
/// deletion records what was removed. That only happens because
/// `file rm`'s upsert leaves the column alone.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_loop_closes_when_file_rm_stages_its_own_commit() {
    let mut f = fixture!("wp-rm-loop");
    f.branch("main");
    let (_, session) = f.session();

    let set = f
        .set_as("doomed.txt", "bye\n", Some(session), true)
        .unwrap();
    let file_id = f.file_id("doomed.txt");
    f.rm("doomed.txt", session).unwrap();

    let out = f.commit("remove doomed.txt", None, session).unwrap();
    assert_eq!(out.files, 1);
    assert_eq!(out.deletions, 1);

    // The content hash, not the sentinel.
    let states = f.file_states(out.commit_id);
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].0.as_deref(), Some("doomed.txt"));
    assert_eq!(states[0].1, "deleted");
    assert_eq!(
        states[0].2.as_deref(),
        Some(set.hash.as_str()),
        "the deleted bytes are named in history, which is what the \
         untouched content_hash is for"
    );
    assert_eq!(states[0].3, Some(4), "`bye\\n` is four bytes");

    // And the file is gone for real, through the reader.
    assert!(f.read_back("doomed.txt").is_none());
    assert!(
        !f.file_row_exists("doomed.txt"),
        "hard-deleted, not flipped to status = 'deleted'"
    );
    assert_eq!(
        f.working_state(file_id),
        None,
        "its staged row went with it"
    );
}

/// With no prior staged row the insert arm runs, `content_hash` is NULL,
/// and the commit records the `DELETED` sentinel.
///
/// The other half of the test above, and the pairing is the point:
/// whether history names the deleted bytes or the sentinel depends
/// entirely on whether the file had been staged before, which is a
/// property of the *caller's* sequence rather than of this command. Both
/// are Python's.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn an_unstaged_file_deletes_through_the_sentinel() {
    let mut f = fixture!("wp-rm-sentinel");
    f.branch("main");
    let (_, session) = f.session();

    // `Fixture::set` pins no session, so nothing is staged.
    f.set("never-staged.txt", "content\n", None).unwrap();
    let file_id = f.file_id("never-staged.txt");
    assert_eq!(f.staged_row(file_id), None, "no row to update");

    let out = f.rm("never-staged.txt", session).unwrap();
    assert_eq!(
        out.previous_state, None,
        "None distinguishes the insert arm from the update one"
    );
    let row = f.staged_row(file_id).expect("the insert arm wrote a row");
    assert_eq!(row.1, "deleted");
    assert_eq!(row.2, None, "no hash: Python's INSERT does not name one");
    assert_eq!(row.5, None, "and no intent link either");

    let committed = f.commit("drop the unstaged one", None, session).unwrap();
    assert_eq!(committed.deletions, 1);
    let states = f.file_states(committed.commit_id);
    assert_eq!(states[0].2.as_deref(), Some("DELETED"));
    assert!(!f.file_row_exists("never-staged.txt"));
}

/// A second `file rm` reports success and changes nothing but the owner
/// and the timestamp. Python's does too — its upsert has no guard on the
/// state it is replacing.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_second_deletion_is_a_noop_that_still_reports_success() {
    let mut f = fixture!("wp-rm-twice");
    f.branch("main");
    let (first_id, first) = f.session();
    let (second_id, second) = f.session();

    f.set_as("twice.txt", "x\n", Some(first), true).unwrap();
    let file_id = f.file_id("twice.txt");
    f.rm("twice.txt", first).unwrap();

    let again = f.rm("twice.txt", second).unwrap();
    assert_eq!(
        again.previous_state.as_deref(),
        Some("deleted"),
        "the outcome is what tells the caller this changed nothing"
    );
    let row = f.staged_row(file_id).unwrap();
    assert_eq!(row.1, "deleted");
    assert_eq!(
        row.3,
        Some(second_id),
        "the second session takes the row over, as the auto-stage does"
    );
    assert_ne!(row.3, Some(first_id));
}

/// A soft-deleted file can still be staged for deletion, because
/// Python's lookup returns rows of any status and says so in its own
/// doc comment.
///
/// Not meaningless: the `project_files` row is still there and the
/// commit is what removes it, so staging one is how a soft-deleted row
/// is finally retired. Reproduced rather than tightened — refusing here
/// would make this port the only thing that cannot clear that state.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_soft_deleted_file_can_still_be_staged_for_deletion() {
    let mut f = fixture!("wp-rm-soft-deleted");
    f.branch("main");
    let (_, session) = f.session();

    f.set("already.txt", "gone-ish\n", None).unwrap();
    f.soft_delete("already.txt");
    assert!(
        f.read_back("already.txt").is_none(),
        "the read layer already cannot see it"
    );

    let out = f.rm("already.txt", session).unwrap();
    assert_eq!(out.previous_state, None);

    let committed = f.commit("retire it", None, session).unwrap();
    assert_eq!(committed.deletions, 1);
    assert!(
        !f.file_row_exists("already.txt"),
        "and now the row is actually gone"
    );
}

/// `file rm` records no `edit_intents` row, where every `file set`
/// records one.
///
/// Measured on a copy of the live database 2026-11-18 — 1,647 intents
/// before Python's `file rm` and 1,647 after — and reproduced rather
/// than improved. A deletion has no provenance in either
/// implementation, which is an asymmetry worth stating; inventing a
/// row would put a `status` value in the column that nothing else in
/// either implementation writes.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_staged_deletion_records_no_intent() {
    let mut f = fixture!("wp-rm-no-intent");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("tracked.txt", "one\n", Some(session), true)
        .unwrap();
    let before = f.intents("tracked.txt");
    assert_eq!(before.len(), 1, "the write recorded its own");

    f.rm("tracked.txt", session).unwrap();
    assert_eq!(
        f.intents("tracked.txt"),
        before,
        "the deletion adds nothing and changes nothing"
    );
}

/// A path with no `project_files` row is refused, and nothing is
/// written. Python's status is 1 and so is this one.
///
/// `file rm` stages a row that exists; it does not create one. That is
/// the opposite of `intent create`, where a proposal against an
/// unregistered path is legal on both sides because `apply` is where
/// the missing row becomes an error.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn removing_a_path_with_no_row_is_refused_and_writes_nothing() {
    let mut f = fixture!("wp-rm-missing");
    f.branch("main");
    let (_, session) = f.session();

    let err = f.rm("no/such/file.txt", session).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::USAGE);
    let message = format!("{err:#}");
    assert!(
        message.contains("no/such/file.txt"),
        "the refusal must name the path: {message}"
    );
    assert!(
        !f.file_row_exists("no/such/file.txt"),
        "nothing was created"
    );
}

/// A project with no active and no default branch is refused, which is
/// Python's `No active branch found for project '<name>'` at the same
/// status 1.
///
/// Measured on a copy 2026-11-18 against `feedback`, which has eight
/// active files and zero branches — so this is a live shape and not a
/// constructed one. `file set` *tolerates* the same project, staging
/// nothing and saying so, because it has content to write either way;
/// this command has nothing left to do, which is why the two differ.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_project_with_no_branch_refuses_the_deletion() {
    let mut f = fixture!("wp-rm-no-branch");
    let (_, session) = f.session();
    f.set("orphan.txt", "no branch here\n", None).unwrap();

    let err = f.rm("orphan.txt", session).unwrap_err();
    assert_eq!(templedb_db::code_of(&err), templedb_db::USAGE);
    let message = format!("{err:#}");
    assert!(
        message.contains("no active branch"),
        "the refusal must say what is missing: {message}"
    );
    // And the file is untouched, which is the part a caller cares
    // about: a refusal here must not look like a half-done delete.
    assert_eq!(
        f.read_back("orphan.txt").as_deref(),
        Some("no branch here\n")
    );
}

/// The deletion is visible to `vcs commit-staged`'s *count* as well as
/// to its effect: a session that staged one write and one deletion
/// commits two files, and only the deletion hard-deletes.
///
/// Cross-read through the ported commit rather than through a query,
/// which is the rule the whole file follows. The reason this is a
/// separate test from the loop above is that the loop stages one file
/// and this one proves the deletion does not swallow its neighbours —
/// the bug shape a `DELETE` with a loose `WHERE` would produce.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_deletion_commits_alongside_an_ordinary_write() {
    let mut f = fixture!("wp-rm-with-neighbour");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("keep.txt", "keep me\n", Some(session), true)
        .unwrap();
    f.set_as("drop.txt", "drop me\n", Some(session), true)
        .unwrap();
    f.rm("drop.txt", session).unwrap();

    let out = f.commit("one of each", None, session).unwrap();
    assert_eq!(out.files, 2);
    assert_eq!(out.deletions, 1);

    assert_eq!(f.read_back("keep.txt").as_deref(), Some("keep me\n"));
    assert!(f.file_row_exists("keep.txt"));
    assert!(f.read_back("drop.txt").is_none());
    assert!(!f.file_row_exists("drop.txt"));
}

// --- `file mark`, the seventh write -----------------------------------
//
// The strongest cross-read available to any write in this port, and it
// is a property of the column rather than of the test: `file ls -l`
// prints `edit_mode` and `file ls --edit-mode` filters on it, and the
// sweep byte-diffs both against Python. So `mark_through_ls` below
// reads the write back through the *same function* the parity run
// verifies, which no other write here can do — `vcs_working_state` has
// only the commit, and `edit_intents` only `intent show`, and neither
// of those readers existed before the writer it verifies.
//
// No `MARK_INSTANT` beside the other two constants, and that absence is
// the point: `mark_file` takes no clock. See `filemark.rs` for the
// trigger that moves the one timestamp Python's statement does not name.

impl Fixture {
    /// Set a file's edit mode through the ported command.
    fn mark(
        &mut self,
        path: &str,
        mode: templedb_db::EditMode,
    ) -> anyhow::Result<templedb_db::MarkOutcome> {
        let project_id = self.project_id;
        let mut tx = self.writer.transaction()?;
        let outcome = templedb_db::mark_file(&mut tx, project_id, path, mode);
        match outcome {
            Ok(o) => {
                tx.commit()?;
                Ok(o)
            }
            Err(e) => {
                drop(tx);
                Err(e)
            }
        }
    }

    /// One file's `edit_mode` **through `file ls`**, as the reader
    /// renders it.
    ///
    /// `templedb_db::list_files`, not SQL written for this test, and the
    /// rendering rule applied on top: `file ls -l` prints a NULL as the
    /// word `immutable`. So the value this returns is the string a
    /// caller sees in a listing, which is what the sweep compares.
    ///
    /// `None` means the reader does not list the file at all — a
    /// `status = 'deleted'` row, which is the one case where this write
    /// succeeds and no reader can observe it.
    fn mode_through_ls(&self, path: &str) -> Option<String> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::list_files(&store, &self.slug, "")
            .unwrap()
            .into_iter()
            .find(|f| f.path == path)
            .map(|f| f.edit_mode.unwrap_or_else(|| "immutable".to_string()))
    }

    /// The paths `file ls --edit-mode <filter>` would list.
    ///
    /// The second of the two readers, and the one that makes the
    /// `immutable`-is-NULL asymmetry observable rather than theoretical.
    fn ls_filtered(&self, filter: templedb_db::EditModeFilter) -> Vec<String> {
        let url = scratch_url().unwrap();
        let store = templedb_db::PgStore::connect(&url).unwrap();
        templedb_db::list_files(&store, &self.slug, "")
            .unwrap()
            .into_iter()
            .filter(|f| filter.keeps(f.edit_mode.as_deref()))
            .map(|f| f.path)
            .collect()
    }
}

/// The write lands and **`file ls` sees it**, both ways.
///
/// The test the step exists for. `-l`'s rendered column moves, and the
/// `--edit-mode hot-reload` filter starts selecting the file — and
/// neither of those readers was written for this test or for this
/// command. Measured against Python on the copy: `file ls templedb-rust
/// -l --edit-mode hot-reload` printed one row, `    26 loc  hot-reload
/// .gitignore`, after the same write.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_mark_is_visible_to_both_of_file_ls_readers() {
    let mut f = fixture!("wp-mark-visible");
    f.set("hot.txt", "reload me\n", None).unwrap();
    f.set("cold.txt", "leave me\n", None).unwrap();

    assert_eq!(
        f.mode_through_ls("hot.txt").as_deref(),
        Some("immutable"),
        "an unmarked row is NULL and `file ls -l` prints the word"
    );
    assert!(f
        .ls_filtered(templedb_db::EditModeFilter::Exactly("hot-reload"))
        .is_empty());

    let out = f.mark("hot.txt", templedb_db::EditMode::HotReload).unwrap();
    assert_eq!(out.file_id, f.file_id("hot.txt"));
    assert_eq!(out.previous, None, "it had never been marked");

    assert_eq!(f.mode_through_ls("hot.txt").as_deref(), Some("hot-reload"));
    assert_eq!(
        f.mode_through_ls("cold.txt").as_deref(),
        Some("immutable"),
        "the neighbour must not move: the UPDATE's self-join is on the primary key"
    );
    assert_eq!(
        f.ls_filtered(templedb_db::EditModeFilter::Exactly("hot-reload")),
        vec!["hot.txt".to_string()]
    );
    assert_eq!(
        f.ls_filtered(templedb_db::EditModeFilter::NonImmutable),
        vec!["hot.txt".to_string()],
        "`non-immutable` is the filter that catches anything opted out"
    );
}

/// Marking back to `immutable` writes a NULL, and the row then fails
/// the filter of the same name.
///
/// Both halves are Python's and neither is obvious from one side alone.
/// The write is `None if args.edit_mode == 'immutable' else ...`; the
/// filter is a raw column comparison, so `--edit-mode immutable`
/// matches the string and not the NULL that means it. A port that
/// stored the word would pass a round-trip test and diverge from
/// Python's listing on every row it had written.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn immutable_is_stored_as_a_null_and_the_filter_of_that_name_misses_it() {
    let mut f = fixture!("wp-mark-immutable");
    f.set("x.txt", "hi\n", None).unwrap();

    f.mark("x.txt", templedb_db::EditMode::LiveSymlink).unwrap();
    assert_eq!(f.mode_through_ls("x.txt").as_deref(), Some("live-symlink"));

    let back = f.mark("x.txt", templedb_db::EditMode::Immutable).unwrap();
    assert_eq!(
        back.previous.as_deref(),
        Some("live-symlink"),
        "the statement returns the value it replaced"
    );
    assert_eq!(
        f.mode_through_ls("x.txt").as_deref(),
        Some("immutable"),
        "`file ls -l` renders the NULL as the word"
    );
    assert!(
        f.ls_filtered(templedb_db::EditModeFilter::Exactly("immutable"))
            .is_empty(),
        "and `--edit-mode immutable` lists nothing, because it compares the raw column"
    );
    assert!(f
        .ls_filtered(templedb_db::EditModeFilter::NonImmutable)
        .is_empty());
}

/// The single statement returns the value it replaced, across a chain
/// of marks.
///
/// Python prints `old → new` from a `SELECT` issued before its
/// `UPDATE`; this port's `RETURNING was.edit_mode` is the same value
/// from one command. The chain is what checks it rather than a single
/// transition: an implementation that returned the *new* value would
/// agree with this test on the first mark and on nothing after it.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn the_returned_previous_value_is_the_one_being_overwritten() {
    let mut f = fixture!("wp-mark-chain");
    f.set("c.txt", "hi\n", None).unwrap();

    let steps = [
        (templedb_db::EditMode::HotReload, None),
        (
            templedb_db::EditMode::LiveSymlink,
            Some("hot-reload".to_string()),
        ),
        (
            templedb_db::EditMode::Immutable,
            Some("live-symlink".to_string()),
        ),
        (templedb_db::EditMode::HotReload, None),
    ];
    for (mode, expected_previous) in steps {
        let out = f.mark("c.txt", mode).unwrap();
        assert_eq!(out.previous, expected_previous, "marking {mode:?}");
    }
}

/// Re-marking a file to the mode it already has succeeds and reports
/// the transition as a no-op.
///
/// Python's write is unconditional — measured on the copy, the second
/// `--edit-mode hot-reload` printed `edit_mode hot-reload →
/// hot-reload`, exited 0 and still fired the `updated_at` trigger.
/// Reproduced rather than short-circuited: a port that skipped the
/// statement would print the same line and the two databases would
/// disagree on a column neither of them mirrors, which is the worst
/// available combination.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn re_marking_the_same_mode_is_a_noop_that_still_reports_success() {
    let mut f = fixture!("wp-mark-noop");
    f.set("n.txt", "hi\n", None).unwrap();

    f.mark("n.txt", templedb_db::EditMode::HotReload).unwrap();
    let again = f.mark("n.txt", templedb_db::EditMode::HotReload).unwrap();
    assert_eq!(again.previous.as_deref(), Some("hot-reload"));
    assert_eq!(
        templedb_db::mark_line(
            "p",
            "n.txt",
            again.previous.as_deref(),
            templedb_db::EditMode::HotReload
        ),
        "\u{2713} p/n.txt: edit_mode hot-reload \u{2192} hot-reload"
    );
    assert_eq!(f.mode_through_ls("n.txt").as_deref(), Some("hot-reload"));
}

/// A soft-deleted row is markable, and the write is then invisible to
/// every reader this port has.
///
/// Python's lookup has no `status` filter and its own `get_file_by_path`
/// doc comment says rows of any status come back. Measured on the copy:
/// `templedb file mark templedb-rust src/main.py --edit-mode
/// live-symlink` printed its success line, exited 0, wrote the column —
/// and `file ls -l --edit-mode live-symlink` listed nothing, because
/// `file ls` filters on `status`.
///
/// This is the only success in the seven writes with no cross-read
/// available, and it is asserted *as* that: the column moves, read
/// through the writer's own `RETURNING`, and the reader sees no file.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_soft_deleted_row_is_markable_and_no_reader_can_see_it() {
    let mut f = fixture!("wp-mark-soft-deleted");
    f.set("ghost.txt", "boo\n", None).unwrap();
    let file_id = f.file_id("ghost.txt");
    f.soft_delete("ghost.txt");
    assert_eq!(
        f.mode_through_ls("ghost.txt"),
        None,
        "`file ls` does not list a soft-deleted row at all"
    );

    let out = f
        .mark("ghost.txt", templedb_db::EditMode::LiveSymlink)
        .unwrap();
    assert_eq!(out.file_id, file_id, "the row was found despite its status");
    assert_eq!(out.previous, None);

    assert_eq!(
        f.mode_through_ls("ghost.txt"),
        None,
        "and still does not, so the write succeeded unobservably"
    );
    // The un-delete that would make it visible is `file set`'s, and it
    // leaves the column alone — so the mode survives the revival.
    f.set("ghost.txt", "back\n", None).unwrap();
    assert_eq!(
        f.mode_through_ls("ghost.txt").as_deref(),
        Some("live-symlink"),
        "`file set`'s un-delete names `status` and not `edit_mode`"
    );
}

/// A path with no row is refused, and the refusal writes nothing.
///
/// Python's `return 1` after `File not found:`, which this port reaches
/// as [`USAGE`]. The claim worth pinning is the second one: the lookup
/// and the write are **one statement** here, so there is no ordering to
/// get wrong — a `RETURNING` with no row is both the refusal and the
/// proof that nothing was written.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn marking_a_path_with_no_row_is_refused() {
    let mut f = fixture!("wp-mark-missing");
    f.set("real.txt", "hi\n", None).unwrap();

    let err = f
        .mark("imaginary.txt", templedb_db::EditMode::HotReload)
        .expect_err("there is no such file");
    assert_eq!(templedb_db::code_of(&err), templedb_db::USAGE);
    let msg = format!("{err:#}");
    assert!(msg.contains("imaginary.txt"), "{msg}");
    assert!(msg.contains("does not create one"), "{msg}");

    assert_eq!(
        f.mode_through_ls("real.txt").as_deref(),
        Some("immutable"),
        "the neighbour is untouched"
    );
}

/// A mark names one project's row, not every row with that path.
///
/// `project_files` is `UNIQUE (project_id, file_path)` and the fixture
/// deliberately writes the same path into two projects, which is the
/// shape that catches a `WHERE file_path = $1` missing its companion —
/// the same mistake `README.md in two projects` exists for in the
/// read-side fixture.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_mark_is_scoped_to_one_project() {
    let mut a = fixture!("wp-mark-scope-a");
    let mut b = fixture!("wp-mark-scope-b");
    a.set("shared.txt", "mine\n", None).unwrap();
    b.set("shared.txt", "mine too\n", None).unwrap();

    a.mark("shared.txt", templedb_db::EditMode::HotReload)
        .unwrap();

    assert_eq!(
        a.mode_through_ls("shared.txt").as_deref(),
        Some("hot-reload")
    );
    assert_eq!(
        b.mode_through_ls("shared.txt").as_deref(),
        Some("immutable"),
        "the other project's row must not move"
    );
}

/// **What the single statement buys under a concurrent write**,
/// measured, and with the case it does *not* measure named.
///
/// What it measures: a transaction that read the row, then had the row
/// changed under it by another transaction, then marked it. Python's
/// shape — `SELECT`, decide, `UPDATE` — would print the value from its
/// own earlier read, which is a transition that never happened. The
/// single statement reads the `from` side in the same snapshot as the
/// write, so it reports what it actually overwrote.
///
/// What it does **not** measure, stated because the mechanism is easy
/// to claim and hard to produce: the interleaving where A commits while
/// B's `UPDATE` is already blocked on the row lock. That one goes
/// through Postgres's `EvalPlanQual` recheck, and reaching it needs two
/// threads rather than two transactions driven in sequence. Here A
/// commits *before* B's statement is issued, so B's per-statement
/// snapshot — this is READ COMMITTED — already includes the commit and
/// both the target scan and the `FROM` self-join see it directly. No
/// recheck is involved and none is asserted.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_racing_mark_reports_the_value_it_actually_replaced() {
    let mut f = fixture!("wp-mark-race");
    f.set("r.txt", "hi\n", None).unwrap();
    f.mark("r.txt", templedb_db::EditMode::HotReload).unwrap();
    let project_id = f.project_id;

    // B's transaction opens before A's write lands, so its snapshot
    // predates it. The `UPDATE` itself is issued afterwards.
    let url = scratch_url().unwrap();
    let mut other = PgWriter::connect(&url).unwrap();
    let mut b_tx = other.transaction().unwrap();
    // Force the snapshot to be taken now rather than at the UPDATE:
    // READ COMMITTED takes a fresh snapshot per statement, so without a
    // read here "B opened first" would not mean anything.
    let seen: Option<String> = b_tx
        .query_one(
            "SELECT edit_mode FROM project_files
              WHERE project_id = $1 AND file_path = 'r.txt'",
            &[&project_id],
        )
        .unwrap()
        .get(0);
    assert_eq!(seen.as_deref(), Some("hot-reload"));

    // A commits a different mode.
    f.mark("r.txt", templedb_db::EditMode::LiveSymlink).unwrap();

    // B now marks. Its own snapshot says `hot-reload`; the row says
    // `live-symlink`.
    let out = templedb_db::mark_file(
        &mut b_tx,
        project_id,
        "r.txt",
        templedb_db::EditMode::Immutable,
    )
    .unwrap();
    b_tx.commit().unwrap();

    // Measured 2026-11-18: `live-symlink`. Both halves of the statement
    // read the same per-statement snapshot, which already holds A's
    // commit, so `RETURNING was.edit_mode` is the value this command
    // actually overwrote and not the one B's earlier SELECT saw. That is
    // the property the single statement was written for: Python's shape
    // — and a `SELECT` then `UPDATE` in this same transaction — would
    // have printed `hot-reload → immutable`, a transition that never
    // happened.
    assert_eq!(
        out.previous.as_deref(),
        Some("live-symlink"),
        "the reported `from` must be the value overwritten, not the one read earlier"
    );
    assert_eq!(f.mode_through_ls("r.txt").as_deref(), Some("immutable"));
}

/// The mark survives a commit, and the commit does not move it.
///
/// `edit_mode` is not in `vcs_file_states` and not in anything
/// `vcs commit-staged` writes, so this is a check that the two writes
/// are independent — the one claim about a new column that is cheap to
/// get wrong and expensive to notice, since the commit's `UPDATE
/// project_files` for a deletion is a hard `DELETE`.
#[test]
#[ignore = "needs a Postgres scratch database; run with `just write-test`"]
fn a_marked_file_keeps_its_mode_across_a_commit() {
    let mut f = fixture!("wp-mark-commit");
    f.branch("main");
    let (_, session) = f.session();

    f.set_as("m.txt", "one\n", Some(session), true).unwrap();
    f.mark("m.txt", templedb_db::EditMode::HotReload).unwrap();
    f.commit("with a mode", None, session).unwrap();

    assert_eq!(f.read_back("m.txt").as_deref(), Some("one\n"));
    assert_eq!(
        f.mode_through_ls("m.txt").as_deref(),
        Some("hot-reload"),
        "the commit writes six tables and `project_files.edit_mode` is in none of them"
    );

    // And a second write to the same file leaves it alone too: `file
    // set`'s upsert names `status` and never this column.
    f.set("m.txt", "two\n", None).unwrap();
    assert_eq!(f.mode_through_ls("m.txt").as_deref(), Some("hot-reload"));
}
