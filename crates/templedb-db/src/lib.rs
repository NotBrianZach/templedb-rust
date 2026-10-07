//! SQLite access for the TempleDB port.
//!
//! Scope note: this layer is deliberately READ-ONLY for now. The Python
//! implementation owns every write path, and those write paths are where
//! the hard-won invariants live — session-scoped staging, checkout role
//! resolution, blob-age guards. A reader cannot violate any of them, so
//! porting reads first lets the two implementations coexist against one
//! live database instead of racing for authority over it.
//!
//! Two properties of this crate exist because of specific Python bugs:
//!
//! 1. `journal_size_limit` is set. It was unset there (`-1`, unlimited)
//!    and nothing in the tree configured it, so a WAL that spiked once
//!    stayed spiked: 5.9 GB against a 776 MB database on 2026-10-04. A
//!    new client that opens the same file must not reintroduce that.
//!
//! 2. Statements are finalized by `Drop`. The Python side pinned WAL
//!    checkpoints twice — once via `query_one` leaving an un-exhausted
//!    cursor on a pooled connection, once via raw `conn.cursor()` calls
//!    in the MCP server — because a cursor holding an unfinished
//!    statement holds a read transaction with it. In Rust that failure
//!    mode is not available: `Statement` finalizes when it leaves scope.
//!    Worth stating explicitly, because it is the single clearest thing
//!    this port buys beyond speed.
//!
//! # Module layout
//!
//! One module per Python command family, so the SQL for a given noun
//! lives in exactly one place and the CLI cannot reach around it. Names
//! are re-exported flat: callers write `templedb_db::list_projects`, not
//! `templedb_db::projects::list_projects`. The split happened when
//! `entity trace` and `source snapshot` landed and a single `lib.rs`
//! stopped being one screen of context.

mod conn;
mod entities;
mod files;
mod fuzzy;
mod like;
mod projects;
mod search;
mod source;

pub use conn::{apply_standard_pragmas, db_path, open_ro};
pub use entities::{
    entity_stats, explore, search_entities, shortest_path, trace, Direction, Edge, EntityMatch,
    Explored, PathResult, TraceHit,
};
pub use files::{list_files, read_file, FileEntry};
pub use fuzzy::{match_project, simple_score, ProjectLookup};
pub use projects::{list_projects, Project};
// `Connection` is re-exported so the CLI crate never names `rusqlite`
// itself. The point of the split is that SQL lives here; a CLI that
// depends on the driver can reach around this module, and eventually
// something will.
pub use rusqlite::Connection;
pub use search::{search_content, SearchHit};
pub use source::{revisions, snapshot, snapshot_body, Revision, Snapshot, SnapshotBody};

#[cfg(test)]
pub(crate) mod testdb;
