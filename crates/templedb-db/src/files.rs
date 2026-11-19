//! `templedb file ...` reads.

use crate::like::escape_like;
use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: String,
    pub lines: i64,
    /// `project_files.edit_mode` (migration 106), raw.
    ///
    /// NULL *is* the default and means `immutable`, but the NULL is kept
    /// rather than coalesced here because the two readers need different
    /// things from it: `-l` renders NULL as the word `immutable`, and
    /// `--edit-mode non-immutable` has to exclude both NULL and the
    /// literal `'immutable'`, which it cannot do once they are the same
    /// value. All 2,491 live rows are NULL today, so coalescing in SQL
    /// would have looked correct indefinitely.
    pub edit_mode: Option<String>,
}

/// Active files under `prefix` for a project, with line counts.
///
/// `?2 || '%'` concatenates in both dialects, and both treat `ESCAPE`
/// on `LIKE` the same way — neither has a default escape character, so
/// the clause is required rather than optional in both.
pub fn list_files(store: &dyn Store, slug: &str, prefix: &str) -> Result<Vec<FileEntry>> {
    // The prefix is a literal path fragment, not a pattern: `_` is in
    // half the slugs in this database (`proj_a`, `system_config`) and
    // LIKE would treat it as "any character", so `file ls x proj_a`
    // would also list `proj-a`. Escape the three LIKE metacharacters
    // and declare the escape in the SQL.
    let rows = store
        .query(
            "SELECT pf.file_path, COALESCE(fc.line_count, 0), pf.edit_mode
               FROM project_files pf
               JOIN projects p ON p.id = pf.project_id
          LEFT JOIN file_contents fc
                 ON fc.file_id = pf.id AND fc.is_current = 1
              WHERE p.slug = ?1
                AND pf.status = 'active'
                AND pf.file_path LIKE ?2 || '%' ESCAPE '\\'
              ORDER BY pf.file_path",
            &[Value::from(slug), Value::from(escape_like(prefix))],
        )
        .context("listing files")?;

    rows.iter()
        .map(|r| {
            Ok(FileEntry {
                path: r.get_string(0)?,
                lines: r.get_i64(1)?,
                edit_mode: r.get_opt_string(2)?,
            })
        })
        .collect()
}

/// Which files `file ls --edit-mode` keeps.
///
/// Python filters this in Python rather than in SQL, and so does this —
/// not for symmetry but because `non-immutable` is "not NULL and not
/// `'immutable'`", which is two conditions on a column whose NULL is
/// meaningful, and expressing it in SQL would need an `IS NOT NULL` that
/// is easy to drop. The set is already in memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditModeFilter {
    /// An exact `edit_mode` value. `immutable` matches only the literal
    /// string, *not* the NULL that means the same thing — which is
    /// Python's behaviour (`em != edit_mode_filter` with `em` as the raw
    /// column) and matches nothing at all on current data, where every
    /// row is NULL.
    Exactly(&'static str),
    /// Anything that opted out of the default: not NULL, not
    /// `'immutable'`.
    NonImmutable,
}

impl EditModeFilter {
    pub fn keeps(self, edit_mode: Option<&str>) -> bool {
        match self {
            EditModeFilter::NonImmutable => !matches!(edit_mode, None | Some("immutable")),
            EditModeFilter::Exactly(want) => edit_mode == Some(want),
        }
    }
}

/// Current contents of one tracked file.
///
/// The join through `content_blobs` is the part worth getting right:
/// `file_contents` holds the hash, not the bytes, and the same query
/// shape is what `reports.py` uses. `content_text` is NULL for binary
/// blobs, which is reported as an error rather than an empty file —
/// silently printing nothing for a PNG is how you lose an afternoon.
pub fn read_file(store: &dyn Store, slug: &str, path: &str) -> Result<String> {
    let row = store
        .query_opt(
            "SELECT cb.content_text
               FROM project_files pf
               JOIN projects p ON p.id = pf.project_id
               JOIN file_contents fc
                 ON fc.file_id = pf.id AND fc.is_current = 1
               JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
              WHERE p.slug = ?1 AND pf.file_path = ?2 AND pf.status = 'active'",
            &[Value::from(slug), Value::from(path)],
        )
        .context("reading file")?;

    match row {
        None => anyhow::bail!("{slug}: no active file at {path}"),
        Some(r) => match r.get_opt_string(0)? {
            None => anyhow::bail!("{slug}: {path} has no text content (binary blob?)"),
            Some(s) => Ok(s),
        },
    }
}

/// The current `content_hash` for a project's path, or `None`.
///
/// `file where`'s one database read, and the whole of its left column.
/// Python's `_get_current_hash` carries a `LIMIT 1` that cannot select
/// anything: `(project_id, file_path)` is unique among active rows and
/// `is_current = 1` is unique per file, so the join yields at most one
/// row. It is reproduced as `query_opt`, which is the same contract
/// without pretending there is a choice being made.
///
/// `None` is both "no such file" and "the file is deleted", because
/// `status = 'active'` is inside the predicate rather than beside it.
/// The caller needs that collapse — Python prints one message for both
/// and exits 2 — so it is not disentangled here.
pub fn current_hash(store: &dyn Store, slug: &str, path: &str) -> Result<Option<String>> {
    let row = store
        .query_opt(
            "SELECT fc.content_hash
               FROM file_contents fc
               JOIN project_files pf ON pf.id = fc.file_id
               JOIN projects p ON p.id = pf.project_id
              WHERE p.slug = ?1 AND pf.file_path = ?2
                AND pf.status = 'active' AND fc.is_current = 1",
            &[Value::from(slug), Value::from(path)],
        )
        .context("reading the current content hash")?;

    // `content_hash` is declared NOT NULL, so a row without one is a
    // schema violation rather than a deleted file -- `get_string`
    // refusing NULL is the right answer and is why this is not
    // `get_opt_string`, which would silently report the file as absent.
    row.map(|r| r.get_string(0)).transpose()
}

#[cfg(test)]
mod tests {
    use crate::testdb;

    /// The three answers `file where`'s left column can have, on the
    /// fixture rows built for exactly this: an active file with a
    /// current hash, an active file with no content row, and a row
    /// whose status is `deleted`.
    #[test]
    fn current_hash_distinguishes_present_absent_and_deleted() {
        let c = testdb::fixture();
        assert!(super::current_hash(&c, "alpha", "src/main.rs")
            .unwrap()
            .is_some());
        // Active, but no `file_contents` row is current -- the join
        // drops it, so there is no hash to report.
        assert_eq!(super::current_hash(&c, "alpha", "notes.md").unwrap(), None);
        // status = 'deleted': Python prints "(no current row -- file is
        // deleted or never existed)" and exits 2 for this and for a
        // path that was never there, so both collapse to None here.
        assert_eq!(super::current_hash(&c, "alpha", "gone.txt").unwrap(), None);
        assert_eq!(
            super::current_hash(&c, "alpha", "no/such/file").unwrap(),
            None
        );
        assert_eq!(
            super::current_hash(&c, "no-such-project", "README.md").unwrap(),
            None
        );
    }

    #[test]
    fn lists_active_files_under_prefix() {
        let c = testdb::fixture();
        let all = super::list_files(&c, "alpha", "").unwrap();
        let paths: Vec<_> = all.iter().map(|f| f.path.as_str()).collect();
        // gone.txt is status 'deleted'; notes.md is active but has no
        // current content row, so it lists with 0 lines rather than
        // vanishing.
        assert_eq!(
            paths,
            vec![
                "README.md",
                "notes.md",
                "src/hot.rs",
                "src/live.rs",
                "src/main.rs",
                "src/pin.rs",
            ]
        );
        assert_eq!(all[1].lines, 0);
        assert_eq!(all[4].lines, 3);

        let under = super::list_files(&c, "alpha", "src/").unwrap();
        assert_eq!(under.len(), 4);
    }

    /// `edit_mode` comes back raw, NULL included, because `-l` and
    /// `--edit-mode non-immutable` need to tell NULL from the literal
    /// `'immutable'` and cannot once it is coalesced. Every row in the
    /// live database is NULL, so the fixture is the only place this is
    /// observable.
    #[test]
    fn edit_mode_is_returned_raw_including_its_null() {
        use super::EditModeFilter as F;
        let c = testdb::fixture();
        let all = super::list_files(&c, "alpha", "").unwrap();
        let mode = |p: &str| all.iter().find(|f| f.path == p).unwrap().edit_mode.clone();
        assert_eq!(mode("src/main.rs"), None);
        assert_eq!(mode("src/hot.rs").as_deref(), Some("hot-reload"));
        assert_eq!(mode("src/pin.rs").as_deref(), Some("immutable"));

        // `non-immutable` excludes both NULL and the literal.
        let kept: Vec<&str> = all
            .iter()
            .filter(|f| F::NonImmutable.keeps(f.edit_mode.as_deref()))
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(kept, vec!["src/hot.rs", "src/live.rs"]);

        // And `--edit-mode immutable` matches the literal only, not the
        // NULL that means the same thing — Python compares the raw
        // column, so on live data this selects nothing at all.
        let pinned: Vec<&str> = all
            .iter()
            .filter(|f| F::Exactly("immutable").keeps(f.edit_mode.as_deref()))
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(pinned, vec!["src/pin.rs"]);
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
