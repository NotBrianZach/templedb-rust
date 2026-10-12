//! The two [`Store`](crate::Store) implementations.
//!
//! Neither is behind a cargo feature. A feature-gated Postgres backend
//! would mean the SQLite-only build never type-checks the Postgres one,
//! and the whole reason this split exists is to keep the two from
//! drifting — a configuration in which half the code is invisible to
//! `cargo test` is the drift, pre-arranged.

pub mod pg;
pub mod sqlite;
