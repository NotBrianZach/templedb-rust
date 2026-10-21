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
        let opts = templedb_db::SetOptions {
            if_match,
            allow_mojibake: false,
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
