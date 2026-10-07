//! `templedb source ...` — read-only observations of file state at a
//! revision, over the `source_snapshots` view.
//!
//! The view is a `UNION ALL` of two very different things: the current
//! state (from `file_contents`, `revision = 'current'`) and every
//! historical state (from `vcs_file_states`, `revision = commit_hash`).
//! The historical half `LEFT JOIN`s `content_blobs`, so a row can name a
//! `content_hash` whose blob is gone — and `vcs_file_states.content_hash`
//! is itself nullable, which is how a `deleted` change type is recorded.
//! Both cases are handled explicitly here; see `SnapshotBody`.

use crate::like::escape_like;
use anyhow::{Context, Result};
use rusqlite::Connection;

/// Metadata for one snapshot. Deliberately does not carry the content:
/// `--meta` has no use for a 5 MB blob, and the body is fetched
/// separately by hash when it is actually wanted.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub project_slug: String,
    pub file_path: String,
    /// Normalised to lowercase for display, as the Python side does.
    /// Historical commits are bimodal — older 16-char uppercase, newer
    /// 40-char lowercase — and the raw value is left alone in the
    /// database for provenance. `current` is a literal, not a hash.
    pub revision: String,
    pub content_hash: Option<String>,
    pub file_size_bytes: Option<i64>,
    pub line_count: Option<i64>,
    pub observed_at: String,
    pub source_authority: String,
}

impl Snapshot {
    pub fn is_current(&self) -> bool {
        self.revision == "current"
    }
}

/// What came back for a snapshot's bytes.
#[derive(Debug, Clone)]
pub enum SnapshotBody {
    Text(String),
    Binary(Vec<u8>),
}

/// Resolve `<slug>/<path>` at a revision to its metadata.
///
/// `rev = None` means the current state. A revision is matched
/// case-insensitively by prefix, newest first, because the hashes are
/// bimodal in case and length and the user types whatever prefix they
/// saw in `vcs log`.
pub fn snapshot(conn: &Connection, slug: &str, path: &str, rev: Option<&str>) -> Result<Snapshot> {
    let row = match rev {
        None | Some("current") => conn
            .query_row(
                "SELECT revision, content_hash, file_size_bytes, line_count,
                        observed_at, source_authority
                   FROM source_snapshots
                  WHERE project_slug = ?1
                    AND file_path = ?2
                    AND revision = 'current'
                  LIMIT 1",
                (slug, path),
                read_snapshot_row,
            )
            .optional_row()?,
        Some(r) => conn
            .query_row(
                // The prefix is escaped for LIKE: a commit hash is hex,
                // but the user's input is not validated anywhere and a
                // `%` would otherwise match every revision and silently
                // return the newest one.
                "SELECT revision, content_hash, file_size_bytes, line_count,
                        observed_at, source_authority
                   FROM source_snapshots
                  WHERE project_slug = ?1
                    AND file_path = ?2
                    AND revision != 'current'
                    AND UPPER(revision) LIKE UPPER(?3) || '%' ESCAPE '\\'
                  ORDER BY observed_at DESC, revision
                  LIMIT 1",
                (slug, path, escape_like(r)),
                read_snapshot_row,
            )
            .optional_row()?,
    };

    let Some((revision, content_hash, file_size_bytes, line_count, observed_at, authority)) = row
    else {
        match rev {
            Some(r) => anyhow::bail!(
                "no snapshot for {slug}/{path} at revision {r:?} \
                 (try `templedb-rs source revisions {slug} {path}`)"
            ),
            None => anyhow::bail!(
                "no current snapshot for {slug}/{path} \
                 (deleted, or not yet ingested)"
            ),
        }
    };

    Ok(Snapshot {
        project_slug: slug.to_string(),
        file_path: path.to_string(),
        revision: normalise_revision(&revision),
        content_hash,
        file_size_bytes,
        line_count,
        observed_at,
        source_authority: authority,
    })
}

/// Fetch the bytes for a resolved snapshot.
///
/// Errors rather than returning empty when the blob is unreachable.
/// The Python version falls through both of its branches in that case
/// and writes nothing with exit status 0, so a missing blob is
/// indistinguishable from an empty file — the single worst outcome for
/// a command whose output gets piped into `sha256sum` or a patch.
pub fn snapshot_body(conn: &Connection, snap: &Snapshot) -> Result<SnapshotBody> {
    let Some(hash) = snap.content_hash.as_deref() else {
        anyhow::bail!(
            "{}/{} at {} records no content hash \
             (a deletion, not a file state)",
            snap.project_slug,
            snap.file_path,
            snap.revision
        );
    };
    let row: Option<(Option<String>, Option<Vec<u8>>, String)> = conn
        .query_row(
            "SELECT content_text, content_blob, content_type
               FROM content_blobs WHERE hash_sha256 = ?1",
            [hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional_row()?;

    let Some((text, blob, content_type)) = row else {
        anyhow::bail!(
            "{}/{} at {}: blob {} is not in content_blobs \
             (pruned or never ingested)",
            snap.project_slug,
            snap.file_path,
            snap.revision,
            &hash[..hash.len().min(12)]
        );
    };

    // `content_type` is advisory; the columns are the truth. Text first,
    // matching the Python precedence, so a row mislabelled 'binary' that
    // does carry text still prints as text.
    match (text, blob) {
        (Some(t), _) => Ok(SnapshotBody::Text(t)),
        (None, Some(b)) => Ok(SnapshotBody::Binary(b)),
        (None, None) => anyhow::bail!(
            "{}/{} at {}: blob {} has content_type {content_type:?} \
             but neither content_text nor content_blob",
            snap.project_slug,
            snap.file_path,
            snap.revision,
            &hash[..hash.len().min(12)]
        ),
    }
}

/// One row of `source revisions`.
#[derive(Debug, Clone)]
pub struct Revision {
    pub revision: String,
    pub content_hash: Option<String>,
    pub observed_at: String,
    pub file_size_bytes: Option<i64>,
    pub line_count: Option<i64>,
}

/// Every known revision of a file, newest first — the prelude to
/// `snapshot --rev`.
pub fn revisions(conn: &Connection, slug: &str, path: &str) -> Result<Vec<Revision>> {
    let mut stmt = conn.prepare(
        // The tie-breaks are not decoration. A commit and the current
        // state routinely share an `observed_at` to the second — the
        // commit is what produced the current state — and Python orders
        // on `observed_at DESC` alone, so which of the two prints first
        // is up to the query planner. `current` wins a tie because it
        // is the state the file is actually in; `revision` breaks the
        // rest so two runs agree.
        "SELECT revision, content_hash, observed_at, file_size_bytes, line_count
           FROM source_snapshots
          WHERE project_slug = ?1 AND file_path = ?2
          ORDER BY observed_at DESC, (revision = 'current') DESC, revision",
    )?;
    let rows = stmt.query_map((slug, path), |r| {
        Ok(Revision {
            revision: normalise_revision(&r.get::<_, String>(0)?),
            content_hash: r.get(1)?,
            observed_at: r.get(2)?,
            file_size_bytes: r.get(3)?,
            line_count: r.get(4)?,
        })
    })?;
    let out = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("listing revisions")?;
    if out.is_empty() {
        anyhow::bail!("no snapshots for {slug}/{path}");
    }
    Ok(out)
}

fn normalise_revision(raw: &str) -> String {
    if raw == "current" {
        raw.to_string()
    } else {
        raw.to_lowercase()
    }
}

#[allow(clippy::type_complexity)]
fn read_snapshot_row(
    r: &rusqlite::Row<'_>,
) -> rusqlite::Result<(
    String,
    Option<String>,
    Option<i64>,
    Option<i64>,
    String,
    String,
)> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
    ))
}

/// `QueryReturnedNoRows` means "absent", not "broken". rusqlite has
/// `OptionalExtension` for this; a local trait keeps the import list
/// from suggesting the whole extension surface is in use.
trait OptionalRow<T> {
    fn optional_row(self) -> rusqlite::Result<Option<T>>;
}

impl<T> OptionalRow<T> for rusqlite::Result<T> {
    fn optional_row(self) -> rusqlite::Result<Option<T>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    #[test]
    fn current_snapshot_is_the_default() {
        let c = testdb::fixture();
        let s = snapshot(&c, "alpha", "src/main.rs", None).unwrap();
        assert!(s.is_current());
        assert_eq!(s.line_count, Some(3));
        assert_eq!(s.source_authority, "git");
        match snapshot_body(&c, &s).unwrap() {
            SnapshotBody::Text(t) => assert_eq!(t, "fn main() {}\nsecond\nthird\n"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    /// Prefix match, case-insensitive, against an uppercase stored hash.
    #[test]
    fn revision_prefix_matches_either_case() {
        let c = testdb::fixture();
        for probe in ["ABCD", "abcd", "AbCd1234"] {
            let s = snapshot(&c, "alpha", "src/main.rs", Some(probe)).unwrap();
            // Stored uppercase, reported lowercase.
            assert_eq!(s.revision, "abcd1234efff", "probe {probe}");
            assert_eq!(s.line_count, Some(1));
        }
    }

    /// A `%` in the revision argument must not match everything.
    #[test]
    fn revision_prefix_wildcards_are_literal() {
        let c = testdb::fixture();
        let err = snapshot(&c, "alpha", "src/main.rs", Some("%"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("no snapshot"), "{err}");
    }

    #[test]
    fn unknown_revision_points_at_the_revisions_command() {
        let c = testdb::fixture();
        let err = snapshot(&c, "alpha", "src/main.rs", Some("deadbeef"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("source revisions"), "{err}");
    }

    #[test]
    fn revisions_lists_current_and_history_newest_first() {
        let c = testdb::fixture();
        let revs = revisions(&c, "alpha", "src/main.rs").unwrap();
        let names: Vec<_> = revs.iter().map(|r| r.revision.as_str()).collect();
        assert_eq!(names, vec!["current", "abcd1234efff"]);
        assert!(revisions(&c, "alpha", "nope.txt").is_err());
    }

    /// The Python bug this port does not reproduce: a historical row
    /// whose blob is missing printed nothing and exited 0.
    #[test]
    fn missing_blob_is_an_error_not_an_empty_file() {
        let c = testdb::fixture();
        let s = snapshot(&c, "beta", "orphan.txt", Some("cafe")).unwrap();
        let err = snapshot_body(&c, &s).unwrap_err().to_string();
        assert!(err.contains("not in content_blobs"), "{err}");
    }

    /// `vcs_file_states.content_hash` is NULL for a deletion. Python
    /// indexes `[:12]` into it and raises a TypeError in `revisions`.
    #[test]
    fn deletion_rows_have_no_hash_and_do_not_panic() {
        let c = testdb::fixture();
        let revs = revisions(&c, "beta", "removed.txt").unwrap();
        assert!(revs.iter().any(|r| r.content_hash.is_none()), "{revs:?}");
        let s = snapshot(&c, "beta", "removed.txt", Some("dead")).unwrap();
        let err = snapshot_body(&c, &s).unwrap_err().to_string();
        assert!(err.contains("no content hash"), "{err}");
    }

    #[test]
    fn binary_snapshots_come_back_as_bytes() {
        let c = testdb::fixture();
        let s = snapshot(&c, "beta", "logo.png", None).unwrap();
        match snapshot_body(&c, &s).unwrap() {
            SnapshotBody::Binary(b) => assert_eq!(b, vec![0x89, 0x50, 0x4e, 0x47]),
            other => panic!("expected bytes, got {other:?}"),
        }
    }
}
