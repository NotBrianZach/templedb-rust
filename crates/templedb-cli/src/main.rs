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

fn main() {
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
                let slug = resolve_project(&conn, &project)?;
                // print! not println!: the stored blob keeps its own
                // trailing newline, and adding one makes `| sha256sum`
                // disagree with the Python `file cat` it is replacing.
                print!("{}", templedb_db::read_file(&conn, &slug, &path)?);
            }
            FileCmd::Ls {
                project,
                prefix,
                long,
            } => {
                let slug = resolve_project(&conn, &project)?;
                let files = templedb_db::list_files(&conn, &slug, &prefix)?;
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
                let e = templedb_db::explore(&conn, kind, eref)?;
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
                    templedb_db::trace(&conn, kind, eref, depth, direction.into(), &via, limit)?;
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
                let hits = templedb_db::search_entities(&conn, &query, kind.as_deref(), limit)?;
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
                    &conn,
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
                // The project filter resolves fuzzily too, but an
                // unmatched name here must not silently widen the search
                // to every project.
                let slug = match project {
                    Some(p) => Some(resolve_project(&conn, &p)?),
                    None => None,
                };
                let hits = templedb_db::search_content(&conn, &pattern, slug.as_deref(), limit)?;
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
            }
        },
        Command::Source { command } => match command {
            SourceCmd::Snapshot {
                project,
                file_path,
                rev,
                meta,
            } => {
                let slug = resolve_project(&conn, &project)?;
                let snap = templedb_db::snapshot(&conn, &slug, &file_path, rev.as_deref())?;
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
                match templedb_db::snapshot_body(&conn, &snap)? {
                    SnapshotBody::Text(t) => out.write_all(t.as_bytes())?,
                    SnapshotBody::Binary(b) => out.write_all(&b)?,
                }
                out.flush()?;
            }
            SourceCmd::Revisions { project, file_path } => {
                let slug = resolve_project(&conn, &project)?;
                let revs = templedb_db::revisions(&conn, &slug, &file_path)?;
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
    }
    Ok(0)
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

/// Resolve a user-supplied project name to a slug, reporting ambiguity
/// the way the Python matcher does (● for a strong match, ○ for weak).
fn resolve_project(conn: &templedb_db::Connection, pattern: &str) -> Result<String> {
    match templedb_db::match_project(conn, pattern)? {
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

/// Python's `repr()` for a string, which `entity search`'s empty-result
/// line prints with `{!r}` and which is therefore part of the output
/// being diffed. Rust's `{:?}` always double-quotes; Python prefers
/// single quotes and only switches when the string contains one.
fn py_repr(s: &str) -> String {
    if s.contains('\'') && !s.contains('"') {
        format!("\"{s}\"")
    } else {
        format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
    }
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

fn num(v: Option<i64>) -> String {
    v.map_or_else(|| "?".to_string(), |n| n.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

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
