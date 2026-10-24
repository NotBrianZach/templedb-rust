//! `file set` against a real Postgres, verified by reading it back.
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
        // every row.
        let opts = templedb_db::SetOptions {
            if_match,
            allow_mojibake: false,
            skip_intent: false,
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

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(mut tx) = self.writer.transaction() {
            let _ = tx.execute("DELETE FROM projects WHERE slug = $1", &[&self.slug]);
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
