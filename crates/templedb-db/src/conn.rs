//! Database location and connection setup.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// `db_path` is the one function both implementations must agree on
    /// character for character; a disagreement means the port silently
    /// reads a different — probably absent — database.
    #[test]
    fn db_path_prefers_env_var() {
        with_scratch_env(|| {
            std::env::set_var("TEMPLEDB_PATH", "/tmp/elsewhere.sqlite");
            assert_eq!(db_path().unwrap(), PathBuf::from("/tmp/elsewhere.sqlite"));
        });
    }

    /// An empty `TEMPLEDB_PATH` must fall through to the default rather
    /// than being honoured: `sqlite3.connect("")` on the Python side
    /// opens a *temporary* database, which answers "0 projects" instead
    /// of failing, and an exported-but-empty variable is a common shell
    /// accident.
    #[test]
    fn empty_env_var_falls_back_to_default() {
        with_scratch_env(|| {
            std::env::set_var("TEMPLEDB_PATH", "");
            std::env::set_var("HOME", "/home/someone");
            assert_eq!(
                db_path().unwrap(),
                PathBuf::from("/home/someone/.local/share/templedb/templedb.sqlite")
            );
        });
    }

    /// Serialise the env-mutating tests and restore what they clobber.
    /// `cargo test` runs test threads in one process, so without this
    /// the two above race on `TEMPLEDB_PATH`.
    fn with_scratch_env(f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_path = std::env::var("TEMPLEDB_PATH").ok();
        let old_home = std::env::var("HOME").ok();
        f();
        restore("TEMPLEDB_PATH", old_path);
        restore("HOME", old_home);
    }

    fn restore(key: &str, value: Option<String>) {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
