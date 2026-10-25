# Checks for templedb-rust.
#
# The point of this file is `just check`, which is the full gate: the
# hermetic tests and then the all-command parity sweep. Before it
# existed, the sweep was a script nothing ran — it was written on
# 2026-10-14 after an audit found three commands that had silently
# drifted behind Python, and a check that only runs when someone
# remembers it is the same failure mode one layer up.
#
# The split between `just` and `nix flake check` follows what needs the
# live database. `nix flake check` builds in a sandbox and can run the
# unit tests and the difflib oracle; it cannot run the sweep, which
# needs the real SQLite file and the Python `templedb` on PATH. So the
# sweep lives here and only here.
#
# Note on layout: a comment block directly above a recipe becomes its
# `just --list` description, so the long explanations sit above a blank
# line and only the one-liners touch the recipe.
#
# The sweep is invoked as `bash scripts/...` rather than executed
# directly. TempleDB's content store does not carry the execute bit, so
# a fresh `templedb edit` workspace — which is where this file runs —
# has the script at 644 and the shebang is unreachable: a plain
# `scripts/parity-sweep.sh` fails with "Permission denied". `publish`
# restores the bit from git's index in the canonical checkout, which is
# read-only and not where the sweep gets run. `chmod +x` here works
# until the next materialise.

# Hermetic tests, then the sweep against Python.
default: check

# Unit tests: everything needing no database and no interpreter.
test:
    cargo test --workspace

# The CPython oracles. Two of them now:
#
#   difflib_parity  ~700 generated cases, repeated-block tie-breaks and
#                   this repository's own source, through both the
#                   unified and context renderers.
#   clock_parity    2,000 timestamp pairs through SQLite's julianday and
#                   Python's _age_hint, half of them landing on an exact
#                   truncation boundary.
#
# Both are `#[ignore]`d in `cargo test` because they shell out to
# python3, so they need their own invocation. Run from the crate
# directory because `real_source_files_diff_identically` reads
# `src/difflib.rs` by relative path.

# The CPython oracles: difflib and the clock (needs python3).
oracle:
    cd crates/templedb-db && cargo test --test difflib_parity -- --ignored
    cd crates/templedb-db && cargo test --test clock_parity -- --ignored

# All 54 implemented invocations, diffed against the Python templedb,
# plus `summary` compared one pane at a time — its whole stdout can
# never match, because two of Python's nine panes are not ported.
#
# Impure by construction: needs the live database and `templedb` on
# PATH. Exits with the number of *unexpected* differences, so a
# deliberate divergence does not fail it — the list lives in the script.

# All 54 invocations plus summary's panes, diffed against the Python templedb.
sweep: build
    bash scripts/parity-sweep.sh python

# The same sweep across backends. PGURL is a mirrored Postgres; see
# `templedb-rs mirror`.

# SQLite vs a mirrored Postgres, same 54 invocations.
sweep-backends PGURL: build
    bash scripts/parity-sweep.sh backends {{PGURL}}

# The sweep against a previous templedb-rs build (the regression form).
sweep-regress OLDBIN: build
    bash scripts/parity-sweep.sh regress {{OLDBIN}}

# Release binary, which the sweep recipes diff.
build:
    cargo build --release

# Everything. Run this before publishing.
check: test oracle sweep

# Only the half `nix flake check` can run without a live database.
check-hermetic:
    nix flake check

# --- Postgres ---------------------------------------------------------
#
# A cluster this project owns, from the postgresql the flake pins. Not a
# system service: the port is a separate project that owns its database,
# the scratch half is meant to be disposable, and needing
# `nixos system-switch` to change a test database would be miserable.
#
# Port 55432, not 5432, so this can never be confused with -- or collide
# with -- a host instance. PGDATA sits under the project and is
# gitignored; it is also not DB-tracked, so `templedb commit` will not
# try to swallow a database cluster.
#
# TWO databases, because `mirror` opens with TRUNCATE:
#
#   templedb_parity   faithful copy, written ONLY by `mirror`, read by
#                     the sweep. The port connects as a read-only role
#                     so it cannot write this one by mistake -- the
#                     server-enforced replacement for SQLite's
#                     SQLITE_OPEN_READ_ONLY flag.
#   templedb_scratch  the write-path target. Disposable, re-mirrored at
#                     will, read-write.
#
# Sharing one database would make a sweep run after a write test report
# the port's own writes as parity failures, and the next re-mirror would
# silently destroy whatever the write tests built.

# The cluster lives OUTSIDE the project tree, and must.
#
# It was under `.pgdata/` for one commit and that was a mistake: a
# `.gitignore` entry does not protect anything here, because TempleDB's
# importer only consults git's file list when the tree *is* a git repo,
# and an edit workspace is not one. So the scanner classified
# `postgresql.conf` and friends as `config_file` and `templedb commit`
# pulled four cluster files into the database (commit 1348, removed in
# the next one). A live database directory inside a tree that gets
# scanned and materialised is a bad arrangement in both directions:
# cluster internals end up in content_blobs, and a later materialise
# could write over a running server's state.
#
# $XDG_DATA_HOME rather than $XDG_RUNTIME_DIR: the parity database is a
# ~190k-row mirror that costs a `just mirror` to rebuild, so it should
# survive a logout. Only the socket is in the runtime dir.
PGDATA := env_var_or_default("XDG_DATA_HOME", env_var("HOME") / ".local/share") / "templedb-rs/pgdata"
PGPORT := "55432"

# The socket directory is NOT under PGDATA, and cannot be. A Unix-domain
# socket path is capped at 107 bytes, and this project is normally
# checked out into a TempleDB edit workspace:
#
#   .../edit-workspaces/templedb-rust/claude-code-agent-22526/.pgdata/sock/.s.PGSQL.55432
#
# is 109 bytes, so the server starts, fails to create any socket, and
# exits FATAL. Nothing about that error mentions path depth until you
# read the log. $XDG_RUNTIME_DIR is short (44 bytes for the same socket),
# user-private, and cleaned up on logout.
#
# Socket only, no TCP: `listen_addresses=` is empty in pg-up, so trust
# auth is reachable solely by this user through a 0700 directory rather
# than by anyone who can open a local port.
PGHOST := env_var_or_default("XDG_RUNTIME_DIR", "/tmp") / "templedb-rs-pg"

# Initialise the cluster if absent, start it, create both databases.
pg-up:
    #!/usr/bin/env bash
    set -euo pipefail
    if [ ! -d "{{PGDATA}}" ]; then
        echo "initdb -> {{PGDATA}}"
        initdb --pgdata="{{PGDATA}}" --auth=trust --no-locale --encoding=UTF8 >/dev/null
    fi
    # Recreated every time: $XDG_RUNTIME_DIR does not survive logout, so
    # a cluster that outlives a session would otherwise fail to restart.
    mkdir -p "{{PGHOST}}" && chmod 700 "{{PGHOST}}"
    if pg_ctl --pgdata="{{PGDATA}}" status >/dev/null 2>&1; then
        echo "already running"
    else
        pg_ctl --pgdata="{{PGDATA}}" \
               --options="-p {{PGPORT}} -k {{PGHOST}} -c listen_addresses=" \
               --log="{{PGDATA}}/server.log" start
    fi
    # Idempotent: both may already exist.
    createdb -h "{{PGHOST}}" -p {{PGPORT}} templedb_parity 2>/dev/null || true
    createdb -h "{{PGHOST}}" -p {{PGPORT}} templedb_scratch 2>/dev/null || true
    # The read-only role on the parity database. This is the part that
    # makes "two databases" a guarantee rather than a convention: the
    # port reads parity as templedb_ro and is refused by the server if
    # it tries to write, which is what replaces SQLITE_OPEN_READ_ONLY.
    # `mirror` connects as the owner instead, because it must TRUNCATE
    # and COPY.
    psql -h "{{PGHOST}}" -p {{PGPORT}} -d postgres -q -c \
        "DO \$\$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='templedb_ro')
         THEN CREATE ROLE templedb_ro LOGIN; END IF; END \$\$;"
    # Applied to existing tables and, via default privileges, to the
    # ones a later `mirror --create-schema` will create.
    psql -h "{{PGHOST}}" -p {{PGPORT}} -d templedb_parity -q \
        -c "GRANT CONNECT ON DATABASE templedb_parity TO templedb_ro" \
        -c "GRANT USAGE ON SCHEMA public TO templedb_ro" \
        -c "GRANT SELECT ON ALL TABLES IN SCHEMA public TO templedb_ro" \
        -c "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO templedb_ro"
    psql -h "{{PGHOST}}" -p {{PGPORT}} -d postgres -tAc \
        "SELECT datname FROM pg_database WHERE datname LIKE 'templedb%' ORDER BY datname"

pg-down:
    pg_ctl --pgdata="{{PGDATA}}" stop || true

pg-status:
    pg_ctl --pgdata="{{PGDATA}}" status || true

# The parity database is a copy and the scratch one is disposable by
# design, so this loses nothing a `mirror` run cannot rebuild.

# Stop the cluster and delete it.
pg-nuke: pg-down
    rm -rf "{{PGDATA}}"

# Owner connection: only `mirror` should use this one.
PG_PARITY_RW := "postgresql://?host=" + PGHOST + "&port=" + PGPORT + "&dbname=templedb_parity"
# Read-only connection: everything that reads parity uses this.
PG_PARITY_RO := "postgresql://templedb_ro@?host=" + PGHOST + "&port=" + PGPORT + "&dbname=templedb_parity"
# The write-path target. Read-write by design, disposable.
PG_SCRATCH := "postgresql://?host=" + PGHOST + "&port=" + PGPORT + "&dbname=templedb_scratch"

# --create-schema is the destructive half, so it belongs on a first run
# only: on an existing mirror the CREATE TABLEs fail and nothing is
# copied, which is the right outcome for a typo'd URL.

# Copy the read subset from live SQLite into templedb_parity (first run).
mirror: build
    ./target/release/templedb-rs mirror --to "{{PG_PARITY_RW}}" --create-schema

mirror-refresh: build
    ./target/release/templedb-rs mirror --to "{{PG_PARITY_RW}}"

# Is the parity database still a faithful copy? Exits 4 if not.
pg-check: build
    ./target/release/templedb-rs mirror --check --to "{{PG_PARITY_RO}}"

# The cross-backend sweep against the local parity database, through the
# read-only role. Refuses to run on a stale mirror -- see the staleness
# note in scripts/parity-sweep.sh.
sweep-pg: build
    bash scripts/parity-sweep.sh backends "{{PG_PARITY_RO}}"

# Refresh first, then sweep. The convenience form, and the one to reach
# for in practice: any `templedb commit` anywhere adds rows and makes
# the mirror stale, so the bare `sweep-pg` refuses more often than not.
#
# Deliberately NOT what `sweep-pg` does on its own. Auto-refreshing
# would hide the staleness the check exists to surface, and a sweep that
# silently re-copies 194k rows before every run is a different, slower
# command wearing the same name.
sweep-pg-fresh: mirror-refresh sweep-pg

# Prove the read-only role is actually enforced rather than assumed.
# Expected: the SELECT succeeds and the UPDATE is refused.
pg-verify-ro:
    #!/usr/bin/env bash
    set -uo pipefail
    echo -n "read as templedb_ro:  "
    psql "{{PG_PARITY_RO}}" -tAc "SELECT count(*) FROM projects" || echo FAILED
    echo -n "write as templedb_ro: "
    if psql "{{PG_PARITY_RO}}" -q -c "UPDATE projects SET name = name" 2>/dev/null; then
        echo "ACCEPTED -- the role is not read-only, this is a bug"
        exit 1
    else
        echo "refused (correct)"
    fi

# The write path, against the scratch Postgres (needs a server).
#
# #[ignore]d in `cargo test` for the same reason the CPython oracles
# are: it needs something the build does not. Keeping it out means
# `cargo test` still runs with no server, which is a property the README
# advertises and which the write path must not quietly cost.
#
# PG_SCRATCH, never PG_PARITY_*: `mirror` opens the parity database
# with TRUNCATE, and a write test that landed there would be destroyed
# by the next re-mirror while reporting its own writes as parity
# failures in between.
#
# --test-threads=1 because every test creates and drops a project, and
# two of them racing on `MAX(id) + 1000` would collide on the slug's
# UNIQUE constraint.
write-test:
    TEMPLEDB_WRITE_URL="{{PG_SCRATCH}}" \
        cargo test --test write_parity -- --ignored --test-threads=1

# Create the write-scratch schema from the subset DDL (first run).
#
# Separate from `mirror`: the scratch database does not need the data,
# only the shape — and pointing `mirror` at it would make the write
# target a TRUNCATE target, which is the arrangement the two databases
# exist to prevent.
pg-scratch-schema:
    psql "{{PG_SCRATCH}}" -q -f schema/postgres/001_read_subset.sql

# Both databases carry the subset DDL, and it is not static: it has
# grown a column or an identity every time a write path landed —
# `edit_intents` with the intent record, `vcs_commits` and
# `vcs_file_states` with `vcs commit-staged`. `--create-schema` is
# CREATE-only, so it cannot apply a change to a database that already
# has the tables, and the symptom is remote from the cause: a `mirror`
# run fails inside a binary COPY with a column count mismatch.
#
# Recreating is the right move rather than writing ALTERs. Neither
# database is a source of truth — parity is a copy and scratch is
# disposable — so the DDL file stays the single declaration of the shape
# and nothing has to keep a migration list in step with it.
#
# Destructive, and deliberately not a dependency of anything: `mirror`
# refilling parity afterwards is a 222,890-row copy, and the write tests
# rebuild their own fixtures.

# Rebuild both databases' schemas from the DDL. Destructive.
pg-reschema:
    #!/usr/bin/env bash
    set -euo pipefail
    for url in "{{PG_PARITY_RW}}" "{{PG_SCRATCH}}"; do
        psql "$url" -q -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public;"
        psql "$url" -q -f schema/postgres/001_read_subset.sql
        echo "reschema'd $(psql "$url" -tAc 'SELECT current_database()')"
    done
    # DROP SCHEMA took the grants with it, so the read-only role loses
    # its SELECT and every parity read starts failing with "permission
    # denied for table projects". Re-applying is the whole reason this
    # recipe exists rather than two psql calls in a shell.
    psql "{{PG_PARITY_RW}}" -q \
        -c "GRANT USAGE ON SCHEMA public TO templedb_ro" \
        -c "GRANT SELECT ON ALL TABLES IN SCHEMA public TO templedb_ro" \
        -c "ALTER DEFAULT PRIVILEGES IN SCHEMA public GRANT SELECT ON TABLES TO templedb_ro"
    echo "now run: just mirror-refresh"
