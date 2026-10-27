-- TempleDB, Postgres: the objects the Rust port reads.
--
-- SCOPE. This is not the schema. The live SQLite database has 163
-- tables, 45 views, 61 triggers and 361 indexes; this file has twenty-one
-- tables and one view, because that is the entire set the Rust port
-- touches today. The full translation is the next step toward Postgres
-- as the primary store and belongs with the write-path design document,
-- not here: the remaining 142 tables have no reader in this codebase
-- yet, and mirroring data nothing reads would be work with no way to
-- verify it.
--
-- Columns are likewise limited to the ones the queries name, plus the
-- keys the joins and uniqueness constraints need. The omitted columns
-- are not all uninteresting — `project_files` alone has twenty — but
-- several carry foreign keys to tables outside this subset
-- (`file_types`, `commit_files`), so including them would drag the
-- boundary somewhere arbitrary rather than somewhere defensible. The
-- boundary moves when a reader appears and not before: `vcs_sessions`
-- arrived with `vcs log`'s default view, at two of its ten columns.
--
-- Three tables are exceptions to "follow the readers", and they follow
-- the same rule read sideways: nothing *reads* them here, a writer
-- writes them, and the columns carried are the ones that writer
-- supplies. `edit_intents` at twelve of fourteen, `vcs_working_state`
-- at nine of eleven, and `vcs_commit_metadata` at three of forty —
-- where the trim is not a boundary decision at all but the table's
-- entire information content. See each table's own note.
--
-- `vcs_file_states.file_path` is the same exception one column wide,
-- and the sharpest case of it: nothing reads that column in *either*
-- implementation, and it is carried because `vcs commit-staged` is the
-- one path in the system that hard-deletes the `project_files` row its
-- `file_id` points at. See the table.
--
-- Load with:  psql -d templedb -f schema/postgres/001_read_subset.sql
-- Populate with:  templedb-rs mirror --to <url>

BEGIN;

-- ---------------------------------------------------------------------
-- Two conventions apply throughout, and both are load-bearing for
-- cross-backend agreement. Changing either makes this port answer
-- differently depending on where the bytes live.
--
-- 1. EVERY TEXT COLUMN IS `COLLATE "C"`.
--
--    SQLite compares TEXT with BINARY collation: byte order, always.
--    Postgres uses the database's collation, which on a typical
--    `en_US.UTF-8` cluster sorts case-insensitively and ignores
--    punctuation. Four queries in this port sort on text —
--    `project list` on `p.slug`, `entity search` on
--    `(observed_at DESC, kind, external_ref)`, `file ls` on
--    `pf.file_path`, and the relation fetch on
--    `(r.kind, e.kind, e.external_ref)` — and every one of them would
--    silently return the same rows in a different order. `C` is byte
--    order, which is what SQLite does.
--
--    It buys a second agreement for free. `entity search` filters on
--    `LOWER(col) LIKE <lowercased pattern>`, and SQLite's `LOWER()` is
--    ASCII-only while Postgres's is collation-aware. Under `C` the
--    Postgres one is also ASCII-only, so the two now agree on
--    non-ASCII queries as well — a divergence this port had recorded
--    as a known gap and which the collation choice closes.
--
-- 2. BOOLEAN-LOOKING COLUMNS ARE `integer`, NOT `boolean`.
--
--    `file_contents.is_current` is declared `BOOLEAN` in SQLite, which
--    has no boolean type: it stores 1 and 0. Three queries say
--    `WHERE fc.is_current = 1`. A real Postgres `boolean` rejects that
--    comparison outright, so the column is `integer` here and the one
--    SQL string serves both engines. The backend's type bridge does map
--    `BOOL` to `Int` as a safety net (see `backend/pg.rs`), but relying
--    on it would mean the predicate reads as a coincidence.
--
-- SQLite's `INTEGER` is a 64-bit signed value, so it becomes `bigint`
-- rather than `integer` everywhere. Timestamps stay `text`: they are
-- written by Python `str()` and reformatting them as `timestamptz`
-- would change `observed_at` in printed output, which is a parity
-- decision rather than a type conversion.
-- ---------------------------------------------------------------------

CREATE TABLE projects (
    id         bigint PRIMARY KEY,
    slug       text COLLATE "C" NOT NULL UNIQUE,
    name       text COLLATE "C",
    -- `project show` prints these two and nothing else reads them.
    repo_url   text COLLATE "C",
    git_branch text COLLATE "C",
    -- Which branch `vcs branch` marks active. No FK: the live schema
    -- declares one to `vcs_branches`, but the mirror loads `projects`
    -- first (every other table references it), so enforcing it here
    -- would make the load order circular. `vcs branch` falls back to
    -- the default branch when this does not resolve, exactly as
    -- Python does.
    active_branch_id bigint,
    -- `project list`'s two filters. `is_nix_project` is 0/1 in SQLite
    -- and stays an integer here rather than becoming `boolean`: the
    -- cross-backend digest hashes the value as it is read, so a type
    -- that renders `1` on one side and `t` on the other would report
    -- every row as different. Same reasoning as `is_current` and
    -- `is_default`.
    is_nix_project   integer,
    -- No CHECK constraint, though Python's argparse offers a fixed list
    -- of five. The constraint belongs to the writer, and a value
    -- outside the list should mirror and render as itself rather than
    -- fail the copy — the mirror's job is to carry what is there.
    project_category text COLLATE "C"
);

CREATE TABLE project_files (
    id            bigint PRIMARY KEY,
    project_id    bigint NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_path     text COLLATE "C" NOT NULL,
    status        text COLLATE "C" DEFAULT 'active',
    -- Both read only by `project show`. No FK on `file_type_id`:
    -- `file_types` is outside this subset and the only use is
    -- `COUNT(DISTINCT file_type_id)`, which needs the value, not the row.
    file_type_id  bigint,
    lines_of_code bigint,
    -- migration 106. Read by `file ls -l`, which renders NULL as the
    -- word `immutable`, and filtered on by `--edit-mode`. Nullable and
    -- NULL on all 2,491 live rows, so the column is a constant today and
    -- the mirror has nothing to prove — but `--edit-mode non-immutable`
    -- distinguishes NULL from the literal 'immutable', so the NULL has
    -- to survive the copy rather than be coalesced into it.
    edit_mode     text COLLATE "C",
    UNIQUE (project_id, file_path)
);

-- `file ls` filters `status = 'active'` and orders by `file_path`;
-- `project list` joins every active file per project.
CREATE INDEX project_files_project_status_path
    ON project_files (project_id, status, file_path);

CREATE TABLE content_blobs (
    hash_sha256     text COLLATE "C" PRIMARY KEY,
    -- Exactly one of these is populated. `content_text` is NULL for a
    -- binary file, which `file cat` reports as an error rather than as
    -- an empty file.
    content_text    text,
    content_blob    bytea,
    content_type    text COLLATE "C" NOT NULL,
    file_size_bytes bigint NOT NULL
);

-- `content_text` is not `COLLATE "C"`, and unlike the columns above
-- that is not because nothing compares it. `search content` compares it
-- with `ILIKE`. An earlier revision of this file claimed the column was
-- only ever selected, which stopped being true when the search's
-- matching moved into Rust and the prefilter became a `LIKE`.
--
-- It is left at the database collation anyway, for a reason worth
-- stating because the obvious change does nothing. `COLLATE "C"` was
-- tried first, on the theory that it would make `ILIKE` fold ASCII only
-- and so agree with SQLite exactly, and that it would be faster:
-- measured, it changed neither — Postgres 17's `ILIKE` cost 1,280 ms
-- either way. Agreement does not depend on it, because the prefilter
-- only has to be no narrower than the Rust matcher (see `search.rs`),
-- and `ILIKE`'s broader folding is on the safe side of that.

-- A pre-lowercased copy of the text, for `search content` to prefilter
-- against. This is the single biggest difference to that command's cost
-- on Postgres, and the two cheaper things were tried first and measured:
--
--   * `ILIKE '%term%'` on `content_text`, with a trigram index on it:
--     1,341 ms for `run`. The index is not even used — `%run%` is one
--     trigram and it occurs in nearly every document, so the planner
--     correctly prefers a sequential scan, and then pays 1.3 s because
--     case-insensitive matching is slow per byte.
--   * `LOWER(content_text) LIKE ...`: worse, 1,237 ms even for a rare
--     term, because wrapping the column in a function call hides it
--     from the planner entirely.
--
-- Against this column, case-sensitive `LIKE` with a trigram index does
-- `run` in 249 ms and `checkout` in 146 ms — the case-sensitive matcher
-- is several times faster per byte, and the index becomes usable for
-- selective terms. The cost is storing the text twice, which is
-- acceptable in a read replica that is rebuilt by one command.
--
-- SQLite needs no equivalent: its `LIKE` is already ASCII
-- case-insensitive and reasonably quick, and this port does not own
-- that schema anyway. So the prefilter column differs per backend. The
-- candidate sets do not — see `search.rs`, which also explains why the
-- prefilter is allowed to fold more than the Rust matcher does.
--
-- `GENERATED ... STORED` rather than a column the mirror fills, so it
-- cannot go stale and so `mirror` needs no knowledge of it. The COPY
-- does not name it, which is what Postgres requires for a generated
-- column.
ALTER TABLE content_blobs
    ADD COLUMN text_lower text GENERATED ALWAYS AS (lower(content_text)) STORED;

-- Wrapped in a DO block because `pg_trgm` is a contrib extension and
-- may not be installed. Everything still works without it; only
-- `search content` is slower, and it answers the same.
DO $$
BEGIN
    CREATE EXTENSION IF NOT EXISTS pg_trgm;
    CREATE INDEX IF NOT EXISTS content_blobs_lower_trgm
        ON content_blobs USING gin (text_lower gin_trgm_ops);
EXCEPTION WHEN insufficient_privilege OR undefined_file THEN
    RAISE NOTICE 'pg_trgm unavailable; search content will scan sequentially';
END $$;

CREATE TABLE file_contents (
    id              bigint PRIMARY KEY,
    file_id         bigint NOT NULL REFERENCES project_files(id) ON DELETE CASCADE,
    content_hash    text COLLATE "C" NOT NULL,
    file_size_bytes bigint NOT NULL,
    line_count      bigint,
    -- See convention 2 above: integer, not boolean.
    is_current      integer DEFAULT 1,
    updated_at      text COLLATE "C" NOT NULL,
    -- `vcs diff <proj> <file>` orders by this. DEFAULT 1 because the
    -- live column was appended by ALTER TABLE.
    version         bigint DEFAULT 1,
    -- Mirrors the live constraint, whose comment reads "Only one
    -- current version per file" -- which is not what it says. With
    -- `is_current` as 1/0 it permits one current row AND one
    -- non-current row, so it caps the table at two revisions per file
    -- rather than constraining how many are current. Expressing the
    -- stated intent needs a partial index
    -- (`UNIQUE (file_id) WHERE is_current = 1`).
    --
    -- Moot in practice today: the table holds no history at all. All
    -- 2,095 files have exactly one row, every row is is_current = 1,
    -- and `version` increments in place (observed up to 14) rather
    -- than a prior row being retained. Reproduced as-is because the
    -- mirror has to accept the rows the live database produces.
    UNIQUE (file_id, is_current)
);

-- No FK from `file_contents.content_hash` to `content_blobs`, and none
-- from `vcs_file_states.content_hash` either. SQLite declares the first
-- one but 509 `vcs_file_states` rows in the live database reference a
-- hash that is not in `content_blobs` — pruned or never ingested — so a
-- mirror that enforced it would refuse to load real data. The port
-- handles the dangling case explicitly: `source snapshot` errors and
-- names the blob, where the Python implementation printed nothing and
-- exited 0.

CREATE INDEX file_contents_current ON file_contents (file_id) WHERE is_current = 1;

-- `vcs log` joins this to resolve each commit.s branch name. Only the
-- three columns that join and display are carried; `parent_branch_id`,
-- `is_default`, `is_protected` and `head_commit_id` belong to `vcs
-- branch` and `vcs switch`, which are not ported.
CREATE TABLE vcs_branches (
    id             bigint PRIMARY KEY,
    project_id     bigint NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    branch_name    text COLLATE "C" NOT NULL,
    -- `integer`, not `boolean`, per convention 2 above: SQLite stores
    -- 1/0 and `vcs branch` compares `is_default = 1`.
    is_default     integer DEFAULT 0,
    -- No FK to `vcs_commits`: branches load before commits, and the
    -- head can name a commit that was pruned. `vcs branch` reads the
    -- head message through a subquery that yields NULL in that case.
    head_commit_id bigint,
    UNIQUE (project_id, branch_name)
);

-- The staging sessions a commit can belong to. Two columns of ten, and
-- the boundary follows the readers as it does everywhere else: the port
-- resolves a session the caller *named* and checks that it is live, so
-- it reads `id` and `ended_at` and nothing more. `name`, `author`,
-- `host`, `pid`, `started_at`, `ended_reason`,
-- `expected_lifetime_seconds` and `reap_policy` belong to
-- `vcs session list` / `show` / `prune`, none of which is ported — and a
-- column with no reader has nothing to verify a mirror against.
--
-- `ended_at IS NULL` means active, which is the one predicate anything
-- here asks. 842 rows live, 40 of them active.
CREATE TABLE vcs_sessions (
    id       bigint PRIMARY KEY,
    ended_at text COLLATE "C"
);

-- Active sessions are 5% of the table and the only ones ever looked up.
CREATE INDEX vcs_sessions_live ON vcs_sessions (id) WHERE ended_at IS NULL;

CREATE TABLE vcs_commits (
    id               bigint PRIMARY KEY,
    project_id       bigint NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- No FK, though the live schema declares one. `vcs log` reproduces
    -- Python.s INNER JOIN, which drops a commit whose branch does not
    -- resolve; enforcing the constraint here would instead make the
    -- mirror refuse to load such a row, turning a display question into
    -- an ingest failure.
    branch_id        bigint NOT NULL,
    commit_hash      text COLLATE "C" NOT NULL UNIQUE,
    author           text COLLATE "C" NOT NULL,
    commit_message   text COLLATE "C" NOT NULL,
    commit_timestamp text COLLATE "C" NOT NULL,
    -- NULL means published; a value means the commit belongs to one
    -- staging session. `vcs log`'s default view compares it against the
    -- resolved session and the marker is printed from it either way.
    --
    -- Still no FK to `vcs_sessions`, now that the table is here.
    -- `vcs session prune` deletes ended sessions, and nothing nulls the
    -- commits that referenced them — so a dangling `session_id` is a
    -- state the live database can reach, and a mirror that enforced the
    -- constraint would refuse to load it. Same reasoning as
    -- `content_hash`, where 509 rows already name a pruned blob.
    session_id       bigint
);

-- `vcs log` filters on project and orders by timestamp with `id` as the
-- tie-break, so the index carries all three.
CREATE INDEX vcs_commits_project_time
    ON vcs_commits (project_id, commit_timestamp DESC, id ASC);

CREATE TABLE vcs_file_states (
    id           bigint PRIMARY KEY,
    commit_id    bigint NOT NULL REFERENCES vcs_commits(id) ON DELETE CASCADE,
    file_id      bigint NOT NULL,
    -- Migration 118's denormalised path, and the fourth column here
    -- carried for a writer rather than a reader: nothing in *either*
    -- implementation selects it. It exists because `vcs commit` is the
    -- only path in the system that hard-deletes a `project_files` row —
    -- everything else flips `status` — so for a committed deletion this
    -- is the last record of which file the state belonged to. Without
    -- it, `commit_staged`'s own DELETE would leave a row whose
    -- `file_id` resolves to nothing, which is the hole 118 closed.
    --
    -- 7,183 of 7,197 live rows carry one. Nullable, because the 14 that
    -- do not predate the column.
    file_path    text COLLATE "C",
    change_type  text COLLATE "C" NOT NULL,
    -- Nullable, and NULL is how a deletion is recorded. The Python
    -- implementation does `row['content_hash'][:12]` unconditionally
    -- and raises `TypeError` on these rows, after printing the rows
    -- above the first deletion; 29 rows in the live database have one.
    content_hash text COLLATE "C",
    file_size    bigint,
    line_count   bigint,
    UNIQUE (commit_id, file_id)
);

-- `file_id` has no FK: `vcs_file_states` outlives the
-- `project_files` row for a file that was deleted and whose row was
-- later reaped, and the `source_snapshots` view inner-joins
-- `project_files` anyway, so an orphan simply does not appear.
CREATE INDEX vcs_file_states_file ON vcs_file_states (file_id);

-- The parent edges `vcs show` diffs a commit against. A join table
-- rather than a column on `vcs_commits` because a merge commit has
-- more than one parent; `parent_order` picks the first, which is the
-- one `git show` semantics compare against.
--
-- 718 rows in the live database, every one with `parent_order = 0` —
-- migration 123 records that as a known-constant column, because no
-- merge has been committed yet. So the ordering is reproduced from
-- Python rather than exercised by the data.
CREATE TABLE vcs_commit_parents (
    commit_id        bigint NOT NULL REFERENCES vcs_commits(id) ON DELETE CASCADE,
    parent_commit_id bigint NOT NULL REFERENCES vcs_commits(id) ON DELETE CASCADE,
    parent_order     bigint NOT NULL,
    PRIMARY KEY (commit_id, parent_commit_id)
);

-- Provenance for `file set`: who set this, when, from which session.
--
-- The first table in this file that exists for a *writer* rather than a
-- reader, and the column set is chosen accordingly: these are the ones
-- `record_intent` writes, plus `id`. Twelve of fourteen.
--
-- The two omitted are `cancelled_at` and `applied_commit_id`, which
-- belong to `intent cancel` and to the commit link `vcs commit` would
-- make. Neither is ported, and `applied_commit_id` additionally
-- references `vcs_commits` — a column carried for nobody that would also
-- constrain the load order.
--
-- `created_at` is carried even though the writer lets it default,
-- because `mirror` has to reproduce what the live rows hold and the
-- cross-backend digest compares it. The DEFAULT exists for the writer;
-- the column exists for the copy.
CREATE TABLE edit_intents (
    id               bigint PRIMARY KEY,
    -- No FK to `vcs_sessions`, though the live schema declares one.
    -- Same reasoning as `vcs_commits.session_id` and then some: the
    -- live constraint is a latent bug rather than a guarantee, because
    -- `prune_sessions` guards only on `vcs_working_state` and would hit
    -- this FK on a batch delete. 976 of 1,220 rows are NULL and 218
    -- reference a session that has already ended.
    session_id       bigint,
    project_id       bigint NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    file_path        text COLLATE "C" NOT NULL,
    -- `'current'` on all 886 rows migration 123 catalogued, which is
    -- why it is a `constant` in that baseline: the field designed to
    -- record what an edit was made against never recorded anything.
    -- It does now — the previous content hash, or the literal
    -- `'new-file'` — so the two cases stay distinguishable from a
    -- column that is merely unfilled. DEFAULT reproduced for fidelity;
    -- the writer always supplies a value.
    base_revision    text COLLATE "C" NOT NULL DEFAULT 'current',
    new_content_hash text COLLATE "C" NOT NULL,
    patch_summary    text COLLATE "C",
    author           text COLLATE "C",
    description      text COLLATE "C",
    -- CHECK'd to ('proposed','applied','cancelled') in SQLite and not
    -- reproduced, for the reason `ingestion_runs` gives: a constraint
    -- that rejects a future status turns a copy into a failure. The
    -- writer only ever inserts 'applied'.
    status           text COLLATE "C" NOT NULL DEFAULT 'proposed',
    created_at       text COLLATE "C" NOT NULL,
    applied_at       text COLLATE "C"
);

-- The provenance question is always "what happened to this file", so
-- the index carries the pair and the time.
CREATE INDEX edit_intents_file ON edit_intents (project_id, file_path, created_at DESC);

-- Three columns of forty, and that is the table's whole information
-- content rather than the usual follow-the-readers trim.
--
-- Measured 2026-10-24 over all 436 live rows: `intent`, `change_type`,
-- `scope`, `tags`, `ai_tool`, `review_status` and `confidence_level` are
-- non-NULL on ZERO rows, and `is_breaking` / `ai_assisted` are 0 on
-- every one. They are reachable only through `commit --interactive` or
-- an explicit flag, and nobody has ever passed one.
--
-- The one populated column is derived: `_collect_commit_metadata` sets
-- `impact_level` from the staged file count — `'high'` above 50,
-- `'medium'` above 20, else `'low'` (410 low, 18 medium, 8 high). So the
-- table is a bucketed count, once per commit, read by nothing. Same
-- shape as `vcs log`'s `files_changed`, except stored.
CREATE TABLE vcs_commit_metadata (
    id            bigint PRIMARY KEY,
    -- UNIQUE in the live schema and reproduced: one metadata row per
    -- commit. The FK is declared because `vcs_commits` loads first and
    -- a metadata row for a pruned commit is not a state the live
    -- database can reach — the live FK is ON DELETE CASCADE.
    commit_id     bigint NOT NULL UNIQUE REFERENCES vcs_commits(id) ON DELETE CASCADE,
    impact_level  text COLLATE "C",
    created_at    text COLLATE "C" NOT NULL
);

-- Staging. Nothing in the port reads it yet: it arrives with
-- `file set --stage`, whose only consumer is `vcs commit`. The second
-- table here carried for a writer rather than a reader, after
-- `edit_intents` — see the TempleDB report *vcs commit: the widest
-- door, measured*.
--
-- `content_text` and `content_blob` are deliberately absent. What
-- `vcs commit` reads is the staged *hash*, and those columns hold the
-- workspace's copy of bytes `content_blobs` already stores under that
-- hash. Carrying them would store staged content twice, on top of the
-- `text_lower` copy `content_blobs` already pays for.
CREATE TABLE vcs_working_state (
    id                   bigint PRIMARY KEY,
    project_id           bigint NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- No FK on `branch_id` or `file_id`, though the live schema
    -- declares both ON DELETE CASCADE. `file_id` is the one that
    -- matters: `sync_engine` and some migrations bulk-delete with FK
    -- enforcement off, so rows pointing at a reaped `project_files`
    -- row are a state the live database reaches — it is why
    -- `vcs commit` carries a defensive orphan reap and why
    -- `working_state_references_valid_files` is an invariant check. A
    -- mirror enforcing the constraint would refuse to load real data.
    branch_id            bigint NOT NULL,
    file_id              bigint NOT NULL,
    content_hash         text COLLATE "C",
    -- 'unmodified' | 'modified' | 'added' | 'deleted' | 'conflict'.
    -- CHECK'd nowhere in the live schema either; the comment there is
    -- the only constraint.
    state                text COLLATE "C" NOT NULL DEFAULT 'unmodified',
    last_modified        text COLLATE "C" NOT NULL,
    -- NULL means NOT STAGED, not "staged by nobody". Migration 083
    -- dropped the `staged` BOOLEAN in favour of this column, and
    -- `unstage_files` is literally `SET staged_by_session_id = NULL`.
    -- No FK to `vcs_sessions`: the default reap policy orphans these
    -- rows by design, and `vcs session prune` does not consult them.
    staged_by_session_id bigint,
    -- migration 088, so a provenance query can join a staged row to
    -- the `edit_intents` row that produced it. No FK, matching the
    -- live NO ACTION — nothing nulls it when an intent goes.
    intent_id            bigint,
    UNIQUE (project_id, branch_id, file_id)
);

-- The staged-rows lookup `vcs commit` and `publish` both make.
CREATE INDEX vcs_working_state_staged
    ON vcs_working_state (staged_by_session_id)
    WHERE staged_by_session_id IS NOT NULL;

CREATE TABLE entities (
    id               bigint PRIMARY KEY,
    kind             text COLLATE "C" NOT NULL,
    -- Nullable in the schema. No row in the live database has a NULL
    -- one, so neither this port's `(none)` nor Python's `TypeError` has
    -- ever been observed; both remain reachable in principle.
    external_ref     text COLLATE "C",
    source_authority text COLLATE "C" NOT NULL,
    label            text COLLATE "C",
    observed_at      text COLLATE "C" NOT NULL,
    UNIQUE (kind, external_ref)
);

-- `entity search` scans with `LOWER(label) LIKE '%...%'`, which no
-- btree index can serve. Left unindexed rather than reaching for
-- `pg_trgm`: adding an extension here would make the mirror depend on
-- one being installed, and the scan is over 55,196 rows.

CREATE TABLE relations (
    id               bigint PRIMARY KEY,
    from_entity_id   bigint NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    kind             text COLLATE "C" NOT NULL,
    to_entity_id     bigint NOT NULL REFERENCES entities(id) ON DELETE CASCADE,
    source_authority text COLLATE "C" NOT NULL,
    observed_at      text COLLATE "C" NOT NULL,
    UNIQUE (from_entity_id, kind, to_entity_id)
);

-- Both directions, because the BFS behind `entity trace` and
-- `entity paths` anchors on `from_entity_id` walking out and on
-- `to_entity_id` walking in, and issues one query per visited node.
-- `entity paths` removes the Python implementation's hardcoded 100-edge
-- fan-out cap, so a failed search exhausts the reachable component:
-- without the inbound index that is a sequential scan per hop rather
-- than the whole search costing 17 ms.
CREATE INDEX relations_from ON relations (from_entity_id, kind);
CREATE INDEX relations_to   ON relations (to_entity_id, kind);

-- Label and authority history, written by a trigger on `entities` in
-- the live database and only read here. `entity observations` is its
-- one reader.
--
-- No foreign key to `entities`, and that is the schema's intent rather
-- than an omission: the live column is commented "FK-ish (entity may
-- have been deleted)", because the archive is meant to outlive what it
-- describes. `entity_id` is carried for provenance and is not joined.
CREATE TABLE observations_archive (
    id                     bigint PRIMARY KEY,
    entity_kind            text COLLATE "C" NOT NULL,
    -- The entity's `external_ref`, denormalised. This is the address
    -- the command looks rows up by.
    entity_ref             text COLLATE "C" NOT NULL,
    label                  text COLLATE "C",
    source_authority       text COLLATE "C",
    observed_at            text COLLATE "C" NOT NULL,
    entity_id              bigint,
    prior_label            text COLLATE "C",
    prior_source_authority text COLLATE "C"
);

-- The lookup is always `(entity_kind, entity_ref)` ordered by
-- `observed_at DESC, id ASC`, so the index carries all four and the
-- query needs no sort.
CREATE INDEX observations_archive_entity
    ON observations_archive (entity_kind, entity_ref, observed_at DESC, id ASC);

-- ---------------------------------------------------------------------
-- The six tables `summary` reads.
--
-- Added 2026-10-18 with the `summary` port. The boundary still follows
-- the readers — only the columns the panes name are here, out of 14 on
-- `ingestion_runs` and 27 on `fleet_machines` — and the rule holds
-- that a table with no reader has nothing to verify a mirror against.
--
-- One shape note that applies to four of them. These are *event log*
-- tables: `ingestion_runs`, `invariant_checks` and `reconcile_runs`
-- each hold one row per run, and every pane that reads them wants the
-- latest row per group. That read pattern is why the indexes below
-- carry `(group, timestamp DESC)` rather than the timestamp alone, and
-- it is the same pattern the SQLite-only `GROUP BY` extension exists to
-- serve — see `summary.rs` for why that query cannot be written once.
-- ---------------------------------------------------------------------

CREATE TABLE ingestion_runs (
    id          bigint PRIMARY KEY,
    adapter     text COLLATE "C" NOT NULL,
    started_at  text COLLATE "C" NOT NULL,
    -- CHECK'd to ('running','ok','partial','error') in SQLite. Not
    -- reproduced: the mirror has to accept whatever the live database
    -- holds, and a constraint that rejects a future status would turn
    -- a copy into a failure.
    status      text COLLATE "C" NOT NULL
);

CREATE INDEX ingestion_runs_adapter_started
    ON ingestion_runs (adapter, started_at DESC);

CREATE TABLE invariant_checks (
    id          bigint PRIMARY KEY,
    check_name  text COLLATE "C" NOT NULL,
    ran_at      text COLLATE "C" NOT NULL,
    status      text COLLATE "C" NOT NULL,
    issue_count bigint NOT NULL
);

CREATE INDEX invariant_checks_name_time
    ON invariant_checks (check_name, ran_at DESC);

CREATE TABLE reconcile_runs (
    id           bigint PRIMARY KEY,
    machine_name text COLLATE "C" NOT NULL,
    ran_at       text COLLATE "C" NOT NULL,
    status       text COLLATE "C" NOT NULL
);

CREATE INDEX reconcile_runs_machine_time
    ON reconcile_runs (machine_name, ran_at DESC);

-- `last_deployed_at` is declared TIMESTAMP in SQLite, which stores it
-- as whatever string was written — SQLite has no date type. Mirrored as
-- text for the same reason every other timestamp here is: the port
-- compares these as strings and parses them in Rust, so turning one
-- into a `timestamptz` would introduce a timezone that the SQLite side
-- does not have.
CREATE TABLE fleet_machines (
    id               bigint PRIMARY KEY,
    machine_name     text COLLATE "C" NOT NULL,
    last_deployed_at text COLLATE "C"
);

CREATE TABLE handoff_notes (
    id         bigint PRIMARY KEY,
    to_session text COLLATE "C",
    to_topic   text COLLATE "C",
    acked_at   text COLLATE "C"
);

-- The unacked-broadcast query is `to_session IS NULL AND to_topic IS
-- NULL AND acked_at IS NULL`, which no btree index helps with on a
-- table this size; the partial index SQLite carries for it is not
-- reproduced.
CREATE INDEX handoff_notes_session
    ON handoff_notes (to_session, acked_at);

CREATE TABLE reconciliations (
    id                bigint PRIMARY KEY,
    kind              text COLLATE "C" NOT NULL,
    subject_ref       text COLLATE "C" NOT NULL,
    status            text COLLATE "C" NOT NULL,
    -- The pane prints this and never reads `reconciliation_items`, so
    -- that table is not mirrored. If a surface ever needs the items
    -- themselves it gains a reader and then a mirror, in that order.
    item_count        bigint NOT NULL,
    owner             text COLLATE "C",
    first_detected_at text COLLATE "C",
    last_observed_at  text COLLATE "C",
    review_after      text COLLATE "C"
);

-- ---------------------------------------------------------------------
-- `source_snapshots`, translated from the live view.
--
-- Structurally identical to the SQLite original — a `UNION ALL` of the
-- current state and every historical state — with two changes, both
-- required rather than stylistic:
--
--   * The string literals are cast (`'current'::text COLLATE "C"`).
--     An uncoerced literal in Postgres has type `unknown`, and in a
--     `UNION ALL` against a `COLLATE "C"` column the result's collation
--     would be resolved implicitly. `ORDER BY revision` in
--     `source revisions` sorts on this column, so the collation has to
--     be the one SQLite uses, said out loud.
--
--   * `fc.is_current = 1` compares against an integer, per convention 2.
--
-- The `LEFT JOIN content_blobs` in the historical half is the subtle
-- part and is preserved exactly: a row may name a `content_hash` whose
-- blob is gone, which is why `content_text`, `content_blob` and
-- `content_type` are all nullable in the result.
-- ---------------------------------------------------------------------

CREATE VIEW source_snapshots AS
    -- Current state (is_current = 1 in file_contents)
    SELECT
        p.slug                        AS project_slug,
        pf.file_path                  AS file_path,
        'current'::text COLLATE "C"   AS revision,
        fc.content_hash               AS content_hash,
        cb.content_text               AS content_text,
        cb.content_blob               AS content_blob,
        cb.content_type               AS content_type,
        fc.file_size_bytes            AS file_size_bytes,
        fc.line_count                 AS line_count,
        fc.updated_at                 AS observed_at,
        'git'::text COLLATE "C"       AS source_authority
    FROM file_contents fc
    JOIN project_files pf ON pf.id = fc.file_id
    JOIN projects p       ON p.id = pf.project_id
    JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
    WHERE fc.is_current = 1
      AND pf.status = 'active'

    UNION ALL

    -- Historical state (any vcs_file_states row)
    SELECT
        p.slug                        AS project_slug,
        pf.file_path                  AS file_path,
        c.commit_hash                 AS revision,
        vfs.content_hash              AS content_hash,
        cb.content_text               AS content_text,
        cb.content_blob               AS content_blob,
        cb.content_type               AS content_type,
        vfs.file_size                 AS file_size_bytes,
        vfs.line_count                AS line_count,
        c.commit_timestamp            AS observed_at,
        'git'::text COLLATE "C"       AS source_authority
    FROM vcs_file_states vfs
    JOIN vcs_commits c    ON c.id = vfs.commit_id
    JOIN project_files pf ON pf.id = vfs.file_id
    JOIN projects p       ON p.id = pf.project_id
    LEFT JOIN content_blobs cb ON cb.hash_sha256 = vfs.content_hash;

-- ---------------------------------------------------------------------
-- NOT TRANSLATED, AND NOT NEEDED: `file_contents_fts`.
--
-- The FTS5 virtual table that used to back `search content` has no
-- Postgres equivalent returning the same rows, and the gap was measured
-- rather than assumed: FTS5 stems with the original Porter algorithm
-- and Postgres's `english` configuration with Porter2, so on these
-- 2,063 files they disagree by up to a quarter of the hit set —
-- `deployment` matches 356 documents under one and 449 under the other.
-- That is not a difference in ordering or in snippet formatting; it
-- changes which files are returned.
--
-- So no full-text index is translated, because none is used. Matching,
-- ranking and snippet rendering all happen in Rust
-- (`crates/templedb-db/src/textsearch.rs`), and SQL's only job is the
-- `ILIKE` prefilter the trigram index above accelerates. That is what
-- lets the command mean one thing on both backends; the cost is that
-- its semantics are substring rather than stemmed.
--
-- The SQLite side no longer queries `file_contents_fts` either. The
-- table is still maintained by the Python implementation and is not
-- this port's to drop.
-- ---------------------------------------------------------------------

-- ---------------------------------------------------------------------
-- Identity on the two id columns a write has to generate.
--
-- This schema was built for `mirror`, which copies rows that already
-- have ids, so every `id` is a plain `bigint PRIMARY KEY` with no
-- default. A writer has nothing to generate one from, and that is the
-- first structural thing the write path needs that the read subset
-- never did.
--
-- `BY DEFAULT`, not `ALWAYS`: `mirror` supplies explicit ids and must
-- keep doing so. `ALWAYS` would reject its `COPY` outright, and
-- `OVERRIDING SYSTEM VALUE` on a binary COPY is not available.
--
-- The consequence is a sequence that must be advanced after every
-- mirror run, because `COPY` with explicit ids does not touch it — a
-- freshly mirrored database would otherwise hand out id 1 and collide
-- on the primary key. `mirror` does that itself; see `sync_sequences`
-- in `mirror.rs`. Getting this wrong is loud rather than silent, which
-- is the one merciful thing about it.
--
-- Six tables: what `file set` and `vcs commit-staged` insert into. The
-- rest of the subset keeps following the readers.
ALTER TABLE project_files
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;
ALTER TABLE file_contents
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;
-- `edit_intents` joined them on 2026-10-23 with the intent record. Its
-- id is also the one thing the command *prints* from this table, so a
-- sequence left behind by a mirror would be visible rather than merely
-- wrong.
ALTER TABLE edit_intents
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;
-- `vcs_commits` and `vcs_file_states` joined on 2026-10-25 with
-- `vcs commit-staged`. The commit's id is load-bearing twice over and
-- not merely printed: `vcs_commit_parents` stores the parent edge by id,
-- and `vcs_branches.head_commit_id` is advanced to it by a
-- compare-and-swap this port does in application code because it has
-- none of the live triggers. A sequence handing out an id that already
-- exists would fail the primary key — loudly, which is the one merciful
-- thing about it, and the reason `mirror`'s `sync_sequences` has to keep
-- covering every table in this list.
ALTER TABLE vcs_commits
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;
ALTER TABLE vcs_file_states
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;
-- `vcs_working_state` joined on 2026-10-27 with `file set`'s
-- auto-stage, which is the write that closes the loop: this table is
-- what `vcs commit-staged` above selects from, so until something could
-- insert here the two halves were verified one at a time with a SQL
-- fixture standing in for the first.
ALTER TABLE vcs_working_state
    ALTER COLUMN id ADD GENERATED BY DEFAULT AS IDENTITY;

COMMIT;
