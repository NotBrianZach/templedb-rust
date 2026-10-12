//! Owned column values and rows, so no driver type escapes this crate.
//!
//! Before this module the query closures took a `&rusqlite::Row` and
//! `lib.rs` re-exported `rusqlite::Connection` outright, with a comment
//! admitting that a CLI able to name the driver would eventually reach
//! around the SQL layer. Supporting a second backend is what forces the
//! issue: the row type is the one place every query touches the driver,
//! so it is the one place a second driver has to be absorbed.
//!
//! The type set is SQLite's storage classes, not Postgres's — five
//! variants against Postgres's several hundred OIDs. That direction is
//! deliberate. SQLite is dynamically typed, so its driver hands back
//! whatever was stored and the queries in this crate were written
//! against that latitude; Postgres is statically typed and hands back
//! exactly the declared type. Narrowing Postgres into this set at the
//! backend boundary (see `backend::pg::value_from_pg`) means the
//! widening happens once, in a table that can be read, instead of
//! per-query at each `get`.
//!
//! The specific bug this shape prevents: `projects.rs` selects
//! `COALESCE(SUM(fc.line_count), 0)`. SQLite's `sum()` over integers
//! returns an integer; Postgres's `sum(bigint)` returns `numeric`. A
//! typed `row.get::<i64>(4)` compiles against both and fails at runtime
//! against one of them. Mapping NUMERIC into `Int` at the boundary makes
//! the failure impossible rather than merely tested for.

use anyhow::{bail, Result};

/// One column value, in SQLite's five storage classes.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
    /// A set of integers, for `= ANY(?1)`.
    ///
    /// The one variant that is not a SQLite storage class. The BFS in
    /// `entities.rs` fetches a whole level at a time and has to ask
    /// "which of these several hundred ids", which SQLite can only
    /// express as an `IN` list of that many placeholders. Postgres
    /// takes the whole set as one `bigint[]`, which keeps the query
    /// text constant across levels — so the prepared statement is
    /// reused — and stops the server probing the index for the padding
    /// an `IN` list needs to hit a cache-friendly width.
    ///
    /// Never produced by a query — no column reads back as this — so
    /// `Row` has no getter for it. Parameters only.
    IntSet(Vec<i64>),
}

impl Value {
    /// The variant name, for error messages. Not `Display`: this is
    /// diagnostic text about the value's *type*, never the value.
    fn type_name(&self) -> &'static str {
        match self {
            Value::Null => "NULL",
            Value::Int(_) => "integer",
            Value::Real(_) => "real",
            Value::Text(_) => "text",
            Value::Blob(_) => "blob",
            Value::IntSet(_) => "integer set",
        }
    }
}

impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Text(v)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_string())
    }
}

/// `None` becomes SQL NULL rather than an absent parameter, which is
/// what `search.rs`'s `?2 IS NULL OR p.slug = ?2` filter relies on.
impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(inner) => inner.into(),
            None => Value::Null,
        }
    }
}

/// One result row: positional values, as both drivers deliver them.
///
/// The getters are fallible rather than panicking because a type
/// mismatch here means the SQL and the backend's schema disagree — a
/// real condition when the same query text runs against two dialects,
/// and one worth surfacing with the column index attached instead of a
/// bare `unwrap` panic deep in a BFS.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    values: Vec<Value>,
}

impl Row {
    pub fn new(values: Vec<Value>) -> Self {
        Self { values }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// The raw value, for callers that want to move it somewhere
    /// untyped rather than read it as a particular Rust type. The
    /// mirror uses this: it copies columns through without caring what
    /// is in them, and a typed getter per column would mean the mirror
    /// restating the schema it is copying.
    pub fn value(&self, i: usize) -> Result<&Value> {
        self.at(i)
    }

    fn at(&self, i: usize) -> Result<&Value> {
        self.values.get(i).ok_or_else(|| {
            anyhow::anyhow!("column {i} out of range ({} columns)", self.values.len())
        })
    }

    /// A required integer column.
    pub fn get_i64(&self, i: usize) -> Result<i64> {
        match self.at(i)? {
            Value::Int(v) => Ok(*v),
            other => bail!("column {i}: expected integer, got {}", other.type_name()),
        }
    }

    /// A nullable integer column — `line_count` and `file_size_bytes`
    /// are both NULL for a deletion row.
    pub fn get_opt_i64(&self, i: usize) -> Result<Option<i64>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            Value::Int(v) => Ok(Some(*v)),
            other => bail!(
                "column {i}: expected integer or NULL, got {}",
                other.type_name()
            ),
        }
    }

    /// A required text column.
    pub fn get_string(&self, i: usize) -> Result<String> {
        match self.at(i)? {
            Value::Text(v) => Ok(v.clone()),
            other => bail!("column {i}: expected text, got {}", other.type_name()),
        }
    }

    /// A nullable text column — `label`, `external_ref`, `name` and
    /// `content_hash` are all nullable in the live schema, and each of
    /// them being read as `Option` rather than unwrapped is load-bearing
    /// somewhere in this crate.
    pub fn get_opt_string(&self, i: usize) -> Result<Option<String>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            Value::Text(v) => Ok(Some(v.clone())),
            other => bail!(
                "column {i}: expected text or NULL, got {}",
                other.type_name()
            ),
        }
    }

    /// A nullable blob column. `content_blobs.content_blob` is NULL for
    /// every text file and populated only for binary ones, so the
    /// `Option` is the normal case rather than an edge case.
    pub fn get_opt_blob(&self, i: usize) -> Result<Option<Vec<u8>>> {
        match self.at(i)? {
            Value::Null => Ok(None),
            Value::Blob(v) => Ok(Some(v.clone())),
            // A text blob stored in a BLOB column reads back as Text on
            // SQLite, which is dynamically typed. Accepting it keeps the
            // two backends agreeing on a column the schema declares BLOB
            // but SQLite does not enforce.
            Value::Text(v) => Ok(Some(v.as_bytes().to_vec())),
            other => bail!(
                "column {i}: expected blob or NULL, got {}",
                other.type_name()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn getters_report_the_column_and_both_types_on_a_mismatch() {
        let row = Row::new(vec![Value::Text("x".into()), Value::Null]);
        let err = row.get_i64(0).unwrap_err().to_string();
        assert!(err.contains("column 0"), "{err}");
        assert!(err.contains("expected integer"), "{err}");
        assert!(err.contains("got text"), "{err}");
    }

    /// A required getter must refuse NULL rather than substituting a
    /// zero or an empty string. Both would be silently wrong output.
    #[test]
    fn required_getters_refuse_null() {
        let row = Row::new(vec![Value::Null]);
        assert!(row.get_i64(0).is_err());
        assert!(row.get_string(0).is_err());
        // The nullable forms accept it.
        assert_eq!(row.get_opt_i64(0).unwrap(), None);
        assert_eq!(row.get_opt_string(0).unwrap(), None);
    }

    #[test]
    fn out_of_range_names_the_width() {
        let row = Row::new(vec![Value::Int(1)]);
        let err = row.get_i64(5).unwrap_err().to_string();
        assert!(err.contains("out of range"), "{err}");
        assert!(err.contains("1 columns"), "{err}");
    }

    /// `Option<&str>` has to reach SQL as NULL, not as the string
    /// "None" and not as a missing parameter. The Python side's
    /// equivalent bug — an f-string turning a NULL name into the literal
    /// `"None (slug)"` — is in the README.
    #[test]
    fn option_params_become_sql_null() {
        assert_eq!(Value::from(None::<&str>), Value::Null);
        assert_eq!(Value::from(Some("beta")), Value::Text("beta".into()));
    }

    /// SQLite returns text for a value stored as text in a BLOB column.
    /// `get_opt_blob` accepts it so the backends agree.
    #[test]
    fn blob_getter_accepts_text_storage() {
        let row = Row::new(vec![Value::Text("PNG".into())]);
        assert_eq!(row.get_opt_blob(0).unwrap(), Some(b"PNG".to_vec()));
    }
}
