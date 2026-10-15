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
