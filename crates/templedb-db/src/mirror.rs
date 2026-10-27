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
pub(crate) struct TableSpec {
    pub(crate) name: &'static str,
    pub(crate) columns: &'static [&'static str],
    pub(crate) types: &'static [Type],
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
    pub(crate) key: &'static [&'static str],
    /// Rows per read from SQLite. Tuned down for `content_blobs`
    /// because its rows are not uniform: the table averages 28 KB per
    /// row but its largest single `content_text` is 4.9 MB, so a page
    /// count that is comfortable on average is not bounded in the worst
    /// case.
    pub(crate) page: i64,
}

/// FK order. `projects` before `project_files` before `file_contents`,
/// `entities` before `relations`, and `content_blobs` before anything
/// referencing a hash. The subset schema declares the FKs it can, so
/// this order is enforced rather than merely intended.
pub(crate) const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "projects",
        columns: &[
            "id",
            "slug",
            "name",
            "repo_url",
            "git_branch",
            "active_branch_id",
            // `project list --nix-only` and `--category`.
            "is_nix_project",
            "project_category",
        ],
        types: &[
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::INT4,
            Type::TEXT,
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
    // Before `vcs_commits` by convention rather than by constraint:
    // `session_id` declares no foreign key here, because `vcs session
    // prune` can delete a session whose commits still name it. Ordered
    // this way so the file reads in dependency order even where the
    // dependency is not enforced.
    TableSpec {
        name: "vcs_sessions",
        // Two columns of ten. `session.rs` asks exactly one question of
        // this table — is the id the caller named still live — so `id`
        // and `ended_at` are what it carries.
        columns: &["id", "ended_at"],
        types: &[Type::INT8, Type::TEXT],
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
            // Migration 118's denormalised path. Nothing reads it in
            // either implementation — `vcs show` resolves paths by
            // joining `project_files` on both sides — and it is carried
            // because `vcs commit-staged` hard-deletes that row for a
            // committed deletion, which makes this the only surviving
            // record of which file the state was. 7,183 of 7,197 live
            // rows have one; the 14 that do not predate the column.
            "file_path",
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
    // After `vcs_commits`, which it references with a UNIQUE FK.
    //
    // **Three columns of forty**, and that is not the usual
    // follow-the-readers trim — it is the whole information content of
    // the table. Measured 2026-10-24 over all 436 live rows: `intent`,
    // `change_type`, `scope`, `tags`, `ai_tool`, `review_status` and
    // `confidence_level` are non-NULL on ZERO rows, and `is_breaking`
    // and `ai_assisted` are 0 on every one. They are reachable only
    // through `commit --interactive` or an explicit flag, and nobody
    // has ever passed one.
    //
    // The one column that is populated is **derived**:
    // `_collect_commit_metadata` sets `impact_level` to `'high'` when
    // `added + modified + deleted > 50`, `'medium'` above 20, else
    // `'low'`. So the table is a bucketed file count, once per commit —
    // 410 low, 18 medium, 8 high.
    //
    // Carried anyway rather than skipped, because the cross-backend
    // digest should cover what the live database holds; what is *not*
    // carried is 37 columns of unanimous NULL. This is the same shape as
    // `vcs log`'s `files_changed` — a count computed and discarded —
    // except here it is computed, bucketed, stored, and still read by
    // nothing.
    TableSpec {
        name: "vcs_commit_metadata",
        columns: &["id", "commit_id", "impact_level", "created_at"],
        types: &[Type::INT8, Type::INT8, Type::TEXT, Type::TEXT],
        key: &["id"],
        page: 2000,
    },
    // After `projects`, which it references. The one table here that
    // exists for a writer rather than a reader, so its projection is
    // the columns `record_intent` supplies rather than the columns a
    // query names — twelve of fourteen, with `cancelled_at` and
    // `applied_commit_id` left to `intent cancel` and `vcs commit`.
    TableSpec {
        name: "edit_intents",
        columns: &[
            "id",
            "session_id",
            "project_id",
            "file_path",
            "base_revision",
            "new_content_hash",
            "patch_summary",
            "author",
            "description",
            "status",
            "created_at",
            "applied_at",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
        ],
        key: &["id"],
        page: 2000,
    },
    // Last of the VCS group, and after `edit_intents` on purpose:
    // `intent_id` points at it (migration 088). The FK is not declared
    // in the subset — `vcs_working_state.intent_id` is NO ACTION live
    // and nothing nulls it — but the load order follows the reference
    // anyway, so the file reads in dependency order even where the
    // dependency is not enforced.
    //
    // Staging, and the table `vcs commit` consumes. Nothing in the port
    // reads it yet; it arrives with `file set --stage`, whose only
    // consumer is the commit path. So this is the second table carried
    // for a writer rather than a reader, after `edit_intents`. See the
    // TempleDB report *vcs commit: the widest door, measured*.
    //
    // `content_text` and `content_blob` are NOT carried. The staged
    // *hash* is what `vcs commit` reads — "content_hash in working_state
    // is the hash of the file on disk (set by detect_changes /
    // refresh). Use it for the commit" — and those two columns are the
    // workspace's copy of bytes `content_blobs` already holds under
    // that hash. Mirroring them would store the staged content twice
    // for no reader, on top of the `text_lower` copy
    // `content_blobs` already pays for.
    TableSpec {
        name: "vcs_working_state",
        columns: &[
            "id",
            "project_id",
            "branch_id",
            "file_id",
            "content_hash",
            "state",
            "last_modified",
            "staged_by_session_id",
            "intent_id",
        ],
        types: &[
            Type::INT8,
            Type::INT8,
            Type::INT8,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::INT8,
        ],
        key: &["id"],
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
    // The six `summary` reads. None of them references another table in
    // the subset — `fleet_machines.network_id` points at
    // `fleet_networks`, which has no reader and is not mirrored — so
    // they can sit at the end in any order.
    TableSpec {
        name: "ingestion_runs",
        columns: &["id", "adapter", "started_at", "status"],
        types: &[Type::INT8, Type::TEXT, Type::TEXT, Type::TEXT],
        key: &["id"],
        page: 5000,
    },
    TableSpec {
        name: "invariant_checks",
        columns: &["id", "check_name", "ran_at", "status", "issue_count"],
        types: &[Type::INT8, Type::TEXT, Type::TEXT, Type::TEXT, Type::INT8],
        key: &["id"],
        page: 5000,
    },
    TableSpec {
        name: "reconcile_runs",
        columns: &["id", "machine_name", "ran_at", "status"],
        types: &[Type::INT8, Type::TEXT, Type::TEXT, Type::TEXT],
        key: &["id"],
        page: 5000,
    },
    TableSpec {
        name: "fleet_machines",
        columns: &["id", "machine_name", "last_deployed_at"],
        types: &[Type::INT8, Type::TEXT, Type::TEXT],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "handoff_notes",
        columns: &["id", "to_session", "to_topic", "acked_at"],
        types: &[Type::INT8, Type::TEXT, Type::TEXT, Type::TEXT],
        key: &["id"],
        page: 2000,
    },
    TableSpec {
        name: "reconciliations",
        columns: &[
            "id",
            "kind",
            "subject_ref",
            "status",
            "item_count",
            "owner",
            "first_detected_at",
            "last_observed_at",
            "review_after",
        ],
        types: &[
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::INT8,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
            Type::TEXT,
        ],
        key: &["id"],
        page: 2000,
    },
];

/// How many rows one table contributed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableReport {
    pub table: &'static str,
    pub rows: u64,
}

/// Where the mirror records what it copied, so a later run can be asked
/// whether it is still current.
///
/// Not part of `001_read_subset.sql`: that file is the *subset of
/// TempleDB's schema* this port reads, and nothing in TempleDB has a
/// `mirror_provenance`. It is bookkeeping this command owns, created on
/// demand so an existing mirror gains it on its next run rather than
/// needing the destructive `--create-schema`.
const PROVENANCE_DDL: &str = "
CREATE TABLE IF NOT EXISTS mirror_provenance (
    table_name TEXT PRIMARY KEY,
    row_count  BIGINT NOT NULL,
    copied_at  TIMESTAMPTZ NOT NULL DEFAULT now()
)";

/// How many tables the mirror covers, for callers that want to say so
/// without reaching into [`TABLES`].
pub const MIRRORED_TABLES: usize = TABLES.len();

/// One table whose row count no longer matches the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    pub table: &'static str,
    /// What `mirror` recorded when it last copied this table.
    pub mirrored: i64,
    /// What the live SQLite holds now.
    pub source: i64,
}

/// The answer to "is this mirror still a faithful copy?"
#[derive(Debug, Clone)]
pub struct FreshnessReport {
    /// `None` when the mirror predates provenance tracking, which is
    /// not the same as fresh and must not be reported as such.
    pub copied_at: Option<String>,
    pub drift: Vec<Drift>,
}

impl FreshnessReport {
    pub fn is_fresh(&self) -> bool {
        self.copied_at.is_some() && self.drift.is_empty()
    }
}

/// Compare recorded counts against live ones.
///
/// Split out from [`check_freshness`] so it can be tested without a
/// Postgres server, which is the same reason the placeholder rewriting
/// and `Value` getters are separable.
///
/// **What this does and does not catch.** Row counts catch inserts and
/// deletes, which is what actually drifts here — a `publish` adds
/// commits, file states and entities, and that is the case observed on
/// 2026-10-16 when the cross-backend sweep went from 40/40 identical to
/// six differences purely because the mirror had not been re-run. It
/// does *not* catch an update in place: a row whose `is_current` flipped
/// leaves the count alone. A per-table checksum would, at the cost of
/// reading every row on both sides on every check, which is the copy
/// this is trying to avoid. Counts are the cheap 90%, and claiming more
/// than that would be worse than claiming less.
fn compare_counts(recorded: &[(String, i64)], live: &[(&'static str, i64)]) -> Vec<Drift> {
    let mut drift = Vec::new();
    for (table, source) in live {
        // A table absent from provenance is reported as drift rather
        // than skipped: the mirror cannot vouch for it, and silence
        // would read as agreement.
        let mirrored = recorded
            .iter()
            .find(|(name, _)| name == table)
            .map(|(_, n)| *n)
            .unwrap_or(-1);
        if mirrored != *source {
            drift.push(Drift {
                table,
                mirrored,
                source: *source,
            });
        }
    }
    drift
}

/// Ask whether `dest_url` is still a faithful copy of `src`.
///
/// Reads 11 counts from each side and compares them. Does not write, to
/// either side — so it is safe to run against a mirror the port is
/// connected to read-only, and cheap enough to run before every sweep.
pub fn check_freshness(src: &SqliteStore, dest_url: &str) -> Result<FreshnessReport> {
    let mut client = Client::connect(dest_url, NoTls).context("connecting to the mirror target")?;

    let exists: bool = client
        .query_one(
            "SELECT EXISTS (SELECT 1 FROM information_schema.tables
                             WHERE table_name = 'mirror_provenance')",
            &[],
        )
        .context("looking for mirror_provenance")?
        .get(0);

    if !exists {
        return Ok(FreshnessReport {
            copied_at: None,
            drift: Vec::new(),
        });
    }

    let mut recorded = Vec::new();
    let mut copied_at = None;
    for row in client
        .query(
            "SELECT table_name, row_count, copied_at::text FROM mirror_provenance",
            &[],
        )
        .context("reading mirror_provenance")?
    {
        recorded.push((row.get::<_, String>(0), row.get::<_, i64>(1)));
        let at: String = row.get(2);
        // The oldest copy time, because a mirror is only as fresh as
        // its stalest table.
        if copied_at.as_ref().is_none_or(|c: &String| at < *c) {
            copied_at = Some(at);
        }
    }

    let mut live = Vec::new();
    for spec in TABLES {
        let rows = src.query(&format!("SELECT COUNT(*) FROM {}", spec.name), &[])?;
        let n = rows
            .first()
            .and_then(|r| r.value(0).ok())
            .and_then(|v| match v {
                Value::Int(i) => Some(*i),
                _ => None,
            })
            .unwrap_or(0);
        live.push((spec.name, n));
    }

    Ok(FreshnessReport {
        copied_at,
        drift: compare_counts(&recorded, &live),
    })
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

    // Provenance goes in the same transaction as the copy. If it were a
    // separate one, an interrupted run could leave counts claiming a
    // copy that rolled back — a mirror that reports itself fresh while
    // being empty is worse than one that cannot answer at all.
    tx.batch_execute(PROVENANCE_DDL)
        .context("creating mirror_provenance")?;
    tx.execute("DELETE FROM mirror_provenance", &[])
        .context("clearing mirror_provenance")?;
    for report in &reports {
        tx.execute(
            "INSERT INTO mirror_provenance (table_name, row_count) VALUES ($1, $2)",
            &[&report.table, &(report.rows as i64)],
        )
        .with_context(|| format!("recording provenance for {}", report.table))?;
    }

    // Advance the identity sequences to match the ids just copied.
    //
    // `COPY` with explicit ids does not touch the sequence behind an
    // identity column, so a freshly mirrored database would hand out
    // id 1 on the first insert and collide on the primary key. The
    // failure is loud, which is the only good thing about it.
    //
    // Inside the transaction, unlike `ANALYZE`: a mirror whose
    // sequences are behind is not merely unoptimised, it is one that
    // the first write breaks. That belongs to the copy's atomicity.
    sync_sequences(&mut tx).context("advancing the identity sequences")?;

    tx.commit().context("committing the mirror")?;

    // `ANALYZE` after the copy, and deliberately *after* the commit.
    //
    // `COPY` does not update the planner's statistics, so until
    // autovacuum gets round to it a freshly mirrored database plans from
    // defaults — which for the 104 MB `content_blobs` means guessing at
    // the size of the biggest table in the subset. It did not turn out
    // to matter for the query that prompted the investigation (the plan
    // was identical before and after), which is why this is a one-line
    // repair rather than the subject of a measurement; what it buys is
    // that the *next* query to go wrong is not wrong for this reason.
    //
    // Outside the transaction because statistics are not part of the
    // copy's atomicity: a mirror that committed is correct whether or
    // not it has been analysed, and holding the copy's locks through an
    // `ANALYZE` of 210,000 rows would extend the one window in which
    // the mirror is unreadable. Which is also why a failure here is not
    // propagated — see below.
    //
    // A failed `ANALYZE` must not fail `mirror`. The data is committed
    // and correct at that point; returning `Err` would report a
    // successful copy as a failed one and send the caller to re-run a
    // `TRUNCATE`. It is reported on stderr instead, which is where this
    // crate's diagnostics go.
    if let Err(e) = client.batch_execute("ANALYZE") {
        eprintln!(
            "mirror copied {} tables but ANALYZE failed: {e}",
            reports.len()
        );
        eprintln!(
            "the mirror is usable; the planner will work from defaults until autovacuum runs"
        );
    }

    Ok(reports)
}

/// The tables whose `id` is an identity column, and whose sequence
/// therefore has to be advanced past the ids `COPY` just inserted.
///
/// Kept beside [`TABLES`] rather than derived from it because being an
/// identity column is a property of the DDL, not of the copy, and a
/// table gains one only when a write path needs it. Six today; the
/// list grows with the writer — `vcs_commits` and `vcs_file_states`
/// joined it with `vcs commit-staged` and `vcs_working_state` with
/// `file set`'s auto-stage, and missing one here is how a freshly
/// mirrored database hands out an id that already exists.
const IDENTITY_TABLES: &[&str] = &[
    "project_files",
    "file_contents",
    "edit_intents",
    "vcs_commits",
    "vcs_file_states",
    "vcs_working_state",
];

/// `setval` each identity sequence to the table's current `MAX(id)`.
///
/// `COALESCE(MAX(id), 0) + 1` with `false` as the third argument rather
/// than `setval(seq, MAX(id))`: an empty table has no maximum, and
/// `setval(..., 0)` is an error because sequences start at 1. Passing
/// `is_called = false` means "the next value handed out is exactly
/// this", so the arithmetic is the same for an empty table and a full
/// one and there is no branch.
fn sync_sequences(tx: &mut postgres::Transaction<'_>) -> Result<()> {
    for table in IDENTITY_TABLES {
        // `pg_get_serial_sequence` rather than guessing
        // `<table>_id_seq`: the name is an implementation detail and
        // asking is both correct and self-documenting.
        tx.execute(
            &format!(
                "SELECT setval(
                     pg_get_serial_sequence('{table}', 'id'),
                     (SELECT COALESCE(MAX(id), 0) + 1 FROM {table}),
                     false
                 )"
            ),
            &[],
        )
        .with_context(|| format!("advancing {table}'s id sequence"))?;
    }
    Ok(())
}

/// This row's values with every `Value::Text` escaped for Postgres.
///
/// Returns owned values because `escape` may allocate, and the
/// `BinaryCopyInWriter` borrows what it is handed — so the escaped
/// strings have to outlive the `write` call.
///
/// Only `Value::Text` is transformed. `Value::Blob` goes to a `BYTEA`
/// column where a NUL is an ordinary byte, and `content_blob` carries
/// plenty of them; escaping those would corrupt every binary file in
/// the mirror.
fn escape_text_columns(row: &crate::value::Row, spec: &TableSpec) -> Result<Vec<Value>> {
    (0..spec.columns.len())
        .map(|i| {
            Ok(match row.value(i)? {
                Value::Text(s) => Value::Text(crate::pgtext::escape(s).into_owned()),
                other => other.clone(),
            })
        })
        .collect()
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
            // Text is escaped on the way in, and `backend/pg.rs`
            // unescapes on the way out, so a U+0000 survives a round
            // trip through a column that cannot store one. Postgres
            // `text` rejects it outright — the server's own diagnosis is
            // `invalid byte sequence for encoding "UTF8": 0x00` with no
            // table, no column and no row, arriving from `finish()`
            // after 104 MB has already streamed — and one real file in
            // the live database contains one. See `pgtext.rs`.
            //
            // Every text column, not only `content_text`: the rule is
            // "this crate's Postgres backend stores escaped text", and
            // a rule with an exception list is one a future column gets
            // added to incorrectly. `escape` is a no-op and
            // allocation-free for text containing neither U+0000 nor
            // U+0001, which is all but three rows of the subset.
            let escaped = escape_text_columns(row, spec)?;
            let values: Vec<&(dyn ToSql + Sync)> =
                escaped.iter().map(|v| v as &(dyn ToSql + Sync)).collect();
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

    fn rec(pairs: &[(&str, i64)]) -> Vec<(String, i64)> {
        pairs.iter().map(|(t, n)| (t.to_string(), *n)).collect()
    }

    /// The `content_blobs` spec, which is the one that hits this in
    /// practice: a TEXT column, a TEXT key, and a BYTEA column that
    /// must *not* be flagged.
    fn blobs_spec() -> &'static TableSpec {
        TABLES
            .iter()
            .find(|t| t.name == "content_blobs")
            .expect("content_blobs is in the subset")
    }

    /// The row that forced `pgtext` to exist: a NUL in `content_text`
    /// leaves this function with no NUL in any text column, so
    /// `COPY` can accept it.
    #[test]
    fn a_nul_in_a_text_column_is_escaped_before_the_copy() {
        let row = crate::value::Row::new(vec![
            Value::Text("ff4938cc03ca2b40".into()),
            Value::Text("join('\0')".into()),
            Value::Null,
            Value::Text("text".into()),
            Value::Int(13410),
        ]);
        let escaped = escape_text_columns(&row, blobs_spec()).unwrap();
        let Value::Text(text) = &escaped[1] else {
            panic!("content_text should still be text: {escaped:?}");
        };
        assert!(!text.contains('\0'), "{text:?} would be rejected");
        assert_eq!(text, "join('\u{1}0')");
        // And it is the inverse of what the read path does, which is
        // the property that makes the round trip faithful.
        assert_eq!(crate::pgtext::unescape(text), "join('\0')");
    }

    /// A NUL in the BYTEA column is an ordinary byte and must survive
    /// untouched. `content_blob` holds binary files and nearly all of
    /// them contain one; escaping those would corrupt every binary in
    /// the mirror.
    #[test]
    fn a_nul_in_a_blob_column_is_left_alone() {
        let row = crate::value::Row::new(vec![
            Value::Text("abc".into()),
            Value::Null,
            Value::Blob(vec![0x7f, 0x45, 0x00, 0x00]),
            Value::Text("application/octet-stream".into()),
            Value::Int(4),
        ]);
        let escaped = escape_text_columns(&row, blobs_spec()).unwrap();
        assert_eq!(escaped[2], Value::Blob(vec![0x7f, 0x45, 0x00, 0x00]));
        // NULL stays NULL rather than becoming an empty string.
        assert_eq!(escaped[1], Value::Null);
    }

    /// The common case: every value identical, which is all but three
    /// rows of the live subset.
    #[test]
    fn ordinary_rows_pass_through_unchanged() {
        let row = crate::value::Row::new(vec![
            Value::Text("abc".into()),
            Value::Text("hello\nworld".into()),
            Value::Null,
            Value::Text("text".into()),
            Value::Int(11),
        ]);
        let escaped = escape_text_columns(&row, blobs_spec()).unwrap();
        for (i, v) in escaped.iter().enumerate() {
            assert_eq!(v, row.value(i).unwrap(), "column {i} changed");
        }
    }

    /// Every text column is escaped, not just `content_text`. The rule
    /// has no exception list, because a rule with one is a rule a
    /// future column gets left out of.
    #[test]
    fn every_text_column_is_escaped_not_just_the_known_one() {
        let spec = blobs_spec();
        let row = crate::value::Row::new(vec![
            // The *key* column carrying a NUL, which is the case an
            // implementation that only escaped `content_text` would
            // miss.
            Value::Text("key\0hash".into()),
            Value::Text("body".into()),
            Value::Null,
            Value::Text("ty\0pe".into()),
            Value::Int(1),
        ]);
        let escaped = escape_text_columns(&row, spec).unwrap();
        for (i, v) in escaped.iter().enumerate() {
            if let Value::Text(s) = v {
                assert!(
                    !s.contains('\0'),
                    "column {i} ({}) kept its NUL",
                    spec.columns[i]
                );
            }
        }
    }

    #[test]
    fn matching_counts_are_not_drift() {
        let drift = compare_counts(
            &rec(&[("projects", 25), ("entities", 68403)]),
            &[("projects", 25), ("entities", 68403)],
        );
        assert!(drift.is_empty(), "{drift:?}");
    }

    /// The case observed on 2026-10-16: publishing added rows, so the
    /// mirror's counts were low and six sweep rows differed for reasons
    /// that had nothing to do with the port.
    #[test]
    fn a_source_that_grew_is_drift() {
        let drift = compare_counts(
            &rec(&[("entities", 68403), ("relations", 77481)]),
            &[("entities", 68403), ("relations", 77674)],
        );
        assert_eq!(
            drift,
            vec![Drift {
                table: "relations",
                mirrored: 77481,
                source: 77674,
            }]
        );
    }

    /// Deletions drift too. Counting only growth would miss a source
    /// that shrank, which is the shape a row removal takes.
    #[test]
    fn a_source_that_shrank_is_also_drift() {
        let drift = compare_counts(&rec(&[("projects", 25)]), &[("projects", 24)]);
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].source, 24);
    }

    /// A table the mirror never recorded cannot be vouched for, so it
    /// is drift rather than a silent pass. `-1` is the sentinel for
    /// "absent", which no real count can collide with.
    #[test]
    fn a_table_missing_from_provenance_is_drift() {
        let drift = compare_counts(
            &rec(&[("projects", 25)]),
            &[("projects", 25), ("entities", 7)],
        );
        assert_eq!(drift.len(), 1);
        assert_eq!(drift[0].table, "entities");
        assert_eq!(drift[0].mirrored, -1);
    }

    /// Provenance absent entirely — an older mirror — is reported as
    /// not-fresh, never as fresh. The distinction matters because the
    /// sweep refuses on it, and defaulting to fresh would make the
    /// check worthless on exactly the mirrors that need it.
    #[test]
    fn a_mirror_without_provenance_is_not_fresh() {
        let report = FreshnessReport {
            copied_at: None,
            drift: Vec::new(),
        };
        assert!(!report.is_fresh());
    }

    #[test]
    fn a_recorded_mirror_with_no_drift_is_fresh() {
        let report = FreshnessReport {
            copied_at: Some("2026-10-16 10:00:00+00".to_string()),
            drift: Vec::new(),
        };
        assert!(report.is_fresh());
    }

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

    /// Every table whose sequence this advances must actually have one,
    /// and every table that has one must be advanced.
    ///
    /// The two halves fail differently and both are quiet until they are
    /// not. A name here with no `GENERATED … AS IDENTITY` in the DDL
    /// makes `pg_get_serial_sequence` return NULL and `setval(NULL, …)`
    /// return NULL — no error, nothing advanced. A table in the DDL and
    /// missing *here* keeps a sequence that starts at 1, so the first
    /// write after a mirror collides on the primary key.
    #[test]
    fn the_identity_tables_are_exactly_the_ones_the_ddl_declares() {
        for table in IDENTITY_TABLES {
            assert!(
                READ_SUBSET_DDL.contains(&format!("ALTER TABLE {table}\n    ALTER COLUMN id")),
                "{table} is in IDENTITY_TABLES but the DDL gives it no identity, \
                 so sync_sequences silently advances nothing"
            );
        }
        for spec in TABLES {
            let declared = READ_SUBSET_DDL
                .contains(&format!("ALTER TABLE {}\n    ALTER COLUMN id", spec.name));
            assert_eq!(
                declared,
                IDENTITY_TABLES.contains(&spec.name),
                "{} has an identity in the DDL but is not in IDENTITY_TABLES (or \
                 the reverse), so a mirror leaves its sequence at 1",
                spec.name
            );
        }
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
        before("projects", "edit_intents");
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
