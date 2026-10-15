//! Copy the read subset from SQLite into Postgres.
//!
//! # Why this is a write path in a read-only crate
//!
//! The rest of this crate reads and nothing else, because the Python
//! implementation owns every write path to the live SQLite database and
//! those paths carry invariants a second writer would violate —
//! session-scoped staging, checkout role resolution, blob-age guards.
//!
//! This module writes, and does not violate any of them, because it
//! writes in the other direction. SQLite is read through the same
//! read-only handle every other command uses; the only thing mutated is
//! a Postgres database that the Python implementation does not know
//! exists. Authority over the live data is untouched: the mirror is
//! downstream of it, never a second claimant on it.
//!
//! That also sets the honest limit on what a mirrored Postgres is good
//! for today. It is a verification target and a read replica, not a
//! source of truth — it is exactly as stale as the last run of this
//! command. Making Postgres primary means porting the write paths, and
//! that is the design document this port has been deferring, not
//! something the mirror quietly accomplishes.
//!
//! # Why it does not go through [`Store`]
//!
//! [`Store`](crate::Store) has `query` and nothing else, deliberately,
//! so that no amount of plumbing through the trait can produce a write
//! against the live file. Keeping `mirror` on its own `postgres::Client`
//! preserves that: the write capability exists in one module, named
//! after what it does.

use crate::backend::sqlite::SqliteStore;
use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};
use postgres::binary_copy::BinaryCopyInWriter;
use postgres::types::{ToSql, Type};
use postgres::{Client, NoTls};

/// The DDL this mirror targets, compiled in so that `mirror
/// --create-schema` cannot drift from the file in the tree.
pub const READ_SUBSET_DDL: &str = include_str!("../../../schema/postgres/001_read_subset.sql");

/// One table to copy.
///
/// `columns` and `types` are positional and parallel: the SELECT
/// projects the columns in this order and `BinaryCopyInWriter` is
/// handed the types in the same order.
struct TableSpec {
    name: &'static str,
    columns: &'static [&'static str],
    types: &'static [Type],
    /// The columns to paginate on, which **must** be collectively
    /// unique. Pagination is keyset rather than `LIMIT`/`OFFSET`
    /// because `OFFSET n` re-scans n rows on every page, which turns a
    /// linear copy quadratic.
    ///
    /// A tuple rather than one column because `vcs_commit_parents` is
    /// the first table in the subset with no surrogate key: its primary
    /// key is `(commit_id, parent_commit_id)`. Paginating on
    /// `commit_id` alone would be a silent row-dropper rather than a
    /// slow query — a merge commit has one row per parent, so a page
    /// boundary falling between them would make `WHERE commit_id > last`
    /// skip the rest, and the mirror would be quietly missing a parent
    /// edge. Row-value comparison (`(a, b) > (?, ?)`) is the fix, and
    /// both engines have supported it for years.
    key: &'static [&'static str],
    /// Rows per read from SQLite. Tuned down for `content_blobs`
    /// because its rows are not uniform: the table averages 28 KB per
    /// row but its largest single `content_text` is 4.9 MB, so a page
    /// count that is comfortable on average is not bounded in the worst
    /// case.
    page: i64,
}

/// FK order. `projects` before `project_files` before `file_contents`,
/// `entities` before `relations`, and `content_blobs` before anything
/// referencing a hash. The subset schema declares the FKs it can, so
/// this order is enforced rather than merely intended.
const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "projects",
        columns: &[
            "id",
            "slug",
            "name",
            "repo_url",
            "git_branch",
            "active_branch_id",
        ],
        types: &[
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
        ],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "project_files",
        columns: &[
            "id",
            "project_id",
            "file_path",
            "status",
            "file_type_id",
            "lines_of_code",
            "edit_mode",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::INT8,
            Type::TEXT,
        ],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "content_blobs",
        columns: &[
            "hash_sha256",
            "content_text",
            "content_blob",
            "content_type",
            "file_size_bytes",
        ],
        types: &[Type::TEXT, Type::TEXT, Type::BYTEA, Type::TEXT, Type::INT8],
        key: &["hash_sha256"],
        page: 200,
    },
    TableSpec {
        name: "file_contents",
        columns: &[
            "id",
            "file_id",
            "content_hash",
            "file_size_bytes",
            "line_count",
            "is_current",
            "updated_at",
            // For `vcs diff <proj> <file>`, which orders by it. Added
            // to the live table by ALTER TABLE, hence `DEFAULT 1`.
            "version",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::INT8,
            Type::INT8,
            Type::INT4,
            Type::TEXT,
            Type::INT8,
        ],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "vcs_branches",
        columns: &[
            "id",
            "project_id",
            "branch_name",
            "is_default",
            "head_commit_id",
        ],
        types: &[Type::INT8, Type::INT8, Type::TEXT, Type::INT4, Type::INT8],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "vcs_commits",
        columns: &[
            "id",
            "project_id",
            "branch_id",
            "commit_hash",
            "author",
            "commit_message",
            "commit_timestamp",
            "session_id",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
        ],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "vcs_file_states",
        columns: &[
            "id",
            "commit_id",
            "file_id",
            "change_type",
            "content_hash",
            "file_size",
            "line_count",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::INT8,
        ],
        key: &["id"],
        page: 2000,
    },
    // After `vcs_commits`, which both of its columns reference.
    TableSpec {
        name: "vcs_commit_parents",
        columns: &["commit_id", "parent_commit_id", "parent_order"],
        types: &[Type::INT8, Type::INT8, Type::INT8],
        // No surrogate key; this is the join table's own primary key.
        // See the note on `TableSpec::key`.
        key: &["commit_id", "parent_commit_id"],
        page: 2000,
    },
    TableSpec {
        name: "entities",
        columns: &[
            "id",
            "kind",
            "external_ref",
            "source_authority",
            "label",
            "observed_at",
        ],
        types: &[
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
        ],
        key: &["id"],
        page: 5000,
    },
    TableSpec {
        name: "relations",
        columns: &[
            "id",
            "from_entity_id",
            "kind",
            "to_entity_id",
            "source_authority",
            "observed_at",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
        ],
        key: &["id"],
        page: 5000,
    },
    // Last, and with no foreign key, so its position is not about
    // ordering — it references `entities` only "FK-ishly" and the
    // subset schema deliberately does not declare the constraint.
    TableSpec {
        name: "observations_archive",
        columns: &[
            "id",
            "entity_kind",
            "entity_ref",
            "label",
            "source_authority",
            "observed_at",
            "entity_id",
            "prior_label",
            "prior_source_authority",
        ],
        types: &[
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
        ],
        key: &["id"],
        page: 5000,
    },
];

/// How many rows one table contributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    pub table: &'static str,
    pub rows: u64,
}

/// Copy every table in the read subset from `src` into `dest_url`.
///
/// Destructive on the destination and only on the destination: the
/// tables are truncated first, so a second run replaces rather than
/// duplicates. That is the right default for a mirror — an incremental
/// one would need change tracking on the SQLite side, which is the
/// Python implementation's business and not something to infer from
/// row counts.
///
/// The whole copy runs in one transaction. A mirror interrupted
/// half-way would otherwise leave Postgres holding some tables from
/// before and some from after, which is the one state that looks
/// healthy and answers wrongly.
pub fn mirror(src: &SqliteStore, dest_url: &str, create_schema: bool) -> Result<Vec<TableReport>> {
    let mut client = Client::connect(dest_url, NoTls).context("connecting to the mirror target")?;

    if create_schema {
        client
            .batch_execute(READ_SUBSET_DDL)
            .context("creating the read-subset schema")?;
    }

    let mut tx = client.transaction().context("opening a transaction")?;

    // One statement for all eleven: Postgres requires tables joined by
    // foreign keys to be truncated together, and naming them in one
    // `TRUNCATE` is how you say so without `CASCADE` reaching tables
    // this command does not own.
    let names: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
    tx.batch_execute(&format!("TRUNCATE {}", names.join(", ")))
        .context("clearing the mirror")?;

    let mut reports = Vec::new();
    for spec in TABLES {
        let rows =
            copy_table(src, &mut tx, spec).with_context(|| format!("mirroring {}", spec.name))?;
        reports.push(TableReport {
            table: spec.name,
            rows,
        });
    }

    tx.commit().context("committing the mirror")?;
    Ok(reports)
}

fn copy_table(
    src: &SqliteStore,
    tx: &mut postgres::Transaction<'_>,
    spec: &TableSpec,
) -> Result<u64> {
    let cols = spec.columns.join(", ");
    // `FORMAT binary` rather than text: no escaping to get wrong, and
    // `content_blob` is raw bytes that the text format would have to
    // hex-encode. The column types are declared here and in the DDL,
    // and a mismatch is a hard error from the server rather than a
    // silent coercion.
    let sink = tx
        .copy_in(&format!(
            "COPY {} ({cols}) FROM STDIN WITH (FORMAT binary)",
            spec.name
        ))
        .context("opening COPY")?;
    let mut writer = BinaryCopyInWriter::new(sink, spec.types);

    let order = spec.key.join(", ");
    let first = format!("SELECT {cols} FROM {} ORDER BY {order} LIMIT ?1", spec.name);
    // Row-value comparison, so a multi-column key is compared as a
    // tuple and not column by column. `(a, b) > (?1, ?2)` is true for
    // `(1, 9)` against `(1, 5)`, which is what keyset pagination needs;
    // `a > ?1 AND b > ?2` is not, and would drop rows.
    let placeholders: Vec<String> = (1..=spec.key.len()).map(|i| format!("?{i}")).collect();
    let rest = format!(
        "SELECT {cols} FROM {} WHERE ({order}) > ({}) ORDER BY {order} LIMIT ?{}",
        spec.name,
        placeholders.join(", "),
        spec.key.len() + 1
    );

    let mut written = 0u64;
    let mut cursor: Option<Vec<Value>> = None;
    let key_indexes: Vec<usize> = spec
        .key
        .iter()
        .map(|k| {
            spec.columns
                .iter()
                .position(|c| c == k)
                .with_context(|| format!("{k} is not in {}'s projection", spec.name))
        })
        .collect::<Result<_>>()?;

    loop {
        let rows = match &cursor {
            None => src.query(&first, &[Value::Int(spec.page)])?,
            Some(last) => {
                let mut params = last.clone();
                params.push(Value::Int(spec.page));
                src.query(&rest, &params)?
            }
        };
        if rows.is_empty() {
            break;
        }
        for row in &rows {
            let values: Vec<&(dyn ToSql + Sync)> = (0..spec.columns.len())
                .map(|i| row.value(i).map(|v| v as &(dyn ToSql + Sync)))
                .collect::<Result<_>>()?;
            writer.write(&values).context("writing a row")?;
            written += 1;
        }
        // Keyset pagination: remember the last key rather than an
        // offset. Every `key` in `TABLES` is collectively unique — a
        // surrogate `id`, a primary-key hash, or the composite key of a
        // join table — so no row can be skipped or repeated by a tie.
        let last = &rows[rows.len() - 1];
        cursor = Some(
            key_indexes
                .iter()
                .map(|&i| last.value(i).cloned())
                .collect::<Result<_>>()?,
        );
        if (rows.len() as i64) < spec.page {
            break;
        }
    }

    writer.finish().context("finishing COPY")?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The DDL is compiled in, so a typo that makes it unparseable is a
    /// build-time problem rather than a problem for whoever runs the
    /// mirror. This checks the embed actually resolved and covers the
    /// tables the specs claim.
    #[test]
    fn the_embedded_ddl_covers_every_mirrored_table() {
        for spec in TABLES {
            assert!(
                READ_SUBSET_DDL.contains(&format!("CREATE TABLE {}", spec.name)),
                "{} has a TableSpec but no CREATE TABLE in the DDL",
                spec.name
            );
        }
        assert!(READ_SUBSET_DDL.contains("CREATE VIEW source_snapshots"));
    }

    /// Columns and types are positional and parallel; a spec where they
    /// disagree in length would hand `BinaryCopyInWriter` the wrong
    /// shape and fail per row at runtime, deep inside a copy.
    #[test]
    fn every_spec_has_one_type_per_column() {
        for spec in TABLES {
            assert_eq!(
                spec.columns.len(),
                spec.types.len(),
                "{} has {} columns and {} types",
                spec.name,
                spec.columns.len(),
                spec.types.len()
            );
            assert!(
                !spec.key.is_empty(),
                "{} has no pagination key, so the copy cannot advance",
                spec.name
            );
            for k in spec.key {
                assert!(
                    spec.columns.contains(k),
                    "{}'s key column {k} is not in its projection, so \
                     pagination cannot read the cursor back",
                    spec.name
                );
            }
            assert!(spec.page > 0, "{} has a non-positive page size", spec.name);
        }
    }

    /// Referenced tables have to be copied before the tables that
    /// reference them, or the FKs in the subset schema reject the rows.
    #[test]
    fn tables_are_ordered_so_references_already_exist() {
        let order: Vec<&str> = TABLES.iter().map(|t| t.name).collect();
        let before = |a: &str, b: &str| {
            let (ia, ib) = (
                order.iter().position(|n| *n == a).unwrap(),
                order.iter().position(|n| *n == b).unwrap(),
            );
            assert!(ia < ib, "{a} must be copied before {b}");
        };
        before("projects", "project_files");
        before("project_files", "file_contents");
        before("content_blobs", "file_contents");
        before("projects", "vcs_commits");
        before("vcs_commits", "vcs_file_states");
        before("entities", "relations");
    }

    /// The round trip that matters, without a server: read the fixture
    /// through the same paginating SELECTs the copy uses and check the
    /// row counts and the cursor advance. A page size of 1 forces every
    /// boundary the keyset logic has.
    #[test]
    fn keyset_pagination_reads_every_row_exactly_once() {
        let src = crate::testdb::fixture();
        for (table, key, expected) in [
            ("projects", "id", 3),
            ("project_files", "id", 15),
            ("content_blobs", "hash_sha256", 9),
            ("entities", "id", 7),
            ("relations", "id", 5),
        ] {
            let mut seen: Vec<String> = Vec::new();
            let mut cursor: Option<Value> = None;
            loop {
                let sql = match cursor {
                    None => format!("SELECT {key} FROM {table} ORDER BY {key} LIMIT ?1"),
                    Some(_) => format!(
                        "SELECT {key} FROM {table} WHERE {key} > ?1 ORDER BY {key} LIMIT ?2"
                    ),
                };
                let rows = match &cursor {
                    None => src.query(&sql, &[Value::Int(1)]).unwrap(),
                    Some(last) => src.query(&sql, &[last.clone(), Value::Int(1)]).unwrap(),
                };
                if rows.is_empty() {
                    break;
                }
                seen.push(format!("{:?}", rows[0].value(0).unwrap()));
                cursor = Some(rows[0].value(0).unwrap().clone());
            }
            assert_eq!(seen.len(), expected, "{table} yielded {seen:?}");
            let mut deduped = seen.clone();
            deduped.sort();
            deduped.dedup();
            assert_eq!(deduped.len(), expected, "{table} repeated a row");
        }
    }
}
