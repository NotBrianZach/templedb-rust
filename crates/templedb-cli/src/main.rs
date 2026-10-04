//! `templedb-rs` — the Rust port's CLI.
//!
//! Named `templedb-rs`, not `templedb`, on purpose: the Python binary is
//! on PATH as `templedb` and is the one every script, systemd unit and
//! agent wrapper invokes. A port that shadows it while incomplete would
//! break callers silently, which is the opposite of what a port is for.
//! Rename it when it can answer everything the original can, not before.
//!
//! Subcommands mirror the Python verb/noun shape (`project list`,
//! `file cat`) so muscle memory and documentation transfer.

use anyhow::Result;
use clap::{Parser, Subcommand};

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
}

#[derive(Subcommand)]
enum SearchCmd {
    /// Full-text search over file contents (FTS5 syntax)
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
}

fn main() {
    // Return an exit code rather than letting a panic print a backtrace:
    // this is a CLI, and "Error: no database at ..." is the useful output.
    if let Err(e) = run() {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let conn = templedb_db::open_ro()?;

    match cli.command {
        Command::Project { command } => match command {
            ProjectCmd::List => {
                let projects = templedb_db::list_projects(&conn)?;
                println!("{:<28} {:>7} {:>10}", "SLUG", "FILES", "LINES");
                for p in &projects {
                    println!("{:<28} {:>7} {:>10}", p.slug, p.files, p.lines);
                }
                eprintln!("\n{} project(s)", projects.len());
            }
        },
        Command::File { command } => match command {
            FileCmd::Cat { project, path } => {
                // print! not println!: the stored blob keeps its own
                // trailing newline, and adding one makes `| sha256sum`
                // disagree with the Python `file cat` it is replacing.
                print!("{}", templedb_db::read_file(&conn, &project, &path)?);
            }
            FileCmd::Ls {
                project,
                prefix,
                long,
            } => {
                let files = templedb_db::list_files(&conn, &project, &prefix)?;
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
                // <kind>/<external_ref>, split on the FIRST slash only:
                // refs routinely contain slashes themselves (File entities
                // are '<slug>/<path>'), so splitting on the last would
                // mangle every one of the 1,972 of them.
                let (kind, eref) = entity
                    .split_once('/')
                    .ok_or_else(|| anyhow::anyhow!(
                        "expected <kind>/<external_ref>, got {entity:?}"))?;
                let (out, inc) = templedb_db::explore(&conn, kind, eref)?;
                println!("{kind}/{eref}");
                println!("\n  outbound ({})", out.len());
                for e in &out {
                    println!("    --{}--> {}  [{}]", e.kind, e.other, e.authority);
                }
                println!("\n  inbound ({})", inc.len());
                for e in &inc {
                    println!("    <--{}-- {}  [{}]", e.kind, e.other, e.authority);
                }
            }
            EntityCmd::Stats => {
                let stats = templedb_db::entity_stats(&conn)?;
                let total: i64 = stats.iter().map(|(_, n)| n).sum();
                for (kind, n) in &stats {
                    println!("{:<20} {:>8}", kind, n);
                }
                println!("{:<20} {:>8}", "TOTAL", total);
            }
        },
        Command::Search { command } => match command {
            SearchCmd::Content {
                pattern,
                project,
                limit,
            } => {
                let hits = templedb_db::search_content(
                    &conn,
                    &pattern,
                    project.as_deref(),
                    limit,
                )?;
                if hits.is_empty() {
                    eprintln!("no files containing {pattern:?}");
                    return Ok(());
                }
                for h in &hits {
                    println!("{}  {}", h.project, h.path);
                    // Snippets are one line so output stays greppable.
                    println!("    {}", h.snippet.replace('\n', " "));
                }
                eprintln!("\n{} hit(s)", hits.len());
            }
        },
    }
    Ok(())
}
