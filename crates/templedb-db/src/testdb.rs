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

use rusqlite::Connection;

pub fn fixture() -> Connection {
    let conn = Connection::open_in_memory().expect("in-memory sqlite");
    crate::apply_standard_pragmas(&conn).expect("pragmas");
    conn.execute_batch(SCHEMA).expect("fixture schema");
    conn.execute_batch(DATA).expect("fixture data");
    conn
}

const SCHEMA: &str = r#"
CREATE TABLE projects (
    id   INTEGER PRIMARY KEY,
    slug TEXT NOT NULL UNIQUE,
    name TEXT
);

CREATE TABLE project_files (
    id         INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL,
    file_path  TEXT NOT NULL,
    status     TEXT DEFAULT 'active',
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

CREATE TABLE vcs_commits (
    id               INTEGER PRIMARY KEY,
    project_id       INTEGER NOT NULL,
    commit_hash      TEXT NOT NULL UNIQUE,
    commit_timestamp TEXT NOT NULL
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
INSERT INTO projects (id, slug, name) VALUES
    (1, 'alpha', 'Alpha Project'),
    (2, 'beta',  'beta'),
    (3, 'empty', NULL);

INSERT INTO project_files (id, project_id, file_path, status) VALUES
    (1,  1, 'src/main.rs', 'active'),
    -- active but with no current content row: must list at 0 lines,
    -- not disappear.
    (2,  1, 'notes.md',    'active'),
    -- has content, but deleted: must not list and must not be readable.
    (3,  1, 'gone.txt',    'deleted'),
    (4,  1, 'README.md',   'active'),
    -- a_b.txt / axb.txt: the pair that distinguishes a literal '_'
    -- from a LIKE wildcard.
    (5,  2, 'a_b.txt',     'active'),
    (6,  2, 'axb.txt',     'active'),
    (7,  2, 'logo.png',    'active'),
    (8,  2, 'README.md',   'active'),
    -- history only, and its blob is missing.
    (9,  2, 'orphan.txt',  'active'),
    -- history only, recorded as a deletion with a NULL content_hash.
    (10, 2, 'removed.txt', 'active');

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
INSERT INTO vcs_commits (id, project_id, commit_hash, commit_timestamp) VALUES
    (1, 1, 'ABCD1234EFFF', '2026-01-01 00:00:00'),
    (2, 2, 'CAFE0001',     '2026-01-02 00:00:00'),
    (3, 2, 'DEAD0002',     '2026-01-03 00:00:00');

INSERT INTO vcs_file_states
    (commit_id, file_id, change_type, content_hash, file_size, line_count)
VALUES
    (1, 1,  'modified', 'h-main-old', 13, 1),
    (2, 9,  'modified', 'h-missing',  10, 2),
    (3, 10, 'deleted',  NULL,         NULL, NULL);

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
"#;
