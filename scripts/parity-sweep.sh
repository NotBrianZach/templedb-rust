#!/usr/bin/env bash
#
# Diff every implemented command against the Python `templedb`, or across
# the two backends, or against a previous build.
#
# This exists because per-command harnesses do not catch drift. Each one
# was written while its command was being ported and retired when the
# command was declared done, and on 2026-10-14 an audit found three
# commands that had silently fallen behind Python — `entity stats` was
# missing its entire `relations:` section, `file ls -l` was missing a
# column added by migration 106, and `source revisions` rendered NULL
# differently. Every number those commands *did* print agreed, which is
# exactly why a diff of one command at a time never caught it.
#
# Two of the three were found only once all 38 invocations ran in one
# pass. The third — the blob-classification regression — was found by a
# `--side-by-side` harness that happened to also run the unified
# renderer. The common thread is coverage beyond the thing being worked
# on, so this script is deliberately the whole surface and not a
# parameterised subset.
#
# Usage:
#   scripts/parity-sweep.sh python           # vs the Python templedb (default)
#   scripts/parity-sweep.sh backends PGURL   # SQLite vs a mirrored Postgres
#   scripts/parity-sweep.sh regress OLD_BIN  # vs a previous templedb-rs build
#
# Exit status is the number of unexpected differences, so this is usable
# as a check. Known-deliberate divergences are listed in EXPECTED below
# and reported separately rather than counted.
set -uo pipefail

MODE="${1:-python}"
ARG="${2:-}"
RS="${TEMPLEDB_RS:-./target/release/templedb-rs}"

if [[ ! -x $RS ]]; then
    echo "no templedb-rs at $RS (set TEMPLEDB_RS, or cargo build --release)" >&2
    exit 2
fi

# Every implemented command, with the arguments that reach its
# interesting branches. Where a row exists to cover one specific
# behaviour, the comment says which.
INVOCATIONS=(
    "project list"
    "project show templedb"
    # SUM(lines_of_code) over no non-NULL values: Python used to crash here.
    "project show cathedraldb"
    "entity stats"
    "file ls templedb"
    "file ls templedb src -l"
    # A prefix whose only matching file is status='deleted'.
    "file ls templedb src/gui_pages"
    # All live rows have a NULL edit_mode, so both of these are empty —
    # which is the answer, and differing on it would be the bug.
    "file ls templedb --edit-mode non-immutable"
    "file ls templedb -l --edit-mode immutable"
    "file cat templedb src/cli/commands/entity.py"
    "entity explore Machine/zMothership2"
    "entity trace Machine/zMothership2 --depth 1"
    "entity trace Machine/zMothership2 --depth 2"
    "entity trace Machine/zMothership2 --depth 3"
    "entity trace Machine/zMothership2 --depth 3 --direction both --via ran,installs --limit 3"
    "entity trace Machine/zMothership2 --direction in --limit 1"
    # Above the fan-out, so the truncation advisory must *not* print.
    "entity trace Machine/zMothership2 --limit 500"
    "entity search zMothership --limit 5000"
    # A literal underscore, which LIKE would read as a wildcard.
    "entity search deploy_ --limit 5000"
    "entity search deploy --kind Symbol --limit 100"
    "entity dead-imports --limit 50"
    "entity observations Machine/zMothership2 --limit 50"
    # A file with deleted revisions: NULL hash, size and line count.
    "source revisions templedb CHANGELOG.md"
    "source snapshot templedb src/cli/commands/entity.py --meta"
    "source snapshot templedb CHANGELOG.md --rev fff643edfa4a --meta"
    "vcs log templedb --all-sessions -n 10"
    # The default view, which Python widens with your staging session's
    # commits and this port does not. A deliberate divergence, kept in
    # the sweep so that it stays *visible* rather than forgotten.
    "vcs log templedb -n 10"
    "vcs branch templedb"
    # Three blobs typed text/x-prolog: the misclassification case.
    "vcs show templedb 40E93339139FA809 --no-color"
    # 24 binary files in one commit, 52k lines of output.
    "vcs show bza FBC7ECF45AE34942 --no-color"
    # 68 files whose content_hash names no blob.
    "vcs show nixos_anywhere_vultr_woofs 1C04E7EF81BD7A3C --no-color"
    "vcs diff templedb src/cli/commands/vcs.py 6FABDF9D66BB2951 2A44DE2D9062BC2F --no-color"
    "vcs diff templedb src/cli/commands/vcs.py 6FABDF9D66BB2951 2A44DE2D9062BC2F -s --no-color"
    # A file deleted in the older of the two revisions.
    "vcs diff templedb src/gui_pages/README.md B65BADB158475631 A2DD61F68EDFB702 -s --no-color"
    "search content rusqlite"
    "search content checkout -p templedb"
    "entity paths Commit/templedb/FBDACA1F09FBBEF7 Commit/templedb/E534AF92F549CDDD"
    # No path, so the whole reachable component is exhausted.
    "entity paths Machine/zMothership2 Machine/web1 --max-depth 6"
    # Across a node with more than Python's old hardcoded 100 edges.
    "entity paths AgentSession/c3129fbf-4fc3-45b7-ad9e-908c32d0408c ToolCall/17815"
    # An entity that does not exist: Python exits 2, this exits 1. The
    # one exit-code divergence left, and a constructed ref rather than a
    # real-but-deleted one so the row cannot start passing when the data
    # changes.
    "entity paths Machine/no-such-machine-ever Machine/zMothership2"
)

# Divergences that are deliberate and documented in the README. Matched
# as a prefix of the invocation. Keeping them here rather than deleting
# the rows means a divergence that *closes* shows up as a surprise —
# which is how `entity search`'s ordering was found to have converged.
EXPECTED=(
    # Different table entirely: Python has a name column and a heading.
    "project list"
    # Python's query has no ORDER BY; this sorts. See the README.
    "entity explore"
    # Python's default view includes your staging session's commits.
    "vcs log templedb -n"
    # Python exits 2 for a missing entity, 1 for a malformed address.
    "entity paths Machine/no-such-machine-ever"
    # Substring not stemmed, and a different layout.
    "search content"
)

is_expected() {
    local inv=$1 e
    for e in "${EXPECTED[@]}"; do
        [[ $inv == "$e"* ]] && return 0
    done
    return 1
}

case $MODE in
python)
    label="python"
    run_b() { templedb "$@"; }
    ;;
backends)
    [[ -n $ARG ]] || { echo "backends mode needs a Postgres URL" >&2; exit 2; }
    # Refuse to compare against a stale mirror.
    #
    # This mode diffs SQLite against Postgres, so it is only a test of
    # *this port* while both hold the same data. The moment the live
    # SQLite moves, it silently becomes a test of replication lag. On
    # 2026-10-16 a `publish` added rows and this went from 40 of 40
    # identical to six differences -- project list, entity stats,
    # entity explore, entity trace and two entity search rows -- none of
    # which was a bug. Six convincing parity failures is a worse outcome
    # than a refusal, because somebody will spend an afternoon on them.
    if ! "$RS" mirror --check --to "$ARG" >/dev/null 2>&1; then
        echo "refusing to sweep: the mirror is stale or cannot vouch for itself" >&2
        "$RS" mirror --check --to "$ARG" 2>&1 >/dev/null | sed 's/^/  /' >&2
        echo "  run \`just mirror-refresh\` first" >&2
        exit 2
    fi
    label="postgres"
    run_b() { TEMPLEDB_URL="$ARG" "$RS" "$@"; }
    # The mirror is as current as the last `mirror` run and the live
    # SQLite file is written by the Python implementation while this
    # runs, so a stale mirror shows up here as a row-count difference
    # that looks exactly like a translation bug. Re-mirror before
    # believing a failure.
    EXPECTED=()
    ;;
regress)
    [[ -x $ARG ]] || { echo "regress mode needs an executable old binary" >&2; exit 2; }
    label="old build"
    run_b() { "$ARG" "$@"; }
    # Every difference is interesting when comparing two builds of this
    # port: the point is to notice unintended ones.
    EXPECTED=()
    ;;
*)
    echo "unknown mode: $MODE (python | backends | regress)" >&2
    exit 2
    ;;
esac

# **stdout and the exit code are the contract; stderr is not.** The two
# implementations disagree about stderr by design and the README records
# it as a known gap: Python routes diagnostics through `logging`, so they
# arrive prefixed `ERROR` / `INFO`, and this port writes them plain.
# `file ls` additionally prints a `N file(s)` count that Python does not,
# which is on stderr precisely so stdout stays diffable.
#
# Folding stderr in would make seven of these rows permanently red and
# the script useless as a check. They are counted separately instead, so
# a *new* stderr difference is still visible without being an error.
pass=0 unexpected=0 expected=0 stderr_only=0
errfile_a=$(mktemp) errfile_b=$(mktemp)
trap 'rm -f "$errfile_a" "$errfile_b"' EXIT

for inv in "${INVOCATIONS[@]}"; do
    # Unquoted on purpose: the rows above are argument lists.
    # shellcheck disable=SC2086
    a=$("$RS" $inv 2>"$errfile_a"); ea=$?
    # shellcheck disable=SC2086
    b=$(run_b $inv 2>"$errfile_b"); eb=$?
    same_err=no
    cmp -s "$errfile_a" "$errfile_b" && same_err=yes

    if [[ $a == "$b" && $ea == "$eb" ]]; then
        pass=$((pass + 1))
        if [[ $same_err == no ]]; then
            stderr_only=$((stderr_only + 1))
            printf 'stderr    %s\n' "$inv"
        fi
    elif is_expected "$inv"; then
        expected=$((expected + 1))
        printf 'expected  %s\n' "$inv"
    else
        unexpected=$((unexpected + 1))
        printf 'DIFFERS   %s   (exit %s vs %s)\n' "$inv" "$ea" "$eb"
        diff <(printf '%s\n' "$a") <(printf '%s\n' "$b") | head -8
    fi
done

printf '\ntempledb-rs vs %s, %d invocations: %d stdout-identical (%d differing on stderr only), %d deliberate, %d unexpected\n' \
    "$label" "${#INVOCATIONS[@]}" "$pass" "$stderr_only" "$expected" "$unexpected"
exit $((unexpected > 125 ? 125 : unexpected))
