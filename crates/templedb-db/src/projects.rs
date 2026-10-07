//! `templedb project ...` reads.

use anyhow::{Context, Result};
use rusqlite::Connection;

#[derive(Debug, Clone)]
pub struct Project {
    pub id: i64,
    pub slug: String,
    pub name: Option<String>,
    pub files: i64,
    pub lines: i64,
}

/// Projects with their current file and line counts.
///
/// Counts come from `file_contents.is_current = 1` joined to active
/// `project_files`, which is the same basis `templedb project list`
/// reports. A project with rows but no current snapshot shows zero
/// rather than being dropped, so the list length always matches
/// `SELECT count(*) FROM projects`.
///
/// The Python version prints `None` in the `total_lines` column for
/// those projects, because its query has no `COALESCE` and it formats
/// the NULL straight into the table. `0` is the honest answer and is
/// what this returns; it is a deliberate divergence, not a mismatch.
pub fn list_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT p.id,
                p.slug,
                p.name,
                COUNT(DISTINCT pf.id)                      AS files,
                COALESCE(SUM(fc.line_count), 0)            AS lines
           FROM projects p
      LEFT JOIN project_files pf
             ON pf.project_id = p.id AND pf.status = 'active'
      LEFT JOIN file_contents fc
             ON fc.file_id = pf.id AND fc.is_current = 1
          GROUP BY p.id
          ORDER BY lines DESC, p.slug",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Project {
            id: r.get(0)?,
            slug: r.get(1)?,
            name: r.get(2)?,
            files: r.get(3)?,
            lines: r.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing projects")
}

#[cfg(test)]
mod tests {
    use crate::testdb;

    #[test]
    fn counts_only_current_active_rows() {
        let c = testdb::fixture();
        let projects = super::list_projects(&c).unwrap();

        // Every project appears, including the one with no files.
        let slugs: Vec<_> = projects.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha", "beta", "empty"]);

        let alpha = &projects[0];
        // alpha has four rows in project_files; `gone.txt` is status
        // 'deleted' so it counts for neither files nor lines, even
        // though it has a current content row. `notes.md` is active
        // with no content row, so it counts as a file and zero lines.
        assert_eq!(alpha.files, 3);
        assert_eq!(alpha.lines, 4);

        // NULL SUM becomes 0, not a missing row and not "None".
        let empty = projects.iter().find(|p| p.slug == "empty").unwrap();
        assert_eq!((empty.files, empty.lines), (0, 0));
    }
}
