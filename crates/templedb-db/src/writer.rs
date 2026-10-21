//! The write path's handle, and the only thing in this crate that can
//! mutate a database the port is expected to read back.
//!
//! # Why this is not a `Store` method
//!
//! [`Store`](crate::Store) has `query`, `query_opt` and `dialect`, and
//! deliberately nothing else. The write-path design document
//! (*The write path: what porting it actually requires*, 2026-10-15)
//! recommends against widening it, and the three reasons survive the
//! document's own later reversal of which engine gets written:
//!
//! 1. **A write trait would have one implementation.** There is one
//!    writable backend and there is not going to be a second.
//! 2. **Widening the trait costs every reader its guarantee.** Today a
//!    `&dyn Store` provably cannot write — the SQLite backend holds a
//!    `SQLITE_OPEN_READ_ONLY` handle and the Postgres parity target is
//!    reached through a read-only role. That is cheap insurance and
//!    there is no reason to spend it.
//! 3. **Transactions do not fit behind a trait object.**
//!    `postgres::Transaction` borrows its `Client`, so it cannot be
//!    handed back through `&dyn Store` — the same lifetime reason
//!    `store.rs` already records for keeping `query_map` out.
//!
//! So a write means a second, explicitly-opened handle, and the read
//! handle stays exactly what it is. `mirror` set this precedent: it
//! owns a `postgres::Client` outside the trait and writes only the
//! database it owns.
//!
//! # Postgres, not SQLite
//!
//! The design document's body recommends a `SqliteWriter`. Its
//! 2026-10-16 decision reverses that — Postgres is primary and the
//! port does not carry SQLite — and the README's porting order
//! repeated the superseded recommendation until 2026-10-21. This writes
//! Postgres.
//!
//! That is also the safe direction. The live SQLite file is Python's
//! and its write paths carry invariants a second writer would violate;
//! the port's Postgres is disposable, re-mirrored at will, and nothing
//! else depends on it. A wrong write costs a `just mirror-refresh`.
//!
//! # Two databases, and why the writer will not touch the other one
//!
//! `mirror` opens with `TRUNCATE`. A database used as a read-parity
//! target must therefore only ever be written by `mirror`, or a sweep
//! run after a write test reports the port's own writes as parity
//! failures — and the next re-mirror silently destroys whatever the
//! write tests built. Both failures look like bugs in something else.
//!
//! So the writer reads a *different* variable from the reader:
//! [`WRITE_URL_ENV`] rather than `TEMPLEDB_URL`. Isolation by
//! connection string rather than by `search_path`, because a wrong
//! database name is loud and a wrong `search_path` is silent, and the
//! operation on the other side of that mistake is `TRUNCATE`.
//!
//! # The opt-in
//!
//! [`OPT_IN_ENV`] must be set to `1` before any write is attempted.
//! The design document asks for this explicitly — "keep the writer
//! behind an explicit opt-in (an env var, not a default)" — and the
//! reason is that the write path is incomplete in ways a caller cannot
//! see from the command line. `file set` in this port reproduces the
//! SQL and two of Python's seven guards; a default-on writer would let
//! a script reach it by habit.

use anyhow::{bail, Context, Result};
use postgres::{Client, NoTls};

use crate::backend::pg::redact;

/// Where the writable database lives. Deliberately not `TEMPLEDB_URL`.
pub const WRITE_URL_ENV: &str = "TEMPLEDB_WRITE_URL";

/// Must be `1`. See the module note.
pub const OPT_IN_ENV: &str = "TEMPLEDB_RS_ALLOW_WRITES";

/// A writable Postgres handle.
///
/// Holds the `Client` rather than a transaction, because a transaction
/// borrows the client and the lifetime has to start at the call site
/// that knows what the unit of work is.
pub struct PgWriter {
    client: Client,
}

impl PgWriter {
    /// Connect to `url`.
    ///
    /// Does not consult the opt-in: [`PgWriter::open`] does that, and
    /// this exists so a test can build one without setting a process
    /// -wide environment variable.
    pub fn connect(url: &str) -> Result<PgWriter> {
        let client = Client::connect(url, NoTls)
            .with_context(|| format!("connecting to the write target ({})", redact(url)))?;
        Ok(PgWriter { client })
    }

    /// Resolve the write target from the environment and connect.
    ///
    /// Both refusals are plain `bail!`, which `exit.rs` maps to
    /// [`USAGE`](crate::USAGE) — the default for anything with no
    /// `Coded` in its chain, and the right status for a caller doing
    /// something wrong rather than a database being unavailable. Not a
    /// new `Coded` variant: `exit.rs` deliberately offers only
    /// `not_found`, on the grounds that a second way to spell the
    /// default would invite drift.
    pub fn open() -> Result<PgWriter> {
        match std::env::var(OPT_IN_ENV) {
            Ok(v) if v == "1" => {}
            _ => bail!(
                "writes are opt-in: set {OPT_IN_ENV}=1 to enable them.\n\
                 This port's write path is incomplete in ways the command \
                 line does not show — see the README's *The first write* — \
                 so it does not default on."
            ),
        }
        let url = match std::env::var(WRITE_URL_ENV) {
            Ok(v) if !v.trim().is_empty() => v,
            _ => bail!(
                "no write target: set {WRITE_URL_ENV} to a Postgres URL.\n\
                 Deliberately not {}: that one names the read-parity target, \
                 which only `mirror` may write — it opens with TRUNCATE.",
                crate::conn::URL_ENV
            ),
        };
        PgWriter::connect(url.trim())
    }

    /// Begin a unit of work.
    ///
    /// Every write in this crate goes through one of these. There is no
    /// autocommit path, because the first write already spans three
    /// tables and a partial application of it is the one state that
    /// looks healthy and answers wrongly.
    pub fn transaction(&mut self) -> Result<postgres::Transaction<'_>> {
        self.client.transaction().context("opening a transaction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exit::{code_of, USAGE};

    /// Both refusals are usage errors, and both say which variable to
    /// set. A write path's refusals are its contract.
    ///
    /// Reads the real environment, so it asserts the *shape* of the
    /// refusal rather than setting variables — `std::env::set_var` is
    /// process-wide and racing with another test would make this
    /// flaky in a way that is hard to see.
    #[test]
    fn the_opt_in_and_the_url_both_refuse_with_usage() {
        // Whichever of the two is unset in this process, `open` must
        // refuse with USAGE and name the variable. In CI neither is
        // set, so this takes the opt-in arm.
        if std::env::var(OPT_IN_ENV).as_deref() == Ok("1") && std::env::var(WRITE_URL_ENV).is_ok() {
            // A developer shell with both set: nothing to assert
            // without connecting, which this test will not do.
            return;
        }
        // `expect_err` would need `PgWriter: Debug`, and a handle whose
        // `Debug` printed a connection string is exactly what `redact`
        // exists to prevent. So match instead.
        let Err(err) = PgWriter::open() else {
            panic!("should refuse without the opt-in");
        };
        assert_eq!(code_of(&err), USAGE);
        let msg = format!("{err:#}");
        assert!(
            msg.contains(OPT_IN_ENV) || msg.contains(WRITE_URL_ENV),
            "{msg:?} should name the variable to set"
        );
    }

    /// The two variables are distinct names, which is the entire
    /// mechanism keeping `TRUNCATE` away from the write scratch and the
    /// writer away from the parity target.
    #[test]
    fn the_write_url_is_not_the_read_url() {
        assert_ne!(WRITE_URL_ENV, crate::conn::URL_ENV);
    }
}
