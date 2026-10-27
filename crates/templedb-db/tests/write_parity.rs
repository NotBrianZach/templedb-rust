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
