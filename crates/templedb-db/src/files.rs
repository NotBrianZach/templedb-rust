//! `templedb file ...` reads.

use crate::like::escape_like;
use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub lines: i64,
}

/// Active files under `prefix` for a project, with line counts.
pub fn list_files(conn: &Connection, slug: &str, prefix: &str) -> Result<Vec<FileEntry>> {
    let mut stmt = conn.prepare(
        "SELECT pf.file_path, COALESCE(fc.line_count, 0)
           FROM project_files pf
           JOIN projects p ON p.id = pf.project_id
      LEFT JOIN file_contents fc
             ON fc.file_id = pf.id AND fc.is_current = 1
          WHERE p.slug = ?1
            AND pf.status = 'active'
            AND pf.file_path LIKE ?2 || '%' ESCAPE '\\'
          ORDER BY pf.file_path",
    )?;
    // The prefix is a literal path fragment, not a pattern: `_` is in
    // half the slugs in this database (`proj_a`, `system_config`) and
    // LIKE would treat it as "any character", so `file ls x proj_a`
    // would also list `proj-a`. Escape the three LIKE metacharacters
    // and declare the escape above.
    let pattern = escape_like(prefix);
    let rows = stmt.query_map((slug, pattern), |r| {
        Ok(FileEntry {
            path: r.get(0)?,
            lines: r.get(1)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing files")
}

/// Current contents of one tracked file.
///
/// The join through `content_blobs` is the part worth getting right:
/// `file_contents` holds the hash, not the bytes, and the same query
/// shape is what `reports.py` uses. `content_text` is NULL for binary
/// blobs, which is reported as an error rather than an empty file —
/// silently printing nothing for a PNG is how you lose an afternoon.
pub fn read_file(conn: &Connection, slug: &str, path: &str) -> Result<String> {
    let mut stmt = conn.prepare(
        "SELECT cb.content_text
           FROM project_files pf
           JOIN projects p ON p.id = pf.project_id
           JOIN file_contents fc
             ON fc.file_id = pf.id AND fc.is_current = 1
           JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
          WHERE p.slug = ?1 AND pf.file_path = ?2 AND pf.status = 'active'",
    )?;
    let text: Option<Option<String>> = stmt
        .query_row((slug, path), |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })?;
    match text {
        None => anyhow::bail!("{slug}: no active file at {path}"),
        Some(None) => anyhow::bail!("{slug}: {path} has no text content (binary blob?)"),
        Some(Some(s)) => Ok(s),
    }
}

#[cfg(test)]
mod tests {
    use crate::testdb;

    #[test]
    fn lists_active_files_under_prefix() {
        let c = testdb::fixture();
        let all = super::list_files(&c, "alpha", "").unwrap();
        let paths: Vec<_> = all.iter().map(|f| f.path.as_str()).collect();
        // gone.txt is status 'deleted'; notes.md is active but has no
        // current content row, so it lists with 0 lines rather than
        // vanishing.
        assert_eq!(paths, vec!["README.md", "notes.md", "src/main.rs"]);
        assert_eq!(all[1].lines, 0);
        assert_eq!(all[2].lines, 3);

        let under = super::list_files(&c, "alpha", "src/").unwrap();
        assert_eq!(under.len(), 1);
    }

    /// `_` must not act as a LIKE wildcard. `beta` has `a_b.txt` and
    /// `axb.txt`; an unescaped prefix of `a_b` matches both.
    #[test]
    fn prefix_underscore_is_literal() {
        let c = testdb::fixture();
        let hits = super::list_files(&c, "beta", "a_b").unwrap();
        let paths: Vec<_> = hits.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a_b.txt"]);
    }

    #[test]
    fn reads_current_text_content() {
        let c = testdb::fixture();
        assert_eq!(
            super::read_file(&c, "alpha", "src/main.rs").unwrap(),
            "fn main() {}\nsecond\nthird\n"
        );
    }

    /// A binary blob has NULL `content_text`. Printing nothing and
    /// exiting 0 would be indistinguishable from an empty file.
    #[test]
    fn binary_blob_is_an_error_not_an_empty_file() {
        let c = testdb::fixture();
        let err = super::read_file(&c, "beta", "logo.png")
            .unwrap_err()
            .to_string();
        assert!(err.contains("no text content"), "{err}");
    }

    #[test]
    fn missing_file_is_an_error() {
        let c = testdb::fixture();
        assert!(super::read_file(&c, "alpha", "nope.txt").is_err());
        // Deleted rows are not readable either.
        assert!(super::read_file(&c, "alpha", "gone.txt").is_err());
    }
}
