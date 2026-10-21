//! `templedb project ...` reads.

use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};

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
///
/// Two details are load-bearing for cross-backend agreement:
///
/// * **The `CAST(... AS BIGINT)`.** SQLite's `sum()` over an integer
///   column returns an integer; Postgres's `sum(bigint)` returns
///   `numeric`, which this crate deliberately does not map. `CAST` is
///   understood by both — SQLite applies INTEGER affinity to any type
///   name containing `INT` — so one string serves both engines.
///
/// * **`GROUP BY p.id, p.slug, p.name`** rather than `GROUP BY p.id`.
///   SQLite permits bare columns in the select list, and Postgres
///   permits them too when the grouping key is a declared primary key.
///   Naming all three stops that from depending on the mirror's DDL
///   getting the `PRIMARY KEY` right.
///
/// `ORDER BY ... p.slug` is where the backends can silently disagree:
/// SQLite compares TEXT with BINARY collation, Postgres with the
/// database's. The Postgres schema declares these columns `COLLATE "C"`
/// for that reason — see `schema/postgres/001_read_subset.sql`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectFilter {
    /// `--nix-only`: `is_nix_project = 1`.
    pub nix_only: bool,
    /// `--category`: an exact match on `project_category`.
    ///
    /// A `String` rather than an enum even though Python's argparse
    /// offers a fixed list of five. The list is input validation and
    /// belongs at the CLI edge, which has it; down here a category the
    /// live database grew after this port was built should filter
    /// correctly rather than be unrepresentable.
    pub category: Option<String>,
}

impl ProjectFilter {
    pub fn is_empty(&self) -> bool {
        !self.nix_only && self.category.is_none()
    }
}

/// [`list_projects_filtered`] with no filter.
pub fn list_projects(store: &dyn Store) -> Result<Vec<Project>> {
    list_projects_filtered(store, &ProjectFilter::default())
}

/// Projects matching `filter`, in the same order and with the same
/// counts as the unfiltered list.
///
/// # Why the filter does not change the numbers
///
/// Python runs an entirely separate query when either flag is passed:
/// `COUNT(DISTINCT pf.id)` over **all** `project_files` rather than the
/// active ones, `SUM(pf.lines_of_code)` instead of the current
/// snapshot's `line_count`, and `ORDER BY p.slug` instead of by size.
/// So `templedb project list` and `templedb project list --nix-only`
/// can report different file counts for the same project, from the same
/// database, in the same second.
///
/// This adds a `WHERE` and changes nothing else, so the flags select
/// rows and never reinterpret them. The counts a project shows are the
/// same whether you filtered or not, which is the property that makes
/// the filtered output worth believing.
///
/// Python also prints *two* tables when a filter is passed — a
/// six-column "Nix Projects" one and then the ordinary "Projects" one
/// over the same rows — because the filtered branch prints without
/// returning and falls through to the shared tail. Not reproduced:
/// `project list` is already a deliberate format divergence, and
/// doubling the output is not a format.
pub fn list_projects_filtered(store: &dyn Store, filter: &ProjectFilter) -> Result<Vec<Project>> {
    // Built rather than interpolated from user input: `category` is a
    // bound parameter, and `nix_only` contributes a constant.
    let mut params: Vec<Value> = Vec::new();
    let mut wheres: Vec<String> = Vec::new();
    if filter.nix_only {
        // `= 1`, as Python has it, not `IS NOT NULL` and not a
        // truthiness test. The column is NULL for most rows and 0 for
        // some, and both mean "not a Nix project".
        wheres.push("p.is_nix_project = 1".to_string());
    }
    if let Some(category) = &filter.category {
        params.push(Value::from(category.as_str()));
        wheres.push(format!("p.project_category = ?{}", params.len()));
    }
    let where_clause = if wheres.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wheres.join(" AND "))
    };

    let rows = store
        .query(
            &format!(
                "SELECT p.id,
                        p.slug,
                        p.name,
                        COUNT(DISTINCT pf.id)                              AS files,
                        CAST(COALESCE(SUM(fc.line_count), 0) AS BIGINT)    AS lines
                   FROM projects p
              LEFT JOIN project_files pf
                     ON pf.project_id = p.id AND pf.status = 'active'
              LEFT JOIN file_contents fc
                     ON fc.file_id = pf.id AND fc.is_current = 1
                  {where_clause}
                  GROUP BY p.id, p.slug, p.name
                  ORDER BY lines DESC, p.slug"
            ),
            &params,
        )
        .context("listing projects")?;

    rows.iter()
        .map(|r| {
            Ok(Project {
                id: r.get_i64(0)?,
                slug: r.get_string(1)?,
                name: r.get_opt_string(2)?,
                files: r.get_i64(3)?,
                lines: r.get_i64(4)?,
            })
        })
        .collect()
}

/// Everything `project show` prints about one project.
///
/// Three queries' worth, kept together because the command prints them
/// as one block and the shapes are small.
#[derive(Debug, Clone)]
pub struct ProjectDetail {
    pub slug: String,
    pub name: Option<String>,
    pub repo_url: Option<String>,
    pub git_branch: Option<String>,
    /// Every row in `project_files`, not just the active ones.
    pub file_count: i64,
    /// `SUM(lines_of_code)`, which is NULL when no row has one. Carried
    /// as an `Option` because that is the case Python crashes on — see
    /// [`project_detail`].
    pub total_lines: Option<i64>,
    pub file_types: i64,
    pub branch_count: i64,
    pub commit_count: i64,
}

/// One project's detail, by exact slug.
///
/// # Exact, not fuzzy
///
/// Every other command that takes a project fuzzy-matches it. This one
/// does not, in Python either: `project show` calls
/// `get_by_slug(args.slug, required=True)` where the rest call
/// `fuzzy_match_project`, and its own `--help` says "Project slug"
/// rather than "Project name or pattern". Reproduced rather than
/// unified, because making it fuzzy would change which project an
/// existing script resolves to.
///
/// # The NULL that crashes Python
///
/// The statistics query is
/// `SELECT COUNT(*), SUM(lines_of_code), COUNT(DISTINCT file_type_id)`,
/// and `SUM` over no non-NULL values returns NULL. Python then formats
/// it with a thousands separator — `f"Lines of code: {stats['total_lines']:,}"`
/// — and `format(None, ',')` raises:
///
/// ```text
/// $ templedb project show cathedraldb
/// Error: unsupported format string passed to NoneType.__format__
///
/// Project: cathedraldb
/// Name: CathedralDB - Share TempleDB Projects
/// Repository: templedb://cathedraldb
/// Branch: None
///
/// Files: 0
/// ```
///
/// Six of the eight lines, then exit 1. This is not an edge case: **10
/// of the 30 projects** in the live database have a NULL sum, and they
/// are not all empty — `prediction_dashboards` has 32 files and no
/// `lines_of_code` on any of them. It is the same shape as the
/// `source revisions` crash the README records, partial output and all.
///
/// Carried as `Option<i64>` so the caller decides; the CLI prints `0`.
///
/// # Two oddities reproduced rather than fixed
///
/// `COUNT(*)` counts every `project_files` row regardless of `status`,
/// so a project's `Files:` here includes deleted ones and disagrees
/// with `project list`, which filters `status = 'active'`. And
/// `commit_count` is counted through `vcs_branches`, so it counts
/// commits on this project's *branches* rather than commits with this
/// project's id — the two differ if a commit's `branch_id` points at
/// another project's branch. Both are Python's behaviour and both are
/// on stdout, so both are kept.
pub fn project_detail(store: &dyn Store, slug: &str) -> Result<Option<ProjectDetail>> {
    let Some(row) = store
        .query_opt(
            "SELECT id, slug, name, repo_url, git_branch
               FROM projects WHERE slug = ?1",
            &[Value::from(slug)],
        )
        .context("looking up project")?
    else {
        return Ok(None);
    };
    let id = row.get_i64(0)?;

    // `CAST(... AS BIGINT)` on the SUM for the same reason
    // `list_projects` needs it: Postgres's `sum(bigint)` is `numeric`.
    // The cast preserves NULL, which is the point — wrapping it in
    // `COALESCE` here would hide the very case this function exists to
    // report.
    let stats = store
        .query_opt(
            "SELECT COUNT(*),
                    CAST(SUM(lines_of_code) AS BIGINT),
                    COUNT(DISTINCT file_type_id)
               FROM project_files WHERE project_id = ?1",
            &[Value::from(id)],
        )
        .context("reading project statistics")?;
    // An aggregate with no `GROUP BY` always yields one row, so the
    // `else` is unreachable; it costs nothing and avoids an `unwrap`.
    let (file_count, total_lines, file_types) = match &stats {
        Some(r) => (r.get_i64(0)?, r.get_opt_i64(1)?, r.get_i64(2)?),
        None => (0, None, 0),
    };

    let vcs = store
        .query_opt(
            "SELECT COUNT(DISTINCT vb.id), COUNT(DISTINCT vc.id)
               FROM vcs_branches vb
          LEFT JOIN vcs_commits vc ON vc.branch_id = vb.id
              WHERE vb.project_id = ?1",
            &[Value::from(id)],
        )
        .context("reading vcs counts")?;
    let (branch_count, commit_count) = match &vcs {
        Some(r) => (r.get_i64(0)?, r.get_i64(1)?),
        None => (0, 0),
    };

    Ok(Some(ProjectDetail {
        slug: row.get_string(1)?,
        name: row.get_opt_string(2)?,
        repo_url: row.get_opt_string(3)?,
        git_branch: row.get_opt_string(4)?,
        file_count,
        total_lines,
        file_types,
        branch_count,
        commit_count,
    }))
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
        // alpha has seven rows in project_files; `gone.txt` is status
        // 'deleted' so it counts for neither files nor lines, even
        // though it has a current content row. `notes.md` is active
        // with no content row, so it counts as a file and zero lines —
        // and so do the three `edit_mode` rows, which is why `files`
        // grew by three and `lines` did not grow at all. `lines` sums
        // `file_contents.line_count`, not `project_files.lines_of_code`.
        assert_eq!(alpha.files, 6);
        assert_eq!(alpha.lines, 4);

        // NULL SUM becomes 0, not a missing row and not "None".
        let empty = projects.iter().find(|p| p.slug == "empty").unwrap();
        assert_eq!((empty.files, empty.lines), (0, 0));
    }

    /// `--nix-only` is `is_nix_project = 1`, so a 0 and a NULL are both
    /// excluded. Writing it as `IS NOT NULL` would pass a fixture where
    /// every non-Nix row is NULL and fail on the live data, which has
    /// both.
    #[test]
    fn nix_only_excludes_both_zero_and_null() {
        let c = testdb::fixture();
        let filter = super::ProjectFilter {
            nix_only: true,
            category: None,
        };
        let slugs: Vec<_> = super::list_projects_filtered(&c, &filter)
            .unwrap()
            .into_iter()
            .map(|p| p.slug)
            .collect();
        assert_eq!(slugs, vec!["alpha"]);
    }

    /// `--category` matches on its own, and does not imply
    /// `--nix-only`: `beta` shares alpha's category and is not a Nix
    /// project.
    #[test]
    fn category_matches_independently_of_the_nix_flag() {
        let c = testdb::fixture();
        let filter = super::ProjectFilter {
            nix_only: false,
            category: Some("package".to_string()),
        };
        let slugs: Vec<_> = super::list_projects_filtered(&c, &filter)
            .unwrap()
            .into_iter()
            .map(|p| p.slug)
            .collect();
        assert_eq!(slugs, vec!["alpha", "beta"]);
    }

    /// Both together are an AND, not an OR.
    #[test]
    fn the_two_filters_intersect() {
        let c = testdb::fixture();
        let both = super::ProjectFilter {
            nix_only: true,
            category: Some("package".to_string()),
        };
        let slugs: Vec<_> = super::list_projects_filtered(&c, &both)
            .unwrap()
            .into_iter()
            .map(|p| p.slug)
            .collect();
        assert_eq!(slugs, vec!["alpha"]);

        // And a combination nothing satisfies is empty rather than an
        // error: `alpha` is the only Nix project and its category is
        // `package`.
        let none = super::ProjectFilter {
            nix_only: true,
            category: Some("service".to_string()),
        };
        assert!(super::list_projects_filtered(&c, &none).unwrap().is_empty());
    }

    /// An unknown category is empty, not an error. The CLI rejects a
    /// value outside Python's five, but this layer must not — the live
    /// database can grow a sixth without a rebuild.
    #[test]
    fn an_unknown_category_returns_nothing() {
        let c = testdb::fixture();
        let filter = super::ProjectFilter {
            nix_only: false,
            category: Some("no-such-category".to_string()),
        };
        assert!(super::list_projects_filtered(&c, &filter)
            .unwrap()
            .is_empty());
    }

    /// The filter must not reinterpret the counts, which is the one
    /// thing Python's separate query does: the rows a filtered list
    /// shows carry the same numbers as the unfiltered list.
    #[test]
    fn filtering_changes_which_rows_not_what_they_say() {
        let c = testdb::fixture();
        let all = super::list_projects(&c).unwrap();
        let filter = super::ProjectFilter {
            nix_only: true,
            category: None,
        };
        let filtered = super::list_projects_filtered(&c, &filter).unwrap();
        let alpha_all = all.iter().find(|p| p.slug == "alpha").unwrap();
        let alpha_filtered = filtered.iter().find(|p| p.slug == "alpha").unwrap();
        assert_eq!(
            (alpha_all.files, alpha_all.lines),
            (alpha_filtered.files, alpha_filtered.lines)
        );
    }

    /// An empty filter is the unfiltered list, so `list_projects` can
    /// delegate without a branch at the call site.
    #[test]
    fn the_default_filter_is_no_filter() {
        let c = testdb::fixture();
        assert!(super::ProjectFilter::default().is_empty());
        let all = super::list_projects(&c).unwrap();
        let via_filter =
            super::list_projects_filtered(&c, &super::ProjectFilter::default()).unwrap();
        assert_eq!(all.len(), via_filter.len());
    }

    #[test]
    fn detail_carries_metadata_and_counts() {
        let c = testdb::fixture();
        let d = super::project_detail(&c, "alpha").unwrap().unwrap();
        assert_eq!(d.name.as_deref(), Some("Alpha Project"));
        assert_eq!(d.repo_url.as_deref(), Some("git@example:alpha.git"));
        assert_eq!(d.git_branch.as_deref(), Some("main"));
        // Seven `project_files` rows, including the deleted `gone.txt` —
        // this count does not filter on status, unlike `project list`.
        assert_eq!(d.file_count, 7);
        // `SUM(lines_of_code)`, so the three `edit_mode` rows' 7 + 8 + 9
        // land here even though they have no `file_contents` row and
        // therefore contribute nothing to `project list`'s `lines`.
        assert_eq!(d.total_lines, Some(66));
        // Still two distinct `file_type_id` values: the new rows reuse 1.
        assert_eq!(d.file_types, 2);
        // Two branches, and the commits reached through them.
        assert_eq!(d.branch_count, 2);
        assert_eq!(d.commit_count, 5);
    }

    /// The case Python crashes on. `empty` has no files, so
    /// `SUM(lines_of_code)` is NULL — Python formats that with `{:,}`
    /// and raises `TypeError` after printing six of its eight lines.
    /// Ten of the thirty live projects are in this state.
    #[test]
    fn a_null_line_sum_is_none_not_a_crash() {
        let c = testdb::fixture();
        let d = super::project_detail(&c, "empty").unwrap().unwrap();
        assert_eq!(d.file_count, 0);
        assert_eq!(d.total_lines, None);
        assert_eq!(d.file_types, 0);
        // No branches, so the caller omits the VCS block entirely.
        assert_eq!(d.branch_count, 0);
    }

    /// The more interesting half of that bug: a project with files whose
    /// `lines_of_code` are all NULL. `prediction_dashboards` has 32
    /// files and still crashes Python.
    #[test]
    fn files_with_no_line_counts_still_sum_to_none() {
        let c = testdb::fixture();
        let d = super::project_detail(&c, "beta").unwrap().unwrap();
        assert_eq!(d.file_count, 8);
        assert_eq!(d.total_lines, None);
        // And no row carries a type either.
        assert_eq!(d.file_types, 0);
    }

    /// NULL metadata stays NULL so the caller can render Python's two
    /// different placeholders — `None` for the name, `N/A` for the
    /// repository.
    #[test]
    fn null_metadata_is_carried_not_defaulted() {
        let c = testdb::fixture();
        let d = super::project_detail(&c, "empty").unwrap().unwrap();
        assert_eq!(d.name, None);
        assert_eq!(d.repo_url, None);
        assert_eq!(d.git_branch, None);
    }

    /// Exact slug, not fuzzy — `alph` resolves for every other command
    /// and must not here.
    #[test]
    fn the_slug_is_matched_exactly() {
        let c = testdb::fixture();
        assert!(super::project_detail(&c, "alph").unwrap().is_none());
        assert!(super::project_detail(&c, "ALPHA").unwrap().is_none());
        assert!(super::project_detail(&c, "nosuch").unwrap().is_none());
    }
}
