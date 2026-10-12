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
    /// List all projects
    #[command(alias = "ls")]
    List,
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
    /// List tracked files, optionally under a path prefix
    Ls {
        project: String,
        #[arg(default_value = "")]
        prefix: String,
        /// Show line counts
        #[arg(short, long)]
        long: bool,
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
    },
    /// List branches, with the active one marked.
    ///
    /// Listing only — creating and deleting branches are writes.
    Branch { project: String },
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
        Err(e) => {
            eprintln!("Error: {e:#}");
            std::process::exit(1);
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
    if let Command::Mirror { to, create_schema } = &cli.command {
        let src = templedb_db::SqliteStore::open_ro(&templedb_db::db_path()?)?;
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
            ProjectCmd::List => {
                let projects = templedb_db::list_projects(&store)?;
                println!("{:<28} {:>7} {:>10}", "SLUG", "FILES", "LINES");
                for p in &projects {
                    println!("{:<28} {:>7} {:>10}", p.slug, p.files, p.lines);
                }
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
            FileCmd::Ls {
                project,
                prefix,
                long,
            } => {
                let slug = resolve_project(&store, &project)?;
                let files = templedb_db::list_files(&store, &slug, &prefix)?;
                for f in &files {
                    if long {
                        println!("{:>7} loc  {}", f.lines, f.path);
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
                let hits =
                    templedb_db::trace(&store, kind, eref, depth, direction.into(), &via, limit)?;
                println!("● {kind}/{eref}");
                for hit in &hits {
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
                        return Ok(3);
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
                let stats = templedb_db::entity_stats(&store)?;
                let total: i64 = stats.iter().map(|(_, n)| n).sum();
                for (kind, n) in &stats {
                    println!("{:<20} {:>8}", kind, n);
                }
                println!("{:<20} {:>8}", "TOTAL", total);
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
                    println!("content_hash:      {}", opt(&snap.content_hash));
                    println!("size:              {} bytes", num(snap.file_size_bytes));
                    println!("lines:             {}", num(snap.line_count));
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
                    let rev: String = r.revision.chars().take(12).collect();
                    // A deletion has no content hash at all. The Python
                    // version slices into it unconditionally and raises
                    // a TypeError on exactly these rows.
                    let hash: String = match &r.content_hash {
                        Some(h) => h.chars().take(12).collect(),
                        None => "(deleted)".to_string(),
                    };
                    println!(
                        "  {rev:<14} {}  {hash:<12}  {} bytes, {} lines",
                        r.observed_at,
                        num(r.file_size_bytes),
                        num(r.line_count)
                    );
                }
            }
        },
        Command::Vcs { command } => match command {
            VcsCmd::Log {
                project,
                n,
                branch,
                all_sessions,
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
                    let parent = templedb_db::parent_hash(&store, d.id)?;
                    for f in &files {
                        let new = templedb_db::content_at_commit(
                            &store,
                            p.id,
                            &f.file_path,
                            &d.commit_hash,
                        )?
                        .unwrap_or_default();
                        let old = match &parent {
                            Some(h) => {
                                templedb_db::content_at_commit(&store, p.id, &f.file_path, h)?
                                    .unwrap_or_default()
                            }
                            None => String::new(),
                        };

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
        // Handled above, before the read store is opened.
        Command::Mirror { .. } => unreachable!("handled before open_ro"),
    }
    Ok(0)
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

fn opt(v: &Option<String>) -> &str {
    v.as_deref().unwrap_or("(none)")
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

fn num(v: Option<i64>) -> String {
    v.map_or_else(|| "?".to_string(), |n| n.to_string())
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
