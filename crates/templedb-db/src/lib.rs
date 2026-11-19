//! Database access for the TempleDB port, over SQLite or Postgres.
//!
//! Scope note: this layer is deliberately READ-ONLY. The Python
//! implementation owns every write path to the live SQLite database, and
//! those write paths are where the hard-won invariants live —
//! session-scoped staging, checkout role resolution, blob-age guards. A
//! reader cannot violate any of them, so porting reads first lets the
//! two implementations coexist against one live database instead of
//! racing for authority over it.
//!
//! # The backend split
//!
//! There is one [`Store`] trait with one interesting method, and two
//! implementations of it. Query modules write SQLite-flavoured SQL with
//! `?N` placeholders; [`Dialect::bind`] translates the placeholders for
//! Postgres, and the two or three queries that genuinely cannot be
//! written once `match` on [`Dialect`] in the module that owns them.
//!
//! The alternative — a trait method per command, implemented twice —
//! was rejected because it would mean two copies of every query when
//! fewer than a tenth of them differ, and two copies drift. See
//! `store.rs` for the full argument.
//!
//! What this buys, concretely: the cross-backend diff becomes a test of
//! the port rather than of the translation. Where the Python comparison
//! asks "does this answer the same as the implementation we are
//! replacing", the backend comparison asks "does this answer the same
//! regardless of where the bytes live", and a query that passes both is
//! pinned from two directions.
//!
//! The command that forced the sharpest version of this was
//! `search content`. Built on FTS5's `MATCH`, `snippet()` and bm25
//! `rank`, it had no Postgres translation that returned the same rows —
//! measured, the two indexes disagree on roughly a quarter of the hit
//! set, because FTS5 stems with Porter and Postgres with Porter2. So
//! matching, ranking and snippet rendering all moved into Rust
//! ([`textsearch`]), with SQL left to narrow candidates using a `LIKE`
//! both engines read identically. The command now means one thing; the
//! cost is that its semantics are substring rather than stemmed, which
//! is a deliberate break from the Python implementation. See
//! `textsearch.rs`.
//!
//! # Two properties inherited from specific Python bugs
//!
//! 1. `journal_size_limit` is set on the SQLite backend. It was unset
//!    there (`-1`, unlimited) and nothing in the tree configured it, so
//!    a WAL that spiked once stayed spiked: 5.9 GB against a 776 MB
//!    database on 2026-10-04.
//!
//! 2. Statements cannot be left open across caller code. The Python
//!    side pinned WAL checkpoints twice by leaving an un-exhausted
//!    cursor holding a read transaction. In the original version of
//!    this crate that was a property of `rusqlite::Statement`'s `Drop`;
//!    it is now a property of [`Store::query`], which materialises
//!    every row and hands back owned values, so there is no cursor for
//!    a caller to hold.
//!
//! # Module layout
//!
//! One module per Python command family, so the SQL for a given noun
//! lives in exactly one place and the CLI cannot reach around it. Names
//! are re-exported flat: callers write `templedb_db::list_projects`, not
//! `templedb_db::projects::list_projects`.

mod backend;
mod checkouts;
mod classify;
mod clock;
mod commit;
mod conn;
mod dialect;
mod difflib;
mod doctor;
mod entities;
mod exit;
mod filemark;
mod filerm;
mod files;
mod fileset;
mod fuzzy;
mod intent;
mod like;
mod mirror;
mod pgtext;
mod projects;
mod retype;
mod search;
mod session;
mod source;
mod statediff;
mod store;
mod summary;
mod textsearch;
mod value;
mod vcs;
mod writer;

pub use backend::pg::PgStore;
pub use backend::sqlite::SqliteStore;
pub use checkouts::{
    active as active_checkouts, checkout_root, current_session as current_checkout_session,
    live_sessions, repo_url, Checkout, Purpose,
};
pub use classify::{
    classify_path, is_ignored_path, pattern_count, skip_pattern_count, GENERIC_TEXT_TYPE,
};
pub use clock::{julian_day, julian_day_ms, timestamp_of_ms, Age, Clock, Freshness, NOW_ENV};
pub use commit::{
    advance_branch_head, branch_head, commit_hash, commit_staged, CommitOptions, CommitOutcome,
};
pub use conn::{db_path, open_ro, target, Target, URL_ENV};
pub use dialect::Dialect;
pub use difflib::{context_diff, splitlines, unified_diff, Match, Opcode, SequenceMatcher, Tag};
pub use doctor::{check_history, render as render_check_run, CheckRun, HistoryFilter, NO_HISTORY};
pub use entities::{
    dead_imports, entity_stats, explore, observations, search_entities, shortest_path, trace,
    DeadImport, Direction, Edge, EntityMatch, Explored, GraphStats, Observation, PathResult,
    TraceHit, Traced,
};
pub use exit::{code_of, Coded, NOT_FOUND, NO_PATH, STALE_BASE, USAGE};
pub use filemark::{mark_file, mark_line, EditMode, MarkOutcome};
pub use filerm::{remove_file, rm_line, RmOutcome};
pub use files::{current_hash, list_files, read_file, EditModeFilter, FileEntry};
pub use fileset::{
    check_mojibake, content_hash, line_count, mojibake_runs, resolve_project_exact, set_file,
    verify_current, Content, SetOptions, SetOutcome, Staged,
};
pub use fuzzy::{match_file, match_project, simple_score, FileLookup, FileRef, ProjectLookup};
pub use intent::{
    applied_lines, apply_intent, base_is_stale, cancel_intent, cancelled_line, create_intent,
    created_lines, list_intents, marker as intent_marker, patch_summary, render as render_intent,
    render_full as render_intent_full, show_intent, Applied, ApplyRefusal, CancelRefusal,
    CreateOptions, Created, Intent, IntentFilter, IntentSummary, NO_INTENTS,
};
pub use mirror::{
    check_freshness, mirror, Drift, FreshnessReport, TableReport, MIRRORED_TABLES, READ_SUBSET_DDL,
};
pub use projects::{
    list_projects, list_projects_filtered, project_detail, Project, ProjectDetail, ProjectFilter,
};
pub use retype::{
    propose as propose_retype, render as render_retype, Change, Debris, RetypeFilter,
    RetypeProposal, DRY_RUN_NOTICE,
};
pub use search::{search_content, search_files, FileNameHit, SearchHit};
pub use session::{
    ignored_name, resolve as resolve_session, resolve_tx as resolve_session_tx, SessionId,
    SESSION_ID_ENV, SESSION_NAME_ENV,
};
pub use source::{revisions, snapshot, snapshot_body, Revision, Snapshot, SnapshotBody};
pub use statediff::{capture, compare, diff, DbState, TableDelta, TableState};
pub use store::Store;
pub use summary::{
    fleet_reconciles, graph_facts, handoff_inbox, hygiene, ingest_adapters, invariant_results,
    live_reconciliations, observe, reconciliation_counts, AdapterRuns, GraphFacts, HandoffInbox,
    HygieneRow, InvariantResult, MachineReconcile, Reconciliation, ReconciliationCounts,
    SummaryFacts,
};
pub use value::{Row, Value};
pub use vcs::{
    active_branch, branches, changed_files, commit_detail, commit_log, content_at_commit,
    content_by_hash, content_versions, current_content, last_committed_content, parent_hash,
    pending_in_session, staged_files, version_text, BlobKind, BranchEntry, CommitContent,
    CommitDetail, CommitEntry, ContentVersion, FileChange, Visibility, WorkingFile,
};
pub use writer::{PgWriter, OPT_IN_ENV, WRITE_URL_ENV};

// Nothing from `rusqlite` or `postgres` is re-exported. The previous
// revision of this file re-exported `rusqlite::Connection` "so the CLI
// crate never names rusqlite itself", with a comment noting that a CLI
// which *could* name the driver would eventually reach around the SQL
// layer. Supporting a second backend settled it: there is no longer a
// single driver type to export, and the CLI now holds a `dyn Store`,
// which it cannot run SQL through without coming here first.

#[cfg(test)]
pub(crate) mod testdb;
