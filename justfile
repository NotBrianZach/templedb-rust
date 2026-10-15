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

# The CPython difflib oracle: ~700 generated cases, repeated-block
# tie-breaks and this repository's own source, through both the unified
# and context renderers.
#
# `#[ignore]`d in `cargo test` because it shells out to python3, so it
# needs its own invocation. Run from the crate directory because
# `real_source_files_diff_identically` reads `src/difflib.rs` by
# relative path.

# The CPython difflib oracle (needs python3).
oracle:
    cd crates/templedb-db && cargo test --test difflib_parity -- --ignored

# All 40 implemented invocations, diffed against the Python templedb.
#
# Impure by construction: needs the live database and `templedb` on
# PATH. Exits with the number of *unexpected* differences, so a
# deliberate divergence does not fail it — the list lives in the script.

# All 40 invocations, diffed against the Python templedb.
sweep: build
    bash scripts/parity-sweep.sh python

# The same sweep across backends. PGURL is a mirrored Postgres; see
# `templedb-rs mirror`.

# SQLite vs a mirrored Postgres, same 40 invocations.
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
