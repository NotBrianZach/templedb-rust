//! The Postgres backend.
//!
//! # Why the synchronous driver
//!
//! `postgres`, not `tokio-postgres` and not `sqlx`. The async drivers
//! would make every function in this crate `async`, and the fifteen
//! public functions `lib.rs` exports are all called from a CLI that
//! exits after one query — there is no concurrency to recover the cost
//! from. `sqlx` additionally wants a live database at compile time for
//! its query checking, which would make `cargo build` depend on a
//! running Postgres; a port whose build breaks when a server is down is
//! worse than one with unchecked SQL. `postgres` is a thin synchronous
//! wrapper over `tokio-postgres` and keeps the whole crate sync.
//!
//! # The type bridge
//!
//! This is the file that earns [`Value`]'s existence. SQLite is
//! dynamically typed and hands back whatever was stored; Postgres is
//! statically typed and hands back exactly the declared type, so every
//! `get` has to know the column's type in advance. Doing that per call
//! site would mean the query modules encoding Postgres's type system,
//! which is the opposite of the point. Instead [`value_from_pg`]
//! narrows Postgres's types into SQLite's five storage classes once,
//! here, where the mapping can be read as a table.
//!
//! An unmapped type is an error naming both the column and the type,
//! not a silent NULL. The case that motivated it: `projects.rs` selects
//! `SUM(fc.line_count)`, and Postgres's `sum(bigint)` returns `numeric`
//! where SQLite's returns an integer. That query now carries an
//! explicit `CAST(... AS BIGINT)` — portable to both engines — so the
//! numeric branch is unreachable, but it reports clearly rather than
//! guessing if another aggregate reintroduces it.

use crate::dialect::Dialect;
use crate::store::Store;
use crate::value::{Row, Value};
use anyhow::{bail, Context, Result};
use postgres::types::{to_sql_checked, IsNull, ToSql, Type};
use postgres::{Client, NoTls, Statement};
use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;

use super::sqlite::first_line;

/// A read-only Postgres session.
///
/// `RefCell` because `postgres::Client::query` takes `&mut self` while
/// [`Store::query`] takes `&self`. Giving the trait a `&mut self`
/// receiver instead would push the mutability into every caller —
/// including the BFS in `entities.rs`, which holds its own borrows
/// across the call — for no benefit in a single-threaded CLI.
pub struct PgStore {
    client: RefCell<Client>,
    /// Prepared statements, keyed on the *translated* SQL.
    ///
    /// `postgres` has no `prepare_cached`, and the BFS in `entities.rs`
    /// issues one query per visited node: without this, a deep
    /// `entity paths` pays a round-trip for `PREPARE` on every hop, on
    /// a connection where a round-trip is a socket rather than a
    /// function call. This is the SQLite backend's `prepare_cached`
    /// reproduced by hand, and the reason the cache is keyed on the
    /// post-`bind` string is so the key matches what was prepared.
    cache: RefCell<HashMap<String, Statement>>,
}

impl PgStore {
    /// Connect using a libpq connection string or URL.
    ///
    /// The session is pinned read-only at the server rather than by
    /// convention, which is the closest equivalent to the SQLite
    /// backend's `SQLITE_OPEN_READ_ONLY`. It is a backstop, not the
    /// primary control: the role this connects as should be granted
    /// `SELECT` and nothing else.
    pub fn connect(conn_string: &str) -> Result<Self> {
        let mut client = Client::connect(conn_string, NoTls)
            .with_context(|| format!("connecting to {}", redact(conn_string)))?;
        client
            .batch_execute("SET default_transaction_read_only = on")
            .context("pinning the session read-only")?;
        Ok(Self {
            client: RefCell::new(client),
            cache: RefCell::new(HashMap::new()),
        })
    }

    fn prepared(&self, sql: &str) -> Result<Statement> {
        if let Some(stmt) = self.cache.borrow().get(sql) {
            return Ok(stmt.clone());
        }
        let stmt = self
            .client
            .borrow_mut()
            .prepare(sql)
            .with_context(|| format!("preparing: {}", first_line(sql)))?;
        self.cache
            .borrow_mut()
            .insert(sql.to_string(), stmt.clone());
        Ok(stmt)
    }
}

impl Store for PgStore {
    fn dialect(&self) -> Dialect {
        Dialect::Postgres
    }

    fn query(&self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let translated = self.dialect().bind(sql)?;
        let stmt = self.prepared(&translated)?;
        let bound: Vec<&(dyn ToSql + Sync)> =
            params.iter().map(|v| v as &(dyn ToSql + Sync)).collect();

        let rows = self
            .client
            .borrow_mut()
            .query(&stmt, &bound)
            .with_context(|| format!("running: {}", first_line(sql)))?;

        rows.iter().map(convert_row).collect()
    }
}

fn convert_row(row: &postgres::Row) -> Result<Row> {
    let mut values = Vec::with_capacity(row.len());
    for (i, column) in row.columns().iter().enumerate() {
        values.push(
            value_from_pg(row, i, column.type_())
                .with_context(|| format!("column {i} ({})", column.name()))?,
        );
    }
    Ok(Row::new(values))
}

/// Narrow one Postgres column into a [`Value`].
///
/// The groupings are chosen so that a column's *storage class* survives
/// the trip even when its exact type does not. All three integer widths
/// become `Int`, because a query that does not care whether a count
/// came back as `int4` or `int8` should not have to say so, and
/// SQLite's integers are all 64-bit anyway.
///
/// `BOOL` becomes `Int(0 | 1)` rather than a sixth variant, which is
/// what lets `WHERE fc.is_current = 1` stay one SQL string across both
/// engines even if a future schema revision declares that column
/// `boolean` instead of `integer`.
fn value_from_pg(row: &postgres::Row, i: usize, ty: &Type) -> Result<Value> {
    // Every arm reads as `Option<T>`, so a NULL in any column type
    // lands on `Value::Null` instead of panicking in `FromSql`.
    let v = match *ty {
        Type::BOOL => row
            .get::<_, Option<bool>>(i)
            .map(|b| Value::Int(i64::from(b))),
        Type::INT2 => row.get::<_, Option<i16>>(i).map(|n| Value::Int(n.into())),
        Type::INT4 => row.get::<_, Option<i32>>(i).map(|n| Value::Int(n.into())),
        Type::INT8 => row.get::<_, Option<i64>>(i).map(Value::Int),
        Type::OID => row.get::<_, Option<u32>>(i).map(|n| Value::Int(n.into())),
        Type::FLOAT4 => row.get::<_, Option<f32>>(i).map(|f| Value::Real(f.into())),
        Type::FLOAT8 => row.get::<_, Option<f64>>(i).map(Value::Real),
        // `UNKNOWN` is what an uncoerced literal has in a bare SELECT.
        // The mirror's views cast their literals explicitly (`'current'
        // ::text AS revision`) so this should not arise, but reading it
        // as text is both the only sensible guess and what Postgres
        // itself would coerce it to.
        // Unescaped on the way out, so every caller above this layer
        // sees the bytes SQLite holds. Postgres `text` cannot store
        // U+0000 and SQLite's can, and one real file in the live
        // database contains one — see `pgtext.rs` for the encoding and
        // for why escaping beat the three alternatives. The transform
        // is a no-op and allocation-free for text with no U+0000 and no
        // U+0001, which is every row but three.
        //
        // This is also what keeps `statediff`'s cross-backend digest
        // meaningful: the digest hashes rows read through `Store`, so it
        // sees the original text on both backends rather than the
        // stored form on one.
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => row
            .get::<_, Option<String>>(i)
            .map(|s| Value::Text(crate::pgtext::unescape(&s).into_owned())),
        Type::BYTEA => row.get::<_, Option<Vec<u8>>>(i).map(Value::Blob),
        // Deliberately unmapped, with the fix named. `numeric` is what
        // `sum()` over an integer column returns in Postgres and not in
        // SQLite; reading it would need a decimal crate, and the right
        // answer is to cast in SQL where both engines agree.
        Type::NUMERIC => bail!(
            "numeric is not mapped: wrap the expression in \
             CAST(... AS BIGINT), which both SQLite and Postgres accept"
        ),
        // Timestamps in this schema are TEXT, written by Python `str`.
        // A real timestamp column would need a date crate and would
        // format differently from SQLite's text, which is a parity
        // decision rather than a conversion.
        Type::TIMESTAMP | Type::TIMESTAMPTZ | Type::DATE | Type::TIME => bail!(
            "{ty} is not mapped: this schema stores timestamps as text, so \
             either keep the column text or cast it in SQL"
        ),
        ref other => bail!("no Value mapping for Postgres type {other}"),
    };
    Ok(v.unwrap_or(Value::Null))
}

/// Bind a [`Value`] as a query parameter.
///
/// Hand-written rather than converting to a concrete Rust type because
/// of `Value::Null`. Boxing it as, say, `Option::<i64>::None` would
/// send an `int8` NULL, and `search.rs`'s `(?2 IS NULL OR p.slug = ?2)`
/// filter would then fail type resolution against a `text` column. Here
/// NULL is `IsNull::Yes` whatever the inferred type, which is what the
/// wire protocol wants and what SQLite does implicitly.
impl ToSql for Value {
    fn to_sql(
        &self,
        ty: &Type,
        out: &mut postgres::types::private::BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn Error + Sync + Send>> {
        match self {
            Value::Null => Ok(IsNull::Yes),
            // Narrow to the width Postgres asked for. A `LIMIT`
            // parameter is `int8`, but a comparison against an `int4`
            // column is `int4`, and sending eight bytes for a
            // four-byte type is a protocol error rather than a
            // coercion.
            Value::Int(n) => match *ty {
                Type::INT2 => i16::try_from(*n)?.to_sql(ty, out),
                Type::INT4 => i32::try_from(*n)?.to_sql(ty, out),
                Type::BOOL => (*n != 0).to_sql(ty, out),
                _ => n.to_sql(ty, out),
            },
            Value::Real(f) => match *ty {
                Type::FLOAT4 => (*f as f32).to_sql(ty, out),
                _ => f.to_sql(ty, out),
            },
            Value::Text(s) => s.to_sql(ty, out),
            Value::Blob(b) => b.to_sql(ty, out),
            // `bigint[]`, for `= ANY($1)`. This is the variant that
            // makes the level-batched BFS worth doing on Postgres: one
            // parameter whatever the frontier's width, so the query
            // text never changes and the prepared statement is reused
            // across every level.
            Value::IntSet(v) => v.to_sql(ty, out),
        }
    }

    /// Everything, because the variant is chosen at runtime and the
    /// narrowing above is what actually validates the pair. The
    /// `to_sql_checked!` wrapper still enforces it per call.
    fn accepts(_ty: &Type) -> bool {
        true
    }

    to_sql_checked!();
}

/// Strip anything after a `:` in the userinfo of a connection URL, so a
/// failed connection does not put a password in a log or an error
/// message. Leaves non-URL libpq strings alone apart from a `password=`
/// field.
fn redact(conn_string: &str) -> String {
    if let Some((scheme, rest)) = conn_string.split_once("://") {
        if let Some((userinfo, host)) = rest.split_once('@') {
            let user = userinfo.split(':').next().unwrap_or("");
            return format!("{scheme}://{user}:***@{host}");
        }
        return conn_string.to_string();
    }
    conn_string
        .split_whitespace()
        .map(|field| {
            if field.starts_with("password=") {
                "password=***".to_string()
            } else {
                field.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failed connection must not echo the password back. These
    /// strings end up in `anyhow` chains that get printed to a terminal
    /// and pasted into bug reports.
    #[test]
    fn connection_strings_are_redacted() {
        assert_eq!(
            redact("postgres://zach:hunter2@localhost:5432/templedb"),
            "postgres://zach:***@localhost:5432/templedb"
        );
        assert_eq!(
            redact("host=localhost user=zach password=hunter2 dbname=templedb"),
            "host=localhost user=zach password=*** dbname=templedb"
        );
        // Nothing to redact, nothing changed.
        assert_eq!(redact("postgres:///templedb"), "postgres:///templedb");
        assert!(!redact("postgres://zach:hunter2@/templedb").contains("hunter2"));
    }
}
