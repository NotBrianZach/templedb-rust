//! SQLite access for the TempleDB port.
//!
//! Scope note: this layer is deliberately READ-ONLY for now. The Python
//! implementation owns every write path, and those write paths are where
//! the hard-won invariants live — session-scoped staging, checkout role
//! resolution, blob-age guards. A reader cannot violate any of them, so
//! porting reads first lets the two implementations coexist against one
//! live database instead of racing for authority over it.
//!
//! Two properties of this crate exist because of specific Python bugs:
//!
//! 1. `journal_size_limit` is set. It was unset there (`-1`, unlimited)
//!    and nothing in the tree configured it, so a WAL that spiked once
//!    stayed spiked: 5.9 GB against a 776 MB database on 2026-10-04. A
//!    new client that opens the same file must not reintroduce that.
//!
//! 2. Statements are finalized by `Drop`. The Python side pinned WAL
//!    checkpoints twice — once via `query_one` leaving an un-exhausted
//!    cursor on a pooled connection, once via raw `conn.cursor()` calls
//!    in the MCP server — because a cursor holding an unfinished
//!    statement holds a read transaction with it. In Rust that failure
//!    mode is not available: `Statement` finalizes when it leaves scope.
//!    Worth stating explicitly, because it is the single clearest thing
//!    this port buys beyond speed.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags};
use std::path::PathBuf;

/// Resolve the database path exactly as the Python side does:
/// `$TEMPLEDB_PATH`, else `~/.local/share/templedb/templedb.sqlite`.
///
/// Matching it matters more than being clever — the two implementations
/// read one file, and a client that guesses a different default would
/// silently report on an empty database instead of failing.
pub fn db_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("TEMPLEDB_PATH") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    let home = std::env::var("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home)
        .join(".local")
        .join("share")
        .join("templedb")
        .join("templedb.sqlite"))
}

/// Open the database read-only with TempleDB's standard pragmas.
pub fn open_ro() -> Result<Connection> {
    let path = db_path()?;
    if !path.exists() {
        anyhow::bail!("no database at {}", path.display());
    }
    // READ_ONLY rather than a URI with mode=ro so the flag is visible in
    // the type system rather than buried in a query string.
    let conn = Connection::open_with_flags(
        &path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .with_context(|| format!("opening {}", path.display()))?;
    apply_standard_pragmas(&conn)?;
    Ok(conn)
}

/// The pragma set, kept in one function for the same reason the Python
/// helper of this name exists: so tuning lands in one place instead of
/// being copied inline. An inline copy there is precisely why the MCP
/// server's long-lived connection missed `journal_size_limit`.
pub fn apply_standard_pragmas(conn: &Connection) -> Result<()> {
    // busy_timeout is the one that matters on a read-only handle: the
    // writer is another process entirely and checkpoints take the lock.
    conn.pragma_update(None, "busy_timeout", 30_000i64)?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // 64 MiB, matching the Python side. Harmless on a read-only handle
    // and correct if this crate ever opens for write.
    conn.pragma_update(None, "journal_size_limit", 67_108_864i64)?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Project {
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
pub fn list_projects(conn: &Connection) -> Result<Vec<Project>> {
    let mut stmt = conn.prepare(
        "SELECT p.slug,
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
            slug: r.get(0)?,
            name: r.get(1)?,
            files: r.get(2)?,
            lines: r.get(3)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("listing projects")
}

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
            AND pf.file_path LIKE ?2 || '%'
          ORDER BY pf.file_path",
    )?;
    let rows = stmt.query_map((slug, prefix), |r| {
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

/// Entity counts by kind — the cheapest useful probe of the graph, and
/// a direct cross-check against `templedb entity stats`.
pub fn entity_stats(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT kind, COUNT(*) FROM entities GROUP BY kind ORDER BY 2 DESC",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("entity stats")
}

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

#[derive(Debug, Clone)]
pub struct Edge {
    pub kind: String,
    pub other: String,
    pub authority: String,
}

/// One hop out of and into an entity, addressed as `<kind>/<external_ref>`
/// the way `templedb entity explore` addresses it.
///
/// Returns (outbound, inbound). Split rather than merged because
/// direction is the question being asked — "what did this cause" and
/// "what caused this" are different reads of the same graph.
pub fn explore(
    conn: &Connection,
    kind: &str,
    external_ref: &str,
) -> Result<(Vec<Edge>, Vec<Edge>)> {
    let id: i64 = conn
        .query_row(
            "SELECT id FROM entities WHERE kind = ?1 AND external_ref = ?2",
            (kind, external_ref),
            |r| r.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                anyhow::anyhow!("no entity {kind}/{external_ref}")
            }
            other => anyhow::Error::new(other),
        })?;

    let mut out = conn.prepare(
        "SELECT r.kind, e.kind || '/' || e.external_ref, r.source_authority
           FROM relations r JOIN entities e ON e.id = r.to_entity_id
          WHERE r.from_entity_id = ?1 ORDER BY r.kind, 2",
    )?;
    let outbound = out
        .query_map([id], |r| {
            Ok(Edge { kind: r.get(0)?, other: r.get(1)?, authority: r.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut inc = conn.prepare(
        "SELECT r.kind, e.kind || '/' || e.external_ref, r.source_authority
           FROM relations r JOIN entities e ON e.id = r.from_entity_id
          WHERE r.to_entity_id = ?1 ORDER BY r.kind, 2",
    )?;
    let inbound = inc
        .query_map([id], |r| {
            Ok(Edge { kind: r.get(0)?, other: r.get(1)?, authority: r.get(2)? })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    Ok((outbound, inbound))
}
