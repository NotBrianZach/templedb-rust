# TempleDB Rust

A Rust port of TempleDB, the SQLite-backed project configuration and
version control system. Binary name is `templedb-rs`.

## Status: read-only subset, verified against the Python implementation

Restarted 2026-10-04. The previous contents of this repo described a port
that did not exist — it claimed "Basic CLI structure with clap" and
"Command scaffolding for project and VCS operations" while containing no
Rust at all, zero commits, and a single `src/main.py` holding
`print('hello')`. That README is gone. This one documents only what runs.

**Implemented and cross-checked against `templedb`:**

| Command | Status |
|---|---|
| `templedb-rs project list` | matches, 30 projects |
| `templedb-rs file cat <proj> <path>` | byte-identical output |
| `templedb-rs file ls <proj> [prefix] [-l]` | matches |
| `templedb-rs entity stats` | identical counts, TOTAL 35,012 |
| `templedb-rs search content <pat> [-p] [--limit]` | same hit set as the *fixed* Python |
| `templedb-rs entity explore <kind>/<ref>` | same edge counts and kinds |

Verification is byte-level, not eyeball: `file cat` of
`templedb src/cli/commands/entity.py` (5,048 lines) produces the same
sha256 as the Python implementation, and `entity stats` agrees on every
kind.

`entity explore` prints edges only. The Python version also shows the
entity's `label`, `source_authority` and `observed_at` above them; that is
a known cosmetic gap, not parity.

`search content` found a real bug in the original while being written —
see *Bugs found while porting* below.

**Not implemented.** Everything else — which is most of it. No writes of
any kind, no VCS operations, no sessions, no checkouts, no ingest, no
doctor, no publish, no Nix integration, no GUI. See *Porting order* below.

### Measured, 3-run average on a 776 MB database

| | `templedb-rs` | `templedb` | |
|---|---|---|---|
| `file cat` (5k lines) | 26 ms | 1142 ms | 44× |
| `project list` | 51 ms | 1104 ms | 22× |
| `entity stats` | 48 ms | 1289 ms | 27× |

Most of the gap is Python interpreter and import startup rather than
query execution, which is exactly the cost a single static binary
removes. Treat these as startup-dominated figures, not a claim about
SQLite throughput.

## Reads only, deliberately

This is a design decision, not a stage we are rushing through. The Python
implementation owns every write path, and those paths carry invariants
learned the hard way: session-scoped staging, checkout role resolution
(`resolve(purpose)`), blob-age guards so a stale disk cannot overwrite a
newer blob. A reader cannot violate any of them. Porting reads first lets
both implementations run against one live database instead of competing
for authority over it.

Two bugs from the Python side are already designed out here:

- **`journal_size_limit` is set** (64 MiB, matching the Python fix of
  2026-10-04). It was unset there and configured nowhere in the tree, so
  a WAL that spiked once stayed spiked — 5.9 GB against a 776 MB
  database.
- **Statements finalize on `Drop`.** The Python side pinned WAL
  checkpoints twice by leaving an un-exhausted cursor holding a read
  transaction. That failure mode does not exist in this crate, because
  `rusqlite::Statement` finalizes when it leaves scope. This is arguably
  the strongest argument for the port after startup time.

## Bugs found while porting

Reimplementing a query means reading the schema it runs against, which is
a different kind of attention than using it.

- **`search content` attributed matches to the wrong project.**
  `file_contents_fts` indexes `(file_path UNINDEXED, content_text)` with
  no project column, and the Python query joined `file_search_view` on
  `file_path` alone. 14 projects have a `README.md`, so one hit fanned
  out to one row per project owning that path — and with `-p` the slug
  filter then *passed* for a project whose file never contained the term.
  `search content rusqlite -p trig-navigator` reported a match and showed
  this repo's README as the snippet; unqualified, it returned 18 results
  against a ground truth of 5. No migration was needed: migration 110 had
  already set `rowid = project_files.id`, and the query simply never used
  it. Fixed in templedb as `1DB323BCFDDF1DBA`; this port joined on rowid
  from the start.

## Building

```bash
nix develop            # rust-overlay toolchain, see flake.nix
cargo build --release  # ./target/release/templedb-rs
cargo test
```

Built and verified with cargo/rustc 1.95.0. `rusqlite` uses the
`bundled` feature, so a C compiler is required (`nixpkgs#gcc` suffices);
no system SQLite is needed and the bundled version cannot skew behaviour
relative to whatever the host ships.

## Layout

A workspace from the start, rather than one crate split later — the
thing being ported is 274,689 lines across 801 files over a 163-table
schema, and the Python layering (cli / services / repositories) is why a
fix in one repository file can correct a dozen commands at once. The same
split keeps SQL somewhere the CLI cannot reach around.

```
crates/templedb-db/    SQLite layer: path resolution, pragmas, queries
crates/templedb-cli/   clap binary `templedb-rs`
```

## Porting order

Reads first, by how often they are used and how self-contained they are.
Writes last, and only once the read layer is trusted.

1. **Done** — projects, file read/list, entity stats, `search content`
   (FTS5), `entity explore` (one hop both directions).
2. **Next** — `entity trace` (multi-hop BFS with `--depth` / `--via`),
   `source snapshot` at a revision, and the `label` / `observed_at` header
   `entity explore` still omits.
3. **Then** — `summary` and `doctor` read-only invariants. These are pure
   queries and the highest-value target after search: `doctor` hashes
   roughly 400 files per run, which is work Rust does far better.
4. **Later** — `vcs log` / `diff` / `status` (read paths only).
5. **Last, and not without a design document** — any write path.

Nothing here renames the binary to `templedb`. The Python one is on PATH
under that name and every script, systemd unit and agent wrapper invokes
it; shadowing it while incomplete would break callers silently. Rename
when it can answer everything the original can.

## License

MIT
