//! `templedb-rs` — the Rust port's CLI.
//!
//! Named `templedb-rs`, not `templedb`, on purpose: the Python binary is
//! on PATH as `templedb` and is the one every script, systemd unit and
//! agent wrapper invokes. A port that shadows it while incomplete would
//! break callers silently, which is the opposite of what a port is for.
//! Rename it when it can answer everything the original can, not before.
//!
//! Subcommands mirror the Python verb/noun shape (`project list`,
//! `file cat`) so muscle memory and documentation transfer. Output
//! formats mirror it too, down to the box-drawing arrows in
//! `entity trace`, so the two can be diffed rather than eyeballed.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::Write;
use templedb_db::{Direction, ProjectLookup, SnapshotBody};

mod summary_view;
mod table;
mod where_view;

#[derive(Parser)]
#[command(
    name = "templedb-rs",
    version,
    about = "TempleDB, ported to Rust — read-only subset",
    long_about = "A port in progress. Reads the same \
~/.local/share/templedb/templedb.sqlite the Python implementation owns; \
writes are deliberately not implemented yet."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Project management (read-only subset)
    Project {
        #[command(subcommand)]
        command: ProjectCmd,
    },
    /// File management (read-only subset)
    File {
        #[command(subcommand)]
        command: FileCmd,
    },
    /// Entity graph queries
    Entity {
        #[command(subcommand)]
        command: EntityCmd,
    },
    /// Search file contents and metadata
    Search {
        #[command(subcommand)]
        command: SearchCmd,
    },
    /// Read-only observations of source state (snapshots)
    Source {
        #[command(subcommand)]
        command: SourceCmd,
    },
    /// Version control history (read-only subset)
    Vcs {
        #[command(subcommand)]
        command: VcsCmd,
    },
    /// Invariant-check results. **`history` only — see below.**
    ///
    /// Python's `doctor` has two subcommands and this carries one.
    /// `doctor entities` is not ported **yet**: it is 44 checks, and
    /// re-derived from their bodies on 2026-11-05 they are 17 reachable
    /// from the mirrored subset, 10 needing tables outside it, 8 asking
    /// SQLite about itself (`sqlite_master`, `pragma_function_list`,
    /// the `-wal` sidecar), 8 reading a filesystem and 1 reading the
    /// Python CLI's own argparse tree. Its exit status is computed over
    /// the whole set, so a partial run cannot reproduce even that.
    ///
    /// "Yet" since 2026-10-31, with one exception found on 2026-11-05:
    /// `cli_help_is_populated` has the *Python implementation* as its
    /// subject, so there is no Rust statement of it and the whole-set
    /// exit status is unreachable at 43 of 44 as well as at 17. That is
    /// a reason to decide what `doctor entities` means here, not a
    /// refusal to port it.
    ///
    /// Only **4** of the 8 filesystem checks read a checkout tree, so
    /// the tree-pinning harness (step 26) unblocks four and not
    /// fifteen; the 8 engine checks are blocked on the verification
    /// backend instead, which is a different and harder thing. See
    /// `templedb-db/src/doctor.rs` for the table and the README's
    /// *Deferred, not refused*.
    ///
    /// Clap refuses a bare `doctor` and prints the subcommands, which
    /// is Python's `required=True` behaviour.
    Doctor {
        #[command(subcommand)]
        command: DoctorCmd,
    },
    /// Proposed source edits. **All five of Python's, as of
    /// 2026-11-18.**
    ///
    /// `list` and `show` are reads and landed first on purpose, which
    /// is what makes the three writes verifiable: a write here is
    /// checked by reading its row back through the two commands the
    /// byte-diff sweep already compares against Python.
    ///
    /// The reads were taken before `cancel`, which is what the README
    /// had queued, because they are what `cancelled_at` and
    /// `applied_commit_id` were actually waiting for: `intent show`
    /// prints both, so a read is what moved the mirror's boundary, not
    /// a write. See `templedb-db/src/intent.rs`.
    ///
    /// `apply` carries the family's one behavioural divergence: a base
    /// the file has moved past is refused rather than merged onto, and
    /// the reason is measured rather than scoped — see *The rebase
    /// this port refuses to do*.
    Intent {
        #[command(subcommand)]
        command: IntentCmd,
    },
    /// Copy the read subset from SQLite into a Postgres database.
    ///
    /// The one write path in this binary, and it writes only to the
    /// destination: SQLite is read through the same read-only handle
    /// every other command uses, so the Python implementation's
    /// exclusive authority over the live file is untouched. The mirror
    /// is a read replica and a parity target, and is exactly as stale
    /// as the last run of this command.
    Mirror {
        /// Postgres connection string for the destination.
        #[arg(long = "to", value_name = "URL")]
        to: String,
        /// Run the read-subset DDL before copying.
        ///
        /// Off by default because it is the destructive half: on an
        /// existing mirror the `CREATE TABLE`s fail and nothing is
        /// copied, which is the right outcome for a typo'd URL.
        #[arg(long)]
        create_schema: bool,
        /// Report whether the mirror is still current, and copy
        /// nothing.
        ///
        /// Compares the row counts `mirror` recorded against the live
        /// SQLite and exits 4 if they disagree. Exists because a stale
        /// mirror does not announce itself: on 2026-10-16 a `publish`
        /// added rows and the cross-backend sweep went from 40 of 40
        /// identical to six differences, every one of them replication
        /// lag rather than a bug in this port. Six rows of parity
        /// failure is a very convincing way to be wrong.
        ///
        /// Catches inserts and deletes, not updates in place. See
        /// `compare_counts`.
        #[arg(long, conflicts_with = "create_schema")]
        check: bool,
    },
    /// Digest every mirrored table, for comparison across a write.
    ///
    /// The state-diff harness the write-path design document asks for,
    /// in the only form a shell needs: one line per table, `rows` and
    /// a 128-bit digest, deterministic and ordered by the table list.
    /// Capture before and after a mutation and `diff` the two files.
    ///
    /// Reads whichever backend the environment selects, so the two
    /// captures can come from different engines — which is the point.
    /// `mirror` at a known instant, let Python write to SQLite and the
    /// port write to its Postgres, and compare. The digest is
    /// order-independent precisely so the two engines' disagreement
    /// about TEXT collation cannot show up as a false difference.
    ///
    /// Unlike `mirror --check`, this catches an update in place: a
    /// flipped `is_current` leaves every row count alone and changes
    /// the digest.
    ///
    /// Reads every row of the subset, `content_blobs` included. It is
    /// not cheap and is not meant to be run in a loop.
    State,
    /// Health at a glance — ingest, doctor, reconcile, handoff.
    ///
    /// The first command in this port that is not a pure function of
    /// the database, and the only one whose output the byte-diff sweep
    /// cannot verify: Python's reads the wall clock, the hostname,
    /// `os.getppid()`, systemd and `TEMPLEDB_SESSION_ID` alongside the
    /// rows. The facts come from `templedb-db/src/summary.rs`, which
    /// reads none of those; the clock and the session identity are
    /// resolved here and passed in.
    ///
    /// **Seven of Python's nine panes.** The agent-fleet pane has no
    /// observation layer — its tables are outside the mirrored subset —
    /// and the systemd pane reads `systemctl` rather than the database.
    /// Both print a `(not ported)` line in place, because a pane that
    /// silently vanishes reads as good news.
    Summary {
        /// Print without ANSI colour.
        ///
        /// Not equivalent to piping the coloured output through a
        /// filter: Python pads several strings *after* painting them,
        /// so the column widths differ between the two forms. See
        /// `summary_view.rs`.
        #[arg(long)]
        no_color: bool,
    },
}

#[derive(Subcommand)]
enum SearchCmd {
    /// Search current file contents.
    ///
    /// Whitespace-separated terms must all appear; double quotes group
    /// one. Matching is case-insensitive substring, not stemmed and not
    /// tokenised — `run` finds `runtime`. There are no operators: FTS5's
    /// query language and Postgres's are different languages, and the
    /// part of this command that decides the answer is in Rust so that
    /// it means the same thing on both backends.
    Content {
        pattern: String,
        /// Limit to one project
        #[arg(short, long)]
        project: Option<String>,
        #[arg(short, long, default_value_t = 100)]
        limit: i64,
    },
    /// Search file names.
    ///
    /// Matches the pattern as a literal substring of the path. Python
    /// treats it as a LIKE pattern, so `_` is a wildcard there; see
    /// `search.rs` for the measurement and the README for why this one
    /// diverges.
    ///
    /// No `--limit`: Python's hundred is hardcoded and reproduced, and
    /// a flag only this side has would be a surface divergence bought
    /// for nothing. When the hundred cuts the list short, this says so
    /// on stderr — stdout is Python's table, byte for byte.
    ///
    /// `--project` is an exact slug here and fuzzy nowhere else in this
    /// command family, because Python's `search` module is the one that
    /// never calls the fuzzy matcher. A name that matches nothing
    /// selects nothing and exits 0, as Python does.
    Files {
        pattern: String,
        /// Limit to one project (exact slug)
        #[arg(short, long)]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List all projects.
    ///
    /// The three filters are Python's and select rows only — the counts
    /// a project shows are the same whether you filtered or not. Python
    /// runs a separate query under any of them, with a different file
    /// count, a different line source and a different order, and then
    /// prints two tables. See `projects.rs`.
    #[command(alias = "ls")]
    List {
        /// Only projects with `is_nix_project = 1`
        #[arg(long)]
        nix_only: bool,
        /// Only projects in one category
        #[arg(long, value_enum)]
        category: Option<Category>,
        /// Only projects of one type.
        ///
        /// A different column from `--category`, despite the names:
        /// `--type service` and `--category service` are two questions
        /// that happen to share a word for an answer, and the live data
        /// varies them independently. Python's own note says the flag
        /// replaced `nixos list-configs`, which was this query with the
        /// type hardcoded to `nixos-config`.
        #[arg(long = "type", value_enum, value_name = "TYPE")]
        project_type: Option<ProjectType>,
    },
    /// Show one project's details.
    ///
    /// Takes an exact slug, not a fuzzy name — which is what the Python
    /// command does, and the one place it differs from every other
    /// project argument.
    Show { slug: String },
}

#[derive(Subcommand)]
enum FileCmd {
    /// Print a tracked file's current contents
    Cat { project: String, path: String },
    /// Write a file's contents. **The port's first write, and the only
    /// one that changes source.**
    ///
    /// Writes the port's own Postgres — the one named by
    /// `TEMPLEDB_WRITE_URL` — and never the live SQLite database, which
    /// the Python implementation keeps sole authority over. Requires
    /// `TEMPLEDB_RS_ALLOW_WRITES=1`.
    ///
    /// **Partial.** Python's `file set` carries seven guards; this
    /// carries six — `--if-match`, the un-delete, the mojibake check,
    /// `--verify`, the `edit_intents` record and the auto-stage.
    /// Absent: the `--if-match` three-way rebase and its `--no-rebase`
    /// switch. The opt-in exists because none of that is visible from
    /// a command line. See the README's *The first write*.
    ///
    /// Takes an exact slug, not a fuzzy name — the one place this port
    /// is deliberately stricter than Python, because a mis-resolved
    /// write overwrites a file.
    Set {
        project: String,
        path: String,
        /// Content to write. Reads stdin when absent.
        #[arg(short, long)]
        content: Option<String>,
        /// Refuse unless the file currently hashes to HASH.
        ///
        /// A full sha256 or a prefix of at least 8 hex characters.
        /// `file set` replaces the whole file, so content read earlier
        /// is a lock on all of it: anything written in between is
        /// reverted silently, with no conflict. Use this whenever the
        /// content being written was derived from a copy read earlier.
        #[arg(long = "if-match", value_name = "HASH")]
        if_match: Option<String>,
        /// After the write, confirm `is_current` holds what was
        /// written. Exits 2 on mismatch.
        ///
        /// Checked on a fresh transaction after the commit, so it can
        /// actually fail: reading inside the writing transaction would
        /// see the write's own uncommitted effect and could only agree.
        #[arg(long)]
        verify: bool,
        /// Permit content that newly looks double-encoded.
        ///
        /// The guard refuses when a write *increases* the number of
        /// such sequences, not when any are present — a file that is
        /// already corrupt has to stay editable and a repair has to
        /// pass. Exits 3.
        #[arg(long)]
        allow_mojibake: bool,
        /// Do not record an `edit_intents` row for this write.
        ///
        /// The record is provenance — who set this, when, from which
        /// session — and it is written inside the same transaction as
        /// the write, so unlike Python's it cannot fail independently
        /// of it. That is also why this flag matters more here than
        /// there: Python swallows an intent failure and still writes,
        /// and this refuses the write. A caller who wants the one
        /// without the other says so.
        #[arg(long)]
        skip_intent: bool,
        /// Require the stage: refuse the write unless a session owns it.
        ///
        /// Every `file set` with a session stages the file, so this
        /// flag adds no effect — it withdraws the silence. Without it,
        /// a write with no session stages nothing and says so on
        /// stderr; with it, that becomes a refusal before anything is
        /// written.
        ///
        /// Stricter than Python's, deliberately. Over there `--stage`
        /// only changes the success message, because its auto-stage
        /// *creates* the session it needs and so cannot fail to find
        /// one. This port will not create a session row, and a staged
        /// row with no owner means unstaged — `templedb publish` would
        /// ship nothing and report success. See the README's
        /// *The auto-stage*.
        #[arg(short = 's', long)]
        stage: bool,
        /// Record the intent against this session, and stage the write
        /// as it. A numeric id naming a live row; outranks
        /// `TEMPLEDB_SESSION_ID`.
        ///
        /// An id and not a name. Python accepts `ID|NAME` and its name
        /// path *creates* the session when none matches, which is the
        /// one thing this port will not do — see *Sessions* in the
        /// README. `--session 1163` works; `--session my-agent` is
        /// refused and says why.
        ///
        /// Python's reason for having the flag at all applies here
        /// unchanged, and it is a subprocess problem: identity is
        /// resolved independently by whichever process writes, so
        /// `templedb reports reindex` shelling out to `file set` lands
        /// its stage in a session the parent is not looking at. That
        /// bit twice on 2026-10-08, each time blocking a publish. This
        /// port now has a stage to misplace the same way, so pin the
        /// same id for the `file set` and the `vcs commit-staged` that
        /// follows it or the commit will find nothing.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
        /// After writing, commit this session's staged files. Implies
        /// `--stage`.
        ///
        /// Runs `vcs commit-staged` in **the write's own transaction**,
        /// so either the content is committed or nothing happened.
        /// Python runs the two as separate transactions and keeps the
        /// write when the commit fails; that partial state is still
        /// reachable here, by passing `-s` instead of this.
        ///
        /// **Not scoped to this file.** It commits every row this
        /// session has staged on the branch, which is what Python's
        /// `--commit` does too — the commit selects by session, not by
        /// path. Anything staged by an earlier `file set` in the same
        /// session goes with it.
        #[arg(long)]
        commit: bool,
        /// Commit message when `--commit` is passed. Defaults to
        /// `file set: <path>`, which is Python's default.
        #[arg(short = 'm', long = "commit-msg", value_name = "MSG")]
        commit_msg: Option<String>,
    },
    /// Stage a file for deletion (hard-delete on commit; history preserved)
    ///
    /// **The sixth write.** Writes the port's own Postgres and requires
    /// `TEMPLEDB_RS_ALLOW_WRITES=1`, like every other write here.
    ///
    /// Nothing is deleted by this command. It flips one
    /// `vcs_working_state` row to `state = 'deleted'` and names the
    /// session that owns it; `vcs commit-staged` is what hard-deletes
    /// the `project_files` row, and until then `file cat` still answers
    /// and `file ls` still lists the file.
    ///
    /// **No flags, on purpose**, which is a decision rather than an
    /// omission: Python's `file rm` has none either, so the surfaces
    /// agree exactly and the sweep's flag stage needs no allow-list
    /// entry for this command. The session therefore comes from
    /// `TEMPLEDB_SESSION_ID` only — there is no `--session` here, where
    /// `file set` has one because *Python* has one. Inventing the flag
    /// would widen a surface for this port's convenience, which is the
    /// thing `file where` was given no `--no-color` to avoid.
    ///
    /// Takes an exact slug, not a fuzzy name, for `file set`'s reason.
    Rm { project: String, path: String },
    /// Set the edit_mode of a file (immutable | hot-reload | live-symlink)
    ///
    /// **The seventh write**, and the smallest: one column on one row.
    /// Writes the port's own Postgres and requires
    /// `TEMPLEDB_RS_ALLOW_WRITES=1`, like every other write here.
    ///
    /// `project_files.edit_mode` was mirrored for `file ls` and is read
    /// by it two ways — `-l` prints it and `--edit-mode` filters on it —
    /// so this is the rare write whose effect shows up in output the
    /// sweep already byte-diffs against Python.
    ///
    /// **No session and no clock**, neither of them omitted: Python
    /// resolves no session here, and the `updated_at` its `UPDATE` moves
    /// is moved by a trigger this port does not have, on a column it
    /// does not mirror. That divergence is in `filemark.rs` and in the
    /// README's *Known gaps*, and it predates this command — `file set`
    /// has fired the same trigger since step 15.
    ///
    /// Takes an exact slug, not a fuzzy name, for `file set`'s reason —
    /// and here fuzzy matching is also what makes Python's success line
    /// able to name a project that does not exist. See
    /// *The mode that is spelled by a NULL*.
    Mark {
        project: String,
        path: String,
        /// The mode to set. Python's three `choices=` verbatim.
        ///
        /// Required, as Python's is. Three values and not `file ls`'s
        /// four: `non-immutable` is a predicate over the column rather
        /// than a value it can take, and upstream's two `choices=` lists
        /// differ for exactly that reason.
        #[arg(long, value_enum, value_name = "MODE")]
        edit_mode: MarkMode,
    },
    /// List tracked files, optionally under a path prefix
    Ls {
        project: String,
        #[arg(default_value = "")]
        prefix: String,
        /// Show detailed info (lines of code)
        #[arg(short, long)]
        long: bool,
        /// Filter by edit_mode (migration 106).
        ///
        /// `non-immutable` matches anything opted out of the default —
        /// which on current data is nothing, because all 2,491 live
        /// rows have a NULL `edit_mode`. So does `immutable`: Python
        /// compares the raw column, and NULL is not the string.
        #[arg(long, value_enum, value_name = "MODE")]
        edit_mode: Option<EditMode>,
    },
    /// Re-run the classifier over recorded files and report the
    /// `file_type_id`s it disagrees with
    ///
    /// **A read, and the dry half of a command whose other half writes.**
    /// `project_files.file_type_id` is stamped once, when a file is
    /// first recorded, so it is a snapshot of whatever classifier was in
    /// play then rather than a derived value. This re-runs
    /// `importer.scanner`'s current one over every active row and
    /// reports the disagreements, grouped by the move they would make.
    ///
    /// Python's is dry by default too, which is what makes this the one
    /// command in the `file` family whose interesting behaviour has a
    /// diffable stdout — so it is in the parity sweep and byte-diffed
    /// against Python on live data, where every other `file` write had
    /// to be verified by cross-read.
    ///
    /// **`--apply` is deliberately absent, not accepted and ignored.**
    /// Python's writes; this port mirrors `file_types` at two of five
    /// columns and `category` is `TEXT NOT NULL`, so the resolve-or-
    /// create arm cannot be written faithfully until the subset grows.
    /// A flag that promises a write and silently writes nothing is the
    /// one outcome this project treats as a defect, so it is refused at
    /// the parser and exits 2. See `retype.rs` and the README's
    /// *Known gaps*.
    ///
    /// The project is a fuzzy pattern, as Python's is — unlike every
    /// *write* in this port, which takes an exact slug. Nothing here
    /// writes, so a mis-resolution costs a wrong report rather than a
    /// wrong file.
    Retype {
        /// Project name or pattern (default: all projects)
        project: Option<String>,
        /// Only consider rows currently recorded as this type, e.g.
        /// `--from-type api_route`
        ///
        /// A filter on the joined `file_types.type_name`, which is
        /// Python's — and note that it therefore drops every row whose
        /// `file_type_id` is NULL or dangling, which are the rows most
        /// likely to need re-typing.
        #[arg(long = "from-type", value_name = "TYPE")]
        from_type: Option<String>,
    },
    /// Show every mirror location for a file with content-hash drift status
    ///
    /// **The port's first command that reads a filesystem.** It hashes
    /// the checkout, the edit workspace and any `/nix/store` overlay
    /// copy, and compares each against the hash the database holds. It
    /// writes nothing and resolves nothing, so it competes with the
    /// Python implementation for authority over nothing — which is why
    /// it lands before the tree *layer* rather than after it.
    ///
    /// What it waited on was the sweep's tree fence, not the write
    /// path: the two binaries run sequentially, so a tree edited
    /// between them is a difference neither produced. See
    /// `scripts/parity-sweep.sh` for why that is a fence and not a pin,
    /// and `where_view.rs` for the three places this reproduces
    /// something upstream has wrong — including an `edit-work` probe
    /// that can never fire.
    Where { project: String, path: String },
}

#[derive(Subcommand)]
enum EntityCmd {
    /// Entity counts by kind
    Stats,
    /// One hop out of and into an entity, addressed <kind>/<external_ref>
    Explore {
        /// e.g. Commit/1CC0818B7CC9085A
        entity: String,
    },
    /// Recursive BFS walk from an entity — multi-hop graph queries
    Trace {
        /// e.g. Machine/zMothership2
        entity: String,
        /// Max hops
        #[arg(long, default_value_t = 3)]
        depth: usize,
        /// Follow outbound (default), inbound, or both
        #[arg(long, value_enum, default_value_t = Dir::Out)]
        direction: Dir,
        /// Comma-separated relation kinds to follow (default: all)
        #[arg(long)]
        via: Option<String>,
        /// Per-node fan-out cap, per direction
        #[arg(long, default_value_t = 10)]
        limit: i64,
    },
    /// Case-insensitive substring search across label and external_ref
    Search {
        /// Substring to look for (literal: `_` and `%` are not wildcards)
        query: String,
        /// Restrict to one entity kind — see `entity stats` for the list
        #[arg(long)]
        kind: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// Shortest path between two entities (BFS)
    Paths {
        /// <kind>/<ref>
        #[arg(value_name = "FROM")]
        from: String,
        /// <kind>/<ref>
        #[arg(value_name = "TO")]
        to: String,
        /// BFS cutoff
        #[arg(long, default_value_t = 6)]
        max_depth: usize,
        /// Traversal direction
        #[arg(long, value_enum, default_value_t = Dir::Both)]
        direction: Dir,
        /// Comma-separated relation kinds to follow (default: all)
        #[arg(long)]
        via: Option<String>,
    },
    /// Files that import another file without any Symbol call landing
    /// in it.
    ///
    /// Investigation only — side-effect imports (plugin registration,
    /// monkeypatching) legitimately have no calls.
    DeadImports {
        /// Restrict to one project (default: all)
        #[arg(long)]
        slug: Option<String>,
        /// Max candidates
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Archive history for one entity — when did label /
    /// source_authority change
    Observations {
        /// <kind>/<ref>
        entity: String,
        /// Max rows
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
}

#[derive(Subcommand)]
enum VcsCmd {
    /// Show commit history.
    ///
    /// Defaults to Python's default view — the published commits plus
    /// the ones owned by your session — when a session is identified,
    /// and to the published view alone when none is. A session is
    /// identified by `TEMPLEDB_SESSION_ID` naming a live row and by
    /// nothing else: Python would additionally guess from the process
    /// tree and *create* a row if it had to, which is a write a read
    /// command must not perform. The view is printed in the header
    /// either way, so the output says which one you got.
    Log {
        project: String,
        /// Number of commits to show
        #[arg(short = 'n', default_value_t = 10)]
        n: i64,
        /// Filter by branch name (exact)
        #[arg(short, long)]
        branch: Option<String>,
        /// Accepted and ignored, as Python's is: including your
        /// session's unpublished commits is the default.
        ///
        /// Python marks it `[deprecated no-op — this is now the
        /// default]` and that is exactly what it is on both sides. The
        /// `--no-fts` test applies and passes: a no-op flag is only
        /// harmful when the thing it names is absent, and here the
        /// default view it asks for is the default view you get.
        #[arg(long)]
        session: bool,
        /// Show every commit, including other sessions' unpublished
        /// ones. The one view that consults no session at all.
        #[arg(long)]
        all_sessions: bool,
        /// Show only published commits, hiding your session's
        /// unpublished ones.
        ///
        /// No longer a no-op: until the session view landed this
        /// selected the behaviour the port already had, and now it
        /// selects a narrower one. It is still the most portable of
        /// Python's three views, being the only one that needs no
        /// session at all.
        ///
        /// `--all-sessions` wins if both are passed, which is Python's
        /// precedence inverted — its `--published` overrides
        /// `--all-sessions`. Reproducing the inversion would mean
        /// `--published` silently discarding a view the caller asked
        /// for; conflicting flags are better refused, so clap refuses
        /// them.
        #[arg(long, conflicts_with = "all_sessions")]
        published: bool,
    },
    /// List branches, with the active one marked.
    ///
    /// Listing only — creating and deleting branches are writes.
    Branch { project: String },
    /// Diff one file between revisions.
    ///
    /// Three forms, mirroring Python:
    ///
    /// - `diff <proj> <file> <c1> <c2>` — between two commits.
    /// - `diff <proj> <file> <c1>` — a commit against current.
    /// - `diff <proj> <file>` — the two newest stored revisions. This
    ///   one can never produce a diff against the live database:
    ///   `file_contents` keeps no history, so it always reports that
    ///   only one version exists. Ported anyway, because the message is
    ///   the command's answer.
    ///
    /// Python's `--staged` and its no-file working-set form are absent:
    /// staging is session-scoped and the working set is written by
    /// `vcs status --refresh`. Same boundary as `vcs log`'s default.
    Diff {
        project: String,
        /// File path or pattern (fuzzy). Not required with `--staged`,
        /// and omitting it diffs the whole working set.
        ///
        /// `Option` since 2026-10-31. It was a required positional,
        /// which is a divergence the sweep cannot see: the flag-surface
        /// stage parses the `options:` block and never compares
        /// positional arity. Python has had `nargs='?'` here for as
        /// long as `_diff_working` has existed.
        file: Option<String>,
        /// Commit hash or prefix
        commit1: Option<String>,
        /// Commit hash or prefix
        commit2: Option<String>,
        /// Diff the staged changes instead of a file's revisions.
        ///
        /// Shows every staged row on the active branch, from **any**
        /// session — not just this one. `vcs commit-staged` scopes to
        /// one session, so this can show work a commit would not take.
        /// Python's behaviour; see `staged_files`.
        #[arg(long)]
        staged: bool,
        /// Render as a context diff rather than a unified one.
        ///
        /// The name is Python's and it is wrong: this prints each hunk's
        /// old block and then its new block, one above the other, not in
        /// two columns. `difflib.context_diff` is what the flag selects.
        /// Kept because the flag name is the contract — `-s` is in
        /// scripts and in `templedb vcs diff --help`.
        #[arg(long, short = 's')]
        side_by_side: bool,
        /// Print the diff without ANSI colour.
        #[arg(long)]
        no_color: bool,
    },
    /// Commit this session's staged rows. **Writes.**
    ///
    /// Deliberately NOT called `vcs commit`. Python's scans a workspace,
    /// reconciles it against the database, and carries nine guards —
    /// five of which compare a tree this port does not have. This
    /// command commits what is already staged and refuses anything that
    /// would need a tree, which is the operation `publish` performs
    /// rather than a reduced `vcs commit`. Sharing the name would
    /// promise `--amend`, `--strategy` and the whole scan-and-reconcile
    /// workflow and then refuse them.
    ///
    /// Writes the port's own Postgres — the one named by
    /// `TEMPLEDB_WRITE_URL` — and never the live SQLite database.
    /// Requires `TEMPLEDB_RS_ALLOW_WRITES=1`.
    ///
    /// Needs an explicit session: `TEMPLEDB_SESSION_ID` or
    /// `--session <id>`. Python would guess from the process tree and
    /// *create* a row if it had to; this accepts or declines. Staged
    /// rows belong to a session, so there is nothing to commit without
    /// one — it refuses rather than staging into a guess.
    ///
    /// Takes an exact slug, not a fuzzy name, for the reason `file set`
    /// does: a mis-resolved write lands in the wrong project's history.
    CommitStaged {
        project: String,
        /// Commit message.
        ///
        /// Prefer `-F` for anything containing backticks: an inline
        /// message is a double-quoted shell string, so each backtick
        /// span is command-substituted before argv is built. Commit
        /// 217092EC lost three quoted command names that way and
        /// reported success.
        #[arg(short, long)]
        message: Option<String>,
        /// Read the message from a file; `-` for stdin.
        #[arg(short = 'F', long = "message-file", value_name = "PATH")]
        message_file: Option<String>,
        /// Branch by exact name (default: the project's active branch).
        #[arg(short, long)]
        branch: Option<String>,
        /// `vcs_commits.author`. Defaults to `git config user.name`,
        /// then the literal `unknown` — Python's chain.
        #[arg(long)]
        author: Option<String>,
        /// Whose staged rows to commit. A numeric id naming a live row;
        /// outranks `TEMPLEDB_SESSION_ID`.
        ///
        /// An id and not a name, as everywhere else in this port: the
        /// name path *creates* the session over there.
        #[arg(long, value_name = "ID")]
        session: Option<String>,
    },
    /// Show commit metadata, its changed-file list, and a diff of each
    /// file against the commit's first parent.
    ///
    /// The diff is rendered by this port's reimplementation of CPython's
    /// `difflib.SequenceMatcher` — see `difflib.rs`. A Myers-based crate
    /// would not do: on 16 of 28 real revision pairs from this database
    /// difflib places hunks differently from `diff -u`, six of them at
    /// identical line counts.
    Show {
        project: String,
        /// Commit hash or prefix
        commit: String,
        /// Print metadata and the changed-file list only.
        #[arg(long)]
        no_diff: bool,
        /// Print the diff without ANSI colour.
        #[arg(long)]
        no_color: bool,
    },
}

#[derive(Subcommand)]
enum DoctorCmd {
    /// Show recent invariant check history (drift over time)
    History {
        /// Filter to one invariant name
        #[arg(long, value_name = "NAME")]
        check: Option<String>,
        /// Only show violations / errors
        #[arg(long)]
        violated_only: bool,
        /// Max rows (default 30)
        ///
        /// `i64` where Python takes the argument as a *string* and
        /// calls `int()` on it at the end of the function — so a
        /// non-numeric value gets as far as building the query there
        /// and raises, while clap refuses it during parsing. Both
        /// refuse; the statuses differ (clap's 2, Python's 1) and the
        /// flag surfaces do not, which is what the sweep compares.
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
}

#[derive(Subcommand)]
enum IntentCmd {
    /// Propose an edit (does not apply). **Writes.**
    ///
    /// Postgres only and opt-in, like the other three writes. It writes
    /// `edit_intents` and `content_blobs` and **nothing else** — no
    /// `file_contents`, no `project_files`, no stage. That is the
    /// command's point on both sides: a proposal is a row, and
    /// `intent apply` is what turns it into source.
    ///
    /// Takes an exact slug, not a fuzzy name, where Python fuzzy-matches
    /// — `file set`'s divergence and for `file set`'s reason, plus one
    /// that is forced: the id is resolved inside the write transaction,
    /// because the reader and the writer point at different databases.
    ///
    /// Its restraint — two tables and no source change — is what made
    /// it safe to land while `apply` was unported. `apply` arrived on
    /// 2026-11-18, so a proposal made here is now one this binary can
    /// turn into source.
    Create {
        /// Project slug (exact, not fuzzy)
        project: String,
        /// File path within project
        file_path: String,
        /// New content as string
        ///
        /// Wins over `--from-file` when both are passed, which is
        /// Python's order — and `-c ''` counts as content, because it
        /// tests `is not None`. An empty proposal is therefore
        /// accepted here where `file set` refuses an empty pipe; see
        /// the README's *Known gaps*.
        #[arg(short, long)]
        content: Option<String>,
        /// Read new content from a file
        #[arg(short = 'f', long = "from-file", value_name = "PATH")]
        from_file: Option<String>,
        /// Base revision (default: current)
        #[arg(long = "base-rev", value_name = "REV")]
        base_rev: Option<String>,
        /// Description / commit note
        #[arg(short, long)]
        message: Option<String>,
    },
    /// List intents (default: outstanding)
    List {
        /// Filter by project slug
        #[arg(short, long)]
        project: Option<String>,
        /// Filter by session id
        #[arg(short, long, value_name = "ID")]
        session: Option<i64>,
        /// Include applied + cancelled
        #[arg(long)]
        all_statuses: bool,
        /// Max rows (default 50)
        ///
        /// `i64` where Python's `--limit` has no `type=`, so it is a
        /// string that `int()` is called on inside the query build —
        /// `doctor history`'s note exactly, including that clap
        /// refuses a non-numeric value at parse time with status 2
        /// where Python raises inside the function with status 1.
        #[arg(long, default_value_t = 50)]
        limit: i64,
    },
    /// Show one intent in full
    Show {
        /// Intent id (integer)
        id: i64,
    },
    /// Apply a proposed intent. **Writes.**
    ///
    /// Postgres only and opt-in, like the other four writes. The fifth
    /// one, and the third at zero schema cost: it points
    /// `file_contents` at the proposed hash and marks the row
    /// `'applied'`, both in tables this port already mirrors, reads and
    /// writes.
    ///
    /// It does **not** make a commit, which is Python's own emphasis —
    /// apply changes source, `vcs commit-staged` makes history, and
    /// keeping them apart is what lets several intents land in one
    /// commit.
    ///
    /// **One deliberate divergence, and it is the whole of
    /// `--no-rebase`.** A base the file has moved past is refused here
    /// with exit 4 — always, not only when the flag is given. Python
    /// three-way merges instead, and the engine it merges with is not a
    /// three-way merge: measured 2026-11-18 against `git merge-file
    /// --diff3` over eleven triples, it agrees on 3, and four of its
    /// "clean" results write content neither side ever held. One of
    /// them was reproduced end to end upstream — `templedb intent apply
    /// 1645` exited 0 and dropped a line nobody deleted. See the
    /// README's *The rebase this port refuses to do*.
    Apply {
        /// Intent id (integer)
        id: i64,
        /// Refuse (exit 4) if the file has moved since the intent was
        /// written, instead of three-way merging the proposal onto what
        /// landed.
        ///
        /// Accepted, and what it selects is what happens either way.
        /// That is not the `--no-fts` shape this port rejected as
        /// "accepted and ignored": `--no-fts` promises matching the
        /// port cannot deliver, where this promises a refusal the port
        /// *does* deliver. Omitting it would make the stricter
        /// implementation the one that errors on the flag a caller
        /// passes to ask for strictness — and it is the reason the
        /// surface agrees with Python's exactly, with no allow-list
        /// entry.
        #[arg(long = "no-rebase")]
        no_rebase: bool,
    },
    /// Cancel a proposed intent. **Writes.**
    ///
    /// Postgres only and opt-in, like the other four writes. The
    /// smallest one in the port: it sets `status` and `cancelled_at` on
    /// a table already mirrored and already read, so it adds no table
    /// and no column.
    Cancel {
        /// Intent id (integer)
        id: i64,
    },
}

#[derive(Subcommand)]
enum SourceCmd {
    /// Print a file's content at a revision (default: current)
    Snapshot {
        project: String,
        file_path: String,
        /// Commit hash, prefix accepted (default: current state)
        #[arg(long, value_name = "COMMIT")]
        rev: Option<String>,
        /// Print observation metadata instead of content
        #[arg(long)]
        meta: bool,
    },
    /// List every known revision of a file (for --rev lookup)
    Revisions { project: String, file_path: String },
}

/// The five categories Python's `argparse` offers as `choices`.
///
/// An enum here, and a plain `String` in the query layer, on purpose.
/// Input validation is the edge's job and reproducing Python's fixed
/// list means a bad value exits 2 with a list of the valid ones on both
/// implementations. But the live `project_category` column has no CHECK
/// constraint, so a sixth category added after this binary was built
/// must still filter correctly rather than be unrepresentable — which
/// it would be if the query took this type.
///
/// `clap` derives the kebab-case spellings, which are the ones Python
/// accepts: `desktop-app`, `nixos-module`, `home-module`.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Category {
    Package,
    Service,
    DesktopApp,
    NixosModule,
    HomeModule,
}

impl Category {
    fn as_str(self) -> &'static str {
        match self {
            Category::Package => "package",
            Category::Service => "service",
            Category::DesktopApp => "desktop-app",
            Category::NixosModule => "nixos-module",
            Category::HomeModule => "home-module",
        }
    }
}

/// `project list --type`, Python's three values.
///
/// A separate enum from [`Category`] rather than a shared one, for the
/// same reason the two columns are separate: they overlap on `service`
/// and on nothing else, so one enum covering both would accept
/// `--type package` and `--category nixos-config` — spellings Python
/// refuses — and a refusal is half of what the flag surface compares.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum ProjectType {
    Regular,
    NixosConfig,
    Service,
}

impl ProjectType {
    fn as_str(self) -> &'static str {
        match self {
            ProjectType::Regular => "regular",
            ProjectType::NixosConfig => "nixos-config",
            ProjectType::Service => "service",
        }
    }
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Dir {
    Out,
    In,
    Both,
}

impl From<Dir> for Direction {
    fn from(d: Dir) -> Self {
        match d {
            Dir::Out => Direction::Out,
            Dir::In => Direction::In,
            Dir::Both => Direction::Both,
        }
    }
}

/// `file ls --edit-mode`'s four accepted values, which are Python's
/// `choices=` list verbatim. The first three are `edit_mode` values;
/// `non-immutable` is the catch-all.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum EditMode {
    Immutable,
    #[value(name = "hot-reload")]
    HotReload,
    #[value(name = "live-symlink")]
    LiveSymlink,
    #[value(name = "non-immutable")]
    NonImmutable,
}

impl From<EditMode> for templedb_db::EditModeFilter {
    fn from(m: EditMode) -> Self {
        use templedb_db::EditModeFilter as F;
        match m {
            EditMode::Immutable => F::Exactly("immutable"),
            EditMode::HotReload => F::Exactly("hot-reload"),
            EditMode::LiveSymlink => F::Exactly("live-symlink"),
            EditMode::NonImmutable => F::NonImmutable,
        }
    }
}

/// `file mark --edit-mode`'s three accepted values, which are a
/// *different* `choices=` list from [`EditMode`] above — Python's `ls`
/// takes four and its `mark` takes three.
///
/// A second enum rather than a reuse with a runtime rejection, so the
/// flag surface clap prints is the surface Python's prints and
/// `file mark --edit-mode non-immutable` fails at parse time on both
/// sides rather than in this port's own code.
#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
enum MarkMode {
    Immutable,
    #[value(name = "hot-reload")]
    HotReload,
    #[value(name = "live-symlink")]
    LiveSymlink,
}

impl From<MarkMode> for templedb_db::EditMode {
    fn from(m: MarkMode) -> Self {
        use templedb_db::EditMode as E;
        match m {
            MarkMode::Immutable => E::Immutable,
            MarkMode::HotReload => E::HotReload,
            MarkMode::LiveSymlink => E::LiveSymlink,
        }
    }
}

/// Die on `SIGPIPE` the way every other Unix filter does.
///
/// Rust's runtime sets `SIGPIPE` to `SIG_IGN` before `main`, so a write
/// to a closed pipe returns `EPIPE` instead of killing the process —
/// and `println!` panics on a write error. The effect is that
/// `templedb-rs file cat x | head` printed a Rust panic and a backtrace
/// hint where `templedb file cat x | head` exits quietly:
///
/// ```text
/// thread 'main' panicked at library/std/src/io/stdio.rs:1168:9:
/// failed printing to stdout: Broken pipe (os error 32)
/// ```
///
/// This was not introduced by any one command — it is how the binary
/// has always behaved, and it reproduced on `file cat`, `project list`
/// and `entity search` as soon as anything was piped into `head`.
/// `entity stats` happened not to trigger it only because its output
/// fits in the pipe buffer before the reader exits, which is exactly
/// the kind of accident that keeps a bug like this unnoticed.
///
/// The Python implementation is not uniform here either, and the
/// difference is smaller than it first looked: `project list` exits
/// quietly, while `file cat` catches the error and logs
/// `ERROR Failed to show file: [Errno 32] Broken pipe`. So the claim
/// is not that Python is silent — it is that neither of its behaviours
/// is a panic with a backtrace hint.
///
/// Restoring the default disposition is the conventional fix and
/// better than either: the process dies by signal, the shell reports
/// it as such, and nothing is printed on any command.
fn die_on_sigpipe() {
    // SAFETY: `signal` with `SIG_DFL` is async-signal-safe and this runs
    // before any thread is spawned. Restoring a default disposition
    // cannot fail in a way that matters, so the result is discarded.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() {
    die_on_sigpipe();
    // Return an exit code rather than letting a panic print a backtrace:
    // this is a CLI, and "Error: no database at ..." is the useful output.
    match run() {
        // Which status an error leaves with is the error's own
        // property, not this arm's: `templedb_db::code_of` reads it
        // back off the chain and gives 1 for everything that does not
        // say otherwise. Python separates bad input (1) from an
        // address that names nothing (2), and collapsing both into 1
        // here is what the README carried as a known gap. See
        // `templedb-db/src/exit.rs`.
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(templedb_db::code_of(&e));
        }
        // Not every non-zero exit is an error. `entity paths` answers
        // "no path" on stdout and exits 3, which is a result a script can
        // branch on; collapsing it into 1 would make it indistinguishable
        // from a missing database. `println!` writes through a
        // `LineWriter`, so everything printed is already flushed by the
        // time we get here.
        Ok(code) if code != 0 => std::process::exit(code),
        Ok(_) => {}
    }
}

fn run() -> Result<i32> {
    let cli = Cli::parse();

    // `mirror` is handled before the read store is opened, because it
    // is the one command that does not read through the backend the
    // environment selects — it always goes SQLite -> Postgres. Opening
    // the environment's store first would make `mirror --to <url>`
    // fail whenever `TEMPLEDB_URL` happened to be set, which is
    // precisely when someone is working on the mirror.
    if let Command::Mirror {
        to,
        create_schema,
        check,
    } = &cli.command
    {
        let src = templedb_db::SqliteStore::open_ro(&templedb_db::db_path()?)?;
        if *check {
            let report = templedb_db::check_freshness(&src, to)?;
            match &report.copied_at {
                None => {
                    eprintln!(
                        "mirror has no provenance: it predates `--check`, so it cannot\n\
                         vouch for itself. Re-run `mirror` to record one."
                    );
                    return Ok(4);
                }
                Some(at) => println!("mirrored at {at}"),
            }
            if report.drift.is_empty() {
                println!(
                    "fresh: all {} tables match the source",
                    templedb_db::MIRRORED_TABLES
                );
                return Ok(0);
            }
            eprintln!(
                "STALE: {} table(s) have moved since the mirror",
                report.drift.len()
            );
            for d in &report.drift {
                let was = if d.mirrored < 0 {
                    "absent".to_string()
                } else {
                    d.mirrored.to_string()
                };
                eprintln!(
                    "  {:<20} mirrored {:>9}  source {:>9}",
                    d.table, was, d.source
                );
            }
            eprintln!("\nRe-run `mirror` before trusting a cross-backend comparison.");
            return Ok(4);
        }
        let reports = templedb_db::mirror(&src, to, *create_schema)?;
        let total: u64 = reports.iter().map(|r| r.rows).sum();
        for r in &reports {
            println!("{:<20} {:>9}", r.table, r.rows);
        }
        println!("{:<20} {:>9}", "TOTAL", total);
        return Ok(0);
    }

    let store = templedb_db::open_ro()?;

    match cli.command {
        Command::Project { command } => match command {
            ProjectCmd::List {
                nix_only,
                category,
                project_type,
            } => {
                let filter = templedb_db::ProjectFilter {
                    nix_only,
                    category: category.map(|c| c.as_str().to_string()),
                    project_type: project_type.map(|t| t.as_str().to_string()),
                };
                let projects = templedb_db::list_projects_filtered(&store, &filter)?;
                println!("{:<28} {:>7} {:>10}", "SLUG", "FILES", "LINES");
                for p in &projects {
                    println!("{:<28} {:>7} {:>10}", p.slug, p.files, p.lines);
                }
                // The count goes to stderr so stdout stays a table, and
                // it is the one place a filter is visible in the output:
                // an empty filtered list is otherwise a bare header.
                // Python prints `No projects found matching filters` on
                // stdout instead, which this cannot match anyway — see
                // the README's format divergence.
                eprintln!("\n{} project(s)", projects.len());
            }
            ProjectCmd::Show { slug } => {
                let Some(d) = templedb_db::project_detail(&store, &slug)? else {
                    anyhow::bail!("no project with slug {slug:?}");
                };
                println!();
                println!("Project: {}", d.slug);
                // Python prints these through f-strings over values that
                // may be None, so a missing name renders as the bare
                // word `None` and a missing repo_url as `N/A` — it uses
                // `.get(..., 'N/A')` for the latter two and plain
                // interpolation for the name. Reproduced, because this
                // is the output being diffed.
                println!("Name: {}", d.name.as_deref().unwrap_or("None"));
                println!("Repository: {}", d.repo_url.as_deref().unwrap_or("N/A"));
                println!("Branch: {}", d.git_branch.as_deref().unwrap_or("None"));
                println!();
                println!("Files: {}", d.file_count);
                // The one deliberate divergence: Python formats this
                // with `{:,}` and raises TypeError when the SUM is
                // NULL, printing six of its eight lines and exiting 1.
                // 10 of the 30 live projects hit it. `0` is the honest
                // answer for "no file records a line count".
                println!("Lines of code: {}", thousands(d.total_lines.unwrap_or(0)));
                println!("File types: {}", d.file_types);
                // Python guards this block on `branch_count > 0`, so a
                // project with no branches prints nothing here rather
                // than two zeroes.
                if d.branch_count > 0 {
                    println!();
                    println!("VCS Branches: {}", d.branch_count);
                    println!("VCS Commits: {}", d.commit_count);
                }
                println!();
            }
        },
        Command::File { command } => match command {
            FileCmd::Cat { project, path } => {
                let slug = resolve_project(&store, &project)?;
                // print! not println!: the stored blob keeps its own
                // trailing newline, and adding one makes `| sha256sum`
                // disagree with the Python `file cat` it is replacing.
                print!("{}", templedb_db::read_file(&store, &slug, &path)?);
            }
            FileCmd::Set {
                project,
                path,
                content,
                if_match,
                verify,
                allow_mojibake,
                skip_intent,
                stage,
                session: pin,
                commit,
                commit_msg,
            } => {
                // Read the content before opening anything, so a
                // refusal on stdin does not leave a connection or a
                // transaction waiting on it.
                //
                // stdin is read as BYTES and decoded only if it is
                // valid UTF-8, which is Python's order and Python's
                // reason: reading it as text made `file set` fail
                // outright on any binary file — "'utf-8' codec can't
                // decode byte 0x84 in position 0" — with nothing
                // written. That is why `system_config`'s
                // `.authinfo.gpg` could not be put into the database at
                // all, which let `materialize` delete it from the
                // checkout as an untracked stray and broke a Nix build.
                //
                // `--content` is argv, so it is a `String` already and
                // takes the text path unconditionally. Python's is the
                // same: only the stdin branch can produce bytes.
                let input: Result<String, Vec<u8>> = match content {
                    Some(c) => Ok(c),
                    None => {
                        use std::io::Read;
                        let mut buf = Vec::new();
                        std::io::stdin()
                            .read_to_end(&mut buf)
                            .map_err(|e| anyhow::anyhow!("reading stdin: {e}"))?;
                        // The error carries the bytes back, so an
                        // undecodable pipe costs no second read.
                        String::from_utf8(buf).map_err(|e| e.into_bytes())
                    }
                };
                let content = match &input {
                    Ok(text) => templedb_db::Content::Text(text),
                    Err(bytes) => templedb_db::Content::Binary(bytes),
                };
                if content.is_empty() && if_match.is_none() {
                    // Python refuses empty input outright, on the
                    // grounds that it is almost always a pipe that
                    // produced nothing. `--if-match` makes truncation
                    // explicit, so it is allowed through.
                    anyhow::bail!(
                        "no content provided (use --content, or pipe to stdin).\n\
                         To truncate a file deliberately, pass --if-match with \
                         its current hash."
                    );
                }

                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                // The author, like the clock and the session, is read
                // here and nowhere deeper. Python's order exactly:
                // TEMPLEDB_AUTHOR, then USER, then NULL — and an
                // exported-but-empty variable falls through, because
                // Python's `or` treats `''` as unset.
                let author = edit_author();
                let mut tx = writer.transaction()?;
                // Resolved against the *write* target and inside the
                // write transaction, for the same reason the slug is:
                // the reader and the writer point at different
                // databases, and validating a session against one while
                // stamping it into the other would be a nasty bug.
                let session = templedb_db::resolve_session_tx(&mut tx, pin.as_deref())?;
                // `--commit` needs a session for a reason `--stage`'s
                // refusal does not cover, so it gets its own check and
                // names the flag the caller actually typed: the commit
                // selects staged rows *by session*, so with none there
                // is nothing for it to find and the refusal underneath
                // would be `vcs commit-staged`'s, about a command the
                // caller did not run.
                if commit && session.is_none() {
                    anyhow::bail!(
                        "--commit needs a session: the commit selects staged rows by \
                         session, so without one it would find nothing to commit.\n\
                         \x20 Pass --session <ID>, or set {}. Live ids: \
                         `templedb vcs session list --active`.\n\
                         \x20 Nothing has been written.",
                        templedb_db::SESSION_ID_ENV
                    );
                }
                let opts = templedb_db::SetOptions {
                    if_match: if_match.as_deref(),
                    allow_mojibake,
                    skip_intent,
                    // Python's `want_stage = args.stage or want_commit`.
                    // Here the implication is load-bearing rather than
                    // cosmetic: over there `--stage` only picks a
                    // success message, and here it asserts the stage
                    // happened — which is exactly what `--commit`
                    // depends on.
                    stage: stage || commit,
                    session,
                    author: author.as_deref(),
                };
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let outcome =
                    templedb_db::set_file(&mut tx, &clock, project_id, &path, content, &opts);
                // `?` before the commit, deliberately: on the error path
                // `tx` is dropped without `commit`, and `postgres`
                // rolls an un-committed transaction back on drop. A
                // refusal has to leave nothing behind — `--if-match`
                // exists to *prevent* a write, not to report one — and
                // the ordering here is what makes that true.
                let outcome = outcome?;

                // `--commit`, in the write's own transaction.
                //
                // Python runs the two separately: its write commits,
                // then `VCSCommands().commit()` opens its own
                // transaction, and a failure there returns the commit's
                // status with the write already landed and staged. That
                // outcome is honest over there and it is reachable here
                // too — it is what `-s` without `--commit` produces —
                // so making `--commit` atomic costs no capability and
                // buys the thing the flag's name claims: either the
                // content is committed or nothing happened.
                //
                // It also removes the one genuinely awkward interleave.
                // Two transactions means a HEAD_RACE can land the write,
                // stage it, and exit non-zero, leaving a caller who
                // asked for `file set --commit` to know that the
                // remedy is a *different* command.
                //
                // The author chain is the commit's and not the write's,
                // which is Python's: its `Namespace(author=None)` sends
                // `vcs commit` to resolve its own, so one invocation can
                // stamp `TEMPLEDB_AUTHOR` on the intent and
                // `git config user.name` on the commit. Reproduced
                // rather than unified — they are two columns in two
                // tables answering two questions.
                let committed = if commit {
                    let message = commit_msg
                        .clone()
                        .unwrap_or_else(|| format!("file set: {path}"));
                    let session = session.expect("checked above");
                    let out = templedb_db::commit_staged(
                        &mut tx,
                        &clock,
                        project_id,
                        &project,
                        &templedb_db::CommitOptions {
                            message: &message,
                            author: &commit_author(None),
                            branch: None,
                            session,
                        },
                    )?;
                    Some(out)
                } else {
                    None
                };
                tx.commit()?;

                // After the commit, on its own transaction. Inside the
                // writing one this could only ever agree with itself.
                if verify {
                    let mut tx = writer.transaction()?;
                    templedb_db::verify_current(&mut tx, project_id, &path, &outcome.hash)?;
                }

                // stdout is the hash, so a script can chain on it.
                // Everything else is commentary and goes to stderr,
                // which is the same division `file ls` already uses for
                // its count.
                println!("{}", outcome.hash);
                let mut notes = Vec::new();
                if outcome.created {
                    notes.push("created".to_string());
                }
                if outcome.undeleted {
                    notes.push("revived from deleted".to_string());
                }
                if outcome.unchanged {
                    notes.push("unchanged".to_string());
                } else if let Some(prev) = &outcome.previous {
                    notes.push(format!("was {}", &prev[..12.min(prev.len())]));
                }
                // Python prints `  intent: #N` on stdout, after its
                // `✓ Set <path>` line. Here it joins the stderr
                // commentary, because this command's stdout is the hash
                // and nothing else — a script chains on it. The id is
                // still worth printing for the reason Python gives:
                // "so scripted callers can inspect / cancel it later".
                match outcome.intent_id {
                    Some(id) => notes.push(format!("intent #{id}")),
                    None => notes.push("no intent recorded".to_string()),
                }
                // The stage, always stated — including when it did not
                // happen, which is the case worth a line. Python says
                // nothing here either way: its auto-stage cannot miss,
                // and `--stage` only decorates the success message. A
                // write that staged nothing is a write
                // `vcs commit-staged` will not see, and the two
                // sentences that could follow it are "nothing staged
                // for commit" and, through `publish`, nothing at all.
                notes.push(match &outcome.staged {
                    templedb_db::Staged::Yes { branch, state } => {
                        format!("staged on {branch} as {state}")
                    }
                    templedb_db::Staged::NoSession => {
                        "not staged: no session (pass --session, or -s to refuse \
                         instead)"
                            .to_string()
                    }
                    templedb_db::Staged::NoBranch => {
                        "not staged: the project has no active or default branch".to_string()
                    }
                });
                eprintln!(
                    "{project}/{path}: {} bytes, {} lines{}",
                    outcome.size_bytes,
                    outcome.line_count,
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", notes.join(", "))
                    }
                );
                // The commit goes to stderr, and **stdout stays one
                // line holding the content hash**.
                //
                // That is the part worth arguing. `vcs commit-staged`
                // prints its commit id on stdout because that is what
                // a caller chains on; here the caller chained on the
                // content hash before `--commit` existed, and a flag
                // that changes stdout's shape is how a script breaks
                // without saying so. The cost is real and is not
                // hidden: a `--commit` run cannot pipe its commit id,
                // and a caller who needs it reads stderr or runs
                // `vcs log -n 1`.
                if let Some(c) = &committed {
                    eprintln!(
                        "  committed {} [{}] #{}: {} file(s){}",
                        c.hash,
                        c.branch_name,
                        c.commit_id,
                        c.files,
                        match c.previous_head {
                            Some(h) => format!(", parent #{h}"),
                            None => ", root commit".to_string(),
                        }
                    );
                    // Named when it happened, because `--commit` reads
                    // as "commit this file" and commits the session.
                    // Python has the same behaviour and says nothing.
                    if c.files > 1 {
                        eprintln!(
                            "  note: --commit commits the session, not the path — \
                             {} other staged file(s) went with it",
                            c.files - 1
                        );
                    }
                    if c.other_sessions_staged > 0 {
                        eprintln!(
                            "  note: {} file(s) staged in other sessions were not included",
                            c.other_sessions_staged
                        );
                    }
                }
            }
            FileCmd::Rm { project, path } => {
                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                let mut tx = writer.transaction()?;
                // No pin, because there is no flag to carry one: the
                // resolver reads `TEMPLEDB_SESSION_ID` and checks
                // liveness. Resolved against the write target inside
                // the write transaction, for the reason `file set`'s
                // is — the reader and the writer are different
                // databases.
                let session = templedb_db::resolve_session_tx(&mut tx, None)?;
                let Some(session) = session else {
                    // The documented cost of *resolve never, accept
                    // always*, and here it is the whole command rather
                    // than a side effect of one: `file set` without a
                    // session still writes the content and stages
                    // nothing, and this has nothing left to do.
                    //
                    // The `TEMPLEDB_SESSION` note is included, unlike
                    // `intent create`'s, and that is the measured call
                    // rather than a copy: Python's `file rm` goes
                    // through `get_current_session`, whose step 3 reads
                    // the *name* variable and creates the session when
                    // none matches. Measured on a copy of the live
                    // database 2026-11-18 — `TEMPLEDB_SESSION=zz-no-
                    // such-name templedb file rm …` exited 0 and minted
                    // session 1333. So ignoring the name genuinely
                    // changes the answer here, which is the condition
                    // `ignored_name`'s doc comment sets for warning
                    // about it.
                    let scattered = match templedb_db::ignored_name(None) {
                        Some(name) => format!(
                            "\n{} is set to {name:?}, which this port does not resolve — \
                             the name path creates the session when none matches, and \
                             Python's `file rm` takes it. Pass the id instead.",
                            templedb_db::SESSION_NAME_ENV
                        ),
                        None => String::new(),
                    };
                    anyhow::bail!(
                        "`file rm` needs a session: set {}.\n\
                         A staged row is owned by one, and a NULL \
                         `staged_by_session_id` means *unstaged* rather than staged \
                         by nobody — `vcs commit-staged` would find nothing and \
                         `templedb publish` would ship nothing and report success.\n\
                         There is no --session flag here because Python has none on \
                         this command; this port will not guess either, where Python \
                         resolves through the process tree and creates a row when \
                         that fails.\n\
                         Pick a live id from `templedb vcs session list --active`.{scattered}",
                        templedb_db::SESSION_ID_ENV
                    );
                };
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let outcome = templedb_db::remove_file(&mut tx, &clock, project_id, &path, session);
                // `?` before the commit, for `file set`'s reason: a
                // refusal has to leave nothing behind, and dropping an
                // un-committed `postgres::Transaction` rolls it back.
                let outcome = outcome?;
                tx.commit()?;

                // Python's line, byte for byte, and the only thing on
                // stdout.
                println!("{}", templedb_db::rm_line(&path));
                // The branch and the state that was replaced go to
                // stderr, where this port already puts everything
                // Python does not print. Both are worth saying: the
                // branch because `vcs commit-staged` has to be run
                // against the same one, and the previous state because
                // `Some("deleted")` means this run changed nothing but
                // the owner and the timestamp.
                eprintln!(
                    "{project}/{path}: staged for deletion on {} (was {}, session {session})",
                    outcome.branch,
                    outcome
                        .previous_state
                        .as_deref()
                        .unwrap_or("not staged at all"),
                );
            }
            FileCmd::Mark {
                project,
                path,
                edit_mode,
            } => {
                let mode: templedb_db::EditMode = edit_mode.into();
                let mut writer = templedb_db::PgWriter::open()?;
                let mut tx = writer.transaction()?;
                // No `Clock::from_env()`, and it is the only write here
                // without one. Python's statement names one column and
                // the `updated_at` it moves is moved by a trigger — see
                // `filemark.rs`, which argues why this port does not
                // reproduce the bump and why fixing it for this command
                // alone would be worse than recording it.
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let outcome = templedb_db::mark_file(&mut tx, project_id, &path, mode);
                // `?` before the commit, for `file set`'s reason: a
                // refusal has to leave nothing behind, and dropping an
                // un-committed `postgres::Transaction` rolls it back.
                let outcome = outcome?;
                tx.commit()?;

                // Python's line, byte for byte, and the only thing on
                // stdout. The project it names is the string the caller
                // typed, which is Python's `args.project` — identical to
                // the slug written here only because
                // `resolve_project_exact` accepts nothing else. Over
                // there the two can differ and the line can name a
                // project that does not exist; see `filemark.rs`.
                println!(
                    "{}",
                    templedb_db::mark_line(&project, &path, outcome.previous.as_deref(), mode)
                );
                // The id goes to stderr, where this port already puts
                // everything Python does not print. Worth saying for
                // the one success no reader can observe: a
                // `status = 'deleted'` row takes the column and
                // `file ls` lists neither the file nor its new mode.
                eprintln!("  project_files.id {}", outcome.file_id);
            }
            FileCmd::Ls {
                project,
                prefix,
                long,
                edit_mode,
            } => {
                let slug = resolve_project(&store, &project)?;
                let keep: Option<templedb_db::EditModeFilter> = edit_mode.map(Into::into);
                let files: Vec<_> = templedb_db::list_files(&store, &slug, &prefix)?
                    .into_iter()
                    .filter(|f| keep.is_none_or(|k| k.keeps(f.edit_mode.as_deref())))
                    .collect();
                for f in &files {
                    if long {
                        // `{:>6}`, not `{:>7}`: Python's width is 6 and
                        // this printed one space wider on every row of
                        // every `-l` listing. The `edit_mode` column was
                        // missing outright — NULL renders as the word
                        // `immutable`, which is what the column means.
                        println!(
                            "{:>6} loc  {:<13}  {}",
                            f.lines,
                            f.edit_mode.as_deref().unwrap_or("immutable"),
                            f.path
                        );
                    } else {
                        println!("{}", f.path);
                    }
                }
                eprintln!("\n{} file(s)", files.len());
            }
            FileCmd::Retype { project, from_type } => {
                // Fuzzy, because Python's is and because this writes
                // nothing. `resolve_project_row` reports ambiguity the
                // way the Python matcher does and bails otherwise,
                // which lands on exit 1 — Python's
                // `Project '{x}' not found` is a bare `return 1` too.
                let project_id = match &project {
                    Some(pattern) => Some(resolve_project_row(&store, pattern)?.id),
                    None => None,
                };
                let filter = templedb_db::RetypeFilter {
                    project_id,
                    from_type: from_type.as_deref(),
                };
                let proposal = templedb_db::propose_retype(&store, &filter)?;
                for line in templedb_db::render_retype(&proposal) {
                    println!("{line}");
                }
                // Python prints this only when there were changes: the
                // `not changes` branch returns before reaching it. Not
                // folded into the renderer, because `--apply` replaces
                // this line rather than following it.
                if !proposal.changes.is_empty() {
                    println!();
                    println!("{}", templedb_db::DRY_RUN_NOTICE);
                }
            }
            FileCmd::Where { project, path } => {
                // Python resolves the project first and exits **2** if
                // it does not match — not 1. That is the one place in
                // the port where a fuzzy-resolution failure is not
                // `USAGE`, so it cannot go through `resolve_project`,
                // whose error carries 1. `where`'s docstring says "2 if
                // the DB has no current row for the path" and its code
                // returns 2 for the unresolved project as well, which
                // is a conflation rather than a documented choice —
                // reproduced, because a script branching on 2 sees both.
                //
                // Ambiguity collapses into the same 2, because
                // `fuzzy_match_project` returns `None` for it and
                // `where` cannot tell the two apart. Python prints its
                // candidate list on *stdout* before that and this port
                // writes its own on stderr, which is the stderr gap the
                // README already records for every other command — it
                // only becomes visible here for a pattern matching more
                // than one project, which no sweep row uses.
                // The whole row rather than the slug: resolution needs
                // the project id, and Python's `SyncManager(slug)`
                // re-reads `id` and `repo_url` for the same reason.
                let found = match templedb_db::match_project(&store, &project)? {
                    ProjectLookup::Found(p) => p,
                    ProjectLookup::NotFound => {
                        eprintln!("Project '{project}' not found");
                        return Ok(templedb_db::NOT_FOUND);
                    }
                    ProjectLookup::Ambiguous(_) => {
                        // Delegated so the ● / ○ listing stays in one
                        // place; only the status differs here.
                        let e = resolve_project(&store, &project).unwrap_err();
                        eprintln!("Error: {e:#}");
                        return Ok(templedb_db::NOT_FOUND);
                    }
                };
                let slug = found.slug.clone();

                // `HOME` rather than a home-directory crate: Python
                // builds every probe from `Path.home()`, and the two
                // must agree about what home is or they probe different
                // trees and report drift that is not there.
                let home = std::env::var("HOME").context("HOME is not set")?;
                let home = std::path::Path::new(&home);

                let db_hash = templedb_db::current_hash(&store, &slug, &path)?;

                // Resolve both mirror roots from the `checkouts` table,
                // which is what Python does and what the hardcoded
                // paths this replaces got wrong in both directions —
                // see `where_view`'s decision 1.
                //
                // Resolved ONCE per purpose and with the row list read
                // once, which is also Python's instruction to its own
                // callers ("Resolve ONCE per command through this").
                // Here the reason is weaker but real: `active` is a
                // query and `is_dir` is a `stat` per row, and the two
                // purposes share both.
                let rows = templedb_db::active_checkouts(&store, found.id)?;
                let session =
                    templedb_db::current_checkout_session(&store, &|k| std::env::var(k).ok())?;
                let owners: Vec<i64> = rows.iter().filter_map(|c| c.session_id).collect();
                let live = templedb_db::live_sessions(&store, &owners)?;
                let repo_url = templedb_db::repo_url(&store, found.id)?;
                let is_dir = |p: &str| std::path::Path::new(p).is_dir();
                let is_live = |s: i64| live.contains(&s);
                let root = |purpose| {
                    templedb_db::checkout_root(
                        &rows,
                        repo_url.as_deref(),
                        purpose,
                        session,
                        &is_dir,
                        &is_live,
                    )
                };
                let build_root = root(templedb_db::Purpose::Build);
                let edit_root = root(templedb_db::Purpose::Edit);

                let mut mirrors = where_view::templedb_mirrors(
                    home,
                    &slug,
                    &path,
                    build_root.as_deref(),
                    edit_root.as_deref(),
                );
                mirrors.extend(where_view::overlay_mirrors(home, &path));

                // Probed only when there is something to compare
                // against: Python returns before the probe loop when
                // the DB has no current row, so an absent file never
                // gets its mirrors hashed.
                let probed: Vec<_> = match db_hash.as_deref() {
                    None => Vec::new(),
                    Some(h) => mirrors
                        .iter()
                        .filter_map(|m| where_view::probe(m, h).map(|f| (m.label.clone(), f)))
                        .collect(),
                };

                let (lines, code) = where_view::render(&slug, &path, db_hash.as_deref(), &probed);
                for l in &lines {
                    println!("{l}");
                }
                return Ok(code);
            }
        },
        Command::Entity { command } => match command {
            EntityCmd::Explore { entity } => {
                let (kind, eref) = split_entity(&entity)?;
                let e = templedb_db::explore(&store, kind, eref)?;
                println!("● {kind}/{eref}");
                println!(
                    "  label:            {}",
                    e.label.as_deref().unwrap_or("(none)")
                );
                println!("  source_authority: {}", e.source_authority);
                println!("  observed_at:      {}", e.observed_at);
                // Empty sections are omitted rather than printed as
                // "(0)", matching the Python command this replaces.
                if !e.outbound.is_empty() {
                    println!();
                    println!("  outbound ({}):", e.outbound.len());
                    for edge in &e.outbound {
                        println!(
                            "    -[{}]→ {}{}",
                            edge.relkind,
                            edge.peer_address(),
                            label_suffix(edge.peer_label.as_deref())
                        );
                    }
                }
                if !e.inbound.is_empty() {
                    println!();
                    println!("  inbound ({}):", e.inbound.len());
                    for edge in &e.inbound {
                        println!(
                            "    {}{} -[{}]→",
                            edge.peer_address(),
                            label_suffix(edge.peer_label.as_deref()),
                            edge.relkind
                        );
                    }
                }
            }
            EntityCmd::Trace {
                entity,
                depth,
                direction,
                via,
                limit,
            } => {
                let (kind, eref) = split_entity(&entity)?;
                let via = parse_via(via.as_deref());
                let traced =
                    templedb_db::trace(&store, kind, eref, depth, direction.into(), &via, limit)?;
                let hits = &traced.hits;
                println!("● {kind}/{eref}");
                for hit in hits {
                    let arrow = match hit.edge.direction {
                        Direction::In => format!("←[{}]─", hit.edge.relkind),
                        // `Both` is a traversal mode, never an edge's
                        // own direction; `fetch_edges` always tags a row
                        // with the end it came from.
                        _ => format!("─[{}]→", hit.edge.relkind),
                    };
                    println!(
                        "{}{arrow} {}{}",
                        "  ".repeat(hit.depth),
                        hit.edge.peer_address(),
                        label_suffix(hit.edge.peer_label.as_deref())
                    );
                }
                if hits.is_empty() {
                    eprintln!("no edges within depth {depth}");
                }
                // Say so when the cap bit. A capped list that looks
                // complete is the defect that made `entity paths`
                // report "no path" across an existing edge; the cap
                // itself is fine here, because in `trace` the fan-out
                // *is* the output.
                if traced.truncated {
                    println!();
                    println!(
                        "  (fan-out capped at {limit} per node and direction; \
                         some edges not shown — raise with --limit N)"
                    );
                }
            }
            EntityCmd::Search { query, kind, limit } => {
                let hits = templedb_db::search_entities(&store, &query, kind.as_deref(), limit)?;
                if hits.is_empty() {
                    // On stdout, exit 0: "nothing matched" is an answer.
                    let in_kind = kind.map_or_else(String::new, |k| format!(" in kind={k}"));
                    println!("(no entities match {}{in_kind})", py_repr(&query));
                    return Ok(0);
                }
                for h in &hits {
                    // Column widths, and the trailing spaces a short
                    // source_authority leaves behind when there is no
                    // label, are the Python format string's — this is
                    // diffed, not eyeballed.
                    println!(
                        "  {:<14} {:<50} {:<12}{}",
                        h.kind,
                        h.external_ref.as_deref().unwrap_or("(none)"),
                        h.source_authority,
                        label_suffix(h.label.as_deref())
                    );
                }
            }
            EntityCmd::Paths {
                from,
                to,
                max_depth,
                direction,
                via,
            } => {
                let (from_kind, from_ref) = split_entity(&from)?;
                let (to_kind, to_ref) = split_entity(&to)?;
                let via = parse_via(via.as_deref());
                let found = templedb_db::shortest_path(
                    &store,
                    (from_kind, from_ref),
                    (to_kind, to_ref),
                    max_depth,
                    direction.into(),
                    &via,
                )?;
                match found {
                    templedb_db::PathResult::SameEntity => {
                        println!("● {from_kind}/{from_ref}  (source == target)");
                    }
                    templedb_db::PathResult::NoPath => {
                        println!(
                            "(no path within depth {max_depth} between \
                             {from_kind}/{from_ref} and {to_kind}/{to_ref})"
                        );
                        return Ok(templedb_db::NO_PATH);
                    }
                    templedb_db::PathResult::Found { source_label, hops } => {
                        println!(
                            "● {from_kind}/{from_ref}{}",
                            label_suffix(source_label.as_deref())
                        );
                        for (i, hop) in hops.iter().enumerate() {
                            // A bare `→`/`←` here, not the `─[kind]→`
                            // box-drawing `trace` uses: the two commands
                            // format arrows differently and this one is
                            // `trace`'s simpler form.
                            let arrow = match hop.direction {
                                Direction::In => '←',
                                _ => '→',
                            };
                            println!(
                                "{}{arrow}[{}] {}{}",
                                "  ".repeat(i + 1),
                                hop.relkind,
                                hop.peer_address(),
                                label_suffix(hop.peer_label.as_deref())
                            );
                        }
                        println!();
                        println!("  Path length: {} hops", hops.len());
                    }
                }
            }
            EntityCmd::Stats => {
                // Python's layout, which this command did not previously
                // reproduce: a `kind: total` line, then the kinds
                // indented two spaces and padded to 20 with the count
                // unpadded after one space. The old form right-aligned
                // the counts in a wider column and added a `TOTAL` row,
                // which reads better and is not what the other binary
                // prints.
                let stats = templedb_db::entity_stats(&store)?;
                let section = |head: &str, total: i64, rows: &[(String, i64)]| {
                    println!("{head}{total}");
                    for (kind, n) in rows {
                        println!("  {kind:<20} {n}");
                    }
                };
                section("entities:  ", stats.entity_total(), &stats.entities);
                println!();
                // The second section was missing entirely. The counts
                // the command did print all agreed with Python; the
                // nineteen relation kinds were simply absent.
                section("relations: ", stats.relation_total(), &stats.relations);
            }
            EntityCmd::DeadImports { slug, limit } => {
                // Not fuzzy-resolved, unlike every other project
                // argument: this one is matched as a prefix of
                // `external_ref` rather than looked up in `projects`,
                // and the Python command takes it raw too. Resolving it
                // here would make `--slug` accept names that the filter
                // then cannot match.
                let hits = templedb_db::dead_imports(&store, slug.as_deref(), limit)?;
                if hits.is_empty() {
                    let scope = slug.map_or(String::new(), |s| format!(" for '{s}'"));
                    println!("No candidate dead imports found{scope}.");
                    return Ok(0);
                }
                println!("Found {} candidate dead import(s):", hits.len());
                println!();
                // Grouped under the importing file, which is only
                // correct because the query orders by `from_ref` — the
                // run of rows per source file is contiguous.
                let mut current = None;
                for h in &hits {
                    if current.as_deref() != Some(h.from_ref.as_str()) {
                        println!("● {}", h.from_ref);
                        current = Some(h.from_ref.clone());
                    }
                    println!("    imports (no call resolved) → {}", h.to_ref);
                }
                println!();
                println!("Note: side-effect imports (plugin/monkeypatch) may be legit.");
            }
            EntityCmd::Observations { entity, limit } => {
                let (kind, eref) = split_entity(&entity)?;
                let rows = templedb_db::observations(&store, kind, eref, limit)?;
                if rows.is_empty() {
                    println!("(no archived observations for {kind}/{eref})");
                    println!(
                        "  This means the entity hasn't been mutated since \
                         migration 097 landed, or doesn't exist."
                    );
                    return Ok(0);
                }
                println!("Observation history for {kind}/{eref}:");
                for r in &rows {
                    // Python renders the before/after with `!r`, so a
                    // present value prints single-quoted and a NULL
                    // prints as the bare word `None`. Reproduced rather
                    // than improved: these lines are diffed against it.
                    let mut line = format!("  #{:<5} {}", r.id, r.observed_at);
                    if r.label_changed() {
                        line.push_str(&format!(
                            " label: {} → {}",
                            py_repr_opt(r.prior_label.as_deref()),
                            py_repr_opt(r.label.as_deref())
                        ));
                    }
                    if r.authority_changed() {
                        line.push_str(&format!(
                            " authority: {} → {}",
                            py_repr_opt(r.prior_source_authority.as_deref()),
                            py_repr_opt(r.source_authority.as_deref())
                        ));
                    }
                    println!("{line}");
                }
            }
        },
        Command::Search { command } => match command {
            SearchCmd::Content {
                pattern,
                project,
                limit,
            } => {
                // The project filter resolves fuzzily too, but an
                // unmatched name here must not silently widen the search
                // to every project.
                let slug = match project {
                    Some(p) => Some(resolve_project(&store, &p)?),
                    None => None,
                };
                let (hits, dropped) =
                    templedb_db::search_content(&store, &pattern, slug.as_deref(), limit)?;
                if hits.is_empty() {
                    eprintln!("no files containing {pattern:?}");
                    return Ok(0);
                }
                for h in &hits {
                    println!("{}  {}", h.project, h.path);
                    // Snippets are one line so output stays greppable.
                    println!("    {}", h.snippet.replace('\n', " "));
                }
                eprintln!("\n{} hit(s)", hits.len());
                // A search that read only part of the corpus must say
                // so. Presenting a truncated ranking as a complete one
                // is the same failure as the fan-out cap in `entity
                // paths`: it turns "no match" into a wrong answer with
                // nothing in the output to question.
                if dropped > 0 {
                    eprintln!(
                        "warning: {dropped} further candidate file(s) were not read or ranked"
                    );
                }
            }
            SearchCmd::Files { pattern, project } => {
                // Not `resolve_project`. Python's `search` module is the
                // one command family that never calls the fuzzy matcher
                // — measured, not assumed: `search files README -p
                // templed` prints "No files found matching 'README'" and
                // exits 0 where every `file`/`vcs`/`intent` command
                // would have resolved it. Resolving here would turn a
                // zero-row answer into an error, which the sweep sees as
                // a difference in both stdout and exit status.
                let (hits, truncated) =
                    templedb_db::search_files(&store, &pattern, project.as_deref())?;
                if hits.is_empty() {
                    println!("No files found matching '{pattern}'");
                    return Ok(0);
                }
                // Python runs two queries with two column lists; the
                // difference is only which columns are printed, so this
                // is one query and two projections. `lines_of_code` is
                // selected and discarded in the unfiltered form — the
                // fifth select-and-discard in this port, and the first
                // where the discarded column is printed by the *other*
                // form of the same command.
                let (columns, rows): (&[&str], Vec<Vec<String>>) = if project.is_some() {
                    (
                        &["file_path", "type_name", "lines_of_code"],
                        hits.iter()
                            .map(|h| {
                                vec![
                                    h.path.clone(),
                                    h.type_name.clone(),
                                    table::py_str_opt_i64(h.lines_of_code),
                                ]
                            })
                            .collect(),
                    )
                } else {
                    (
                        &["project_slug", "file_path", "type_name"],
                        hits.iter()
                            .map(|h| vec![h.project.clone(), h.path.clone(), h.type_name.clone()])
                            .collect(),
                    )
                };
                println!(
                    "{}",
                    table::format_table(
                        &rows,
                        columns,
                        &format!("Files matching '{pattern}' ({} results)", hits.len()),
                    )
                );
                // Python prints `(100 results)` and nothing else when
                // the cap bites, which reads as "there are a hundred".
                // Saying so on stderr keeps stdout identical and is the
                // same call `search content` makes about its own
                // candidate cap.
                if truncated {
                    eprintln!(
                        "warning: more files match; the list is capped at {} as Python's is",
                        hits.len()
                    );
                }
            }
        },
        Command::Source { command } => match command {
            SourceCmd::Snapshot {
                project,
                file_path,
                rev,
                meta,
            } => {
                let slug = resolve_project(&store, &project)?;
                let snap = templedb_db::snapshot(&store, &slug, &file_path, rev.as_deref())?;
                if meta {
                    println!("project:           {}", snap.project_slug);
                    println!("path:              {}", snap.file_path);
                    println!("revision:          {}", snap.revision);
                    // All three are nullable and all three are reachable
                    // from live data: a `change_type = 'deleted'` row
                    // has no hash, no size and no line count. Python
                    // interpolates them raw, so each prints `None`.
                    println!(
                        "content_hash:      {}",
                        py_str_opt(snap.content_hash.as_deref())
                    );
                    println!(
                        "size:              {} bytes",
                        py_str_opt(snap.file_size_bytes)
                    );
                    println!("lines:             {}", py_str_opt(snap.line_count));
                    println!("observed_at:       {}", snap.observed_at);
                    println!("source_authority:  {}", snap.source_authority);
                    return Ok(0);
                }
                // Content goes to stdout as bytes. A snapshot can be a
                // PNG, and `println!` would both mangle it and append a
                // newline the stored blob does not have.
                let mut out = std::io::stdout().lock();
                match templedb_db::snapshot_body(&store, &snap)? {
                    SnapshotBody::Text(t) => out.write_all(t.as_bytes())?,
                    SnapshotBody::Binary(b) => out.write_all(&b)?,
                }
                out.flush()?;
            }
            SourceCmd::Revisions { project, file_path } => {
                let slug = resolve_project(&store, &project)?;
                let revs = templedb_db::revisions(&store, &slug, &file_path)?;
                println!("{slug}/{file_path}");
                println!("{} snapshot(s), newest first:", revs.len());
                println!();
                for r in &revs {
                    println!("{}", revision_line(r));
                }
            }
        },
        Command::Vcs { command } => match command {
            VcsCmd::Log {
                project,
                n,
                branch,
                all_sessions,
                published,
                // Accepted and intentionally unread, as Python's is:
                // the session view is the default on both sides, so the
                // flag names what happens anyway. Bound rather than
                // matched with `..` so that adding a fourth view cannot
                // quietly leave this one unhandled.
                session: _,
            } => {
                let p = resolve_project_row(&store, &project)?;
                // Before the queries, and at the edge: the identity is
                // an argument from here down, exactly as the clock is.
                // `None` for the pin because `vcs log` has no
                // `--session <id>` — Python's is a boolean no-op on this
                // command, and the `ID|NAME` form lives on the staging
                // commands.
                let session = templedb_db::resolve_session(store.as_ref(), None)?;
                if let Some(name) = templedb_db::ignored_name(session) {
                    eprintln!(
                        "warning: {} is set to {name:?} and this port does not resolve a \
                         session by name, so the view below may differ from Python's. \
                         Set {} to an id to pin one.",
                        templedb_db::SESSION_NAME_ENV,
                        templedb_db::SESSION_ID_ENV
                    );
                }
                let visibility = match (all_sessions, published, session) {
                    (true, _, _) => templedb_db::Visibility::All,
                    // `--published` is the only way to ask for the
                    // narrow view once a session resolves.
                    (_, true, _) => templedb_db::Visibility::Published,
                    (_, _, Some(s)) => templedb_db::Visibility::Session(s),
                    // Python invents a session here. Its view is then
                    // the published set anyway — a fresh session owns
                    // nothing — so the rows agree and only the header
                    // label differs.
                    (_, _, None) => templedb_db::Visibility::Published,
                };
                let commits =
                    templedb_db::commit_log(&store, p.id, branch.as_deref(), visibility, n)?;

                println!();
                println!("Commit log for {} [{}]", p.slug, visibility.label());
                // The advisory, which Python prints only under the
                // published view and only when the count is non-zero.
                // It needs a session, and when none resolved there is
                // nothing to count: Python would have created one, and a
                // session created a moment ago owns no commits, so the
                // line is absent on both sides for the same reason.
                if visibility == templedb_db::Visibility::Published {
                    if let Some(s) = session {
                        let pending = templedb_db::pending_in_session(store.as_ref(), p.id, s)?;
                        if pending > 0 {
                            println!(
                                "  * {pending} pending commit(s) in your session \
                                 — re-run with --session to include, or `templedb publish` \
                                 to fast-forward onto shared HEAD."
                            );
                        }
                    }
                }
                println!();
                if commits.is_empty() {
                    println!("No commits found");
                    return Ok(0);
                }
                for c in &commits {
                    // Python appends this marker from `session_id`
                    // alone; under `--all-sessions` it is how another
                    // session's commits are distinguishable.
                    let marker = if c.session_id.is_some() {
                        " (unpublished)"
                    } else {
                        ""
                    };
                    println!("commit {}{marker}", c.commit_hash);
                    println!("Branch: {}", c.branch_name);
                    println!("Author: {}", c.author);
                    println!("Date:   {}", c.commit_timestamp);
                    println!();
                    println!("    {}", c.commit_message);
                    println!();
                }
            }
            VcsCmd::CommitStaged {
                project,
                message,
                message_file,
                branch,
                author,
                session: pin,
            } => {
                // The message first, before anything is opened: `-F -`
                // reads stdin, and a refusal on it must not leave a
                // connection or a transaction waiting. Same ordering
                // `file set` uses for its content.
                let message = resolve_commit_message(message.as_deref(), message_file.as_deref())?;

                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                let author = commit_author(author.as_deref());
                let mut tx = writer.transaction()?;
                // Against the write target, inside the write
                // transaction, for the reason `file set`'s is: the
                // reader and the writer point at different databases,
                // and a liveness claim about the wrong server is a
                // nasty bug.
                let session = templedb_db::resolve_session_tx(&mut tx, pin.as_deref())?;
                let Some(session) = session else {
                    // The documented cost of *resolve never, accept
                    // always*. Python would guess from the process tree
                    // and create a row rather than refuse.
                    let scattered = match templedb_db::ignored_name(None) {
                        Some(name) => format!(
                            "\n{} is set to {name:?}, which this port does not resolve — \
                             the name path creates the session when none matches. Pass the \
                             id instead.",
                            templedb_db::SESSION_NAME_ENV
                        ),
                        None => String::new(),
                    };
                    anyhow::bail!(
                        "`vcs commit-staged` needs an explicit session: set {} or pass \
                         --session <id>.\n\
                         Staged rows belong to a session, so there is nothing to commit \
                         without one. This port will not guess — Python resolves through \
                         the process tree and creates a row when that fails.\n\
                         Pick a live id from `templedb vcs session list --active`.{scattered}",
                        templedb_db::SESSION_ID_ENV
                    );
                };
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let outcome = templedb_db::commit_staged(
                    &mut tx,
                    &clock,
                    project_id,
                    &project,
                    &templedb_db::CommitOptions {
                        message: &message,
                        author: &author,
                        branch: branch.as_deref(),
                        session,
                    },
                );
                // `?` before the commit, as `file set` does: on the
                // error path `tx` drops un-committed and `postgres`
                // rolls it back. Every refusal in this command —
                // HEAD_RACE, guard 3, a missing blob — has to leave
                // nothing behind, and the ordering here is what makes
                // that true rather than hoped.
                let outcome = outcome?;
                tx.commit()?;

                // stdout is the commit id and nothing else, so a script
                // can chain on it — the same division `file set` and
                // `file ls` use. Python prints seven labelled lines;
                // this is not a diffable command, so the stdout
                // contract is chosen for callers rather than copied.
                println!("{}", outcome.hash);
                let mut notes = vec![format!("{} file(s)", outcome.files)];
                if outcome.deletions > 0 {
                    notes.push(format!("{} deleted", outcome.deletions));
                }
                notes.push(match outcome.previous_head {
                    Some(h) => format!("parent #{h}"),
                    None => "root commit".to_string(),
                });
                if outcome.reaped > 0 {
                    notes.push(format!("{} orphan(s) reaped", outcome.reaped));
                }
                eprintln!(
                    "{project} [{}] #{}: {}",
                    outcome.branch_name,
                    outcome.commit_id,
                    notes.join(", ")
                );
                if outcome.other_sessions_staged > 0 {
                    // Python prints this too, and it is the footgun
                    // worth repeating: an agent that stages in one tool
                    // call and commits in another is in two sessions.
                    eprintln!(
                        "  note: {} file(s) staged in other sessions were not included",
                        outcome.other_sessions_staged
                    );
                }
            }
            VcsCmd::Branch { project } => {
                let p = resolve_project_row(&store, &project)?;
                let branches = templedb_db::branches(&store, p.id)?;
                if branches.is_empty() {
                    println!("No branches found");
                    return Ok(0);
                }
                for b in &branches {
                    let marker = if b.is_active { "* " } else { "  " };
                    let default_tag = if b.is_default { " (default)" } else { "" };
                    // Python appends the head message only when it is
                    // truthy, so an empty message renders as nothing
                    // rather than a bare dash.
                    let msg = match b.last_message.as_deref() {
                        Some(m) if !m.is_empty() => format!(" - {m}"),
                        _ => String::new(),
                    };
                    println!(
                        "{marker}{}{default_tag}  [{} commits]{msg}",
                        b.branch_name, b.commit_count
                    );
                }
            }
            VcsCmd::Diff {
                project,
                file,
                commit1,
                commit2,
                side_by_side,
                no_color,
                staged,
            } => {
                let p = resolve_project_row(&store, &project)?;

                // The two working-set forms come first, because both
                // short-circuit before the file is resolved — Python
                // checks `--staged` before touching `args.file`, and
                // then treats a missing file as "the whole working
                // set" rather than as an error.
                if staged || file.is_none() {
                    let Some(branch_id) = templedb_db::active_branch(store.as_ref(), p.id)? else {
                        eprintln!("No active branch found");
                        return Ok(1);
                    };
                    return if staged {
                        diff_staged(store.as_ref(), &p, branch_id, side_by_side, no_color)
                    } else {
                        diff_working(store.as_ref(), &p, branch_id, side_by_side, no_color)
                    };
                }
                let file = file.expect("the None case returned above");

                let f = match templedb_db::match_file(&store, p.id, &file)? {
                    templedb_db::FileLookup::Found(f) => f,
                    templedb_db::FileLookup::NotFound => {
                        eprintln!("No file matches '{file}'");
                        eprintln!("Use: templedb-rs file ls {project} to see available files");
                        return Ok(1);
                    }
                    templedb_db::FileLookup::Ambiguous(list) => {
                        eprintln!("Multiple files match '{file}':");
                        for (path, score) in &list {
                            // ● above 0.8, ○ below — Python's threshold.
                            let marker = if *score > 0.8 { '●' } else { '○' };
                            eprintln!("  {marker} {path}");
                        }
                        eprintln!("Please specify exact file name");
                        return Ok(1);
                    }
                };

                // Which pair of texts to compare, and what to label
                // them. The three arms are Python's three forms.
                let (old, new, label1, label2) = match (&commit1, &commit2) {
                    (Some(c1), Some(c2)) => {
                        let a = templedb_db::content_at_commit(&store, p.id, &f.file_path, c1)?;
                        let b = templedb_db::content_at_commit(&store, p.id, &f.file_path, c2)?;
                        (a.content, b.content, a.label, b.label)
                    }
                    (Some(c1), None) => {
                        let a = templedb_db::content_at_commit(&store, p.id, &f.file_path, c1)?;
                        let b = templedb_db::current_content(&store, f.id)?;
                        // Python's early exit, before the None checks
                        // below: identical content prints a pointer at
                        // `vcs show` rather than an empty diff. The
                        // unbalanced parentheses are Python's too.
                        if let (Some(x), Some(y)) = (&a.content, &b) {
                            if x == y {
                                let short: String = c1.chars().take(8).collect();
                                println!("No differences");
                                println!("  ({short}'s content matches the current worktree.");
                                println!("   To see what {short} itself changed, use:");
                                println!("     templedb vcs show {project} {c1})");
                                return Ok(0);
                            }
                        }
                        (a.content, b, a.label, "current".to_string())
                    }
                    // `commit2` without `commit1` is unreachable:
                    // clap fills positionals in order.
                    (None, _) => {
                        let versions = templedb_db::content_versions(&store, f.id)?;
                        if versions.len() < 2 {
                            println!("Only one version exists, no diff available");
                            return Ok(0);
                        }
                        let (newer, older) = (&versions[0], &versions[1]);
                        (
                            templedb_db::version_text(&store, older)?,
                            templedb_db::version_text(&store, newer)?,
                            format!("version {}", older.version),
                            format!("version {} (current)", newer.version),
                        )
                    }
                };

                // Absent content is an error here, where `vcs show`
                // coerces it to empty. Both messages go to stderr, so
                // stdout stays comparable either way.
                let (Some(old), Some(new)) = (old, new) else {
                    eprintln!("Could not retrieve old version");
                    eprintln!("The content may have been deleted or corrupted");
                    return Ok(1);
                };

                let (old_lines, new_lines) =
                    (templedb_db::splitlines(&old), templedb_db::splitlines(&new));
                // Same labels and same `n` either way — Python switches
                // only the renderer, and the two take identical
                // arguments because they consume the same grouped
                // opcodes.
                let render = if side_by_side {
                    templedb_db::context_diff
                } else {
                    templedb_db::unified_diff
                };
                let diff = render(
                    &old_lines,
                    &new_lines,
                    &format!("{} ({label1})", f.file_path),
                    &format!("{} ({label2})", f.file_path),
                    3,
                );
                if diff.is_empty() {
                    println!("No differences");
                } else {
                    for line in &diff {
                        println!("{}", colorize_diff_line(line, !no_color));
                    }
                }
            }
            VcsCmd::Show {
                project,
                commit,
                no_diff,
                no_color,
            } => {
                let color = !no_color;
                let p = resolve_project_row(&store, &project)?;
                let Some((d, files)) = templedb_db::commit_detail(&store, p.id, &commit)? else {
                    eprintln!("Commit not found: {commit}");
                    eprintln!("Use: templedb-rs vcs log {project} to see available commits");
                    return Ok(1);
                };

                println!("commit {}", d.commit_hash);
                println!("Branch: {}", d.branch_name);
                println!("Author: {}", d.author);
                println!("Date:   {}", d.commit_timestamp);
                println!();
                println!("    {}", d.commit_message);
                println!();

                if !files.is_empty() {
                    println!("Changed files ({}):", files.len());
                    for f in &files {
                        println!(
                            "  {} {:<10} {} {}",
                            f.marker(),
                            f.change_type,
                            f.file_path,
                            f.size_info()
                        );
                    }
                }

                // Diff against the first parent, which is `git show`'s
                // semantics and Python's. A root commit has no parent
                // and is diffed against the empty string, so every line
                // reads as an addition.
                if !files.is_empty() && !no_diff {
                    use templedb_db::BlobKind;
                    let parent = templedb_db::parent_hash(&store, d.id)?;
                    for f in &files {
                        // `vcs show` discards the labels and builds its
                        // own `a/path (hash)` pair. It coerces absent
                        // content to the empty string, but only after
                        // the two checks below — without them "nothing
                        // changed", "it is binary" and "the bytes are
                        // gone" all printed `No differences`, which is
                        // right for one of the three. `vcs diff` treats
                        // the same absence as an error instead. Both are
                        // Python's.
                        let new = templedb_db::content_at_commit(
                            &store,
                            p.id,
                            &f.file_path,
                            &d.commit_hash,
                        )?;
                        // A root commit's old side is empty *text*, not
                        // absent content — so it diffs as a whole-file
                        // addition rather than tripping either check.
                        let old = match &parent {
                            Some(h) => {
                                templedb_db::content_at_commit(&store, p.id, &f.file_path, h)?
                            }
                            None => templedb_db::CommitContent {
                                content: Some(String::new()),
                                label: String::new(),
                                kind: BlobKind::Text,
                            },
                        };

                        let kinds = [old.kind, new.kind];
                        if kinds.contains(&BlobKind::Binary) {
                            println!();
                            println!("  {}: binary file, no textual diff", f.file_path);
                            continue;
                        }
                        // The second disjunct is Python's and is not
                        // redundant: two `NotFound` sides are neither
                        // missing nor binary, and would otherwise reach
                        // the diff as empty-against-empty and print
                        // `No differences`.
                        if kinds.contains(&BlobKind::Missing)
                            || (old.content.is_none() && new.content.is_none())
                        {
                            println!();
                            println!(
                                "  {}: content not stored for this revision \
                                 (hash present, blob absent) — cannot diff",
                                f.file_path
                            );
                            continue;
                        }

                        let (old, new) = (
                            old.content.unwrap_or_default(),
                            new.content.unwrap_or_default(),
                        );

                        println!();
                        // `∅` for a root commit, matching Python.
                        let parent_label =
                            parent.as_deref().map_or("∅", |h| &h[..8.min(h.len())]);
                        let labels = (
                            format!("a/{} ({})", f.file_path, parent_label),
                            format!(
                                "b/{} ({})",
                                f.file_path,
                                &d.commit_hash[..8.min(d.commit_hash.len())]
                            ),
                        );
                        let (old_lines, new_lines) =
                            (templedb_db::splitlines(&old), templedb_db::splitlines(&new));
                        let diff = templedb_db::unified_diff(
                            &old_lines, &new_lines, &labels.0, &labels.1, 3,
                        );
                        if diff.is_empty() {
                            println!("No differences");
                            continue;
                        }
                        for line in &diff {
                            println!("{}", colorize_diff_line(line, color));
                        }
                    }
                }
            }
        },
        Command::Doctor { command } => match command {
            DoctorCmd::History {
                check,
                violated_only,
                limit,
            } => {
                let runs = templedb_db::check_history(
                    store.as_ref(),
                    &templedb_db::HistoryFilter {
                        check: check.as_deref(),
                        violated_only,
                    },
                    limit,
                )?;
                // Python prints this and returns 0 — an empty table is
                // not an error, it is a database nobody has run
                // `doctor entities` against. Reproduced including the
                // status, because a script distinguishing "no history"
                // from "the query failed" would read the wrong thing
                // from a non-zero exit.
                if runs.is_empty() {
                    println!("{}", templedb_db::NO_HISTORY);
                } else {
                    for r in &runs {
                        println!("{}", templedb_db::render_check_run(r));
                    }
                }
            }
        },
        Command::Intent { command } => match command {
            IntentCmd::Create {
                project,
                file_path,
                content,
                from_file,
                base_rev,
                message,
            } => {
                // The content first, before anything is opened — the
                // order `file set` takes and for its reason: a refusal
                // on stdin must not leave a connection waiting on it.
                //
                // Python's three sources in Python's order:
                // `--content` if it is not None (so `-c ''` counts),
                // then `--from-file`, then stdin when stdin is not a
                // tty. All three are text; there is no binary branch
                // over there and so none here. See `create_intent`.
                let content: String = match (content, from_file) {
                    (Some(c), _) => c,
                    (None, Some(path)) => {
                        // Python's message, which names the path and
                        // the OS error, and exits 1.
                        std::fs::read_to_string(&path)
                            .with_context(|| format!("Could not read {path}"))?
                    }
                    (None, None) => {
                        use std::io::{IsTerminal, Read};
                        if std::io::stdin().is_terminal() {
                            // Python's `if not sys.stdin.isatty()`
                            // guard, which is what keeps an
                            // interactive invocation from hanging on a
                            // read nobody is feeding.
                            anyhow::bail!("Need content via --content, --from-file, or stdin");
                        }
                        let mut buf = Vec::new();
                        std::io::stdin()
                            .read_to_end(&mut buf)
                            .map_err(|e| anyhow::anyhow!("reading stdin: {e}"))?;
                        // Python's `sys.stdin.read()` raises on
                        // undecodable bytes and the CLI turns that into
                        // exit 1 — measured on a copy: "Error: 'utf-8'
                        // codec can't decode byte 0x84 in position 0".
                        // So this refuses too, rather than taking
                        // `file set`'s binary branch: `intent create`
                        // has no `binary_asset` path upstream, and
                        // inventing one would store a blob Python
                        // cannot produce under a hash an unported
                        // `apply` is supposed to read.
                        String::from_utf8(buf).map_err(|e| {
                            anyhow::anyhow!(
                                "stdin is not valid UTF-8: {e}.\n\
                                 `intent create` is text-only in both \
                                 implementations — Python's reads stdin through \
                                 a text decode and exits 1 here too. \
                                 `file set` takes binary content."
                            )
                        })?
                    }
                };
                // Empty content is *accepted*, which is Python's and is
                // the opposite of `file set`'s refusal one command
                // over. Not tidied: an empty proposal is a proposal to
                // truncate, `intent create` records rather than
                // applies, and the two commands disagreeing about an
                // empty pipe is upstream's inconsistency to own. In the
                // README's *Known gaps*.

                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                // `TEMPLEDB_AUTHOR`, then `USER`, then NULL — the same
                // chain `file set`'s intent reads, because it is the
                // same column and Python fills it from the same two
                // variables in both paths.
                let author = edit_author();
                let mut tx = writer.transaction()?;
                // No `--session` flag, because Python has none here:
                // the only input is `TEMPLEDB_SESSION_ID`, so the pin
                // is `None` and the resolver reads the variable. It
                // also checks liveness, where Python's `int()` does
                // not — the divergence `record_intent` documents.
                // No `ignored_name` warning, unlike `vcs log`'s, and
                // that is the measured call rather than an omission:
                // `TEMPLEDB_SESSION` changes nothing *on either side*
                // here, because Python's `_current_session_id` reads
                // `TEMPLEDB_SESSION_ID` and stops. The warning exists
                // for the case where ignoring the name makes this port
                // answer differently from Python, and this is not one.
                let session = templedb_db::resolve_session_tx(&mut tx, None)?;
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let created = templedb_db::create_intent(
                    &mut tx,
                    &clock,
                    project_id,
                    &file_path,
                    &content,
                    &templedb_db::CreateOptions {
                        base_rev: base_rev.as_deref(),
                        message: message.as_deref(),
                        author: author.as_deref(),
                        session,
                    },
                )?;
                tx.commit()?;
                // The slug printed is the one the caller typed, where
                // Python prints the one it resolved. Equal by
                // construction: the match is exact, so there is one
                // string.
                for line in templedb_db::created_lines(&created, &project) {
                    println!("{line}");
                }
            }
            IntentCmd::List {
                project,
                session,
                all_statuses,
                limit,
            } => {
                // Python fuzzy-matches `--project` and exits 1 when it
                // does not resolve, so the resolution is the edge's job
                // and the filter takes an id. `resolve_project_row`
                // rather than `resolve_project` because the query
                // filters on `project_id`, not on the slug — the join
                // supplies the slug for printing.
                let project_id = match project {
                    Some(p) => Some(resolve_project_row(store.as_ref(), &p)?.id),
                    None => None,
                };
                let rows = templedb_db::list_intents(
                    store.as_ref(),
                    &templedb_db::IntentFilter {
                        project_id,
                        session_id: session,
                        all_statuses,
                    },
                    limit,
                )?;
                // Python prints this and returns 0. An empty result is
                // the answer to "what is outstanding" on a database
                // where nothing is, which is the healthy state — so a
                // non-zero status here would make the good case look
                // like a failure.
                if rows.is_empty() {
                    println!("{}", templedb_db::NO_INTENTS);
                } else {
                    for r in &rows {
                        println!("{}", templedb_db::render_intent(r));
                    }
                }
            }
            IntentCmd::Show { id } => {
                match templedb_db::show_intent(store.as_ref(), id)? {
                    Some(i) => {
                        for line in templedb_db::render_intent_full(&i) {
                            println!("{line}");
                        }
                    }
                    // Exit 1, not `Coded::not_found`'s 2 — and that is
                    // Python's, measured rather than assumed. The read
                    // commands that return 2 for a missing thing
                    // (`source snapshot`, `entity explore`) all route
                    // through a helper that does; `intent show` is a
                    // bare `logger.error(...); return 1`. Reproducing
                    // the inconsistency is the point of step 12: the
                    // contract is Python's status, not a tidy one.
                    None => anyhow::bail!("Intent #{id} not found"),
                }
            }
            IntentCmd::Apply { id, no_rebase } => {
                // Read and discarded. The refusal it selects is this
                // port's behaviour with or without it, so there is
                // nothing to branch on — and the binding is named
                // rather than `_` because the flag is not inert in
                // Python and a future rebase would branch here.
                let _ = no_rebase;
                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                let mut tx = writer.transaction()?;
                match templedb_db::apply_intent(&mut tx, &clock, id)? {
                    Ok(applied) => {
                        tx.commit()?;
                        for line in templedb_db::applied_lines(&applied) {
                            println!("{line}");
                        }
                    }
                    // Every refusal below drops `tx` without
                    // committing, which rolls back the status claim
                    // `apply_intent` made in its first statement. That
                    // is the point of claiming first: the intent is
                    // still `'proposed'` afterwards without a second
                    // statement to put it back.
                    Err(templedb_db::ApplyRefusal::NotFound) => {
                        // Exit 1, which is `intent show`'s and
                        // `intent cancel`'s status for the same miss.
                        anyhow::bail!("Intent #{id} not found");
                    }
                    Err(templedb_db::ApplyRefusal::NotProposed(status)) => {
                        // Exit 2, and the message names the status —
                        // the part that tells "already applied" from
                        // "cancelled".
                        return Err(anyhow::Error::new(templedb_db::Coded::conflict(format!(
                            "Intent #{id} has status '{status}' (only 'proposed' can be applied)"
                        ))));
                    }
                    Err(templedb_db::ApplyRefusal::PathNotRegistered(path)) => {
                        // Exit 3. Python's message points at `file set`
                        // and so does this: `intent create` accepts a
                        // path that is not registered, deliberately on
                        // both sides, so this is where that choice is
                        // paid for.
                        return Err(anyhow::Error::new(templedb_db::Coded::refused(format!(
                            "file_path '{path}' not registered in project — create the file \
                             first via `templedb file set`"
                        ))));
                    }
                    Err(templedb_db::ApplyRefusal::StaleBase {
                        file_path,
                        base,
                        current,
                    }) => {
                        // Exit 4. Python's status and Python's facts;
                        // the wording and the last two lines are this
                        // port's, because the reason differs — over
                        // there this is what `--no-rebase` asked for,
                        // here it is what the command does.
                        return Err(anyhow::Error::new(templedb_db::Coded::stale_base(format!(
                            "Intent #{id} was written against {} but {file_path} now holds \
                             {}; this port refuses a moved base rather than merging onto \
                             it.\n\
                             \x20 Python would three-way merge here, and the engine it uses \
                             is not a three-way merge — measured, it agrees with `git \
                             merge-file --diff3` on 3 of 11 cases and four of its clean \
                             results write content neither side held.\n\
                             \x20 Re-propose against what landed: `templedb-rs file cat \
                             <slug> {file_path}`, then `templedb-rs intent create`.",
                            // Twelve, like Python's `base[:12]` — and
                            // `min` because `base_revision` is
                            // whatever `--base-rev` was given and
                            // nothing validates its length.
                            &base[..12.min(base.len())],
                            &current[..12.min(current.len())],
                        ))));
                    }
                }
            }
            IntentCmd::Cancel { id } => {
                let mut writer = templedb_db::PgWriter::open()?;
                let clock = templedb_db::Clock::from_env()?;
                let mut tx = writer.transaction()?;
                match templedb_db::cancel_intent(&mut tx, &clock, id)? {
                    Ok(()) => {
                        tx.commit()?;
                        println!("{}", templedb_db::cancelled_line(id));
                    }
                    // Both refusals leave the transaction unused rather
                    // than rolled back after a partial write: the CAS
                    // matched nothing, so there is nothing to undo.
                    Err(templedb_db::CancelRefusal::NotFound) => {
                        // Exit 1, which is `intent show`'s status for
                        // the same miss and Python's here too.
                        anyhow::bail!("Intent #{id} not found");
                    }
                    Err(templedb_db::CancelRefusal::NotProposed(status)) => {
                        // Exit 2. Python's, and the message names the
                        // status it found — which is the part a caller
                        // reads to tell "already cancelled" from
                        // "already applied".
                        return Err(anyhow::Error::new(templedb_db::Coded::conflict(format!(
                            "Intent #{id} is '{status}', only 'proposed' intents can be cancelled"
                        ))));
                    }
                }
            }
        },
        Command::State => {
            let state = templedb_db::capture(store.as_ref())?;
            // Fixed-width columns and a zero-padded hex digest, so the
            // output diffs line-for-line and a changed table is one
            // changed line rather than a reflowed block.
            for t in &state.tables {
                println!("{:<22} {:>9} {:032x}", t.table, t.rows, t.digest);
            }
        }
        Command::Summary { no_color } => {
            // Both inputs that are not rows, resolved here and nowhere
            // deeper. `Clock::from_env` honours `TEMPLEDB_RS_NOW`, which
            // is what makes a rendered pane reproducible at all.
            let clock = templedb_db::Clock::from_env()?;
            // The handoff address, NOT a VCS session id. `summary` has
            // no business resolving the latter: no pane reads
            // `vcs_sessions`, and passing a validated integer here
            // would silently change which notes the inbox matches. See
            // `handoff_identity`.
            let inbox = handoff_identity();
            // One call, so no pane can read a database that moved
            // between it and the pane above — the live file is being
            // written by the Python implementation while this runs.
            let facts = templedb_db::observe(store.as_ref(), &clock, &inbox)?;
            for line in summary_view::render(&facts, &clock, &inbox, !no_color) {
                println!("{line}");
            }
        }
        // Handled above, before the read store is opened.
        Command::Mirror { .. } => unreachable!("handled before open_ro"),
    }
    Ok(0)
}

/// The **handoff** identity: `TEMPLEDB_SESSION_ID`, or
/// `{hostname}-{getppid()}`.
///
/// # Not the VCS session, and the distinction is load-bearing
///
/// This returns a `String` address matched against
/// `handoff_notes.to_session`. [`resolve_session`](templedb_db::resolve_session)
/// returns an integer naming a live `vcs_sessions` row. They read the
/// same environment variable and they are not the same identity — one
/// is "who should see this note", the other is "which staging session
/// owns this commit" — and nothing converts between them.
///
/// **Reproduced, including the conflation.** Python has these as two
/// functions that happen to read one variable:
/// `handoff.py::_current_session_id` returns a string "so unregistered
/// shells still get a nameable sender", and
/// `VcsService.get_current_session` returns a row that must exist and
/// be live. So does this port, deliberately: `summary`'s handoff pane
/// is byte-identical with Python's and unifying the two would break
/// that to fix someone else's schema.
///
/// They are kept as separate functions with this note rather than
/// folded together, because folding them is the obvious cleanup and it
/// is wrong. The variable is one name for two things.
///
/// **What the conflation costs, in the live database.** One
/// `handoff_notes` row has `to_session = '328'` — a note addressed by
/// the integer scheme, from a sender using the `<hostname>-<ppid>` one.
/// 328 is not a `vcs_sessions` id at all, so reading that note requires
/// `TEMPLEDB_SESSION_ID=328`, which is the pin that makes `vcs log`
/// exit 1 for naming no live session. The note is unacked and
/// unreachable: the two meanings of its address are mutually exclusive.
/// That is a TempleDB bug rather than a port bug, and it is why this
/// comment is longer than the function.
///
/// # Why it is two system calls rather than a query
///
/// This is the third of `summary`'s three layers — environment. The
/// handoff inbox is scoped to it, which means the pane's answer depends
/// on *which shell* invoked the command: the same database, read from
/// two terminals, reports two different inbox counts. Resolving it at
/// the entry point does not remove that dependency, but it puts it
/// where a caller can see it and a test can fix it.
///
/// An exported-but-empty variable falls back, because Python's `or`
/// treats `''` as unset — the same reading `conn.rs` gives an empty
/// `TEMPLEDB_PATH`.
fn handoff_identity() -> String {
    match std::env::var("TEMPLEDB_SESSION_ID") {
        Ok(v) if !v.is_empty() => v,
        // SAFETY: `getppid` takes no arguments, cannot fail, and
        // touches no memory this program owns.
        _ => format!("{}-{}", hostname(), unsafe { libc::getppid() }),
    }
}

/// `edit_intents.author`: `TEMPLEDB_AUTHOR`, then `USER`, then NULL.
///
/// Python's order and Python's emptiness rule — its `or` chain treats
/// `''` as unset, so an exported-but-empty variable falls through rather
/// than recording an empty author. The same reading `conn.rs` gives an
/// empty `TEMPLEDB_PATH` and `session_id` gives an empty
/// `TEMPLEDB_SESSION_ID`.
///
/// Not `_resolve_author()`, which is what the *commit* path uses: that
/// one falls back to `git config user.name` and then the literal
/// `'unknown'`. `file set`'s intent reads these two variables and
/// nothing else, and reproducing the richer chain here would make the
/// column disagree with Python's on the machines where git is
/// configured and `USER` is not.
fn edit_author() -> Option<String> {
    for key in ["TEMPLEDB_AUTHOR", "USER"] {
        if let Ok(v) = std::env::var(key) {
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// The commit message, from `-m` or `-F`.
///
/// Python's `cli/commit_message.resolve_message`, whose docstring
/// records why `-F` exists at all: "`-m` cannot carry this codebase's
/// own prose. An inline message is a double-quoted shell string, so each
/// backtick span in it is command-substituted before argv is built:
/// commit 217092EC lost three quoted command names, printed three
/// 'command not found' lines on stderr, and reported success."
///
/// Neither option is `required` on its own, because the rule is that
/// *one* of the two must be present and `clap` cannot express that any
/// more than `argparse` can. So the check is here, where the error gets
/// to name both.
///
/// `rstrip('\n')` and nothing else, which is Python's: leading
/// whitespace can be deliberate and a body's indentation certainly is.
fn resolve_commit_message(inline: Option<&str>, path: Option<&str>) -> Result<String> {
    match (inline, path) {
        (Some(_), Some(_)) => anyhow::bail!("pass either -m or -F, not both."),
        (None, Some(path)) => {
            let text = if path == "-" {
                use std::io::Read;
                let mut buf = String::new();
                std::io::stdin()
                    .read_to_string(&mut buf)
                    .context("reading the commit message from stdin")?;
                buf
            } else {
                std::fs::read_to_string(path)
                    .with_context(|| format!("could not read the message file {path}"))?
            };
            let text = text.trim_end_matches('\n').to_string();
            if text.trim().is_empty() {
                anyhow::bail!("{path} holds no message.");
            }
            Ok(text)
        }
        (Some(inline), None) => {
            // Python tests `if not inline`, so `-m ''` falls through to
            // the same refusal as passing neither. Reproduced: an empty
            // message is a pipe that produced nothing, not a decision.
            if inline.is_empty() {
                anyhow::bail!(
                    "a commit message is required: -m \"…\" or -F <path> \
                     (-F for anything with backticks)."
                );
            }
            Ok(inline.to_string())
        }
        (None, None) => anyhow::bail!(
            "a commit message is required: -m \"…\" or -F <path> \
             (-F for anything with backticks)."
        ),
    }
}

/// `vcs_commits.author`: the flag, then `git config user.name`, then the
/// literal `unknown`.
///
/// Python's `_get_author`, and a *different* chain from
/// [`edit_author`]'s `TEMPLEDB_AUTHOR` / `USER` — which is also Python's
/// doing: `edit_intents.author` and `vcs_commits.author` are filled by
/// two unrelated code paths over there. Kept separate rather than
/// unified, because the sessions design document's closing note is about
/// exactly this hazard one table over: two identity schemes that "should
/// not share a resolver, and today they nearly do".
///
/// A subprocess at the edge, like the clock and the session: a query
/// module that shelled out could not be tested at a fixed value. Python
/// passes `timeout=5`; `std::process` has no timeout and none is added,
/// because `git config user.name` reads a local file and a hang there
/// needs an environment pathological enough to have bigger problems.
fn commit_author(explicit: Option<&str>) -> String {
    if let Some(a) = explicit {
        return a.to_string();
    }
    let from_git = std::process::Command::new("git")
        .args(["config", "user.name"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    // Python's fallback is the literal string, not NULL — and the column
    // is NOT NULL, so there is nowhere else to go.
    from_git.unwrap_or_else(|| "unknown".to_string())
}

/// `socket.gethostname()`.
///
/// Empty on failure rather than an error: Python raises here, but a
/// hostname this process cannot read would fail `summary` outright over
/// one line of one pane, and the inbox query it feeds returns zero rows
/// for an unmatched session either way.
fn hostname() -> String {
    // HOST_NAME_MAX is 64 on Linux. POSIX leaves truncation unspecified
    // when the buffer is too small — it may come back without a NUL —
    // so the terminator is searched for rather than assumed.
    let mut buf = [0u8; 256];
    // SAFETY: the pointer and length describe `buf` exactly, and
    // `gethostname` writes at most `len` bytes into it.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// Colour one diff line the way Python's `_colorize_diff_line` does.
///
/// The branch order is Python's and it is load-bearing: `---` and `+++`
/// are tested *before* the bare `-` and `+` cases, so the two file
/// headers come out cyan rather than red and green. Testing `-` first
/// would colour `--- a/foo` red, which is what a reimplementation that
/// reorders the obvious-looking cases produces.
///
/// A context line — leading space — and anything else fall through
/// uncoloured. Python has no branch for `\ No newline at end of file`
/// either, and difflib with `lineterm=''` never emits one.
fn colorize_diff_line(line: &str, color: bool) -> String {
    const RED: &str = "\x1b[31m";
    const GREEN: &str = "\x1b[32m";
    const CYAN: &str = "\x1b[36m";
    const RESET: &str = "\x1b[0m";

    if !color {
        return line.to_string();
    }
    let paint = |c: &str| format!("{c}{line}{RESET}");
    if line.starts_with("---") || line.starts_with("+++") || line.starts_with("@@") {
        paint(CYAN)
    } else if line.starts_with('-') {
        paint(RED)
    } else if line.starts_with('+') {
        paint(GREEN)
    } else {
        line.to_string()
    }
}

/// `<kind>/<external_ref>`, split on the FIRST slash only: refs
/// routinely contain slashes themselves (File entities are
/// `<slug>/<path>`, StorePaths start with `/nix/store/`), so splitting
/// on the last would mangle every one of them.
fn split_entity(entity: &str) -> Result<(&str, &str)> {
    entity
        .split_once('/')
        .filter(|(kind, _)| !kind.is_empty())
        .ok_or_else(|| anyhow::anyhow!("expected <kind>/<external_ref>, got {entity:?}"))
}

/// `--via a,b,` → `["a", "b"]`. Empty segments are dropped, as on the
/// Python side, so a trailing comma is not a relation kind named "".
fn parse_via(via: Option<&str>) -> Vec<String> {
    via.into_iter()
        .flat_map(|s| s.split(','))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Resolve a project name to its whole row, not just its slug.
///
/// The `vcs` commands need the id — their queries filter on
/// `c.project_id`, as the Python ones do — where every other command
/// only needs the slug. Sharing [`resolve_project`]'s ambiguity
/// formatting matters more than the two extra lines: the ●/○ listing is
/// on stdout and has to read the same whichever command produced it.
/// The per-file header both working-set diff forms print, which is
/// Python's four `print`s with a `\n` on the last one.
fn print_working_header(f: &templedb_db::WorkingFile) {
    let rule = "=".repeat(70);
    println!("{rule}");
    println!("File: {}", f.file_path);
    println!("State: {}", f.state);
    println!("{rule}");
    println!();
}

/// Python's `_display_diff`, which the single-file form inlines and the
/// two working-set forms call.
fn print_diff(old: &str, new: &str, old_label: &str, new_label: &str, sbs: bool, color: bool) {
    let (old_lines, new_lines) = (templedb_db::splitlines(old), templedb_db::splitlines(new));
    let render = if sbs {
        templedb_db::context_diff
    } else {
        templedb_db::unified_diff
    };
    let diff = render(&old_lines, &new_lines, old_label, new_label, 3);
    if diff.is_empty() {
        println!("No differences");
        return;
    }
    for line in &diff {
        println!("{}", colorize_diff_line(line, color));
    }
}

/// Python's truthiness for a `TEXT` column: NULL *and* the empty string
/// are both falsy, and several branches here turn on that rather than
/// on NULL-ness alone.
fn truthy(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.is_empty())
}

/// `vcs diff <proj> --staged`.
fn diff_staged(
    store: &dyn templedb_db::Store,
    p: &templedb_db::Project,
    branch_id: i64,
    sbs: bool,
    no_color: bool,
) -> Result<i32> {
    let files = templedb_db::staged_files(store, p.id, branch_id)?;
    if files.is_empty() {
        println!("No staged changes");
        return Ok(0);
    }
    println!("Staged changes for {}:", p.slug);
    println!();

    for f in &files {
        print_working_header(f);
        match f.state.as_str() {
            "added" => {
                // `staged_hash or current_hash` — Python's `or`, so an
                // empty staged hash falls through to the current one.
                let hash = truthy(f.content_hash.as_deref())
                    .or(truthy(f.current_hash.as_deref()))
                    .map(str::to_string);
                let content = match &hash {
                    Some(h) => templedb_db::content_by_hash(store, h)?,
                    // Python passes None into the query, which matches
                    // no row. Skipping the round trip is the same answer.
                    None => None,
                };
                if let Some(c) = truthy(content.as_deref()) {
                    println!("+++ New file");
                    for line in templedb_db::splitlines(c) {
                        println!("+{line}");
                    }
                }
                // Outside the `if`, which is Python's indentation.
                println!();
            }
            "deleted" => {
                let old = templedb_db::last_committed_content(store, f.file_id)?;
                match truthy(old.as_deref()) {
                    Some(o) => {
                        for line in templedb_db::splitlines(o) {
                            println!("-{line}");
                        }
                    }
                    // An *empty* committed file reaches this too, not
                    // only a missing one: Python's test is truthiness.
                    None => println!("--- File deleted (no committed content to show)"),
                }
                println!();
            }
            "modified" => {
                let old = templedb_db::last_committed_content(store, f.file_id)?;
                // The new side is `current_hash` and the label says
                // `(staged)`. That mismatch is Python's and is argued
                // on `staged_files` — the staged hash is right there in
                // the row and unused.
                let new = match truthy(f.current_hash.as_deref()) {
                    Some(h) => templedb_db::content_by_hash(store, h)?,
                    None => None,
                };
                if let Some(n) = truthy(new.as_deref()) {
                    print_diff(
                        old.as_deref().unwrap_or(""),
                        n,
                        &format!("{} (committed)", f.file_path),
                        &format!("{} (staged)", f.file_path),
                        sbs,
                        !no_color,
                    );
                }
                println!();
            }
            // Any other state prints the header and nothing else — not
            // even the trailing blank line, because Python's `print()`
            // calls are inside the three branches. `'unmodified'` is
            // the reachable one: a commit sets the state and clears the
            // staging owner together, so a staged-and-unmodified row
            // should not exist, and nothing enforces that.
            _ => {}
        }
    }
    Ok(0)
}

/// A bare `vcs diff <proj>` — every changed row, staged or not.
///
/// Structurally close to [`diff_staged`] and deliberately not merged
/// with it. Three things differ and each one is a behaviour: the new
/// side comes from `ws.content_hash` rather than `file_contents`, the
/// unreadable-content check is an explicit `is None` rather than a
/// truthiness test, and the final branch catches every state instead of
/// only `'modified'`. A shared renderer taking three flags would hide
/// exactly the differences worth seeing.
fn diff_working(
    store: &dyn templedb_db::Store,
    p: &templedb_db::Project,
    branch_id: i64,
    sbs: bool,
    no_color: bool,
) -> Result<i32> {
    let files = templedb_db::changed_files(store, p.id, branch_id)?;
    if files.is_empty() {
        println!("No changes");
        println!(
            "  (run `templedb vcs status {} --refresh` if you expected some \
             — working state is only as current as the last scan)",
            p.slug
        );
        return Ok(0);
    }

    for f in &files {
        print_working_header(f);
        if f.state == "deleted" {
            let old = templedb_db::last_committed_content(store, f.file_id)?;
            match truthy(old.as_deref()) {
                Some(o) => {
                    for line in templedb_db::splitlines(o) {
                        println!("-{line}");
                    }
                }
                None => println!("--- File deleted (no committed content to show)"),
            }
            println!();
            continue;
        }

        let new = match f.content_hash.as_deref() {
            Some(h) => templedb_db::content_by_hash(store, h)?,
            None => None,
        };
        // `is None`, not truthiness — so an *empty* staged file falls
        // through to the diff below and renders as an empty-vs-old
        // comparison, where `--staged` would have skipped it. The two
        // forms genuinely disagree here.
        let Some(new) = new else {
            println!("(binary or unreadable content — no textual diff)");
            println!();
            continue;
        };

        if f.state == "added" {
            println!("+++ New file");
            for line in templedb_db::splitlines(&new) {
                println!("+{line}");
            }
            println!();
            continue;
        }

        let old = templedb_db::last_committed_content(store, f.file_id)?;
        print_diff(
            old.as_deref().unwrap_or(""),
            &new,
            &format!("{} (committed)", f.file_path),
            &format!("{} (working)", f.file_path),
            sbs,
            !no_color,
        );
        println!();
    }
    Ok(0)
}

fn resolve_project_row(
    store: &dyn templedb_db::Store,
    pattern: &str,
) -> Result<templedb_db::Project> {
    match templedb_db::match_project(store, pattern)? {
        ProjectLookup::Found(p) => Ok(p),
        // Delegate both failure shapes so the messages stay in one
        // place; `resolve_project` always errors on these.
        _ => {
            resolve_project(store, pattern)?;
            unreachable!("resolve_project errors unless it found exactly one project")
        }
    }
}

/// Resolve a user-supplied project name to a slug, reporting ambiguity
/// the way the Python matcher does (● for a strong match, ○ for weak).
fn resolve_project(store: &dyn templedb_db::Store, pattern: &str) -> Result<String> {
    match templedb_db::match_project(store, pattern)? {
        ProjectLookup::Found(p) => Ok(p.slug),
        ProjectLookup::NotFound => {
            anyhow::bail!("no project matches {pattern:?} (try `templedb-rs project list`)")
        }
        ProjectLookup::Ambiguous(candidates) => {
            let listed = candidates
                .iter()
                .map(|(key, score)| {
                    let mark = if *score > 0.8 { '●' } else { '○' };
                    format!("\n  {mark} {key}")
                })
                .collect::<String>();
            anyhow::bail!("{pattern:?} matches several projects:{listed}\nUse an exact slug")
        }
    }
}

/// Python's `repr()` for a string, which `entity search`'s
/// empty-result line and `entity observations`' change lines both
/// print with `{!r}` — so it is part of the output being diffed.
///
/// Rust's `{:?}` always double-quotes. Python prefers single quotes and
/// only switches to double when the string contains a single quote and
/// no double quote, in which case it switches rather than escaping.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            // C0 and C1 controls. Python also escapes a wider set of
            // non-printable Unicode, which no label in this database
            // contains; going further would mean reimplementing
            // `unicodedata` for a case that has never occurred.
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// [`py_repr`] for a nullable column. `None` is the bare word, not
/// `'None'` — which is why `Observation` carries these as `Option` all
/// the way here instead of unwrapping to `""` at the query.
fn py_repr_opt(value: Option<&str>) -> String {
    value.map_or_else(|| "None".to_string(), py_repr)
}

fn label_suffix(label: Option<&str>) -> String {
    match label {
        Some(l) if !l.is_empty() => format!(" — {l}"),
        _ => String::new(),
    }
}

/// One row of `source revisions`, as its own function so the column
/// widths can be tested — they are the part of this command most likely
/// to drift, and the drift is one space.
///
/// Python's line is
///
/// ```text
/// f"  {rev:<14} {observed_at}  "
/// f"{h[:12] if h else '(deleted)  '}  "
/// f"{size if size is not None else '-'} bytes, "
/// f"{lines if lines is not None else '-'} lines"
/// ```
///
/// Two details are deliberate reproductions rather than choices:
///
/// - **The deleted placeholder is eleven characters** where a truncated
///   hash is twelve, so the size column sits one place left on a
///   deletion. Padding `(deleted)` to 12 lines the table up and
///   disagrees with Python on all 29 live rows that have no hash. This
///   port did exactly that until it was diffed.
/// - **A NULL size or line count is `-`**, not `None`. The same two
///   columns print `None` under `source snapshot --meta`, because that
///   command interpolates them with no fallback — see [`py_str_opt`].
///   One schema, two renderings, and both are on stdout.
fn revision_line(r: &templedb_db::Revision) -> String {
    let rev: String = r.revision.chars().take(12).collect();
    let hash: String = match &r.content_hash {
        Some(h) => h.chars().take(12).collect(),
        None => "(deleted)  ".to_string(),
    };
    let dash = |v: Option<i64>| v.map_or_else(|| "-".to_string(), |n| n.to_string());
    format!(
        "  {rev:<14} {}  {hash}  {} bytes, {} lines",
        r.observed_at,
        dash(r.file_size_bytes),
        dash(r.line_count)
    )
}

/// A nullable column the way an f-string interpolates it: `str(None)`
/// is the bare word `None`.
///
/// Ugly, and it is Python's `None` leaking into the output rather than a
/// considered choice of placeholder — but `source snapshot --meta` is a
/// labelled dump of a row, nothing is misreported by it, and the status
/// table claims the command is byte-identical. An earlier revision
/// printed `(none)` and `?` here, which is nicer and was a silent,
/// undocumented divergence in three fields.
///
/// This is `str`, not [`py_repr_opt`]'s `repr`: `entity observations`
/// prints `'text'` with quotes because Python builds that line with
/// `{!r}`, and this one has no quotes because `--meta` does not.
fn py_str_opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "None".to_string(), |x| x.to_string())
}

/// Group digits in threes with commas, as Python's `{:,}` does.
///
/// `project show` prints its line count that way, so the separators are
/// part of the output being diffed. Negative values keep the sign
/// outside the grouping, which is also what Python does — not that a
/// line count can be negative, but the function should not be the
/// reason anyone finds out.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if n < 0 {
        out.push('-');
    }
    for (i, ch) in digits.char_indices() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file headers must be cyan, not red and green. `---` starts
    /// with `-` and `+++` with `+`, so a colouriser that tests the
    /// single-character cases first gets both wrong — and the output
    /// still looks like a plausible coloured diff, which is why this is
    /// worth a test rather than a glance.
    #[test]
    fn diff_headers_are_cyan_not_red_and_green() {
        const CYAN: &str = "\x1b[36m";
        const RED: &str = "\x1b[31m";
        const GREEN: &str = "\x1b[32m";

        assert!(colorize_diff_line("--- a/x (abc12345)", true).starts_with(CYAN));
        assert!(colorize_diff_line("+++ b/x (def67890)", true).starts_with(CYAN));
        assert!(colorize_diff_line("@@ -1,3 +1,3 @@", true).starts_with(CYAN));
        assert!(colorize_diff_line("-removed", true).starts_with(RED));
        assert!(colorize_diff_line("+added", true).starts_with(GREEN));
        // A context line is untouched — no escape, and no reset either.
        assert_eq!(colorize_diff_line(" context", true), " context");
        // An empty context line (a blank line in the file) renders as
        // the single space difflib emits, uncoloured.
        assert_eq!(colorize_diff_line(" ", true), " ");
    }

    /// `--no-color` must leave every line exactly as difflib produced
    /// it, including the ones that would otherwise be painted.
    #[test]
    fn no_color_passes_every_line_through_unchanged() {
        for line in [
            "--- a/x (abc)",
            "+++ b/x (def)",
            "@@ -1 +1 @@",
            "-gone",
            "+new",
            " same",
        ] {
            assert_eq!(colorize_diff_line(line, false), line);
        }
    }

    fn rev(hash: Option<&str>, size: Option<i64>, lines: Option<i64>) -> templedb_db::Revision {
        templedb_db::Revision {
            revision: "ed5de8b09e24".into(),
            content_hash: hash.map(str::to_string),
            observed_at: "2026-10-07 20:36:44".into(),
            file_size_bytes: size,
            line_count: lines,
        }
    }

    /// The two lines below are copied from `templedb source revisions
    /// templedb CHANGELOG.md`, which is why they are asserted as whole
    /// strings rather than field by field: the thing being pinned is the
    /// run of spaces between the columns, and a field-wise test cannot
    /// see it.
    #[test]
    fn a_revision_line_matches_pythons_columns() {
        assert_eq!(
            revision_line(&rev(Some("f74e31da94bfaaaa"), Some(0), Some(12))),
            "  ed5de8b09e24   2026-10-07 20:36:44  f74e31da94bf  0 bytes, 12 lines"
        );
    }

    /// A deletion: `-` for both numbers, and the placeholder is one
    /// character *narrower* than a hash, so the size column shifts left.
    /// Padding `(deleted)` to 12 is the obvious thing to write and is
    /// wrong on all 29 live rows that reach this branch.
    #[test]
    fn a_deleted_revision_keeps_pythons_off_by_one_padding() {
        let line = revision_line(&rev(None, None, None));
        assert_eq!(
            line,
            "  ed5de8b09e24   2026-10-07 20:36:44  (deleted)    - bytes, - lines"
        );
        // Four spaces after the placeholder, not five.
        assert!(line.contains("(deleted)    -"));
        assert!(!line.contains("(deleted)     "));
    }

    /// A NULL line count alone, which is the common case: the blob is
    /// there and empty, so only one column falls back.
    #[test]
    fn a_null_line_count_alone_renders_one_dash() {
        assert_eq!(
            revision_line(&rev(Some("f74e31da94bf"), Some(0), None)),
            "  ed5de8b09e24   2026-10-07 20:36:44  f74e31da94bf  0 bytes, - lines"
        );
    }

    /// The same two columns print `None` under `--meta` and `-` here,
    /// because Python writes the two lines differently. Asserted
    /// together so a future simplification to one helper fails rather
    /// than silently picking one.
    #[test]
    fn meta_and_revisions_render_the_same_null_differently() {
        assert_eq!(py_str_opt(None::<i64>), "None");
        assert_eq!(py_str_opt(Some(12)), "12");
        assert_eq!(py_str_opt(None::<&str>), "None");
        assert!(revision_line(&rev(None, None, None)).contains("- bytes, - lines"));
    }

    #[test]
    fn entity_addresses_split_on_the_first_slash() {
        assert_eq!(split_entity("Machine/box1").unwrap(), ("Machine", "box1"));
        // StorePath refs are absolute paths; everything after the first
        // slash is the ref, leading slash included.
        assert_eq!(
            split_entity("StorePath//nix/store/abc").unwrap(),
            ("StorePath", "/nix/store/abc")
        );
        assert_eq!(
            split_entity("File/templedb/src/x.py").unwrap(),
            ("File", "templedb/src/x.py")
        );
        assert!(split_entity("Machine").is_err());
        // A leading slash would ask for the empty kind.
        assert!(split_entity("/box1").is_err());
    }

    #[test]
    fn via_parsing_drops_empty_segments() {
        assert_eq!(parse_via(None), Vec::<String>::new());
        assert_eq!(parse_via(Some("")), Vec::<String>::new());
        assert_eq!(parse_via(Some("ran")), vec!["ran"]);
        assert_eq!(
            parse_via(Some(" ran , installs ,")),
            vec!["ran", "installs"]
        );
    }

    /// The empty-result line is on stdout, so Python's `repr()` quoting
    /// is part of what gets diffed.
    #[test]
    fn query_echo_matches_python_repr_quoting() {
        assert_eq!(py_repr("plain"), "'plain'");
        assert_eq!(py_repr(""), "''");
        // A string containing a single quote switches to double quotes
        // rather than escaping, which is Python's rule.
        assert_eq!(py_repr("it's"), "\"it's\"");
        // Unless it also contains a double quote, in which case Python
        // keeps single quotes and escapes.
        assert_eq!(py_repr("it's \"x\""), "'it\\'s \"x\"'");
        assert_eq!(py_repr("a\\b"), "'a\\\\b'");
        // Control characters take their short escapes, not \xNN.
        assert_eq!(py_repr("a\nb\tc"), "'a\\nb\\tc'");
        assert_eq!(py_repr("\r"), "'\\r'");
        // And anything else non-printable is hex. A label carrying a
        // raw control byte would otherwise emit it to the terminal.
        assert_eq!(py_repr("\u{1}"), "'\\x01'");
        assert_eq!(py_repr("\u{7f}"), "'\\x7f'");
        // Non-ASCII is printable and passes through, as Python 3 does.
        assert_eq!(py_repr("héllo"), "'héllo'");
    }

    /// `entity observations` prints a NULL label as the bare word
    /// `None`, because Python's `!r` on `None` does. `'None'` would be
    /// indistinguishable from a label whose text is "None".
    #[test]
    fn null_columns_render_as_pythons_bare_none() {
        assert_eq!(py_repr_opt(None), "None");
        assert_eq!(py_repr_opt(Some("None")), "'None'");
        assert_eq!(py_repr_opt(Some("x")), "'x'");
    }

    /// `project show` prints its line count with Python's `{:,}`, so
    /// the separators are part of the diffed output.
    #[test]
    fn thousands_groups_like_python() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1), "1");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(12345), "12,345");
        assert_eq!(thousands(1234567), "1,234,567");
        // The sign sits outside the grouping, as Python puts it.
        assert_eq!(thousands(-1234), "-1,234");
        assert_eq!(thousands(i64::MIN), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn labels_render_only_when_present() {
        assert_eq!(label_suffix(None), "");
        assert_eq!(label_suffix(Some("")), "");
        assert_eq!(label_suffix(Some("gen 1")), " — gen 1");
    }

    /// `clap` configuration errors only surface at runtime, so assert
    /// the command tree is well-formed rather than discovering it on
    /// first use.
    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
