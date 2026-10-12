-- TempleDB, Postgres: the objects the Rust port reads.
--
-- SCOPE. This is not the schema. The live SQLite database has 163
-- tables, 45 views, 61 triggers and 361 indexes; this file has ten
-- tables and one view, because that is the entire set the Rust port
-- touches today. The full translation is the next step toward Postgres
-- as the primary store and belongs with the write-path design document,
-- not here: the remaining 153 tables have no reader in this codebase
-- yet, and mirroring data nothing reads would be work with no way to
-- verify it.
--
-- Columns are likewise limited to the ones the queries name, plus the
-- keys the joins and uniqueness constraints need. The omitted columns
-- are not all uninteresting — `project_files` alone has twenty — but
-- several carry foreign keys to tables outside this subset
-- (`file_types`, `vcs_branches`, `vcs_sessions`), so including them
-- would drag the boundary somewhere arbitrary rather than somewhere
-- defensible.
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
    active_branch_id bigint
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
    -- NULL means published. No FK to `vcs_sessions`, which is not in
    -- this subset: the port reads the column only to decide whether to
    -- print `(unpublished)`, never to resolve a session.
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

COMMIT;
