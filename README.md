# TempleDB Rust

A Rust port of TempleDB, the project configuration and version control
system. Binary name is `templedb-rs`. Reads SQLite or PostgreSQL.

## Status: read-only subset, verified against the Python implementation

Restarted 2026-10-04. The previous contents of this repo described a port
that did not exist — it claimed "Basic CLI structure with clap" and
"Command scaffolding for project and VCS operations" while containing no
Rust at all, zero commits, and a single `src/main.py` holding
`print('hello')`. That README is gone. This one documents only what runs.

**Implemented and cross-checked against `templedb`:**

| Command | Status |
|---|---|
| `templedb-rs project list` | same 30 projects and counts; **output format differs** — see below |
| `templedb-rs project show <slug>` | byte-identical where Python does not crash — see below |
| `templedb-rs file cat <proj> <path>` | byte-identical output |
| `templedb-rs file ls <proj> [prefix] [-l]` | matches |
| `templedb-rs entity stats` | identical counts, TOTAL 35,012 |
| `templedb-rs search content <pat> [-p] [--limit]` | **deliberately different** — substring, not stemmed; see below |
| `templedb-rs entity explore <kind>/<ref>` | byte-identical, header included |
| `templedb-rs entity trace <kind>/<ref> [--depth] [--direction] [--via] [--limit]` | byte-identical |
| `templedb-rs source snapshot <proj> <path> [--rev] [--meta]` | byte-identical |
| `templedb-rs source revisions <proj> <path>` | byte-identical where Python does not crash |
| `templedb-rs entity search <query> [--kind] [--limit]` | same hit set; ordering differs on purpose |
| `templedb-rs entity paths FROM TO [--max-depth] [--direction] [--via]` | byte-identical, exit codes included |
| `templedb-rs entity dead-imports [--slug] [--limit]` | byte-identical across 5 variants |
| `templedb-rs entity observations <kind>/<ref> [--limit]` | byte-identical across 6 variants |
| `templedb-rs vcs log <proj> [-n] [--branch] [--all-sessions]` | byte-identical; Python's session-scoped default not ported |
| `templedb-rs vcs show <proj> <commit> [--no-diff] [--no-color]` | byte-identical across 39 commits in 6 projects, per-file diffs included |
| `templedb-rs vcs branch <proj>` | byte-identical across 7 projects |

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

171 unit tests, run by `cargo test`: 162 in `templedb-db` and 9 in
`templedb-cli` for the formatting the SQL layer cannot see. Three more
are `#[ignore]`d because they need `python3`: they drive CPython's
`difflib` as a reference oracle, and are described under *Porting
order* below. The 95 added
since the backend split cover the parts that have no database behind
them and would otherwise only be exercised by running against a server —
placeholder rewriting (including a `?` inside a string literal, a
doubled `''`, and the `ESCAPE '\'` whose backslash must not swallow the
closing quote), the `Value` getters refusing NULL in a required column,
backend selection from the environment, connection-string redaction, and
the mirror's table ordering and keyset pagination.

Six of them pin the batched graph walk, and they are the reason three
rewrites of it stayed byte-identical: that the fan-out cap is per node
rather than per level, that a padded `IN` list does not duplicate
edges, that each anchor gets its own edges and an anchor with none gets
no entry, and that a NULL `external_ref` — which the live database does
not contain — sorts where SQLite would put it.

Another twenty-odd cover `search content` now that matching is Rust's
job, and they are the only place several of its properties are checked
at all: that offsets survive multibyte text, that case folding stops at
ASCII (because the SQL prefilter's does), that occurrences do not
overlap, and that a file naming the search term sixty times at close
range still renders a bounded excerpt rather than most of the file —
which it did not, the first time. The database tests
run against an in-memory fixture
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

**Not implemented.** Everything else — which is most of it. No VCS
operations, no sessions, no checkouts, no ingest, no doctor, no publish,
no Nix integration, no GUI. See *Porting order* below.

No writes to the live database, either. The one write path in the binary
is `mirror`, and it writes only into a Postgres mirror it owns — never
back to the SQLite file the Python implementation has authority over.
See *Two backends*.

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

## Two backends

`templedb-db` reads through one `Store` trait with two
implementations. Set `TEMPLEDB_URL` to a Postgres connection string to
use Postgres; leave it unset and the SQLite path is unchanged. A
`TEMPLEDB_URL` that is set but is not a Postgres connection string is an
error rather than a fall-through — silently reading a different database
than the caller asked for is the worst available outcome, and it is the
same reasoning that makes an empty `TEMPLEDB_PATH` fall back to the
default instead of opening SQLite's anonymous temporary database.

```bash
psql -d templedb_mirror -f schema/postgres/001_read_subset.sql
templedb-rs mirror --to 'postgres:///templedb_mirror'
TEMPLEDB_URL='postgres:///templedb_mirror' templedb-rs project list
```

### Verified both ways

The port's method has been byte-level diffs against Python. The backend
split adds a second axis, and both were run:

- **The refactor changed no output.** The binary built from the last
  commit before any of this and the binary built from the current tree
  were diffed across 30 invocations against one frozen copy of the live
  776 MB database — `project list`, `entity stats`, `file ls` with and
  without `-l`, `file cat` of a 5,048-line file, `entity explore`,
  `entity trace` at three depths and with `--via`, `--direction in` and
  a cap of 1, `entity search` including the underscore and `--kind`
  cases, `source revisions`, `source snapshot` with and without
  `--meta`, both fuzzy-project outcomes, and 10 `entity paths`
  variants. Identical, exit codes included.
- **The backends agree.** The same 30 invocations, same frozen data
  mirrored into Postgres, are byte-identical between SQLite and
  Postgres. The `entity paths` variants cover both directions, every
  depth from 1 to 6, a matching `--via` and a bogus one, and
  source-equals-target; exit codes agree too, including the `3` for "no
  path within depth N".
- **`search content` agrees too.** The combined suite is 39
  invocations, 21 of them searches: ten single terms, two-term queries,
  a project-scoped search, a literal `%` and a literal `_`, a
  non-ASCII term, an empty query, and a term matching nothing. This is
  the command that could not be made to agree at all until matching
  left SQL — see below.

The first two diffs were re-run after each rewrite of the graph walk,
and the baseline for the regression axis is the original pre-refactor
commit rather than the preceding attempt — so the claim is end-to-end,
not a chain of pairwise comparisons that could have drifted together.
`search content` is deliberately excluded from the regression axis,
because changing its answer was the point.

The cluster used for that diff was deliberately initialised
`en_US.UTF-8`, not `C`. Under that collation Postgres sorts
`Zed, a-b, aXb, a_b, abc` where SQLite's BINARY collation gives
`Zed` last — so the agreement is produced by the schema declaring
`COLLATE "C"` on every text column, not by a cluster that happened to
be configured conveniently. Four queries sort on text and every one of
them would otherwise have returned the same rows in a different order.

That the two backends can be diffed against each other is worth more
than it looks. The Python comparison asks "does this answer the same as
the thing being replaced"; the backend comparison asks "does this
answer the same regardless of where the bytes live". A query that
passes both is pinned from two directions.

### Measured, same frozen data, 7-run average

| | SQLite | Postgres |
|---|---|---|
| `project list` | 19 ms | 10 ms |
| `entity stats` | 14 ms | 19 ms |
| `file cat` (5k lines) | 13 ms | 13 ms |
| `entity trace --depth 3` | 10 ms | 8 ms |
| `entity search` (1k hits) | 36 ms | 29 ms |
| `source revisions` | 11 ms | 13 ms |
| `entity paths` (no path, depth 6) | 15 ms | 15 ms |

The graph walk is the row that took work, and the first two attempts at
it were both wrong in instructive ways. It is written up here because
the wrong turns are the content.

**The original shape** fetched one node's neighbours per query. On
SQLite that is a function call into an embedded library and it
disappeared into the 19 ms the whole search took; on Postgres each one
is a socket round-trip, and `entity paths` exhausting the reachable
component issued 1,106 of them for 61 ms.

**Attempt one: fetch a whole BFS level per query.** This is the obvious
fix and it did cut 1,106 queries to 12 — and made Postgres *slower*,
74 ms. The frontier was passed as an `IN` list padded up to a bucketed
width so the prepared-statement cache would hit, and padding a 300-id
level to 1,024 placeholders means the server probes the index 724 times
for ids that are not in the set.

**Attempt two: pass the frontier as a `bigint[]`.** One parameter,
constant query text, no padding. 53 ms — better, but still 3× SQLite,
which meant the round-trips had never been the problem. `EXPLAIN
ANALYZE` said so plainly: the index-only scan that found the 1,625
edges took **0.26 ms**, and the other 22.7 ms was Postgres sequentially
scanning all 60,719 rows of `entities` to build a hash table for the
join. The planner was not misinformed — `ANALYZE` and
`random_page_cost = 1.1` were both tried and neither changed the plan —
it genuinely costs 4,410 estimated index probes above one scan of a
small table. It is just that the hash build was the entire cost and the
probes would all have been cache hits.

**What is there now** does not ask for the join. One query fetches the
edges (that 0.26 ms index-only scan), a second fetches the peer rows by
primary key, and Rust joins them in a `HashMap`. Postgres drops from
61 ms to 15 ms, matching SQLite, and the deduplication is a bonus: an
entity that twenty edges point at is fetched once instead of twenty
times.

Sorting moved to Rust with it, and that part is better rather than
merely faster. `str`'s ordering is byte ordering, which is exactly what
SQLite's BINARY collation and Postgres's `COLLATE "C"` both give, so
neighbour order is now identical across backends *by construction*
rather than by two `ORDER BY` clauses happening to agree. It also
retired a latent divergence — see below.

Nothing about the traversal moved into SQL. The visited set, the
ordering and the per-node fan-out cap are all still decided in Rust,
which is what let the 30-invocation diff stay byte-identical through
three rewrites. A recursive CTE would have pushed the walk into the
database and with it the visited-set and emission-order semantics
`trace` matches Python on — a much larger behavioural risk for a
latency win that turned out to be available without it.

One rewrite did break something, and the test suite caught it: sorting
each anchor's edges after both direction passes had contributed
reordered across the out-before-in grouping, because `installs` sorts
ahead of `used-by`. That grouping is what decides whether a hop prints
as `→` or `←`, so `entity paths` started reporting a different (equally
short) path. The sort now happens per direction pass.

### A NULL-ordering divergence the live data cannot expose

`external_ref` is nullable, and the two engines sort NULLs oppositely:
SQLite puts them first on an ascending sort, Postgres puts them last.
Any `ORDER BY` touching that column therefore returned the same rows in
a different order per backend.

No row in the live database has a NULL `external_ref`, so no amount of
diffing against real data would have found this. The neighbour ordering
is immune now that it happens in Rust (`None` sorts before `Some`,
matching SQLite). `entity search` still sorts in SQL and says
`NULLS FIRST` explicitly. A unit test inserts the NULL row the live
database does not have, because that test is the only thing that can
catch a regression here.

### `search content`: why matching had to leave SQL

This was the command the backend split could not absorb, and the way it
resisted is the most useful thing in this file.

It was built on FTS5: `MATCH` for the predicate, `snippet()` for the
excerpt, `ORDER BY rank` for bm25. The plan was to keep each backend's
index for matching and move only ranking and snippet rendering into
Rust, so that the output *format* would at least agree. Then the premise
got measured. Against the same 2,063 text files:

| query | FTS5 | Postgres `tsvector` |
|---|---|---|
| `deployment` | 356 | 449 |
| `checkout` | 273 | 244 |
| `generated` | 602 | 532 |
| `queries` | 635 | 532 |
| `session` | 459 | 467 |

FTS5 stems with the original Porter algorithm, Postgres's `english`
configuration with Porter2, so they disagree about which words share a
stem — `deployment` and `deploy` are one term to one and two to the
other. A command built on those answers up to a quarter differently
depending on which database it is pointed at, with nothing in the output
to say so. Moving only the rendering would have made the two outputs
*look* identical while still listing different files, which is worse
than the honest refusal it replaced.

So matching moved too. SQL narrows candidates with a case-insensitive
substring test; `textsearch.rs` decides what matches, scores it with
BM25 and renders the excerpt. All 20 search invocations in the
cross-backend diff are byte-identical.

**The cost is a semantics change**, and it is visible:

| query | FTS5 | now | why |
|---|---|---|---|
| `rusqlite` | 13 | 13 | unchanged |
| `checkout` | 273 | 273 | unchanged |
| `deployment` | 356 | 367 | substring also finds `redeployment` |
| `run` | 804 | 915 | also finds `runtime`, `runner`, `prerun` |
| `running` | 804 | 313 | **no stemming**: FTS5 read this as `run` |
| `deploy_` | 343 | 123 | **no tokenising**: FTS5 dropped the `_` |

The last two are the interesting ones. `running` lost two thirds of its
hits because FTS5 was never searching for `running` — it stemmed the
query to `run` and returned every file containing any form of it.
`deploy_` lost most of its hits because `unicode61` treats `_` as a
separator, so the query became the token `deploy` and matched
`deployment`, `deployed`, `redeploy`. Both old answers were defensible
for English prose and neither was what someone searching source code
typed. This is the same class of surprise the README already records for
`entity search`, arrived at from the other direction.

Substring is also the only semantics reproducible *exactly* without
shipping a stemmer that has to agree with two others, which is what
makes one answer across backends possible at all.

What is gone: FTS5 query syntax. No `OR`, no `NEAR`, no prefix `*`.
Whitespace-separated terms are ANDed and double quotes group a phrase.
Those operators were a third query language, and supporting them would
put the dialect back into the part of the command that decides the
answer.

**And it is slower**, which is the honest other half:

| | FTS5 before | SQLite now | Postgres now |
|---|---|---|---|
| `rusqlite` (13 files, 160 KB) | 36 ms | 197 ms | 218 ms |
| `checkout` (273 files, 9 MB) | 36 ms | 221 ms | 400 ms |
| `run` (915 files, 29 MB) | 36 ms | 305 ms | 581 ms |

An index lookup became a scan, and ranking needs every candidate's
text, so the cost tracks bytes matched rather than hits returned. For
context, the Python implementation answers the same queries in 857 ms
with FTS5, so this remains faster than what it replaces — but it is a
regression against this port's own previous figure, and that is the
trade for one answer across backends.

(The host was loaded when these were taken: `project list`, whose code
has not changed, measured 91 ms in the same session against 19 ms
earlier. Read the absolute figures as upper bounds; the comparisons
below were measured interleaved in one session and are not affected.)

#### Getting Postgres back under control

The first version of the prefilter cost 1.5 s on Postgres — worse than
the Python implementation. Three things were tried, and the order
matters because the obvious two were the wrong ones.

**The planned optimisation was wrong, and measuring it was the only way
to find out.** The plan was to compute term frequency in SQL and fetch
full text only for the rows surviving ranking, cutting `run`'s 29 MB
transfer to about 25 files' worth. Measured against the real thing:

| | time |
|---|---|
| prefilter alone (the floor) | 1,341 ms |
| prefilter + term frequency in SQL | 2,246 ms |
| prefilter + all matching text returned | 1,458 ms |

The transfer was 120 ms of the 1.5 s. The prefilter was 90% of it, and
computing frequency server-side *added* 900 ms of `LOWER` and `REPLACE`
passes. The optimisation would have made it slower.

**The trigram index was not being used.** `EXPLAIN ANALYZE` showed a
sequential scan over `content_blobs` with `ILIKE` as a filter, 1,500 ms.
That is the planner being right: `%run%` is a single trigram present in
nearly every document, so the index offers nothing and a scan wins. The
cost was `ILIKE` itself, which is several times slower per byte than a
case-sensitive match.

**What worked** was to stop asking Postgres to match case-insensitively
at all. The mirror now carries `text_lower`, a `GENERATED ALWAYS AS
(lower(content_text)) STORED` column, with the trigram index on that,
and the prefilter is a case-sensitive `LIKE`. Interleaved A/B in one
session, both forms given their own trigram index, answers verified
byte-identical:

| query | `ILIKE content_text` | `LIKE text_lower` |
|---|---|---|
| `rusqlite` (13 files) | 276 ms | 222 ms |
| `universal` (26) | 473 ms | 307 ms |
| `checkout` (273) | 978 ms | 398 ms |
| `deployment` (367) | 1566 ms | 564 ms |
| `run` (915) | 1481 ms | 591 ms |

The cost is storing the text twice — `content_blobs` goes to 104 MB —
which is acceptable in a read replica rebuilt by one command, and the
mirror needs no knowledge of the column because Postgres generates it.

SQLite needs none of this: its `LIKE` is already ASCII case-insensitive.
So the column the prefilter reads is the one genuinely backend-specific
expression in this query. The candidate *sets* are not, which is what
the 39-invocation diff checks.

One subtlety worth recording, because it is what lets the prefilter
differ per dialect at all. The prefilter does not have to *agree* with
the matcher — only to be no narrower. SQLite's `LIKE` folds ASCII;
Postgres's `ILIKE` folds the full Unicode range and hands over more
than `textsearch` accepts, and the surplus is discarded when the
matcher finds no occurrence. Had the authority folded more than the
prefilter, the asymmetry would run the other way and SQLite would
silently miss files.
### What a mirrored Postgres is and is not

It is a read replica and a parity target, exactly as stale as the last
`mirror` run. It is not a source of truth. `mirror` is the one write
path in the binary and it writes only to the destination: SQLite is read
through the same read-only handle every other command uses, so the
Python implementation's exclusive authority over the live file is
untouched. The mirror is downstream of that authority, never a second
claimant on it.

Making Postgres primary means translating the rest of the schema — 163
tables, 45 views, 61 triggers, 361 indexes, against the 8 tables and 1
view the port reads today — and porting the write paths, which is the
design document this port has been deferring. The mirror does not
quietly accomplish either.

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
- **`search content` is slower than it was**, ~200–300 ms on SQLite and
  ~220–580 ms on Postgres against FTS5's 36 ms, depending on how many
  bytes the term matches. An index lookup became a scan, and ranking
  needs every candidate's text. Still faster than the Python
  implementation's 857 ms. No further optimisation is queued: the
  obvious one — computing term frequency in SQL — was measured and
  makes it slower, and the measurements are above.
- **The Postgres mirror stores file text twice**, once as written and
  once lowercased for the search prefilter. 104 MB for `content_blobs`
  against roughly half that. Worth it — see the A/B above — but it is a
  real cost and it only buys speed, not a different answer.
- **`search content` no longer accepts FTS5 operators.** No `OR`,
  `NEAR` or prefix `*`. Terms are ANDed, quotes group a phrase.
- **A freshly mirrored Postgres has no statistics.** `mirror` uses
  `COPY` and does not `ANALYZE`, so until autovacuum gets to it the
  planner works from defaults. It did not turn out to matter for the
  query that prompted the investigation — the plan was the same before
  and after — but it is a real gap, and `ANALYZE` after a mirror run is
  worth doing.
- **The Postgres schema covers 11 tables and 1 view.** That is the whole
  set the port reads, out of 163 tables and 45 views. Columns are
  likewise limited to the ones the queries name. Anything not yet
  ported has nothing to mirror and no way to verify a mirror, so the
  boundary follows the readers.

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
gets done, and it is why the SQL in the query modules is left as text
you can paste rather than being assembled by a query builder.

The Postgres client is the synchronous `postgres` crate, not
`tokio-postgres` and not `sqlx`. Async would make all fifteen public
functions in `templedb-db` `async` for a CLI that exits after one query,
with no concurrency to recover the cost from. `sqlx` additionally wants
a live database at compile time for its query checking, and a port whose
`cargo build` fails when a server is down is worse than one with
unchecked SQL. Neither backend is behind a cargo feature, deliberately:
a SQLite-only build would never type-check the Postgres code, and a
configuration in which half the crate is invisible to `cargo test` is
pre-arranged drift.

No Postgres server is needed to build or to run `cargo test`.

## Layout

A workspace from the start, rather than one crate split later — the
thing being ported is 274,689 lines across 801 files over a 163-table
schema, and the Python layering (cli / services / repositories) is why a
fix in one repository file can correct a dozen commands at once. The same
split keeps SQL somewhere the CLI cannot reach around.

```
crates/templedb-db/    data layer, one module per command family
  store.rs             trait Store — the backend boundary
  value.rs             owned Value / Row; no driver type escapes
  dialect.rs           ?N -> $N rewriting, literal-aware
  backend/sqlite.rs    rusqlite, pragmas, prepare_cached
  backend/pg.rs        postgres (sync), OID->Value, statement cache
  conn.rs              path/URL resolution, backend selection
  mirror.rs            SQLite -> Postgres copy (the one write path)
  projects.rs          project list
  files.rs             file cat / ls
  search.rs            search content — candidate SQL + orchestration
  textsearch.rs        matching, BM25, snippets; no SQL, no backend
  entities.rs          entity stats / explore / trace / search / paths
  source.rs            source snapshot / revisions
  fuzzy.rs             project name resolution
  like.rs              LIKE-pattern escaping, shared by the three
                       modules that take a user string
  testdb.rs            in-memory fixture (cfg(test))
schema/postgres/       the DDL the mirror targets
crates/templedb-cli/   clap binary `templedb-rs`, formatting only
```

Nothing from `rusqlite` or `postgres` is re-exported. The previous
revision of `lib.rs` exported `rusqlite::Connection` "so the CLI crate
never names rusqlite itself", with a comment conceding that a CLI able
to name the driver would eventually reach around the SQL layer. A second
backend settled it: there is no longer a single driver type to export,
and the CLI holds a `dyn Store`, which it cannot run SQL through without
coming here first. The change cost two lines in `main.rs`, which is the
clearest evidence the original split was drawn in the right place.

`Store` carries `query` and nothing else. The alternative — a method per
command, implemented once per dialect — looks cleaner and was rejected:
it would mean two copies of every query when fewer than a tenth of them
differ, and two copies drift. A drift between backends is exactly the
class of bug this port exists to catch, so one query string that both
engines run is a stronger guarantee than two strings and a test
asserting they agree. The dialect differences with no behavioural
content (placeholders) are rewritten centrally; the one that changes
which rows come back (`search content`) is a visible refusal in the
module that owns it, not something hidden behind a rewriter that makes
it look free.

## Porting order

Reads first, by how often they are used and how self-contained they are.
Writes last, and only once the read layer is trusted.

1. **Done** — projects, file read/list, entity stats, `search content`
   (FTS5), `entity explore` (one hop both directions, with its header),
   `entity trace` (multi-hop BFS, `--depth` / `--direction` / `--via` /
   `--limit`), `entity search`, `entity paths` (BFS with parent
   pointers), `source snapshot` / `source revisions`, fuzzy project name
   resolution.
2. **Done** — the backend split: `Store`, the Postgres implementation,
   the read-subset DDL, and `mirror`. Taken before porting more
   commands on purpose, because it is cross-cutting: doing it now meant
   translating ten queries instead of the twenty-five that `doctor` and
   `vcs` would have added first.
3. **Done** — `search content` in Rust: matching, BM25 ranking and
   snippet rendering. Intended to be ranking and snippets only, but
   measuring the two full-text indexes showed they disagree on the hit
   set, so matching had to move as well. This is the port's first
   deliberate change to *what a command answers* rather than how it
   says it.
4. **Done** — `entity observations` and `entity dead-imports`, which
   completes every read-only command in the `entity` family.
   `observations_archive` is the ninth table in the Postgres subset.
5. **Done** — `vcs log`, with `vcs_branches` as the tenth table. Its
   session-scoped default is deliberately absent: see below.
6. **Done** — `vcs show`, metadata and changed-file list. Its per-file
   diff followed in step 8.
7. **Done** — `project show` and `vcs branch`, the two deterministic
   read commands left outside the difflib question. `project show`
   needed four more mirrored columns and `vcs branch` three.
8. **Done** — CPython's `difflib.SequenceMatcher`, and with it
   `vcs show`'s per-file diffs. This was listed as a decision rather
   than a command and the decision was to reimplement; see below.
   `vcs_commit_parents` is the eleventh mirrored table.
9. **Next** — `vcs diff`, which now needs no new algorithm. Three forms:
   `vcs diff <proj> <file>` (newest two revisions), `vcs diff <proj>
   <file> <c1> <c2>`, and `--context` for `difflib.context_diff`, which
   is a renderer over the same `grouped_opcodes` this port already has.
   `vcs status` stays out: staging is session-scoped and `--refresh`
   writes working state.
10. **Last, and not without a design document** — any write path, and
    with it the rest of the Postgres schema. Postgres becoming the
    primary store is the same project as porting the writes; the mirror
    is a read replica and does not change that.

### difflib, reimplemented rather than approximated

`vcs show`, `vcs diff` and `vcs diff --staged` all render through
`difflib.unified_diff`, so one answer unblocked three commands. The
answer was to port the algorithm, and the measurement that ruled out the
alternative was taken before any code was written.

difflib's matcher is not Myers. It is a recursive longest-matching-block
search with its own tie-breaks and an "autojunk" heuristic that excludes
elements appearing in more than 1% of positions once a sequence passes
200. Taking 28 real consecutive-revision pairs from this database and
comparing difflib against `diff -u`:

| | pairs |
|---|---|
| identical output | 12 |
| **different output** | **16** |

Six of the sixteen differ at the *same line count* — 136 vs 136, 18 vs
18, 79 vs 79, 52 vs 52, 63 vs 63, 94 vs 94 — so the hunks are grouped
and placed differently rather than merely sized differently. It is not a
near miss, and `similar` and `diffs` both implement Myers or Patience,
so neither can close it.

`crates/templedb-db/src/difflib.rs` is therefore a port of
`find_longest_match`, the recursive block search, adjacent-block
merging, `get_opcodes`, `get_grouped_opcodes` and `unified_diff`.
`isjunk` is left out: the call site passes `None`, which makes two of
`find_longest_match`'s four extension loops dead.

**Autojunk is the part that matters.** Past 200 elements, any line
occupying more than 1% of positions is dropped from the index, so it can
no longer *seed* a match — but the extension loops test equality
directly and still absorb it, so a match runs *through* a popular line
without beginning at one. The popular set is not the junk set, and
conflating the two is the easiest way to produce plausible diffs that do
not match Python's: it only shows up on files long enough for the
threshold to fire. A file full of blank lines or bare braces diffs
differently above 200 lines than below.

It also ports `str.splitlines`, which is not `str::lines`. Python treats
a form feed, a bare CR, U+0085, U+2028 and three C1 separators as line
boundaries. A form feed is a conventional page break in older C and
Emacs Lisp, and missing one would shift every line number after it.

#### How it is verified

Two ways, because the unit tests only pin what the author thought to
write down.

`cargo test -- --ignored` runs CPython's `difflib` as a reference oracle
over 720 generated pairs: a skewed line alphabet so autojunk actually
fires, lengths of 199/200/201 to straddle its threshold, clustered and
distant edits to exercise hunk grouping. Plus constructed cases with
repeated blocks, where two alignments are equally long and the tie-break
decides the answer, and this repo's own source files with lines dropped,
blocks moved and the whole file reversed. All agree byte for byte with
difflib on CPython 3.14.7.

That harness was then checked against itself by mutation, because a
parity test that passes on its first run may be vacuous. Disabling
autojunk fails 2 of its 3 cases; inverting the longest-match tie-break
from `>` to `>=` fails all 3.

End to end, `templedb-rs vcs show` is byte-identical to
`templedb vcs show` across 39 commits in 6 projects, with colour on and
off, the largest being a 15,171-line diff. Root commits are included:
they have no parent row, so the file is diffed against the empty string
and the header reads `∅`, which is Python's marker. A 4,816-line
whole-file addition agrees exactly.

One reproduced wrongness worth naming, because it is on stdout: a
**binary** file in a commit diffs as `No differences`. Python's content
lookup returns nothing for any blob whose `content_type` is not `text`,
the caller turns that into the empty string, and empty-against-empty is
no change — so a binary file that did change reports that it did not.
This port does the same thing, and `content_at_commit`'s doc comment
records why.

### Two bugs found porting it

**`vcs show`'s commit lookup is a LIKE injection, and this one needs no
luck.** Python resolves the commit with
`c.commit_hash = ? OR c.commit_hash LIKE ?`, where the second parameter
is built by interpolation — `f"{args.commit}%"` — with no escaping and
no `ESCAPE` clause:

```
$ templedb vcs show templedb '%' --no-diff
commit E067215FD8DAA5D38CA956734405C574594D3E8F7DB5EDF914D80EDBB6400F83
Branch: master
```

That is an arbitrary commit presented as the one you asked for; `_` does
the same. This is the fifth instance of the unescaped-LIKE mistake in
the codebase and the first reachable by typing one character — the
others needed an underscore in real data (`file ls`, `entity search`,
`search content`) or a particular pair of slugs (`entity dead-imports`).
Escaped here, so `%` matches nothing and the command reports the commit
as not found.

The query also has no `ORDER BY`, so among several commits sharing a
prefix Python returns the planner's choice. This port orders by
`commit_hash`, which makes an ambiguous prefix resolve the same way
every run. It does not *report* the ambiguity — git refuses such a
prefix outright, which is better, but that changes the output shape
rather than making it deterministic, and determinism was what was
missing.

**The binary panicked on a closed pipe.** Rust's runtime sets `SIGPIPE`
to `SIG_IGN` before `main`, so a write to a closed pipe returns `EPIPE`
and `println!` panics on it:

```
$ templedb-rs file cat templedb src/cli/commands/entity.py | head -2
thread 'main' panicked at library/std/src/io/stdio.rs:1168:9:
failed printing to stdout: Broken pipe (os error 32)
```

Not introduced by any one command — it is how the binary has always
behaved, and it reproduced on `file cat`, `project list` and
`entity search` the moment anything was piped into `head`. `entity
stats` happened not to trigger it only because its output fits in the
pipe buffer before the reader exits, which is the kind of accident that
keeps a bug like this unnoticed.

Python is not uniform here either, and the gap is smaller than it first
looked: `project list` exits quietly, `file cat` catches the error and
logs `ERROR Failed to show file: [Errno 32] Broken pipe`. Neither is a
panic with a backtrace hint. Restoring the default `SIGPIPE`
disposition is better than both — the process dies by signal, the shell
reports it as such, nothing is printed, on every command.

### `project show` crashes on a third of the projects

Python formats its line count with a thousands separator:

```python
print(f"Lines of code: {stats['total_lines']:,}")
```

`total_lines` is `SUM(lines_of_code)`, and `SUM` over no non-NULL values
returns NULL. `format(None, ',')` raises, so the command prints six of
its eight lines and exits 1:

```
$ templedb project show cathedraldb
Error: unsupported format string passed to NoneType.__format__

Project: cathedraldb
Name: CathedralDB - Share TempleDB Projects
Repository: templedb://cathedraldb
Branch: None

Files: 0
```

**Ten of the thirty projects** are in this state, and they are not all
empty ones. `prediction_dashboards` has 32 files, 8 file types, a branch
and a commit — and Python never prints any of that, because it dies on
the line before:

| | Python | this port |
|---|---|---|
| `cathedraldb` | 7 lines, exit 1 | 10 lines, exit 0 |
| `prediction_dashboards` | 7 lines, exit 1 | 13 lines, exit 0 |

It is the same shape as the `source revisions` crash recorded below —
partial output, then a `NoneType` on a column that is nullable by
design. This port prints `0`, which is the honest answer to "no file
records a line count", and the output is byte-identical to Python's for
every project where Python completes.

Two oddities in the same command are reproduced rather than fixed,
because both are on stdout and neither is wrong so much as surprising.
`Files:` counts every `project_files` row regardless of `status`, so it
includes deleted files and disagrees with `project list`, which filters
`status = 'active'`. And `VCS Commits:` is counted through
`vcs_branches`, so it counts commits on the project's *branches* rather
than commits carrying the project's id — the two differ for a commit
whose `branch_id` points at another project's branch.

`project show` is also the one project argument that is **not** fuzzy
matched, in Python either: it calls `get_by_slug` where every other
command calls `fuzzy_match_project`, and its `--help` says "Project
slug" rather than "Project name or pattern". Kept exact, because making
it fuzzy would change which project an existing script resolves to.

### `project list` prints a different table

Worth stating plainly because the status table above used to say only
"matches". The two agree on the data — 30 projects, same file and line
counts — and disagree on everything about the presentation. Python
prints a blank line, a `Projects` heading, a `slug name file_count
total_lines` header and a rule; this prints a `SLUG FILES LINES` header
and the rows. Python also has a `name` column that this omits, and
renders a project with no current snapshot as `None` where this renders
`0` (the deliberate `COALESCE` divergence recorded under *Bugs found
while porting*).

So `project list` is the one implemented command that is **not** a
candidate for byte-level diffing against Python, and the status table
now says so. Nothing depends on it matching; it is listed here because
a reader comparing the two outputs deserves to know before they try.

### `vcs log` has no session-scoped default, and says so

Python's `vcs log` has three views. `--published` shows commits with
`session_id IS NULL`; `--all-sessions` shows everything; and the
**default** shows published commits *plus the ones owned by your current
staging session*.

That default is not ported, because resolving "your session" goes
through six fallbacks — a `--session` override, `TEMPLEDB_SESSION_ID`,
an instance cache, `TEMPLEDB_SESSION`, a lookup keyed on the
session-leader PID within the last 24 hours, then one keyed on the PPID
— and **creates a session row** if none of them hit. Environment-
dependent and a write, so neither half belongs in this crate.

So `templedb-rs vcs log` defaults to the published view, and prints
which view it used in the header exactly as Python does. That makes the
difference visible in the output rather than something you have to know:

```
Commit log for templedb [published only]
```

against Python's default of `[default view (published + your
unpublished)]`. `--all-sessions` consults no session at all and is
byte-identical to Python's.

One thing Python prints that this does not: under `--published` it adds
a `N pending commit(s) in your session` advisory. That needs the session
too.

### The tie-break, and getting it backwards first

`vcs log` has the same latent defect as `entity search` and
`entity observations`: `ORDER BY commit_timestamp DESC` alone, and the
timestamps tie — 1,196 commits across 1,186 distinct values. With `-n`,
which of a tied group survives the cutoff is the planner's choice.

I first broke the tie with `id DESC`, reasoning that a newest-first log
should be newest-first inside a tie too. That was wrong twice over, and
the diff against Python is what showed it.

It buys nothing: the defect is that Python's order is *unspecified*, and
`ASC` and `DESC` are equally total, so both fix it. Between two fixes,
the one that also agrees byte for byte with the implementation being
replaced is strictly better. The ordering divergences this port does
carry — `entity search`, `entity trace` — exist because no ordering was
both stable and faithful. Here one is.

And the data disagreed. `slimevr_fitness_arena` has two commits at
`2026-05-30 19:54:32`: id 80 is "Initial commit: SlimeVR Fitness Arena
project", id 81 is "Scaffold source code". Ascending shows them in the
order they happened; descending shows a project scaffolded before it was
created. Within a tie the rows are a sequence, not a set — which is the
same conclusion `entity observations` reached, from the opposite
direction.

The lesson generalises: a tie-break is a question about the data, not
about symmetry with the column above it.

### `files_changed` is computed and thrown away

Python's commit-history query carries a column this port omits:

```sql
COALESCE((SELECT COUNT(*) FROM vcs_file_states WHERE commit_id = c.id),
         (SELECT COUNT(*) FROM commit_files    WHERE commit_id = c.id))
```

Two independent problems. **Nothing reads it** — it is not printed by
the human formatter and not placed in the dict the JSON formatter
emits, so it is a correlated subquery evaluated once per returned commit
whose result is discarded. And **the fallback is unreachable**:
`COUNT(*)` over an empty set returns `0`, not `NULL`, so the first
branch always wins. The intent was plainly "use the newer table, fall
back to the legacy one"; the effect is "use the newer table, and report
0 when it has no rows". 354 commits have no `vcs_file_states` rows at
all, and `commit_files` holds 6,146 rows that this expression can never
consult.

Omitted rather than reproduced, because mirroring `commit_files` to
compute a discarded value is work for nobody. If a caller ever wants the
count, it wants a real fallback, tested against those 354 commits.

### Why `summary` and `doctor` are not next

An earlier revision of this list had them as step 4, justified by "`doctor`
hashes roughly 400 files per run, which is work Rust does far better."
That is not true and the claim is withdrawn. `doctor` has two
subcommands, `entities` and `history`; `entities` runs entity-graph
invariant SQL and hashes nothing. The 400-file hashing belongs to a
checkout/diff path elsewhere in the tree.

Both are also poorer targets than they look:

- **`doctor entities` is a write path.** It `INSERT`s every result into
  `invariant_checks` and asserts a claim on success. That puts it behind
  the write-path design document, not in front of it. `doctor history`
  is a pure read but is a single `SELECT` over a table nothing else
  needs yet.
- **`summary` cannot be verified by this port's method.** Its output
  carries relative ages ("last 3h ago") computed from the clock, and a
  session id derived from `os.getppid()`, so two runs a second apart
  disagree with each other and a byte diff against Python is
  meaningless. It also reads five tables outside the mirror subset
  (`ingestion_runs`, `invariant_checks`, `reconcile_runs`,
  `fleet_machines`, `handoff_notes`) and shells out to systemd for one
  of its panes.

None of that makes `summary` unportable — it makes it the first command
needing a different verification approach, probably injected time and a
fixed session id, which is its own piece of work rather than a
precondition smuggled into a command port.

Nothing here renames the binary to `templedb`. The Python one is on PATH
under that name and every script, systemd unit and agent wrapper invokes
it; shadowing it while incomplete would break callers silently. Rename
when it can answer everything the original can.

## License

MIT
