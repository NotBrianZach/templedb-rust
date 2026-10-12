//! An in-memory fixture database for the crate's tests.
//!
//! The real schema is 163 tables; this reproduces only the columns the
//! queries in this crate actually touch, plus `source_snapshots` copied
//! verbatim from the live database so the `UNION ALL` and the
//! `LEFT JOIN content_blobs` behave identically. Foreign keys and
//! `CHECK` constraints are dropped on purpose — the point is to exercise
//! our SQL, not to re-certify the schema.
//!
//! Every row here exists to pin one behaviour, and the comments say
//! which. Adding a row changes test expectations in several modules, so
//! prefer adding a new row to editing an existing one.

use crate::backend::sqlite::SqliteStore;

/// The fixture is a [`SqliteStore`] rather than a `&dyn Store` because
/// two tests need to write to it — `entities.rs` builds 150 edges on one
/// node to pin the behaviour past Python's hardcoded fan-out cap — and
/// writes are deliberately not on the [`Store`](crate::Store) trait.
/// Call sites pass `&c` and the coercion to `&dyn Store` happens there.
pub fn fixture() -> SqliteStore {
    let store = SqliteStore::open_in_memory().expect("in-memory sqlite");
    store.execute_batch(SCHEMA).expect("fixture schema");
    store.execute_batch(DATA).expect("fixture data");
    store
}

/// [`fixture`] plus the import graph `entity dead-imports` needs.
///
/// A second fixture rather than more rows in [`DATA`], because that
/// command wants nine `File` and `Symbol` entities and adding them to
/// the shared set would move the expectations of a dozen unrelated
/// tests — `entity stats` counts every row, and `entity search o`
/// matches anything containing the letter, which every `proj_a/...`
/// reference does. The module note above says to prefer adding rows
/// over editing them; this keeps that cheap by adding them somewhere
/// nothing else reads.
pub fn fixture_with_imports() -> SqliteStore {
    let store = fixture();
    store.execute_batch(IMPORT_GRAPH).expect("import fixture");
    store
}

const SCHEMA: &str = r#"
CREATE TABLE projects (
    id               INTEGER PRIMARY KEY,
    slug             TEXT NOT NULL UNIQUE,
    name             TEXT,
    repo_url         TEXT,
    git_branch       TEXT,
    active_branch_id INTEGER
);

CREATE TABLE project_files (
    id            INTEGER PRIMARY KEY,
    project_id    INTEGER NOT NULL,
    file_path     TEXT NOT NULL,
    status        TEXT DEFAULT 'active',
    file_type_id  INTEGER,
    lines_of_code INTEGER,
    UNIQUE(project_id, file_path)
);

CREATE TABLE content_blobs (
    hash_sha256     TEXT PRIMARY KEY,
    content_text    TEXT,
    content_blob    BLOB,
    content_type    TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL
);

CREATE TABLE file_contents (
    id              INTEGER PRIMARY KEY,
    file_id         INTEGER NOT NULL,
    content_hash    TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    line_count      INTEGER,
    is_current      BOOLEAN DEFAULT 1,
    updated_at      TEXT NOT NULL,
    UNIQUE(file_id, is_current)
);

CREATE TABLE vcs_branches (
    id             INTEGER PRIMARY KEY,
    project_id     INTEGER NOT NULL,
    branch_name    TEXT NOT NULL,
    is_default     BOOLEAN DEFAULT 0,
    head_commit_id INTEGER,
    UNIQUE(project_id, branch_name)
);

CREATE TABLE vcs_commits (
    id               INTEGER PRIMARY KEY,
    project_id       INTEGER NOT NULL,
    branch_id        INTEGER NOT NULL,
    commit_hash      TEXT NOT NULL UNIQUE,
    author           TEXT NOT NULL,
    commit_message   TEXT NOT NULL,
    commit_timestamp TEXT NOT NULL,
    -- NULL means published; a value means the commit belongs to one
    -- staging session and only that session's view shows it.
    session_id       INTEGER
);

CREATE TABLE vcs_file_states (
    id           INTEGER PRIMARY KEY,
    commit_id    INTEGER NOT NULL,
    file_id      INTEGER NOT NULL,
    change_type  TEXT NOT NULL,
    content_hash TEXT,
    file_size    INTEGER,
    line_count   INTEGER,
    UNIQUE(commit_id, file_id)
);

CREATE TABLE vcs_commit_parents (
    commit_id        INTEGER NOT NULL,
    parent_commit_id INTEGER NOT NULL,
    parent_order     INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (commit_id, parent_commit_id)
);

CREATE TABLE entities (
    id               INTEGER PRIMARY KEY,
    kind             TEXT NOT NULL,
    external_ref     TEXT,
    source_authority TEXT NOT NULL,
    label            TEXT,
    observed_at      TEXT NOT NULL,
    UNIQUE(kind, external_ref)
);

CREATE TABLE relations (
    id               INTEGER PRIMARY KEY,
    from_entity_id   INTEGER NOT NULL,
    to_entity_id     INTEGER NOT NULL,
    kind             TEXT NOT NULL,
    source_authority TEXT NOT NULL,
    observed_at      TEXT NOT NULL DEFAULT '2026-10-01 00:00:00',
    UNIQUE(from_entity_id, kind, to_entity_id)
);

CREATE TABLE observations_archive (
    id                     INTEGER PRIMARY KEY,
    entity_kind            TEXT NOT NULL,
    entity_ref             TEXT NOT NULL,
    label                  TEXT,
    source_authority       TEXT,
    observed_at            TEXT NOT NULL,
    entity_id              INTEGER,
    prior_label            TEXT,
    prior_source_authority TEXT
);

CREATE VIRTUAL TABLE file_contents_fts USING fts5(
    file_path UNINDEXED,
    content_text,
    tokenize='porter unicode61 remove_diacritics 1'
);

-- Verbatim from the live database. The two halves differ in more than
-- the revision label: the historical one LEFT JOINs content_blobs, so a
-- row can name a blob that is gone.
CREATE VIEW source_snapshots AS
    SELECT
        p.slug             AS project_slug,
        pf.file_path       AS file_path,
        'current'          AS revision,
        fc.content_hash    AS content_hash,
        cb.content_text    AS content_text,
        cb.content_blob    AS content_blob,
        cb.content_type    AS content_type,
        fc.file_size_bytes AS file_size_bytes,
        fc.line_count      AS line_count,
        fc.updated_at      AS observed_at,
        'git'              AS source_authority
    FROM file_contents fc
    JOIN project_files pf ON pf.id = fc.file_id
    JOIN projects p       ON p.id = pf.project_id
    JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
    WHERE fc.is_current = 1
      AND pf.status = 'active'

    UNION ALL

    SELECT
        p.slug               AS project_slug,
        pf.file_path         AS file_path,
        c.commit_hash        AS revision,
        vfs.content_hash     AS content_hash,
        cb.content_text      AS content_text,
        cb.content_blob      AS content_blob,
        cb.content_type      AS content_type,
        vfs.file_size        AS file_size_bytes,
        vfs.line_count       AS line_count,
        c.commit_timestamp   AS observed_at,
        'git'                AS source_authority
    FROM vcs_file_states vfs
    JOIN vcs_commits c    ON c.id = vfs.commit_id
    JOIN project_files pf ON pf.id = vfs.file_id
    JOIN projects p       ON p.id = pf.project_id
    LEFT JOIN content_blobs cb ON cb.hash_sha256 = vfs.content_hash;
"#;

const DATA: &str = r#"
-- 'alpha' has a name that differs from its slug, which is the shape
-- that trips the Python fuzzy matcher. 'empty' has a NULL name, the
-- shape that makes the literal string "None" searchable there.
-- `alpha` carries the repo/branch metadata `project show` prints, and
-- pins its active branch explicitly. `beta` leaves `active_branch_id`
-- NULL so the default-branch fallback is exercised. `empty` has
-- neither, and no files at all — which is the shape that makes Python's
-- `SUM(lines_of_code)` NULL and crashes `project show` on a third of
-- the live projects.
INSERT INTO projects (id, slug, name, repo_url, git_branch, active_branch_id) VALUES
    (1, 'alpha', 'Alpha Project', 'git@example:alpha.git', 'main', 2),
    (2, 'beta',  'beta',          NULL,                    NULL,   NULL),
    (3, 'empty', NULL,            NULL,                    NULL,   NULL);

-- `file_type_id` and `lines_of_code` are read only by `project show`.
-- alpha gets two distinct types and two of its four rows carry a line
-- count, so `SUM` is non-NULL and `COUNT(DISTINCT file_type_id)` is
-- more than one. beta's are all NULL, which is the shape that makes
-- Python's `f"{None:,}"` raise.
INSERT INTO project_files
    (id, project_id, file_path, status, file_type_id, lines_of_code)
VALUES
    (1,  1, 'src/main.rs', 'active', 1, 30),
    -- active but with no current content row: must list at 0 lines,
    -- not disappear.
    (2,  1, 'notes.md',    'active', 2, 12),
    -- has content, but deleted: must not list and must not be readable.
    (3,  1, 'gone.txt',    'deleted', 1, NULL),
    (4,  1, 'README.md',   'active', 2, NULL),
    -- a_b.txt / axb.txt: the pair that distinguishes a literal '_'
    -- from a LIKE wildcard.
    (5,  2, 'a_b.txt',     'active', NULL, NULL),
    (6,  2, 'axb.txt',     'active', NULL, NULL),
    (7,  2, 'logo.png',    'active', NULL, NULL),
    (8,  2, 'README.md',   'active', NULL, NULL),
    -- history only, and its blob is missing.
    (9,  2, 'orphan.txt',  'active', NULL, NULL),
    -- history only, recorded as a deletion with a NULL content_hash.
    (10, 2, 'removed.txt', 'active', NULL, NULL);

INSERT INTO content_blobs
    (hash_sha256, content_text, content_blob, content_type, file_size_bytes)
VALUES
    ('h-main',     'fn main() {}' || char(10) || 'second' || char(10) || 'third' || char(10),
                   NULL, 'text', 26),
    ('h-main-old', 'fn main() {}' || char(10), NULL, 'text', 13),
    ('h-readme-a', 'distinctiveword shared' || char(10), NULL, 'text', 23),
    ('h-readme-b', 'shared' || char(10), NULL, 'text', 7),
    ('h-ab',       'ab' || char(10),  NULL, 'text', 3),
    ('h-axb',      'axb' || char(10), NULL, 'text', 4),
    -- binary: content_text is NULL, which must read as an error rather
    -- than an empty file.
    ('h-png',      NULL, X'89504E47', 'binary', 4);
-- 'h-missing' is deliberately absent from content_blobs.

INSERT INTO file_contents
    (file_id, content_hash, file_size_bytes, line_count, is_current, updated_at)
VALUES
    (1, 'h-main',     26, 3,    1, '2026-10-05 00:00:00'),
    (3, 'h-main',     26, 3,    1, '2026-10-05 00:00:00'),
    (4, 'h-readme-a', 23, 1,    1, '2026-10-05 00:00:00'),
    (5, 'h-ab',        3, 1,    1, '2026-10-05 00:00:00'),
    (6, 'h-axb',       4, 1,    1, '2026-10-05 00:00:00'),
    (7, 'h-png',       4, NULL, 1, '2026-10-05 00:00:00'),
    (8, 'h-readme-b',  7, 1,    1, '2026-10-05 00:00:00');

-- rowid = project_files.id, which is what migration 110 established and
-- what the Python query failed to use. Two projects own a 'README.md';
-- only alpha's contains 'distinctiveword'.
INSERT INTO file_contents_fts (rowid, file_path, content_text) VALUES
    (4, 'README.md', 'distinctiveword shared'),
    (8, 'README.md', 'shared');

-- Hashes are stored uppercase here on purpose: the live data is bimodal
-- (older 16-char uppercase, newer 40-char lowercase) and lookup has to
-- be case-insensitive in both directions.
-- `alpha`'s active branch is `feature` (id 2) via `active_branch_id`,
-- while `main` (id 1) is the default — so the listing's `*` and
-- `(default)` markers land on different rows and cannot be confused.
-- `beta` has no `active_branch_id`, so its active branch comes from the
-- `is_default` fallback. `main` has a head commit; `feature` does not,
-- which is the NULL `last_message` case.
INSERT INTO vcs_branches (id, project_id, branch_name, is_default, head_commit_id) VALUES
    (1, 1, 'main',    1,    14),
    (2, 1, 'feature', 0,    NULL),
    (3, 2, 'main',    1,    20);

INSERT INTO vcs_commits
    (id, project_id, branch_id, commit_hash, author, commit_message,
     commit_timestamp, session_id)
VALUES
    (1, 1, 1, 'ABCD1234EFFF', 'zach', 'first',  '2026-01-01 00:00:00', NULL),
    (2, 2, 3, 'CAFE0001',     'zach', 'beta 1', '2026-01-02 00:00:00', NULL),
    (3, 2, 3, 'DEAD0002',     'zach', 'beta 2', '2026-01-03 00:00:00', NULL);

-- `vcs log` fixtures. C2 and C3 share a timestamp, which is the tie
-- Python's `ORDER BY commit_timestamp DESC` alone leaves to the
-- planner; C3 is session-owned so the published view must hide it; C9
-- names a branch id that does not exist, which Python's inner join
-- drops.
INSERT INTO vcs_commits
    (id, project_id, branch_id, commit_hash, author, commit_message,
     commit_timestamp, session_id)
VALUES
    (11, 1, 1, 'C1', 'zach',  'one',         '2026-02-01 00:00:00', NULL),
    (12, 1, 1, 'C2', 'zach',  'two',         '2026-02-02 00:00:00', NULL),
    (13, 1, 2, 'C3', 'agent', 'three',       '2026-02-02 00:00:00', 7),
    (14, 1, 1, 'C4', 'zach',  'four',        '2026-02-03 00:00:00', NULL),
    (19, 1, 99, 'C9', 'zach', 'dangling',    '2026-02-04 00:00:00', NULL),
    (20, 2, 3, 'B1', 'zach',  'beta commit', '2026-02-01 00:00:00', NULL);

INSERT INTO vcs_file_states
    (commit_id, file_id, change_type, content_hash, file_size, line_count)
VALUES
    (1, 1,  'modified', 'h-main-old', 13, 1),
    (2, 9,  'modified', 'h-missing',  10, 2),
    (3, 10, 'deleted',  NULL,         NULL, NULL),
    -- `vcs show`'s diff fixtures. C1 and C2 both touch src/main.rs, so
    -- C2-against-its-parent is a real one-line-to-three-lines diff
    -- rather than a whole-file addition.
    (11, 1, 'modified', 'h-main-old', 13, 1),
    (12, 1, 'modified', 'h-main',     26, 3),
    -- A binary file in a commit. Python's content lookup rejects
    -- anything whose content_type is not 'text', so both sides of the
    -- diff come back empty and the command says "No differences" — a
    -- wrong answer that is reproduced because it is on stdout.
    (20, 7, 'modified', 'h-png',      4,  NULL);

-- Parent edges. C1 is a root commit (absent here), C2's parent is C1,
-- C4's is C2.
--
-- C9 is given **two** parents, which the live database has no example
-- of: all 718 rows there have `parent_order = 0` because no merge has
-- ever been committed. Two things are untestable against live data
-- without it — that `ORDER BY parent_order` picks the first parent
-- rather than whichever the planner returns, and that the mirror's
-- keyset pagination over this table's composite primary key does not
-- drop the second row of a pair.
INSERT INTO vcs_commit_parents (commit_id, parent_commit_id, parent_order) VALUES
    (12, 11, 0),
    (14, 12, 0),
    (19, 12, 1),
    (19, 11, 0);

INSERT INTO entities (id, kind, external_ref, source_authority, label, observed_at) VALUES
    (1, 'Machine',    'box1',            'nix', 'box one',   '2026-10-01 00:00:00'),
    (2, 'Generation', 'gen-1',           'nix', 'gen one',   '2026-10-01 00:00:00'),
    -- NULL label: must stay absent rather than printing as "None".
    (3, 'Generation', 'gen-2',           'nix', NULL,        '2026-10-01 00:00:00'),
    (4, 'Generation', 'gen-3',           'nix', 'gen three', '2026-10-01 00:00:00'),
    (5, 'StorePath',  '/nix/store/sp1',  'nix', 'sp1',       '2026-10-01 00:00:00'),
    -- mod_a / modxa: the `entity search` counterpart of a_b.txt /
    -- axb.txt — a literal '_' in the query must not match the 'x'. Both
    -- carry a newer observed_at than the rows above so `ORDER BY
    -- observed_at DESC` is also exercised, and both are unconnected, so
    -- the relation tests are untouched.
    (6, 'Symbol',     'mod_a',           'ast', 'module mod_a', '2026-10-02 00:00:00'),
    (7, 'Symbol',     'modxa',           'ast', 'module modxa', '2026-10-02 00:00:00');

-- box1 fans out to three Generations; gen-1 installs a StorePath, which
-- points back at gen-1. That last edge is the cycle a depth-20 walk has
-- to survive.
INSERT INTO relations (from_entity_id, to_entity_id, kind, source_authority) VALUES
    (1, 2, 'ran',      'nix'),
    (1, 3, 'ran',      'nix'),
    (1, 4, 'ran',      'nix'),
    (2, 5, 'installs', 'nix'),
    (5, 2, 'used-by',  'nix');

-- Three rows for one entity sharing an `observed_at`, which is the tie
-- Python's `ORDER BY observed_at DESC` alone leaves to the planner —
-- and with a `--limit` a tie straddling the cutoff decides which rows
-- are shown at all. Also one row where only the authority moved and
-- one where only the label did, so the caller's two conditionals are
-- each exercised alone.
INSERT INTO observations_archive
    (id, entity_kind, entity_ref, label, source_authority, observed_at,
     entity_id, prior_label, prior_source_authority)
VALUES
    (1, 'Machine', 'box1', 'box one',  'nix', '2026-10-01 00:00:00', 1, 'box 1',   'nix'),
    (2, 'Machine', 'box1', 'box one',  'nix', '2026-10-02 00:00:00', 1, 'box one', 'manual'),
    (3, 'Machine', 'box1', 'box one',  'nix', '2026-10-03 00:00:00', 1, 'box one', 'nix'),
    (4, 'Machine', 'box1', 'box one',  'nix', '2026-10-03 00:00:00', 1, 'box 1',   'nix'),
    (5, 'Machine', 'box1', NULL,       'nix', '2026-10-03 00:00:00', 1, 'box one', 'nix');
"#;

/// The `File -imports-> File` graph `entity dead-imports` reads, loaded
/// only by [`fixture_with_imports`].
///
/// Four files in two projects, arranged so all three shapes the query
/// distinguishes are present: an import bridged by a symbol call, an
/// import with symbols on both ends but no call between them, and an
/// import whose source file defines no symbols at all — which is the
/// case the chain of `LEFT JOIN`s exists for and the one an `INNER
/// JOIN` would silently drop.
///
/// `proj_a` / `projxa` is this command's version of the
/// `mod_a`/`modxa` pair in [`DATA`]: `--slug proj_a` must not reach
/// `projxa` through an unescaped LIKE wildcard. Python's filter does.
const IMPORT_GRAPH: &str = r#"
INSERT INTO entities (id, kind, external_ref, source_authority, label, observed_at) VALUES
    (10, 'File',   'proj_a/app.py',         'ast', NULL, '2026-10-01 00:00:00'),
    (11, 'File',   'proj_a/used.py',        'ast', NULL, '2026-10-01 00:00:00'),
    (12, 'File',   'proj_a/dead.py',        'ast', NULL, '2026-10-01 00:00:00'),
    (13, 'File',   'proj_a/bare.py',        'ast', NULL, '2026-10-01 00:00:00'),
    (14, 'File',   'projxa/app.py',         'ast', NULL, '2026-10-01 00:00:00'),
    (15, 'File',   'projxa/dead.py',        'ast', NULL, '2026-10-01 00:00:00'),
    (16, 'Symbol', 'proj_a/app.py:caller',  'ast', NULL, '2026-10-01 00:00:00'),
    (17, 'Symbol', 'proj_a/used.py:callee', 'ast', NULL, '2026-10-01 00:00:00'),
    (18, 'Symbol', 'proj_a/dead.py:unused', 'ast', NULL, '2026-10-01 00:00:00');

INSERT INTO relations (from_entity_id, to_entity_id, kind, source_authority) VALUES
    -- app.py imports used.py and dead.py; bare.py imports used.py.
    (10, 11, 'imports', 'ast'),
    (10, 12, 'imports', 'ast'),
    (13, 11, 'imports', 'ast'),
    (14, 15, 'imports', 'ast'),
    -- app.py defines caller, used.py defines callee, dead.py defines
    -- unused. bare.py defines nothing: the LEFT JOIN case.
    (10, 16, 'defines', 'ast'),
    (11, 17, 'defines', 'ast'),
    (12, 18, 'defines', 'ast'),
    -- caller calls callee, so app.py -> used.py is bridged. Nothing
    -- calls `unused`, so app.py -> dead.py is a candidate.
    (16, 17, 'calls',   'ast');
"#;
