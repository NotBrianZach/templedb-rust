//! `file set` — the port's first write.
//!
//! Chosen by the write-path design document over `vcs commit`, and the
//! reasoning is worth keeping in view: `commit` is "the widest door in
//! the system — workspace scan, three guards, conflict detection with
//! resolution strategies, session head bookkeeping, commit metadata,
//! and per-file writes across four tables". This is one logical change,
//! no workspace scan, no resolution strategies, and its entire effect
//! is visible through read commands the port already verifies byte for
//! byte — `file cat`, `source revisions`, `search content`. That last
//! property is the one that matters, because it is the only
//! verification technique available without new machinery: the document
//! calls it *cross-read*.
//!
//! # What this does NOT do
//!
//! Stated first, because the design document's sharpest finding is that
//! **the guards are the deliverable** — "a port that reproduces the SQL
//! and not these is a regression even if every byte of output matches".
//! This reproduces the SQL and two of the guards. The rest are absent,
//! and the writer is behind an opt-in
//! ([`OPT_IN_ENV`](crate::OPT_IN_ENV)) precisely because that is not
//! visible from a command line:
//!
//! | absent | what it protects against |
//! |---|---|
//! | the mojibake guard | UTF-8 read as Latin-1 and written back; valid UTF-8 all the way down, so nothing downstream notices. `bza`'s `page.tsx` shipped that way for two weeks |
//! | `edit_intents` | provenance — who set this, when, from which session. Its table is outside the mirrored subset |
//! | auto-staging and `--stage` | needs sessions, which the port does not have |
//! | `--commit` / `-m` | needs `vcs commit`, which is the step after this one |
//! | `--verify` | reads back `is_current` after the write. Cheap to add and not yet added |
//! | `deploy_stage_runs` | chain observability; outside the subset |
//! | binary content | Python falls back to `content_blob` for undecodable stdin. This takes text only |
//!
//! Two guards *are* here: `--if-match` optimistic concurrency, and the
//! un-delete that revives a soft-deleted row. Both are below, with the
//! incidents that produced them.
//!
//! # Postgres, not the live SQLite
//!
//! This writes the port's own disposable Postgres. Python keeps sole
//! authority over `~/.local/share/templedb/templedb.sqlite`, which is
//! the arrangement every earlier step of the port was built to
//! preserve. A wrong write here costs a `just mirror-refresh`.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::pgtext;

/// What a write did, for the caller to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetOutcome {
    /// sha256 of the content now stored.
    pub hash: String,
    /// The previous `is_current` hash, if the file had one.
    pub previous: Option<String>,
    /// True when the content was already what is stored, so the write
    /// changed nothing. Python's `deploy_stage_runs` classifies this as
    /// `noop`; the port reports it rather than recording it.
    pub unchanged: bool,
    /// True when `project_files.status` was revived from a non-active
    /// value.
    pub undeleted: bool,
    /// True when the file did not exist and a `project_files` row was
    /// created.
    pub created: bool,
    pub size_bytes: i64,
    pub line_count: i64,
}

/// sha256 of the UTF-8 bytes, lowercase hex — the same digest Python
/// takes, over the same bytes.
pub fn content_hash(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    // Hex by hand rather than `{:x}` on the output: sha2 0.11 returns a
    // `hybrid_array::Array`, which does not implement `LowerHex`. The
    // zero-padded two-digit form is the one Python's `hexdigest()`
    // produces and the one `content_blobs.hash_sha256` stores.
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Python's line count: `content.count('\n') + 1`, and 0 for empty.
///
/// Not the number of lines by any ordinary definition — a file ending
/// in a newline counts the empty string after it — but it is the number
/// `file ls -l` and `source revisions` print, so it is the number to
/// reproduce.
pub fn line_count(content: &str) -> i64 {
    if content.is_empty() {
        0
    } else {
        content.matches('\n').count() as i64 + 1
    }
}

/// Validate an `--if-match` argument.
///
/// Python accepts a prefix of at least 8 hex characters, lowercased,
/// "because every place that shows a hash truncates it — `file where`,
/// `source snapshot --meta`, doctor output — so requiring 64 characters
/// would mean nobody uses the flag".
pub fn normalise_if_match(expected: &str) -> Result<String> {
    let exp = expected.trim().to_ascii_lowercase();
    if exp.len() < 8 || !exp.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("--if-match wants at least 8 hex chars of a sha256, got {expected:?}");
    }
    Ok(exp)
}

/// Apply the `--if-match` guard against the hash currently stored.
///
/// # Why this guard exists
///
/// `file set` replaces the **whole** file, so content fetched earlier
/// is effectively a lock on all of it: anything written between the
/// read and the write is reverted silently, with no conflict and a
/// clean `--verify` — which only checks that what you wrote is what
/// landed, not that you meant to drop the rest.
///
/// That happened twice on 2026-10-04 to `src/cli/commands/entity.py`, a
/// 5,239-line file four separate fixes were landing in, and the first
/// instance went unnoticed for three commits.
pub fn check_if_match(expected: &str, current: Option<&str>) -> Result<()> {
    let exp = normalise_if_match(expected)?;
    match current {
        None => bail!(
            "--if-match {} given but the file has no current content — it is \
             a new file, so there is no base to match. Drop --if-match if \
             creating it is what you meant.",
            &exp[..exp.len().min(12)]
        ),
        Some(cur) if !cur.starts_with(&exp) => bail!(
            "--if-match mismatch: expected {}, the database currently holds \
             {}. Something changed the file since you read it, and writing \
             now would revert it.",
            &exp[..exp.len().min(12)],
            &cur[..cur.len().min(12)]
        ),
        Some(_) => Ok(()),
    }
}

/// The project id for an **exact** slug, in the write target.
///
/// Exact, where every read command accepts a fuzzy name. Python's
/// `file set` fuzzy-matches too, and this is a deliberate divergence in
/// the stricter direction: fuzzy resolution on a write is how content
/// lands in the wrong project, and the scoring function has two known
/// bugs wrapped around it (see the README's fuzzy section). A reader
/// that guesses wrong wastes a second; a writer that guesses wrong
/// overwrites a file.
///
/// Resolved inside the write transaction rather than through the read
/// `Store`, because the reader and the writer point at *different
/// databases* — `TEMPLEDB_URL` is the parity target and
/// `TEMPLEDB_WRITE_URL` is the scratch. Resolving a slug against one
/// and writing to the other would be a genuinely nasty class of bug.
pub fn resolve_project_exact(tx: &mut postgres::Transaction<'_>, slug: &str) -> Result<i64> {
    match tx.query_opt("SELECT id FROM projects WHERE slug = $1", &[&slug])? {
        Some(row) => Ok(row.get(0)),
        None => bail!(
            "no project with slug {slug:?} in the write target.\n\
             `file set` takes an exact slug, not a fuzzy name — unlike every \
             read command — because a mis-resolved write overwrites a file."
        ),
    }
}

/// The `is_current` content hash for one path, or `None`.
fn current_hash(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    file_path: &str,
) -> Result<Option<String>> {
    let row = tx.query_opt(
        "SELECT fc.content_hash
           FROM project_files pf
           JOIN file_contents fc ON fc.file_id = pf.id AND fc.is_current = 1
          WHERE pf.project_id = $1 AND pf.file_path = $2",
        &[&project_id, &file_path],
    )?;
    Ok(row.map(|r| r.get::<_, String>(0)))
}

/// Write `content` as the current contents of `project_id`/`file_path`.
///
/// Mirrors Python's `_write_content_to_db` across three tables, in the
/// order Python uses, inside one transaction. The ordering is not
/// incidental: the blob has to exist before a `file_contents` row
/// references its hash.
pub fn set_file(
    tx: &mut postgres::Transaction<'_>,
    clock: &Clock,
    project_id: i64,
    file_path: &str,
    content: &str,
    if_match: Option<&str>,
) -> Result<SetOutcome> {
    let previous = current_hash(tx, project_id, file_path)?;
    if let Some(expected) = if_match {
        check_if_match(expected, previous.as_deref())?;
    }

    let hash = content_hash(content);
    let size_bytes = content.len() as i64;
    let lines = line_count(content);
    let stored = pgtext::escape(content);

    // `ON CONFLICT DO NOTHING`, which is Python's `INSERT OR IGNORE`.
    // Content is addressed by its own hash, so a blob that is already
    // there is already correct and re-writing it would be a no-op that
    // churned the row.
    //
    // `encoding` and `reference_count` are absent from the mirrored
    // subset, so they are not written. Python sets them — `'utf-8'` and
    // 0, the latter so SQLite's `increment_blob_reference` trigger can
    // count the `file_contents` row that follows. Neither column is read
    // by any ported command, so the boundary still follows the readers;
    // it is recorded in the README as partial fidelity rather than left
    // to be discovered.
    tx.execute(
        "INSERT INTO content_blobs
             (hash_sha256, content_text, content_blob, content_type, file_size_bytes)
         VALUES ($1, $2, NULL, 'text', $3)
         ON CONFLICT (hash_sha256) DO NOTHING",
        &[&hash, &stored.as_ref(), &size_bytes],
    )?;

    let existing = tx.query_opt(
        "SELECT id, status FROM project_files WHERE project_id = $1 AND file_path = $2",
        &[&project_id, &file_path],
    )?;

    let (file_id, undeleted, created) = match existing {
        Some(row) => {
            let file_id: i64 = row.get(0);
            let status: Option<String> = row.get(1);
            // Un-delete. `file set` on a soft-deleted row revives it;
            // without this the new blob lands and `file ls`, `publish
            // materialize` and everything else filtering on
            // `status = 'active'` silently skips the file. Found when
            // `set bza flake.nix` looked like it succeeded and shipped
            // nothing.
            let undeleted = status.as_deref() != Some("active");
            if undeleted {
                tx.execute(
                    "UPDATE project_files SET status = 'active' WHERE id = $1",
                    &[&file_id],
                )?;
            }
            (file_id, undeleted, false)
        }
        None => {
            // `file_type_id` is left NULL rather than derived from the
            // extension. Python maps 14 extensions onto `file_types`
            // rows, and that table is outside the mirrored subset — the
            // only ported reader of the column is `project show`'s
            // `COUNT(DISTINCT file_type_id)`, which counts a NULL as
            // nothing. Inventing an id would be worse than omitting
            // one.
            let row = tx.query_one(
                "INSERT INTO project_files (project_id, file_path, status)
                 VALUES ($1, $2, 'active')
                 RETURNING id",
                &[&project_id, &file_path],
            )?;
            (row.get::<_, i64>(0), false, true)
        }
    };

    // `ON CONFLICT (file_id, is_current)`, which the mirrored schema
    // declares because the live one does. Python's `datetime('now')`
    // becomes the injected clock: the whole point of the clock seam is
    // that a write's timestamps are equal by construction rather than
    // excluded from a state diff, and excluding them would hide the
    // columns most likely to be wrong.
    let now = clock.now_timestamp();
    tx.execute(
        "INSERT INTO file_contents
             (file_id, content_hash, file_size_bytes, line_count, is_current, updated_at)
         VALUES ($1, $2, $3, $4, 1, $5)
         ON CONFLICT (file_id, is_current) DO UPDATE SET
             content_hash    = excluded.content_hash,
             file_size_bytes = excluded.file_size_bytes,
             line_count      = excluded.line_count,
             updated_at      = excluded.updated_at",
        &[&file_id, &hash, &size_bytes, &lines, &now],
    )?;

    Ok(SetOutcome {
        unchanged: previous.as_deref() == Some(hash.as_str()),
        hash,
        previous,
        undeleted,
        created,
        size_bytes,
        line_count: lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is over the UTF-8 bytes and nothing else, so it
    /// matches what Python's `hashlib.sha256(content.encode())`
    /// produces for the same string.
    #[test]
    fn the_hash_is_sha256_of_the_utf8_bytes() {
        // `printf '' | sha256sum`
        assert_eq!(
            content_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // `printf 'hello\n' | sha256sum`
        assert_eq!(
            content_hash("hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        // Multibyte hashes its bytes, not its chars.
        assert_eq!(content_hash("é").len(), 64);
        assert_ne!(content_hash("é"), content_hash("e"));
    }

    /// Python's count, including the quirk: a trailing newline adds a
    /// line, because it counts separators and adds one.
    #[test]
    fn the_line_count_reproduces_pythons_arithmetic() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 2);
        assert_eq!(line_count("a\nb"), 2);
        assert_eq!(line_count("a\nb\n"), 3);
        assert_eq!(line_count("\n"), 2);
    }

    #[test]
    fn if_match_needs_eight_hex_characters() {
        assert!(normalise_if_match("abcdef12").is_ok());
        // Uppercase is accepted and folded, since every display path
        // truncates and some of them upcase.
        assert_eq!(normalise_if_match("ABCDEF12").unwrap(), "abcdef12");
        assert_eq!(normalise_if_match("  abcdef12  ").unwrap(), "abcdef12");
        // Too short.
        assert!(normalise_if_match("abcdef1").is_err());
        // Not hex.
        assert!(normalise_if_match("abcdefgh").is_err());
        assert!(normalise_if_match("").is_err());
    }

    #[test]
    fn if_match_accepts_a_prefix_and_rejects_a_mismatch() {
        const HASH: &str = "ff4938cc03ca2b40be9d7c85e8b7001f";
        let current = Some(HASH);
        assert!(check_if_match("ff4938cc", current).is_ok());
        assert!(check_if_match("FF4938CC03CA2B40", current).is_ok());
        // The whole hash, not a prefix of it.
        assert!(check_if_match(HASH, current).is_ok());

        let err = check_if_match("deadbeef", current).unwrap_err();
        let msg = format!("{err:#}");
        // Both hashes in the message, truncated the way every other
        // display path truncates them.
        assert!(msg.contains("deadbeef"), "{msg}");
        assert!(msg.contains("ff4938cc03ca"), "{msg}");
        assert!(msg.contains("would revert it"), "{msg}");
    }

    /// `--if-match` against a file that does not exist is a refusal
    /// with its own message, not a silent create. Python separates the
    /// two because "it is a new file, so there is no base to match" is
    /// a different mistake from a stale base.
    #[test]
    fn if_match_on_a_new_file_refuses_and_says_why() {
        let err = check_if_match("abcdef12", None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no current content"), "{msg}");
        assert!(msg.contains("new file"), "{msg}");
    }

    /// A short prefix must not match a *different* hash that merely
    /// shares its first characters by accident — `starts_with` is the
    /// right test and the length minimum is what makes it safe.
    #[test]
    fn a_prefix_matches_only_a_real_prefix() {
        let current = Some("abcdef1234567890");
        assert!(check_if_match("abcdef12", current).is_ok());
        assert!(check_if_match("abcdef13", current).is_err());
        // Longer than the stored hash cannot match.
        assert!(check_if_match("abcdef1234567890ff", current).is_err());
    }
}
