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
| `templedb-rs entity explore <kind>/<ref>` | byte-identical, header included |
| `templedb-rs entity trace <kind>/<ref> [--depth] [--direction] [--via] [--limit]` | byte-identical |
| `templedb-rs source snapshot <proj> <path> [--rev] [--meta]` | byte-identical |
| `templedb-rs source revisions <proj> <path>` | byte-identical where Python does not crash |
| `templedb-rs entity search <query> [--kind] [--limit]` | same hit set; ordering differs on purpose |
| `templedb-rs entity paths FROM TO [--max-depth] [--direction] [--via]` | byte-identical, exit codes included |

Verification is byte-level, not eyeball. Same sha256 as the Python
implementation for `file cat` of `templedb src/cli/commands/entity.py`
(5,048 lines) and for `source snapshot --rev`; identical stdout for
`entity explore Machine/zMothership2` (156 outbound edges, header and
all), for `entity trace Machine/zMothership2 --depth 2`, and for
`entity trace --depth 3 --direction both --via ran,installs --limit 3`.

`entity paths` is diffed across eight variants of one 2-hop query
(`Commit/templedb/FBDACA1F09FBBEF7` →
`Symbol/…:DeployCommands.deploy`) — both directions, at and below the
depth cutoff, each `--direction`, a matching `--via` and a bogus one.
All eight agree byte for byte, and so do the exit codes: 0 for a path,
3 for "no path within depth N".

`entity search` agrees on *which* entities match and deliberately
disagrees on the order they are listed in.
`entity search zMothership` matches 1,064 rows; the two outputs are
byte-identical once sorted, and differ unsorted because Python's
`ORDER BY` has no tie-break. See *Bugs found while porting*.

The `label` / `source_authority` / `observed_at` header that
`entity explore` used to omit is now printed; that gap is closed.

Every command that takes a project now accepts a fuzzy name, not just an
exact slug, because every Python command does. The scoring function is
reproduced value-for-value from `fuzzy_matcher.py`; the two bugs wrapped
around it are not — see below.

50 unit tests, run by `cargo test`: 45 in `templedb-db` and 5 in
`templedb-cli` for the formatting the SQL layer cannot see. The database
tests run against an in-memory fixture
(`crates/templedb-db/src/testdb.rs`) carrying one row per behaviour
worth pinning — a deleted-but-populated file, an active file with no
content row, a binary blob, a `README.md` in two projects, a missing
blob, a NULL `content_hash`, a NULL project name, a relation cycle, and
a `mod_a`/`modxa` pair that tells a literal underscore from a LIKE
wildcard. One test builds 150 edges on a single node, because the
behaviour it pins — finding a neighbour past Python's hardcoded
hundredth — is invisible on a fixture small enough to read. The previous
revision of this README advertised `cargo test` with no tests in the
tree.

**Not implemented.** Everything else — which is most of it. No writes of
any kind, no VCS operations, no sessions, no checkouts, no ingest, no
doctor, no publish, no Nix integration, no GUI. See *Porting order* below.

### Measured, 3-run average on a 776 MB database

| | `templedb-rs` | `templedb` | |
|---|---|---|---|
| `file cat` (5k lines) | 26 ms | 1142 ms | 44× |
| `project list` | 51 ms | 1104 ms | 22× |
| `entity stats` | 48 ms | 1289 ms | 27× |
| `entity trace --depth 3` | 9 ms | 849 ms | 94× |
| `source snapshot --rev` | 11 ms | 865 ms | 79× |
| `source revisions` | 11 ms | 851 ms | 77× |
| `entity search` (1,064 matches) | 35 ms | 857 ms | 24× |
| `entity paths` (2 hops) | 9 ms | 829 ms | 92× |

Most of the gap is Python interpreter and import startup rather than
query execution, which is exactly the cost a single static binary
removes. Treat these as startup-dominated figures, not a claim about
SQLite throughput — most rows here sit at 9–11 ms because they do not do
enough work to show up over process spawn.

The exception is worth stating, because it is the one place this port
does strictly *more* work than the original. `entity paths` removes
Python's fan-out cap (below), so a failed search has to exhaust the
reachable component instead of an arbitrary slice of it.
`entity paths Machine/zMothership2 Machine/web1 --max-depth 6` — no path,
the worst case — costs 17 ms against Python's 923 ms. Doing the whole
search correctly is still 54× faster than doing part of it.

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

- **`source revisions` crashes on any file that was ever deleted.**
  `vcs_file_states.content_hash` is nullable and a `change_type =
  'deleted'` row leaves it NULL. `revisions()` does
  `row['content_hash'][:12]` unconditionally, so it raises
  `'NoneType' object is not subscriptable` — *after* printing the rows
  above the first deletion. `templedb source revisions templedb
  CHANGELOG.md` prints 8 of its 12 revisions and exits 1 today. 29 rows
  in the live database have a NULL hash. This port prints `(deleted)`
  and lists all 12.

- **`source snapshot` prints nothing and exits 0 when the blob is
  gone.** The historical half of the `source_snapshots` view
  `LEFT JOIN`s `content_blobs`, so a row can name a `content_hash` that
  no longer has a blob. `snapshot()` tests `content_type == 'text'` and
  then `content_blob is not None`, falls through both when the join
  produced NULLs, and returns 0 having written zero bytes — identical,
  to any caller, to a genuinely empty file. 509 `vcs_file_states` rows
  reference a hash that is not in `content_blobs`;
  `source snapshot templedb src/cli/commands/deploy.py --rev
  FBDACA1F09FBBEF7` is one of them, and is silent on both streams. This
  port errors and names the blob.

- **Fuzzy project matching rejects most projects as "ambiguous" against
  themselves.** `ProjectFuzzyMatcher` indexes each project under two
  keys — `slug` and `"{name} ({slug})"` — then fails whenever more than
  one *key* scores, without checking whether the keys name the same
  project. So `qa-run`, which identifies exactly one project, is
  refused: `Multiple projects match 'qa-run': ● qa-runner / ○ QA Runner
  (qa-runner)`. Eleven of the thirty projects here have `name != slug`
  and are therefore reachable only by typing the slug exactly, which is
  the one case fuzzy matching was not needed for. This port collapses
  candidates by project id before deciding ambiguity, and still reports
  genuine ambiguity (`trig-nav` → two projects) with the same ●/○
  markers.

- **A NULL project name is searchable as the literal string `None`.**
  The second candidate key is built with an f-string, so
  `templedb-backup-service` (whose `name` is NULL) is indexed as
  `"None (templedb-backup-service)"`. `templedb source revisions None
  flake.nix` resolves and prints its snapshot. Here a missing name
  contributes no key.

- **`entity paths` reports "no path" between entities that share an
  edge.** `graph_paths` fetches each node's neighbours with a hardcoded
  `limit=100` and no `ORDER BY`, so a node with more than 100 edges
  contributes an arbitrary hundred of them to the BFS and the rest are
  simply not in the graph being searched. 122 entities exceed that; the
  largest has 1,301 edges. The failure is silent and inverted — it turns
  a yes into a no:

  ```
  $ templedb entity paths AgentSession/ba3f0999-… ToolCall/1113
  (no path within depth 6 between AgentSession/ba3f0999-… and ToolCall/1113)
  $ templedb-rs entity paths AgentSession/ba3f0999-… ToolCall/1113
  ● AgentSession/ba3f0999-… — whats next in bza and any updates on bza…
    →[invoked] ToolCall/1113 — Bash (running)

    Path length: 1 hops
  ```

  There is a direct `invoked` edge between them, and `ToolCall/1000` —
  the same relation, on the same node, inside the first hundred — is
  found. Capping is defensible in `trace`, where the fan-out *is* the
  output and a short list is visibly short; it is not defensible when
  the whole answer is one yes or no. This port does not cap: BFS visits
  each entity at most once, so the uncapped cost is bounded by the
  64,562 rows of `relations`, which is the 17 ms measured above.

- **`entity search` treats the query as a LIKE pattern.** The pattern is
  built `f"%{args.query.lower()}%"` and interpolated raw, so `_` means
  "any character". `entity search deploy_` returns 858 rows against a
  ground truth of 256 — the extras include
  `class SafeDeploymentQueries`, which matched because `deploym` fits
  `deploy_`. `gui_` gives 539 against 370. 9,846 of the 55,196
  `entities` rows have an underscore in `external_ref`, so this is the
  normal case for anyone searching Python identifiers or Nix store
  paths, and nothing in the output distinguishes a wildcard hit from a
  real one. Same bug as the `file ls` prefix below, in a command where
  it is much more likely to be hit. Escaping now lives in one module
  (`like.rs`) rather than being re-derived per caller.

- **`entity search`'s `ORDER BY` has no tie-break, and it is paged.**
  The query is `ORDER BY observed_at DESC LIMIT 30`. The live `entities`
  table holds 55,196 rows across 47 distinct `observed_at` values, and
  the largest single timestamp covers 11,093 of them — ingest stamps a
  whole batch with one second. So for any query matching more than the
  limit, *which* 30 rows you are shown is the query planner's choice;
  the command can answer differently on two identical runs, and the
  first page is not the newest 30 in any meaningful sense. This port
  orders by `(observed_at DESC, kind, external_ref)`. That is the one
  intentional output difference in `entity search`: the hit sets agree
  exactly (1,064 rows for `zMothership`, byte-identical once sorted),
  the order does not.

- **`entity trace`'s fan-out cap is `LIMIT` with no `ORDER BY`.** Which
  ten of `Machine/zMothership2`'s 156 `ran` edges you see is up to the
  query planner, the same command can print different neighbours on two
  runs, and nothing in the output says it truncated. This port orders by
  `(relation kind, peer kind, peer ref)` before the cap. On current data
  the planner happens to agree, which is why the trace outputs above are
  byte-identical — that is luck, not a guarantee.

Two more, found in this repository rather than in Python:

- **The dev shell had two Rust toolchains on `PATH`.** `flake.nix`
  listed `rustToolchain` *and* `pkgs.cargo` / `pkgs.rustc` /
  `pkgs.rust-analyzer`; `which -a cargo` returned 1.99.0 from the
  overlay and 1.98.1 from nixpkgs. The overlay won only because
  `mkShell` resolves collisions by list order, so the compiler version
  was decided by where a line sat in the file. The duplicates are gone.

- **`file ls <proj> <prefix>` treated the prefix as a LIKE pattern.**
  `_` means "any character" to LIKE and appears in a third of the slugs
  and paths in this database, so the prefix is now escaped with an
  explicit `ESCAPE`. The same applies to `--rev`, where an unescaped `%`
  would match every revision and silently return the newest.

## Known gaps

Differences that are not deliberate and are not yet closed.

- **Error exits are all `1`.** Python distinguishes a malformed
  `<kind>/<ref>` (1) from an entity that does not exist (2); every error
  here leaves through one `anyhow` path and exits 1. The messages differ
  too, and go to stderr in both. Successful-but-negative exits *are*
  matched: `entity paths` exits 3 for "no path within depth N", which is
  the one a script is likely to branch on.
- **`entity search` would print a NULL `external_ref` as `(none)`**
  where Python raises `TypeError` from `{:<50}`. No row in the live
  database has a NULL one, so neither behaviour has ever been observed;
  the column is nullable, so both are reachable in principle.

## Building

```bash
nix develop            # rust-overlay toolchain, see flake.nix
cargo build --release  # ./target/release/templedb-rs
cargo test
```

Built and verified with cargo/rustc 1.99.0, pinned by `flake.lock`.
`flake.nix` asks for `rust-bin.stable.latest`, so without the lock file
the toolchain floats with whatever rust-overlay revision you fetch; the
lock is tracked for that reason.

`rusqlite` uses the `bundled` feature, so a C compiler is required (the
one `mkShell`'s stdenv provides suffices); no system SQLite is needed
and the bundled version cannot skew behaviour relative to whatever the
host ships. The `sqlite` CLI is in the dev shell anyway — checking a
query against the live database by hand is how most of the parity work
gets done.

## Layout

A workspace from the start, rather than one crate split later — the
thing being ported is 274,689 lines across 801 files over a 163-table
schema, and the Python layering (cli / services / repositories) is why a
fix in one repository file can correct a dozen commands at once. The same
split keeps SQL somewhere the CLI cannot reach around.

```
crates/templedb-db/    SQLite layer, one module per command family
  conn.rs              path resolution, pragmas
  projects.rs          project list
  files.rs             file cat / ls
  search.rs            FTS5 content search
  entities.rs          entity stats / explore / trace / search / paths
  source.rs            source snapshot / revisions
  fuzzy.rs             project name resolution
  like.rs              LIKE-pattern escaping, shared by the three
                       modules that take a user string
  testdb.rs            in-memory fixture (cfg(test))
crates/templedb-cli/   clap binary `templedb-rs`, formatting only
```

`templedb-db` re-exports `rusqlite::Connection` so the CLI crate never
depends on the driver. The point of the split is that SQL lives in one
place; a CLI that can name `rusqlite` can reach around that, and
eventually will.

## Porting order

Reads first, by how often they are used and how self-contained they are.
Writes last, and only once the read layer is trusted.

1. **Done** — projects, file read/list, entity stats, `search content`
   (FTS5), `entity explore` (one hop both directions, with its header),
   `entity trace` (multi-hop BFS, `--depth` / `--direction` / `--via` /
   `--limit`), `entity search`, `entity paths` (BFS with parent
   pointers), `source snapshot` / `source revisions`, fuzzy project name
   resolution. That is every read-only command in `entity` except
   `observations` and `dead-imports`.
2. **Next** — `summary` and `doctor` read-only invariants. These are pure
   queries and the highest-value target after search: `doctor` hashes
   roughly 400 files per run, which is work Rust does far better.
3. **Later** — `vcs log` / `diff` / `status` (read paths only).
4. **Last, and not without a design document** — any write path.

Nothing here renames the binary to `templedb`. The Python one is on PATH
under that name and every script, systemd unit and agent wrapper invokes
it; shadowing it while incomplete would break callers silently. Rename
when it can answer everything the original can.

## License

MIT
