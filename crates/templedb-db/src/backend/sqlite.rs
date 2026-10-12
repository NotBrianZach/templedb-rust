//! The SQLite backend — the original implementation, now behind
//! [`Store`].
//!
//! Two properties of this file exist because of specific Python bugs,
//! and both predate the backend split:
//!
//! 1. `journal_size_limit` is set. It was unset there (`-1`, unlimited)
//!    and nothing in the tree configured it, so a WAL that spiked once
//!    stayed spiked: 5.9 GB against a 776 MB database on 2026-10-04.
//!
//! 2. Statements are finalized by `Drop`. The Python side pinned WAL
//!    checkpoints twice — once via `query_one` leaving an un-exhausted
//!    cursor on a pooled connection, once via raw `conn.cursor()` calls
//!    in the MCP server — because a cursor holding an unfinished
//!    statement holds a read transaction with it. In Rust that failure
//!    mode is not available.
//!
//! The second one is worth re-reading now that [`Store::query`]
//! materialises every row: the trait makes it structurally impossible
//! to hold a cursor open across caller code, because there is no cursor
//! to hold. What was a property of the language is now a property of
//! the interface.

use crate::dialect::Dialect;
use crate::store::Store;
use crate::value::{Row, Value};
use anyhow::{Context, Result};
use rusqlite::types::{Value as SqlValue, ValueRef};
use rusqlite::{params_from_iter, Connection, OpenFlags};
use std::path::Path;

/// A read-only SQLite connection.
pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    /// Open a database file read-only, with TempleDB's standard pragmas.
    pub fn open_ro(path: &Path) -> Result<Self> {
        if !path.exists() {
            anyhow::bail!("no database at {}", path.display());
        }
        // READ_ONLY rather than a URI with mode=ro so the flag is
        // visible in the type system rather than buried in a query
        // string.
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .with_context(|| format!("opening {}", path.display()))?;
        let store = Self { conn };
        store.apply_standard_pragmas()?;
        Ok(store)
    }

    /// The pragma set, in one place for the same reason the Python
    /// helper of this name exists: so tuning lands here instead of
    /// being copied inline. An inline copy there is precisely why the
    /// MCP server's long-lived connection missed `journal_size_limit`.
    pub fn apply_standard_pragmas(&self) -> Result<()> {
        // busy_timeout is the one that matters on a read-only handle:
        // the writer is another process entirely and checkpoints take
        // the lock.
        self.conn.pragma_update(None, "busy_timeout", 30_000i64)?;
        self.conn.pragma_update(None, "temp_store", "MEMORY")?;
        // 64 MiB, matching the Python side. Harmless on a read-only
        // handle and correct if this crate ever opens for write.
        self.conn
            .pragma_update(None, "journal_size_limit", 67_108_864i64)?;
        Ok(())
    }

    /// An empty in-memory database, for the test fixture and for the
    /// mirror command's source-side reads.
    pub fn open_in_memory() -> Result<Self> {
        let store = Self {
            conn: Connection::open_in_memory().context("opening in-memory sqlite")?,
        };
        store.apply_standard_pragmas()?;
        Ok(store)
    }

    /// Run one or more statements for their effect.
    ///
    /// Present for the test fixture and for nothing else: the live
    /// handle is opened `SQLITE_OPEN_READ_ONLY`, so calling this on it
    /// fails in SQLite rather than relying on a Rust-side guard.
    pub fn execute_batch(&self, sql: &str) -> Result<()> {
        self.conn.execute_batch(sql).context("executing batch")
    }

    /// Run one parameterised statement for its effect. Same caveat as
    /// [`SqliteStore::execute_batch`].
    pub fn execute(&self, sql: &str, params: &[Value]) -> Result<usize> {
        let bound: Vec<SqlValue> = params.iter().map(to_sqlite).collect::<Result<_>>()?;
        self.conn
            .execute(sql, params_from_iter(bound))
            .with_context(|| format!("executing: {}", first_line(sql)))
    }
}

impl Store for SqliteStore {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        // `prepare_cached`, not `prepare`. The BFS in `entities.rs`
        // fetches a level at a time, so this is called a dozen times
        // for a deep `entity paths` rather than the thousands the
        // per-node version needed — but on this backend the frontier is
        // expanded into one placeholder per id, so the SQL text varies
        // with the level's width. Bucketing the width (see
        // `entities::BUCKETS`) keeps that down to a handful of distinct
        // statements, and the cache is keyed on the SQL string, so
        // passing it through unchanged for SQLite (see
        // [`Dialect::bind`]) is what lets those few be reused.
        let mut stmt = self
            .conn
            .prepare_cached(sql)
            .with_context(|| format!("preparing: {}", first_line(sql)))?;
        let width = stmt.column_count();
        let bound: Vec<SqlValue> = params.iter().map(to_sqlite).collect::<Result<_>>()?;

        let mut rows = stmt
            .query(params_from_iter(bound))
            .with_context(|| format!("running: {}", first_line(sql)))?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(width);
            for i in 0..width {
                values.push(from_sqlite(row.get_ref(i)?));
            }
            out.push(Row::new(values));
        }
        Ok(out)
    }
}

fn to_sqlite(v: &Value) -> Result<SqlValue> {
    Ok(match v {
        Value::Null => SqlValue::Null,
        Value::Int(i) => SqlValue::Integer(*i),
        Value::Real(f) => SqlValue::Real(*f),
        Value::Text(s) => SqlValue::Text(s.clone()),
        Value::Blob(b) => SqlValue::Blob(b.clone()),
        // SQLite has no array type, so there is nothing to bind this
        // to. Callers that need set membership on this backend expand
        // the set into one placeholder per element — see
        // `entities::collect_direction`, which picks its predicate from
        // `Store::dialect`. Reaching here means a caller built SQL for
        // one dialect and ran it against the other.
        Value::IntSet(v) => anyhow::bail!(
            "SQLite cannot bind a {}-element integer set as one parameter; \
             expand it into placeholders",
            v.len()
        ),
    })
}

/// SQLite's storage classes map one-to-one onto [`Value`], which is why
/// [`Value`] has the variants it does. The interesting conversion is the
/// Postgres one; this direction is a rename.
fn from_sqlite(v: ValueRef<'_>) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Int(i),
        ValueRef::Real(f) => Value::Real(f),
        // Lossy only for non-UTF-8 text, which SQLite permits and this
        // schema does not contain: every TEXT column here is written by
        // Python `str`. Binary lives in `content_blob`, typed BLOB.
        ValueRef::Text(b) => Value::Text(String::from_utf8_lossy(b).into_owned()),
        ValueRef::Blob(b) => Value::Blob(b.to_vec()),
    }
}

/// SQL in this crate is written across several lines for readability,
/// which makes it useless in a one-line error message. Take the first
/// non-empty line so the context says `SELECT p.slug,` rather than
/// reproducing the whole query.
pub(crate) fn first_line(sql: &str) -> &str {
    sql.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(sql)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_every_storage_class() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.execute_batch("CREATE TABLE t (i INTEGER, r REAL, s TEXT, b BLOB, n TEXT)")
            .unwrap();
        s.execute(
            "INSERT INTO t (i, r, s, b, n) VALUES (?1, ?2, ?3, ?4, ?5)",
            &[
                Value::Int(42),
                Value::Real(1.5),
                Value::Text("hi".into()),
                Value::Blob(vec![0x89, 0x50]),
                Value::Null,
            ],
        )
        .unwrap();

        let rows = s.query("SELECT i, r, s, b, n FROM t", &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_i64(0).unwrap(), 42);
        assert_eq!(rows[0].get_string(2).unwrap(), "hi");
        assert_eq!(rows[0].get_opt_blob(3).unwrap(), Some(vec![0x89, 0x50]));
        assert_eq!(rows[0].get_opt_string(4).unwrap(), None);
    }

    #[test]
    fn query_opt_returns_none_for_no_rows() {
        let s = SqliteStore::open_in_memory().unwrap();
        s.execute_batch("CREATE TABLE t (i INTEGER)").unwrap();
        assert!(s.query_opt("SELECT i FROM t", &[]).unwrap().is_none());
        s.execute("INSERT INTO t VALUES (?1)", &[Value::Int(7)])
            .unwrap();
        let row = s.query_opt("SELECT i FROM t", &[]).unwrap().unwrap();
        assert_eq!(row.get_i64(0).unwrap(), 7);
    }

    /// The error context has to name the query, and naming it in full
    /// buries the message. One line is the compromise.
    #[test]
    fn errors_quote_only_the_first_line_of_the_query() {
        let s = SqliteStore::open_in_memory().unwrap();
        let err = s
            .query(
                "SELECT a\n  FROM nonexistent\n WHERE b = ?1",
                &[Value::Int(1)],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("SELECT a"), "{err}");
        assert!(!err.contains("nonexistent"), "{err}");
    }

    #[test]
    fn dialect_is_sqlite_and_sql_passes_through_unchanged() {
        let s = SqliteStore::open_in_memory().unwrap();
        assert_eq!(s.dialect(), Dialect::Sqlite);
        let sql = "SELECT ?1 WHERE x LIKE ?2 ESCAPE '\\'";
        assert_eq!(s.dialect().bind(sql).unwrap(), sql);
    }

    /// A read-only handle must refuse writes in SQLite itself rather
    /// than relying on this crate to never call `execute`.
    #[test]
    fn a_read_only_handle_refuses_writes() {
        let dir = std::env::temp_dir().join(format!("templedb-ro-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.sqlite");
        {
            let seed = Connection::open(&path).unwrap();
            seed.execute_batch("CREATE TABLE t (i INTEGER)").unwrap();
        }
        let s = SqliteStore::open_ro(&path).unwrap();
        assert!(s
            .execute("INSERT INTO t VALUES (?1)", &[Value::Int(1)])
            .is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn opening_a_missing_file_names_the_path() {
        // `.map(drop)` because `unwrap_err` wants the Ok type to be
        // Debug, and a live connection has no business being printable.
        let err = SqliteStore::open_ro(Path::new("/nonexistent/x.sqlite"))
            .map(drop)
            .unwrap_err()
            .to_string();
        assert!(err.contains("/nonexistent/x.sqlite"), "{err}");
    }
}
