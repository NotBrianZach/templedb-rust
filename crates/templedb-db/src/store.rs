//! The backend boundary: one trait, two implementations.
//!
//! # Why the trait is this narrow
//!
//! The obvious alternative was a trait with one method per command —
//! `fn list_projects(&self) -> Result<Vec<Project>>`, and so on for the
//! fifteen functions `lib.rs` re-exports. It was rejected, and the
//! reason is worth recording because it looks like the cleaner design.
//!
//! Per-command methods would mean writing every query twice, once per
//! dialect, when fewer than a tenth of them actually differ. The cost
//! is not the typing; it is that two copies of a query drift, and a
//! drift between backends is exactly the class of bug this port exists
//! to catch. A single query string that both engines run is a stronger
//! guarantee than two strings and a test asserting they agree.
//!
//! So the trait carries `query` and nothing else, the query modules own
//! their SQL, and the three places where the dialects genuinely diverge
//! are a `match` on [`Dialect`] in the module that owns the query.
//!
//! # Why it returns owned rows
//!
//! A borrowing row — `rusqlite`'s shape, where `Row<'_>` is valid only
//! inside the `query_map` closure — cannot be put behind a trait object
//! without lending the lifetime to the trait, and the callers in this
//! crate collect everything into a `Vec` anyway. Materialising is also
//! what makes the type bridge possible: Postgres's typed columns are
//! narrowed into [`Value`] once, at the boundary, rather than at every
//! `get` call site.
//!
//! The one place this costs something is the BFS in `entities.rs`, which
//! allocates a `Vec<Row>` per visited node. It was already collecting
//! each node's edges into a `Vec`, so the extra cost is the `Row`
//! wrapper rather than the rows themselves, and `entity paths` across
//! the whole 64,562-row `relations` table still lands in the same
//! tens of milliseconds.

use crate::dialect::Dialect;
use crate::value::{Row, Value};
use anyhow::Result;

/// A read-only query interface over one database.
///
/// Read-only is a property of every implementation rather than of the
/// trait, which has no way to express it. The SQLite backend opens with
/// `SQLITE_OPEN_READ_ONLY`; the Postgres one holds a session that should
/// be granted `SELECT` and nothing else. Keeping writes out of this
/// trait is what lets both implementations coexist with the Python
/// implementation's exclusive authority over the live SQLite file.
pub trait Store {
    /// Which dialect [`Store::query`] expects its SQL in.
    ///
    /// Callers write SQLite-flavoured SQL with `?N` placeholders; this
    /// tells the handful of queries that cannot be written once which
    /// engine they are talking to.
    fn dialect(&self) -> Dialect;

    /// Run a query and materialise every row.
    ///
    /// `sql` uses `?N` placeholders regardless of backend — the
    /// Postgres implementation rewrites them (see [`Dialect::bind`]).
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;

    /// Run a query expected to yield at most one row.
    ///
    /// Extra rows are discarded rather than being an error, matching
    /// `rusqlite::Connection::query_row`, which the callers of this were
    /// written against. Every such query in this crate carries an
    /// explicit `LIMIT 1` or selects on a unique key, so the
    /// discarding branch is unreachable by construction; relying on the
    /// `LIMIT` rather than enforcing it here keeps the behaviour the
    /// same as the SQLite-only version it replaces.
    fn query_opt(&self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        Ok(self.query(sql, params)?.into_iter().next())
    }
}

/// Blanket forwarding so `&dyn Store` and `Box<dyn Store>` are usable
/// wherever a `&impl Store` is, without every call site deciding which
/// it holds. `open_ro` hands back a `Box<dyn Store>`; the query modules
/// take `&dyn Store`.
impl<T: Store + ?Sized> Store for &T {
    fn dialect(&self) -> Dialect {
        (**self).dialect()
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        (**self).query(sql, params)
    }
}

impl<T: Store + ?Sized> Store for Box<T> {
    fn dialect(&self) -> Dialect {
        (**self).dialect()
    }
    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        (**self).query(sql, params)
    }
}
