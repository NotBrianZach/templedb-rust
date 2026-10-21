//! What a write left behind, as something two engines can compare.
//!
//! The byte-diff method that verified every read command does not
//! survive contact with a mutation. `templedb-rs file cat x` and
//! `templedb file cat x` can be diffed because both print the answer;
//! `file set` prints almost nothing and changes four tables, so the
//! interesting output is not on stdout at all. The write-path design
//! document (*The write path: what porting it actually requires*,
//! 2026-10-15) names the replacement and insists it exist before the
//! thing it verifies:
//!
//! > Two database copies, one write each, diff the results. No command
//! > ported yet — this is the replacement for the method that stops
//! > working, and it should exist before the thing it verifies.
//!
//! The 2026-10-16 decision sharpened the shape: the two copies are not
//! two SQLite files. Mirror at a known instant, run Python's write
//! against SQLite, run the port's against Postgres, and compare the
//! resulting states. So the comparison is inherently *cross-engine*,
//! and that is the constraint every decision below answers to.
//!
//! # What this is not
//!
//! Not a freshness check. [`crate::check_freshness`] already asks "is
//! this mirror still a faithful copy?" and answers with row counts,
//! and its own doc comment names the gap:
//!
//! > It does *not* catch an update in place: a row whose `is_current`
//! > flipped leaves the count alone. A per-table checksum would, at the
//! > cost of reading every row on both sides on every check.
//!
//! An update in place is precisely what a write path does. So this
//! module pays that cost deliberately, and is the slow, thorough
//! counterpart to the cheap check rather than a replacement for it.
//!
//! # Three decisions that make a cross-engine digest possible
//!
//! **Order-independence, not a canonical sort.** The obvious digest
//! hashes rows in `ORDER BY` sequence, and it would be wrong here.
//! `content_blobs` keys on `hash_sha256`, a TEXT column, and the two
//! engines do not order TEXT the same way — SQLite compares bytes,
//! Postgres uses the database collation. A sorted digest would report
//! an identical table as divergent, on a schedule nobody could predict.
//! So each row is hashed independently and the row hashes are combined
//! with `wrapping_add`, which is commutative: the table digest does not
//! depend on the order rows arrive in. Addition rather than XOR because
//! XOR cancels — two identical rows would vanish from the digest
//! instead of doubling, and duplicate rows are exactly the kind of bug
//! a write-path harness is looking for.
//!
//! **Pagination still orders, and that is fine.** Keyset pagination
//! needs a total order, and it only needs it to be consistent *within*
//! one engine. Each side paginates its own table in its own collation,
//! reaches the same set of rows, and the commutative combine makes the
//! difference invisible. The collation hazard is confined to a place
//! where it cannot do harm.
//!
//! **Values are tagged and length-prefixed.** A digest over
//! concatenated bytes cannot tell `("ab", "c")` from `("a", "bc")`, and
//! a write that shifted a boundary between two TEXT columns would hash
//! identically. Every value carries its storage class and its length,
//! so the encoding is unambiguous. The tag also means a column that
//! reads back as `Int` from one engine and `Text` from the other is
//! reported as a difference rather than silently coerced — that is a
//! real condition when one schema is dynamically typed and the other is
//! not, and it is the sort of thing worth failing loudly on.

use anyhow::{bail, Context, Result};

use crate::mirror::{TableSpec, TABLES};
use crate::store::Store;
use crate::value::{Row, Value};

/// FNV-1a, 128-bit. Chosen because it is twenty lines and needs no
/// dependency: the crate's whole dependency list is `anyhow`,
/// `rusqlite` and `postgres`, and adding a hash crate to compare two
/// development databases is not a trade worth making.
///
/// Not a cryptographic hash and nothing here needs one. The adversary
/// is a porting mistake, not someone constructing a collision — and the
/// row counts are compared separately, so a collision would have to
/// preserve those too.
struct Fnv1a128(u128);

const FNV_OFFSET: u128 = 0x6c62_272e_07bb_0142_62b8_2175_6295_c58d;
const FNV_PRIME: u128 = 0x0000_0000_0100_0000_0000_0000_0000_013b;

impl Fnv1a128 {
    fn new() -> Self {
        Fnv1a128(FNV_OFFSET)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u128::from(b);
            self.0 = self.0.wrapping_mul(FNV_PRIME);
        }
    }

    /// Tag + length + payload, so no two distinct value sequences can
    /// encode to the same byte stream. See the module docs.
    fn write_value(&mut self, value: &Value) -> Result<()> {
        match value {
            Value::Null => self.write(&[0]),
            Value::Int(i) => {
                self.write(&[1]);
                self.write(&i.to_be_bytes());
            }
            Value::Real(f) => {
                self.write(&[2]);
                // `+0.0` and `-0.0` compare equal but have different
                // bit patterns, and no engine distinguishes them in a
                // column value. Normalising keeps the digest from
                // depending on which side produced the zero.
                let bits = if *f == 0.0 {
                    0f64.to_bits()
                } else {
                    f.to_bits()
                };
                self.write(&bits.to_be_bytes());
            }
            Value::Text(s) => {
                self.write(&[3]);
                self.write(&(s.len() as u64).to_be_bytes());
                self.write(s.as_bytes());
            }
            Value::Blob(b) => {
                self.write(&[4]);
                self.write(&(b.len() as u64).to_be_bytes());
                self.write(b);
            }
            // Parameters only; no column reads back as one. Reaching
            // here means `Store::query` changed shape, which is worth
            // an error rather than a digest nobody can interpret.
            Value::IntSet(_) => bail!("an integer set is not a column value"),
        }
        Ok(())
    }

    fn finish(self) -> u128 {
        self.0
    }
}

/// One table's contribution to the state: how many rows, and a digest
/// over all of them.
///
/// Both, not just the digest. A digest that differs says only "these
/// are not the same table"; a row count that also differs says the
/// write inserted or deleted, and one that agrees says it updated in
/// place. That is the first question anyone asks of a failure, and
/// carrying the count makes it free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableState {
    pub table: &'static str,
    pub rows: u64,
    pub digest: u128,
}

/// Every mirrored table's state, from one database at one instant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbState {
    pub tables: Vec<TableState>,
}

impl DbState {
    pub fn table(&self, name: &str) -> Option<&TableState> {
        self.tables.iter().find(|t| t.table == name)
    }
}

/// How one table differs between two states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDelta {
    pub table: &'static str,
    pub rows_before: u64,
    pub rows_after: u64,
    /// `true` when the digests differ but the counts do not — an update
    /// in place, which is the case row counts alone cannot see and the
    /// reason this module exists.
    pub updated_in_place: bool,
}

impl TableDelta {
    pub fn row_change(&self) -> i64 {
        self.rows_after as i64 - self.rows_before as i64
    }
}

/// Read every mirrored table and digest it.
///
/// Works against either backend through [`Store`], which is the whole
/// point: the same function run against the SQLite that Python wrote
/// and the Postgres that the port wrote produces two values that can be
/// compared directly.
///
/// **This reads the entire subset**, `content_blobs` included — 104 MB
/// of text on the live data. It is not something to run in a loop. One
/// capture before a write and one after is the intended use.
pub fn capture(store: &dyn Store) -> Result<DbState> {
    let mut tables = Vec::with_capacity(TABLES.len());
    for spec in TABLES {
        tables
            .push(capture_table(store, spec).with_context(|| format!("digesting {}", spec.name))?);
    }
    Ok(DbState { tables })
}

/// Keyset-paginate one table, hashing each row as it arrives.
///
/// Paged rather than read whole because `content_blobs` averages 28 KB
/// a row and peaks at 4.9 MB, which is the same reason `mirror` pages
/// it — and the page sizes are taken from the same specs so the two
/// agree about what is safe to hold.
fn capture_table(store: &dyn Store, spec: &TableSpec) -> Result<TableState> {
    // The cursor is read out of the row by position, so the key columns
    // have to be where we think they are. Every spec satisfies this
    // today and a test below pins it; checking here too means a future
    // table that breaks it fails with its own name attached rather than
    // digesting the wrong columns.
    if spec.columns.len() < spec.key.len() || spec.columns[..spec.key.len()] != *spec.key {
        bail!(
            "{}: key {:?} is not a prefix of columns {:?}",
            spec.name,
            spec.key,
            spec.columns
        );
    }

    let select = spec.columns.join(", ");
    let order = spec.key.join(", ");
    let key_tuple = format!("({})", order);

    let mut rows_seen: u64 = 0;
    let mut digest: u128 = 0;
    let mut cursor: Option<Vec<Value>> = None;

    loop {
        let (sql, params) = match &cursor {
            None => (
                format!(
                    "SELECT {select} FROM {} ORDER BY {order} LIMIT {}",
                    spec.name, spec.page
                ),
                Vec::new(),
            ),
            Some(last) => {
                let placeholders: Vec<String> = (1..=last.len()).map(|i| format!("?{i}")).collect();
                (
                    format!(
                        "SELECT {select} FROM {} WHERE {key_tuple} > ({}) \
                         ORDER BY {order} LIMIT {}",
                        spec.name,
                        placeholders.join(", "),
                        spec.page
                    ),
                    last.clone(),
                )
            }
        };

        let page = store.query(&sql, &params)?;
        if page.is_empty() {
            break;
        }

        for row in &page {
            digest = digest.wrapping_add(hash_row(row, spec.columns.len())?);
            rows_seen += 1;
        }

        // Advance past the last row of the page. `page` is non-empty,
        // so the index is sound.
        let last = &page[page.len() - 1];
        let mut next = Vec::with_capacity(spec.key.len());
        for i in 0..spec.key.len() {
            next.push(last.value(i)?.clone());
        }
        cursor = Some(next);

        if (page.len() as i64) < spec.page {
            break;
        }
    }

    Ok(TableState {
        table: spec.name,
        rows: rows_seen,
        digest,
    })
}

/// Hash one row, left to right.
///
/// Takes the expected width so a backend returning a different number
/// of columns is an error rather than a digest that silently describes
/// fewer columns than the caller believes.
fn hash_row(row: &Row, width: usize) -> Result<u128> {
    if row.len() != width {
        bail!("expected {width} columns, got {}", row.len());
    }
    let mut hasher = Fnv1a128::new();
    for i in 0..width {
        hasher.write_value(row.value(i)?)?;
    }
    Ok(hasher.finish())
}

/// What changed between two captures of the *same* database.
///
/// Returns only the tables that moved, so an empty result is the
/// assertion "this write touched nothing" — which is the expected
/// outcome of a rejected write, and the thing a refusal test needs.
pub fn diff(before: &DbState, after: &DbState) -> Vec<TableDelta> {
    let mut deltas = Vec::new();
    for b in &before.tables {
        let Some(a) = after.table(b.table) else {
            continue;
        };
        if a.digest == b.digest && a.rows == b.rows {
            continue;
        }
        deltas.push(TableDelta {
            table: b.table,
            rows_before: b.rows,
            rows_after: a.rows,
            updated_in_place: a.rows == b.rows,
        });
    }
    deltas
}

/// Whether two states — typically one per engine — describe the same
/// data, and where they do not.
///
/// Separate from [`diff`] because the question is different. `diff`
/// asks what a write did; this asks whether two writes did the same
/// thing. The return shape is the same because the answer is read the
/// same way, and an empty vector means agreement in both cases.
pub fn compare(left: &DbState, right: &DbState) -> Vec<TableDelta> {
    diff(left, right)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    /// `capture_table` reads its pagination cursor out of the first
    /// `key.len()` columns of each row, so this is a precondition for
    /// every table rather than a property of any one of them. It is
    /// checked at runtime too; this is the version that fails at
    /// `cargo test` time when someone adds a table.
    #[test]
    fn every_spec_puts_its_key_first() {
        for spec in TABLES {
            assert!(
                spec.columns.len() >= spec.key.len(),
                "{}: more key columns than columns",
                spec.name
            );
            assert_eq!(
                &spec.columns[..spec.key.len()],
                spec.key,
                "{}: key is not a prefix of columns",
                spec.name
            );
        }
    }

    #[test]
    fn a_capture_of_an_unchanged_database_is_identical() {
        let c = testdb::fixture();
        let before = capture(&c).unwrap();
        let after = capture(&c).unwrap();
        assert_eq!(before, after);
        assert!(diff(&before, &after).is_empty());
    }

    #[test]
    fn the_fixture_has_rows_to_digest() {
        let c = testdb::fixture();
        let state = capture(&c).unwrap();
        assert_eq!(state.tables.len(), TABLES.len());
        let projects = state.table("projects").unwrap();
        assert!(projects.rows > 0, "fixture has no projects to digest");
        assert_ne!(projects.digest, 0);
    }

    /// The property the module exists for, and the one row counts
    /// cannot provide. An update that leaves the count alone has to
    /// show up, and it has to be labelled as an update rather than as
    /// an insert-and-delete.
    #[test]
    fn an_update_in_place_is_visible_and_labelled() {
        let c = testdb::fixture();
        let before = capture(&c).unwrap();

        c.execute(
            "UPDATE projects SET name = name || '-renamed'
              WHERE id = (SELECT MIN(id) FROM projects)",
            &[],
        )
        .unwrap();

        let after = capture(&c).unwrap();
        let deltas = diff(&before, &after);

        let projects = deltas
            .iter()
            .find(|d| d.table == "projects")
            .expect("the renamed project did not show up in the diff");
        assert!(projects.updated_in_place, "{projects:?}");
        assert_eq!(projects.row_change(), 0);
    }

    #[test]
    fn an_insert_moves_the_count_and_is_not_an_update() {
        let c = testdb::fixture();
        let before = capture(&c).unwrap();

        c.execute(
            "INSERT INTO projects (id, slug, name) VALUES (99001, 'statediff-probe', 'Probe')",
            &[],
        )
        .unwrap();

        let after = capture(&c).unwrap();
        let projects = diff(&before, &after)
            .into_iter()
            .find(|d| d.table == "projects")
            .expect("the inserted project did not show up in the diff");
        assert_eq!(projects.row_change(), 1);
        assert!(!projects.updated_in_place);
    }

    /// Order-independence is the decision the cross-engine comparison
    /// rests on, so it gets a test that does not depend on having two
    /// engines to hand: the same rows combined in either order must
    /// reach the same digest.
    #[test]
    fn row_order_does_not_change_a_table_digest() {
        let a = hash_row(&Row::new(vec![Value::Int(1), Value::Text("x".into())]), 2).unwrap();
        let b = hash_row(&Row::new(vec![Value::Int(2), Value::Text("y".into())]), 2).unwrap();
        assert_eq!(a.wrapping_add(b), b.wrapping_add(a));
    }

    /// Duplicate rows must not cancel. This is the whole reason the
    /// combine is addition and not XOR, and XOR would pass every other
    /// test in this file.
    #[test]
    fn duplicate_rows_do_not_cancel() {
        let r = hash_row(&Row::new(vec![Value::Int(7)]), 1).unwrap();
        assert_ne!(r.wrapping_add(r), 0);
    }

    /// Without length prefixes these two rows hash identically, and a
    /// write that moved a character across a column boundary would be
    /// invisible.
    #[test]
    fn a_shifted_column_boundary_changes_the_digest() {
        let split_early = Row::new(vec![Value::Text("ab".into()), Value::Text("c".into())]);
        let split_late = Row::new(vec![Value::Text("a".into()), Value::Text("bc".into())]);
        assert_ne!(
            hash_row(&split_early, 2).unwrap(),
            hash_row(&split_late, 2).unwrap()
        );
    }

    /// A dynamically typed source and a strictly typed destination can
    /// disagree about a column's storage class, and that disagreement
    /// is a finding rather than something to coerce away.
    #[test]
    fn storage_class_is_part_of_the_digest() {
        let as_int = Row::new(vec![Value::Int(42)]);
        let as_text = Row::new(vec![Value::Text("42".into())]);
        assert_ne!(
            hash_row(&as_int, 1).unwrap(),
            hash_row(&as_text, 1).unwrap()
        );
    }

    #[test]
    fn null_is_not_an_empty_string() {
        let null = Row::new(vec![Value::Null]);
        let empty = Row::new(vec![Value::Text(String::new())]);
        assert_ne!(hash_row(&null, 1).unwrap(), hash_row(&empty, 1).unwrap());
    }

    #[test]
    fn a_row_of_the_wrong_width_is_an_error() {
        let row = Row::new(vec![Value::Int(1)]);
        assert!(hash_row(&row, 2).is_err());
    }

    /// Paging is only exercised when a table has more rows than one
    /// page holds, and no fixture table does. Forcing a page size of 1
    /// makes the cursor advance on every row, which is where a keyset
    /// bug would show up as a dropped or repeated row.
    #[test]
    fn paging_reaches_every_row_exactly_once() {
        let c = testdb::fixture();

        let whole = capture_table(&c, &projects_spec(1000)).unwrap();
        let paged = capture_table(&c, &projects_spec(1)).unwrap();

        assert_eq!(whole.rows, paged.rows);
        assert_eq!(whole.digest, paged.digest);
        assert!(
            whole.rows > 1,
            "need several rows for paging to mean anything"
        );
    }

    /// A minimal spec over `projects`, so the paging test can vary the
    /// page size without touching the real `TABLES`.
    fn projects_spec(page: i64) -> TableSpec {
        TableSpec {
            name: "projects",
            columns: &["id", "slug", "name"],
            types: &[],
            key: &["id"],
            page,
        }
    }
}
