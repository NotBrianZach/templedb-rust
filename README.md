# TempleDB Rust

A Rust port of TempleDB, the project configuration and version control
system. Binary name is `templedb-rs`. Reads SQLite or PostgreSQL.

## Status: the read-only subset, plus seven opt-in writes

Restarted 2026-10-04. The previous contents of this repo described a port
that did not exist — it claimed "Basic CLI structure with clap" and
"Command scaffolding for project and VCS operations" while containing no
Rust at all, zero commits, and a single `src/main.py` holding
`print('hello')`. That README is gone. This one documents only what runs.

**Implemented and cross-checked against `templedb`:**

| Command | Status |
|---|---|
| `templedb-rs project list [--nix-only] [--category] [--type]` | same 30 projects and counts; all three filters select the same projects Python does; **output format differs** — see below |
| `templedb-rs project show <slug>` | byte-identical |
| `templedb-rs file cat <proj> <path>` | byte-identical output |
| `templedb-rs file ls <proj> [prefix] [-l] [--edit-mode]` | byte-identical |
| `templedb-rs file where <proj> <path>` | **the first command that reads a filesystem.** Byte-identical across its 8 sweep rows — 6 that read a tree and 2 that exit 2 before the probe loop — including the colour escapes, all three exit statuses and the `os.walk` ordering of overlay hits. Both mirror roots are resolved from the `checkouts` table since 2026-11-15, replacing two hardcoded paths that were wrong in opposite directions — see *The resolver the hardcoded paths were standing in for* |
| `templedb-rs entity stats` | byte-identical, both sections |
| `templedb-rs search content <pat> [-p] [--limit]` | **deliberately different** — substring, not stemmed, and a different layout; Python's `-i` and `--no-fts` not ported; see below |
| `templedb-rs search files <pat> [-p]` | byte-identical across 8 sweep rows, both column layouts, the literal word `None` and the trailing padding included. **One deliberate divergence:** `_` and `%` are literals here and LIKE wildcards in Python — `search files get_` returns ten rows upstream of which none contains `get_`. See *`search files`, and the order Python does not have* |
| `templedb-rs entity explore <kind>/<ref>` | same edge set; **ordering differs on purpose** — see below |
| `templedb-rs entity trace <kind>/<ref> [--depth] [--direction] [--via] [--limit]` | byte-identical, truncation advisory included |
| `templedb-rs source snapshot <proj> <path> [--rev] [--meta]` | byte-identical |
| `templedb-rs source revisions <proj> <path>` | byte-identical |
| `templedb-rs entity search <query> [--kind] [--limit]` | byte-identical, ordering included |
| `templedb-rs entity paths FROM TO [--max-depth] [--direction] [--via]` | byte-identical, exit codes included |
| `templedb-rs entity dead-imports [--slug] [--limit]` | byte-identical across 5 variants |
| `templedb-rs entity observations <kind>/<ref> [--limit]` | byte-identical across 6 variants |
| `templedb-rs vcs log <proj> [-n] [--branch] [--session] [--all-sessions] [--published]` | byte-identical, **all three views**, with the session-scoped default closed 2026-10-23 — see *Sessions* |
| `templedb-rs vcs show <proj> <commit> [--no-diff] [--no-color]` | byte-identical across 39 commits in 6 projects, per-file diffs included |
| `templedb-rs vcs branch <proj>` | byte-identical across 7 projects |
| `templedb-rs vcs diff <proj> [file] [c1] [c2] [--staged] [-s] [--no-color]` | byte-identical across 300 runs, and **`--staged` plus the working-set form landed 2026-10-31** — all five forms now, at no schema cost. See *The two forms that were never blocked* |
| `templedb-rs summary [--no-color]` | **seven of Python's nine panes**; six of the seven byte-identical on live data, the seventh a deliberate tie-break — see below |
| `templedb-rs doctor history [--check] [--violated-only] [--limit]` | byte-identical across 9 variants, exit codes and the empty-table message included. `doctor entities` is **not** ported — see *Why `doctor entities` is not a command this port can have* |
| `templedb-rs intent list [-p] [-s] [--all-statuses] [--limit]` | byte-identical across 8 variants; **one deliberate ordering divergence** — this has a tie-break and Python does not. See *The tie-break Python does not have* |
| `templedb-rs intent show <id>` | byte-identical across 3 variants, exit 1 on a missing id included |
| `templedb-rs intent apply <id> [--no-rebase]` | **the fifth write**, 2026-11-18, and the last of the family — `intent` is now complete at 5 of 5. No new table and no new column. Its success path is byte-identical to Python's, measured on a copy; all four of Python's exit statuses reproduced. **One deliberate divergence:** a base the file has moved past exits 4 *always*, where Python three-way merges — because the engine it merges with is not a three-way merge and four of its "clean" results write content neither side held. See *The rebase this port refuses to do* |
| `templedb-rs intent cancel <id>` | **the third write**, and the smallest — no new table, no new column. Postgres only, opt-in. A compare-and-swap where Python checks and then acts; both exit statuses reproduced. Its success line was measured byte-identical against Python on 2026-11-17, by step 31's copy technique — it had been written from Python's source and never compared |
| `templedb-rs intent create <proj> <path> [-c] [-f] [--base-rev] [-m]` | **the fourth write**, 2026-11-17, and the first whose stdout was measured against Python rather than read off its source: byte-identical to Python's, id for id, by running Python against a *copy* of the live database. No new table and no new column. Writes `edit_intents` and `content_blobs` and nothing else — no source change, which is what makes it safe while `apply` is unported. Exact slug, not fuzzy. See *`intent create`, and the write whose stdout could be measured* |
| `templedb-rs file set <proj> <path> [-c] [--if-match] [--verify] [--allow-mojibake] [--skip-intent] [-s] [--session] [--commit] [-m]` | **the first write.** Postgres only, opt-in, six of Python’s seven guards, text **and binary** content — see *The first write* |
| `templedb-rs vcs commit-staged <proj> {-m\|-F} [-b] [--author] [--session]` | **the second write.** Postgres only, opt-in; the database half of Python's `vcs commit`, with the tree-dependent half deferred and the name saying so — see *The second write* |
| `templedb-rs file rm <proj> <path>` | **the sixth write**, 2026-11-18. No new table and no new column. Stages a deletion and removes nothing: `file cat` still answers until `vcs commit-staged` hard-deletes the row. Its one line of stdout is byte-identical to Python's, measured on a copy *and then reproduced through the shipping binary*; all four of Python's exit statuses reproduced. **One deliberate divergence:** no session is a refusal, where Python creates one. No flags on either side. See *The deletion that deletes nothing* |
| `templedb-rs file retype [proj] [--from-type]` | **the dry half, and a read** — byte-identical across 9 sweep rows and 110 of 110 cross-backend, exit 1 on an unresolved project included. The classifier it is a function of is `importer.scanner`'s 179-entry pattern table, ported as `classify.rs` and pinned against CPython over 24,757 generated paths with **0 mismatches on either function**. Zero schema cost. **One deliberate ordering divergence:** Python's query has no `ORDER BY` and `most_common` breaks ties by row order, and the 25-kind cut falls *inside* a tie group on live data — so this adds `ORDER BY pf.id`. **`--apply` is not ported**, and the flag is absent rather than inert: it is the only behaviour that writes, and `file_types.category` is `NOT NULL` on a table this port mirrors at two of five columns. See *The classifier, and the write that needs two more columns* |
| `templedb-rs file mark <proj> <path> --edit-mode` | **the seventh write**, 2026-11-18, and the smallest — one column on one row, no new table and no new column. **All four of its stdout lines are byte-identical to Python's**, measured on a copy and then diffed against Python's captured bytes through the shipping binary with nothing substituted; all four of Python's exit statuses reproduced, and the flag surfaces agree exactly with no allow-list entry. **No deliberate divergence in its own behaviour** — the first write here with none — but two things it inherits: Python's success line names the project the *caller typed*, which fuzzy matching lets name a project that does not exist, and no `project_files` write in this port moves the `updated_at` that a trigger moves upstream. Both in *Known gaps*. See *The mode that is spelled by a NULL* |

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
3 for "no path within depth N", 2 for an endpoint that does not exist
and 1 for one that is not an address. The last two were 1 and 1 until
2026-10-17; see *The error statuses*.

`entity search` used to agree on *which* entities match and
deliberately disagree on their order, because Python's `ORDER BY` had no
tie-break. Python has one now, and the two outputs are byte-identical
unsorted — the port's one intentional ordering divergence has closed by
the other side converging on it. `entity explore`, which has no
`ORDER BY` in Python at all, has opened in its place; see below.

The `label` / `source_authority` / `observed_at` header that
`entity explore` used to omit is now printed; that gap is closed.

Every command that takes a project accepts a fuzzy name, not just an
exact slug, because the Python command does — *except* `search`, which
is the one family that never calls `fuzzy_match_project`. This paragraph
said "every Python command does" until 2026-11-17, and porting
`search files` is what measured it: `templedb search files README -p
templed` prints `No files found matching 'README'` and exits 0, where
every `file` / `vcs` / `intent` / `source` command would have resolved
the name. So `search files -p` is an exact slug here too, and a miss
selects nothing rather than erroring.

`search content -p` is the one place this port has **not** followed
that, and it is a pre-existing divergence this step found rather than
introduced — it calls the resolver, so an unmatched `-p` exits non-zero
where Python exits 0 with an empty result. It is left alone deliberately
(changing an already-shipped command's behaviour is not this step) and
is in *Known gaps*. The scoring function itself is reproduced
value-for-value from `fuzzy_matcher.py`; the two bugs wrapped around it
are not — see below.

481 tests run by `cargo test`, and since 2026-11-17 every number in
this paragraph is **checked by the gate** rather than recounted by
hand: 417 in `templedb-db`'s library, 51 in `templedb-cli`'s, for the
formatting the SQL layer cannot see, 6 in `tests/sweep_coverage.rs`,
which checks the parity sweep still covers every command this binary
has — see *Checking the checker* — and 7 in `tests/gate_counts.rs`,
which checks these counts, its own included. Those six are the only
tests here whose subject is a shell script; the seven beside them are
the only ones whose subject is this README. Between them, neither the
sweep nor this paragraph is a claim that nothing can contradict.
7 more are `#[ignore]`d because they need
`python3`: 3 drive CPython's `difflib` as a reference oracle for *both*
renderers, 2 drive CPython's `sqlite3` to pin the clock's
arithmetic against `julianday`, and 2 drive CPython's
`importer.scanner` over a generated corpus of 24,757 paths to pin the
179-entry path classifier `file retype` is a function of; all three are
described under *Porting order* below. The third oracle is the first
that needs more than the standard library — it imports the
`templedb` package itself, through the `PYTHONPATH` its own launcher
exports, so that it diffs against the same `scanner.py` the sweep's
`templedb` runs. 90 more are `#[ignore]`d because they need a Postgres
scratch database — the write path's only verification, run by
`just write-test`.

**Four of this README's test counts in a row were wrong**, which is why
the gate counts them now rather than the prose claiming them. This
paragraph said 393 (347 / 40 / 6) and step 29's entry said 411 when the
figure was **409**; step 30 said 429, which was right when it was
written; step 31 corrected the total to 502 and left the breakdown it
was a breakdown of at 372. None had drifted far, and that is the point
rather than a mitigation — a reader cannot tell a current number from a
stale one, and nothing here could either. See
*Counting what the gate claims*, and step 32 for the measurement that
the two counting methods were one apart and why.

The 95 added
since the backend split cover the parts that have no database behind
them and would otherwise only be exercised by running against a server —
placeholder rewriting (including a `?` inside a string literal, a
doubled `''`, and the `ESCAPE '\'` whose backslash must not swallow the
closing quote), the `Value` getters refusing NULL in a required column,
backend selection from the environment, connection-string redaction, and
the mirror's table ordering and keyset pagination.

Nineteen of the `templedb-cli` tests are `summary`'s, and they carry
more weight than the count suggests: `summary` is the one command the
sweep cannot diff whole, so hand-built facts rendered at a fixed instant
are most of what verifies it. They are the only place several of its
properties are checked at all — that a check which *errored* never
prints its stale issue count, that the stale-reconciliation set
*replaces* the listing rather than preceding it, that `subject_ref[:40]`
slices characters and not bytes, and that padding a string Python
painted first leaves the escape codes inside the column width. That
last one is a misalignment being reproduced on purpose, which the
obvious implementation silently fixes.

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

**Not implemented.** Everything else — which is most of it. No session
*management* (sessions are accepted when named and never created — see
*Sessions*), no checkout *management*, no ingest, no `doctor entities`,
no publish, no Nix integration, no GUI. See *Porting order* below.

Measured 2026-11-05 by walking both argument parsers: Python exposes
**42 top-level groups, 272 second-level nodes and 428 leaf
subcommands**; this binary exposes **11 groups and 33 leaves**, of
which 30 share a Python name. That is about 8% of the surface, and the
figure is here because the per-command table above reads like more.
Three of the 33 are the port's own — `mirror`, `state` and
`vcs commit-staged`. Two families are complete: `source` at 2 of 2 and,
since 2026-11-18, `intent` at 5 of 5; `file` is at 6 of 10.

The port's half of that count is recounted here on 2026-11-18 and
**does not reconcile with either figure it replaces**: the paragraph
said 25 and `parity-sweep.sh` said 29, where this run of the same walk
finds 31 — and 30 before `intent apply` landed, so neither of the
earlier two was simply stale by one command. The method is the one both
of them named: count the lines under `Commands:` in each group's
`--help`, drop `help`, and count `mirror`, `state` and `summary` as
their own leaves. Stated rather than silently corrected, because this
is precisely the class *Known gaps* names as decaying — nothing in the
gate counts commands, and three honest walks have produced three
numbers.

**A fourth walk, 2026-11-18 evening, makes four numbers**, and this one
is worth more than the tally because it isolates the error. Step 35 ran
the method above verbatim and got **32 leaves for the tree the
paragraph described as 31** — the enumeration is `project` 2, `file` 5,
`entity` 7, `search` 2, `source` 2, `vcs` 5, `intent` 5, `doctor` 1,
plus the three standalones, and it is printed here because a number
with its addends beside it is checkable and a number alone is not. So
the third walk was one low rather than the first two being stale, which
is the opposite of what the paragraph above inferred from them. 33 with
`file mark`, and the breakdown is in the enumeration rather than in
prose for that reason.

That makes it the **seventh** count here to decay. The sixth is further
down this section — the write-command count, which was wrong twice in
two weeks. Both were found by the same step, both are counts of
*commands*, and the gate can catch neither.

`vcs commit` itself is **not yet** ported: five of its nine guards
reconcile a workspace against the database, and this port has no
workspace. What is ported is the half that is a database operation,
under a different name — `vcs commit-staged` — so that the name does
not promise a workflow it cannot perform. The other half is **deferred
rather than refused**, which is a 2026-10-31 decision and not what this
paragraph said before: the goal is eventual replacement, so the tree
work is last, not out of scope. See *Deferred, not refused* and
*The second write*.

**The write loop closes, as of 2026-10-27, and becomes one command on
2026-10-29.** `file set` stages what it writes and `vcs commit-staged`
commits what is staged, so the two are one path rather than two halves
— and `file set --commit` now drives both from a single invocation, in
a single transaction. That is steps 21 and 22; see *The auto-stage* and
*`--commit`, and why it is one transaction*.

Nothing of Python's `file set` is missing any more. `--commit` and
binary content were the last two entries on that list, and both landed
2026-10-29. What *is* missing is newer than the list: Python grew a
three-way rebase behind `--if-match` since the audit, and this port
**refuses** it rather than lacking it — a 2026-11-18 decision with a
measurement behind it, where the reason had been scope. See *The rebase
this port refuses to do*.

No writes to the live database, either — and that is still true now
that the binary has seven write commands. There are eight write paths
in it: `mirror`, `file set`, `file rm`, `file mark`,
`vcs commit-staged`, `intent cancel`, `intent create` and
`intent apply`. All eight write only a Postgres
database this port owns, and none can reach the SQLite file the Python
implementation has authority over. The seven commands additionally
require `TEMPLEDB_RS_ALLOW_WRITES=1` and a `TEMPLEDB_WRITE_URL` that is
deliberately *not* the variable the readers use. See *Two backends*,
*The first write*, *The second write* and *`intent create`*.

This paragraph said "two write commands" and "three write paths" until
2026-11-17, having been written when that was true and not re-checked
when `intent cancel` landed on 2026-11-03. It is a small instance of
the *Known gaps* entry about counts nothing checks, and it is recorded
rather than quietly edited because the port's own rule is that a
divergence from the live state should be visible. `intent apply` moved
both numbers again on 2026-11-18, and that one *was* re-checked here —
which is what the entry asks for and not what it predicts.

**And then `file rm` did not, on the same day.** It said five commands
and six paths when there were six and seven, and the list two sentences
up omitted `file rm` outright — written the morning of 2026-11-18,
corrected the evening of 2026-11-18 by step 35, which needed the same
sentence. That is the **sixth** count in this README to decay — after
the four test counts `gate_counts.rs` now catches and the "four opt-in
writes" heading step 34 corrected — and the
second inside this paragraph, which is now the single densest instance
of the thing *Known gaps* says nothing in the gate can catch: step 34's
own entry records correcting the *heading* three screens above from
"four opt-in writes" to six, and did not look down here. A count being
wrong once is drift; the same paragraph being wrong twice in two weeks,
including once by the step that fixed its neighbour, is the shape of the
gap. Nothing counts commands.

`intent apply` is also the first write in this port that changes
**source** rather than only metadata or history. `file set` does too,
but it is the writer of the content it stores; this one points a file
at content a different invocation proposed, possibly days earlier. That
is the whole reason its stale-base refusal is argued at the length it
is below.

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

Both axes are now run over **one sweep of 92 invocations covering every
implemented command**, and it is committed as
`scripts/parity-sweep.sh` rather than retyped each time:

```bash
cargo build --release
scripts/parity-sweep.sh python                  # vs the Python templedb
scripts/parity-sweep.sh backends 'postgres:///templedb_mirror'
scripts/parity-sweep.sh regress ./old/templedb-rs
```

It exits non-zero on the count of *unexpected* differences. The
deliberate ones are a list in the script, matched by prefix and reported
separately — which means a divergence that **closes** also shows up,
printed as `expected` for something that now agrees. That is not a
detail: it is how `entity search`'s ordering was discovered to have
converged, and a script that simply filtered those rows out would have
hidden it.

stdout and the exit code are the contract; stderr is counted but not
failed on. Python routes diagnostics through `logging`, so they arrive
prefixed `ERROR` / `INFO` where this port writes them plain, and
`file ls` prints a `N file(s)` count to stderr precisely so stdout stays
diffable. Fifteen rows differ on stderr alone today and the summary
says so. Ten of the fifteen are the error-status rows added on
2026-10-17, which print nothing on stdout at all: for those the exit
code *is* the comparison, and the differing stderr is the message
wording, which is not the contract. See *The error statuses*.

Committing the sweep is the audit's main deliverable. Three commands had
drifted *because* each harness was written during one command's port and
retired when that command was declared done.

- **Python axis**: 44 of 49 stdout-identical, 5 deliberate, 0
  unexpected. It was 34 of 40 with 6 deliberate when the audit ran; the
  sixth deliberate row was the exit-code divergence, which is now a
  passing row rather than an excused one.
- **Regression axis**, pre-audit binary against new: 25 identical, 15
  changed, every one on purpose — `entity stats`'s layout and new
  `relations:` section, `file ls -l`'s `edit_mode` column and width,
  `entity trace`'s truncation advisory, `source revisions` and
  `--meta`'s NULL rendering, three `vcs show` commits from the blob fix,
  and the two `-s` renderings.
- **Backend axis**, SQLite against a freshly mirrored Postgres: **49 of
  49 identical**, exit codes and stderr included.

The blob classification fix is the one that needed the second axis: it
asks the database `content_blob IS NOT NULL` through a `CASE`, which
SQLite types as an integer and Postgres as a boolean, so the expression
is wrapped rather than selected raw. That difference is invisible to a
diff against Python.

One honest note on how the backend axis behaves against a live
database. The first run of the 38 disagreed on one row — `project list`
reported 29 files for `math` against the mirror's 27 — and the cause was
staleness, not a defect: the live SQLite file is being written by the
Python implementation while the sweep runs, and the mirror is as current
as the last `mirror` call. Re-mirroring and re-running agreed on all of
them.
Worth recording because the failure mode looks exactly like a
translation bug, and the earlier runs of this axis used a *frozen* copy
of the database for that reason.

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

Making Postgres primary means translating the rest of the schema —
recounted 2026-11-17, 171 `sqlite_master` tables (160 discounting FTS5's
ten shadow tables), 45 views, 61 triggers and 381 indexes, against the
23 tables and 2 views the port reads today — and porting the write
paths, which is the design document this port has been deferring. The
mirror does not quietly accomplish either. (The port's side of that
comparison said **8** until 2026-11-16, which is what it was when this
paragraph was written; the live side said 163 tables and 361 indexes
until 2026-11-17, which is what *it* was. The boundary has moved with
every reader and the schema has moved on its own, and a count in prose
is the one kind of measurement nothing re-checks.)

## Reads only, deliberately

This is a design decision, not a stage we are rushing through. The Python
implementation owns every write path, and those paths carry invariants
learned the hard way: session-scoped staging, checkout role resolution
(`resolve(purpose)`), blob-age guards so a stale disk cannot overwrite a
newer blob. A reader cannot violate any of them. Porting reads first lets
both implementations run against one live database instead of competing
for authority over it.

Two of those invariants are now the reason a command is *not yet* ported
rather than the reason none is. Checkout role resolution and the
blob-age guards are what `vcs commit`'s five filesystem guards are made
of — so the half of commit
that is a database operation is ported under a name of its own and the
half that reconciles a tree is **deferred**. The decision above is what
makes that the right answer instead of a compromise: a port that grew a
workspace scanner *while Python is still authoritative* would be
competing for authority again. See *The second write*.

**Half of "checkout role resolution" stopped being an invariant this
port lacks on 2026-11-15**, and the split is worth stating precisely
because this paragraph read as though the whole of it were out of
reach. Resolution is a `SELECT` over `checkouts` plus a `stat`, and
`file where` needs it to say which directory it is hashing — so
`checkouts.rs` ports the two purposes a *reader* can have,
`PURPOSE_BUILD` and `PURPOSE_EDIT`. What is still absent is the two
that exist to *refuse*: `PURPOSE_WRITE`, which declines to hand a
writer the published tree, and `PURPOSE_SCAN`, which declines to adopt
a stale one. Those are the ones `vcs commit`'s guards are made of, they
have no reader here to verify them against, and `Purpose` has two
variants rather than four so that a future writer has to add the branch
rather than inherit a guard that was never exercised. See
*The resolver the hardcoded paths were standing in for*.

### Deferred, not refused — decided 2026-10-31

That paragraph said "refused" until 2026-10-31, and three sections of
this README argued the tree-dependent half of the system was out of
scope rather than late. **The goal is eventual replacement**, so it is
late.

The distinction matters because the authority argument is about
*coexistence, not capability*. It holds for exactly as long as two
implementations run against one live database. At cutover there is one
implementation, nothing to compete with, and the argument dissolves
entirely — so it settles the **sequencing** of the checkout work and
says nothing about whether it happens.

Reading it as scope produced a contradiction this document carried
without noticing: the closing paragraph says "rename when it can answer
everything the original can", and *everything* includes `materialize`,
`templedb commit <slug> <dir>`, the `project checkout-*` family and
`file where` — the workflow every agent in this system actually uses.
"Never support checkouts" and "rename when complete" cannot both be
true. The rename sentence is the one that stands.

So the order is the one step 15 already used for writes, one layer out:

1. Everything that is not a tree — reads and database-only writes. This
   is where the port is now, and it is most of what is left.
2. **The tree-pinning harness**, which is the real precondition and is
   not a command. The byte-diff method compares two sequential
   invocations, so a tree being edited between them produces a false
   difference; a clock could be injected as one scalar and a checkout
   cannot. Needs a fixture tree or a snapshot mechanism before any
   tree-reading command is verifiable.
3. **A design document for the tree layer** — the workspace scanner, the
   checkout-role resolver, the mirror-to-disk step. The 2026-10-24
   report called this "a different project, not a harder version of
   this one", which is accurate about its size and was being read as a
   refusal. Same shape as *The write path* report, which is also what
   made step 15 tractable. The resolver's **read** half came out of this
   bucket on 2026-11-15 without the document and should not have been
   in it: it resolves and writes nothing, and `file where` was already
   answering with two paths it had built by hand. The document is for
   the half that decides where a *write* lands.
4. The tree-touching commands, and then cutover.

Two things in that list are *not* blocked and are worth separating out.
`file where` only **reads** a tree — it hashes the checkout and prints
`✓ match`, mutating nothing — so it never competes for authority and is
blocked on (2) alone. And `doctor entities`' 15 non-database checks are
the same shape. Neither is waiting on the cutover.

What does not change: Python stays authoritative until cutover, the
mirror stays a read replica, and the 2026-10-16 decision — the port
stays a separate project and owns a Postgres populated by copy — still
holds. Deferred means last, not soon.

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

## The audit: almost every bug below is now fixed upstream

**Read this before the next section.** The list that follows was written
as a bug report against the Python implementation. On 2026-10-14 every
item in it was re-probed against the live `templedb`, and **nine of the
twelve are fixed there**. The section is kept because it records what
the port found and why each divergence was taken, but it is history, not
a description of the current Python. The status column says which:

| Reported bug | Python today |
|---|---|
| `search content` attributed matches to the wrong project | **fixed** (`1DB323BCFDDF1DBA`) |
| `source revisions` crashes on a deleted file | **fixed** — prints `(deleted)`, exits 0, lists all 14 |
| `source snapshot` silent + exit 0 on a missing blob | **fixed** — errors, names the blob, exits 1 |
| Fuzzy project matching rejects projects as ambiguous against themselves | **fixed** — `qa-run` resolves |
| A NULL project name is searchable as the literal `None` | **fixed** — `None` matches no project |
| `entity paths` reports "no path" across an edge (`limit=100`) | **fixed** — the 1-hop path is found |
| `entity search` treats the query as a LIKE pattern | **fixed** — `deploy_` is literal |
| `entity search`'s `ORDER BY` has no tie-break | **fixed** — byte-identical, ordering included |
| `entity trace`'s fan-out cap is `LIMIT` with no `ORDER BY` | **fixed** — same `ORDER BY`, plus a truncation advisory |
| `vcs show`'s commit lookup is a LIKE injection | **fixed** — escaped, with `ORDER BY` |
| `project show` crashes on a third of the projects | **fixed** — prints `0` |
| `file ls <prefix>` treated the prefix as a LIKE pattern | **fixed** — filtered in Python, not SQL |

Two of those were fixed *to what this port does*, which is the most
useful thing in this file. `entity trace` now carries the same
`ORDER BY (relation kind, peer kind, peer ref)` the port added, and
`entity search` now has a tie-break — so the one ordering divergence the
port deliberately carried has closed, and the status table says
byte-identical where it used to say "ordering differs on purpose".
`entity trace` went further than the port had: it added a
`(fan-out capped at N …)` advisory for the exact complaint recorded
below, that "nothing in the output says it truncated". The port did not
have that line and now does.

**And the traffic is not one-way.** Three commands were silently behind
Python when the audit ran, which is the real finding:

| Command | What was wrong |
|---|---|
| `entity stats` | the entire `relations:` section was missing — 19 kinds and a total, never printed. Every number the command *did* print agreed, which is why a diff of counts never caught it |
| `file ls -l` | no `edit_mode` column (migration 106), and the line-count field was one space too wide on every row |
| `source revisions` / `snapshot --meta` | NULL rendered as `?` and `(none)` where Python renders `-` and `None` |

Plus the blob-classification regression written up under *The first
place this port fell behind Python*. All four are fixed here now, and all four
were invisible to the verification this port had been doing, for the
same reason: **the harnesses only covered the command being worked on.**
The blob bug surfaced from a `--side-by-side` sweep that happened to
also run the unified renderer; the other three surfaced only once every
implemented command was diffed in one pass.

So that is now the method: a single sweep over all 92 invocations
(`scripts/parity-sweep.sh`), run against Python and across both
backends, rather than a per-command harness retired when the command is
declared done. Five differences remain and every one is deliberate and
documented — `project list`'s layout, `entity explore`'s ordering, two
`search content` invocations, and one `intent list -s` row where this
port has a tie-break and Python does not. Two more were gaps rather
than decisions and both closed: `entity paths`'s exit code on
2026-10-17 and `vcs log`'s session default on 2026-10-23 — see *The
error statuses* and *Sessions*.

## Bugs found while porting

Reimplementing a query means reading the schema it runs against, which is
a different kind of attention than using it.

- **`file set`'s un-delete guard fired unconditionally, and not for the
  reason its comment gives.** Found 2026-10-27 while porting the
  auto-stage; **fixed upstream the same day** in `templedb`
  BF4E6BEA6BD93C6E. `_write_content_to_db` tests
  `file_record.get('status') != 'active'`, and `get_file_by_path` —
  which produced `file_record` — did not select `status` at all, so the
  test was `None != 'active'` and the revive `UPDATE` ran on every write
  to an existing file. The guard worked; the condition did not. The risk
  was not the wasted `UPDATE` but the next reader: anyone who noticed
  the check looked redundant and removed it would restore the
  `set bza flake.nix` bug it was added for. Two more findings came out
  of the same work and went upstream in the same commit — Python's two
  branch lookups disagreeing, which staged *nothing* and reported
  success, and migration 088's provenance link going stale on a second
  write. Both under *Three findings from porting the auto-stage*.
- **`vcs session prune` would refuse to delete 9 of the 558 sessions it
  offers to.** Found 2026-10-23 while deciding whether the mirror should
  declare `edit_intents.session_id`'s foreign key. The live column does
  declare one, `foreign_keys` is on, and `prune_sessions` guards only on
  `vcs_working_state.staged_by_session_id` — it never consults
  `edit_intents`. 218 intent rows reference a session that has already
  ended, and 20 of those name a session that is *prunable* by the
  command's own criteria. The delete is one batched
  `DELETE ... WHERE id IN (...)`, so one offending row fails the whole
  statement: `--dry-run` reports 558 and the real run cannot do any of
  them.

  Not reproduced and not reachable here — this port does not prune — and
  recorded for two reasons. It is why the mirror does not declare that
  FK: the live constraint is a latent failure rather than a guarantee,
  and a mirror enforcing it would refuse rows the source holds. And it
  is the shape the sessions report warned about from the other side:
  `edit_intents.session_id` is "most of the point" of the table, and a
  cleanup path that does not know the column exists is how the point
  gets lost.

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
  orders by `(observed_at DESC, kind, external_ref)`.

  This was the port's one intentional output difference, and it is
  **gone**: Python added a tie-break and the two are now byte-identical
  unsorted. Worth keeping the entry because of what it shows — the
  divergence closed by convergence, not by either side conceding, and
  the port's own README-recorded reasoning ("between two fixes, the one
  that also agrees with the implementation being replaced is strictly
  better") is the argument the other side appears to have reached too.

- **`entity trace`'s fan-out cap is `LIMIT` with no `ORDER BY`.** Which
  ten of `Machine/zMothership2`'s 156 `ran` edges you see is up to the
  query planner, the same command can print different neighbours on two
  runs, and nothing in the output says it truncated. This port orders by
  `(relation kind, peer kind, peer ref)` before the cap. On current data
  the planner happens to agree, which is why the trace outputs above are
  byte-identical — that is luck, not a guarantee.

  **Also fixed upstream, and then some.** Python now carries the same
  `ORDER BY (r.kind, e.kind, e.external_ref)`, so the agreement is by
  construction rather than by luck. It also answered the second half of
  this complaint, which the port had not: it prints
  `(fan-out capped at N per node and direction; some edges not shown —
  raise with --limit N)` when the cap bit, detecting that by asking for
  `limit + 1` rows and discarding the extra. The port now prints the
  same line. It needs no `limit + 1` trick, because it holds every edge
  in memory and applies the cap in Rust, so "more than `limit` exist" is
  a length comparison.

  The luck ran out elsewhere, though — on `entity explore`, whose query
  Python still does not order. See *`entity explore` orders its edges
  and Python does not*.

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

### The error statuses

Closed 2026-10-17. This was the longest-standing entry under *Known
gaps* and the last one that was a divergence rather than a decision.

Python's read commands use three statuses:

| code | meaning | where |
|---|---|---|
| 1 | bad input, or a project that does not resolve | everywhere |
| 2 | the thing you named does not exist | `entity explore` / `trace` / `paths`, `source snapshot` / `revisions` |
| 3 | a valid negative answer | `entity paths`: no path within depth N |

Three was matched from the start, because it is the one a script is
likely to branch on and it is not an error at all — `entity paths`
prints its answer on stdout and returns through the normal `Ok` path.
Every actual error, though, left through one `anyhow` arm in `main` and
exited 1, so a caller could not tell `entity explore Machin/zMothership2`
— a typo in the *kind*, which is bad input — from
`entity explore Machine/zMothership3`, which is a well-formed question
answered no.

The fix is `crates/templedb-db/src/exit.rs`: a `Coded` error carrying a
status, a `bail_not_found!` macro, and `code_of` called once in `main`.
**Five sites** needed it, and they turn out to be three — `lookup_entity`,
which all three entity commands resolve addresses through, and two
`bail!`s in `source.rs`.

Two things about the shape are deliberate:

- **The status rides on the error rather than through the signatures.**
  Every one of those sites is two or three frames below a CLI arm that
  already returns `anyhow::Result`. Threading a code out would mean a
  bespoke error enum per module and a `match` per call site to collapse
  it again — the sites that do not care, which is nearly all of them,
  would pay for the two that do. The cost is that the status is found
  by downcast rather than visible in the type, and a unit test pins the
  property that makes that safe: **a `Coded` survives any number of
  `.context(...)` layers**. If it did not, the code would be lost
  silently on the way up and every error would exit 1 again — this
  exact bug, reintroduced invisibly.
- **Only `NOT_FOUND` is constructible.** `USAGE` is what an untagged
  error already produces, so a second way to spell it would only invite
  two call sites to disagree.

Ten rows were added to the sweep, which is where the real verification
is: six that must exit 2, four that must exit 1. They print nothing on
stdout, so the exit code is the entire content of the row — including
`entity explore Machine` and `entity paths Machine Machine/zMothership2`,
which exist to pin the *other* half of the split. A change that tagged
every error as not-found would pass the six and fail those two. All ten
agree with Python, and the refs are constructed (`no-such-machine-ever`)
rather than real-but-deleted, so no row can start passing when the data
moves.

The unit tests were checked by mutation rather than trusted: reverting
the `bail_not_found!` in `lookup_entity` to a plain `bail!` fails
`paths_rejects_unknown_endpoints`.

All three axes were run on the finished change, and the regression one
is the useful result: against a release build of the previous tree,
**43 of 49 rows identical and exactly the six intended ones changed**,
each `exit 2 vs 1`. Nothing else moved — stderr included, on all 49, so
the messages did not drift while the statuses were being sorted out.
The Python axis is 44 of 49 with 0 unexpected and the backend axis is
49 of 49.

What is still not matched is the *message*, and that is in *Known gaps*
below rather than here.

## Known gaps

Differences that are not deliberate and are not yet closed.

- ~~**The sweep is a shell script, not a test.**~~ **Closed
  2026-10-29**, from the only angle it can be. The sweep still needs a
  live database and the Python binary and still cannot be a `#[test]`
  — but its *coverage* can be, and that was the half silently rotting:
  `crates/templedb-cli/tests/sweep_coverage.rs` runs in `cargo test`
  and fails if a command exists in the binary and in no sweep row, or
  if an exemption outlives the command it exempts. The CI entry point
  is `just check-all`, which is also the first thing in this repository
  to run the write tests and the cross-backend sweep as part of a gate
  rather than by hand. See *Checking the checker*.

  What is **not** closed, and is a weaker claim than the above: a row's
  arguments are not checked for reaching anything interesting. Coverage
  means a row exists, not that it is a good row.
- **The tree fence detects churn and does not prevent it.** Six rows
  read a directory and run between two digests of it; a row whose tree
  moved through all three attempts is reported as `churned` and counted
  in neither `pass` nor `unexpected`. So a tree under continuous
  rewriting would stop being checked, loudly rather than silently.
  Measured bursty — 432 files a day in bursts of a few hundred, zero
  churned on the first run — which is what makes detect the right
  trade. Argued at length in `scripts/parity-sweep.sh`; the alternative
  needed fixture rows in a database this port must not write.
- ~~**`just check` does not run `cargo fmt --check`, and two hunks have
  been unformatted since 2026-11-05.**~~ ~~**Nothing in the gate counts
  tests either.**~~ **Both closed 2026-11-17** by step 32, which is the
  step this entry argued for by name: "the *gate* is the actual gap …
  which is the argument for fixing the gate rather than the hunks".
  `just check` and `just check-all` now begin with
  `cargo fmt --all --check` and `cargo clippy --all-targets -D
  warnings`; `nix flake check` runs the first of the two; and
  `crates/templedb-cli/tests/gate_counts.rs` reads this README and
  fails when a number in it stops describing the tree. The two hunks —
  over-long `assert_eq!` lines in `files.rs`'s `current_hash` test —
  are formatted, by the step that made them fail a check rather than
  by the next person to edit the file. **Three** consecutive steps had
  deferred them on the rule that a step should not reformat a file it
  did not otherwise change, and that rule was right every time; what it
  could not do was end.

  The evidence for the second half was four wrong numbers in a row:
  393 against 409, step 29's 411 against 409, step 30's 429 against the
  438 it became a day later, and step 31's 502 beside a breakdown still
  summing to 372 + 51 + 6. See *Counting what the gate claims*.

  What is **not** closed, and is a weaker claim than the above: **the
  gate counts only what this source tree determines.** Ten numbers are
  checked — the test total, its four-part breakdown, the `#[ignore]`d
  groups and a second statement of the total 1,800 lines away. Three
  kinds of number are deliberately left unchecked and will go on
  decaying: the sweep's invocation count, because three of its rows are
  discovered at run time from the live database and no figure written
  in this tree can be right about it — and its *deliberate* count for a
  sharper version of the same reason, since step 32 decomposed that one
  into a static allow-list of 9 plus however many rows the live data
  makes differ today; every count describing a database
  (mirrored tables, digested rows, the 1,346 live `created_at` values),
  because a hermetic test cannot read a 483 MB SQLite file; and every
  claim about what a test *covers*, which is the same limit
  `sweep_coverage.rs` states about a sweep row. Where the gate cannot
  count, the fix applied here is to stop claiming — the two justfile
  recipe descriptions that said "all 54 invocations" when the sweep had
  101 now say neither number.
- **One file per mirror label is inside the fence's blind spot, and
  since 2026-11-15 it has to be.** The fence digests the roots a row
  reads, and `file where`'s two TempleDB roots are now chosen at run
  time by checkout resolution, so the row cannot name them in advance.
  Naming the enclosing directory instead was measured and rejected:
  `edit-workspaces/templedb-rust` is 70,806 entries and
  `edit-workspaces/templedb` is 57,046 — an edit workspace holds
  `target/`, which this sweep is writing into while it runs — so
  fencing them would make every row `churned`, which is to say
  unchecked. What stays unfenced is a `stat` plus one hash of
  `<resolved root>/<path>`, not a walk; the three overlay roots, which
  *are* walked, remain fenced. Strictly smaller than the exposure the
  fence already covers, and stated rather than left to be inferred from
  a passing row.
- **`file where`'s unreadable-mirror line is not byte-comparable.**
  Python prints its `Exception` text in parentheses and this prints its
  own wording. Same class as the error-message gap below, and reachable
  only for a file that exists and cannot be read — which no live
  slug/path pair is.
- **`file where` resolves two of Python's four checkout purposes**, and
  the other two are deliberately absent rather than stubbed.
  `PURPOSE_WRITE` exists upstream to *refuse* — it declines to hand a
  writer the published tree — and `PURPOSE_SCAN` declines to adopt a
  stale tree, because a whole-tree scan records an absent file as a
  deletion. Both belong to the tree layer of step 27, both write, and
  neither has a reader here to verify it against, so `Purpose` has two
  variants: a future caller has to add the branch rather than discover
  that a guard it assumed was there is not. Python's three resolution
  warnings are not reproduced either — they are stderr, where this
  README already records the wording as the port's own, and one of them
  hashes up to 200 files to decide whether to print.

  This entry said **"`file where` reproduces a probe that can never
  fire"** until 2026-11-15, and that reproduction was of a bug upstream
  had already repaired. See *The resolver the hardcoded paths were
  standing in for* — it is the gap list's own instance of a
  blocked-reason outliving the blockage.
- **`search files` treats the pattern as a literal and Python treats it
  as a LIKE pattern.** Deliberate, and the largest instance of this bug
  class the port has found. Python interpolates `f"%{pattern}%"` raw, so
  `_` means "any character": measured 2026-11-17, `search files get_`
  returns ten rows and **none of them contains `get_`** — the hits are
  `docs/GETTING_STARTED.md`, `get-booking-availability/index.ts`,
  `getDialpadOpts.mjs` and five more. `set_` returns 321 against a
  ground truth of 5 and `api_` 128 against 4. 1,112 of the 2,780 live
  paths hold a literal underscore, so this is the normal case for
  anyone searching a Python identifier or a Nix store path. Same bug
  `entity search` and the `file ls` prefix had — both fixed upstream —
  and this is the copy still live. The `search files get_` sweep row is
  in the EXPECTED list so that it converging shows up as a surprise.
- **`search files` lists deleted files and nothing says so.** Reproduced
  rather than fixed, because it is the view's behaviour on both sides:
  `files_with_types_view` has no `status` filter where `file ls` and
  `search content` both do, so all 373 of the live `status = 'deleted'`
  rows are findable and are printed identically to a present file. It is
  the only file-listing surface in either implementation that does this.
  Pinned by the `search files src/main.py -p templedb-rust` sweep row —
  a file `file ls templedb-rust` does not list.
- **`search files` says on stderr that its hundred cut the list, and
  Python does not.** Python's `(100 results)` reads as "there are a
  hundred" for the 536 files matching `.py`. stdout is byte-identical;
  the extra line is stderr, where this README already records the
  wording as the port's own, and it is the same call `search content`
  makes about its candidate cap. The `search files .py` sweep row is
  counted as stderr-only.
- **`search content -p` resolves fuzzily and Python's does not.** Found
  2026-11-17 while porting `search files`, and pre-existing rather than
  introduced: `search.py` is the one command family that never calls
  `fuzzy_match_project`, so `search content checkout -p templed` exits 0
  with no results upstream and exits non-zero here. Not closed in step
  30 on scope grounds — changing a shipped command's behaviour is a
  different step — and invisible to the sweep today, because its only
  `-p` row passes an exact slug. `search files` was given the faithful
  behaviour and has a sweep row (`-p no-such-project-ever`) pinning it,
  which is why the two now differ from each other inside one command
  family. That is stated rather than smoothed over: the inconsistency is
  real and it is Python's.
- **`intent create` accepts empty content and `file set` refuses it.**
  Reproduced on both counts, and the inconsistency is upstream's:
  `file set` tests `if not content` and refuses, on the grounds that an
  empty pipe is "almost always a pipe that produced nothing", while
  `intent create` tests `if content is None`, so both `-c ''` and an
  empty pipe count as content. Measured on a copy of the live database
  2026-11-17: `printf '' | templedb intent create` created intent
  #1627, recorded `patch_summary: 1 lines, 0 bytes` and exited 0.
  Pinned by `empty_content_is_proposable_where_file_set_refuses_it`,
  which asserts the sha256 of zero bytes Python recorded for that row.

  Two defensible readings and the port picks neither: a proposal to
  truncate a file is a legitimate thing to record and nothing is
  applied until `intent apply` runs, so accepting it is not obviously
  wrong — and `file set`'s refusal is not obviously wrong either. What
  is wrong is that one command family answers the same question two
  ways, and that is a decision for upstream rather than for a port.
- **`intent create`'s `patch_summary` uses Python's *other* line
  count.** `_short_summary` is `decode().count('\n') + 1` with no empty
  case, where `file set`'s is the same expression behind an explicit
  `0 if not content`. So an empty proposal reads `1 lines, 0 bytes` and
  the same content through `file set` records 0 lines. Both reproduced,
  in two functions — `intent::patch_summary` and `line_count` — with a
  test asserting they disagree on the one input they disagree on.
  Listed here rather than silently absorbed because a port that reused
  one function for both would be right on every input but one.
- **`intent create` matches the project slug exactly and Python
  fuzzy-matches it.** `file set`'s divergence, for `file set`'s reason
  plus one more that is forced: the id is resolved inside the write
  transaction, because the reader and the writer point at different
  databases, so a fuzzy read-side resolution would score a name against
  the parity copy and stamp the result into the scratch one. Invisible
  to the sweep in both commands, because it is a positional rather than
  a flag — the flag-surface stage's third blind spot.
- **`intent create` refuses a bad `TEMPLEDB_SESSION_ID` and Python does
  three different things with one.** The same divergence `file set`'s
  intent already carries, with the measurement that was missing:
  Python's `_current_session_id` runs `int()` and swallows the failure
  and never checks `ended_at`, so a non-numeric value creates the
  intent with a NULL session and exits 0, an *ended* session's id goes
  into the column as written and exits 0, and an id naming no row at
  all hits the live FK and exits 1 with `Error: FOREIGN KEY constraint
  failed`. Measured on a copy 2026-11-17 and tabulated in step 31.
  This port exits 1 on all three with a message naming the variable.
  The middle case is the one that matters in practice: an agent wrapper
  holding a pruned session's id writes a dangling reference upstream
  and gets told nothing. The port's refusal is also what keeps a
  dangling id out of its own Postgres, because the subset deliberately
  declares no FK on that column.
- ~~**`intent apply` is not ported and is the last of the family.**~~
  **Closed 2026-11-18** by step 33, and the entry's stated reason was
  wrong in the way this list keeps being wrong. It said the command
  "three-way merges a stale proposal onto what landed, which is the
  merge engine `file set --if-match` also lacks" — true about the
  effort, never checked against the engine. The engine is not a
  three-way merge: measured against `git merge-file --diff3` over 11
  triples it agrees on 3, and 4 of its clean results write content
  neither side held. So `apply` is ported with the strict behaviour and
  the merge is refused on purpose. `intent` is complete at 5 of 5.
- **`intent apply` refuses a moved base where Python merges onto it.**
  The replacement for the entry above, and the *one* behavioural
  divergence in the command: exit 4 on any stale base, which is what
  Python's `--no-rebase` selects. Deliberate, argued and measured under
  *The rebase this port refuses to do* — the decisive number is that
  upstream's failures are not conservative, 4 of them reaching
  `file_contents` with content neither the author nor the database ever
  held, one of which was reproduced end to end through the shipping
  command at exit 0.

  Invisible to the sweep, and that is a **fourth** blind spot in the
  flag stage rather than an instance of the first three: the surfaces
  agree exactly — `--no-rebase` on both sides — while the flag means
  different things. Named in `parity-sweep.sh` beside the others.
- **`intent apply` accepts `--no-rebase` and `file set` does not, and
  the two decisions used different rules.** Python has the flag on both
  commands with one meaning. `file set`'s is absent and allow-listed
  under the `--no-fts` rule: a flag switching off behaviour the port
  does not have would be accepted and ignored. `intent apply`'s is
  accepted under the opposite reading: it promises a refusal this port
  delivers on every input, and the stricter implementation erroring on
  the flag that asks for strictness is worse.

  Both are defensible and they are not the same argument, so one of the
  two commands is wrong — and by the sharper reading it is `file set`,
  which should accept the flag and leave the allow-list. Not done in
  step 33, on the grounds step 30 used for `search content -p`:
  changing a shipped command's flag surface is a step of its own.
- **`file rm` refuses without a session and Python creates one.** The
  divergence `file set --stage` and `vcs commit-staged` already carry,
  with no softer half available: `file set` without a session still
  writes the content and stages nothing, and this command's entire
  effect *is* the stage. Measured on a copy of the live database
  2026-11-18 — with neither variable set Python printed
  `✓ Staged .gitignore for deletion`, exited 0, and minted
  `vcs_sessions` id 1331 to own the row; with `TEMPLEDB_SESSION` set to
  a name matching nothing it minted id 1333 from the name. Deliberate,
  and the refusal names the second variable for that reason — unlike
  `intent create`'s, where ignoring the name changes nothing on either
  side. Invisible to the sweep, because a write has no diffable stdout.
- **`file rm` has no `--session` where `file set` does, so the
  environment variable is the only way to supply what it requires.** A
  decision, not a gap in the usual sense: Python's `file rm` has no
  flags at all, so inventing one would be the port widening a surface
  for its own convenience and would cost a permanent `FLAGS_EXPECTED`
  entry. `file where`'s absent `--no-color` is the same call. The cost
  is real and falls on exactly one caller — a subprocess that cannot
  see the parent's environment, which is the problem `file set
  --session` exists to solve. Stated rather than left to be discovered
  from a refusal.
- **`file rm` leaves `content_hash` and `intent_id` on the row it
  flips, and nothing says so.** Python's `DO UPDATE SET` names three
  columns; the other two keep whatever the last `file set` put there.
  Verified on a copy 2026-11-18 — `templedb-rust/.gitignore`'s row held
  `c20683…` before and after. Reproduced, and the consequence is worth
  more than the fidelity: `vcs commit-staged` writes that column to
  `vcs_file_states.content_hash`, so a committed deletion names the
  bytes that were removed when the file had been staged first and names
  the `DELETED` sentinel when it had not. **Which of the two a reader of
  `vcs show` gets depends on the caller's sequence, and no comment in
  either implementation mentions it.** Both branches are pinned by
  tests. Not a bug on either side; an asymmetry that was undocumented
  until this step went looking.
- **`file rm` records no `edit_intents` row where every `file set`
  does.** Confirmed on the copy: 1,647 intents before and after.
  Reproduced rather than improved, for the reason the port declines to
  invent column values generally — a `'deleted'` status would be a value
  nothing else in either implementation writes, and the live CHECK is
  not mirrored, so this port would be the only thing able to read it
  back. The consequence is that a deletion has no provenance: nothing
  records who staged it or from which session, where every content
  write does.
- **`file retype --apply` is not ported, and the blocker is two
  columns rather than the write.** The read half landed 2026-11-19
  (step 36) and is byte-diffed against Python in the sweep; the write
  half is `UPDATE project_files SET file_type_id = ?, updated_at =
  datetime('now') WHERE id = ?` per change, which is ordinary, preceded
  by a resolve-or-create of a `file_types` row per target name, which
  is not. `file_types.category` is `TEXT NOT NULL` with no default and
  this port mirrors that table at **two of five columns** — `id` and
  `type_name` — because the boundary follows the readers and
  `search files` reaches it only through a view's join. Python inserts
  `(type_name, 'uncategorized', 'Auto-registered by file retype')`.

  Closing it is the same two-part shape as the entry below: `category`
  and `description` into `mirror.rs` and the DDL, a `pg-reschema` and
  a full re-mirror, and then the command. Not done in step 36 on the
  rule step 30 used for `search content -p`: a step about a classifier
  should not also be a step that moves the mirror, and the two halves
  turned out to be separable — the classifier is all of the read and
  the insert is all of the write.

  **The flag is absent from the parser rather than accepted and
  ignored**, which is the part that is a decision rather than a gap.
  `search content --no-fts` is absent because accepting it would
  promise matching this port cannot do; `intent apply --no-rebase` is
  accepted because it promises a refusal this port always delivers.
  `--apply` switches **on** the only behaviour that writes, so
  accepting it would mean exiting 0 having written nothing. It exits 2
  and holds one `FLAGS_EXPECTED` entry, which should leave the list
  when the subset grows.

  One thing that will make the growth worth more than this command:
  Python's `retype` is the only `project_files` writer whose own
  statement names `updated_at`, so the subset growing for it lets that
  writer set the column from the clock rather than needing a trigger
  reproduced. See the entry below, which has wanted a reason to move
  since 2026-11-18.
- **`pm2_config` is dead code upstream and this port reproduces it.**
  Found 2026-11-19 by `classify_parity.rs`'s coverage check, which
  asserts the generated corpus reaches every `type_name`
  `FILE_TYPE_PATTERNS` can produce and reported exactly one it could
  not. `ecosystem\.config\.js$` → `pm2_config` is entry 135; `\.js$` →
  `javascript` is entry 29, carries no guard, and matches every string
  the later pattern can match, because a path ending
  `ecosystem.config.js` ends in `.js` by construction. So no path can
  receive that type on either side.

  Measured on the live database: `file_types` holds `pm2_config` as id
  131 in category `infrastructure`, **0 of the 2,786 `project_files`
  rows carry it**, and the one live `ecosystem.config.js` is typed
  `javascript` — which `file retype` agrees with, correctly as far as
  the table is concerned. Reported upstream and not fixed here: the
  repair is a reorder of `FILE_TYPE_PATTERNS`, which changes what every
  command that classifies anything answers.

  A second entry is shadowed the same way and costs nothing:
  `templedb-deploy\.sh$` (138) is covered by `deploy\.sh$` (136), which
  is unanchored at the start and yields the same `deployment_script`.
  One dead rule changes an answer and the other does not, and the
  difference is only whether the shadowing entry carries the same name.
  So the oracle's exclusion list holds one name, not two, and it is
  asserted in both directions — upstream reordering the table makes
  this entry fail rather than quietly stop being true.
- **Python's `file retype` docstring names the wrong `file_types`
  row.** Upstream, reported rather than fixed, and the mechanism it
  describes is right. The docstring explains that `_get_file_type_id`
  used to fall back to `SELECT id FROM file_types LIMIT 1` and that
  "that row is `api_route`, which is how 495 rows across 10 projects …
  came to be recorded as API routes". Measured 2026-11-19: the first
  row of `file_types` is **`ansible_inventory` (id 126)**, not
  `api_route` (id 18) — migration 138 reseeded the table and moved
  which row `LIMIT 1` returns. The live symptom matches the new row:
  44 of the 88 changes the command proposes today are
  `ansible_inventory -> …`, including the four `.rs` files in this
  project. A docstring about a stale snapshot going stale itself is the
  same shape as this README's own prose counts, one repository over.
- **No `project_files` write in this port moves `updated_at`, and
  Python's every one does — by a trigger.** Found 2026-11-18 by step
  35, and **not new**: it has been true since the first write in step
  15. The live database carries
  `update_project_files_updated_at`, one of the three triggers on that
  table that are not cr-sqlite's, and it is
  `AFTER UPDATE ON project_files FOR EACH ROW` setting
  `updated_at = datetime('now')`. Python's `file mark` names one column
  and Python's `file set` un-delete names one column, and the trigger
  bumps a second one behind both. Measured on a copy:
  `templedb-rust/.gitignore` held `2026-10-16 15:27:09` before
  `templedb file mark … --edit-mode hot-reload` and
  `2026-11-19 03:33:59` after.

  This port has none of the live database's 61 triggers, so it cannot
  reproduce the bump as a trigger; and it cannot reproduce it in the
  statement either, because `project_files` is mirrored at **seven
  columns and `updated_at` is not one of them**. Closing it is two
  changes rather than one — the column into `mirror.rs` and the DDL,
  and then *both* writers setting it from the clock, the way
  `file_contents.updated_at` already is in `file set`, `intent apply`
  and the commit. Not done in step 35, on the rule step 30 used for
  `search content -p` and step 33 for `file set --no-rebase`: fixing
  it for the new command and leaving the month-old one wrong is a worse
  state than two writers diverging identically and in writing.

  It is the most latent divergence on this list, which is why it went
  a month unnoticed and why it is recorded rather than rushed:
  **no query in either implementation reads the column.** The only two
  `SELECT`s naming `pf.updated_at` are in `services/
  component_service.py`, which nothing imports and which also selects
  two columns `project_files` does not have. The state digest cannot
  see it either, since it digests the mirrored projection. So nothing
  in the gate will catch this entry going stale — the same shape as the
  prose counts, one layer down.
- **Python's `file mark` success line names the project the caller
  *typed*, which can be a project that does not exist.** Upstream, and
  reported rather than fixed here. The line is
  `f"✓ {args.project}/{args.file_path}: …"` and the row is resolved by
  `fuzzy_match_project`, so the two are different strings whenever the
  name was fuzzy. Measured on a copy 2026-11-18:
  `templedb file mark templedb-RUST .gitignore --edit-mode hot-reload`
  printed `✓ templedb-RUST/.gitignore: edit_mode immutable →
  hot-reload`, exited 0, and wrote the `templedb-rust` row. There is no
  `templedb-RUST`. The same interpolation is in the command's
  `File not found:` message.

  This port is byte-identical anyway and that is construction rather
  than luck — `resolve_project_exact` accepts only the slug, so the
  typed string and the written row's slug are the same string. The
  divergence is confined to inputs this port refuses outright. It is
  the mirror image of `file rm`, whose line carries **no** project
  prefix at all: neither is a rule, and the two commands are forty
  lines apart in one file.
- **`file mark` succeeds on a soft-deleted row and no reader can see
  it.** Reproduced: Python's lookup has no `status` filter and its
  `get_file_by_path` doc comment says rows of any status come back.
  Measured — `templedb file mark templedb-rust src/main.py --edit-mode
  live-symlink` printed its success line, exited 0 and wrote the
  column, while `file ls -l --edit-mode live-symlink` listed nothing,
  because `file ls` filters on `status`. It is the only success among
  the seven writes with no cross-read available, and
  `a_soft_deleted_row_is_markable_and_no_reader_can_see_it` asserts it
  *as* that rather than skipping the case. Not meaningless — `file
  set`'s un-delete leaves `edit_mode` alone, so the mode is waiting if
  the file comes back.
- **`file mark --edit-mode immutable` writes a NULL that
  `file ls --edit-mode immutable` does not match.** Both halves are
  Python's, in two commands, and each reads like a bug from the other's
  side: the write is
  `None if args.edit_mode == 'immutable' else args.edit_mode`, and the
  filter compares the raw column, where NULL is not the string. So the
  default mode is unaddressable by the filter named after it — on all
  2,785 live rows, every one of which holds NULL. Reproduced in both
  places, with `immutable_is_stored_as_a_null_and_the_filter_of_that_
  name_misses_it` pinning the pair; a port that stored the word would
  pass a round-trip test and diverge from Python's listing on every row
  it had written.
- **`file set --if-match` is a strict compare-and-swap and Python's is
  not any more.** Python three-way merges a stale base onto what landed
  and only a genuine overlap exits 2; its `--no-rebase` selects the
  behaviour this port has unconditionally. Deliberate, argued under
  *The rebase this port refuses to do*, and in the sweep's flag
  allow-list as one pair.

  This said "needs a merge engine this port does not carry" until
  2026-11-18, which read as a gap waiting on effort. It is a decision:
  the engine was measured when `intent apply` forced the question, and
  reproducing it would mean writing content neither side authored into
  the table that holds the user's files.
- **`intent apply` clears `line_count` and resets `version`.**
  Python's `INSERT` names four columns, and the current row is deleted
  and re-inserted rather than updated, so a file `file set` left on
  version 7 with a line count is on version 1 with a NULL afterwards.
  Both reproduced rather than improved — verified on a copy of the live
  database, `file_contents` row 49986 — and both differ from
  `file set`, which writes the count and leaves the version alone. The
  consequence a reader would notice is `file ls -l` and
  `source revisions`, which print the column.
- **`intent apply` is not transactional upstream, and this one is.**
  The one place the port is more correct rather than merely stricter,
  and it is not a divergence the port chose: Python's four statements
  go through a helper that commits each one, and the `DELETE FROM
  file_contents` runs before the `INSERT` that replaces it. Measured on
  a copy 2026-11-18 — an intent whose `new_content_hash` has no blob
  fails the insert with `NOT NULL constraint failed:
  file_contents.file_size_bytes`, exits 1, and leaves the file with no
  current content row at all: `templedb file cat` then answers "Could
  not read file content from database" for a file that was intact a
  moment earlier. Reported, not fixed here. The port's apply is one
  transaction, so the same input changes nothing, and the statuses
  agree at 1 for different reasons.
- **`search content`'s layout differs too**, not only its hit set.
  Python prints a padded `project_slug / file_path / snippet` table with
  `<b>`-delimited terms; this prints a two-line form per hit with
  `[term]` brackets. The README previously documented only the
  semantics difference, which left the impression the two were
  comparable line-for-line once the hit sets were allowed to differ.
  They are not, and `search content` therefore joins `project list` as a
  command that is not a byte-diff candidate at all.
- **`search content`'s flags do not match.** Python has `-i` /
  `--ignore-case` and `--no-fts`, which this does not; this has
  `--limit`, which Python does not. Both directions, and all of it
  deliberate — see below. The *checking* half of this gap is closed:
  `parity-sweep.sh` now compares flag surfaces directly, which is the
  only way this class of difference is visible at all. It found four
  undocumented divergences the first time it ran. See
  *Flag surfaces* below.
- **Error *messages* differ**, though the statuses no longer do (see
  *The error statuses*). Python routes them through `logging` and
  prefixes `ERROR`; this writes them plain, and the wording is the
  port's own. Both go to stderr, which the sweep counts and does not
  fail on — so ten of the fifteen stderr-only rows are exactly these.
  Closing it would mean reproducing another implementation's log
  formatting, which is a worse contract than the exit code.
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
- **`search content`'s flag surface differs from Python's, deliberately.**
  Decided 2026-10-16, after reading what the two flags the port lacks
  actually do:
  - `--limit` is kept, and Python has no equivalent. Its FTS path
    applies no limit at all and its fallback hardcodes `LIMIT 100`.
    Keeping it is the one place this command is deliberately more
    capable than the original.
  - `-i` / `--ignore-case` is **not** ported. It is dead unless
    `--no-fts` is passed too — `search.py:95` reads it inside the
    `else` branch, so under the default FTS path it is ignored and
    `search content RUSQLITE` returns the same 81 rows with or without
    it. What it selects when it *is* live, a case-insensitive
    substring match, is this port's default behaviour.
  - `--no-fts` is **not** ported either. It switches Python to
    `content_text LIKE '%pattern%'`, and this port has no FTS path to
    turn off — its matching already *is* substring. The flag would have
    to be accepted and ignored, which is worse than absent: a flag that
    promises different matching and silently delivers none.

  So both omissions describe behaviour the port already has. Worth
  stating because the sweep cannot catch this class of difference at
  all: a flag that only one implementation defines is never invoked on
  both sides, so flag-surface drift is invisible to a diff of outputs.
- ~~**A freshly mirrored Postgres has no statistics.**~~ **Closed
  2026-10-21.** `mirror` now runs `ANALYZE` after the copy, outside the
  transaction — statistics are not part of the copy's atomicity, and
  holding the copy's locks through an `ANALYZE` of 210,000 rows would
  extend the one window in which the mirror is unreadable. A failure
  there is reported on stderr and does not fail the command: the data
  is committed and correct at that point, and returning an error would
  send the caller back to a `TRUNCATE`. Verified: all six
  `content_blobs` columns carry statistics after a run, against none
  for that table before.
- ~~**The mirror cannot represent a NUL in a TEXT column.**~~ **Closed
  2026-10-21**, same day it was found. Written up under *The one place
  the two text types are not the same type* below; the short version is
  that the mirror now escapes text on the way in and `backend/pg.rs`
  unescapes on the way out, so a U+0000 survives a round trip through a
  column that cannot hold one. The cost is that the bytes in Postgres
  are not always the bytes in SQLite — a `psql` session sees the escape
  — and nothing reading through this crate does.
- **`summary` is two panes short of Python's nine.** The agent-fleet
  pane needs `agent.store.fleet_rollup()`'s tables, which are outside
  the mirrored subset, so there is nothing to observe yet; the systemd
  pane reads `systemctl`. Both print a `(not ported)` line in the
  position Python prints the pane, and the sweep names them. Omitting
  them silently was the alternative and is worse than a wrong number: a
  missing systemd pane reads as "no failed units" and a missing fleet
  pane as "nothing waiting on you". Python's fleet pane *does* vanish
  when the agent tables are absent, so silence there would have been
  faithful — which is exactly why it is not safe.
- **`vcs log` unpinned prints a different header label**, and only the
  label. With no `TEMPLEDB_SESSION_ID`, Python invents a session and
  says `default view (published + your unpublished)`; this says
  `published only`. The *rows* are the same — a session created a
  moment ago owns nothing — so the divergence is one line, in one
  direction, and it is the honest one: the port has not resolved a
  session and does not claim to have. Closing it would mean creating a
  session row, which is the thing the architecture forbids. Pinning the
  variable makes the two byte-identical, which is what the sweep does.
- ~~**Two session decisions are open.**~~ **Both closed.** Whether to
  refuse outright when `TEMPLEDB_SESSION` is set: no, warn — see
  *Sessions*, 2026-10-23. And whether an unpinned `file set --stage`
  should refuse or stage with a NULL owner: **refuse**, settled
  2026-10-27 when the auto-stage landed. The half of the old answer that
  survives is that a NULL owner means *unstaged* and must never be
  written; the half that did not is "`--stage` is message-only, so the
  port should not have it" — once the stage exists there are two
  outcomes to distinguish and Python has one flag for it, so the flag
  gets the stricter meaning. Written up under *The auto-stage*.
- ~~**`edit_intents` is mirrored at twelve of fourteen columns**, and the
  two omitted are the ones a *reader* would want.~~ **Closed
  2026-10-30**, and the entry had named the wrong unblocker for eleven
  days. It said `cancelled_at` and `applied_commit_id` "belong to
  `intent cancel` and to the commit link `vcs commit` would make,
  neither of which is ported" — two **writes**. `intent show` is
  `SELECT i.*` and prints both, so a pure *read* is what moved them,
  which is the boundary rule exactly as written rather than an exception
  to it. `mirror.rs` carried the same sentence and has been corrected
  too.

  What produced the error is worth keeping: `edit_intents` arrived for
  `file set`'s provenance, so for two weeks it genuinely had a writer
  and no reader, and the reasoning about its missing columns inherited
  that frame and stopped being re-checked. The count was also stale —
  785 rows carried an `applied_commit_id` when the entry was written and
  882 of 1,346 do now. See *The intent reads*.
- ~~**The intent's own write path is partial in one way worth
  naming.**~~ **Closed 2026-10-27** with the auto-stage: migration 088's
  `vcs_working_state.intent_id` link is written now, in the staging
  upsert rather than in a statement after it. What came with it is a new
  gap rather than a clean close, below.
- ~~**The staged row's `intent_id` goes stale on a second write.**~~
  **Closed 2026-10-27**, the same day it was found, and on both sides.
  Python's link was guarded by `AND intent_id IS NULL`, so the row kept
  the first intent that staged it; the port reproduced that with a
  `COALESCE` for one commit, reported it, and dropped the workaround
  when `templedb` BF4E6BEA6BD93C6E fixed the original. The link now
  always names the intent whose hash the row holds. The branch-scoping
  half of the same statement was never reproduced here and is fixed
  there too. Written up under *Three findings from porting the
  auto-stage*.
- **The Postgres schema covers 23 tables and 2 views.** That is the whole
  set the port reads. Recounted 2026-11-17, the live database has 171
  rows of type `table` in `sqlite_master` — 160 once FTS5's ten shadow
  tables are set aside — plus 45 views; the figures here and in the DDL
  header had said 163 tables and 361 indexes since the schema was first
  translated, and the real numbers are 171 and 381. Columns are
  likewise limited to the ones the queries name — four of
  `ingestion_runs`'s fourteen, three of `fleet_machines`'s
  twenty-seven, four of `vcs_sessions`'s ten, seven of `checkouts`'s
  nine, two of `file_types`'s five, eight of `projects`'s.
  Anything not yet ported has nothing to mirror and no way to verify a
  mirror, so the boundary follows the readers, and it moves when one
  does: `project list`'s filters added `is_nix_project` and
  `project_category` on 2026-10-21 and `project_type` on 2026-10-29,
  `vcs log`'s default view added `vcs_sessions` on 2026-10-23, the
  intent reads completed `edit_intents` on 2026-10-30, `file
  where`'s mirror resolution added `checkouts` as the 22nd table plus
  `vcs_sessions.name` / `.started_at` on 2026-11-15, and `search files`
  added `file_types` as the 23rd on 2026-11-17. `intent create`, the
  same day, moved it by **nothing** — the fourth write and the second
  step running at zero schema cost, because every column it fills was
  mirrored for `file set`'s provenance and then completed for
  `intent show`. Worth naming in a list of boundary moves precisely
  because it is not one: a write that needs no new column is what a
  precondition looks like from the far side. `intent apply` on
  2026-11-18 and `file rm` the same day moved it by nothing either, so
  the boundary has now stood still across **four** consecutive steps —
  which is a claim about the *subset* rather than about the steps: a
  write that needs no new column means the table it writes was already
  being read, and that is the only arrangement in which the write can
  be verified at all. The
  `project_type` column and the `checkouts` table are the two additions
  here made because *Python* moved rather than because this port grew a
  reader: the flag-surface stage reported `--type`, and the sweep
  reported three of `file where`'s four tree-reading rows differing.

  `file_types` is the first table here reached **only through a view**:
  no query in either implementation selects from it, and the DDL header
  had named it, by name, as an example of a table the boundary should
  *not* be dragged toward. That it moved anyway is the rule working
  rather than an exception to it — a reader appeared. The header is
  corrected.
- **Two of the 23 tables have no reader at all**, which is the one
  place the rule above reads sideways: `vcs_working_state` (9 of 11) and
  `vcs_commit_metadata` (3 of 40) are carried for a *writer*.
  `vcs_working_state` is now written *and* read inside this port — by
  `file set`'s auto-stage and then by `vcs commit-staged`, which reads,
  unstages and reaps it — which makes it the strongest-verified table in
  the subset as of 2026-10-27 rather than the weakest: the loop writes
  it and reads it back through three readers.

  It was three tables until 2026-10-30. **`edit_intents` left this list
  by gaining readers**, `intent list` and `intent show`, and is now at
  all fourteen columns on the ordinary rule. That is worth separating
  from "the exception shrank": the table did not change, the reasoning
  about it did. Its two missing columns had been explained by naming two
  *writes* as their unblocker, and a read was what they needed — an
  exception stated often enough starts getting used as a reason. See
  *The intent reads*.

  `vcs_commit_metadata` is the one left out: measured
  while porting the commit, **nothing in the port's reach writes it**,
  because only `templedb commit <slug> <dir>` does. For that table the
  cross-backend digest over the copy is still the only thing verifying
  it, which is a weaker claim than every other table here has and is
  stated rather than left to be inferred from a passing sweep.
- **One column is carried for a writer that no reader in *either*
  implementation names**: `vcs_file_states.file_path`, migration 118's
  denormalised path. `vcs show` resolves paths by joining
  `project_files` on both sides. It is carried because
  `vcs commit-staged` is the only path in the system that hard-deletes
  that `project_files` row, so for a committed deletion this is the last
  record of which file the state belonged to. The consequence is
  asserted rather than wished away: after a committed deletion
  `vcs show` lists nothing, because its reader inner-joins the row that
  is gone. See *The second write*.
- **`vcs_branches.head_commit_id` is the first column whose two sides
  are not a copy of each other.** Every other cross-backend comparison
  in this README compares SQLite against Postgres after `mirror`
  copied it. That column is advanced by a trigger on the live side and
  by an application-level compare-and-swap here, because the port has
  none of the 61 live triggers. The CAS is *more* correct than Python's
  arrangement allows — see *The second write* — but "produced by
  different mechanisms" is a different claim from "identical after a
  copy", and only the digest-after-mirror half is covered by the sweep.
- **`vcs_commit_metadata` is 3 of 40 columns because 37 are unanimously
  empty**, not because the port is being frugal. Measured over all 436
  live rows: `intent`, `change_type`, `scope`, `tags`, `ai_tool`,
  `review_status` and `confidence_level` are non-NULL on *zero* rows,
  and `is_breaking` / `ai_assisted` are 0 on every one — they are
  reachable only through `commit --interactive` or an explicit flag and
  nobody has ever passed one. The single populated column is derived:
  `impact_level` is `'high'` above 50 staged files, `'medium'` above 20,
  else `'low'` (410 / 18 / 8). It is `vcs log`'s `files_changed` again —
  a count computed and read by nothing — except this one is stored.

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

### The project's own Postgres

```bash
just pg-up          # initdb if absent, start, create both databases + the ro role
just mirror         # copy the read subset from live SQLite into templedb_parity
just sweep-pg       # the 92 invocations, SQLite vs Postgres
just pg-verify-ro   # prove the read-only role is enforced, not assumed
just pg-down        # stop; pg-nuke throws the cluster away
```

The server comes from this flake, not from the host and not from the
`system_config` project. The port is a separate project that owns its
database: the version is pinned the way the toolchain is, the scratch
database is *meant* to be disposable rather than a persistent service,
and changing a test database should not require `nixos system-switch`.

**Two databases, because `mirror` opens with `TRUNCATE`.**

| | written by | the port's access |
|---|---|---|
| `templedb_parity` | `mirror` only | **read-only role** (`templedb_ro`) |
| `templedb_scratch` | the port | read-write, disposable |

Sharing one database would make a sweep run after a write test report
the port's own writes as parity failures, and the next re-mirror would
silently destroy whatever the write tests built — each looking like a
bug somewhere else. The read-only role is the part that makes the split
a guarantee rather than a convention, and it is what replaces
`SQLITE_OPEN_READ_ONLY` when the file handle goes away: the server
refuses the write instead of the caller remembering a flag.
`just pg-verify-ro` asserts both halves, because a role that is only
believed to be read-only is worth nothing.

**The connection strings are not in this repository.** They are
project-scoped TempleDB variables on `templedb-rust`:

```
$ templedb var list templedb-rust
  TEMPLEDB_URL        …dbname=templedb_parity    (read, as templedb_ro)
  TEMPLEDB_WRITE_URL  …dbname=templedb_scratch   (write scratch)
```

`conn.rs` already reads `TEMPLEDB_URL` from the environment and falls
back to SQLite when it is unset, so a project variable feeds the port
with no code change at all — `templedb env direnv generate` exports them
on entry to the directory. Keeping them there rather than in
`flake.nix` means they are discoverable from the project they belong to
and can differ per machine without editing tracked files.

Measured 2026-10-17, with the mirror above: **49 of 49 invocations are
stdout-identical between the SQLite and Postgres backends**, through the
read-only role — and on this axis stderr agrees too, which is why the
summary reports zero stderr-only rows where the Python axis reports
fifteen. The Postgres read path is not a plan.

Re-measured 2026-10-21 with `summary` added: **49 of 49 invocations and
7 of 7 `summary` panes identical**, which exercises the three
dialect-split queries in `summary.rs` — the bare-column `GROUP BY`
against `DISTINCT ON`, the fleet subquery, and `instr` against `strpos`
— through a renderer for the first time. The result also settles the
seventh pane: the reconciliation listing agrees across both backends
and disagrees with Python, which is what a tie-break this port added
should look like and not what a translation bug looks like. Had the two
backends disagreed with each other as well, the diagnosis would have
been the other one.

**This axis was blocked for several hours the same day** and is open
again. A blob containing a NUL landed in the live database between that
measurement and the next mirror run, which `mirror` could not represent
in Postgres `text`; see *The one place the two text types are not the
same type*. The staleness guard behaved correctly throughout, which is
worth recording because it is the case it was built for:
`mirror --check` exited 4 naming every drifted table and `sweep-pg`
refused to run rather than producing a comparison against a mirror it
could not vouch for.

Re-measured 2026-10-21 after the escape landed, against a mirror
rebuilt with `--create-schema` for the two new `projects` columns:
**50 of 50 invocations, 7 of 7 `summary` panes, 19 of 19 flag surfaces
and 8 of 8 `project list` filters identical**, and **all 17 tables
digest identically over 211,617 rows**. That last number is the one
that matters for what comes next: it is the same comparison the write
path's verification rests on, and it now holds over a column the two
engines cannot represent the same way.

Re-measured 2026-11-16 with `checkouts` as the 22nd table and
`vcs_sessions` at four columns, after a `just pg-reschema` for the new
table and a full re-mirror: **92 of 92 invocations stdout-identical
across the two backends, 0 unexpected, 0 differing even on stderr**, 7
of 7 panes, 25 of 25 flag surfaces, 13 of 13 filters, and **all 22
tables digest identically over 274,270 rows**. The six fenced tree rows
churned out zero times on either axis.

Re-measured 2026-11-17 with `file_types` as the 23rd table and
`files_with_types_view` as the second view, after another
`just pg-reschema` and re-mirror: **101 of 101 invocations
stdout-identical across the two backends, 0 unexpected, 0 differing
even on stderr**, 7 of 7 panes, 26 of 26 flag surfaces, 13 of 13
filters, and **all 23 tables digest identically over 277,295 rows**,
`file_types` at 188 of them. The six fenced rows churned out zero times.

Re-measured 2026-11-17 after step 31, with no schema change and so no
`pg-reschema` — only a `mirror-refresh`, because `intent create`
added no table and no column: **101 of 101 invocations
stdout-identical across the two backends, 0 unexpected, 0 differing
even on stderr**, 7 of 7 panes, **27 of 27 flag surfaces**, 13 of 13
filters, and **all 23 tables digest identically over 279,009 rows** —
2,714 more than the morning's run, which is ordinary drift from the
day's `templedb commit`s rather than anything this step wrote. The six
fenced rows churned out zero times.

That run is also the one place in this README where this axis found a
defect the Python axis could not: `search files` was 100 of 101 before
the fix, because SQLite's `LIKE` folds ASCII case and Postgres's does
not. See step 30.

The new table is why this axis was re-run rather than taken on
yesterday's numbers: `checkouts` is read by the one command here that
turns a database row into a filesystem path, so a disagreement between
the engines about that column would be a disagreement about which file
gets hashed. `checkout_path` is `text COLLATE "C"` for the reason every
other path column here is, and the digest is order-independent so a
collation difference cannot show up as a false match either.

Re-measured 2026-10-29 with `projects.project_type`, `doctor history`
and the last of `file set`: **60 of 60 invocations against Python, 7 of
7 panes, 21 of 21 flag surfaces, 13 of 13 filters, 0 unexpected**;
cross-backend **57 of 57**; and **all 21 tables digest identically over
242,515 rows**.

The cross-backend axis took four attempts and none of the failures was
this port. Something writes the live database continuously — it grew
from 242,470 to 242,515 rows over the hour — so `mirror --check`
reported 7 stale tables within minutes of a refresh, and twice the
sweep reported one difference on `search content checkout -p templedb`,
which is the row most exposed to churn because it reads the contents of
the project being committed to. Diffing that one invocation by hand
immediately afterwards gave byte-identical output on both backends. It
is the condition the staleness guard exists for, seen from the inside:
the guard catches a mirror that was stale *before* the run and cannot
catch one that goes stale *during* it.

Two of those numbers moved for a reason worth separating. The filter
count went 8 → 13 because `--type` is a *third* filter on
`project list` and the five new rows include `--type service` and
`--category service`, which select different projects — the one
spelling that makes confusing the two columns invisible. And the flag
count went 20 → 21 because `doctor history` joined the stage: it is
the first command added to this port since the stage was built, so it
is the first one the coverage test could have caught being left out of
it, and that test now exists.

Re-measured 2026-10-23 with `vcs_sessions` added and `vcs log`'s three
views ported: **54 of 54 invocations, 7 of 7 `summary` panes, 20 of 20
flag surfaces and 8 of 8 `project list` filters identical**, and **all
18 tables digest identically over 216,318 rows**. The new thing this
exercises is the session lookup itself — `ended_at IS NULL` against the
mirrored table rather than the live file — so a pinned session resolves
the same through the replica as through SQLite, which is the property
`file set --session` will stand on.

Re-measured 2026-10-25 with `vcs_file_states.file_path`, the column
`vcs commit-staged` writes: **51 of 51 invocations stdout-identical, 7
of 7 panes, 20 of 20 flag surfaces, 8 of 8 filters, 0 unexpected**, and
**all 21 tables digest identically over 223,811 rows** —
`vcs_file_states` at 7,200 of them, now with the path in the projection.
`vcs commit-staged` is absent from the flag-surface stage on purpose:
the name is this port's, so there is nothing on the other side to
compare it against. That is the point of the name; see *The second
write*.

Re-measured 2026-10-27 with the auto-stage: **54 of 54 invocations, 7 of
7 panes, 20 of 20 flag surfaces, 8 of 8 filters**, and **all 21 tables
digest identically over 226,512 rows**. The flag count is the one worth
reading twice: it was 20 of 20 before this step as well, with `-s` and
`--stage` excused by the allow-list, and it is 20 of 20 now with those
two entries deleted. An allow-list only suppresses differences, so a
flag that becomes present on both sides stops producing one — the count
does not move and the stale entry has to be found by hand.

Re-measured 2026-10-24 with `vcs_working_state` and
`vcs_commit_metadata`: **54 of 54 invocations, 7 of 7 panes, 20 of 20
flag surfaces, 8 of 8 filters**, and **all 21 tables digest identically
over 222,890 rows**. Three of those 21 had no reader at all at the
time, so the digest over the copy was the only thing verifying them.
Two now have a writer behind them as well; `vcs_commit_metadata` still
does not, and is named as such in *Known gaps*.

Re-measured earlier the same week with `edit_intents`: **54 of 54
invocations, 7 of 7 panes, 20 of 20 flag surfaces, 8 of 8 filters**, and
**all 19 tables digest identically over 217,919 rows**. Nothing reads
`edit_intents` here, so what the digest covers is the copy itself — the
twelve-column projection over 1,220 rows, 976 of them with a NULL
`session_id` — which is the only check a write-only table can have
before something reads it.

**The sweep refuses to run on a stale mirror**, and that guard was
written the same afternoon it was needed. `sweep-pg` diffs SQLite
against Postgres, so it only tests *this port* while both hold the same
data; the moment the live SQLite moves it silently becomes a test of
replication lag. Publishing added rows and the sweep went from 40 of 40
to six differences — `project list`, `entity stats`, `entity explore`,
`entity trace` and two `entity search` rows — not one of them a bug.
Six convincing parity failures is a worse outcome than a refusal,
because somebody will spend an afternoon on them.

So `mirror` now records what it copied, in a `mirror_provenance` table
it creates on demand and writes **inside the copy's transaction** — an
interrupted run must not leave counts vouching for a copy that rolled
back.

```bash
just pg-check         # fresh: all 23 tables match the source
just mirror-refresh   # when it isn't
just sweep-pg-fresh   # refresh, then sweep -- the form to actually use
```

`mirror --check` exits 4 on drift and names the tables with their two
counts. A mirror predating provenance reports *cannot vouch for itself*
rather than *fresh*, because defaulting to fresh would disarm the check
on exactly the mirrors needing it. `sweep-pg` runs the check first and
exits 2 rather than producing a comparison it cannot stand behind.

Row counts catch inserts and deletes — what actually drifts here — and
**not updates in place**: a row whose `is_current` flipped leaves the
count alone. A per-table checksum would catch those, at the cost of
reading every row on both sides on every check, which is the copy the
check exists to avoid. Six unit tests pin the comparison, including the
grew/shrank/absent cases and the no-provenance one, and they need no
server because the comparison is split from the queries.

Note that `sweep-pg` on its own refuses more often than it runs: any
`templedb commit`, in any project, adds rows. `sweep-pg-fresh` is the
convenience form, and refreshing is deliberately *not* folded into
`sweep-pg` — that would hide the staleness the check exists to surface.

**Neither the cluster nor its socket is inside the project tree**, and
both exclusions were learned rather than designed.

The socket has to be short: a Unix socket path is capped at 107 bytes,
and this project is normally checked out into a TempleDB edit workspace,
where `.pgdata/sock/` lands at 109. The server starts, creates no
socket, and exits `FATAL` with nothing about path depth in the message
until you read the log. It lives in `$XDG_RUNTIME_DIR` (44 bytes), mode
0700, with no TCP listener at all.

The data directory lives under `$XDG_DATA_HOME` because a `.gitignore`
entry does not protect it. TempleDB's importer only consults git's file
list when the tree *is* a git repo, and an edit workspace is not one —
so with the cluster at `.pgdata/` the scanner classified
`postgresql.conf` and three siblings as `config_file` and
`templedb commit` pulled them into the database. A live database
directory inside a tree that gets scanned *and* materialised is wrong in
both directions: cluster internals land in `content_blobs`, and a later
materialise could write over a running server's state.

### Checks

```bash
just check-all         # the whole gate, Postgres included. Point CI here.
just check             # the SQLite+Python half: tests, oracles, parity sweep
nix flake check        # the hermetic half only
```

`just check-all` is what to run before publishing, and it exists
because `just check` was being read as the whole gate and never was.
`check` is five things in order — `cargo fmt --all --check`,
`cargo clippy --all-targets -D warnings`, `cargo test --workspace`, the
three CPython oracles (`difflib`, the clock and the path classifier), and
`scripts/parity-sweep.sh python` — and it skips the cross-backend
sweep, the mirror freshness check and the write tests, which are the
*only* verification the write path has. `check-all` adds those three,
cheapest-first so a failure arrives early. It needs a running cluster
(`just pg-up`).

**The first two of those five arrived 2026-11-17 and are step 32.**
Both had been true of this project and false of its gate: the README
claimed `clippy` clean and `fmt` clean in four step entries, and
nothing ran either, which is how two formatting hunks survived twelve
days in `files.rs`. They go first because they are the cheapest and
because `fmt` needs no build at all.

The sweep is every implemented invocation diffed against the Python
`templedb`, then three stages for what a whole-stdout diff cannot
reach: `summary` one pane at a time, the flag surfaces of 29 commands,
and `project list`'s filters by which projects they select — plus the
tree fence, which runs the rows that read a directory between two
digests of it. It exits with the number of *unexpected* differences, so
the deliberate divergences do not fail it and a new one does.

Four of those 29 flag surfaces belong to *writes* — `intent cancel`,
`intent create`, `intent apply` and `file rm` — which is the one half
of the write path this stage can see. It can, because all four share
Python's command name, where `file set`'s shape differs and
`vcs commit-staged` has a name only this port has.

**Those two numbers were 27 and "two" until 2026-11-18**, written when
they were right after step 32 and not revisited by step 33, which took
the surfaces to 28. Corrected here with the error named rather than
quietly edited, because this is the same decay the heading of the
*Status* section had and the same reason: `gate_counts.rs` checks ten
numbers and all ten are test counts. **Nothing in the gate counts
commands or flag surfaces** — the sweep prints the figure it actually
compared, which is the only one that cannot be stale, and a number
written here is a claim about a list in a shell script that no test
reads.

A current clean run reads (2026-11-18, after step 34):

```
pinned    TEMPLEDB_SESSION_ID=1330, which owns unpublished commits in bza
summary   7 of 7 claimed panes identical, 2 not ported
flags     29 of 29 commands agree (deliberate divergences allowed)
filters   13 of 13 project-list filters select the same projects

templedb-rs vs python, 101 invocations: 93 stdout-identical
(24 differing on stderr only), 17 deliberate, 0 unexpected
fence     6 of 101 rows read a tree and ran between two digests of it; 0 churned out
```

The `pinned` line is the sweep reporting which session it put both
implementations in, and whether that session owns anything — see
*Sessions*. Three of the 101 rows are appended by that search rather than
written down, so the count moves with the data; the line says what was
covered. The `fence` line is the tree rows, and it reports what was
*not* checked as well as what was: a row whose tree moved through all
three attempts is `churned` and is counted in neither column.

**What the sweep cannot see is now checked by `cargo test`.** Its
coverage — whether every command in the binary appears in it at all —
is asserted hermetically by `crates/templedb-cli/tests/sweep_coverage.rs`,
which also catches an exemption that has outlived its command. See
*Checking the checker* for why that is the answer to the *known gap*
this section used to carry and the whole-sweep-as-a-test is not.

**And what no check could see at all is the prose.** Since 2026-11-17
`crates/templedb-cli/tests/gate_counts.rs` reads this document and
fails if a number in it no longer describes the tree — ten of them, the
test count and its breakdown among them. It is the second file here
whose subject is a check rather than a command, and the first whose
subject is this README. See *Counting what the gate claims*.

The deliberate count went from 16 to 23 when `file set` landed — seven
of its flags are absent on purpose and each is listed individually, so
porting any one of them shows up here as a change rather than
disappearing into a waved-at exception — and has fallen to 14 as
sessions, the intent record, the session pin, the auto-stage and
finally `--commit` closed `vcs log`'s default view, its `--session`
flag, and `file set`'s `--skip-intent`, `--session`, `-s`, `--stage`,
`--commit`, `-m` and `--commit-msg`. The one entry that arrived from
the other direction is `--no-rebase`, which Python grew and this stage
reported. It went to 15 on 2026-11-17, and the new entry was a *row*
rather than a flag: `search files get_`, the LIKE-metacharacter
divergence. `search files` itself added no flag exemption at all, which
was the point of giving it no `--limit`.

**It read 17 hours later with nothing changed, and that number is not a
property of the code.** Step 32 decomposed it, because a figure this
paragraph has tracked across eight steps turns out to be two different
things added together: **9** are flag-surface pairs, which is every
entry of `FLAGS_EXPECTED` and moves only when one side grows a flag,
and the other **8** are invocation rows that *actually differed on this
run*. Three of those eight are the `intent list --all-statuses` rows,
and whether each differs depends on where the newest `created_at` tie
group sits in the live data — 6 rows differed when step 31 measured and
8 forty minutes later, because step 31's own commit wrote six
`file set` intents inside four seconds and two of them share
`2026-11-17 21:38:01`. See *The tie-break Python does not have*, which
predicted exactly this and is why those rows are exempted by name
rather than by argument choice. The lesson for *this* paragraph is
narrower: "the deliberate count" is not a number to track, and the
9 is.

`just write-test` is **not** part of `just check`, because it needs a
Postgres server and `check` is meant to run anywhere the build does.
It *is* part of `just check-all`, which is the recipe to reach for when
touching the write path — and the reason that recipe exists.

`just pg-reschema` is the one to remember when a write path lands,
because the subset DDL is not static — it has grown a column or an
identity with every writer so far. `mirror --create-schema` is
CREATE-only and cannot apply a change to a database that already has
the tables, and the symptom is remote from the cause: `mirror` fails
inside a binary `COPY` with a column-count mismatch. The recipe drops
and re-applies both schemas from the file and restores the
`templedb_ro` grants that `DROP SCHEMA` takes with it; follow it with
`just mirror-refresh`. Destructive and deliberately not a dependency of
anything — parity is a copy and scratch is disposable, so nothing is
lost, but refilling parity is a 242,470-row recopy.

Each of the three stages has been sabotage-tested — a deliberate
regression introduced, then the stage confirmed to report it and the
script to exit non-zero. That is not ceremony. **Two of the three were
wrong when first written, and both failed in the bad direction**: the
filter stage reported six failures against a correct port because it
looked for rows under a dashed rule only Python prints, and the summary
stage required *every* pane to agree before it stopped retrying — so
one continuously-churning pane consumed the attempts and the rest were
judged against whatever the last capture held. It reported a false
failure the first time an ingest ran during a sweep.

The summary stage now tracks agreement **per pane across attempts**: a
pane that matched on any attempt is agreeing, and only one that
differed on every attempt is reported. That is sound because a real
divergence is not intermittent. It has to be this way rather than
simpler, because the thing being compared is two processes reading a
database a third is actively writing — seven ingest adapters run on
timers and the `ok_count` the pane prints increments on every run, so
that pane can legitimately differ on every attempt. Verified stable
over four consecutive runs.

A check that cannot fail on demand is not evidence of anything, and one
that fails for its own reasons is worse than none.

The split between the two commands is not organisational, it is about
what needs the live database:

| | `nix flake check` | `just check` |
|---|---|---|
| `cargo fmt --all --check` | yes | yes |
| `cargo clippy --all-targets -D warnings` | **no** | yes |
| unit tests (481), the two self-checks included | yes | yes |
| CPython oracles (difflib, clock) | yes | yes |
| CPython oracle (importer.scanner) | **no** | yes |
| 101-invocation parity sweep, + `summary`'s panes | **no** | yes |

`nix flake check` builds in a sandbox with no live SQLite file and no
Python `templedb` installed, so the sweep cannot run there and is not
wired in pretending otherwise. What it *can* do it now does: the
`checkPhase` runs `cargo fmt --all --check`, then the unit tests, then
re-runs two of the three oracle targets with `--ignored`, from the
crate directory, because `cargo test` alone skips them and the one that
reads this repository's own source needs that working directory.

**Two of three, and the third is the same sandbox limit one row down
rather than an omission.** `classify_parity.rs` imports
`importer.scanner` out of the installed `templedb` package — through
the `PYTHONPATH` that package's own launcher exports, which is what
makes it diff against the same `scanner.py` the sweep's `templedb`
runs — and the sandbox has neither the package nor the launcher. The
other two need only the standard library. So the `checkPhase` names its
two targets one at a time rather than running a bare `--ignored` over
the workspace, which would have picked the third one up the day it
existed and failed the flake with something that reads like a broken
check instead of a missing dependency. `just oracle` runs all three.

**`clippy` is in `just check` and deliberately not in the flake**, and
the line is drawn by build cost rather than by purity: `fmt` reads the
source and needs no build at all, while `cargo clippy --all-targets`
recompiles the workspace and its dependencies under a second driver
inside a sandbox that shares nothing with the dev shell's `target/`.
Both are hermetic and both are in the gate CI is pointed at; adding the
expensive one to the cheap check would make the cheap check the slow
one for no coverage this project does not already have.

Verified running rather than merely compiling: the formatting check,
every default test and the 5 sandbox-runnable oracle tests pass inside
it. This sentence used to carry the sandbox's own per-target counts —
"218 + 13 unit tests" — and they are gone rather than corrected, on
step 32's rule: a build log is not a file this tree can read, so no
number written here about one can be checked by anything.

The 5 is `difflib_parity`'s 3 plus `clock_parity`'s 2, and it is now
**two fewer than the 7 the paragraph near the top of this file
counts** — which is `just oracle`'s figure and includes
`classify_parity`'s 2. The two numbers describe different things and
only one of them is gate-checked: the 7 is a count of `#[ignore]`d
python3 tests in this tree, which `gate_counts.rs` asserts, and the 5
is a count of what a *sandbox* can run, which nothing here can check
for the reason the paragraph above gives. Said out loud because two
numbers that used to be equal and are now not is exactly the shape of
drift this file keeps finding in itself.

Both `checks` and `packages` build with the same `rust-bin` toolchain
the dev shell pins, through `makeRustPlatform`. The nixpkgs default
would have been a second compiler, which is the failure the comment on
`rustToolchain` in `flake.nix` already records once.

The sweep recipes invoke `bash scripts/parity-sweep.sh` rather than
executing the script, because the execute bit is present in exactly the
tree where it is not needed. TempleDB's content store does not carry the
mode, so a fresh `templedb edit` workspace — which is where `just` runs
— has the script at 644 and a direct call fails with `Permission
denied`. `publish` restores it from git's index when it materialises the
canonical checkout, which is read-only and is not where anyone runs the
sweep. `chmod +x` in a workspace works until the next materialise.

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
  statediff.rs         order-independent per-table digests, so a write's
                       effect can be compared across two engines
  projects.rs          project list
  files.rs             file cat / ls
  search.rs            search content — candidate SQL + orchestration;
                       search files — one query over the view, and the
                       ORDER BY Python does not have
  textsearch.rs        matching, BM25, snippets; no SQL, no backend
  classify.rs          importer.scanner's 179-entry path classifier and
                       its six-entry skip list, in order — the first
                       module here whose subject is a Python module
                       *constant*. Pure, path-only, no SQL, no backend
  retype.rs            `file retype`'s read half: the query, the move
                       grouping and the renderer. The ORDER BY Python
                       does not have, for the second time
  summary.rs           summary's observation layer: facts only, no clock
  entities.rs          entity stats / explore / trace / search / paths
  source.rs            source snapshot / revisions
  doctor.rs            doctor history, and the measurement of why
                       doctor entities is not here
  intent.rs            intent list / show — the two reads that moved
                       edit_intents to all fourteen columns
  fuzzy.rs             project name resolution
  like.rs              LIKE-pattern escaping, shared by the three
                       modules that take a user string
  exit.rs              the three exit statuses, and the one error type
                       that carries a non-default one
  clock.rs             one injectable instant; julianday and _age_hint,
                       reproduced to the bit. No SQL, no backend
  session.rs           resolve-or-decline: the session the caller named,
                       validated live. Two of Python's seven steps, and
                       never the two that write
  checkouts.rs         which directory holds a project's content: the
                       read half of the checkout-role resolver, as one
                       pure function over rows plus three predicates.
                       Two of Python's four purposes, and the one place
                       TEMPLEDB_SESSION is read by name — because this
                       lookup, unlike session.rs's, creates nothing
  writer.rs            PgWriter — the one writable handle, outside the
                       Store trait on purpose. Opt-in, scratch-only
  fileset.rs           `file set`: the first write, and its six guards —
                       the sixth being the auto-stage that `commit.rs`
                       below consumes
  filerm.rs            `file rm`: one upsert, and the writer the
                       deletion branch of `commit.rs` had been missing
                       since step 20. Deletes nothing itself
  commit.rs            `vcs commit-staged`: the database half of Python's
                       `vcs commit`, with the tree-dependent half
                       deferred, and the branch head advanced by a
                       compare-and-swap the trigger over there makes
                       impossible
  pgtext.rs            the NUL escape: SQLite TEXT holds U+0000 and
                       Postgres text cannot. No SQL, no backend
  testdb.rs            in-memory fixture (cfg(test))
schema/postgres/       the DDL the mirror targets
crates/templedb-cli/   clap binary `templedb-rs`, formatting only
  main.rs              the command tree, and the impure inputs resolved
                       at the edge: the clock, the handoff identity, and
                       the VCS session pin
  summary_view.rs      summary's interpretation layer: facts plus an
                       instant to lines. Ages, colour, column widths
  table.rs             Python's Command.format_table, byte for byte —
                       the first module here whose subject is a Python
                       *method* rather than a command. Deliberately not
                       shared with project list, which has its own form
                       and is documented as not byte-comparable
  where_view.rs        `file where`'s tree half: the mirror probes, the
                       overlay walk, and the rendering. The only module
                       in the port that touches a filesystem — the two
                       TempleDB roots it probes come from checkouts.rs
                       above, not from a path built here
scripts/
  parity-sweep.sh      every implemented invocation, against Python /
                       the other backend / a previous build — the count
                       moves with the data, because three rows are found
                       by searching it. Plus three stages for
                       the commands whose whole stdout cannot be
                       diffed: `summary` one pane at a time, the flag
                       surfaces of 26 commands, and `project list`'s
                       filters by which projects they select. And the
                       tree fence, which runs the rows that read a
                       directory between two digests of it. Not run
                       by `cargo test` — it needs the live database.
                       Its *coverage* is, by sweep_coverage.rs below.
crates/templedb-cli/tests/
  sweep_coverage.rs    does the sweep still name every command this
                       binary has? Hermetic — walks the binary's own
                       --help tree and parses the script. Also checks
                       the other direction: an exemption that has
                       outlived its command suppresses nothing.
  gate_counts.rs       do the numbers in README.md still describe this
                       tree? Hermetic — counts `#[test]` per target and
                       parses ten claims out of the prose above
                       *Porting order*, itself included.
crates/templedb-db/tests/
  difflib_parity.rs    CPython difflib as an oracle (needs python3)
  clock_parity.rs      SQLite julianday as an oracle (needs python3)
  classify_parity.rs   importer.scanner as an oracle over 24,757
                       generated paths, plus a coverage check that the
                       corpus reaches every type the table can produce
                       (needs python3 AND the templedb package)
  write_parity.rs      `file set` and `vcs commit-staged` against a real
                       Postgres, read back through the ported readers
                       (needs a server)
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
   session-scoped default was deliberately absent until step 16 closed
   it.
6. **Done** — `vcs show`, metadata and changed-file list. Its per-file
   diff followed in step 8.
7. **Done** — `project show` and `vcs branch`, the two deterministic
   read commands left outside the difflib question. `project show`
   needed four more mirrored columns and `vcs branch` three.
8. **Done** — CPython's `difflib.SequenceMatcher`, and with it
   `vcs show`'s per-file diffs. This was listed as a decision rather
   than a command and the decision was to reimplement; see below.
   `vcs_commit_parents` is the eleventh mirrored table.
9. **Done** — the fuzzy *file* matcher, and `vcs diff`'s three
   deterministic forms. `file_contents.version` is the one column the
   mirror gained. See below for the form that cannot work.
10. **Done** — `vcs diff --side-by-side`, which is
    `difflib.context_diff`: a second renderer over the
    `grouped_opcodes` this port already had, and the last output format
    in the `vcs` family. The read layer was then as complete as it could
    get without sessions, which is what step 16 went and got. See below
    for what the flag is actually called for.
11. **Done** — the all-commands audit. Every implemented command
    diffed against Python in one sweep, rather than per-command
    harnesses retired on completion. Nine of the twelve bugs this
    README reported are fixed upstream; three commands had silently
    drifted behind and are fixed here. See *The audit* above. The
    lesson is a method change, not a command: the sweep replaces the
    per-command harness. It is now wired in rather than sitting in
    `scripts/` waiting to be remembered — `just check` runs it, and
    `nix flake check` runs the half that needs no live database. See
    *Checks* above.
12. **Done** — the error statuses. Not a command: the write-path
    design document names it as one of three things to do *before* any
    write lands, on the grounds that a harness which cannot distinguish
    a rejected mutation from a crashed one is not a harness. It is also
    the last read-layer divergence that was a gap rather than a
    decision. See *The error statuses* above.
13. **Done** — the clock seam. The design document's *first*
    precondition, and like the exit codes it is not a command:
    `crates/templedb-db/src/clock.rs` is one injectable instant plus
    the age arithmetic Python does in SQL, pinned against CPython
    driving SQLite over 2,000 timestamp pairs. It unblocks `summary`,
    whose output is relative ages, and it is what a state-diff harness
    needs to stop excluding the columns most likely to be wrong. See
    *The clock seam* below for what reproducing it turned up.
14. **Done, at seven panes of nine** — `summary`, and with it a
    question about what a command *is*. Its observation layer landed
    2026-10-18: six more mirrored tables (`ingestion_runs`,
    `invariant_checks`, `reconcile_runs`, `fleet_machines`,
    `handoff_notes`, `reconciliations`) and eight pure functions in
    `summary.rs` that return facts with absolute timestamps and read no
    clock. The rendering layer landed 2026-10-21 as
    `templedb-cli/src/summary_view.rs` — one function from facts plus an
    instant to lines — along with the session identity, which is
    `TEMPLEDB_SESSION_ID` or `(gethostname, getppid)` resolved at the
    entry point and threaded in.

    **Six of the seven ported panes are byte-identical with Python on
    live data**, measured over three consecutive runs: the entity graph,
    ingestion, doctor invariants, per-machine reconcile, Python hygiene
    and the handoff inbox. The seventh diverges by one line order and on
    purpose; see below.

    Two panes are **not** ported and print `(not ported)` where Python
    prints them: the agent fleet, whose tables are outside the mirrored
    subset, and systemd, which reads the machine. See
    *`summary` is not a function of the database* below, and the
    TempleDB report of the same name.
15. **Last, and not without a design document** — any write path, and
    with it the rest of the Postgres schema. The design document now
    exists, as the TempleDB report
    *The write path: what porting it actually requires*
    (2026-10-15). Its conclusion is that neither blocker is SQL: the
    byte-diff method cannot verify a mutation, and `Store` — two
    methods, `query` and `dialect`, over a `READ_ONLY` handle — has no
    way to perform one. It recommends a clock seam, closing the
    error-exit gap and a state-diff harness *before* any command, and
    `file set` rather than `commit` as the first write. **All three
    preconditions are now done** — the exit codes (step 12), the clock
    seam (step 13) and the state-diff harness (step 15a below).

    **This entry said `SqliteWriter` until 2026-10-21 and that was
    wrong.** The recommendation is in the document's body, and the
    document's own 2026-10-16 decision — recorded two paragraphs down
    in the same report, and summarised in 15a below — reverses it:
    Postgres is primary and the port does not carry SQLite. The first
    write therefore goes to the **write scratch** database as a
    concrete `PgWriter`, following `mirror`'s precedent of owning a
    `postgres::Client` outside the `Store` trait. The reasoning against
    widening `Store` survives the reversal intact and is the part worth
    keeping: a write trait would have one implementation, widening it
    costs every reader its read-only guarantee, and
    `rusqlite::Transaction` / `postgres::Transaction` borrow their
    connection so neither fits behind `&dyn Store` anyway.

    Reading a stale recommendation out of a superseded section is
    exactly the failure the report warned about when it said it would
    "record what it changes rather than quietly leaving the body of the
    document wrong". The body is still wrong; this entry no longer
    repeats it.

15a. **Done** — the state-diff harness, `crates/templedb-db/src/statediff.rs`
    and `templedb-rs state`. One digest per mirrored table, built so
    that the *same* function run against SQLite and against Postgres
    produces comparable values — which is what the 2026-10-16 decision
    requires, since the two copies being compared are no longer two
    SQLite files. Measured 2026-10-20: **all 17 tables digest
    identically across both backends over 207,857 rows**, in under a
    second on SQLite. See *The state digest, and why it must not sort*
    below.

    The document also carries a later decision, 2026-10-16: **the port
    stays a separate project and owns a Postgres populated by copy**,
    rather than cutting over. That is why `just pg-up` provisions two
    databases, and it is why the schema boundary still follows the
    readers instead of jumping to 164 tables. It also records
    two boundaries not to cross by accident: 58 of the live database's
    61 triggers are cr-sqlite's, calling functions from an extension
    this port never loads, and the bundled SQLite (3.50.2) is not the
    one Python uses (3.53.3) — which first matters when a write fires
    the FTS5 triggers. Postgres becoming the
    primary store is the same project as porting the writes; the mirror
    is a read replica and does not change that.

15b. **The two questions the design document left open have both
    moved**, and neither blocks the first write any more. Recorded here
    because the document itself says they are "decisions, not research
    tasks" and because one of them was the stated blocker.

    **"What replaces the in-memory test fixture?" is a cutover
    question, not a blocker.** The document calls it "the only open
    question that blocks work", reasoning that Postgres-as-primary means
    no SQLite, so `testdb.rs` has nothing to run on. But the
    *2026-10-16* decision in the same report defers cutover
    indefinitely: the port stays a separate project, data arrives by
    copy, and SQLite remains both Python's authoritative store and one
    of this port's two backends. So the fixture is not going anywhere
    yet, the 248 `templedb-db` tests that stand on it keep working, and
    the property the README advertises — **no Postgres server needed to
    build or to `cargo test`** — survives. The question becomes live at
    cutover, when `backend/sqlite.rs` is deleted; it is not live now.

    What follows for the writer: its own tests do need a server, so
    they go behind `#[ignore]` with a `just` target, exactly as the two
    CPython oracles already do. That keeps the no-server property for
    `cargo test` while still testing the write.

    **"Does `mirror` need a known-instant snapshot mode?" is answered,
    by work that landed after the document.** It asks because the
    state-diff method depends on knowing which SQLite state a Postgres
    was copied from, and "today nothing records it". Something does
    now: `mirror_provenance` (2026-10-16) records `copied_at` plus the
    per-table row count, `mirror --check` compares those against the
    live source and exits 4 on drift, and `parity-sweep.sh` refuses to
    run a cross-backend comparison against a mirror that cannot vouch
    for itself. The `ANALYZE` half of that same question closed
    2026-10-21. What is *not* covered is an update in place — counts
    catch inserts and deletes — which is precisely the gap
    `templedb-rs state` fills.

16. **Done** — sessions, as *resolve never, accept always*. Not a
    command either: `session.rs` is one function at the CLI edge that
    accepts the session the caller named and declines to guess, which is
    two of Python's seven resolution steps and neither of the two that
    write. It closed the last read-layer divergence — `vcs log`'s
    session-scoped default — and with it that command's `--session` flag
    surface, and it added `vcs_sessions` as the eighteenth mirrored
    table at two of its ten columns.

    The precondition document is the TempleDB report *Sessions: the
    identity the port has been refusing to resolve* (2026-10-23), which
    names five blocked items and argues they are one item. This closes
    its first three steps.

17. **Done** — the `edit_intents` record, which is the report's step 4
    and guard five of `file set`'s seven. The nineteenth mirrored table,
    at twelve of its fourteen columns — and the first one here that
    exists for a *writer* rather than a reader, so the projection is the
    columns the writer supplies rather than the columns a query names.

    It also closed `--skip-intent`, the third `file set` flag to leave
    the sweep's allow-list. One structural divergence from Python, argued
    under *The intent record*: the insert goes inside the write's
    transaction rather than after it, which makes the 2026-09-14 lying
    `status = 'applied'` row unreachable by construction instead of by
    ordering, at the price of an intent failure failing the write.

18. **Done, and smaller than planned** — `--session <ID>` on `file set`,
    which the sessions report lists as its step 5 and guard six. Integer
    only, outranking `TEMPLEDB_SESSION_ID`, resolved against the write
    target inside the write transaction.

    Guard six was supposed to be `--stage` as well, and investigating
    the decision the README had been carrying dissolved it rather than
    settling it: `--stage` is message-only in Python, the auto-stage is
    unconditional, and `staged_by_session_id IS NULL` means *unstaged*
    rather than *unowned* in all five readers. The auto-stage therefore
    moved to `vcs commit`, which was the only thing that reads what it
    writes — and then became step 21 two days after the commit landed.
    See *The auto-stage* — the finding is that a table gaining a
    *writer* with no reader is the one shape this port's boundary rule
    does not cover, and `edit_intents` got in on being provenance rather
    than on being written.

    What was left of `file set` after this step: `--stage` / `-s`, which
    is step 21, `--commit` / `-m`, which is still open, and binary
    content.

    One capability closes permanently as a result: `--session <name>`.
    The name path creates when absent, and a port that creates session
    rows has no read-only guarantee left to argue from. See *Sessions*
    below for the whole argument and what it cost.

19. **Done** — `vcs_working_state` and `vcs_commit_metadata`, the 20th
    and 21st mirrored tables, and step 1 of the TempleDB report
    *vcs commit: the widest door, measured* (2026-10-24). Both are
    carried for a writer with no reader yet, which the report argues is
    the right order: `file set --stage` is blocked on the first and
    `vcs commit` consumes both, so mirroring them is what makes the pair
    verifiable by cross-read rather than by assertion.

    Measuring commit to write that report is what moved the numbers.
    The sessions report had guessed "six remaining guards, four tables";
    it is **seven tables written and nine guards**, and
    `project_files` is the one to notice — commit is the only path in
    the system that *hard-deletes* a `project_files` row, which is why
    `file set` needed an un-delete guard at all.

    Two findings are in the report and worth the pointer. **The branch
    head is advanced by a trigger**, and the port has none of the live
    database's 61 — so a faithful port of the *code* leaves the head
    frozen, and the fix is the application-level compare-and-swap that
    Python's own comment records as having been buggy *because the
    trigger beat it to the column*. A port without the trigger is the
    only implementation that can do that CAS correctly: the first place
    "reproduce Python" and "reproduce Python's behaviour" come apart,
    and the first mirrored table whose contents would be produced by
    different mechanisms on the two backends rather than by a copy.

    And **five of the nine guards reconcile a filesystem**, not a
    database. The report's recommendation is therefore to split the
    command: port the staged-rows write and refuse the tree-dependent
    half explicitly, which is what `publish` already does — its guard
    is `COUNT(*) … WHERE staged_by_session_id IS NOT NULL` and it
    commits without scanning anything.

    Measured: **all 21 tables digest identically across both backends
    over 222,890 rows**, 54 of 54 invocations on both axes.

20. **Done** — `vcs commit-staged`, the report's step 4 and its step 3
    together: the staged-rows write, and the branch-head
    compare-and-swap that is the port's one chance to be correct where
    Python cannot be. The report's open question — may the reduced
    command be called `vcs commit` — was settled **no** on 2026-10-25 by
    the `--no-fts` precedent, so the command has a name that does not
    promise the tree half. See *The second write* for the whole
    argument.

    Its steps 3 and 5 came with it. The CAS has the test the report asks
    for, two transactions on two connections racing on one branch; the
    cross-read harness is three readers rather than one, because a
    commit is visible to `vcs log`, `vcs show` *and* `file cat`.

    Three of the report's measurements were over-counts and are
    corrected under *The second write*: `vcs commit` writes **six**
    tables, not seven, and touches neither `vcs_commit_metadata` nor
    `vcs_session_heads` nor `vcs_commits.session_id` — all three belong
    to `templedb commit <slug> <dir>`. The fourth table the work needed
    turned out to be one column, `vcs_file_states.file_path`, which is
    the only thing in the subset carried for a writer that *no reader in
    either implementation* names.

    Its step 2 — `file set --stage` — was the only thing left between
    this and a closed loop, and it is step 21 below.

21. **Done** — the auto-stage, which is `file set` producing the
    `vcs_working_state` row `vcs commit-staged` consumes. The report's
    step 2, and the last of the five items the sessions report named.
    `vcs_working_state` becomes the sixth table with an identity column
    and the first one that is both written *and* read by this port,
    which retires the weakest claim in the subset: it had a writer with
    no reader for two days and a reader with no writer for two more.

    What the work actually was, after the measuring: **one upsert**.
    Everything expensive about it had already been paid for — the
    session resolver in step 16, the clock in step 13, the exit codes in
    step 12, the table in step 19. That is what a precondition being
    genuinely a precondition looks like from the far side, and it is
    worth recording because the README spent three revisions treating
    this as a decision rather than a statement.

    The decision that *was* left is where the flag goes, and it came out
    stricter than Python: with no session the write stages nothing and
    says so, and `--stage` turns that into a refusal. See *The
    auto-stage*.

    It also turned up three bugs in Python's copy of the same path, all
    three **fixed upstream the same day** in `templedb`
    BF4E6BEA6BD93C6E: the un-delete guard that fired unconditionally
    because its query never selected the column it tested, two branch
    lookups that disagreed and staged *nothing* while reporting success,
    and migration 088's provenance link naming the wrong intent after a
    second write. The port reproduced the third for parity and dropped
    that workaround when the fix landed — one commit of divergence. See
    *Three findings from porting the auto-stage*.

    Measured: all 21 tables digest identically across both backends over
    **226,512 rows**; the sweep reports 54 invocations, 0 unexpected
    differences, and 20 of 20 commands agreeing on flags *after* `-s`
    and `--stage` were removed from the allow-list rather than left in
    it. 326 unit tests and 36 write tests.

    Still open from the report: nothing. Still open from Python's
    `file set`: **`--commit` / `-m`**, which runs the commit in-process,
    and binary content. Both closed in step 22.

22. **Done** — the last of `file set`, and the first measurement that
    changed a plan rather than confirming one. Four items, 2026-10-29.

    **`--commit` / `-m`.** `set_file` and `commit_staged` in **one**
    transaction, where Python runs two. The flag implies `--stage`, the
    default message is Python's `file set: <path>`, and the author
    chain is the commit's rather than the write's — which is also
    Python's, since its `Namespace(author=None)` sends `vcs commit` to
    resolve its own. See *`--commit`, and why it is one transaction*.

    **Binary content.** `Content::Binary` stores the bytes in
    `content_blob` as a `binary_asset` with a NULL `content_text`, which
    is the shape Python's scanner writes for the same file. The mojibake
    guard does not run on it, and the line count is 0 rather than a
    count of `0x0a` — both Python's, both with its reasoning. The branch
    exists because a file that cannot be stored is one `materialize`
    later deletes from the checkout as a stray, which is how
    `.authinfo.gpg` broke a Nix build.

    **`doctor history`**, which is a whole new command that cost no
    table and no column: `invariant_checks` was already mirrored, and
    the five columns it carries are exactly the five the command prints.
    Its query names seven — `duration_ms` and `sample_issues_json` are
    selected and thrown away, the same shape as `vcs log`'s
    `files_changed`. Byte-identical across 9 variants.

    **The sweep is checked by `cargo test` now**, which is the
    *Known gaps* entry about it, closed from the only angle available.
    The sweep itself still cannot be a test — it needs the live database
    and the Python binary — but its *coverage* can be, and that is the
    half that was silently rotting. See *Checking the checker*.

    Two things were found rather than built, and both are drift in the
    other direction — Python moved while the port was not looking, which
    is the failure mode the audit's method exists for:

    - **`--if-match` with a malformed argument exits 2, not 1.** This
      port exited 1 on the strength of a split the README described in
      detail and nobody had measured: "a malformed flag is bad input, a
      mismatched one is a conflict". Python makes no such split — both
      leave `set()` through the same `return 2`. Fixed, and the test
      that asserted the wrong value now asserts the measured one. It is
      step 12's work one command later: the exit statuses were closed
      for the read layer and this one sat behind a flag no harness could
      reach, because a write has no diffable stdout.
    - **`project list --type`**, which Python grew and the flag-surface
      stage reported the first time it ran after this work. Ported, with
      `projects.project_type` as the one column the mirror gained — and
      it is a different column from `project_category` with an
      overlapping value, so the fixture and the sweep both test
      `service` specifically. See *The two columns that nearly rhyme*.

    Measured: 60 invocations against Python, **0 unexpected**, 21 of 21
    flag surfaces, 13 of 13 project-list filters, 7 of 7 summary panes.
    Cross-backend 57 of 57, and all 21 tables digest identically over
    **242,515 rows**. 351 unit tests, 5 oracles, 42 write tests, `fmt`
    and `clippy` clean, `nix flake check` passes.

23. **Done** — `intent list` and `intent show`, 2026-10-30. Two reads
    that cost no new table, and the first step here chosen by
    *correcting* the queue rather than by following it.

    The README had `intent cancel` queued as the thing that would
    unblock `edit_intents`' two missing columns. That was wrong in a
    specific and repeated way — the *Known gaps* entry, `mirror.rs`'s
    table comment and the Postgres DDL's header all said the columns
    were waiting on `intent cancel` and `vcs commit`, two **writes**.
    `intent show` is `SELECT i.*` and prints both. A read was all either
    column ever needed, and the port's own ordering rule — reads first —
    had been pointing at the right step the whole time.

    So the step is ordinary and the finding is about the queue: the
    reasoning went stale because `edit_intents` arrived for `file set`'s
    provenance, genuinely had a writer and no reader for two weeks, and
    nothing re-checked the frame when `intent show` became portable. All
    three sites are corrected. `edit_intents` is now at **all fourteen
    columns** and off the writer-only exception list, which drops from
    three tables to two.

    **"All three sites are corrected" was two of three**, found
    2026-11-17 by step 31 and corrected there. *Known gaps* and
    `mirror.rs` were; the Postgres DDL's header paragraph still said
    "Twelve of fourteen" and still named the two writes, eighteen days
    after the columns it describes were added to the table directly
    below it — with the correct account sitting in the same file beside
    those columns. Left in place here with the correction beside it,
    because the shape of the error is the entry's own subject: nothing
    in the gate compares a projection comment against the column list
    under it, so a count in prose decays silently.

    **One deliberate divergence, and it is an ordering one.** Python
    orders `intent list` by `created_at DESC` with no tie-break, and
    `created_at` has second granularity. Measured: **32 of the 1,346
    live `created_at` values are shared by two rows**, so the order
    among those pairs is whatever the engine returns and `--limit` can
    cut inside a tie group — which changes *which* rows appear, not just
    their order. This port adds `id DESC`. The decisive argument is not
    tidiness but that the port has two backends and the sweep measures
    them against each other: without a total key, SQLite and Postgres
    are free to disagree. See *The tie-break Python does not have*.

    The newest tie group sits at **rank 134**, which is what makes the
    byte diff still meaningful: every sweep row stays inside the first
    134 rows, where Python is deterministic. Past that Python disagrees
    with itself run to run, and a row there would be diffing against an
    unstable oracle. Three candidate `-s` rows were rejected on exactly
    that measurement — `-s 941`, `-s 1169` and `-s 1000` all have ties
    inside their first 50 — which is a case of checking the filtered
    view rather than only the unfiltered one.

    Two smaller findings. **Python's `list` query selects eleven columns
    and prints eight**: `base_revision`, `new_content_hash` and `author`
    never reach stdout. That is the fourth select-and-discard in the
    port and the widest, after `doctor history`'s two and `vcs log`'s
    one — enough instances now that "what does the command print" is a
    better guide to the subset than "what does the query select". And
    **`intent show` exits 1 on a missing id** where `source snapshot`
    and `entity explore` exit 2 for the same class of miss; Python's is
    a bare `return 1`, so the port reproduces the inconsistency rather
    than tidying it, and a sweep row pins it.

    `intent create`, `apply` and `cancel` stay unported. All three are
    writes, and `apply` carries the three-way merge `file set
    --if-match` also does not have.

    **The sweep's coverage test caught this work before the parity run
    did**, which is step 22's half-day of machinery paying for itself:
    `cargo test` failed with "these commands are in the binary and in no
    sweep invocation" the first time the new commands compiled.

    Measured: **72 invocations against Python, 0 unexpected** (up from
    60), 23 of 23 flag surfaces, 13 of 13 project-list filters, 7 of 7
    summary panes. Cross-backend **72 of 72**, and all 21 tables digest
    identically over **245,352 rows**. 374 tests across the workspace —
    23 new — 5 oracles, 42 write tests, `fmt` and `clippy` clean,
    `nix flake check` passes.

24. **Done** — `vcs diff --staged` and the bare `vcs diff <proj>`,
    2026-10-31. **Zero schema cost**: no new table, no new column, no
    new machinery, and both renderers reuse the difflib and
    unified/context output that steps 8 and 10 already built.

    Both had been listed as not ported since step 9, and this is the
    second step running whose work was mostly *finding out the stated
    reason was wrong*. Three sites said so. The *Status* table grouped
    `--staged` with "the working-set form" as one gap; they are not one
    gap, and neither is tree-dependent. The sweep's flag allow-list
    exempted `--staged` because it "needs the staging session, which is
    a write and six environment fallbacks" — and `_diff_staged`
    resolves no session and writes nothing. That exemption is the worse
    of the two, because the allow-list exists so that a *closing*
    divergence is surprising, and a wrong entry is what makes it
    invisible instead.

    Measuring also turned up that **`file` was a required positional
    here and Python's is `nargs='?'`** — a divergence the flag-surface
    stage structurally cannot see, since it parses the `options:` block
    and never compares positional arity. Second blind spot in that
    stage, alongside commands only one side has.

    **One upstream bug, reproduced and reported.** `_diff_staged`
    selects `ws.content_hash AS staged_hash` and uses it in one of its
    three branches; the `modified` branch builds the side it labels
    `(staged)` out of `file_contents.is_current` instead. All three
    live staged rows diverge between those two hashes, so every
    `--staged` diff of a modified file currently labels the database's
    bytes as staged. The sibling `_diff_working` does it right two
    screens away, which is what makes it a bug rather than a
    convention. The `regr-test --staged` sweep row pins it.

    **One tie-break added**, `vc.id DESC` on
    `_get_last_committed_content`, same reasoning as `intent list`'s:
    4 `(file_id, commit_timestamp)` groups hold more than one row, so
    the committed side was arbitrary and could differ across the two
    backends. Verified to change no current output.

    The two forms keep two renderers rather than one parameterised
    one, because three of their behaviours genuinely differ — the new
    side, the empty-content test, and what an unknown state does. See
    *The two forms that were never blocked*.

    Measured: **84 invocations against Python, 0 unexpected** (up from
    72), 23 of 23 flag surfaces with the `--staged` exemption
    **removed** rather than retained, 13 of 13 project-list filters,
    7 of 7 summary panes. Cross-backend **84 of 84**, and all 21 tables
    digest identically over **246,080 rows**. 384 tests across the
    workspace — 10 new — 5 oracles, 42 write tests, `fmt` and `clippy`
    clean, `nix flake check` passes.

25. **Done, in part** — `intent cancel`, 2026-11-03. The third write
    and the smallest: **no new table, no new column**. `edit_intents`
    arrived for `file set`'s provenance in step 17 and gained its last
    two columns to `intent show` in step 23, so this writes a column
    the port already mirrors and already reads.

    That is the point of doing it, and it is about verification rather
    than effort. `cancelled_at` had one live row behind it and was
    verified only by the cross-backend digest over the copy — the
    weakest claim available. Writing it and reading it back through
    `intent show` *and* `intent list` closes the same loop that turned
    `vcs_working_state` from the weakest-verified table in the subset
    into the strongest.

    **A compare-and-swap, where Python checks and then acts.** Python
    reads the status, decides, then issues an unconditional
    `UPDATE … WHERE id = ?`; two concurrent cancels both pass the guard
    and the second overwrites the first's `cancelled_at`. This port
    puts the guard *in* the UPDATE — `WHERE id = $1 AND status =
    'proposed'` — and reads the row afterwards only to decide which of
    Python's two refusals to report. Both exit statuses are reproduced
    (1 for a missing id, 2 for a terminal status) without reproducing
    the race. Same shape as step 20's branch-head CAS and easier to
    justify, because no trigger is racing the application here.

    **It is also the first write whose flag surface the sweep can
    see.** `file set`'s shape differs and `vcs commit-staged`'s name is
    the port's own, so neither can be compared; `intent cancel` shares
    Python's name, so `--help` resolves on both sides. Flag surfaces
    went 23 → 24, and to 25 when `file where` landed on 2026-11-05.

    Measured: 48 write tests, **6 new**, all cross-read — the write is
    read back through the two ported readers rather than through a
    query written for the test, including through `intent list`'s
    `status = 'proposed'` filter, so the write is observable in the
    *view* and not only in the columns.

**Steps 26 onward are not written yet, and the list stops naming them
on purpose.** What it does name, as of the 2026-10-31 decision, is the
*shape* of what is left, because the thing that kept going wrong was a
step's blocked-reason outliving the blockage — four times now. So:

    What is left of step 25 — everything else that is not a tree, all
    of it unblocked: ~~`search files`~~ (**done, step 30**; it cost
    `file_types` as the 23rd table and `files_with_types_view` as the
    second view, and needed the ordering divergence argued because
    Python's query has **no
    `ORDER BY` at all** under a hardcoded `LIMIT 100`, so *which* 100
    rows appear is arbitrary for any pattern matching more),
    ~~`intent create`~~ (**done, step 31**; zero schema cost, and the
    first write here whose stdout was measured against Python's rather
    than inferred from its source), ~~`apply`~~ (**done, step 33**;
    zero schema cost again, and the item's stated blocker — "needs a
    merge engine this port does not carry" — dissolved on being
    measured rather than being settled: the engine is not a three-way
    merge, so the step is a *refusal* with evidence instead of a
    feature with a dependency. That is the fifth time a queued step's
    blocked-reason has outlived the blockage, and the first where the
    reason was wrong about the thing itself rather than about which
    command would unblock it), `summary`'s agent-fleet pane once the
    subset reaches its tables, and the rest of the ~270 subcommands
    that touch no filesystem. This is most of what remains.

    With `apply` done, **`intent` and `source` are both complete and
    `file` is at 5 of Python's 10** — counting `cat`, which is an alias
    for `show` over there, as covering both. (6 of 10 since 2026-11-18;
    see step 34 — and **7 of 10 later the same day**, see step 35.)
    What is left of `file`
    sorts into two halves rather than one, and the first draft of this
    sentence got it wrong by calling them all tree work: `edit` and
    `checkout` do touch a filesystem and belong to step 27, but ~~`rm`~~
    (**done, step 34**; zero schema cost, and chosen from the three on
    the one property the other two lack — the commit's deletion branch
    had been reachable only from hand-written fixture SQL since step
    20),
    `retype` and ~~`mark`~~ (**done, step 35**; zero schema cost again,
    and the prediction below was the one in this list that held —
    it named the property that made the step worth taking and the step
    took it for that property) are **database writes** — a staged
    deletion, a `file_type_id` rewrite and an `edit_mode` change. Those
    three are unblocked today, and `retype` is the one that would move
    the mirror, since `file_types` is carried at 2 of its 5 columns.

    `mark` is the one with the shape `rm` turned out to have, and it is
    worth naming rather than leaving to be rediscovered: its column,
    `project_files.edit_mode`, is **already mirrored and already read**
    — `file ls -l` prints it and `file ls --edit-mode` filters on it —
    so it is another zero-cost write whose effect is visible through a
    byte-diffed reader. `retype` is the expensive one of the three and
    not because of the mirror: it needs `importer.scanner`'s
    `classify_path` and `is_ignored_path`, which is a classifier rather
    than a query, and it *creates* `file_types` rows for names it
    cannot resolve.

    **Step 35 kept this paragraph's promise and found one it had not
    made.** The zero-cost claim and the byte-diffed-reader claim both
    held — which is worth recording only because this list's tally runs
    the other way: five queued reasons have outlived their blockage by
    step 33's count, and this is a queued *reason to act* that was still
    true when acted on. No new tally is started here; the point is that
    the two kinds of stale note are not symmetrical, and a note saying
    "do this because X" decays more slowly than one saying "cannot do
    this because Y". What it did not predict is that `file mark` is the
    first write with **no clock**, because the one timestamp it moves
    is moved by a trigger on a column the subset does not carry. A
    paragraph written from the mirror correctly described the mirror and
    could not have seen the trigger; see *The mode that is spelled by a
    NULL*.

    The next step is still not named here, for the reason the
    paragraph above gives.

26. **Done, 2026-11-05, and it is a fence rather than a pin** — the
    tree harness, plus its first consumer. Not a command. The byte-diff
    method runs the two binaries sequentially, so a tree edited between
    the invocations is a difference neither implementation produced.

    This entry said "needs a fixture tree or a snapshot mechanism
    first" and named `file where` plus "the 15 non-database checks in
    `doctor entities`" as what it unblocks. **Two of those three claims
    were wrong**, and finding out which cost less than building the
    thing they recommended:

    **A fixture was the wrong shape.** `checkout_matches_db` does not
    read a fixed path — it loops every project and calls
    `CheckoutRepository.resolve(id, PURPOSE_EDIT)`, which picks the
    tree from the **`checkouts` table** and the ambient session. So a
    fixture tree needs fixture rows, and those rows live in the SQLite
    file this port must not write; point one side at a fixture and the
    sweep compares Python-on-live against Rust-on-fixture, which
    compares nothing. A fixture is also blind to the one check whose
    subject is POSIX mode, because the content store carries no execute
    bit. Two of resolve()'s three inputs were already solved: the
    session by step 16's pin, which the sweep has used since, and the
    table by being a read.

    **It unblocks four `doctor entities` checks, not fifteen.** The
    re-derivation above splits that bucket; only 4 of the 8 filesystem
    checks read a checkout tree, and 8 others are blocked on the
    verification backend instead.

    **What was true is the churn.** Measured: 432 non-`.git` files
    under `checkouts/` changed in 24 hours and 1,749 in seven days of
    229,389. Real, but *bursty* — a `publish` moves a few hundred at
    once and nothing moves between. So the harness detects rather than
    prevents: digest the roots, run both binaries, digest again, and
    retry up to three times. A row that churns through every attempt is
    reported as `churned` and counted in neither `pass` nor
    `unexpected`, because the tree moving says nothing about whether
    the two implementations agree. Same argument as `summary_parity`
    one substrate over. 471 ms per digest, four fenced rows, zero
    churned on the first run.

    **`file where` is ported** and is the first command in this port
    that reads a filesystem — six sweep rows byte-identical including
    the colour escapes, the `os.walk` ordering of overlay hits and all
    three exit statuses. (This said "9 variants" and the sweep had six
    rows; the count is corrected under *The first tree read*, where
    step 29 took it to eight.)

    What is still not started is the half this step was conflated with:
    pinning a tree the port would *write*. Nothing here writes one.

27. **Not started, and not without a design document** — the tree
    layer: the workspace scanner, the checkout-role resolver, the
    mirror-to-disk step. The 2026-10-24 report calls this "a different
    project, not a harder version of this one", which is accurate about
    its size and was being read as a refusal for a week. Same shape as
    *The write path* report, which is what made step 15 tractable, and
    the document comes before the code for the same reason.

    The incidents are the argument for the document. `materialize`
    deleted `.authinfo.gpg` as a stray and broke a Nix build; on
    2026-09-24 a mirror-to-disk miss made a refresh classify a staged
    file as deleted and the next commit hard-deleted its
    `project_files` row — "announced Files: 2 and committed one". A
    half-right tree layer destroys data rather than printing the wrong
    bytes, which is a different failure class from everything above.

28. **Not started** — the tree-touching commands (`materialize`,
    `templedb commit <slug> <dir>`, the `project checkout-*` family,
    `vcs commit`'s five filesystem guards), and then **cutover**: the
    rename to `templedb`, the point at which `backend/sqlite.rs` is
    deleted and the in-memory fixture question in 15b becomes live.
    Nothing before this step competes with Python for authority, and
    after it there is nothing to compete with.

29. **Done, 2026-11-15, written up and gated 2026-11-16** — the read
    half of the checkout-role resolver, which is `file where`'s two
    mirror roots coming from the `checkouts` table instead of from two
    paths built by hand. `checkouts` is the **22nd mirrored table**, at
    seven of its nine columns, and `vcs_sessions` goes from two columns
    to four.

    **The number is higher than two steps that have not started, and
    that is deliberate**: these numbers record landing order, and 27 and
    28 are the *shape* of what is left rather than the next two things
    to do. Nothing here needs 27's design document, because the
    document is for the half that writes a tree. This half is a
    `SELECT`, a `stat` and a pure function — and splitting it out is the
    same move step 26 made when it took `file where` before the tree
    layer.

    **This step was chosen by the sweep rather than from the queue, and
    it was a defect rather than a feature.** Measured 2026-11-15, `file
    where` differed from Python on **three of its four tree-reading
    rows**, in both directions at once: three `edit-work` lines that
    Python prints and this did not, one `checkout ` line Python prints
    and this did not (`woofsledger`, whose canonical tree is not at the
    canonical path), and one `checkout ` line **this port invented**
    (`nixos_anywhere_vultr_woofs`, where the hardcoded path exists and
    is not what resolution picks). An invented mirror is the worse of
    the two: the command exists to make drift visible, and it was
    reporting drift between the database and a tree no other command
    reads.

    **It is the second time this port has fallen behind a moving
    Python**, and the first one with a measurement standing behind the
    mistake. Step 26 reproduced Python's `edit-work` probe as "can never
    fire" and backed that with a count of the data the probe depended on
    — "none of the 24 project directories under that root holds a file
    at depth 1" — which was true, is still true, and never bore on the
    question, because upstream had already moved both labels onto
    `CheckoutRepository.resolve`. The heading *The one place this port
    fell behind Python* is now *The first place*. See *The resolver the
    hardcoded paths were standing in for* for the resolver, the two
    purposes deliberately not ported, and the one session variable this
    module reads where `session.rs` refuses it.

    Measured 2026-11-16, after the fix: **92 invocations against Python,
    0 unexpected**, 87 stdout-identical with 23 differing on stderr
    only, 14 deliberate; **25 of 25 flag surfaces**, 13 of 13
    project-list filters, 7 of 7 summary panes, and **6 of the 92 rows
    fenced with 0 churned out**. Cross-backend **92 of 92**, and **all
    22 tables digest identically over 274,270 rows** after a
    `pg-reschema` and a full re-mirror. 411 tests across the workspace —
    **18 new**, 15 of them the resolver's branch table and 3 the probe
    paths it feeds — 5 oracles, 48 write tests, `clippy` clean,
    `nix flake check` passes.

    **`cargo fmt --check` is not clean, and it was not this step.** It
    reports two hunks, both in `files.rs:161`–`169`, both from step
    26's `current_hash` test — over-long `assert_eq!` lines rustfmt
    wants wrapped. They are left alone here because the file is
    otherwise untouched by this step, and they are named because this
    README has claimed "fmt and clippy clean" in four step entries and
    a reader is entitled to know when it stops being true. The reason
    it survived eleven days is that **`just check` does not run
    `cargo fmt`** — see *Known gaps*.

    The "411 tests" above is two high; the figure was **409**, measured
    with `cargo test -- --list` on 2026-11-17 by subtracting step 30's
    twenty from 429. Left in place with the correction beside it rather
    than silently edited, because what it demonstrates is the entry's
    own point one line up: nothing in the gate counts tests either.

30. **Done, 2026-11-17** — `search files`, which the list above has
    named as the next read since 2026-10-31. The **23rd mirrored
    table** (`file_types`, at two of five columns) and the **second
    view** (`files_with_types_view`, at five of seventeen columns). It
    is the first table in the subset reached only through a view: no
    query in either implementation selects from `file_types` directly,
    so the boundary moved for a join rather than for a projection — and
    the DDL header had named that table, by name, as an example of
    where the boundary should *not* be dragged. A reader appeared, so
    it moved; that is the rule working rather than an exception.

    **The ordering divergence the queue predicted is real, and smaller
    than it sounded.** Python's query is `WHERE file_path LIKE ?
    LIMIT 100` with no `ORDER BY` at all, so which hundred rows it
    prints is the planner's choice — not merely an unstable order but a
    different answer. This port adds `ORDER BY id`, and the reason that
    is the right key rather than `(project_slug, file_path)` is a
    measurement: SQLite returns exactly the `ORDER BY id` sequence
    today. Over **702 comparisons** — 26 patterns unfiltered and 26
    across each of 26 project slugs, **71 of them matching more than
    the hundred** so the cut actually bites — Python's output and this
    port's agree row for row, **0 mismatches**. Any other total key
    would have reordered every row of every table for no gain.

    What makes it *incidental* on Python's side, and therefore worth
    saying out loud, is that the agreement does not depend on one plan:
    unfiltered, SQLite scans `project_files` by rowid, and under `-p` it
    drives from `projects` through `idx_project_files_project_id` — two
    different plans, both ascending by rowid, neither promised. This
    port has two backends and the sweep compares them, so a Postgres
    sequential scan returning heap order would have been free to
    disagree about *which* hundred. `id` is the only total key the view
    carries that also reproduces Python.

    **The cross-backend sweep earned its keep, on a bug the byte-diff
    structurally cannot see.** Written the faithful way — a bare
    `file_path LIKE ?` — the query found `bza/NIX_FLAKE_GUIDE.md` for
    `search files flake` on SQLite and not on Postgres, because
    SQLite's `LIKE` folds ASCII case and Postgres's does not. Python
    has one engine, so a diff against it would have passed forever. The
    fix is `entity search`'s shape, `LOWER(file_path) LIKE <folded
    pattern>`, which agrees on both: SQLite's `LOWER()` is ASCII-only
    and so is Postgres's on a `COLLATE "C"` column — measured, both
    return `École_Ä.md` for `ÉCOLE_Ä.md`. The pattern is folded with
    `to_ascii_lowercase`, **not** `to_lowercase`, and that one-word
    difference from `entity search` is itself a parity decision: there
    Python calls `.lower()` and the port reproduces its half-working
    full-Unicode fold, here Python folds nothing and SQLite's `LIKE`
    does not fold non-ASCII either, so folding further would make this
    port match where Python does not.

    **One deliberate divergence, and it is the widest instance of this
    bug class the port has found.** Python interpolates `f"%{pattern}%"`
    raw, so `_` is a wildcard: `search files get_` returns ten rows and
    **not one of them contains `get_`** — `docs/GETTING_STARTED.md`,
    `get-booking-availability/index.ts`, `getDialpadOpts.mjs`. `set_`
    gives 321 against a ground truth of 5, `api_` 128 against 4, and
    1,112 of the 2,780 live paths hold a literal underscore. Same bug
    `entity search` (858 against 256) and the `file ls` prefix had, both
    fixed upstream; this is the copy still live. Escaped here, with the
    `search files get_` row in the sweep's EXPECTED list so that
    upstream converging is a surprise rather than a silence.

    **Two upstream findings, neither fixed here.** `search files` lists
    **deleted** files: `files_with_types_view` has no `status` filter
    where `file ls` and `search content` both do, so all 373 live
    `status = 'deleted'` rows are findable and print identically to a
    present file. It is the only file-listing surface in either
    implementation that does this, and the
    `search files src/main.py -p templedb-rust` row pins it — a path
    `file ls templedb-rust` does not show. And **Python's `search`
    module is the one command family that never resolves a project
    name**, which falsified a blanket claim this README had carried near
    the top ("every Python command does"). `search files -p` is
    therefore an exact slug here too; `search content -p`, which already
    shipped resolving fuzzily, is left alone and moved to *Known gaps*
    rather than changed from a step about something else.

    **`format_table` had to be ported first**, which is why this step
    touches `templedb-cli`. `crates/templedb-cli/src/table.rs` is
    Python's `cli/core.py:348` byte for byte — the first module here
    whose subject is a Python *method* rather than a command. The parts
    that matter are the ones a plausible renderer gets wrong: the header
    labels are the raw column keys, the rule is `'-' * len(header)` and
    so spans the separators, the last column is padded like the others
    so most rows end in spaces, and `str(None)` is the literal word
    `None` — which 576 of the 2,780 live `lines_of_code` values are, so
    it is the common case and not an edge. It is deliberately **not**
    shared with `project list`, whose own form is documented as the one
    implemented command that is not byte-comparable; routing that
    through here would close a gap this README argues against closing,
    from a step that is about something else.

    Two smaller things. The command has **no `--limit`**, because
    Python's hundred is hardcoded — unlike its FTS path, which applies
    no limit at all and is why `search content` has the flag. So the
    flag surface agrees exactly and needs no allow-list entry, and when
    the hundred does cut the list this says so on **stderr**, leaving
    stdout identical; Python's `(100 results)` reads as "there are a
    hundred" for the 536 files matching `.py`. And the unfiltered form
    selects `lines_of_code` and never prints it — the **fifth**
    select-and-discard in this port, and the first where the discarded
    column is printed by the *other* form of the same command, which is
    why one query serves both here where Python runs two.

    Measured: **101 invocations against Python, 0 unexpected** (up from
    92), 95 stdout-identical with 24 differing on stderr only, 15
    deliberate; **26 of 26 flag surfaces**, 13 of 13 project-list
    filters, 7 of 7 summary panes, 6 of the 101 rows fenced with 0
    churned out. Cross-backend **101 of 101** after the `LOWER()` fix
    and 100 of 101 before it. **All 23 tables digest identically over
    277,295 rows** after a `pg-reschema` and a full re-mirror. 429
    tests across the workspace — **20 new**, 11 the query's and 9 the
    table renderer's — 5 oracles, 48 write tests, `clippy` clean,
    `nix flake check` passes. `cargo fmt --all --check` reports the
    same two `files.rs` hunks as 2026-11-16 and nothing from this step.

31. **Done, 2026-11-17** — `intent create`, the fourth write and the
    item this list has named as next since 2026-10-31. **Zero schema
    cost**: no new table, no new column, no new view. `edit_intents`
    arrived for `file set`'s provenance in step 17 and completed to
    fourteen columns for `intent show` in step 23; `content_blobs` has
    been written since the first write. Every column this command
    fills was already mirrored, already read, and already verified.

    **The method moved, and that is the part worth recording.** This
    README has said since step 15 that a write "can never be
    byte-diffed, ever" — Python writes SQLite and the port writes
    Postgres, so there is no shared input. That is true of the *effect*
    and it was being applied to the *output*, which does not follow.
    Python's `TEMPLEDB_PATH` points it at any SQLite file, so a copy of
    the live database is a sandbox it will happily write: `cp` the
    483 MB file plus its `-wal` and `-shm`, run `intent create` against
    the copy, and capture the bytes. Measured that way, the port's
    stdout is **identical to Python's, byte for byte, with only the
    intent id substituted** — 173 bytes for the plain form and 195 for
    the `-m` plus `--base-rev` form, `diff` clean both times. Those
    bytes are now pinned in `intent.rs`'s renderer tests rather than
    described in prose.

    It matters because reading the source would have got three things
    wrong, and each is invisible in a `print(f"...")`: the label
    padding is hand-counted and lands in **two** columns, not one
    (`base:`/`new:`/`note:` at column 9, `Apply:`/`Cancel:` at 10 —
    `{:<N}` over all five would align all five and differ on two
    lines); there is a bare `print()`, so line six is empty rather
    than absent; and the two hints name `templedb`, one of them for
    `intent apply`, a command this port does not have. All three are
    reproduced.

    The technique does not generalise to the whole sweep and the
    `UNSWEEPABLE` entry says so: repeating it on every run would mean a
    sweep that writes, and a copy per row. It is a one-off measurement,
    which is still strictly better than a reading.

    **What the command deliberately does not do** is the other half of
    why it was safe to land with `apply` unported. It writes
    `edit_intents` and `content_blobs` and **nothing else** — no
    `file_contents`, no `project_files`, no stage — and the test for
    that is driven with a branch *and* a live session available, the
    exact state under which `file set` stages. It also does not require
    the path to exist: Python's `apply` is where a missing
    `project_files` row becomes an exit 3, so a proposal against an
    unregistered path is legal on both sides.

    **The loop closes.** `write_parity.rs` has minted its `proposed`
    rows with a raw `INSERT` since step 25, because "`intent create` is
    not ported — the fixture has to mint the row the command
    consumes". It is ported, and `a_created_intent_is_cancellable`
    drives `create_intent` into `cancel_intent` with no SQL in
    between. `Fixture::propose` stays, for `Fixture::stage`'s reason:
    it can force a terminal status, which no ported command can.

    Three divergences, and **none of them is new** — all three are
    inherited from decisions already argued for `file set`, which is
    what a precondition looks like from the far side:

    - **The slug is exact where Python's is fuzzy.** `file set`'s
      divergence, plus one reason that is forced rather than chosen:
      the id is resolved inside the write transaction, because the
      reader and the writer point at *different databases*.
    - **The session is the resolved pin, not `int()` of the
      environment.** Python's `_current_session_id` parses and
      swallows and checks nothing; this port has one resolver and it
      requires `ended_at IS NULL`. The copy made the three cases
      measurable and they do not behave alike upstream — see
      *A third thing the copy found* below, because one of them is a
      bug and not a divergence.
    - **`created_at` comes from the clock** where Python lets the
      column default. The same value by construction, and the reason
      step 13 came before any write.

    What is *not* a divergence, and was checked rather than assumed:
    the `TEMPLEDB_SESSION` warning `vcs log` prints is deliberately
    absent here, because that variable changes nothing on either side —
    Python reads `TEMPLEDB_SESSION_ID` and stops. A warning there would
    be this port apologising for an answer that agrees.

    **A third thing the copy found, and this one is an upstream bug.**
    A stale `TEMPLEDB_SESSION_ID` naming no row makes Python's
    `intent create` fail with **`Error: FOREIGN KEY constraint
    failed`** and exit 1 — a raw constraint message with no mention of
    a session, from a variable a script exports. Measured on the copy,
    2026-11-17, and the three cases do not behave alike:

    | `TEMPLEDB_SESSION_ID` | Python | this port |
    |---|---|---|
    | `notanumber` | `int()` raises, is swallowed, intent created with a NULL session, **exit 0** | exit 1, names the variable |
    | `999999999` (no such row) | **`FOREIGN KEY constraint failed`**, exit 1, nothing written | exit 1, names the variable and where to get a live id |
    | `1295` (a session that has *ended*) | intent created, **exit 0**, with the dead id in the column | exit 1, "references no live session" |

    Three behaviours for one class of mistake, and the middle one
    leaks a database error. The statuses happen to agree on that row —
    1 and 1 — for entirely different reasons, which is the sort of
    accidental agreement step 12's work exists to stop being mistaken
    for a contract. Reported, not fixed here: Python is a different
    project.

    It also retroactively justifies a schema decision. The subset
    declares **no FK** on `edit_intents.session_id` even though the
    live schema does, on the recorded grounds that "the live constraint
    is a latent bug rather than a guarantee" — `prune_sessions` guards
    only `vcs_working_state` and would hit this FK on a batch delete.
    Dropping it means nothing in the port's Postgres would refuse a
    dangling id, so the liveness check in `session.rs` is the only
    thing keeping one out. That was true of `file set` already and
    nobody had said it: the FK's absence is safe *because* the
    resolver is strict, and the two decisions have to move together.

    **One upstream inconsistency, reproduced and reported.**
    `intent create` **accepts empty content** and `file set` refuses
    it. Measured on the copy: `printf '' | templedb intent create`
    created intent #1627, recorded `patch_summary: 1 lines, 0 bytes`
    and exited 0, where `file set` with nothing on stdin refuses
    outright on the grounds that it is "almost always a pipe that
    produced nothing". The two code paths never converged — `file set`
    tests `if not content` and `intent create` tests
    `if content is None`, so `-c ''` and an empty pipe both count as
    content over there. Reproduced rather than harmonised, and in
    *Known gaps*.

    **And the line-count arithmetic is Python's second one.**
    `_short_summary` is `decode().count('\n') + 1` with **no empty
    case**, where `file set`'s line count has an explicit `0 if not
    content`. So an empty proposal is `1 lines, 0 bytes` and the same
    content through `file set` is 0 lines. One character apart in the
    source, and a port that reused the other function would look right
    and be wrong on one input. Measured, not read.

    **A stale comment found and corrected, and it is this README's own
    claim that was wrong.** Step 23 says of `edit_intents`' two late
    columns that "All three sites are corrected" — *Known gaps*,
    `mirror.rs`'s table comment and the Postgres DDL's header. Two of
    three were. The DDL's header paragraph still said "Twelve of
    fourteen" and still named `intent cancel` and `vcs commit` as the
    unblocker for two columns the table below it has carried since
    2026-10-30, with the correction sitting *in the same file* beside
    the columns themselves. Eighteen days, and nothing in the gate
    compares a projection comment against the column list under it.
    Corrected, with the error kept rather than deleted.

    The `status` column's comment is corrected too, and by this step
    rather than by drift: it said "the writer only ever inserts
    `'applied'`", which was true of one writer. All three values are
    written here now — `'applied'` by `file set`'s intent, `'proposed'`
    by this, `'cancelled'` by `intent cancel`.

    **One statement extracted rather than copied.** The
    `content_blobs` insert is now `fileset::store_blob`, called by both
    writers. Its own comment is the argument: the two arms are written
    as one statement *so that* a change to a shared column cannot reach
    one and miss the other, and two copies in two modules is a worse
    version of the same hazard than two arms in one. The `file set`
    tests pass unchanged through it, including the NUL round-trip, and
    `proposed_content_with_a_nul_round_trips` is the assertion that
    sharing it did not cost the new path `pgtext`'s escape.

    Measured: **101 invocations against Python, 0 unexpected**
    (unchanged — a write adds no sweep row), 95 stdout-identical with
    24 differing on stderr only, 15 deliberate; **27 of 27 flag
    surfaces** (up from 26, and with **no allow-list entry**: all four
    of Python's flags are ported and none was invented), 13 of 13
    project-list filters, 7 of 7 summary panes, 6 of the 101 rows
    fenced with 0 churned out. Cross-backend **101 of 101**, and **all
    23 tables digest identically over 279,009 rows**. **59 write
    tests, 11 new**, every one of them cross-read — the proposal is
    read back through `show_intent` and `list_intents` rather than
    through a query written for the test. `clippy` clean.
    `cargo fmt --all --check` reports the same two `files.rs` hunks as
    2026-11-16 and nothing from this step; this step's four changed
    `.rs` files were formatted before it landed, with `rustfmt` on the
    files rather than `cargo fmt --all`, so the two pre-existing hunks
    are still there for whoever next edits `files.rs`.

    **The test count is 502 and step 30's "429" does not reconcile.**
    Measured here with the command that entry names — `cargo test
    --workspace -- --list | grep -c ': test'` — which reports 502 on
    this tree. The delta this step contributes is **20**: 9 unit tests
    in `intent.rs` for the renderer and the summary arithmetic, and 11
    write tests. Counting `#[test]` attributes across the DB's copy of
    every `.rs` file — a method a reader can re-run without a build —
    gives 483 before this step against 503 after, which agrees with the
    delta and not with 429. Step 30's figure is therefore low by about
    fifty and this does not re-derive how; what it does is name the
    method beside the number, because the entry two above this one is
    itself a correction of a test count and the lesson evidently did
    not take. **Nothing in the gate counts tests**, which is the
    standing reason any figure here decays — the same gap as the
    `cargo fmt` one.

    **That "503" is one high, and the reason is worth more than the
    one.** Found by step 32 hours later, which had to pick a counting
    rule a test could apply: `grep -c '#\[test\]'` counts a line of
    `sweep_coverage.rs`'s own module doc, the one that says "the sweep
    should become a `#[test]`, and it cannot". The attribute count is
    502, not 503, and it then agrees with `cargo test -- --list`
    exactly rather than approximately — per target as well as in total.
    So the sentence above is right that the two methods disagree with
    429 and wrong that they agree with each other. A file about testing
    talks about tests, and a count of a thing is not a count of
    mentions of it.

32. **Done, 2026-11-17** — the gate's own bookkeeping. Not a command,
    no schema cost, no query touched, and no divergence from Python
    either way: the subject is this repository's checks and this
    document's numbers. It closes the two *Known gaps* entries that
    were about the gate rather than about Python, and it is the step
    that entry asked for in those words — "the *gate* is the actual
    gap … which is the argument for fixing the gate rather than the
    hunks".

    **Two checks that existed and were not wired in.** `just check` and
    `just check-all` now start with `cargo fmt --all --check` and
    `cargo clippy --all-targets -D warnings`, cheapest first, and
    `nix flake check`'s `checkPhase` runs the first of the two. Both
    were things this README claimed — four step entries say "fmt and
    clippy clean" — and nothing ran either, which is how two
    `assert_eq!` hunks in `files.rs` survived twelve days and three
    steps that each declined to reformat a file they had not otherwise
    changed. That rule was right every time it was applied; what it
    could not do was end. The hunks are formatted here because here
    they fail a check.

    `clippy` is in `just check` and **deliberately not** in the flake.
    `fmt` reads the source and needs no build; `cargo clippy
    --all-targets` recompiles the workspace and its dependencies under
    a second driver in a sandbox that shares no `target/` with
    anything. Measured in the dev shell at **1.5 s incremental and
    clean**, which is why it belongs in the gate at all, and the
    sandbox figure is the one that would not be incremental.

    **And a test whose subject is this README.**
    `crates/templedb-cli/tests/gate_counts.rs` — 7 tests, hermetic, no
    database and no interpreter — counts `#[test]` attributes per
    target and compares them against **ten numbers** parsed out of the
    prose above *Porting order*: the test total, each of its four
    parts, the two `#[ignore]`d groups and their split, and a second
    statement of the total 1,800 lines further down that had been
    **296 against an actual 438** since some earlier revision. It also
    checks the breakdown sums to its own total, which is prose against
    prose and is the half that catches a total edited without its
    parts — step 31's mistake.

    It reads the prose rather than a machine-readable block, for
    `sweep_coverage.rs`'s reason: parse the authoritative artifact
    instead of keeping a second copy, and here the artifact is the
    sentence a reader believes. The cost is that the anchors are
    fragments of English, so rewording one fails a test — deliberately,
    with a message saying to re-aim it. One convention falls out and is
    now house style: **a gate-checked number is written as a numeral**,
    because the parser looks for digits. That is why the *Status*
    paragraph no longer says "Five more are `#[ignore]`d".

    Its region stops at `## Porting order` **on purpose**. Everything
    below that heading is a measurement taken on a date; step 22's "351
    unit tests" is not a claim about this tree, and a gate that forced
    those numbers forward would destroy the record this README keeps
    deliberately — every entry above that left an error in place with
    the correction beside it.

    **Sabotage-tested four ways**, because a check that cannot fail on
    demand is not evidence of anything: a total edited by one (caught
    by two tests, one of them as a prose-internal contradiction), a real
    `#[test]` added to `value.rs` (caught, naming all three numbers it
    moved), an anchor reworded (caught as "occurs 0 times", with the
    instruction rather than a wrong number), and a new integration
    target nothing claims (caught by the direction an exemption list
    cannot check by being consulted). All four messages name the cause;
    none of them points at the README when the tree is what moved.

    **The finding is about counting, and it is one the step had to
    settle before it could assert anything.** The two methods this
    README has used disagreed: `cargo test --workspace -- --list |
    grep -c ': test'` reports **502** and `grep -c '#\[test\]'`
    reports **503**, and the extra one is a doc comment in
    `sweep_coverage.rs` *discussing* `#[test]`. Step 31 had reported
    the second as agreeing with its delta hours earlier. The rule the
    test uses — the attribute alone on its line — reproduces `--list`
    exactly, 502 for 502, and **per target as well as in total**: 381,
    51, 6, 59, 3, 2. A unit test pins the rule against a sample whose
    third line is the mention that caused the problem.

    **What stays uncounted is named in *Known gaps* rather than left to
    be inferred from a passing test**: the sweep's invocation count,
    because three rows are discovered at run time; every count
    describing the live database; and what a test covers rather than
    whether it exists. Where the gate cannot count, this step's rule is
    to stop claiming — the two `justfile` recipe descriptions that said
    "all 54 invocations" while the sweep ran 101 now give no number,
    and the sweep still prints the one it ran.

    **And the gate's own report turned out to carry one of these.**
    This run read **17 deliberate** where step 31 read 15, with nothing
    changed in between, and the number had to be decomposed rather than
    copied: **9 of the 17 are flag-surface pairs** — every entry of
    `FLAGS_EXPECTED`, which moves only when a flag does — and the other
    **8 are invocation rows that differed on this run**, three of them
    the `intent list --all-statuses` rows whose difference depends on
    where the newest `created_at` tie group sits in the live database.
    Step 31's own commit put it at rank 4: six `file set` intents
    written inside four seconds, #1629 and #1630 both stamped
    `2026-11-17 21:38:01`. So the port's write path manufactured the
    tie that moved the sweep's headline count forty minutes later, and
    *The tie-break Python does not have* had already argued why the
    rows are exempted by name instead of by argument choice. The *Checks*
    section now tracks the 9 and not the 17 — a sum of a static list and
    a live measurement is not a figure anyone can watch for drift.

    Measured: **101 invocations against Python, 0 unexpected** (nothing
    here touches a command), 93 stdout-identical with 24 differing on
    stderr only, 17 deliberate as decomposed above; 27 of 27 flag
    surfaces, 13 of 13 project-list filters, 7 of 7 summary panes, 6 of
    the 101 rows fenced with 0 churned out. **509 tests across the
    workspace, 7 new**, of which 445 run by default and 64 are
    `#[ignore]`d. `cargo fmt --all --check` is **clean for the first
    time since 2026-11-05**, and is now checked rather than claimed;
    `clippy` clean and likewise checked; `nix flake check` passes.

33. **Done, 2026-11-18** — `intent apply`, the fifth write and the
    last of the intent family. **Zero schema cost**: no new table, no
    new column, no new view, no new identity sequence — the third step
    running at zero, and the one with the strongest claim to it.
    `edit_intents` arrived in step 17 and completed in 23,
    `file_contents` has been written by `file set` since step 15,
    `content_blobs` since the first write, and `project_files` is
    read-only here. Every column this command touches was already
    mirrored, already read, already written and already verified.

    It is the item the list above has named as next since 2026-10-31,
    and the item three *Known gaps* entries have been deferring on one
    sentence: it "needs a merge engine this port does not carry". **That
    sentence was never measured, and measuring it is this step's
    finding.** The engine exists upstream, it is called
    `merge_resolver.ThreeWayMerge`, its docstring says "diff3
    algorithm", and it is not a three-way merge. It walks base, ours
    and theirs with three indices it advances in lockstep — one line
    each, every iteration — so it is correct only while all three
    versions have the same line count, and its loop never visits
    anything either side added past the end of base. It also computes
    two `difflib.unified_diff`s and reads neither: the sixth
    select-and-discard in this README and the first in code rather than
    SQL.

    Measured against `git merge-file -p --diff3` over 11
    `(base, ours, theirs)` triples: **3 agree.** 4 report a clean merge
    and produce different text, 3 conflict where a real diff3 merges,
    and 2 report clean where a real diff3 conflicts. The failures are
    not conservative — 7 triples report clean, 5 of those 7 produce
    text a three-way merge does not yield,
    `rebase_onto_current`'s `merged == current_text` guard catches 1 of
    the 5, and **4 reach `file_contents`**.

    **And it was reproduced end to end, in the shipping command.** On a
    copy of the live database: `templedb intent apply 1645` printed
    `✓ Intent #1645 applied`, exited **0**, and left the file holding
    `ONE/two/NEW/three` where base, ours and theirs had all held a
    fourth line. Nobody deleted it. See
    *The rebase this port refuses to do*, whose heading changed from
    *does not* with this step.

    So the divergence is decided rather than deferred: **a moved base
    exits 4, always**, which is what Python's `--no-rebase` selects.
    That is `file set --if-match`'s divergence one command over, and
    the step did not make it — it gave it evidence. Reproducing the
    engine would mean writing content neither the author nor the
    database ever held, under a success message, into the one table
    whose rows are the user's files.

    **The non-stale path is byte-identical**, measured by step 31's
    copy technique rather than read off the source: Python's two lines
    for intent #1639, `cat -A`'d, then produced by this port against
    the write scratch for the same content and `diff`'d clean with only
    the id substituted — and the hash matched without substitution,
    because both sides sha256 the same bytes. Three things in those two
    lines a renderer from the source gets wrong: the path carries **no
    project prefix** where `intent create`'s second line does, the
    arrow is U+2192 with one space each side, and the hash is cut to
    twelve.

    **A second upstream bug, independent of the merge, and it is the
    one that justifies the port's structure.** `intent apply` runs four
    statements through a helper that commits each one, and the third is
    `DELETE FROM file_contents WHERE file_id = ? AND is_current = 1`.
    Measured on the copy: an intent whose `new_content_hash` has no
    blob makes the fourth statement's scalar subquery yield NULL, it
    fails `NOT NULL constraint failed: file_contents.file_size_bytes`,
    exit 1 — and `templedb file cat` then answers *"Could not read file
    content from database"* for a file that was intact a moment
    earlier. The delete committed and the insert did not. This port's
    apply is one transaction across both tables, so the same input is a
    refusal that changes nothing. Pinned by
    `a_proposal_whose_blob_is_gone_leaves_the_file_intact`.

    **Two columns Python leaves behind, reproduced rather than
    improved.** Its `INSERT` names four columns, so `line_count` is
    left NULL and `version` falls back to `DEFAULT 1` — the row is
    deleted and re-inserted rather than updated, so a file on version 7
    is on version 1 after an apply. Both differ from `file set`, which
    writes the line count and leaves the version alone. Verified on the
    copy (`file_contents` row 49986) and pinned by a test whose helper
    exists *because* `Fixture::stats` unwraps the line count and cannot
    represent a NULL — asserting Python's behaviour through a reader
    that cannot hold it is how the behaviour gets quietly fixed.

    **`--no-rebase` is accepted**, which took a decision rather than
    being free: what it selects is this port's behaviour either way.
    It is not the `--no-fts` case — that flag would promise matching
    the port cannot deliver, this one promises a refusal the port does
    deliver — but it *is* inconsistent with `file set`, where the same
    flag is absent and allow-listed. Both arguments are in
    *The rebase this port refuses to do*; one of the two commands is
    wrong and reconciling them is not this step. Flag surfaces go
    **28 of 28** with no allow-list entry.

    It also opened the **fourth blind spot** in the sweep's flag stage,
    and this command is the first concrete instance rather than a
    hypothetical: the stage compares flag *names*, so these two
    surfaces agree perfectly while `--no-rebase` means different things
    on the two sides. Stated in `parity-sweep.sh` beside the other
    three.

    **The loop closes a third time.** `create_intent` →
    `apply_intent` → `read_file`, with no SQL between them, which is
    the move step 21 made for the stage and step 31 for the cancel.
    Only two of the twelve new write tests write SQL at all, and both
    are for states no ported command can produce: a blob that has been
    reaped, and a `project_files` row with no content beside it.

    Measured: **101 invocations against Python, 0 unexpected**
    (unchanged — a write adds no sweep row), 93 stdout-identical with
    24 differing on stderr only, 17 deliberate; **28 of 28 flag
    surfaces** (up from 27, with no allow-list entry), 13 of 13
    project-list filters, 7 of 7 summary panes, 6 of the 101 rows
    fenced with 0 churned out. Cross-backend **101 of 101**, and **all
    23 tables digest identically over 281,416 rows** after a
    `mirror-refresh`. **529 tests across the workspace, 20 new** — 8
    unit tests for the renderer and the staleness branch table, 12
    write tests — of which 453 run by default and 76 are `#[ignore]`d.
    **71 write tests, 12 new**, every one of them cross-read: the
    applied content is read back through `read_file`, `show_intent`
    and `list_intents` rather than through a query written for the
    test. `fmt` and `clippy` clean and both checked rather than
    claimed; `nix flake check` passes.

    Still open from Python's intent family: **nothing**. `create`,
    `list`, `show`, `apply` and `cancel` are all here, which makes
    `intent` the second command family complete at 5 of 5 after
    `source`'s 2 of 2.

34. **Done, 2026-11-18** — `file rm`, the sixth write. **Zero schema
    cost**: no new table, no new column, no new view, no new identity
    sequence — the fourth step running at zero. `vcs_working_state`
    arrived in step 19 for a writer it did not yet have, got that
    writer in step 21 and a reader in step 20; this writes four of the
    nine columns already mirrored there, and every one of them has been
    written and read back by this port for three weeks.

    It is the first of the three database writes the list below has
    named as unblocked since 2026-10-31, and it was chosen over
    `retype` and `mark` on a property neither of those has: **it gives
    an existing reader its missing writer.** `vcs commit-staged` has
    handled `state = 'deleted'` since step 20 — it is the one path in
    the system that hard-deletes a `project_files` row — and nothing in
    this port could produce such a row. Every test of that branch had
    to mint it with `Fixture::stage`, whose own doc comment says why:
    *"still SQL, and still needed, for the states no writer
    produces: a `deleted` row comes from a workspace scan."* That
    sentence is now two-thirds true. The two corruptions the commit's
    guards exist for still need SQL; the deletion does not, and
    `the_loop_closes_when_file_rm_stages_its_own_commit` drives
    `file set` → `file rm` → `vcs commit-staged` → `file cat` with no
    SQL between any two of them.

    **The stdout was measured, and then reproduced end to end.** Step
    31's copy technique first: `cp` the 494 MB SQLite file plus its
    `-wal` and `-shm`, point `TEMPLEDB_PATH` at the copy, run
    `templedb file rm templedb-rust .gitignore`, `cat -A` the bytes. It
    is one line, 35 bytes — `✓ Staged .gitignore for deletion`. Then
    the same path through the port's own binary against the write
    scratch, captured and `diff`'d against Python's file: **clean, with
    nothing substituted**, which `intent create` and `intent apply`
    could not claim because their lines carry an id. The bytes are
    pinned in `filerm.rs`'s renderer tests.

    What reading the source would have got wrong is one thing and it is
    the kind only a sibling comparison exposes: the path carries **no
    project prefix**. `intent create`'s first line has one, `file set`'s
    has none, `intent apply`'s has none, and there is no rule — each is
    whatever that function interpolates. So a caller who typed a fuzzy
    project name cannot tell from `file rm`'s stdout which project was
    staged.

    **One deliberate divergence, and it is `file set --stage`'s.** No
    session is a refusal here, where Python creates one. Measured on
    the copy: with neither variable set, Python printed its success
    line, exited 0 and left `vcs_sessions` **one row longer** — id
    1331, minted to own a staged deletion. Unlike `file set` there is
    no "write the content and stage nothing" half to fall back on,
    because this command's entire effect *is* the stage, so there is
    one outcome and no flag to make it conditional. The refusal carries
    the `TEMPLEDB_SESSION` note that `intent create`'s deliberately
    omits, and that is a measurement rather than a copy: Python's
    `file rm` resolves through `get_current_session`, whose step 3 reads
    the *name* variable and creates when none matches —
    `TEMPLEDB_SESSION=zz-no-such-name templedb file rm …` exited 0 and
    minted session 1333. Ignoring the name changes the answer here,
    which is the condition the warning exists for.

    **No flags, and that took a decision.** Python's `file rm` has none
    beyond `-h`, and the obvious thing to give this port's stricter
    version was a `--session` — `file set` has one, and here the
    environment variable is the *only* way to supply the session the
    command now requires. Not invented: it would have bought
    convenience and a permanent `FLAGS_EXPECTED` entry, and the
    surfaces agree exactly instead. Flag surfaces go **29 of 29 with no
    allow-list entry**, which is `file where`'s missing `--no-color`
    one command over.

    **Python's session resolver here is the strict one**, which
    falsifies a frame step 31 set up. `intent create`'s
    `_current_session_id` parses with a swallowed `int()` and never
    checks `ended_at`, so it answered three ways for one class of
    mistake and the port diverged on all three. `file rm` goes through
    `get_current_session`, which raises on both — measured on the copy,
    `notanumber` gives `TEMPLEDB_SESSION_ID must be an integer, got
    'notanumber'` and an ended session's id gives `references no live
    session`, both exit 1, both **agreeing with this port's resolver
    word for word on the first message**. So upstream has two session
    resolvers with different strictness and the lax one is the odd
    case, not the rule. Worth recording because the README's account of
    that divergence was a sentence away from reading as "Python is lax
    about sessions".

    **Three things reproduced that a tidier port would have changed**,
    all three verified on the copy rather than argued:

    - **`content_hash` survives the upsert.** Python's
      `DO UPDATE SET` names three columns and not this one, so a file
      staged `modified` by an earlier `file set` keeps that write's
      hash under a `deleted` state — the row for `.gitignore` held
      `c20683…` before and after. It reads like an oversight and it is
      load-bearing: the commit records that hash in
      `vcs_file_states.content_hash`, so **the history of a committed
      deletion names the bytes that were deleted** instead of the
      `DELETED` sentinel. Both branches are pinned —
      `a_staged_deletion_moves_three_columns_and_leaves_two_alone` and
      `an_unstaged_file_deletes_through_the_sentinel` — and the pairing
      is the finding: which of the two a deletion gets depends on
      whether the caller staged the file first, not on anything this
      command does.
    - **`intent_id` survives too**, and that follows from the above
      rather than standing on its own. Migration 088's link names the
      intent whose hash the row holds; the hash does not move, so the
      link stays correct. Clearing it would break the invariant step 21
      went and fixed.
    - **No `edit_intents` row at all.** Every `file set` records
      provenance and a deletion records none, on either side — 1,647
      intents before Python's `file rm` and 1,647 after. Reproduced:
      inventing a row would put a `status` value in a column nothing
      else in either implementation writes.

    **A third copy of the active-branch lookup, and it is Python's
    weakest.** `vcs commit`'s joins `vcs_branches`; the auto-stage's
    does not; `file rm`'s does not either, and its `is_default = 1`
    fallback is the only place it joins. So an `active_branch_id`
    naming no branch row stages against a branch that does not exist
    and `vcs commit`, which joins, then finds nothing staged. This port
    calls the one shared [`active_or_default_branch`], so it joins
    *and* refuses a pointer into another project's branch — two
    divergences in the stricter direction, both latent: measured
    2026-11-18, **no live project has an `active_branch_id` naming a
    missing or foreign branch**.

    **One message this step had to fix rather than add.** The
    write-target session refusal ended with "or unset it to record the
    intent with no session", which was true of both callers it had —
    `file set` and `intent create` both proceed without one — and is
    wrong for `file rm`, which refuses. Made generic rather than
    specialised per caller, for `resolve_project_exact`'s stated
    reason: a refusal that explains itself in another command's terms
    is worse than one that explains less. It is the smallest instance
    of the thing this list keeps finding — a true sentence that a later
    caller makes false — and the only reason it was caught is that the
    new caller was driven at the command line rather than only through
    its tests.

    **The heading above this document's status table said "four opt-in
    writes" and there were five.** It has said four since step 31 and
    `intent apply` did not revisit it. Corrected to six here, and named
    because it is the *fifth* count in this README to decay — the four
    that `gate_counts.rs` now catches were all test counts, and this
    one is outside the region that test reads, in the heading of the
    section it reads. Nothing counts commands.

    Measured: **101 invocations against Python, 0 unexpected**
    (unchanged — a write adds no sweep row), 93 stdout-identical with
    24 differing on stderr only, 17 deliberate; **29 of 29 flag
    surfaces** (up from 28, with no allow-list entry), 13 of 13
    project-list filters, 7 of 7 summary panes, 6 of the 101 rows
    fenced with 0 churned out. Cross-backend **101 of 101**, and **all
    23 tables digest identically over 282,874 rows** after a
    `mirror-refresh`. **541 tests across the workspace, 12 new** — 2
    unit tests for the renderer, 10 write tests — of which 455 run by
    default and 86 are `#[ignore]`d. **81 write tests, 10 new**, every
    one of them cross-read: the staged deletion is read back through
    `read_file`, `file ls`'s stats and the ported commit rather than
    through a query written for the test. `fmt` and `clippy` clean and
    both checked rather than claimed; `nix flake check` passes.

    Still open from Python's `file`: `retype` and `mark`, both database
    writes and both unblocked, plus `edit` and `checkout`, which are
    step 27's. `file` goes from 5 of 10 to **6 of 10**.

35. **Done, 2026-11-18** — `file mark`, the seventh write and the
    smallest in the port: **one column on one row**. **Zero schema
    cost** — no new table, no new column, no new view, no new identity
    sequence — the fifth step running at zero.

    It is the item step 34's list nominates, and it was named there on
    the property that turned out to matter: `project_files.edit_mode`
    is **already mirrored and already read**, by `file ls -l`, which
    prints it, and `file ls --edit-mode`, which filters on it. That
    makes it the only write here whose cross-read runs through output
    the parity sweep *byte-diffs against Python*. Every other write in
    this port is verified by a reader the port wrote for it — the
    commit for `vcs_working_state`, `intent show` for `edit_intents` —
    and both of those readers landed within days of the writer they
    verify. This one reads back through a function that has been
    byte-identical against Python since before the write path existed.

    **All four of its stdout lines are byte-identical to Python's**, and
    the measurement went further than `file rm`'s. Step 31's copy
    technique first — `cp` the 498 MB SQLite file plus `-wal` and
    `-shm`, point `TEMPLEDB_PATH` at the copy, run the four transitions
    and `cat -A` each. Then a project and a path of the same names in
    the write scratch, the port's own release binary driven over the
    same four, and each stdout `diff`'d against Python's captured file:
    **four of four clean, with nothing substituted.** `file rm` could
    claim that for one line; `intent create` and `intent apply` could
    not claim it at all, because their lines carry an id.

    The four, with their byte counts:

    ```text
    ✓ templedb-rust/.gitignore: edit_mode immutable → hot-reload      65
    ✓ templedb-rust/.gitignore: edit_mode hot-reload → hot-reload     66
    ✓ templedb-rust/.gitignore: edit_mode hot-reload → live-symlink   68
    ✓ templedb-rust/.gitignore: edit_mode live-symlink → immutable    67
    ```

    **No deliberate divergence in its own behaviour**, which is the
    first time that sentence has been available here. `file set` is
    stricter on five counts, `file rm` refuses a session Python creates,
    `intent apply` refuses a merge, `intent cancel` and
    `vcs commit-staged` are compare-and-swaps where Python checks and
    acts. This one takes the exact slug every write here takes and
    otherwise does what Python does. Its one implementation choice —
    one statement rather than two — changes no output and is argued
    below.

    **The first write with no clock**, and that absence is the step's
    main finding rather than a convenience. Step 13 built the clock seam
    as a precondition for *any* write, and six writes have taken one
    since. Python's statement here names one column; the `updated_at` it
    also moves is moved by **a trigger** —
    `update_project_files_updated_at`, one of the three on
    `project_files` that are not cr-sqlite's. Measured on the copy: the
    row for `.gitignore` held `2026-10-16 15:27:09` before and
    `2026-11-19 03:33:59` after.

    This port has none of the live database's 61 triggers and
    `project_files` is mirrored at seven columns of which `updated_at`
    is not one, so the bump is **not reproduced and cannot be without a
    schema change.** It is the step-20 situation a second time — a
    column written by a mechanism the port does not carry — and it came
    out the opposite way, because the two cases differ on the thing that
    decides it: the branch head has readers in both implementations and
    a frozen one makes `vcs commit` find nothing, whereas
    `project_files.updated_at` has **no reader in either
    implementation**. The only two `SELECT`s naming it are in
    `services/component_service.py`, which nothing imports and which
    also selects two columns the table does not have.

    **And it is not new.** `file set`'s un-delete arm is
    `UPDATE project_files SET status = 'active'`, which has fired the
    same trigger since the first write in step 15 and went a month
    unrecorded. So the choice was not "reproduce it or not" but
    "reproduce it *here*, and leave the older writer wrong" — which is
    worse than two writers diverging identically and in writing, by the
    rule step 30 used for `search content -p` and step 33 for
    `file set --no-rebase`. It is in *Known gaps* with both writers
    named and the two-part fix spelled out. The honest summary is that
    this is the most latent divergence on that list and nothing in the
    gate can catch the entry going stale, which is the prose-count
    problem one layer down.

    The other two triggers do not fire: `file_contents_fts_file_update`
    is `AFTER UPDATE OF file_path, status`, so this is the first
    `project_files` write in the port that touches no full-text index —
    which is what made it the clean case to ask the column question on.

    **One upstream bug, measured and reported.** Python's success line
    interpolates `args.project` — *the string the caller typed* — and
    resolves the row with `fuzzy_match_project`. Measured on the copy:
    `templedb file mark templedb-RUST .gitignore --edit-mode hot-reload`
    printed `✓ templedb-RUST/.gitignore: …`, exited 0 and wrote the
    `templedb-rust` row. There is no `templedb-RUST`. The same
    interpolation is in the command's `File not found:`. This port is
    byte-identical anyway and by construction rather than luck —
    `resolve_project_exact` accepts only the slug, so the typed string
    and the written row's slug are one string, and the gap is confined
    to inputs the port refuses outright.

    It is the **mirror image of step 34's finding**, which is the pair
    worth keeping: `file rm`'s line carries no project prefix and a
    caller cannot tell which project was staged; `file mark`'s carries
    one and it can name a project that does not exist. Neither is a
    rule. Forty lines apart in one file, two interpolations, two
    different answers to the same question — which is the second time
    reading the siblings rather than the source was what caught it.

    **One statement where Python has two**, and a measurement rather
    than a principle. Python reads the row, decides, then issues an
    unconditional `UPDATE`; this does the lookup, the read of the
    replaced value and the write in one `UPDATE … FROM` with a
    self-join, so `RETURNING` can hand back the pre-command value of
    the column the statement is overwriting. The missing-file refusal
    then falls out of `RETURNING` returning no row.

    Explicitly **not** a compare-and-swap, unlike steps 20 and 25:
    `mark` has no precondition on the old value, so there is nothing to
    refuse and the window closed is a *reporting* one. What it buys is
    measured by `a_racing_mark_reports_the_value_it_actually_replaced`
    — a transaction that reads the row, has it changed under it, then
    marks it reports the value it overwrote, where Python's shape
    reports the one its own `SELECT` returned, a transition that never
    happened. That test also names what it does **not** reach, because
    the mechanism is easy to claim: a commit landing while the statement
    is blocked on the row lock goes through Postgres's `EvalPlanQual`
    recheck, and producing it needs two threads rather than two
    transactions in sequence. Nothing here claims anything about that
    interleaving. The first draft of this entry attributed the measured
    result to the recheck and was wrong — the per-statement snapshot of
    READ COMMITTED is sufficient to explain it.

    **Four things reproduced that a tidier port would have changed**,
    all four verified on the copy:

    - **An unconditional write, including the no-op.** Marking a
      `hot-reload` file `hot-reload` still issues the `UPDATE`, prints
      `hot-reload → hot-reload`, exits 0 and fires the trigger.
    - **A soft-deleted row is markable**, and the write is then
      invisible to every reader either implementation has. Measured:
      `templedb-rust/src/main.py` took `live-symlink` at exit 0, and
      `file ls -l --edit-mode live-symlink` listed nothing, because
      `file ls` filters on `status`. The only success among the seven
      writes with no cross-read available, and the test asserts it *as*
      that rather than skipping the case.
    - **`immutable` is stored as a NULL**, so `file ls --edit-mode
      immutable` does not match a row this command marked `immutable`.
      Both halves are Python's, in two commands, and each reads like a
      bug from the other's side. Pinned as a pair.
    - **An empty-string `edit_mode` prints as `immutable`.** Python's
      renderer is `row['edit_mode'] or 'immutable'`, which is falsiness
      and not a null check. Caught by writing the test before trusting
      the obvious `unwrap_or`, which would have been right on every
      input this port can produce and wrong on one the table can hold.

    **No flags invented, and the surface agrees exactly.** One flag on
    each side — `--edit-mode`, required on both, three choices on both —
    and no `FLAGS_EXPECTED` entry. The tempting fourth value is
    `non-immutable`, which `file ls --edit-mode` takes and this must
    not: upstream's two `choices=` lists genuinely differ, so the port
    carries two enums rather than one. That divergence would have been
    invisible to the sweep in any case — its flag stage compares
    *names*, so a port-only flag **value** is a fifth blind spot, and it
    is noted in `parity-sweep.sh` beside the other four. Flag surfaces
    go **30 of 30**.

    All four of Python's exit statuses reproduced and measured on both
    sides: missing file 1, unresolved project 1, bad `--edit-mode` 2,
    absent `--edit-mode` 2. The third is worth a note — Python's body
    carries its own `valid = {...}` check returning 1, and it is
    **dead code**, because argparse's `choices=` rejects the value
    first. Measured: the bad value never reaches `mark()` at all, and
    upstream's dispatcher reports it as `❌ Unknown command: 'bogus'`.
    The status agrees at 2; the message is the port's own, like every
    other stderr line here.

    Measured: **101 invocations against Python, 0 unexpected**
    (unchanged — a write adds no sweep row), 93 stdout-identical with
    24 differing on stderr only, 17 deliberate; **30 of 30 flag
    surfaces** (up from 29, with no allow-list entry), 13 of 13
    project-list filters, 7 of 7 summary panes, 6 of the 101 rows fenced
    with 0 churned out. Cross-backend **101 of 101**, and **all 23
    tables digest identically over 284,097 rows** after a
    `mirror-refresh`. **555 tests across the workspace, 14 new** — 5
    unit tests for the renderer and the mode type, 9 write tests — of
    which 460 run by default and 95 are `#[ignore]`d. **90 write tests,
    9 new**, and every one of the nine cross-reads through `file ls`
    rather than through a query written for the test. `fmt` and
    `clippy` clean and both checked rather than claimed;
    `nix flake check` passes.

    Still open from Python's `file`: **`retype`**, plus `edit` and
    `checkout`, which are step 27's. `file` goes from 6 of 10 to **7 of
    10**, and `retype` is now the only database write left in the
    family — the expensive one, and not because of the mirror: it needs
    `importer.scanner`'s `classify_path` and `is_ignored_path`, which
    is a classifier rather than a query, and it *creates* `file_types`
    rows for names it cannot resolve. It is also the one that would
    move the mirror, since `file_types` is carried at 2 of its 5
    columns.

36. **Done in part, 2026-11-19** — `file retype`, the item step 35
    nominates, at its **read half**: the classifier, the query, the move
    grouping and the renderer. `--apply` is not ported and is argued
    below rather than deferred silently. **Zero schema cost** — the
    sixth step running at zero — and the one step here whose *cost* was
    a dependency rather than a table.

    **The step entry step 35 wrote was wrong about which half is
    expensive, and right that something was.** It called `retype` "the
    expensive one … not because of the mirror: it needs
    `importer.scanner`'s `classify_path` and `is_ignored_path`" and
    predicted the mirror would move for the `file_types` rows the
    command creates. Both halves of that hold. What it did not see is
    that the two sit on **opposite sides of the dry/apply line**: the
    classifier is all of the read and none of the write, and the
    `file_types` insert is all of the write and none of the read. So the
    expensive half and the schema-moving half are separable, and
    separating them is what let the classifier land verified instead of
    landing behind a re-mirror. Sixth time a queued reason has been
    re-derived on contact; the first where the reason was *true* and
    pointed at the wrong seam.

    **The dry form is a read, and that is the whole reason this half
    was worth taking first.** Python's `retype` is dry by default and
    `--apply` writes, so the default form has a **diffable stdout** —
    the only thing in the `file` family with that property that is not
    already ported. Every write in this port had to be verified by
    cross-read or by step 31's copy technique, because a write has no
    stdout to diff. This one's entire answer is stdout, over 2,413 live
    rows, so it goes in the sweep and gets byte-diffed like a read.
    Nine rows, **0 unexpected**, exit statuses included.

    **`classify.rs` is the first module here whose subject is a Python
    module *constant*.** `table.rs` was the first whose subject was a
    Python method; this is a 179-entry ordered list of
    `(regex, type_name, test_func)` triples plus a six-entry skip list,
    transcribed as a list and compiled, so the table diffs against
    `scanner.py`'s line by line.

    That decision cost the port's **first dependency that was not
    already in `Cargo.lock`** — `regex`, plus three transitive crates —
    and it is argued in the workspace manifest rather than slipped in.
    The alternative was a hand-written matcher over the dozen shapes
    the patterns actually use, rejected on verification grounds: it
    turns all 179 entries into judgment calls, and `(^|/)Dockerfile\.`
    being a path-*segment* prefix rather than a basename prefix is the
    kind of mistake that looks like nothing in a review.

    **The oracle is the deliverable as much as the module is, and it
    earned that in the first run.** `classify_parity.rs` imports
    `importer.scanner` — it does not restate it, because a transcribed
    oracle only proves two transcriptions agree — and runs both
    functions over a generated corpus of **24,757 paths**: 229 literal
    basenames the table names, under twelve directory prefixes, with
    eight suffix decorations and an Emacs-lock form. **0 mismatches on
    `classify_path` and 0 on
    `is_ignored_path`**, and the two table lengths compared against
    CPython's `len()` rather than against a number written here.

    It is the **third** CPython oracle and the first that needs more
    than the standard library, so it is in `just oracle` and
    deliberately *not* in `nix flake check` — the sandbox has no
    `templedb` package. The `PYTHONPATH` is read out of the `templedb`
    launcher on `PATH` rather than guessed, which is what makes the
    oracle diff against the same `scanner.py` the sweep's `templedb`
    runs.

    **Three things the unit tests asserted from a reading of the source
    and got wrong**, all three caught by the oracle on its first run,
    all three now asserting the measured answer:

    - **`readme` and `ReadMe` classify as `None`, not `readme`.** The
      guard is `p.upper() == 'README'`, which reads as "any casing" and
      *is* case-insensitive — and it is only consulted for a path the
      pattern `README$` already matched, which is case-**sensitive**.
      So the guard's tolerance is unreachable. A port that read the
      guard would be wrong on two of three inputs; one that read only
      the pattern would be right by accident.
    - **The table has 179 entries, not 240.** Asserted from a glance at
      the list. The oracle compares against `len()`.
    - **The skip list has six patterns, not five**, because its last
      source line holds two of them.

    The lesson is an ordering one and it is the same one step 31 drew
    about `intent create`'s renderer: the unit tests in `classify.rs`
    are regression guards on an answer the oracle *establishes*, not
    the establishment of it. Written the other way round they are 240
    restatements of a misreading.

    **One upstream bug, measured and reported: `pm2_config` is dead
    code.** Found by the oracle's own coverage check, which asserts the
    corpus reaches every `type_name` the table can produce and reported
    exactly one it could not. `ecosystem\.config\.js$` → `pm2_config`
    is entry 135; `\.js$` → `javascript` is entry 29, carries no guard,
    and matches every string the later pattern can match, since a path
    ending `ecosystem.config.js` ends in `.js` by construction. So no
    path can ever receive that type.

    Measured on the live database: `file_types` carries `pm2_config` as
    id 131 in category `infrastructure`, **0 of the 2,786
    `project_files` rows hold it**, and there is exactly one
    `ecosystem.config.js` — which this command correctly proposes to
    move to `javascript`. Reported, not fixed: the repair is a reorder
    of `FILE_TYPE_PATTERNS`, which changes what every command that
    classifies anything answers, and that is upstream's call.

    A second entry is shadowed identically and is **harmless**, which
    is the pair worth keeping rather than the single finding:
    `templedb-deploy\.sh$` (138) is covered by `deploy\.sh$` (136),
    which is unanchored at the start and yields the same
    `deployment_script`. One dead rule changes an answer and one does
    not, and the difference is whether the shadowing entry carries the
    same name. The exclusion list in the oracle therefore holds one
    name and not two, and it is asserted in **both** directions — a
    name on it that becomes reachable fails the test, so upstream
    reordering the table arrives as a surprise rather than a silence.

    **A second upstream finding, and it is a stale docstring with a
    live symptom.** Python's `retype` docstring explains the bug the
    command exists to repair: `_get_file_type_id` used to fall back to
    `SELECT id FROM file_types LIMIT 1`, and "that row is `api_route`,
    which is how 495 rows across 10 projects … came to be recorded as
    API routes". Measured 2026-11-19: the first row of `file_types` is
    now **`ansible_inventory` (id 126)**, not `api_route` (id 18) —
    migration 138 reseeded the table and moved it. The docstring names
    the wrong culprit, and the live evidence is that 44 of the 88
    changes this command proposes are `ansible_inventory -> …`. The
    *mechanism* it describes is exactly right; only the row it names
    has moved. Reported.

    **One deliberate divergence, and it is `search files`'s shape with
    teeth.** Python's query has **no `ORDER BY`**, and here the order
    decides the answer rather than only its presentation:
    `Counter.most_common(25)` breaks ties by first-encounter order,
    which is row order, and the list is cut at 25 kinds.

    Measured on live data: the unfiltered form finds **26 kinds of
    change with 11 of them at a count of 1**, and the 25th and 26th
    both have count 1 — so the cut falls **inside a tie group**, and
    which kind prints versus which becomes `… and 1 more kinds of
    change` is whatever the planner returned first. This port adds
    `ORDER BY pf.id`.

    `pf.id` because it is the key that reproduces Python, measured
    across all three forms — unfiltered (2,413 rows), `-p
    templedb-rust` (54) and `--from-type ansible_inventory` (44) —
    where SQLite returns exactly that sequence today. So the clause
    changes no current output. The decisive argument is the one step 30
    made: this port has two backends and the sweep compares them, and
    **110 of 110 cross-backend rows are identical** with the clause.
    Without it, SQLite's rowid scan and a Postgres heap-order scan
    would both be correct and free to disagree about *which rows
    appear*.

    **`--apply` is absent from the parser rather than accepted and
    ignored**, and that took choosing between two precedents this
    README already carries rather than following one. `search content
    --no-fts` is listed absent because it would promise matching the
    port cannot do; `intent apply --no-rebase` is accepted because it
    promises a refusal the port delivers on every input. `--apply` is
    neither shape: it switches **on** the only behaviour that writes,
    so accepting it would mean `file retype --apply` exiting 0 having
    written nothing — the silent divergence this project treats as its
    one real defect. Refused at the parser, exit 2, one
    `FLAGS_EXPECTED` entry.

    The blocker is the **create** arm and not the update, which is
    worth being precise about because "the write is not ported" sounds
    like effort and is a schema boundary. `file_types.category` is
    `TEXT NOT NULL` with no default; this port mirrors `file_types` at
    **two of five columns**; Python inserts `(type_name,
    'uncategorized', 'Auto-registered by file retype')`. So the faithful
    insert needs the subset to grow two columns, a `pg-reschema` and a
    full re-mirror, and an unfaithful one would write a row only this
    port could read back. In *Known gaps* with the two-part fix.

    **Four things reproduced that a tidier port would have changed**,
    two of them unreachable on live data and unit-tested for that
    reason:

    - **A NULL `current_type` prints as the literal word `None`.** The
      join is a `LEFT JOIN` and the comparison is `want ==
      r['current_type']`, so a NULL is never equal to a target and the
      row is *always* a change; the label is an f-string, so
      `str(None)` reaches stdout. The fourth `str(None)` in this port
      after `format_table`'s three. Measured: 0 of 2,413 active rows.
    - **The debris block counts every ignored row and lists ten.**
      Measured: 0 of 2,413 active paths match `SKIP_FILE_PATTERNS`, so
      the block never prints on live data. Its `… and N more` is
      `len - 10`, a *different* expression from the move list's
      `len - 25`, and the two are off by one in opposite directions if
      copied from each other.
    - **Debris is excluded from the numerator and counted in the
      denominator.** So a project of pure debris lists its debris and
      then says `Nothing to re-type (N active file(s) checked)`.
    - **`unresolved` counts paths considered, not paths changed.** A
      path the classifier cannot name that is *already* `text_file`
      increments the count and produces no move, so the trailing line
      can report paths that appear nowhere above it. 9 against 88
      changes on live data.

    **`--from-type` is a condition on a `LEFT JOIN`ed column**, which
    means it silently drops every row whose `file_type_id` is NULL or
    dangling — the rows most likely to need re-typing. Python's, and
    reproduced rather than moved into the `ON` clause.

    **The project is fuzzy here**, unlike every write in this port.
    That is not a relaxation of the exact-slug rule but the rule's
    scope: the rule exists because a mis-resolved *write* overwrites a
    file, and nothing here writes.

    Measured: **110 invocations against Python, 0 unexpected** (up from
    101 — 9 new rows, and the first new *sweep* rows since step 30,
    because the five steps in between were all writes), 102
    stdout-identical with 25 differing on stderr only, 18 deliberate;
    **31 of 31 flag surfaces** (up from 30, and the first new command
    since step 31 to arrive *with* an allow-list entry), 13 of 13
    project-list filters, 7 of 7 summary panes, 6 of the 110 rows
    fenced with 0 churned out. Cross-backend **110 of 110**, and **all
    23 tables digest identically over 285,687 rows** after a
    `mirror-refresh`. **578 tests across the workspace, 23
    new** — 12 unit tests in `classify.rs`, 9 in `retype.rs` and 2
    oracle tests — of which 481 run by default and 97 are
    `#[ignore]`d. 3 oracles, 90 write tests unchanged, `fmt` and
    `clippy` clean.

    One gate note, and it is the gate doing its job rather than a
    problem: adding an integration target made
    `no_test_target_is_unaccounted_for` fail by name — "these test
    targets hold tests and no README claim counts them:
    `classify_parity`" — which is precisely what step 32 built it to
    say. Three README numbers moved with it and the gate named all
    three.

    Still open from Python's `file`: **`retype --apply`**, plus `edit`
    and `checkout`, which are step 27's. `file` stays at **7 of 10**
    rather than going to 8, deliberately: a command with its write half
    missing is not a ported command, and counting it would make the
    tally the kind of claim this file keeps catching itself making.

### The classifier, and the write that needs two more columns

`file retype` is the first command in this port whose *answer* is
mostly not a query. The SQL is four lines and a join; everything
interesting is a 179-entry regex table walked in order, and the
decisions worth recording are about how to be sure a transcription of
that table is right.

**The short version of what happened.** The module was written from
`scanner.py`, its unit tests were written from the same reading, all
twelve passed, and then the oracle found three of the assertions wrong
on its first run. None of the three was a typo in a pattern — the
patterns were transcribed correctly. All three were *descriptions* of
what the patterns do, which is the failure mode a reading cannot catch
and a differential test catches immediately.

The sharpest is `README`. Its entry is
`(r'README$', 'readme', lambda p: p.upper() == 'README')`, and read
top to bottom that says "a file called README in any casing". It does
not, because the guard runs only for paths the pattern matched and the
pattern is case-sensitive:

| path | CPython | what reading the guard suggests |
|---|---|---|
| `README` | `readme` | `readme` |
| `readme` | **`None`** | `readme` |
| `ReadMe` | **`None`** | `readme` |
| `docs/README` | `None` | `readme` |

Three of four wrong. The fourth is right for a different reason worth
naming on its own: the pattern is unanchored at the start, so
`docs/README` *does* match it, and then fails the guard because the
guard compares the whole relative path rather than the basename. The
`continue` falls through to `README\.md`, which needs the extension,
and nothing below matches an extensionless name — so an ordinary
`docs/README` classifies as nothing at all.

That `continue` is the other thing a reimplementation gets wrong.
Python's loop body is `if matched: if guard and not guard(p): continue;
return name`, so a failed guard falls through to the *rest of the
table* rather than returning `None`. Three of the four guards produce
nothing without it: `\.sql$` + `'migration' in p.lower()` falls through
to a bare `\.sql$` → `sql_file`, `\.ts$` + `'supabase/functions' not in
p` falls through to `supabase/functions/.*\.(ts|js)$` →
`edge_function`, and `settings\.json$` + `'.vscode' in p` falls through
eventually to `\.json$` → `config_json`. Each pair is written as a
guard plus a fallthrough where two ordered patterns would have done,
and the fallthrough is load-bearing.

**Why the corpus is generated and not sampled.** The live database has
2,786 paths and they are overwhelmingly `.py`, `.ts`, `.md` and
`.nix`: measured, they reach **74 of the 179 entries and 66 of the 164
distinct type names**. A corpus sampled from the database would leave
98 names untested — including every one of the four guards' negative
branches. The generated cross product reaches **163 of the 164**, and
the 164th is `pm2_config`, which is unreachable by construction.

That last number is the part that makes the corpus checkable rather
than merely large. A corpus can only be trusted if something fails when
it shrinks, so the oracle asks CPython for the table's own name set and
asserts the corpus covers it. Delete an atom and a name goes uncovered
and the test says which. It is `sweep_coverage.rs`'s argument one layer
down: coverage means a probe exists, and the only way to keep that true
is to have a test whose subject is the coverage.

**Three regex-dialect differences, two inert and one recorded.**
Python's `re` and the `regex` crate agree across this table's whole
feature set except for `$`: Python's matches at end of string *or*
immediately before a trailing newline, Rust's only at end of haystack.
So `a.py\n` is `python` upstream and `None` here, and `a.orig\n` is
ignored upstream and not here. Measured: **0 of 2,786 live
`file_path` values contain a newline**, and a path that did would be
unopenable on the filesystem the column describes. It is asserted by a
test rather than argued away, because a reader who assumes the two
dialects agree on `$` will be wrong about exactly this and nothing else
would tell them. Alternation semantics are inert because this module
only ever asks *whether* a pattern matched, never what it matched, and
`\d` is Unicode-aware on both sides and left that way.

**Why the write half is a schema question.** `--apply` is two
statements: resolve-or-create a `file_types` row per target name, then
one `UPDATE project_files SET file_type_id = ?, updated_at =
datetime('now') WHERE id = ?` per change. The update is ordinary. The
create is not, and the reason is the subset boundary rather than the
SQL:

```sql
CREATE TABLE file_types (
    id INTEGER PRIMARY KEY,
    type_name TEXT NOT NULL UNIQUE,
    category TEXT NOT NULL,           -- no default
    description TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
)
```

This port mirrors two of those five columns, `id` and `type_name`,
because the boundary follows the readers and `search files` reaches the
table only through `files_with_types_view`'s join. Python inserts
`(type_name, 'uncategorized', 'Auto-registered by file retype')`. So
there are two options and neither is free: grow the subset by two
columns, which needs a `pg-reschema` and a full re-mirror, or insert
without `category`, which the live schema forbids and the port's own
DDL would have to be weakened to allow — producing a `file_types` row
that only this port could read back. The first is correct and is one
step's work; the second is the kind of invention this project does not
do.

Worth noticing while the column is in view: Python's `retype` is the
**one** `project_files` writer whose own statement names `updated_at`.
`file mark`'s and `file set`'s do not — the trigger moves it behind
them, which is the *Known gaps* entry step 35 opened. So when the
subset does grow for this command, it grows for a writer that can set
the column from the clock rather than one that needs a trigger
reproduced, and the two halves of that gap close in the same change.
It is the first time the `updated_at` entry has had a reason to move
that is not just tidiness.

### The mode that is spelled by a NULL

`file mark` is the smallest write in this port and the one that asked
the sharpest question, and the question is not about `edit_mode`. It is:
**what does a port owe a column that a trigger writes and nothing
reads?**

The column is `project_files.updated_at`. Python's `file mark` does not
name it; `update_project_files_updated_at` does, on every row update,
and it is one of the three triggers on that table that are not
cr-sqlite's. This port has no triggers at all — that was a boundary the
write-path design document named on purpose — so a faithful port of the
*code* leaves the column frozen, and a faithful port of the *behaviour*
has to write it in the statement.

Step 20 met this shape before and went the other way. The branch head is
advanced by a trigger upstream; the port does the compare-and-swap in
the application, and the entry argues that a port without the trigger is
the only implementation that can do that CAS *correctly*, since Python's
own comment records the trigger beating its code to the column. The
difference between the two cases is not the mechanism, which is
identical. It is the reader:

- `vcs_branches.head_commit_id` is read by `vcs log`, `vcs show` and
  `vcs commit` on both sides. A frozen head makes the ported commit find
  nothing, which is a bug with a visible symptom.
- `project_files.updated_at` is read by **nothing**. Not by any command
  in this port, not by any command upstream — the two `SELECT`s that
  name it live in a module nothing imports, and that module also selects
  two columns the table does not have, which is how you can tell how
  long it has been dead. Not by the state digest either, which digests
  the mirrored projection, and the column is outside it.

So the two decisions come apart on evidence rather than on taste, and
the rule they share is the one the subset has always followed: the
boundary follows the readers. Carrying `updated_at` for this write would
make it the fourth column in the subset with a writer and no reader, and
step 23 is the record of what that costs — `edit_intents` sat
writer-only for two weeks and three separate comments went stale
explaining why, each of them wrong about which command would close it.

The part that decided it, though, is that **this write is not the first
one to fire that trigger.** `file set`'s un-delete arm has been an
`UPDATE project_files` since step 15. Fixing the bump for `file mark`
and not for `file set` would mean two writers in one port diverging from
upstream in two different ways, one of them documented and one of them
not — and the undocumented one would be the older, more-used command.
That is strictly worse than both diverging the same way with the
divergence written down, which is what *Known gaps* now records.

What makes the entry fragile is worth stating in the same breath:
nothing in the gate can catch it. The sweep diffs stdout and no command
prints the column. The cross-backend sweep compares two copies of the
port against each other and both omit it. The state digest reads the
projection the column is not in. `gate_counts.rs` reads prose. So this
divergence is held by one paragraph in a 6,900-line file, which is
exactly the class of claim this README has watched decay seven times —
and saying so is the only guard available.

The second thing the command turned up is smaller and is about method
rather than about a column. Python's success line names the project the
caller typed and resolves a different one; `file rm`'s names no project
at all. Both were found by comparing the two commands to *each other*
rather than by reading either one, which is now the second step running
where sibling comparison caught what source reading would have got
wrong. The generalisation is not "check the siblings" — it is that
`print(f"...")` has no contract, so every renderer in the `file` family
is its own measurement, and a port that factors them together is
asserting an agreement upstream never made.

### The deletion that deletes nothing

`file rm` is a write whose entire effect is one word in one column, and
the two things worth recording about it are both about what *else*
would have been a reasonable implementation.

**It could have soft-deleted the row, and that would have been
unrecoverable.** `project_files.status` exists, `file ls` filters on it,
and flipping it to `'deleted'` is what "remove this file" sounds like.
It is also exactly wrong, and the reason is a guard three steps back:
`file set`'s un-delete revives a soft-deleted row on the next write to
that path. So a soft delete here would be undone by the next
`file set`, silently, while the staged deletion that `vcs commit-staged`
reads would never have been written. The caller would see the file come
back and nothing would say why. Python stages, and the help text says
what it is doing — "hard-delete on commit; history preserved" — which
is the one sentence in that parser block that is load-bearing.

**It could have cleared `content_hash`, and that would have cost
history.** The column is stale by every ordinary reading: the row says
`deleted` and carries the hash of content that is about to stop being
current. But `commit_staged` reads that column and writes it to
`vcs_file_states.content_hash`, with `'DELETED'` as the fallback — so
leaving it is what makes a committed deletion record *which bytes were
removed*, and clearing it would replace that with a sentinel. Python
leaves it by omission; this port leaves it on purpose and says so, which
is the difference between reproducing a behaviour and reproducing a
line of SQL.

The asymmetry that falls out is worth stating because no comment in
either implementation does: **whether a committed deletion names the
deleted bytes or the sentinel depends on whether the file was staged
before it was removed.** `file set -s` then `file rm` names the hash;
`file rm` alone names `DELETED`. Both are correct, both are Python's,
and a reader of `vcs show` has no way to tell which happened.

### Counting what the gate claims

This is the second check in this repository whose subject is a check,
and the first whose subject is this document. `sweep_coverage.rs` asks
whether the sweep still covers the binary; `gate_counts.rs` asks
whether the README still describes the tree. Both run in `cargo test`
with nothing but the source, and both exist because the thing they
check had no way to fail.

**The decay is measured, not suspected.** Four consecutive test counts
in this README were wrong: 393 when the figure was 409, step 29's 411
when it was 409, step 30's 429 — correct the day it was written and 438
a day later — and step 31's 502 sitting beside a breakdown that still
summed to 429. Add the `cargo fmt` hunks, the Postgres DDL's "Twelve of
fourteen" eighteen days after the columns landed, "two write commands"
when there were four, "unit tests (296)" when there were 438, and
"all 54 invocations" in the justfile when the sweep ran 101. Nine
instances — eight of them numbers and one a pair of unformatted hunks —
and every one of them a statement nothing in this repository could
contradict. Not one was written carelessly, which is the pattern worth
acting on: these are not mistakes of attention.

**What a claim is.** Ten of them, each an English fragment that sits
immediately beside a number:

| the claim | counts |
|---|---|
| ``N tests run by `cargo test` `` | every `#[test]` less every `#[ignore]`d one |
| ``N in `templedb-db`'s library`` | that crate's `src/` tree |
| ``N in `templedb-cli`'s,`` | that crate's `src/` tree |
| ``N in `tests/sweep_coverage.rs` `` | one integration target |
| ``N in `tests/gate_counts.rs` `` | itself |
| ``N more are `#[ignore]`d because they need `python3` `` | the three oracle targets' ignored tests |
| ``N drive CPython's `difflib` ``, ``… `sqlite3` `` and ``… `importer.scanner` `` | each oracle target |
| ``N more are `#[ignore]`d because they need a Postgres`` | `write_parity.rs` |
| `unit tests (N)` | the total again, from the *Checks* table |

The anchors are matched against README.md with every run of whitespace
collapsed to one space, which is what lets a claim be a sentence rather
than a line: this file is hard-wrapped at about 70 columns, so
``51 in `templedb-cli` `` has a newline in the middle of it and a
literal search finds nothing. Each anchor must occur **exactly once**,
and that assertion is a check in its own right — two occurrences would
make the number verified depend on which came first in the file.

**Why the prose and not a declaration.** A block of counts in an HTML
comment would be trivially parseable and would check the wrong thing.
Nobody reads the comment; the sentence beside it is what a reader
believes, so the sentence is what has to be checked. It is the argument
`sweep_coverage.rs` makes about the sweep's bash arrays one layer down
— parse the authoritative artifact, because a test carrying its own
copy of the expected values passes while the artifact says something
else. The price is that rewording a sentence fails a test. That is the
right price: the failure says to re-aim the anchor, where the
alternative is a claim that quietly stops being checked. One convention
falls out of it and is now house style — **a gate-checked number is a
numeral**, because the parser looks for digits.

**Why it stops at `## Porting order`.** Everything below that heading
is a measurement taken on a date, and this README's method is to leave
a superseded number in place with the correction beside it rather than
to edit it away. Step 22's "351 unit tests" is not a claim about this
tree; forcing it forward would destroy the record. So the region ends
at the heading and the checked claims are the present-tense ones. It is
the one design decision here that a reader might otherwise take for an
oversight, since the step entries are where most of the numbers are.

**The counting rule, and the one it replaces.** A test is a line whose
trimmed text is exactly `#[test]`. That reproduces
`cargo test --workspace -- --list | grep -c ': test'` **exactly** —
502 on 2026-11-17 before this file existed, and per target as well as
in total: 381 + 51 + 6 + 59 + 3 + 2. The obvious shorthand does not.
`grep -c '#\[test\]'` reports 503, and the extra one is
`sweep_coverage.rs`'s own module doc saying *"the sweep should become a
`#[test]`, and it cannot"* — a file about testing talks about tests.
Step 31 had reported 503 as agreeing with its delta hours before this
step measured it, so the rule is pinned by a unit test whose sample
includes the mention that caused the problem, alongside a commented-out
attribute and one inside a string.

**Where each check lives, and why the line is build cost rather than
purity.**

| | `nix flake check` | `just check` |
|---|---|---|
| `cargo fmt --all --check` | yes | yes |
| `gate_counts.rs`, `sweep_coverage.rs` | yes (in `cargo test`) | yes |
| `cargo clippy --all-targets` | **no** | yes |

`fmt` reads the source and needs no build at all, so it is free in both
and goes first in both. The two self-checks are ordinary `#[test]`s and
arrive in the flake for nothing. `clippy` recompiles the workspace and
its dependencies under a second driver — 1.5 s incremental in the dev
shell, a cold build in a sandbox sharing no `target/` — so putting it
in the cheap half would make the cheap half the slow one for coverage
`just check` already has. Stated here because an absence from a gate is
exactly the thing this section is about: it is a decision, and this is
where it is recorded.

**What this does not count**, in *Known gaps* too and repeated here
because a passing test is a bad place to learn what it did not check:
the sweep's invocation count, because three of its rows are discovered
at run time from the live database and nothing in this tree determines
them; every count that describes a database rather than this source —
mirrored tables, digested rows, the 1,346 live `created_at` values —
because a hermetic test has no 483 MB SQLite file and no business
asserting about one; and every claim about what a test *covers*, which
is the same boundary `sweep_coverage.rs` draws when it says a row
existing is not a row being good.

### `intent create`, and the write whose stdout could be measured

The command itself is small — one `INSERT … RETURNING`, one
content-addressed blob, seven or eight lines of stdout — and the
interesting part is the verification, so that is what this section is
about. `crates/templedb-db/src/intent.rs` carries the rest.

**The claim that was too strong.** This README has said since the
write-path design document that "a write cannot be byte-diffed against
Python, ever", and `UNSWEEPABLE` repeats it per command: "the sweep is
a diff of stdout over a shared input and the two implementations write
different databases, so there is no shared input". Every word of that
is about the *effect* of a write — which row landed, in which database.
It had also been read as covering the *output*, and the output is a
different question: `intent create` prints eight lines that depend on
the content, the flags and one generated id, and nothing about them
needs the two implementations to share a database.

**What made it measurable** is that Python resolves its database from
`TEMPLEDB_PATH` — `config.py:13`, one `os.environ` lookup with no
guard — so pointing it at a copy is a supported operation rather than a
trick. The live file is 483 MB; `cp` of it plus `-wal` and `-shm` took
under a minute, and `templedb intent list` against the copy returned
the same two outstanding intents as against the original, which is the
check that the copy is a database and not a torn page. Then **twelve
`intent create` invocations** against the copy: the plain form, `-m`
with `--base-rev`, `--from-file`, an empty pipe, a binary pipe, `-c`
and `-f` together, a missing project, an unreadable `--from-file`, and
four `TEMPLEDB_SESSION_ID` values — live, non-numeric, naming no row,
and naming an ended session. Plus one `intent cancel` and an
`intent show` of every row written, which is how the stored columns
were read back.

Nine of the twelve produced stdout or a status this port now
reproduces exactly. The other three are the refusals whose *messages*
differ, which is the gap this README already records — error wording is
the port's own and goes to stderr, and the status is the contract step
12 made. All three agree on the status: a missing project, an
unreadable `--from-file` and undecodable stdin are exit 1 on both
sides.

The binary pipe is the one where reproducing the bytes was never
available. Python's `sys.stdin.read()` raises `UnicodeDecodeError` and
its CLI prints `Error: 'utf-8' codec can't decode byte 0x84 in position
0` before exiting 1; this port says that stdin is not valid UTF-8 and
that `file set` is the command that takes bytes. The three session
values are the ones where the *behaviour* differs and not only the
wording — see the table in step 31.

**`intent create` is text-only on both sides, and that is a fact rather
than a limitation here.** `file set` grew a `Content::Binary` arm in
step 22 because Python's `file set` has one — it reads stdin as bytes
and stores a `binary_asset` when they do not decode, which is what let
`.authinfo.gpg` into the database at all. `intent create` has no such
arm: `--content` is argv, `--from-file` is `read_text`, and the stdin
path decodes. So the port takes a `&str` where `set_file` takes a
`Content`, and refuses the bytes. Inventing a binary branch would store
a blob Python cannot produce, under a hash an unported `apply` is
supposed to read back.

**It also settled a claim one step back, for free.** `intent cancel`'s
success line was written from Python's source in step 25 and never
compared against Python's bytes, because there was no way to get them.
Run against the copy, `templedb intent cancel 1625` prints
`✗ Intent #1625 cancelled` — `E2 9C 97`, U+2717 BALLOT X, one space
either side of the id — which is byte for byte what `cancelled_line`
produces. So that step was right, and it is now measured rather than
inferred. The same copy would answer the same question for any future
write whose output is a function of its arguments.

**Why the copy is not now part of the sweep.** It would mean a sweep
that writes, a 483 MB copy per run, and an oracle whose output includes
an id that depends on how many times the sweep has run. The
`UNSWEEPABLE` entry states the trade in those terms rather than
repeating the old absolute: the stdout *was* measured, once, and the
bytes are pinned in a unit test where they cost nothing to keep.

### `search files`, and the order Python does not have

`search files` matches on the path and reads no content, which makes it
the cheapest read in the port and the one with the most decisions per
line of SQL. The query is six lines; four of them are a divergence.

The whole of it:

```sql
SELECT id, project_slug, file_path, type_name, lines_of_code
  FROM files_with_types_view
 WHERE LOWER(file_path) LIKE ?2 ESCAPE '\'
   AND (CAST(?1 AS TEXT) IS NULL OR project_slug = ?1)
 ORDER BY id
 LIMIT 101
```

Against Python's two, which differ only in their projection and their
filter:

```sql
SELECT file_path, project_slug, type_name, lines_of_code
  FROM files_with_types_view
 WHERE file_path LIKE ?
 LIMIT 100
```

`LIMIT 101` rather than 100 is how the truncation is detected: one row
past the cap is read and discarded, instead of a second `COUNT(*)` over
the same predicate. The extra row is never rendered.

`ORDER BY id` is argued in the step entry above and in `search.rs`. The
short version is that it is not a tidying choice — without a total key
the two backends are free to disagree about which hundred rows survive
`LIMIT`, and `id` is the key that reproduces Python's incidental order
at the same time.

The `(CAST(?1 AS TEXT) IS NULL OR …)` form collapses Python's two
queries into one, which it can do because the two differ only in what
they print. That is also where the fifth select-and-discard lives: the
unfiltered form never shows `lines_of_code`, and the filtered one does,
so a single query selecting both and two projections over it is strictly
less code than two queries — the first time in this port where "what
the command prints" and "what the query selects" come apart *within one
command*.

What the view brings with it is not a choice at all, and both halves are
reproduced rather than relaxed:

  * **`file_types` is INNER joined**, so a file whose type is
    unresolved is invisible. 0 of 2,780 live rows dangle, so the branch
    is unreachable on live data and the fixture is the only place it can
    be tested — beta's files carry a NULL `file_type_id` and are absent
    from `search files` as a result. Python's own comment on the
    *`search content`* FTS query records the opposite call for that
    query, where an inner join on this table silently dropped indexed
    files; the two queries genuinely want different joins, and this one
    is the view's.
  * **There is no `status` filter.** Covered above; the finding is that
    `search files` is the only file-listing surface in either
    implementation that reports a deleted file as present.

And one thing the port declined to add: no FK from
`project_files.file_type_id` to `file_types`, now that the table is in
the subset and one could be declared. The view's inner join means a
dangling id makes a file *invisible* rather than invalid, and that is a
behaviour the two backends have to agree on; a FK would instead make
`mirror` fail on data this port does not control. Same call the
`project_category` CHECK gets, for the same reason — the mirror's job is
to carry what is there.

### The first tree read

`file where` is the first command here whose answer is not a function
of the database. It hashes the checkout copy, the edit-workspace copy
and any `/nix/store` overlay copy of a path, and compares each against
the hash `file_contents` holds. It writes nothing and resolves nothing,
so it competes with the Python implementation for authority over
nothing — which is why it lands before the tree *layer* of step 27 and
not after it.

**Eight sweep rows**, byte-identical including the ANSI escapes and all
three exit statuses: drift in the checkout alone, an overlay hit that
drifts alongside it, an overlay hit that drifts while the checkout
matches, everything agreeing at exit 0, the two resolution rows step 29
added, and the two exit-2 rows. Six of the eight read a tree and are
fenced; the pair that does not is the exit-2 pair.

That count said *nine* from 2026-11-05 and *eleven* for one day after
step 29, and both were wrong in the same harmless way: the enumeration
beside them has always listed the actual rows, and the number beside it
was never recounted against `parity-sweep.sh`, which says eight in its
own comment. Corrected rather than dropped, because a count nobody
checks is the shape of defect this section is otherwise about.

The exit-2 pair is **deliberately
unfenced**, and that is why they are separate rows: Python returns
before the probe loop when the database has no current row, so neither
reads a tree at all. A change that made them read one would show up as
a flapping row; a fenced row would hide it.

**Two upstream defects are reproduced rather than fixed.** There were
three; the third — the `edit-work` probe that could never fire — turned
out to be a defect upstream had already repaired, and the entry that
described it is now *The resolver the hardcoded paths were standing in
for* below. The short version, because this section is where a reader
will look for it: both mirror paths were built by hand here and both
were wrong by 2026-11-15, and three of this command's four tree-reading
sweep rows were failing because of it.

**The docstring's depth cap does not exist.** It says the overlay walk
is capped "to avoid runaway walks (spacemacs private/ is shallow
anyway)". `os.walk` is called with no depth limit. Reproduced as
written in code, not as described in prose.

**Basename matching compares files that were never the same file.**
The overlay probe matches `Path(path).name` against each directory's
listing, so any `packages.el` under `~/.emacs.d` can be reported as a
mirror of any tracked `packages.el`. That *is* the intent — the
question is which copy Emacs loads, and the overlay does not preserve
project layout — but one sweep row exists to pin it: `file where
emacs-config emacs.d/layers/projdb/packages.el` reports drift against
the **templedb** layer's `packages.el`, a different file that happens
to share a name.

**One divergence is the port's, and it is a hang that is not an
output.** `os.walk(followlinks=True)` with no `seen` set walks a
symlink cycle until it runs out of path length. This keeps a set of
canonicalised directories and visits each once. It cannot show up in a
diff, because the only case where the two differ is the one where
Python has not finished producing output.

**And one place the two are not comparable at all**: an existing file
that cannot be read prints Python's `Exception` text in parentheses,
and this prints its own. Same class as the error messages the known
gaps list, reachable only for a file present and unreadable, and the
only line in the command that is not byte-diffable.

`--no-color` was deliberately **not** added. Python always paints and
has no flag; `summary` has one and pays for it with a permanent entry
in the sweep's flag allow-list, and there was no reason to buy a second.
A byte-diff of coloured output is a byte-diff: flag surfaces went 24 →
25 with no new divergence.

### The resolver the hardcoded paths were standing in for

This is the entry that used to say *the `edit-work` probe can never
fire*. It was a correct reading of Python in September, a stale one by
the time `file where` shipped on 2026-11-05, and the sweep is what said
so — which makes it the clearest instance in this file of the method
working on its own author.

**What the sweep reported.** Measured 2026-11-15, before the change,
`file where` differed from Python on **three of its four tree-reading
sweep rows**, and two further slug/path pairs found by hand show the
same defect pointing the other way — five cases, two directions:

| slug and path | before |
|---|---|
| `templedb-rust README.md` | `edit-work` line missing |
| `templedb integrations/emacs/templedb-agent.el` | `edit-work` line missing |
| `system_config emacs.d/layers/templedb/packages.el` | `edit-work` line missing |
| `woofsledger src/db.py` | `checkout ` line missing — its tree is its `repo_url` and not the canonical path |
| `nixos_anywhere_vultr_woofs .envrc` | `checkout ` line **invented** — this port hashed a directory Python does not resolve |

The last two were not sweep rows at the time and were found by walking
the first files of every project through both binaries. They are now
rows, and they are the two *directions* of one defect: a row that only
caught the missing half would have passed while the port invented a
mirror. The invented line is the worse failure, because the command's
entire job is to say which copy of a file is stale, and it was
answering about a tree nothing else reads.

**What replaced them.** `CheckoutRepository.resolve` classifies the
rows of `checkouts` by migration 113's `kind` and picks one per
purpose. Live shape, measured 2026-11-15 and unchanged on 2026-11-16:
**96 of 250 rows are active — 9 `canonical`, 80 `edit`, 7 `scratch`,
no NULL `kind`** — and 69 of the 96 carry a `session_id`.

| purpose | branch order |
|---|---|
| `Build` (Python's `PURPOSE_BUILD`) | newest extant `canonical`, else the newest extant row of any kind, else the newest row at all |
| `Edit` (`PURPOSE_EDIT`) | an extant `edit` tree this session owns, else one no live session owns, else the `Build` answer |

`crates/templedb-db/src/checkouts.rs` holds it as **one pure
function**: every input that is not a database row is a parameter —
`is_dir`, the ambient session, and `is_live` for a *row's* session — so
the branch table above is testable without a filesystem, an
environment or a session. Python reads `os.path.isdir`, `os.environ`
and two further tables from inside the decision. 15 tests cover the
branches, including the last-resort one where a `scratch` row wins
because every other directory is gone.

**The last fallback names a directory that is not there, and that is
reproduced.** Python's final branch is `(extant(rows) or rows)[0]`: if
no row's directory exists it returns the newest row anyway. The two
agree because the *caller* skips a mirror that does not exist, so the
missing line is the same on both sides — but the function does return a
path to nowhere, and a future caller that trusts it will be wrong on
both implementations equally.

**The `repo_url` fallback is what the `woofsledger` row was about.** A
project with no active checkout row falls back to `projects.repo_url`,
"for projects whose working directory IS the repo", and **14 of the 26
live projects are in that state** — so the hardcoded
`checkouts/<slug>` path was not merely sometimes wrong, it was wrong
for more than half the fleet. The guard inside the fallback is that
`repo_url` is not always a path: **13 of the 26 carry a `://`**, such
as `neko-lan-host`'s `templedb://neko-lan-host`. Reproduced including
the shape of the test — Python's `'://' not in str(repo_url)` rejects a
path with a `://` anywhere in it rather than only a scheme prefix.

**Two of Python's four purposes are deliberately absent.**
`PURPOSE_WRITE` exists to refuse — it declines to hand a writer the
published tree — and `PURPOSE_SCAN` declines to adopt a stale tree,
because a whole-tree scan records an absent file as a deletion. Both
belong to step 27, both exist for writers, and neither has a reader
here to verify it against. `Purpose` therefore has two variants rather
than four with two unreachable: a future caller has to add the branch
rather than discover that a guard it assumed was there is not. Python's
three warnings are not reproduced either — they are stderr, where this
README already records the wording as the port's own, and
`_warn_if_behind` hashes up to 200 files to decide whether to print.

**One tie-break added, and this time it selects nothing today.**
`active` orders by `checkout_at DESC, id DESC`; Python orders by
`checkout_at` alone and then takes `[0]` of whichever bucket it
filtered into, so a tie decides *which directory a command reads*
rather than the order of a listing. Measured 2026-11-15 and again
2026-11-16: **zero of the 96 active rows share a
`(project_id, checkout_at)` with another**, so the tie-break costs no
parity and buys nothing measurable. It is here anyway, for the reason
`intent list`'s is: `checkout_at` has second granularity, `templedb
edit` writes a row per workspace, and two agents provisioning in the
same second is an ordinary event. Staying inside a deterministic window
is not a strategy when the window is a property of live data.

**The one thing that will confuse the next reader**, stated here
because the two functions are a single grep apart: `checkouts.rs`
resolves `TEMPLEDB_SESSION` **by name**, which `session.rs` flatly
refuses to do. They are not in tension.
`VcsService.get_current_session` — what `session.rs` would have to call
— *inserts* a row for a name it does not find, and a port that creates
session rows has no read-only guarantee left to argue from.
`CheckoutRepository.current_session_id` is a different function with an
overlapping job, two `SELECT`s, and a docstring saying exactly that:
"Resolving a path must not have the side effect of opening a session."
So the name is readable here and refused there, on the same rule — the
rule is about writing, not about the variable. Declining it here would
have made this port probe a different directory from Python for every
caller that exports the name, which is most of the agent wrappers in
this system.

**The mirror grew one table and one more reader for another.**
`checkouts` arrives at seven of nine columns — `branch_name` and
`last_sync_at` are selected by Python's resolution query and read by
neither purpose ported here — and `vcs_sessions` goes from two columns
to four, because resolving a session *by name* needs `name` and
`started_at` where checking one by id needed `id` and `ended_at`. It is
also the first table in the subset added because **a shipped command
was wrong** rather than because a new command needed it.

**What it cost the fence**, since `file where` is the one command here
that reads a filesystem: the sweep's tree fence digests the roots a row
reads, and two of those roots are now chosen at run time. Naming the
enclosing directory instead was measured and rejected —
`edit-workspaces/templedb-rust` was **70,806 entries** on 2026-11-15
and **71,822** on 2026-11-16, and `edit-workspaces/templedb` is 57,046
both days — because an edit workspace holds whatever the workspace
holds, including the `target/` directory the sweep is writing into
while it runs. The 1,016-entry move in one day is this port's own
build, which is the argument rather than an aside: fencing those roots
would make every row `churned`, which is to say unchecked. So one file
per mirror label is outside the fence — a `stat` and a hash rather than
a walk, the three overlay roots still fenced — and it is in *Known
gaps* rather than left to be inferred from a passing row.

**The lesson is narrow and it is not "read Python again".** A
reproduced bug needs a check against the other implementation's
*behaviour*. Step 26 had a measurement, and the measurement was of the
state the behaviour used to depend on — the sort of evidence that makes
a stale claim feel verified. `where_view.rs` also had a test for it,
named `edit_work_probe_omits_the_session_directory_python_omits`, and
what it asserted was that this port builds the path Python *used to*
build — a test of the reproduction rather than of the parity, and one
that would have gone on passing for as long as the reproduction stayed
faithful to a September reading.
The thing that actually caught it was the all-command sweep, run after
a change to something else: the same mechanism, and the same month, as
*The first place this port fell behind Python*.

### `--commit`, and why it is one transaction

Python's `--commit` runs two: `file set`'s write commits, then
`VCSCommands().commit()` opens its own, and if the second fails the
first has already landed. Its comment explains the one structural
choice it does make — the call is in-process rather than a subprocess,
"because a subprocess would fork a new session (ppid differs),
defeating the whole point". That reason does not apply here, since this
port resolves a session only when told one, so the question of how to
compose the two halves was open rather than settled by precedent.

One transaction, and the argument is that it costs nothing. The
outcome Python's arrangement produces on a failed commit — content
written, staged, uncommitted — is still reachable from this port: it is
what `file set -s` does. So making `--commit` atomic removes no
capability and buys what the flag's name claims, which is that either
the content is committed or nothing happened.

It also removes the one genuinely awkward interleave. With two
transactions a `HEAD_RACE` lands the write, stages it, and exits
non-zero — leaving a caller who asked for `file set --commit` to know
that the remedy is a *different* command. The refusal's own text says
"Nothing has been written", and under one transaction that sentence is
true of the write as well.

`a_refused_commit_rolls_back_the_write_that_asked_for_it` is the test,
and it is written to be able to fail in both directions: the new path
is asserted absent *and* a staged row from before the transaction is
asserted untouched, so a rollback that took too much is as visible as
one that took too little.

**`--commit` commits the session, not the path.** The commit selects
staged rows by `staged_by_session_id`, so anything an earlier
`file set` in the same session staged goes with it. That is Python's
behaviour exactly and Python says nothing about it; this prints a note
when it happens, because `file set x --commit` reads as a claim about
`x`.

One thing it deliberately does not do: **stdout stays one line holding
the content hash.** `vcs commit-staged` prints its commit id there
because that is what its callers chain on, and here the caller chained
on the content hash before this flag existed. A flag that changes the
shape of stdout is how a script breaks without saying so. The cost is
real and is not hidden — a `--commit` run cannot pipe its commit id,
and a caller who needs it reads stderr or runs `vcs log -n 1`.

### The intent reads

`intent list` and `intent show` are two reads over one already-mirrored
table, which should make them the least interesting step in this list.
What makes them worth a section is that the README had the wrong thing
queued, and said so in three places.

The *Known gaps* entry read:

> `edit_intents` is mirrored at twelve of fourteen columns, and the two
> omitted are the ones a *reader* would want: `cancelled_at` and
> `applied_commit_id`. They belong to `intent cancel` and to the commit
> link `vcs commit` would make, neither of which is ported, so nothing
> here could verify them.

`mirror.rs`'s table comment said the two were "left to `intent cancel`
and `vcs commit`", and the Postgres DDL's header listed `edit_intents`
among three tables that follow the boundary rule "read sideways". All
three name **writes** as the unblocker for two columns. `intent show` is
`SELECT i.*` and prints both of them, so a pure read is what moved them
from unverifiable to byte-diffed — the boundary rule exactly as written,
*follow the readers*, with no sideways reading required.

The failure is not that the rule was wrong. It is that `edit_intents`
arrived in the subset for `file set`'s provenance record, so for two
weeks it really did have a writer and no reader, and the sentence
explaining its missing columns was written in that frame and then copied
twice without being re-asked. By the time `intent show` was portable the
explanation had become the reason not to look.

That is the same shape as two earlier corrections here — the step-15
entry that read `SqliteWriter` out of a superseded recommendation, and
the `doctor` paragraph that was "wrong twice over" and eleven days stale
when it was written. The pattern is specific enough to name: **a
statement about why something is blocked outlives the blockage, because
nothing re-derives it.** The audit's method exists for outputs drifting;
this is prose drifting, and only reading the Python source again catches
it.

So the ordinary accounting. `edit_intents` is at all fourteen columns,
the writer-only exception list drops from three tables to two, and the
two commands are byte-identical across eleven invocations. `intent
create`, `apply` and `cancel` are unported and all three are writes;
`apply`'s three-way merge is the same engine `file set --if-match` does
not carry.

Two details found on the way:

**Python's `list` query selects eleven columns and prints eight.**
`base_revision`, `new_content_hash` and `author` never reach stdout, so
the port's query names the eight and the mirror grew for none of them.
That is the fourth select-and-discard in the port — `vcs log`'s
`files_changed`, `doctor history`'s `duration_ms` and
`sample_issues_json`, and now three at once — and the count has crossed
the line where it is a habit rather than a curiosity: **what a command
prints is a better guide to the subset than what its query selects.**

**`intent show` exits 1 on a missing id.** `source snapshot`, `entity
explore`, `entity trace` and `entity paths` all exit 2 for a
well-formed question with a negative answer, which is what step 12
established. `intent show` is a bare `logger.error(...); return 1` in
Python, so there is no helper to inherit the 2 from. Reproduced at 1,
because the contract is Python's status and not a consistent one, and a
sweep row pins it so a later tidy-up shows up as a diff.

### The tie-break Python does not have

`intent list` carries this port's one open ordering divergence, and
it is the third time the question has come up with a different answer.

Python orders by `created_at DESC` and nothing else. `created_at`
defaults to `datetime('now')`, so it has second granularity, and
measured on the live database 2026-10-30: **32 of the 1,346
`created_at` values are shared by two rows each.** Among those pairs
the order is whatever the engine returns, and neither SQLite nor
Postgres promises anything about equal keys.

This port adds `id DESC`. Three reasons, in the order they actually
decide it:

1. **The port has two backends and the sweep compares them.** Without a
   total sort key, `intent list --all-statuses` past the first tie is
   free to differ between SQLite and Postgres. That is not a cosmetic
   divergence, it is a failing row on an axis this README reports as 72
   of 72.
2. **`--limit` cuts inside a tie group**, so an untied order does not
   merely reorder the output — it changes which rows appear at all.
3. `id` is monotonic per insert, so within a tied second it recovers the
   order the intents were proposed in, which is the question the command
   asks.

Compare the two precedents. `doctor history`'s `id DESC` tie-break was
*already in Python*, with a comment explaining that a full `doctor
entities` pass leaves ~44 rows sharing a timestamp — there, reproducing
it was the work. `entity search` had no tie-break on either side, the
port added one as a deliberate divergence, and the two converged when
Python grew one of its own. This is `entity search` again at the same
stage: a divergence that closes the day upstream adds four characters,
and worth reporting there rather than carrying here indefinitely.

**What it costs against the byte diff was measured, and then the
measurement decayed in one day.** This is the more useful half of the
story, so it is kept rather than tidied.

On 2026-10-30 the newest tie group sat at **rank 134**, and the sweep
rows were chosen to stay inside the first 134 — where both
implementations are deterministic and a byte diff means something.
That was true when it was written. On **2026-10-31** two intents landed
at `2026-10-31 17:43:51`, the newest tie group moved to **rank 4**, and
four sweep rows began differing. The sweep reported it as four
unexpected differences, which is exactly right: the port's output had
not changed and its claim about the oracle had.

The lesson is not that the measurement was wrong. It is that **"stay
inside the deterministic window" is not a strategy**, because the
window is a property of live data and the data moves. A divergence that
is permanent until upstream changes belongs in the sweep's `EXPECTED`
list, where it is stated once and holds, rather than in a row's choice
of arguments, where it has to keep being true. The three
`--all-statuses` rows are exempted there now, alongside `entity
explore`'s ordering and for the same reason.

The narrow rows are deliberately *not* exempted. `intent list` with no
flags is the default `status = 'proposed'` view — 2 of 1,356 live rows,
small enough that a tie is unlikely rather than impossible. If one
lands they move up to `EXPECTED` too, and that is the right failure
mode: a new difference reported once beats a row quietly exempted in
advance, which is the mistake the `--staged` allow-list entry made.

Worth keeping from the original measurement, because it is still true
and still a trap: check the **filtered** views, not only the unfiltered
one, because a filter changes which rows fall inside the limit. Three
candidate `-s` rows were rejected that way — `-s 941`, `-s 1169` and
`-s 1000` each have tie groups inside their first 50.

### The two forms that were never blocked

`vcs diff --staged` and the bare `vcs diff <proj>` landed 2026-10-31 at
**zero schema cost** — no new table, no new column, no new machinery.
Both had been listed as not ported since step 9, and the reason given
for each turned out to be wrong, in two different ways.

The *Status* table said "`--staged` and the working-set form not
ported", grouping them. They are not one gap. `_diff_working` and
`_diff_staged` are both **pure database**: between them they read
`vcs_working_state`, `project_files`, `file_contents`, `vcs_branches`,
`vcs_file_states`, `vcs_commits` and `content_blobs`, every one of
which has been in the 21-table subset since step 19. Neither touches a
filesystem. The genuinely tree-dependent diff is a different thing
again, and this README's section about it — *`vcs diff <proj> <file>`
cannot ever show a diff* — is about the no-commits form and was never
about either of these.

The sweep's own allow-list carried the sharper version of the error:

```
# Needs the staging session, which is a write and six environment
# fallbacks.
"vcs diff p f|--staged"
```

Both halves are false. `_diff_staged` resolves **no session at all** —
its predicate is `staged_by_session_id IS NOT NULL`, meaning *any*
session — and it writes nothing. The exemption had been sitting in a
list whose stated purpose is that "a divergence that *closes* shows up
as a surprise", and this one could not, because the row made the flag's
absence look intentional. That is the fourth stale blocked-reason in
this port's reach, after `edit_intents`' two columns, step 15's
`SqliteWriter` and the `doctor` paragraph.

Also worth recording: `file` was a **required** positional here and
Python's has been `nargs='?'` for as long as `_diff_working` has
existed. The flag-surface stage cannot see that — it parses the
`options:` block and never compares positional arity — so this is a
second blind spot in the same stage, alongside commands only one side
has.

**What the two forms do not agree about.** They are structurally alike
and three of their behaviours differ, which is why the port keeps two
renderers rather than one with flags:

| | `--staged` | bare form |
|---|---|---|
| new side of the diff | `file_contents.content_hash WHERE is_current` | `vcs_working_state.content_hash` |
| label | `(staged)` | `(working)` |
| unreadable content | truthiness, so an empty file is skipped | `is None`, so an empty file still diffs |
| unknown state | header only, no body | falls into the modified renderer |

The first row is a bug, and it is Python's. `_diff_staged`'s query
aliases `ws.content_hash AS staged_hash` and then uses it in exactly
one of its three branches; the `modified` branch builds its new side
from `current_hash` and labels it `(staged)`. Since `ws.content_hash`
is documented upstream as "the hash of the file on disk", those differ
whenever a workspace edit has not been written back — and **all three
live staged rows diverge** as of 2026-10-31: two carry a NULL staged
hash and one a genuinely different one. So every `--staged` diff of a
modified file currently labels the database's bytes as staged.

Reproduced, because the output is the contract and the byte diff is
what proves the rest of the renderer right. Reported rather than fixed
here, and the sibling function is the argument: `_diff_working`, two
screens away in the same file, builds its new side from
`ws.content_hash` and labels it `(working)` — correctly. The sweep row
for `regr-test --staged` is the one that pins it, so an upstream fix
shows up as a diff rather than as silence.

**One tie-break added**, same decision as `intent list`'s.
`_get_last_committed_content` orders by `vc.commit_timestamp DESC LIMIT
1` with nothing after it, and measured on the live database: **4
`(file_id, commit_timestamp)` groups hold more than one row**, up to
three. For those files the committed side of the diff is arbitrary and
can differ between the two backends. `vc.id DESC` makes the key total,
and it changes no current output — checked, not assumed: none of the
three live staged rows is one of the affected files.

**One behaviour worth knowing as a user.** `--staged` is not
session-scoped. Its predicate takes any session's staged rows, while
`vcs commit-staged` scopes to one and refuses the rest — so this
command can show work a commit would not take. Python's behaviour,
reproduced, and flagged because reading the output as "what my next
commit contains" is reading it wrong.

### The rebase this port refuses to do

Python's `--if-match` no longer refuses a stale base. Since the audit
it three-way merges: the expected hash is the base, the database's
current content is the other side, and a clean `diff3` means the write
applies on top of what landed. Only a genuine overlap exits 2, and
`--no-rebase` restores the old behaviour. `intent apply` does the same
thing with the same function, and there the merge is the *normal* path
rather than an edge case, because an intent is designed to outlive the
state it was written against.

Its reasoning is good and is quoted here because it argues against this
port's position: refusing "was the behaviour that made the flag
expensive to use — the caller's only options were redo the work or drop
the guard, and dropping the guard is what silently reverts people".

**This section said "not ported, and the reason is scope rather than
disagreement" until 2026-11-18, and the reason was wrong.** It read:

> It needs a `ThreeWayMerge`, which exists over there for `vcs merge`
> and does not exist here; `difflib` is in this port but a merge engine
> is a different piece of work.

That is accurate about the effort and it had never been checked against
the engine. Step 33 checked it, because `intent apply` could not be
ported without deciding, and the engine named there is **not a
three-way merge**. The heading changes from *does not* to *refuses to*:
the divergence is now a decision with a measurement behind it rather
than a gap waiting on a sprint.

#### What `ThreeWayMerge` actually does

`merge_resolver.ThreeWayMerge._merge_with_diff3` is 110 lines and its
docstring is "Three-way merge using diff3 algorithm". It opens by
computing `difflib.unified_diff(base, ours)` and
`difflib.unified_diff(base, theirs)` and **never reads either one** —
two full diffs, assigned and discarded, which is the sixth
select-and-discard this README has catalogued and the first in code
rather than in SQL. What it does instead is walk the three versions
with three indices, advancing all of them by one on every iteration,
and compare each side's *n*th line against base's *n*th line.

Two consequences follow from the lockstep, and neither is in the
docstring:

- **It is correct only while all three versions have the same number
  of lines.** One insertion on either side shifts that side's indices
  permanently out of alignment with base's, and every line after the
  insertion is compared against the wrong line.
- **Its loop is `while i_base < len(base_lines)`,** so anything either
  side added past the end of base is never visited and silently
  vanishes.

#### Measured, 11 triples, 2026-11-18

Eleven `(base, ours, theirs)` triples built by editing one 4-line base
— in-place edits, insertions, deletions and appends, on each side and
on both — compared against `git merge-file -p --diff3`:

| outcome | count |
|---|---|
| agrees with `git merge-file --diff3` | 3 |
| reports a clean merge, produces different text | 4 |
| conflicts where a real diff3 merges cleanly | 3 |
| reports clean where a real diff3 conflicts | 2 |

Three of eleven, and the three that agree are exactly the
equal-line-count in-place cases. The failures are **not
conservative**, which is the part that decides the question. Seven
triples report a clean merge; five of those seven produce text that is
not what a three-way merge yields; `rebase_onto_current`'s
`merged == current_text` guard catches **one** of the five, and
**four reach `file_contents`**.

Two of the three spurious conflicts are ordinary insert/delete cases.
The third is worth naming on its own: when both sides make the
**identical** edit, it conflicts. Each side is compared against base
rather than against the other, so two equal changes are two changes.

#### Reproduced end to end, not just in the library

The worst outcome is silent, and it was driven through the shipping
command rather than inferred from the function. On a copy of the live
database, with `TEMPLEDB_PATH` pointed at it:

```
$ templedb file cat regr-test test.txt
one
two
three
four
$ printf 'one\ntwo\nNEW\nthree\nfour\n' | \
    templedb intent create regr-test test.txt --base-rev c45d3a272228…
✓ Intent #1645 created (proposed)
$ printf 'ONE\ntwo\nthree\nfour\n' | templedb intent create … && \
    templedb intent apply …        # the file moves in place
$ templedb intent apply 1645
✓ Intent #1645 applied
  rebased onto 6070bdcd4256 (was written against c45d3a272228)
  test.txt → 6fb1ab4272d4 (proposed c7195b42f740)
$ echo $?
0
$ templedb file cat regr-test test.txt
ONE
two
NEW
three
```

`four` is gone. Base had it, `ours` had it, `theirs` had it, nobody
deleted it, the command printed a success line and exited 0.

#### So the port refuses, and says so in four places

`file set --if-match` is the strict compare-and-swap Python's
`--no-rebase` selects, and `intent apply` exits 4 on a moved base for
the same reason. The argument is not that the port lacks a feature.
Reproducing that engine would mean writing content neither the author
nor the database ever held, under a success message, into the one
table whose rows *are* the user's files — and the project's standing
rule is that the unacceptable outcome is the silent one. This is the
loud one.

**The two `--no-rebase` flags are handled differently, and that is an
inconsistency this port now owns.** Python has one on each command,
with the same meaning: refuse instead of merging.
`file set --no-rebase` is **not** accepted here and sits in the sweep's
flag allow-list as one pair, decided under the `--no-fts` rule — a flag
that switches off behaviour the port does not have would be accepted
and ignored, which is worse than absent. `intent apply --no-rebase`
**is** accepted, decided here under a rule that cuts the other way: a
caller passing it is asking for the behaviour this port already has on
every input, and the stricter implementation erroring on the flag that
requests strictness is the worse of the two outcomes.

Both arguments are defensible and they are not the same argument, which
means one of the two commands is wrong. The `--no-fts` rule is about a
flag that *promises* something undeliverable; `--no-rebase` promises a
refusal, and this port refuses. By that reading `file set` should
accept it too and leave the allow-list — and that is not done here, on
the grounds step 30 used for `search content -p`: changing an
already-shipped command's flag surface is a step of its own, not a
side-effect of a step about something else. It is in *Known gaps*
rather than smoothed over.

What the sweep can see of any of this is nothing, and that is the
**fourth blind spot** in its flag stage, added with this step: it
compares flag *names*, so `intent apply`'s surface agrees with
Python's exactly while one of the two shared flags means something
different on each side. The divergence is observable only in the exit
status, which no row of a read-only sweep can reach.

### The two columns that nearly rhyme

`project list` has three filters now. `--nix-only` and `--category`
were ported in step 1; `--type` arrived on Python's side afterwards and
the flag-surface stage reported it.

### The two columns that nearly rhyme

`project list` has three filters now. `--nix-only` and `--category`
were ported in step 1; `--type` arrived on Python's side afterwards and
the flag-surface stage reported it.

The trap is that `project_category` and `project_type` are different
columns whose names almost match, and they share exactly one value:

| flag | column | values |
|---|---|---|
| `--category` | `project_category` | package, service, desktop-app, nixos-module, home-module |
| `--type` | `project_type` | regular, nixos-config, service |

A port that read the wrong column would be right about every value
except `service`, and `service` is the one a casual check is least
likely to try. So both the in-memory fixture and the sweep's filter
list test it specifically: the fixture's `beta` is
`category = 'package'`, `type = 'service'`, which makes
`--type service` and `--category service` select different rows, and
`PROJECT_FILTERS` carries both spellings.

Python's own comment is the clearest statement of what the flag is for:
it "replaces `nixos list-configs`, which was this query with
project_type hardcoded to 'nixos-config'".

### Why `doctor entities` is not a command this port can have *yet*

**The title lost a word on 2026-10-31**, and it was the load-bearing
one. Everything below is still measured and still correct about what
the command costs; what was wrong was the frame — see *Deferred, not
refused*. Under eventual replacement this is a command the port will
have, late, and the three numbers below are its bill rather than its
refusal.

The porting order said for five revisions that `doctor` sat behind the
write-path design document, because `doctor entities` inserts into
`invariant_checks`. That reasoning expired when the write path landed,
so the entry was re-measured on 2026-10-29. The blocker is a different
one and it is larger.

**`doctor entities` is 44 checks**, not a command. Classified by what
each one reads — **re-derived from the 44 method bodies on 2026-11-05**,
which moved three of the five numbers:

| | checks |
|---|---|
| **A** — reachable from the mirrored 21-table subset | 17 |
| **B** — need tables outside it | 10 |
| **C** — ask SQLite about *itself*: `sqlite_master`, `pragma_foreign_key_check`, cr-sqlite's `pragma_function_list`, the `-wal` sidecar's size | 8 |
| **D** — read a filesystem | 8 |
| **E** — read the Python implementation's own argparse tree | 1 |

The previous version of this table had one bucket of 15 where C, D and
E now sit, and it was classified by reading check *names*. Two of its
entries were wrong, and splitting the bucket reports a blocker it hid.

**Only 4 of the 8 in D read a checkout tree** — `checkout_matches_db`,
`checkout_files_are_mode_locked`, `checkout_roles_are_unambiguous` and
`generated_file_matches_generator`. The other four read roots no tree
fixture covers: `~/.nix-profile` resolved into the store,
`/nix/var/nix/profiles/system-*-link`,
`~/.local/share/templedb/backups`, and
`~/.local/share/templedb/fhs-deployments` for `st_mode`. So the
tree-pinning harness of step 26 unblocks **four** checks here, not
fifteen, and the README said fifteen for five weeks.

**C is blocked by the verification backend, not by the tree, and that
is the harder constraint.** Those eight ask SQLite about itself. A
mirrored Postgres has no `sqlite_master`, no `pragma_function_list` and
no `-wal` sidecar, so there is nothing for the cross-backend digest to
compare and nothing for the sweep to byte-diff. While C and D shared a
row this read as one obstacle with one fix; they are two obstacles and
the tree is the tractable one.

**Two checks the old bucket counted as filesystem reads are pure SQL.**
`working_state_references_valid_files` is a `LEFT JOIN` orphan check
over `vcs_working_state` and `project_files` — both already mirrored,
portable today, and it is in A. `checkout_roles_are_unambiguous` is a
query over `checkouts` plus exactly one `Path(...).is_dir()` liveness
filter: no file content, no pinning, only directory existence.

**And one is unportable in principle rather than not yet.**
`cli_help_is_populated` imports `cli.core` and walks argparse's
`_choices_actions`; its subject is the Python CLI's parser tree, so
there is no Rust statement of the invariant. "Arrives whole, late" is
therefore not reachable either — at 43 of 44 the whole-set exit status
is still not reproducible. That needs a decision about what
`doctor entities` *means* in this port, which is a question the 17/12/15
table never raised because it had nowhere to put the check.

One more is portable but is a project: `no_undefined_names` reads
templedb's Python *out of `content_blobs`* rather than off the disk, so
its substrate is mirrored and it sits in A — but running it means a
Python parser and scope analyser in Rust.

So a ported `doctor entities` would run 17 of 44 and print
`(not ported)` 27 times. That is `summary`'s shape at a ratio that
inverts its argument: `summary` is seven panes of nine and names what
is missing, and this would be 39% ported and would mostly be naming.
Its exit status is `0 if not problems else 1` over the **whole** set,
so a partial run cannot reproduce the one part of a diagnostic command
a script actually reads.

It also writes three things rather than one. `invariant_checks` is
mirrored; the claims it asserts on every passing check and the
`reconciliations` episodes it opens and settles are a second and third
write path, and the claims tables are outside the subset entirely.

None of that is an argument about difficulty. It is the boundary rule
the subset has followed since it was built — follow the readers — and
`doctor entities` is on the far side of it for the same reason the
systemd pane is. The entry in the porting order now says this instead
of pointing at a document that no longer blocks anything.

**`doctor history` is none of that**, which is why it is ported: a
single `SELECT` over a table already mirrored, at a projection already
carried, with a tie-break worth reproducing. `ran_at` has second
granularity and one `doctor entities` pass stamps ~44 rows inside it,
so `ORDER BY ran_at DESC LIMIT 20` returns an arbitrary 20 of the
newest run — which is the wrong answer to the only question the command
exists to answer, and over there it made a test pass or fail depending
on what had run before it. `id DESC` recovers the order the checks ran
in. It is also the one ordering in this port that is right on both
backends for the same reason rather than by coincidence: the tie-break
makes the sort key total, so there are no equal keys left for SQLite
and Postgres to break differently.

### Checking the checker

*The sweep is a shell script, not a test* has been the first entry
under *Known gaps* since the audit: `cargo test` does not run it, so
nothing fails if it is never invoked. The entry proposed "a CI entry
point or a `nix flake check`" and both are only half-answers — the
sweep needs the live 776 MB database and the Python `templedb`, and a
test that silently skips when those are absent is the same failure one
layer down.

But **coverage is a different question from agreement**, and coverage
is answerable with nothing but the binary. The sweep exists because
per-command harnesses missed drift; a command absent from the sweep has
no harness at all, which is the same hole one level up. Nothing was
checking for it. `doctor history` could have been added to the CLI and
left out of `INVOCATIONS`, and every number the sweep prints would have
gone up.

`crates/templedb-cli/tests/sweep_coverage.rs` runs in `cargo test`,
with no database and no interpreter. It walks the binary's own `--help`
tree for every leaf command, parses `INVOCATIONS`, `FLAG_COMMANDS` and
`FLAGS_EXPECTED` out of the script — parses, rather than carrying a
second copy, because a test with its own list would pass while the
sweep ran none of them — and asserts six things. The two that matter:

- every leaf command is swept, or named in an exemption list with the
  reason a diff against Python is not defined for it;
- **every exemption still names a real command.** That is the direction
  an exemption list cannot check by being consulted, because it only
  ever suppresses — and it is exactly how `-s` and `--stage` sat stale
  in the flag allow-list for two days after they stopped being needed,
  found by hand.

It caught something on its first run: `summary` was in no `INVOCATIONS`
row. Not a hole — it is compared one pane at a time, because its whole
stdout can never match — but the two reasons a command can be absent
are opposite ("compared a different way" against "comparing it is not
defined") and the test now carries them as two lists, with an assertion
that nothing is in both.

What it still does not check is whether a row's *arguments* reach
anything interesting. `vcs log p` and `vcs log p -n 3 --published` both
count as coverage and only one of them exercises the published view.
That is a judgement the sweep's own comments carry and no test can take
over.

The CI entry point is `just check-all`, and naming it turned up that
`just check` has never been the whole gate: it runs the unit tests, the
oracles and the Python sweep, and skips the cross-backend sweep, the
mirror freshness check and the write tests. Those three are the only
verification the write path has, since a write cannot be byte-diffed
against Python — so the gate was skipping the newest third of the port.

### The state digest, and why it must not sort

The byte-diff method verified every read command because both
implementations print their answer. A write does not: `file set` prints
almost nothing and changes four tables, so the interesting output is not
on stdout at all. `statediff.rs` is the replacement, and the design
document insisted it exist before the thing it verifies.

What makes it more than a checksum loop is a constraint that only shows
up once the two sides are different engines. The obvious digest hashes
rows in `ORDER BY` sequence. That would be wrong here: `content_blobs`
keys on `hash_sha256`, a TEXT column, and SQLite compares TEXT by bytes
while Postgres uses the database collation. A sorted digest would
report an identical table as divergent — not reliably, which is worse,
because it would depend on which hashes happened to be present.

So rows are hashed independently and combined with `wrapping_add`,
which is commutative: the table digest does not depend on arrival
order. Addition rather than XOR, because XOR cancels — two identical
rows would vanish from the digest instead of doubling, and duplicate
rows are exactly what a write-path harness should catch. Keyset
pagination still needs an order, but it only needs one that is
consistent *within* one engine, so the collation difference is confined
to a place where it cannot do harm.

Values carry a type tag and a length. Without the length,
`("ab", "c")` and `("a", "bc")` hash identically and a write that
shifted a boundary between two TEXT columns would be invisible. Without
the tag, a column reading back as `Int` from one engine and `Text` from
the other would be silently coerced, when it is a real condition
between a dynamically typed source and a strict destination and worth
failing on.

**What it catches that `mirror --check` cannot.** `compare_counts` says
so itself: row counts miss an update in place. Demonstrated on
`templedb_scratch` on 2026-10-20 — flip one `file_contents.is_current`
and `mirror --check` still reports *fresh: all 17 tables match the
source*, while `templedb-rs state` reports the same 2,213 rows and a
different digest. The two are complements, not alternatives: counts are
the cheap check to run before every sweep, the digest is the thorough
one to run either side of a mutation.

The harness is a library plus `templedb-rs state`, which prints one
fixed-width line per table so a shell can capture before and after and
`diff` them. There is no comparison subcommand because there does not
need to be one.

**Not yet exercised against a real write**, because there is no write
to exercise it against — which is the order the design document asked
for, and means the first `file set` is also this harness's first real
test rather than its validation.

### `summary` is not a function of the database

The half-ported command, and the one that changed how the rest of this
file reads. Written up in full as the TempleDB report of the same name
(2026-10-18); the part that belongs here is what it means for the
method.

**The byte-diff harness is a purity test, and nobody designed it as
one.** It works because both binaries read the same input and read
nothing else. Every command ported so far satisfies that — `file cat`,
`entity trace`, `vcs show` are all pure functions from database state
to bytes, which is also the only reason asking "do SQLite and Postgres
agree?" is a meaningful question. `summary` is the first command that
fails it, and it fails on five inputs that are not rows: the wall
clock, `os.getppid()`, the hostname, systemd, and
`TEMPLEDB_SESSION_ID`.

This file used to list three commands as not byte-diffable —
`project list`, `search content` and `summary` — as though they were
the same kind of exception. The first two are deliberate output
choices. The third is a different thing entirely, and grouping them
hid it.

So the port splits the command three ways rather than reproducing it:

| layer | depends on | where |
|---|---|---|
| observation | the database alone | `templedb-db/src/summary.rs` |
| interpretation — ages, freshness, colour | the facts **plus an instant passed in** | the CLI |
| environment — systemd, session identity | the machine | the CLI |

The observation half is a function, so it is mirrored, diffed across
backends and tested without a clock. The two queries that genuinely
need the date take it as a parameter — `reconciliation_counts(store,
today)` — rather than reading one. That rule is not for this command:
every write stamps `datetime('now')` and mints `commit_hash` from
`time.time()`, so a write path built the ambient way produces a state
no harness can diff. **The clock and the identity have to be arguments
threaded from the entry point.**

**One query in the port cannot be written once, and it is not the one
you would guess.** Python's doctor pane asks:

```sql
SELECT check_name, status, MAX(ran_at) AS ran_at, issue_count
  FROM invariant_checks GROUP BY check_name
```

`status` and `issue_count` are neither grouped nor aggregated — an
error in Postgres (`42803`) and in standard SQL. It works because
SQLite documents that with exactly one `min()`/`max()` aggregate, the
bare columns come **from the row that produced the extreme**. So it
means "the latest run's status", which is what the pane needs, and the
repair that compiles everywhere — wrapping them in `MAX()` — returns
the alphabetically largest status instead. On live data that reports a
check as `violated` with nine issues when its latest run is `ok` with
none. A fixture row pins exactly that substitution.

`summary.rs` therefore carries two SQL texts for it, `DISTINCT ON` on
the Postgres side, in the open rather than behind the placeholder
rewriter. Verified: 42 invariant rows and 6 machine rows, byte-identical
between SQLite and the mirror. The fleet pane has the same shape and
the same split.

**And a tie-break Python does not have.** `ORDER BY ran_at DESC` alone
ties constantly here, because one `doctor entities` run stamps all 42
checks within the same second — so *which three problems the pane
shows* is the planner's choice. `(ran_at DESC, check_name)` makes it a
sequence. Same reasoning as `entity trace`, and the same kind of
divergence.

#### What the rendering layer turned out to be worth

Built 2026-10-21, in `templedb-cli/src/summary_view.rs`. The split had
been argued for on method grounds; rendering it is what paid for the
argument, because six of the seven ported panes came out byte-identical
with Python the first time they ran against the live database. A command
whose output was supposed to be unverifiable turns out to be verifiable
almost everywhere, once the parts that are not functions are named and
moved to the edge.

**The impure inputs are four lines, not a pane.** `Clock::from_env` and
a `session_id()` of `TEMPLEDB_SESSION_ID` or `(gethostname, getppid)`
is the whole of it. Everything else that looked like it needed the
machine needed an argument.

**The sweep compares panes, not the command.** A `summary` row in
`INVOCATIONS` could only ever land in `EXPECTED` — two panes are
missing, so the whole stdout can never match — and that would have
taken the other seven with it, reporting a command as a known
divergence when six of its panes agree to the byte. So the sweep gained
a pane-scoped stage instead: it splits both outputs on the accent rules
and compares the panes the port *claims*, naming the unported ones
rather than skipping them. It retries the whole capture up to three
times, because the ingestion pane renders ages and a minute boundary
falling between the two invocations reports `0min ago` against
`1min ago` — elapsed time, not a divergence. `TEMPLEDB_RS_NOW` is no
help: it pins this port's clock and not Python's, which would make the
comparison worthless rather than merely noisy.

**The seventh pane is the reconciliation listing, and it is the
tie-break again.** Python orders by `datetime(first_detected_at)` with
nothing after it. Three of the eight open rows share
`2026-10-16 14:32:25` to the second — one `doctor entities` run
detected them together — and the pane prints four rows, so *which* of
the tied three appear, and in what order, is the planner's choice.
`live_reconciliations` adds `, kind, subject_ref`. That is the fourth
place in this port where the same decision has come up, and the first
where it was already in the SQL before anything printed it.

**Two misalignments are reproduced rather than fixed.** Python pads
several strings *after* colouring them — `f"{_c(ts, colour):<40}"` —
so nine characters of ANSI escape count toward the forty and the
columns do not line up. And the doctor pane writes `_c(age, 'muted')`
with `age` set to `''` for a fresh check, putting an empty
`\x1b[90m\x1b[0m` pair at the end of the line. Both are on stdout, so
both are reproduced; the second is nine bytes of nothing that a
byte-diff would otherwise have caught.

The first of those has a consequence worth stating, because this port
has a `--no-color` flag and Python does not: **`--no-color` output is
not the coloured output with the escapes stripped.** Padding an
unpainted 19-character timestamp to 40 leaves 21 spaces; painting it
first leaves 12. The flag produces columns that actually align, which
is a different output rather than a cleaner encoding of the same one,
and a test pins the inequality so that nobody implements the flag as a
post-processing pass.

### The clock seam, and the float error that is part of the answer

Built 2026-10-17. `clock.rs` is small — one instant, a `julianday`
parser, and `_age_hint` — and the interesting part is that the obvious
implementation is wrong in a way no amount of care would have caught.

Python asks the database for every age:

```sql
SELECT (julianday('now') - julianday(?)) * 24 AS hours
```

A Julian Day is about 2.46 million, so an `f64` resolves it to roughly
47 microseconds, and `_age_hint` **truncates** the result. Those two
facts meet at every boundary:

| gap | what SQLite computes | what `summary` prints |
|---|---|---|
| exactly 1 minute | `0.016666665673…` × 60 = 0.99999994 | `0min ago` |
| exactly 1 hour, at 12:00 | `0.9999999962747097` | `59min ago`, **fresh** |
| exactly 1 hour, at 06:30 | `1.0000000074505806` | `1h ago`, **stale** |

The same gap, an hour apart in the day, lands on opposite sides of the
`hours < 1` test. So an implementation that computes the age
*correctly* — integer milliseconds, divided once — prints a different
string from Python's for some inputs, and is wrong for this port's
purpose. `clock.rs` therefore holds the instant as integer
milliseconds and divides by 86,400,000 to get a Julian Day, which is
what SQLite does internally (`sqlite3_int64 iJD`), so that the
rounding is the same rounding.

That is not a conclusion anyone would reach by reading the Python. It
came out of the oracle, and the oracle is the deliverable as much as
the module is: `tests/clock_parity.rs` puts 2,000 timestamp pairs
through CPython's `sqlite3` — half of them landing on an exact minute,
hour or day boundary on purpose — and compares the `f64` bit-for-bit
as well as the rendered string. Checked for vacuity the same way the
difflib oracle was: replacing the subtraction with exact integer
arithmetic, which is the *more* accurate implementation, fails it
immediately.

Three smaller things the exercise settled:

- **The `sqlite3` CLI cannot be used as the oracle.** Its
  `printf('%.17g')` reports `1.000000007450581` for a double whose
  exact value is `1.0000000074505806`. The harness goes through
  Python, whose `repr` round-trips — and Python is the implementation
  being matched anyway.
- **A zone suffix is a different instant, not decoration.** SQLite
  reads `06:30:00+02:00` as 04:30 UTC. No column this port reads
  carries one, so the parser could have refused them; it applies them
  instead, because a parser that silently differs from the one it
  reproduces is the kind of gap that surfaces years later through a
  column nobody checked.
- **The `ancient` cutoff is `threshold × 24`.** With the reconcile
  pane's 168-hour threshold that is 168 *days*, so a machine
  unreconciled for three months renders as `2184h ago` rather than in
  days. It reads like a bug, it is on stdout, and it is reproduced.

The seam itself is the small part: `TEMPLEDB_RS_NOW` pins the instant,
`Clock::system()` is the default, and nothing else in the crate reads
a clock. The prefix is `TEMPLEDB_RS_` rather than `TEMPLEDB_` on
purpose — a variable that pinned one binary's clock and not the
other's would make a parity run look like agreement.

### `entity explore` orders its edges and Python does not

The one divergence the audit *created* rather than closed, and it is the
`entity trace` story with the luck run out.

`explore`'s edge query has no `ORDER BY` in Python, so the rows arrive
in whatever order the planner produces — which for
`Machine/zMothership2` is `gen-321, gen-322, gen-323, gen-325,
gen-324, gen-326`. Stable across runs today, and not sorted. This port
sorts by `(relation kind, peer kind, peer ref)`, the same key it uses
in `trace`, so `gen-324` comes before `gen-325`.

The README used to call `explore` byte-identical, and it was: the
planner happened to agree. It no longer does on one row out of 173.
Nothing about the port changed — the data did, which is precisely the
risk the `trace` note already recorded ("that is luck, not a
guarantee") and the reason the note was written.

Not fixed by removing the sort, for the reason `vcs log`'s tie-break
was chosen the other way: there, one total order both fixed the
nondeterminism *and* matched Python, so taking it was free. Here no
order matches Python, because Python has no order — matching it would
mean reproducing the absence of an `ORDER BY` and inheriting a command
whose output the query planner picks. The edge *set* is identical, which
is the part a caller can depend on.

### `--side-by-side` is not side by side

The flag selects `difflib.context_diff`. A context diff prints each
hunk twice — the old file's version of it under a `*** 1,4 ****`
header, then the new file's under `--- 1,4 ----` — one above the other.
There are no columns. Nothing in Python's `--help` ("Side-by-side
diff") or in the `_display_diff(unified=False)` call site says so; the
only way to find out is to read `difflib`.

Ported under the misleading name, because the name is the contract:
`-s` is the short form, it is in `templedb vcs diff --help`, and
renaming it here would mean the two binaries disagree about their own
CLI. The Rust doc comment and `--help` text say what it actually does,
which is the most that can be fixed from this side.

Three things about the format are easy to get wrong and all three are
visible in the output:

- **A context range names both endpoints**, `first,last`, where a
  unified range is `start,length`. The same four-line hunk is `10,13`
  here and `10,4` there, and both read as a comma-separated pair. They
  coincide only when the hunk starts at line 1 — which is exactly what
  CPython's docstring example does, so the published case cannot catch
  this.
- **An empty range is a bare number** — the line *before* the range —
  where unified prints `N,0`.
- **A side with no changes prints its header and no body.** A hunk that
  only inserts emits `*** 1,2 ****` followed directly by
  `--- 1,3 ----`. Implementing it as "print the lines anyway" produces
  output that looks right on every hunk that both deletes and inserts.

Each of those is a unit test and each was confirmed non-vacuous by
mutation; see *How it is verified*.

### `vcs diff <proj> <file>` cannot ever show a diff

With no commit arguments the command diffs a file's two newest
`file_contents` rows. That table holds **no history**, so there is never
a second row:

| | |
|---|---|
| files with a `file_contents` row | 2,095 |
| files with more than one | **0** |
| rows where `is_current = 1` | all of them |
| highest `version` observed | 14 |

`version` climbs to 14 while the row count stays at one, which says the
writer updates in place and the previous content is not retained here.
(It is not lost — it lives in `vcs_file_states` and `content_blobs`,
which is what the other two forms read.) So `LIMIT 2` returns one row
and the command always prints `Only one version exists, no diff
available`, despite `--help` advertising "Show diff between file
versions".

Ported faithfully anyway, because that message *is* the command's
answer, and the fixture carries a two-revision file so the comparison
path is covered by a test even though live data cannot reach it. Noted
rather than fixed: making the form work means retaining history, which
is a write-path decision.

`UNIQUE(file_id, is_current)` is related and also worth recording. Its
comment reads "Only one current version per file", which is not what it
says: with `is_current` as 1/0 it permits one current row *and* one
non-current row, so it caps the table at two revisions per file rather
than constraining how many are current. Expressing the stated intent
needs a partial index. Reproduced in both schemas, because the mirror
has to accept the rows the live database produces.

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
merging, `get_opcodes`, `get_grouped_opcodes`, `unified_diff` and
`context_diff`. `isjunk` is left out: the call site passes `None`, which
makes two of `find_longest_match`'s four extension loops dead.

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

Every case runs through **both** renderers — the oracle takes the
difflib function name as an argument — because `unified_diff` and
`context_diff` share the matcher and the hunk grouping but not the line
format, and the format is where the differences hide. Asking CPython
the same question twice is cheaper than reasoning about which of the
two could diverge.

That harness was then checked against itself by mutation, because a
parity test that passes on its first run may be vacuous. Disabling
autojunk fails 2 of its 3 cases; inverting the longest-match tie-break
from `>` to `>=` fails all 3. For the context renderer: printing a
context range as `start,length` instead of `first,last` fails all 3 and
one unit test; printing each hunk's old side unconditionally instead of
only when it has deletions fails all 3 and one unit test.

End to end, `templedb-rs vcs show` is byte-identical to
`templedb vcs show` across 39 commits in 6 projects, with colour on and
off, the largest being a 15,171-line diff. Root commits are included:
they have no parent row, so the file is diffed against the empty string
and the header reads `∅`, which is Python's marker. A 4,816-line
whole-file addition agrees exactly. A later sweep added 164 more
`vcs show` invocations — every commit in the database touching a binary
or a misclassified blob, plus 60 by id — including a 52,848-line output
carrying 24 binary notices.

`vcs diff` is diffed across 200 invocations: 10 files with the most
revisions, consecutive commit pairs from each, every pair run through
both renderers with colour on and off and through `-s` as well as
`--side-by-side`. Plus 18 edge cases — the one-commit and no-commit
forms, a `%` and a `_` as the commit ref, a nonexistent commit, an
8-character prefix, a binary file and a file deleted in one of the two
revisions. Exit codes included.

An earlier revision of this file recorded a reproduced wrongness here: a
**binary** file in a commit diffed as `No differences`, because Python's
content lookup returned nothing for any blob whose `content_type` was
not the literal `text`, the caller coerced that to the empty string, and
empty-against-empty is no change. That is fixed on both sides now — see
*The first place this port fell behind Python* below — and `vcs show`
names the three cases apart.

### The first place this port fell behind Python

Every divergence above is this port moving ahead of the implementation
it replaces. This one went the other way, and it is the most useful
entry in this file for anyone maintaining a port against a moving
target.

**This heading said *the one place* until 2026-11-16, and it is now the
first of two.** The second is `file where`'s two mirror roots, written
up under *The resolver the hardcoded paths were standing in for*, and
it is the same failure with one difference that matters: the port
reproduced a bug, recorded a measurement that made the reproduction
feel safe, and the measurement was of the *data* the bug depended on
rather than of what Python does. Keeping the count honest here is the
point — "the one place" is a claim about how often the method fails,
and it had stopped being true.

`content_blobs.content_type` is documented in the schema as `'text' or
'binary'` and is not two-valued. Alongside 4,219 `text` and 35 genuinely
binary rows the live database holds **9 `text/plain`, 5
`text/x-shellscript` and 2 `binary_asset`**. An exact `content_type =
'text'` test therefore calls 14 ordinary source files binary. Both
implementations had that test; this port reproduced it deliberately,
because it was on stdout and the README said so, filed it as handoff
#20, and wrote in `blob_text`'s doc comment that "the two
implementations have to agree until it is fixed there".

It was fixed there. Python's `_get_content_by_hash` now decides on the
data — `content_text is not None` — and returns one of `text`, `binary`
or `missing`, and `vcs show` prints a different line for each. So
reproducing the old test stopped being the parity and became the
divergence, and the port followed. The decision is now
`content_text IS NOT NULL`, with `content_blob IS NOT NULL` telling
binary from a hash that names nothing. `content_type` says what the file
*is*, not which column holds it.

**What it was costing**, and neither symptom was subtle:

| command | before | after |
|---|---|---|
| `vcs show templedb 40E93339139FA809` | `No differences` three times, under `✨ added src/services/env_logic.pl (125 lines)` | the three diffs, 364 lines |
| `vcs diff templedb src/gui_pages/README.md <c1> <c2>` | exit 1, `Could not retrieve old version` | the 9-line addition Python prints |
| `vcs show bza FBC7ECF45AE34942` | `No differences` ×24 for 24 changed SVGs | `binary file, no textual diff` ×24 |
| `vcs show nixos_anywhere_vultr_woofs 1C04E7EF81BD7A3C` | `No differences` ×68 | `content not stored for this revision (hash present, blob absent) — cannot diff` ×68 |

The last two are the part that is not just "the bug is gone". Python
grew two new messages with the fix, because empty-against-empty rendered
"unchanged", "binary" and "the bytes are gone" identically — three
different facts and one output, right for one of them. Those messages
are ported, which is why `BlobKind` exists rather than
`Option<String>`: `None` cannot answer *why*.

**How it was found is the point.** Not by reading Python again — by the
parity harness for an unrelated feature. The 200-invocation sweep built
for `--side-by-side` failed 4 cases, all one file, and all four failed
on the *unified* renderer too. A harness that only exercised the thing
being added would have passed.

`file cat` and `source snapshot` read blobs through their own queries
and were checked against Python on all three live misclassified files;
they already agreed, so the fix stayed in `vcs.rs`. The fixture gained a
`text/x-shellscript` blob holding real text and one with neither column
populated, because no row in it could reach either case — the fixture
labelled all its text `text`, so the old and new tests passed
identically on it.

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

### The first write

Landed 2026-10-21. `templedb-rs file set <proj> <path>`, step 15.

Chosen by the design document over `vcs commit`, which it calls "the
widest door in the system — workspace scan, three guards, conflict
detection with resolution strategies, session head bookkeeping, commit
metadata, and per-file writes across four tables". This is one logical
change across three tables, and its whole effect is visible through
read commands the sweep already verifies byte for byte.

**It writes Postgres, never the live SQLite.** Python keeps sole
authority over `~/.local/share/templedb/templedb.sqlite`; the port
writes the scratch database it owns, so a wrong write costs a
`just mirror-refresh`. Two environment variables and not one:
`TEMPLEDB_WRITE_URL` names the scratch and `TEMPLEDB_URL` names the
parity target, which only `mirror` may write because it opens with
`TRUNCATE`. Isolation by connection string rather than by
`search_path`, because a wrong database name is loud and a wrong
`search_path` is silent, and the operation on the other side of that
mistake truncates eleven tables.

**It is opt-in.** `TEMPLEDB_RS_ALLOW_WRITES=1`, as the design document
asks. Not caution for its own sake — the command is partial in ways a
command line cannot show, and the opt-in is what stops a script
reaching it by habit.

#### What it does not do, in full

The design document's sharpest finding is that **the guards are the
deliverable**: "a port that reproduces the SQL and not these is a
regression even if every byte of output matches." This reproduces the
SQL and **six of the seven guards**. The rest are absent and named,
because an unnamed omission in a write path is how the incidents in
that document happened:

| absent | what it was for |
|---|---|
| the `--if-match` three-way rebase, and `--no-rebase` with it | Python merges a stale base onto what landed instead of refusing, and only a genuine overlap exits 2. **Refused rather than missing**, decided 2026-11-18: the `ThreeWayMerge` it uses is not a three-way merge, and agrees with `git merge-file --diff3` on 3 of 11 measured cases. `--if-match` in this port is the strict compare-and-swap Python's `--no-rebase` selects. See *The rebase this port refuses to do* |

**The table used to have two more rows and both closed on
2026-10-29.** `--commit` / `-m` / `--commit-msg` is ported, in one
transaction rather than Python's two — see *`--commit`, and why it is
one transaction* — and so is binary content, which was the reason
`.authinfo.gpg` could be stored over there and not here. The row that
replaced them arrived from the other direction: Python grew the rebase
after this section was written, and the sweep's flag-surface stage is
what reported it.

`--session <ID>` **is** here, as of 2026-10-23, and it is an id and not
a name: Python accepts `ID|NAME` and its name path creates the session
when none matches. It outranks `TEMPLEDB_SESSION_ID`, which is Python's
precedence, and it is resolved against the write target inside the write
transaction — the same reasoning as the exact-slug resolution, one
column over. Verified at the command line: a live id lands on the
intent, the flag wins over an env var naming a dead session, an ended id
exits 1, and a name is refused as a name rather than as a parse error.

Every one of those flags is listed individually in the sweep's
flag-surface allow-list rather than waved at, so porting one shows up
as a change. That is not hypothetical, and it has now happened four
times: **`--verify` and `--allow-mojibake` were on this list for
exactly one commit**, `--skip-intent` left it on 2026-10-23, and
`--stage` / `-s` on 2026-10-27. The list is how all of them got
picked up.

It also has to be pruned by hand, which is worth stating because the
mechanism does not say so. The allow-list suppresses *differences*, so
a flag that becomes present on both sides stops producing one — the
entry then suppresses nothing and reads as a divergence that still
exists. Removing `-s` and `--stage` left the sweep's count unchanged at
20 of 20 commands agreeing, which is the check that they really do
match now rather than being excused.

The six guards that *are* here:

- **`--if-match`.** `file set` replaces the whole file, so content read
  earlier is a lock on all of it: anything written in between is
  reverted silently, with no conflict and a clean `--verify` — which
  only checks that what you wrote is what landed, not that you meant to
  drop the rest. That happened twice on 2026-10-04 to a 5,239-line file
  four fixes were landing in, and the first instance went unnoticed for
  three commits. A prefix of 8 hex characters is enough, because every
  surface that displays a hash truncates it.
- **The un-delete.** `file set` on a soft-deleted row revives it.
  Without it the blob lands and everything filtering on
  `status = 'active'` skips the file, which is how `set bza flake.nix`
  once looked like a success and shipped nothing.
- **The mojibake guard.** UTF-8 read as Latin-1 and written back
  doubles every non-ASCII character — an em dash's `E2 80 94` becomes
  `C3 A2 C2 80 C2 94` — and the result is *still valid UTF-8*, so every
  decode accepts it, the hash matches and `--verify` passes. `bza`'s
  `frontend/app/page.tsx` shipped that way for two weeks before anyone
  noticed a button reading `â   done` instead of `✓ done`. The cause
  was a `perl -0pi -e` one-liner, not TempleDB, which is why the only
  place to catch it is where content enters the database. Refuses with
  exit 3 and names what the sequences decode *back* to, because
  "this looks like it should be ✓" is the only actionable form of the
  message.
- **`--verify`.** Reads `is_current` back after the write and refuses
  if it is not what was written. On **a fresh transaction after the
  commit**, deliberately: inside the writing transaction it would read
  its own uncommitted effect and could only ever agree, which is a
  check that cannot fail and therefore is not one.
- **The `edit_intents` record**, landed 2026-10-23. Provenance: who set
  this, when, from which session, and *against what*. See below — it is
  the one guard where the port's structure departs from Python's rather
  than its values.
- **The auto-stage**, landed 2026-10-27. Writes the
  `vcs_working_state` row that `vcs commit-staged` reads, carrying the
  **new** hash. Python's comment above the same INSERT is why this is a
  guard and not a convenience: "`vcs commit` reads content via
  `ws.content_hash`, not `file_contents`. Without this, `file set`
  followed by `vcs commit` silently records the previous content." See
  *The auto-stage* below for the one divergence it carries.

#### The intent record, and the one place this is not Python's shape

Guard five, and the `edit_intents` table is the nineteenth mirrored one.
The values are Python's: `description = 'file set'`,
`status = 'applied'`, `patch_summary` a *byte* count, `author` from
`TEMPLEDB_AUTHOR` then `USER` then NULL, and `base_revision` the
previous content hash or the literal `'new-file'`.

That last column is the interesting one, because it spent most of its
life holding nothing. It was the literal `'current'` on all 886 rows
migration 123 catalogued — which is why that baseline lists it as a
`constant` — so **the field designed to record what an edit was made
against never recorded anything**, and the stale-base overwrite of
2026-10-04 left no trace to audit. It records the real base now, on both
sides. `'new-file'` rather than NULL when there is no prior content,
which is Python's choice and the right one: it keeps "there was nothing
here" distinguishable from "nobody filled this in".

**The insert is inside the write's transaction, where Python's is after
it.** This is the only place the port departs from Python's *structure*,
so the reasoning is worth stating.

Python records the intent after the write, in its own statement, and
swallows every error — "intent recording must not break the write path".
The ordering is load-bearing over there and its comment says why: the
previous arrangement wrote the intent *first* and caused the 2026-09-14
*intent applied but `file cat` returns nothing* bug, where a failed write
left a `status = 'applied'` row that survived the abort and lied to every
later provenance query.

This port has something Python does not: the write was already one
transaction across three tables. Putting the intent inside it makes that
bug **unreachable by construction** rather than by ordering — either the
write and its provenance both exist or neither does, and no sequence of
failures can produce the lying row. The cost is the property Python chose
instead: here an intent failure *does* fail the write.

That trade is the right way round for this command. A silent provenance
gap is exactly what the 2026-10-04 overwrite had, and `file set` is
opt-in, partial and behind an environment variable — a caller who wants
the write without the record says `--skip-intent`, which is louder than
discovering later that the row is missing. A test asserts the refused
case directly rather than trusting the claim: a stale `--if-match`
leaves the row count unchanged.

One divergence in the value, not the structure. **Python's intent path
does not resolve a session** — it reads `TEMPLEDB_SESSION_ID` directly
and swallows a parse failure to NULL, so it will record an id for a
session that has ended. This port has one resolver and it checks
liveness, so the column holds a live id or NULL. Verified at the command
line: an unpinned write records NULL, a pinned one records the id, and a
pin naming an ended session is refused before the write rather than
stamped into the provenance.

#### The auto-stage

Guard six, landed 2026-10-27 as step 21, and the write that closes the
loop: `vcs commit-staged` has read `vcs_working_state` since 2026-10-25
and nothing could write it.

The section below is the argument that deferred it, kept because it is
still the reason the design is the shape it is rather than a record of a
wrong turn. The part that *was* wrong is the conclusion, and what
changed is named at the end: the stated blocker was "this table has no
reader here", and step 20 gave it one.

It was going to be guard six then too, and reading the code changed the
question. The README carried it as a decision — *does an unpinned
`file set --stage` refuse, or stage with a NULL `staged_by_session_id`?*
— and both halves of that turned out to be the wrong thing to be
deciding.

**`--stage` is message-only in Python.** The auto-stage is
unconditional: `_write_content_to_db` upserts a `vcs_working_state` row
with `staged_by_session_id` on every write, and `--stage` changes
`✓ Set <path>` to `✓ Set and staged <path>` and nothing else. A
follow-up `stage_files()` is deliberately skipped, because
`_refresh_ws_row_from_disk` re-reads the checkout and would revert the
row's hash to the pre-write one. So porting `--stage` as a flag that
"requires a session" would make it *stricter* than a flag that does
nothing over there, and the thing that actually needs a session is a
side effect of every `file set`.

**And a NULL owner is not an ambiguous state, it is the wrong one.**
`staged_by_session_id IS NOT NULL` *is* the staged flag — migration 083
dropped the `staged BOOLEAN` compat mirror in its favour, and
`unstage_files` is literally `SET staged_by_session_id = NULL`. So the
proposal came to "stage the row by writing the value that means
unstaged", and five readers agree on that reading:

| reader | predicate | a NULL-owned row |
|---|---|---|
| `vcs status` | `staged = [... == sid]` | listed as **modified**, never staged |
| `vcs commit` | `WHERE staged_by_session_id = ?` | never committed |
| `templedb publish` | `COUNT(*) ... IS NOT NULL`, else skip the commit | **silently ships nothing** |
| the importer's refresh | deletes every row, restores only `IS NOT NULL` | deleted on the next sync |
| `vcs session reconcile` | default `orphan_stages` → `SET ... = NULL` | *produces* this state, to mean unstaged |

The publish row is the one that settles it, because it is the
`set bza flake.nix` incident again — the write reports success and the
publish ships nothing, which is exactly what the un-delete guard exists
to prevent. The importer row is the sharpest, because that preservation
snapshot was written *for this command*: its comment says the rows it
saves are the ones "written by `templedb file set` + auto-stage", since
otherwise a disk re-scan replaces the intended blob and `vcs commit`
commits stale on-disk content. A NULL-owned stage is the one row that
mechanism cannot save.

Python already has a word for it. `commit.py` calls these rows "the
orphans this path and the stale-session reaper both produce… precisely
the ones that were never getting cleaned up", and 133 live rows are in
that state today (102 `added`, 28 `deleted`, 3 `modified`).

**So the answer is neither.** The auto-stage is not deferred for want of
a session — it has one now — but because `vcs_working_state` has no
reader here and its whole purpose is to be consumed by `vcs commit`.
Writing it would be the first time this port mirrored a table for
nobody, and unlike `edit_intents`, where the row *is* the product, a
working-state row that nothing reads is not provenance, it is litter.
It also has a second half the port has no equivalent for: Python mirrors
the write into the active checkout, and its comment records what a
missed mirror costs — the refresh classifies a staged file with no file
on disk as `deleted` and the next commit hard-deletes it, which
announced "Files: 2" and committed one on 2026-09-24.

The auto-stage therefore lands *with* `vcs commit`, as part of it, and
the open decision is closed by having become irrelevant.

##### What landed, and the one thing the argument above got wrong

The reasoning held and the conclusion expired. `vcs commit-staged`
landed two days later and the table has a reader, so the litter
objection — the only thing deferring the work — is gone. Nothing else in
the section changed: the state is still owned by a session, a NULL owner
still means unstaged, and the checkout mirror still has no equivalent
here.

So the auto-stage **is** unconditional, as Python's is: every `file set`
with a session pinned upserts the row, carrying the hash just written.
What it cannot do is invent the session, and that is where the one
divergence sits.

| | Python | here |
|---|---|---|
| no session named | resolves one, **creating** the row if needed, and stages | stages nothing and says so on stderr |
| `--stage` / `-s` | changes `✓ Set <path>` to `✓ Set and staged <path>`; nothing else | **refuses the write** before anything is written |

The second row is the port being *stricter* than the original, which
this README argued against when the question was first asked — "porting
`--stage` as a flag that requires a session would make it stricter than
a flag that does nothing over there". That objection loses its force
once the auto-stage exists, because the port then has two outcomes to
distinguish and Python has one. A flag whose whole content is "do not
let this one pass quietly" is the same bargain `--verify` already
strikes one column over: the write happens either way, and the caller
who needs the *effect* rather than the attempt says so.

The alternative was to make a session mandatory for `file set`
outright, which is the strictest honest reading — a `file set` that
cannot stage is not quite the command Python has — and it was rejected
for costing a capability the command has had since it landed: writing
with no session identified at all, which records a NULL-session intent
and is what every write test and every ad-hoc use has done. Taking that
away to prevent a mistake nobody has made yet is the wrong direction for
an opt-in tool.

The unstaged case is therefore **loud rather than silent**, which is
the choice the `(not ported)` panes in `summary` already made for the
same reason. stdout stays the hash; stderr gains one clause:

```text
$ templedb-rs file set cli-demo src/a.rs --content 'fn a() {}'
509a0a5b5ce4e59f…
cli-demo/src/a.rs: 10 bytes, 2 lines (created, intent #50, not staged: no
session (pass --session, or -s to refuse instead))
```

Silence there would read as success, and the sentence after it is
`nothing staged for commit` — or, through `publish`, nothing at all.
That is the `set bza flake.nix` shape for the third time in this
command's history, which is the whole reason the line exists.

Two details of Python's are reproduced rather than tidied, and one of
them is a bug this port is now carrying on purpose; both are in *Three
findings from porting the auto-stage* below.

The `project_files` row existing is what decides `added` against
`modified` — not whether the content changed, and not whether the row
was active. So re-writing identical content stages `modified`, and so
does reviving a soft-deleted file. Both are Python's answers and both
are the right ones for the commit downstream: a file with history is
not an addition.

**Verified** by nine tests in `write_parity.rs` and by the loop end to
end: `file set -s --session N` on a new file, then
`vcs commit-staged -m … --session N`, read back through `vcs log`,
`vcs show` and `file cat` — with the committed `vcs_file_states.content_hash`
asserted equal to the hash `file set` staged, which is the column
Python's comment warns about, checked from the far end. The staged row is
`unmodified` and owned by nobody afterwards, and the branch head points
at the new commit.

#### Three findings from porting the auto-stage

**All three were fixed upstream on 2026-10-27**, the day they were
found, in `templedb` commit BF4E6BEA6BD93C6E — with five regression
tests, four of which fail against the pre-fix sources. So this section
records what porting the statement a second time turned up, and not a
list of things still wrong; each entry says where it stands now.

That is the method working as intended rather than a coincidence of
timing. The port reproduced one of the three for parity and has now
dropped the workaround, which is the shape the README has claimed for
this project since *The audit*: copy the behaviour, report it, delete
the copy when the authoritative writer is fixed. Nine of the twelve
bugs in that section went the same way.

They are here because the only way to find any of them was to write the
same statement twice.

**The un-delete guard worked by accident.** `_write_content_to_db`
decides whether to revive a soft-deleted row with
`file_record.get('status') != 'active'`, and `get_file_by_path` — the
query that produced `file_record` — **did not select `status`**. So the
test was `None != 'active'`, true on every write to an existing file,
and the `UPDATE project_files SET status = 'active'` fired
unconditionally. The guard did its job, for a reason unrelated to the
one its comment gives, at the cost of one pointless `UPDATE` per
`file set`.

The risk was never the wasted statement. It was that the condition could
not be *tightened* without fixing the query first, so anyone reading the
comment, deciding the check looked redundant and removing it would have
silently restored the `set bza flake.nix` bug. Never reproduced here,
because this port reads `status` and branches on it — which is also what
lets it report `revived from deleted` rather than guessing. **Fixed
upstream:** the column is in the projection and the comment now records
which way it failed.

**Python had two branch lookups and they were not the same query.**
`vcs commit` resolves the active branch through `get_active_branch`,
which joins `vcs_branches` and falls back to the default branch; the
auto-stage read `projects.active_branch_id` on its own, with no join and
no existence check. For a project whose `active_branch_id` names a
branch row that is gone, the two steps resolved differently.

Measured while writing the regression test, the failure is worse than
"stages in the wrong place": the old path staged **nothing**.
`vcs_working_state.branch_id` declares a foreign key, so the INSERT with
the dangling id raised `IntegrityError`, and the auto-stage's
`except Exception: pass` swallowed it to a debug line — `file set`
printed `✓ Set <path>` and exited 0 with no staged row anywhere, and the
next `vcs commit` said "nothing to commit". Two successes and the edit
is gone. (With FK enforcement off, which several bulk paths use
deliberately, the dangling row lands instead and the commit still cannot
see it. Same outcome, different mechanism.)

The port prevents this structurally rather than fixing it: `file set`
and `vcs commit-staged` call one `active_or_default_branch`, so they
cannot disagree about which branch a write belongs to. **Fixed
upstream** the same way — the auto-stage now calls `get_active_branch`,
with a comment saying that if it ever needs its own query again,
`vcs commit`'s has to change in the same commit. This is the second time
commit's own measurement turned up a divergence between two copies of
one piece of logic; the first was the parent edge, written twice from
two different resolutions.

**The `intent_id` link was sticky, which made it wrong on the second
write.** Migration 088 added the column so a provenance query can join a
staged row to the intent that produced it, and Python filled it in a
separate statement guarded by `AND intent_id IS NULL` — so the row kept
the **first** intent that ever staged it. After a second `file set` the
row's `content_hash` was the new write's and its `intent_id` named the
older intent: the column added to record which edit produced this staged
content, pointing at an edit that did not.

This is the one of the three the port reproduced, with a `COALESCE` that
is the same rule in one statement. **Fixed upstream, and the workaround
is gone** — `intent_id = excluded.intent_id` now, and the write test's
assertion flipped with it from "keeps the first intent" to "names the
intent whose hash it holds". One commit of divergence, which is what
"copy, report, drop the copy" costs when the report is acted on.

Python's statement had a second, smaller problem the single upsert never
inherited: it was scoped by `project_id` and `file_path` and **not by
branch**, so on a project with the same file staged on two branches it
relabelled both. The port's `ON CONFLICT (project_id, branch_id,
file_id)` touches only the row it staged — a case where reproducing the
statement *shape* would have taken the bug along with it. **Fixed
upstream** by having `_write_content_to_db` return the file id and the
branch it staged against, which also retired a
`file_id = (SELECT … LIMIT 1)` subquery that was re-deriving a value the
writer already had.

#### The refusal codes, which are the contract

Caught the day after the command landed, and it was a real divergence
rather than a nicety. Python returns **2** for an `--if-match`
mismatch and **3** for a mojibake refusal; the first version of this
returned 1 for both, because every refusal was a plain `bail!`.

That matters more for a write than for a read. The design document's
section on verification lists *refusal parity* as one of three
techniques replacing the byte diff — "every guard refuses with a
specific exit code and message; for a write, the refusals are the
contract" — and closing the error-exit gap was made a precondition for
exactly this reason. Shipping a writer whose refusals are
indistinguishable from a missing database would have undone step 12.

So `exit.rs` gained two constructors beside `Coded::not_found`:
`conflict` (2, for `--if-match` and `--verify` — the database moved
under you) and `refused` (3, for a content guard). Both reuse numbers
the read commands already use for other meanings, and that reuse is
Python's rather than this port's invention. A malformed `--if-match`
argument still exits 1, which is also Python's split: a bad flag is bad
input, a mismatched one is a conflict.

Verified at the command line against Python's documented codes: 3 for
mojibake, 2 for `--if-match`, 0 for a `--verify` that passes.

#### Two things the read subset could not do

**`id` had no default.** The schema was built for `mirror`, which
copies rows that already have ids, so every `id` is a plain
`bigint PRIMARY KEY`. A writer has nothing to generate one from. So
`project_files.id`, `file_contents.id` and — since the intent record —
`edit_intents.id` are now
`GENERATED BY DEFAULT AS IDENTITY` — `BY DEFAULT` and not `ALWAYS`,
because `mirror` must keep supplying explicit ids and `ALWAYS` would
reject its `COPY`. The consequence is that `COPY` does not advance the
sequence, so a freshly mirrored database would hand out id 1 and
collide; `mirror` now calls `setval` on each of those sequences inside
the copy's transaction. Inside, unlike `ANALYZE`: a mirror whose
sequences are behind is not merely unoptimised, it is one the first
write breaks. `edit_intents` makes that visible rather than merely
true — its id is the one thing `file set` *prints* from the table, so a
sequence left behind shows up in the output.

**Resolution is exact, not fuzzy.** Every read command takes a fuzzy
project name and Python's `file set` does too. This takes an exact
slug, which is the one place the port is deliberately *stricter*: a
reader that guesses wrong wastes a second, a writer that guesses wrong
overwrites a file — and the scoring function has two known bugs wrapped
around it. The slug is also resolved *inside the write transaction*,
against the write target, because the reader and the writer point at
different databases and resolving against one while writing to the
other would be a genuinely nasty bug.

#### How it is verified

Not by a byte diff, and it never will be: Python writes SQLite and this
writes Postgres, so there is no shared input. The technique is the one
the design document calls **cross-read** — the port writes, and the
already-trusted read layer reads it back.

`crates/templedb-db/tests/write_parity.rs` — twenty-three of its 36
tests are this command's, `#[ignore]`d and run by `just write-test`. The
read-back goes through
`templedb_db::read_file` — the actual ported `file cat` query, not SQL
written for the test — so a write `file cat` cannot see is a failure.
That mattered immediately: the first version used a hand-written query
without `status = 'active'`, and the un-delete test passed against a
soft-deleted row while asserting nothing.

They stay out of `cargo test` for the same reason the two CPython
oracles do, which preserves the property this file advertises: **no
Postgres server is needed to build or to `cargo test`**. The design
document listed replacing the in-memory SQLite fixture as the one
question blocking this work; it is a *cutover* question, and step 15b
says why.

What the first fourteen cover: creating a file, replacing one in place
through the `ON CONFLICT (file_id, is_current)` arm, a no-op rewrite,
the un-delete, empty content, `--if-match` letting a match through *and
refusing a stale base without writing*, content containing a NUL,
`--verify` agreeing after a write *and catching a row reverted behind
the writer's back*, and the mojibake guard refusing through the write
path with `--allow-mojibake` letting the same content past.

Five of them are the intent record, added with it on 2026-10-23: that
every write leaves a row with Python's values, that `base_revision`
chains — `'new-file'` then the previous hash — that `patch_summary`
counts bytes and not characters (asserted with multibyte content, the
only input that tells the two apart), that `--skip-intent` writes the
file *and* records nothing, that a refused write leaves no row at all,
and that a pinned session is stamped while an ended one is refused
before the write. The provenance rows are read back through `Store`
rather than the writer's own connection, which is the cross-read rule
applied to this table too: the write is only verified if something else
can see it.

Nine more are the auto-stage, added 2026-10-27: that a write with a
session stages the **new** hash on the active branch as `added`, owned
by that session and linked to that intent; that a second write restages
`modified` and keeps the first intent, which is the stale link reported
above; that a no-op rewrite and a revived soft-deleted file both stage
`modified`; that a write with no session, and a project with no branch,
each stage nothing and report which; that `--stage` with no session
refuses *and leaves no `project_files` row at all* — the test is the
ordering, not the message — that `--stage` with no *branch* does not
refuse, because the flag asserts ownership and not the existence of a
branch; and that a second session staging the same file takes the row
over rather than adding one, since the row is
`UNIQUE (project_id, branch_id, file_id)`.

These nine are the one group in this file with no cross-read available:
no ported command reads `vcs_working_state` except the commit, which
consumes it. So they read the row with SQL, and the loop test in the
commit section is what verifies it through a reader — the same division
`vcs_file_states.file_path` gets, and stated for the same reason.

Four of the first fourteen are written to be able to fail, which is the
only kind worth having: the `--verify` test reverts the row with its own
`UPDATE` before checking; the mojibake test corrupts a string the way
the incident did — encode to UTF-8, read the bytes back as Latin-1 —
rather than hand-writing a sequence that happens to match the pattern;
the refused-intent test asserts the row count is *unchanged* rather
than merely non-zero; and the session test creates a real ended row to
be refused rather than naming an id that happens not to exist.

That last one ties the write path to `pgtext`: the escape had only ever
been exercised by `mirror`, and a writer inserting raw text would be
rejected for exactly the reason the mirror was. The hash is taken over
the **original** bytes, not the escaped form, so `file cat | sha256sum`
still equals the blob's own key — verified at the command line, where
`file set` prints a hash matching `sha256sum` of the same content.

### The second write, and the command that is not `vcs commit`

Landed 2026-10-25. `templedb-rs vcs commit-staged <proj> -m <msg>
--session <id>`, step 20.

The design document — *vcs commit: the widest door, measured*,
2026-10-24 — measured the door and recommended **against** porting it:
seven tables, nine guards, one trigger, and one dependency that "is not
a translation problem but a question about what the port is for". Five
of the nine guards compare a filesystem against the database. Porting
them means porting the workspace scanner, the checkout-role resolver
and the mirror-to-disk step, which is a different project.

So the recommendation was to split the door: port the three-quarters
that is database work, refuse the quarter that reconciles a filesystem,
and do it "under a name that does not promise the other half". That is
what this is.

#### Why the name is not `vcs commit`

This was the document's one explicitly open question — "whether the
reduced command may be called `vcs commit`… it should be settled before
the name is used rather than after" — and it is settled by a precedent
this port already set. The `--no-fts` argument is that **a flag is only
harmless when the thing it names is present**: a flag promising
different matching that delivers none is worse than an absent flag,
because absence is visible and a no-op is not.

A command name is the same argument at a larger scale. `vcs commit`
over there means *scan the tree, reconcile it against the database,
resolve conflicts, commit*. This cannot scan. Sharing the name would
make `--amend`, `--force`, `--branch`-with-a-tree, `--strategy` and the
whole workflow into things the command refuses rather than things it
does not have.

The name also describes the operation more accurately than "a reduced
`vcs commit`" does. The document's own sentence: "That is not a reduced
`vcs commit`; it is the operation `publish` already performs — its guard
is `COUNT(*) … WHERE staged_by_session_id IS NOT NULL`, and it commits
without scanning anything."

One concrete benefit: the sweep's flag-surface stage has nothing to
compare against, because there is no `templedb vcs commit-staged
--help`. That is the honest outcome. Under the shared name the stage
would have reported four or more absent flags as divergences on a
command that cannot have them, and the allow-list would have had to
grow a permanent exception for each.

#### The trigger, which is the sharpest porting problem

`update_branch_head_on_commit` is an `AFTER INSERT` trigger on
`vcs_commits` that sets `vcs_branches.head_commit_id = NEW.id`. **This
port has none of the live database's 61 triggers.** So a faithful port
of the *code* leaves the branch head frozen forever — the one place
where reproducing Python's statements and reproducing Python's
behaviour come apart, and a case the sweep could never catch because
`vcs branch` would keep printing whatever the mirror copied.

The fix is to do the `UPDATE` in application code, and Python's own
comment is a warning about exactly that:

```text
-- Branch head is auto-advanced by the `update_branch_head_on_commit`
-- trigger ... so no manual UPDATE is required here. The previous
-- app-level compare-and-swap was buggy: it always fired a
-- false-positive HEAD_RACE error because the trigger had already set
-- head_commit_id to the new commit id by the time the CAS's
-- WHERE head_commit_id IS <old> ran.
```

That is a bug *caused by the trigger existing*. An implementation with
no trigger can do the compare-and-swap correctly, because nothing has
moved the column underneath it — and the comment's own closing
sentence, "if cross-transaction race protection is needed, drop the
trigger and do the UPDATE-with-CAS inside `create_commit()`", describes
the situation this port is already in. **It is the first place the port
is better rather than merely faithful**, and it is not a liberty taken:
without it the command does not work at all.

`IS NOT DISTINCT FROM` and not `=`, because the expected head is NULL
for a branch with no commits and `NULL = NULL` is unknown — `=` would
refuse every root commit. The CAS runs where the trigger would have
fired, immediately after the commit row, so a `HEAD_RACE` refusal costs
nothing and happens before any per-file work. Same reason `--if-match`
is checked first in `file set`.

It is a genuine compare-and-swap under Postgres's default
`READ COMMITTED` rather than an optimistic guess. A concurrent
transaction that has advanced the head blocks this `UPDATE` on the row
lock; when it commits, the predicate is re-evaluated against the new
row version, matches nothing, and the refusal fires.

**The cost, stated:** every cross-backend comparison so far has compared
SQLite against Postgres *after a copy*. `vcs_branches.head_commit_id` is
now the first column whose contents are produced by different
mechanisms on the two sides.

#### What is refused, and where

| refused | what Python does there |
|---|---|
| a workspace scan | reconciles staged paths against the resolved tree |
| guards 4, 5, 6, 8 — version conflict, intent conflict, the snapshot refinement, `--allow-mass-delete` | all four compare a tree against the database |
| `--strategy` and its resolution strategies | settle conflicts this port cannot detect |
| `--amend` / `--force` | a different operation on one column; it is not staged-rows work |
| a staged hash naming no blob | resolves the path, joins it onto `~/.config/templedb/checkouts/<slug>`, reads the bytes off disk and stores the blob |
| a staged state of `'conflict'` | writes it, and the live `change_type` CHECK rejects the row |

The last two are the ones worth reading twice.

**The missing blob is where the tree half actually bites**, and it is
why the split is a split rather than a trim. Python leaves the database
at that point. This port refuses by name, with the hash it could not
find and a pointer at `vcs status --refresh`, which is the command that
stores the blob.

**The `'conflict'` refusal is this port being stricter than Python on a
value.** `detect_changes` can produce that state, and the live
`vcs_file_states.change_type` CHECK allows only `added`, `modified`,
`deleted`, `renamed` and `unmodified` — so Python's own INSERT is
rejected by the constraint. The subset schema carries no CHECK, by the
standing rule that a mirror must accept what the live database holds,
which means this port would *silently* write a value the live schema
forbids. A row that can never go home is worse than a refusal.

#### What is ported, and the three guards that came with it

Six tables written in one transaction, in Python's order: `vcs_commits`,
`vcs_commit_parents`, `vcs_file_states`, `file_contents`,
`project_files` (a `DELETE`) and `vcs_working_state`. Plus
`vcs_branches`, by the compare-and-swap above.

**`vcs commit-staged` is the only path in the system that hard-deletes
a `project_files` row.** Everything else soft-deletes by flipping
`status`, which is why `file set` needs an un-delete guard at all.

- **Guard 2, the orphan reap.** `vcs_working_state.file_id` declares an
  `ON DELETE CASCADE` FK live, but `sync_engine` and some migrations
  bulk-delete with FK enforcement off, so rows pointing at a reaped
  `project_files` row are a state the live database reaches. The
  `vcs_file_states` FK then aborts the whole commit; that happened on
  2026-09-19. Reaped and counted. Python wraps it in
  `except Exception: pass` and this does not — the swallow is there
  because the statement runs outside the commit's transaction over
  there, and here it is inside one, so an ignored failure rolls the
  commit back anyway.
- **Guard 3, the content-hash refusal.** A staged non-deleted file with
  no hash can only record the `'DELETED'` sentinel, which says *this
  state names no content* — true for a deletion and a lie for anything
  else, and unrecoverable, because no blob was stored under the hash
  that is missing. 124 live rows were written that way before the guard
  existed, and they feed 501 `source_snapshots` rows pointing at a
  fabricated blob that claims 8,059 bytes and holds nothing. Exit 3.
- **Guard 9's shape.** The session is resolved at the entry point and
  threaded in, never read inside the transaction. Python's incident was
  that session *creation* commits, which ended the caller's transaction
  — so a later per-file failure rolled back the file states but could
  not undo the commit row, leaving a commit with zero `vcs_file_states`
  in the log and a broken parent chain. Reproduced on demand
  2026-10-03. This port cannot hit it, because it never creates; the
  shape of the fix survives anyway, and costs nothing.

Guard 1 (`_has_write_tree`) and guard 7 (`--strategy force` still
refusing intent conflicts) are preconditions for the tree half and go
with it.

The whole set is checked **before anything is written**, where Python
checks inside the per-file loop after the commit row exists. Harmless
there, because the transaction rolls back — but it means the refusal it
prints is about file four while files one to three looked fine. Checking
first is the same information, earlier.

#### Three corrections to the design document's measurements

Measured against the code while porting, and all three are the document
over-counting rather than under:

1. **`vcs commit` does not write `vcs_commit_metadata`.** The
   document's table lists it. Only `templedb commit <slug> <dir>` does,
   through `create_commit_metadata`. Same for `vcs_session_heads`,
   listed as read/write for publish bookkeeping — `vcs commit` touches
   neither.
2. **It does not set `vcs_commits.session_id` either.** Migration 107
   added the column and described the publish fast-forward, but its
   writer side was only ever wired into the other commit command. So
   every commit `vcs commit` makes is *already published*, which is also
   what makes one visible to `vcs log`'s default view the moment it
   lands. 73 of 1,444 live commits carry a session.
3. **So the table count is six written, not seven** — and the fourth
   mirrored table the work needed was `vcs_file_states.file_path`, one
   column, rather than `vcs_commit_metadata`, which had already landed
   with step 19.

#### `vcs_commits.parent_commit_id` is not written, on purpose

Python writes the parent edge twice: `vcs_commits.parent_commit_id`
inside `create_commit`, and a `parent_order = 0` row in
`vcs_commit_parents`. The column is not in the mirrored subset — no
reader names it, `vcs show` walks the join table — and Python's own
`create_commit` docstring records why keeping both is a hazard:
"keeping them in sync was left to callers and they drifted: `vcs merge`
wrote the join-table rows and never set `parent_commit_id`, so merge
commits read as roots to anything walking the column."

One declarer per fact, and the join table is the declarer. Written once,
from the branch head's id rather than from a hash round-trip — Python
resolves `head_commit_id` to a hash and the hash back to an id, which
can only land on the same row or on nothing, and the nothing case is now
refused explicitly: **a head naming a pruned commit is a refusal**, not
a foreign-key violation three statements later. `vcs_branches
.head_commit_id` carries no FK — in this subset by decision and in the
live schema by omission — so a dangling head is a state both databases
can reach, and `vcs_commit_parents` *does* reference `vcs_commits`.

#### The one column carried for a writer that no reader has

`vcs_file_states.file_path`, migration 118's denormalised path, is the
twenty-first mirrored table's new column and the sharpest case of
"follow the readers" read sideways: **nothing selects it in either
implementation.** `vcs show` resolves paths by joining `project_files`
on both sides.

It is carried because this command hard-deletes that `project_files`
row. Without the column, a committed deletion leaves a `vcs_file_states`
row whose `file_id` resolves to nothing and no record of which file it
was — the exact hole 118 closed. 7,183 of 7,197 live rows carry one; the
14 that do not predate the column, so it stays nullable.

The deletion test asserts the consequence rather than wishing it away:
after a committed deletion, `vcs show` lists **nothing**, because its
reader inner-joins `project_files` and that row is gone. The path
survives only in the column. That is the state Python produces too.

#### The commit id, and the float that is part of it

`sha256(f"{slug}:{branch}:{message}:{time.time()}")[:16].upper()`.
Sixteen hex characters over four fields, and deliberately **not**
content-addressed — the clock is in the input precisely so the same tree
committed twice gets different ids, which is what makes Python's
`--amend` safe.

Uppercase is load-bearing rather than cosmetic: `_amend_message`
refuses to touch a commit whose hash is 40 characters *or lowercase*,
because either shape means the row might be imported git history. A port
minting lowercase ids would produce commits Python refuses to amend.

The float is the fiddly part, and it is the same class of problem as
`difflib` and the clock: Python interpolates `time.time()` through
`str()`, which is `repr` — shortest round-trip, with one rule Rust does
not share. **A float with no fractional part still shows `.0`.**
`str(1761393045.0)` is `'1761393045.0'` and Rust's `{}` prints
`1761393045`. That case is not exotic here, it is the common one under a
pinned clock, because `Clock::at` parses whole seconds. Without the
rule the two implementations compute different ids for the same stated
instant, which is the one thing about this hash that *is* checkable —
and a unit test checks it against a CPython-computed value rather than
against sha256 working.

Resolution is milliseconds, not microseconds, because that is what
`Clock` holds — SQLite's own representation, for the reasons under *The
clock seam*. So the two cannot agree on an id for the same *wall clock*
instant; they agree on an id for the same *stated* instant, which is
what `TEMPLEDB_RS_NOW` pins.

#### What it needs that `file set` did not

**Two more identity columns.** `vcs_commits.id` and
`vcs_file_states.id` joined `project_files`, `file_contents` and
`edit_intents` as `GENERATED BY DEFAULT AS IDENTITY`, and `mirror`'s
`sync_sequences` list grew to five. The commit's id is load-bearing
twice over rather than merely printed: `vcs_commit_parents` stores the
parent edge by id, and the branch-head CAS writes it. A unit test now
asserts the two lists agree in both directions, because they fail
differently and both quietly — a name in the Rust list with no identity
in the DDL makes `pg_get_serial_sequence` return NULL and
`setval(NULL, …)` return NULL, advancing nothing; a table in the DDL and
missing from the list keeps a sequence at 1, and the first write after a
mirror collides on the primary key.

**A way to re-apply the DDL.** `just pg-reschema`. The subset schema is
not static — it has grown a column or an identity every time a write
path landed — and `mirror --create-schema` is CREATE-only, so it cannot
apply a change to a database that already has the tables. The symptom is
remote from the cause: a `mirror` run fails inside a binary `COPY` with
a column-count mismatch. Recreating both schemas from the file is the
right move rather than writing `ALTER`s, because neither database is a
source of truth — parity is a copy and scratch is disposable — so the
DDL file stays the single declaration of the shape. The recipe
re-applies the `templedb_ro` grants, which `DROP SCHEMA` takes with it.

**A session, or a refusal.** This is where *resolve never, accept
always* costs something visible. `vcs commit-staged` requires
`TEMPLEDB_SESSION_ID` or `--session <id>`; Python guesses from the
process tree and creates a row when that fails. The refusal names both,
and when `TEMPLEDB_SESSION` is set it says so specifically — which is
not hypothetical, because the agent wrapper this port is developed under
exports exactly that.

#### How it is verified

Cross-read, as `file set`'s is, and a stronger one: **three** separately
verified readers can see a commit. `vcs log` lists it, `vcs show` lists
its changed files and diffs them, and `file cat` reads the content it
made current. A commit none of them can see did not happen.

`crates/templedb-db/tests/write_parity.rs` is now 36 tests, thirteen of
them this command's, `#[ignore]`d and run by `just write-test`. What
they cover beyond the happy path:

- **The race.** Two transactions on two connections, the second holding
  a head the first has already replaced. Deterministic rather than
  timing-dependent: the first transaction *commits* before the second's
  `UPDATE` runs, so nothing blocks, and under `READ COMMITTED` the CAS
  predicate is then evaluated against the new row version and matches
  nothing. The head is asserted to still be the first commit's
  afterwards — a refusal that half-applied would pass a test that only
  checked the error.
- **Both refusals leave nothing behind.** Guard 3 and the missing blob
  each assert no commit row, no head movement, and the staging still
  staged. A guard that rejects after writing is not a guard.
- **Session isolation, from both sides.** One test commits beside
  another session's staged rows and asserts they survive with their flag
  intact; another asserts that "nothing staged" reports the *other*
  session's count rather than saying nothing — Python's message, and the
  footgun worth keeping, because an agent staging in one tool call and
  committing in another is in two sessions.
- **The deletion**, asserting the hard delete, the surviving
  `file_path`, and that `vcs show` consequently lists nothing.
- **The metrics quirk.** Python re-derives size and line count from
  `content_blobs` rather than trusting the `file_contents` join, and it
  has to: a modified file's staged hash is by definition not the current
  one. The size is a *byte* count (asserted with multibyte content, the
  only input that tells it from a character count) and an **empty**
  `content_text` is falsy over there — so it takes the binary branch for
  the size and leaves the line count NULL rather than 0. Both
  reproduced.
- **NUL content**, which ties this path to `pgtext` the way `file set`'s
  test does: the staged blob is unescaped before it is measured, and
  skipping that overcounts every file holding a NUL. One live file does.
- **The tie-break, exercised deliberately for the first time.** Two
  commits under a pinned clock share a `commit_timestamp`, so `vcs log`
  falls through to `id ASC` — see *The tie-break, and getting it
  backwards first*. Live data almost never has two commits in the same
  second, so a write test is the only place that ordering can be
  checked on purpose rather than hoped for.

- **The loop**, added 2026-10-27 with the auto-stage and the thirteenth
  of these tests. Every other test in this group stages with SQL;
  this one runs `file set --stage` and then `vcs commit-staged`, with
  nothing of the test's own in between, and asserts that the hash the
  commit recorded is the hash `file set` staged. A hand-built row proves
  the commit reads what the *fixture* wrote; only this proves it reads
  what the other command wrote.

What cannot be tested here is the half that was refused. There is no
workspace, so there is nothing to scan and no conflict to detect — which
is the point of the command's name.

#### What the fixture still stages by hand, and why

`file set --stage` landed 2026-10-27 as step 21, so the loop closes and
the SQL staging is no longer the only way to build a staged row. It is
still there, and deliberately: three of this command's refusals are
about states no writer produces. A `deleted` state comes from a
workspace scan, a staged hash naming no blob is the corruption guard 3
exists for, and a `file_id` naming a reaped `project_files` row is what
guard 2 reaps. `file set` cannot make any of them, which is the point of
it — so the guards keep their hand-written fixtures and the happy path
uses the real writer.

The design document's own sequencing had `--stage` first. It landed
second, and the ordering was right for a reason the document did not
have: the commit is what gives `vcs_working_state` a reader, and a
`--stage` with no consumer would have been the litter the auto-stage
argument warns about. Doing it in this order also meant the stage was
one upsert when it arrived, because the session resolver, the clock and
the table had all been built for something else first.

### The one place the two text types are not the same type

Found and closed 2026-10-21. `crates/templedb-db/src/pgtext.rs`.

SQLite `TEXT` may contain U+0000. Postgres `text` may not, and no
encoding, collation or cast changes that — it is a NUL-terminated
representation at the protocol level. The live database contains
exactly one such row and it is not a misclassified binary:
`bza/frontend/lib/suggest/normalize.ts` contains `.join('\0')`, which
is an ordinary thing for a TypeScript file to do. The column really is
text, the classification is right, and there is nothing upstream to
fix.

It appeared between two mirror runs on the same afternoon, which is how
a copy that had worked all week started failing — and it was found by
*verifying the `ANALYZE` fix*, not by looking for it. That is the
second time this week a repair turned up a defect next to it.

**What it looked like.** `invalid byte sequence for encoding "UTF8":
0x00`, from `finish()`, after 104 MB had already streamed, naming no
table, no column and no row. `mirror` now checks the text columns
itself before writing, so the message names the column and the row's
key; that diagnostic survived the fix because the next representation
problem will not be this one.

**The decision, and the three rejected.** Escape on the way in,
unescape in the Postgres backend, so every reader above that layer sees
the original bytes. The alternatives each fail something the mirror
exists for:

| option | why not |
|---|---|
| skip the row | `file cat` fails on Postgres only, **and** `content_blobs`'s digest stops matching |
| move the text to `content_blob` | the port would read a text file as binary; digest also moves |
| `content_text` as `BYTEA` | faithful, but reaches the `LIKE` prefilter, `text_lower` and its trigram index, `file cat` and the diffs |

The digest is the deciding criterion, and it is not a detail: it is
what `statediff` compares and what the write path's verification is
built on. Escaping is the only one of the four that keeps it equal,
because the digest hashes rows read *through* `Store` and the unescape
happens there.

**The encoding.** U+0001 introduces an escape: U+0000 becomes
`U+0001 0`, and U+0001 becomes `U+0001 1`. Two things about that are
load-bearing.

The introducer is escaped *too*. Choosing a "rare" marker and assuming
it never occurs would have been wrong here and measurably so — the live
`content_blobs` has 1 row containing U+0000, 2 containing U+0001, 2
containing U+0002 and 4 containing U+001F. Control characters are not
hypothetical in this data. Escaping the introducer makes the transform
injective however often it appears, so rarity buys small diffs rather
than correctness. Without it, a file that already contained the
sequence `U+0001 0` would decode to a NUL that was never there; a test
pins exactly that.

And text containing neither character is returned untouched, as a
borrowed `Cow`. 4,715 of the 4,718 text rows are byte-identical in the
mirror; three are not. That keeps the transform off the hot path and
bounds the blast radius of a mistake to rows containing control
characters.

**One place it leaks out of storage.** `search content`'s Postgres
prefilter does `LIKE` against `text_lower`, which is
`GENERATED ALWAYS AS (lower(content_text))` — so it is generated from
the *escaped* text, and a search term has to be escaped the same way or
the prefilter silently drops the candidate. Matching and ranking happen
in Rust on the unescaped text, so the final answer would still have
been right; what would have gone missing is a hit. `escape` is a no-op
for every realistic term, which is exactly why this needed finding by
reading rather than by testing.

**And a consequence to re-open later, deliberately recorded rather
than inherited.** This was chosen as a *mirror* decision, where the
cost is that a `psql` session sees the escape in three rows and nothing
reading through this crate does. But the design document's 2026-10-16
decision is that **Postgres becomes primary and the port drops
SQLite** — and at that point the escape is no longer a mirror detail,
it is how the canonical bytes are stored, and every non-Rust consumer
of that database sees it rather than one curious session.

That is a materially bigger commitment than the one actually made here,
and the right time to revisit it is at cutover, when `BYTEA` for
`content_text` stops costing a cross-backend digest — because there
would no longer be two backends to compare. Noting it now because the
alternative is inheriting a mirror-scale decision at primary-store
scale without anyone deciding to. While the port carries both backends
the choice stands; reversing it is cheap, which is the other reason to
write the expiry date down: delete `pgtext.rs`, drop its three call
sites, re-mirror.

**Verified end to end**, not just round-tripped in a unit test:

- `file cat bza frontend/lib/suggest/normalize.ts` is byte-identical on
  the two backends, and the sha256 of the output equals the blob's own
  `hash_sha256` — so the content is not merely consistent, it is right.
  The NUL is present in both.
- Postgres stores U+0001 at offset 10,333 of that row and no raw NUL.
- **All 17 tables digest identically across both backends over 211,617
  rows**, `content_blobs` included.
- `just sweep-pg`: **50 of 50 invocations and 7 of 7 `summary` panes
  identical**, which re-opens the cross-backend axis that this defect
  had closed.
- `search content` returns the same hit counts on both backends, and
  still finds the NUL file by a term that sits after its NUL.

### Flag surfaces, and the four divergences nobody had noticed

Added 2026-10-21, closing the checking half of a gap this file has
carried since the audit.

A diff of outputs can only compare flags that *both* implementations
define, because a flag one side does not know is never passed to the
other. So the sweep — 50 invocations, byte-exact, the thing this port
trusts — is structurally blind to a missing flag. It stays invisible
until a caller hits it, which is how `search content --limit` was
found: by hand, passing it to Python and getting exit 2.

Comparing the surfaces directly asks a different question — *can the
same command line be typed at both?* — and it is the one a script cares
about. `parity-sweep.sh` now extracts the long and short options from
each side's `--help` for nineteen commands and diffs the sets. Reading
help text is less fragile than it sounds, because the only thing both
formats agree on is the part being read: an options section whose
entries begin at the start of a line. It deliberately ignores
everything after the first double space, since both implementations
mention *other* flags in their descriptions —
`-i, --ignore-case  Case insensitive (only with --no-fts)` is exactly
the line a naive grep gets wrong.

It found four divergences on three commands the first time it ran, all
of them on commands reported as byte-identical:

| command | flag | disposition |
|---|---|---|
| `vcs log` | `--published` | **ported** — see below |
| `vcs log` | `--session` | **ported** 2026-10-23 — see *Sessions* |
| `vcs branch` | `-d` / `--delete` | a write; cannot be ported |
| `project list` | `--nix-only`, `--category` | **ported** — see below |

`vcs branch -d` is a write and a read-only port cannot have it. All
three of the others are closed, which is the useful thing about a check
that names a gap rather than describing one: the list was four items long
for about an hour, and the last of the three took two days and a design
document rather than an hour — the flag was never the work.

**The stage's one blind spot is a command that is absent entirely**, and
`vcs commit` is now the case. The stage compares `--help` output for a
list of commands, so it can only see a flag divergence on a command both
sides have; a command only Python has is invisible to it, exactly as a
missing flag was invisible to the output diff. That is a deliberate
trade and it is what the naming decision bought. The alternative —
calling the port's staged-rows commit `vcs commit` — would have made
the divergence *visible* as four or more absent flags, at the cost of a
name that promises a workflow it refuses, and a permanent allow-list
entry for each flag. Absent and named in this file beats present and
hollow; see *The second write*.

**`project list`'s filters** needed three more mirrored columns:
`is_nix_project` and `project_category` on 2026-10-21, then
`project_type` with `--type` on 2026-10-29, bringing the `projects`
projection to nine — and because `statediff` derives its digest from
the same `TableSpec`, every one of them is covered by the
cross-backend digest without a second edit.

They select rows and change nothing else, which is **not** what Python
does. Python runs a separate query under either flag:
`COUNT(DISTINCT pf.id)` over *all* `project_files` rather than the
active ones, `SUM(pf.lines_of_code)` instead of the current snapshot's
`line_count`, and `ORDER BY p.slug` instead of by size. So
`templedb project list` and `templedb project list --nix-only` can
report different file counts for the same project, from the same
database, in the same second. Here the counts are the same whether you
filtered or not, which is the property that makes a filtered list worth
believing.

Python also prints **two tables** under either flag — a six-column
"Nix Projects" one and then the ordinary "Projects" one over the same
rows — because the filtered branch prints without returning and falls
through to the shared tail. Not reproduced; doubling the output is not
a format.

`--nix-only` is `is_nix_project = 1`, as Python has it, not
`IS NOT NULL` and not a truthiness test: the live column is NULL for
most rows and 0 for some, and both mean "not a Nix project". A fixture
row pins each.

`--category` takes Python's five choices as a `ValueEnum`, so a bad
value exits 2 on both with the valid list printed — but the query layer
takes a plain `String`, because the live column has no CHECK constraint
and a sixth category added after this binary was built must still
filter rather than be unrepresentable.

Verified: **all eight filter combinations select exactly the projects
Python selects**, including two that select nothing. That comparison is
a sweep stage rather than a sweep row, for the same reason the summary
panes are — `project list`'s output format is a deliberate divergence,
so the bytes will never match and the slug set is the part that carries
the meaning. Writing that stage is also where this file nearly gained a
false claim: the first version keyed on the dashed rule under Python's
header, which this port does not print, so the port's slugs came back
empty and six of the eight comparisons "failed" against a port that was
correct. The stage now drops table furniture by name, and a sabotage
run — making `--nix-only` a no-op — confirms it reports two failures
and exits non-zero.

**`--published` was accepted 2026-10-21 and `--session` on 2026-10-23.**
The two looked asymmetrical and the asymmetry had an expiry date.
`--published` selected behaviour this port already had, and a script
written `vcs log --published` worked against Python and exited 2 here,
which is the opposite of what a port is for. `--session` asked for the
current session's unpublished commits, which this port could not then
produce, so accepting it would have promised session inclusion and
silently delivered none — the same test `search content --no-fts` fails.
That rule did not change; **the thing the flag names stopped being
absent.** Python marks it `[deprecated no-op — this is now the default]`
and it is now a no-op on both sides, for the same reason on both sides.

`--published` is no longer a no-op either: with a session resolved it
selects a genuinely narrower view, and it carries Python's one-line
`N pending commit(s) in your session` advisory, byte for byte, when the
pinned session has commits on the active branch.

One precedence difference, chosen rather than inherited: Python lets
`--published` override `--all-sessions`, and here the two conflict and
clap refuses the pair with exit 2. Reproducing the override would mean
`--published` silently discarding the one view this port renders
identically to Python's.

### Sessions: resolve never, accept always

Landed 2026-10-23, as `crates/templedb-db/src/session.rs`. The
precondition document is the TempleDB report *Sessions: the identity the
port has been refusing to resolve*, written the same day; this is what
implementing it came to.

Python's `VcsService.get_current_session()` resolves through seven
steps, and **two of them insert a row**:

| # | source | behaviour |
|---|---|---|
| 0 | `--session <id\|name>` | explicit pin; outranks both env vars |
| 1 | `TEMPLEDB_SESSION_ID` | must name a row with `ended_at IS NULL` |
| 2 | instance cache | per-process memoisation |
| 3 | `TEMPLEDB_SESSION` | a declared *name*; **creates if absent** |
| 4 | `os.getsid(0)` | `(author, host, sid)` started within 24 hours |
| 5 | `os.getppid()` | the same lookup on the parent pid |
| 6 | — | **creates** `<host>-<sid>-<short-ts>` |

This port is steps 0 and 1, and only the non-writing halves of them. It
reads `TEMPLEDB_SESSION_ID`, it accepts an explicit integer id, it checks
`ended_at IS NULL`, and it does nothing else. With no session identified
every session-scoped behaviour falls back to the published view and says
so in the header.

The argument against the rest is not that it is hard. It is that **the
four automatic steps are heuristics for "same human, same terminal,
recently", and each one was added because the previous one fanned
out** — step 4 keys on the session leader rather than the parent because
`x=$(templedb ...)` forks a subshell with a fresh pid, and step 3 exists
because agent wrappers spawn every tool call under its own session leader
anyway. A port that reimplements them inherits a tuning problem with no
way to validate it: there is no byte diff for "did these two invocations
agree about which session they were in".

**The obvious middle option is rejected explicitly, because it looks
free.** Doing steps 4 and 5 without step 6 — resolve read-only, never
create — finds a session when one happens to exist and finds none when it
does not, so the same command in the same terminal answers differently
depending on whether the Python implementation has run there in the last
24 hours. Correct, unpredictable, and impossible to test. Refusing to
resolve at all is *worse* in capability and *better* in every property
this port holds.

**`--session <name>` closes permanently**, not until later. The name path
creates when absent; a port that creates session rows has given up the
read-only guarantee the whole architecture rests on. Accepting a name
that happens to resolve and refusing one that does not would be worse
again — a flag that works or writes depending on data. Python classifies
the flag's argument with `str.isdigit()`, so `--session my-agent` is not
a malformed id over there but a different feature, and the refusal here
says so rather than reporting a parse error.

#### What the two origins do differently, reproduced

`--session` is classified by `isdigit()` and `TEMPLEDB_SESSION_ID` is
parsed by `int()`, which is not the same test. `--session -5` is a
*name* to Python and is refused here; `TEMPLEDB_SESSION_ID=-5` is a
well-formed id to Python that matches no row, and is the same here.
Reproduced rather than tidied, because a script that exports the variable
gets the same answer from both. Both are trimmed and an empty value reads
as unset, which is Python's `.strip()` and truth test.

`ended_at IS NULL` is load-bearing rather than decorative. Python's
lookup accepted an *ended* session until 2026-10-07, contradicting its
own error message and `CheckoutRepository.current_session_id()`, which
had always required liveness — the split let staging adopt a dead session
that checkout-role resolution then refused.

Both refusals exit 1, which is Python's: its `ValidationError` and
`ResourceNotFoundError` both leave the CLI decorator as `return 1`.
Verified at the command line across four inputs — an unknown id, a
non-integer, an *ended* id and a negative one — with identical exit codes
and empty stdout on both sides. The two messages are byte-identical on
stderr as well, single quotes included, which is unusual here and worth
having where it is free.

#### `vcs log`'s three views, and the one that was missing

All three now agree with Python. `--all-sessions` always did, because it
consults no session. `--published` is `session_id IS NULL`. The default
is `session_id IS NULL OR session_id = ?`, and it prints Python's header
label — `default view (published + your unpublished)` — plus Python's
`N pending commit(s) in your session` advisory under `--published`.

Measured 2026-10-23: **54 of 54 sweep invocations stdout-identical, with
both implementations pinned to the same live session.** The pin is what
makes these rows comparable at all, and the sweep now sets it itself:
`parity-sweep.sh` reads a live id from Python's own `vcs session list
--active` — the authority on which sessions exist, and the one thing this
script must not create — then searches for a *project* the session owns
unpublished commits in, and appends three rows against it.

That search is the part worth explaining, because the first version of it
was wrong in the flattering direction. It compared the default view's
whole output against the published view's and took any difference as
evidence the session owned commits — but the two *always* differ, in the
header label, so every candidate looked good and the union's right half
was never actually covered. It now greps for an `(unpublished)` line,
which is the only visible evidence that
`session_id = ?` contributed anything. The pair cannot be hardcoded: a
session's commits sink below `-n 10` as a project gains history and
vanish entirely when it is published, so a written-down pair would stop
covering the branch without failing. When no pair exists the run says so
in a line of its own, because three rows will pass having checked less
than they look like they checked.

The remaining difference is one header line, in one direction. Unpinned,
Python invents a session — whose view *is* the published set, since a
fresh session owns nothing — and prints `default view`; this port prints
`published only`. Same rows, honest label.

#### `TEMPLEDB_SESSION` is warned about, not refused

The name variable is what the agent wrapper exports, so this port
routinely runs with it set and will ignore it. Ignoring a variable that
changes the answer is the failure worth being loud about, so `vcs log`
prints a one-line warning on stderr naming the variable and what to set
instead.

A warning rather than a refusal, which was the open question the report
left. Erroring would be loud and correct and would make the port unusable
in exactly the environment it is developed in — this session's own
`TEMPLEDB_SESSION=claude-code-agent-9285` is what the warning fired on
first. The warning is suppressed when a session *was* pinned, because
then Python ignores the name too: steps 0 and 1 both outrank step 3, so
the two agree and there is nothing to report. Warning anyway would train
the reader to ignore the warning.

#### The eighteenth mirrored table, at two of ten columns

`vcs_sessions` joined the subset, and the boundary still follows the
readers: `session.rs` asks one question of it — is the id the caller
named still live — so `id` and `ended_at` are carried and the other eight
columns are not. They belong to `vcs session list` / `show` / `prune`,
none of which is ported, and a column with no reader has nothing to
verify a mirror against.

Still no foreign key from `vcs_commits.session_id`, now that the
referenced table is present. `vcs session prune` deletes ended sessions
and nothing nulls the commits that named them, so a dangling
`session_id` is a state the live database can reach and a mirror
enforcing the constraint would refuse to load it. Same reasoning as
`content_hash`, where 509 rows already name a pruned blob.

Measured after the copy: **all 18 tables digest identically across
SQLite and Postgres over 216,318 rows**, and the cross-backend sweep is
54 of 54 with 7 of 7 summary panes — so the session lookup answers the
same through the mirror as through the live file.

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

### Why `summary` and `doctor` were not next, and what that got wrong

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

**All of that is now built, and the conclusion above was too
pessimistic.** The clock seam landed 2026-10-17 as step 13, the
observation layer 2026-10-18, and the rendering layer and session
identity 2026-10-21. The claim that "a byte diff against Python is
meaningless" turned out to be true of the command and false of its
panes: six of the seven ported panes are byte-identical, including the
one whose ages come from the clock. Ages agree because both
implementations compute them from the same stored timestamps and the
same `julianday` arithmetic, so two runs a second apart agree — it is
only a *minute boundary* landing between them that does not, which a
retry settles. The decision that was open is closed: the sweep compares
panes, and the pane that genuinely cannot be diffed is the one reading
systemd, which is not ported.

What is left of `summary` is the two unported panes, listed under
*Known gaps*.

**`doctor` is no longer unchanged, and the sentence that used to be
here was wrong twice over.** It said `doctor entities` "is still a
write path and still sits behind the write-path design document". The
second half expired when the write path landed in step 15 and nobody
went back to it — the document's three preconditions are done and the
port has a `PgWriter` — and the first half, while true, was never the
obstacle. Measured in step 22: `doctor entities` is 44 checks, 15 of
which read something that is not a database — a figure the 2026-11-05
re-derivation split into 8 engine checks, 8 filesystem checks and 1
that reads argparse, of which only 4 read a tree. See *Why `doctor
entities` is not a command this port can have*. `doctor history`, the
subcommand the same paragraph dismissed as "a single `SELECT` over a
table nothing else needs yet", is ported and byte-identical — and the
table is `invariant_checks`, which `summary` has been reading since
step 14, so that clause had been stale for eleven days when it was
written.

Nothing here renames the binary to `templedb`. The Python one is on PATH
under that name and every script, systemd unit and agent wrapper invokes
it; shadowing it while incomplete would break callers silently. Rename
when it can answer everything the original can.

That sentence is the project's goal statement, and on 2026-10-31 it
became the one this README is consistent with. For a week three other
sections argued the tree-dependent half of the system was out of scope,
which cannot be squared with "everything the original can" — the
original's `materialize`, `commit <slug> <dir>` and `checkout-*` family
are the workflow every agent here runs on. The resolution is that the
tree work is **last, not excluded**; see *Deferred, not refused* and
steps 26–28.

## License

MIT
