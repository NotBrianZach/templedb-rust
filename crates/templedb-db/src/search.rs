//! `templedb search ...` reads.

use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub project: String,
    pub path: String,
    pub snippet: String,
}

/// Full-text search over `file_contents_fts`.
///
/// Joined on `file_contents_fts.rowid`, which migration 110 established
/// is `project_files.id`. The Python implementation joined
/// `file_search_view` on `file_path` instead, and `file_path` is not
/// unique across projects — 14 have a `README.md` — so one hit fanned out
/// to one row per project owning that path, and `-p` then matched a
/// project whose file did not contain the term. `search content rusqlite`
/// returned 18 rows against a ground truth of 5 until 2026-10-04. This
/// port never had that bug; it is recorded here so nobody reintroduces
/// the path join thinking it is equivalent.
pub fn search_content(
    conn: &Connection,
    pattern: &str,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<SearchHit>> {
    let mut stmt = conn.prepare(
        "SELECT p.slug,
                pf.file_path,
                snippet(file_contents_fts, 1, '[', ']', ' … ', 16)
           FROM file_contents_fts
           JOIN project_files pf ON pf.id = file_contents_fts.rowid
           JOIN projects p ON p.id = pf.project_id
          WHERE file_contents_fts MATCH ?1
            AND (?2 IS NULL OR p.slug = ?2)
          ORDER BY rank
          LIMIT ?3",
    )?;
    let rows = stmt.query_map((pattern, project, limit), |r| {
        Ok(SearchHit {
            project: r.get(0)?,
            path: r.get(1)?,
            snippet: r.get(2)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("FTS search")
}

#[cfg(test)]
mod tests {
    use crate::testdb;

    /// The regression this port was built around: two projects own a
    /// `README.md`, only one contains the term. Joining on `file_path`
    /// returns both; joining on rowid returns one.
    #[test]
    fn hits_are_attributed_to_the_owning_project_only() {
        let c = testdb::fixture();
        let hits = super::search_content(&c, "distinctiveword", None, 100).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].project, "alpha");
        assert_eq!(hits[0].path, "README.md");

        // And the project filter must not pass for the project whose
        // README never contained the term.
        let filtered = super::search_content(&c, "distinctiveword", Some("beta"), 100).unwrap();
        assert!(filtered.is_empty(), "{filtered:?}");
    }

    #[test]
    fn limit_is_applied() {
        let c = testdb::fixture();
        let hits = super::search_content(&c, "shared", None, 1).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            super::search_content(&c, "shared", None, 10).unwrap().len(),
            2
        );
    }
}
