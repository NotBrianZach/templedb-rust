//! Which database to open, and which backend to open it with.

use crate::backend::pg::PgStore;
use crate::backend::sqlite::SqliteStore;
use crate::store::Store;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// The connection URL for the Postgres backend.
///
/// A separate variable from `TEMPLEDB_PATH` rather than one variable
/// holding either, because the two have different shapes and a single
/// overloaded variable makes "which backend am I talking to" a question
/// about string parsing. With two, the answer is which one is set.
pub const URL_ENV: &str = "TEMPLEDB_URL";

/// Resolve the SQLite database path exactly as the Python side does:
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

/// Which backend the environment selects, without connecting to it.
///
/// Split out from [`open_ro`] so the choice is testable without a
/// database of either kind, and so an error names the misconfiguration
/// rather than failing later as a connection error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Sqlite(PathBuf),
    Postgres(String),
}

/// Read the environment and decide what to open.
///
/// `TEMPLEDB_URL` wins when set, otherwise SQLite. A URL with an
/// unrecognised scheme is an error rather than a fall-through: silently
/// reading a different database than the caller asked for is the exact
/// failure the empty-`TEMPLEDB_PATH` case below exists to prevent, and
/// a typo in a scheme deserves the same treatment.
pub fn target() -> Result<Target> {
    if let Ok(url) = std::env::var(URL_ENV) {
        // An exported-but-empty variable is a common shell accident and
        // means "unset", the same way it does for `TEMPLEDB_PATH`.
        if !url.is_empty() {
            if url.starts_with("postgres://")
                || url.starts_with("postgresql://")
                // libpq keyword/value form, e.g. `host=/tmp dbname=x`.
                || url.contains('=')
            {
                return Ok(Target::Postgres(url));
            }
            anyhow::bail!(
                "{URL_ENV} is set but is not a Postgres connection string \
                 (expected a `postgres://` URL or libpq `key=value` pairs); \
                 unset it to use SQLite at {}",
                db_path()?.display()
            );
        }
    }
    Ok(Target::Sqlite(db_path()?))
}

/// Open the database the environment selects, read-only.
///
/// Returns a trait object rather than a generic because the choice is
/// made from the environment at startup: a type parameter would have to
/// be resolved by the caller, which is the CLI, which would then need
/// to duplicate every command body once per backend.
pub fn open_ro() -> Result<Box<dyn Store>> {
    match target()? {
        Target::Sqlite(path) => Ok(Box::new(SqliteStore::open_ro(&path)?)),
        Target::Postgres(url) => Ok(Box::new(PgStore::connect(&url)?)),
    }
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

    #[test]
    fn no_url_selects_sqlite() {
        with_scratch_env(|| {
            std::env::remove_var(URL_ENV);
            std::env::set_var("TEMPLEDB_PATH", "/tmp/x.sqlite");
            assert_eq!(
                target().unwrap(),
                Target::Sqlite(PathBuf::from("/tmp/x.sqlite"))
            );
        });
    }

    #[test]
    fn a_postgres_url_selects_postgres() {
        with_scratch_env(|| {
            for url in [
                "postgres://localhost/templedb",
                "postgresql://localhost/templedb",
                "host=/tmp dbname=templedb",
            ] {
                std::env::set_var(URL_ENV, url);
                assert_eq!(
                    target().unwrap(),
                    Target::Postgres(url.to_string()),
                    "{url}"
                );
            }
        });
    }

    /// The same reasoning as the empty-`TEMPLEDB_PATH` test: an
    /// exported-but-empty variable means unset, not "open nothing".
    #[test]
    fn an_empty_url_falls_back_to_sqlite() {
        with_scratch_env(|| {
            std::env::set_var(URL_ENV, "");
            std::env::set_var("TEMPLEDB_PATH", "/tmp/x.sqlite");
            assert_eq!(
                target().unwrap(),
                Target::Sqlite(PathBuf::from("/tmp/x.sqlite"))
            );
        });
    }

    /// A mistyped scheme must not quietly read the SQLite database. It
    /// would answer every query successfully against the wrong data,
    /// which is the worst available outcome.
    #[test]
    fn an_unrecognised_url_is_an_error_not_a_fallback() {
        with_scratch_env(|| {
            std::env::set_var(URL_ENV, "postgress://localhost/templedb");
            std::env::set_var("TEMPLEDB_PATH", "/tmp/x.sqlite");
            let err = target().unwrap_err().to_string();
            assert!(err.contains("not a Postgres connection string"), "{err}");
            // And it says what to do about it.
            assert!(err.contains("/tmp/x.sqlite"), "{err}");
        });
    }

    /// Serialise the env-mutating tests and restore what they clobber.
    /// `cargo test` runs test threads in one process, so without this
    /// they race on `TEMPLEDB_PATH` and `TEMPLEDB_URL`.
    fn with_scratch_env(f: impl FnOnce()) {
        use std::sync::Mutex;
        static LOCK: Mutex<()> = Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<String>)> = ["TEMPLEDB_PATH", URL_ENV, "HOME"]
            .into_iter()
            .map(|k| (k, std::env::var(k).ok()))
            .collect();
        // Start from a known state: a stray TEMPLEDB_URL in the
        // developer's shell must not change what these tests assert.
        std::env::remove_var(URL_ENV);
        f();
        for (key, value) in saved {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}
