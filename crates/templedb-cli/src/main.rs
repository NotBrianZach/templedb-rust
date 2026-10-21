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

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use std::io::Write;
use templedb_db::{Direction, ProjectLookup, SnapshotBody};

mod summary_view;

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
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// List all projects.
    ///
    /// The two filters are Python's and select rows only — the counts a
    /// project shows are the same whether you filtered or not. Python
    /// runs a separate query under either flag, with a different file
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
    /// Write a file's contents. **The port's only command that writes.**
    ///
    /// Writes the port's own Postgres — the one named by
    /// `TEMPLEDB_WRITE_URL` — and never the live SQLite database, which
    /// the Python implementation keeps sole authority over. Requires
    /// `TEMPLEDB_RS_ALLOW_WRITES=1`.
    ///
    /// **Partial.** Python's `file set` carries seven guards; this
    /// carries two — `--if-match` and the un-delete. Absent: the
    /// mojibake check, `edit_intents` provenance, staging, `--commit`,
    /// `--verify`, and binary content. The opt-in exists because none
    /// of that is visible from a command line. See the README's
    /// *The first write*.
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
    /// Defaults to the published view — commits on the shared branch.
    /// The Python command's default additionally includes commits owned
    /// by your current staging session, which this port does not
    /// reproduce: resolving "your session" runs six fallbacks over
    /// environment variables and process ids and creates a session row
    /// if none of them hit, which is both environment-dependent and a
    /// write. The view is printed in the header either way, so the
    /// output says which one you got.
    Log {
        project: String,
        /// Number of commits to show
        #[arg(short = 'n', default_value_t = 10)]
        n: i64,
        /// Filter by branch name (exact)
        #[arg(short, long)]
        branch: Option<String>,
        /// Show every commit, including other sessions' unpublished
        /// ones. This is the one view that is identical to Python's,
        /// because it consults no session at all.
        #[arg(long)]
        all_sessions: bool,
        /// Show only published commits — which is already the default
        /// here.
        ///
        /// Accepted because it selects behaviour this port *has*, and
        /// because it is the most portable of Python's three views: it
        /// is the only one that needs no session. A script written
        /// `vcs log --published` works against Python and exited 2
        /// against this binary until 2026-10-21, which is the opposite
        /// of the compatibility a port is for.
        ///
        /// Deliberately not the same decision as `search content`'s
        /// `--no-fts`, which is *not* accepted. That one promises
        /// different matching and would silently deliver none; this one
        /// promises the published view and delivers exactly it. A no-op
        /// flag is only harmful when the thing it names is absent.
        ///
        /// `--all-sessions` wins if both are passed, which is Python's
        /// precedence inverted — its `--published` overrides
        /// `--all-sessions`. Reproducing the inversion would mean
        /// `--published` silently discarding the one view this port
        /// renders identically; conflicting flags are better refused,
        /// so clap refuses them.
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
        file: String,
        /// Commit hash or prefix
        commit1: Option<String>,
        /// Commit hash or prefix
        commit2: Option<String>,
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
            ProjectCmd::List { nix_only, category } => {
                let filter = templedb_db::ProjectFilter {
                    nix_only,
                    category: category.map(|c| c.as_str().to_string()),
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
            } => {
                // Read the content before opening anything, so a
                // refusal on stdin does not leave a connection or a
                // transaction waiting on it.
                let content = match content {
                    Some(c) => c,
                    None => {
                        use std::io::Read;
                        let mut buf = String::new();
                        // Text only. Python falls back to
                        // `content_blob` for undecodable stdin,
                        // deliberately — it is why `.authinfo.gpg`
                        // could be stored at all — and this port does
                        // not have that branch yet, so it refuses
                        // rather than writing mojibake.
                        std::io::stdin().read_to_string(&mut buf).map_err(|e| {
                            anyhow::anyhow!(
                                "stdin is not UTF-8 text ({e}). This port's \
                                     `file set` writes text only; Python's falls \
                                     back to a binary blob."
                            )
                        })?;
                        buf
                    }
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
                let mut tx = writer.transaction()?;
                let project_id = templedb_db::resolve_project_exact(&mut tx, &project)?;
                let outcome = templedb_db::set_file(
                    &mut tx,
                    &clock,
                    project_id,
                    &path,
                    &content,
                    if_match.as_deref(),
                );
                // `?` before the commit, deliberately: on the error path
                // `tx` is dropped without `commit`, and `postgres`
                // rolls an un-committed transaction back on drop. A
                // refusal has to leave nothing behind — `--if-match`
                // exists to *prevent* a write, not to report one — and
                // the ordering here is what makes that true.
                let outcome = outcome?;
                tx.commit()?;

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
                // Accepted and intentionally unread: the published view
                // is this port's default, so the flag names what the
                // `else` arm below already does. Bound rather than
                // matched with `..` so that adding a third view cannot
                // quietly leave this one unhandled.
                published: _,
            } => {
                let p = resolve_project_row(&store, &project)?;
                let visibility = if all_sessions {
                    templedb_db::Visibility::All
                } else {
                    templedb_db::Visibility::Published
                };
                let commits =
                    templedb_db::commit_log(&store, p.id, branch.as_deref(), visibility, n)?;

                println!();
                println!("Commit log for {} [{}]", p.slug, visibility.label());
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
            } => {
                let p = resolve_project_row(&store, &project)?;
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
            let session = session_id();
            // One call, so no pane can read a database that moved
            // between it and the pane above — the live file is being
            // written by the Python implementation while this runs.
            let facts = templedb_db::observe(store.as_ref(), &clock, &session)?;
            for line in summary_view::render(&facts, &clock, &session, !no_color) {
                println!("{line}");
            }
        }
        // Handled above, before the read store is opened.
        Command::Mirror { .. } => unreachable!("handled before open_ro"),
    }
    Ok(0)
}

/// Python's session identity: `TEMPLEDB_SESSION_ID`, or
/// `{hostname}-{getppid()}`.
///
/// This is the third of `summary`'s three layers — environment — and it
/// is two system calls rather than a query on purpose. The handoff
/// inbox is scoped to it, which means the pane's answer depends on
/// *which shell* invoked the command: the same database, read from two
/// terminals, reports two different inbox counts. Resolving it at the
/// entry point does not remove that dependency, but it puts it where a
/// caller can see it and a test can fix it, which is the rule the
/// write path will need when every insert stamps `datetime('now')`.
///
/// An exported-but-empty variable falls back, because Python's `or`
/// treats `''` as unset — the same reading `conn.rs` gives an empty
/// `TEMPLEDB_PATH`.
fn session_id() -> String {
    match std::env::var("TEMPLEDB_SESSION_ID") {
        Ok(v) if !v.is_empty() => v,
        // SAFETY: `getppid` takes no arguments, cannot fail, and
        // touches no memory this program owns.
        _ => format!("{}-{}", hostname(), unsafe { libc::getppid() }),
    }
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
