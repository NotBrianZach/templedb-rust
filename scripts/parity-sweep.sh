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
    # The published view, which needs no session on either side and is
    # this port's default. Byte-identical exactly when your session has
    # nothing pending: Python's `if visibility == 'published' and
    # pending` adds a one-line advisory otherwise, and `just check` runs
    # *before* a publish, which is when pending is non-zero. So this row
    # passes when it can and is listed in EXPECTED for when it cannot —
    # the one arrangement that never reports a false failure.
    "vcs log templedb --published -n 10"
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
    # The error statuses. Python separates bad input (1) from an
    # address that names nothing (2), and until 2026-10-17 everything
    # here left through one `anyhow` path and exited 1.
    #
    # Every one of these prints nothing on stdout, so the exit code is
    # the entire content of the row — which is the reason they are rows
    # at all rather than a note. Constructed refs rather than
    # real-but-deleted ones, so none of them can start passing when the
    # data moves.
    #
    # `paths` is here twice because it resolves two addresses and tags
    # them separately; the second is the one that was failing before
    # the fix, since the first endpoint exists.
    "entity explore Machine/no-such-machine-ever"
    "entity trace Machine/no-such-machine-ever"
    "entity paths Machine/no-such-machine-ever Machine/zMothership2"
    "entity paths Machine/zMothership2 Machine/no-such-machine-ever"
    "source revisions templedb no/such/file.txt"
    "source snapshot templedb no/such/file.txt"
    # Exit 1, not 2: a malformed address is bad input on both sides.
    # Here to pin the *other* half of the split — a change that tagged
    # every error as not-found would pass all the rows above.
    "entity explore Machine"
    "entity paths Machine Machine/zMothership2"
    # Also 1: a project that does not resolve, and a commit that does
    # not exist. Python gives neither of them a 2.
    "source revisions no-such-project-ever CHANGELOG.md"
    "vcs show templedb no-such-commit-ever --no-diff"
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
    # Only when the session has pending commits; see the row's comment.
    "vcs log templedb --published"
    # Substring not stemmed, and a different layout.
    "search content"
)

# `summary` is compared one pane at a time, not as one blob.
#
# It is the first command whose whole stdout can never match: two of
# Python's nine panes are not ported — one reads systemd and one reads
# tables outside the mirrored subset — so a row in INVOCATIONS above
# would land in EXPECTED permanently and take the *other seven* panes
# with it. Six of those seven are byte-identical with Python on live
# data, and a check that cannot say so is not worth running.
#
# Panes the port implements and claims parity for.
SUMMARY_PANES=(
    # No header of its own: the lines between the title and the first
    # pane rule.
    "═════ TempleDB Summary ═════"
    "── Ingestion (per adapter) ──"
    "── Doctor invariants (latest results) ──"
    "── Reconciliations (discrepancies being worked on) ──"
    "── Reconcile (per fleet machine) ──"
    "── Python hygiene ──"
    "── Handoff inbox ──"
)

# Panes deliberately absent, reported rather than skipped. Listed so
# that porting one shows up here as a surprise.
SUMMARY_UNPORTED=(
    "── Agent fleet (per project) ──"
    "── Systemd units ──"
)

# Pane-level divergences that are deliberate.
SUMMARY_EXPECTED=(
    # Python's listing orders by `datetime(first_detected_at)` with no
    # tie-break, and three of the eight open rows share a timestamp to
    # the second — one `doctor entities` run detected them together. The
    # pane prints four rows, so *which* of the tied three appear is the
    # planner's choice. This port adds `, kind, subject_ref`. Same
    # reasoning as `entity trace`.
    "── Reconciliations (discrepancies being worked on) ──"
)

# --- flag surfaces -----------------------------------------------------
#
# The sweep above cannot see this class of difference at all, and the
# README has carried that as a known gap since the audit.
#
# A diff of outputs only compares flags that *both* implementations
# define, because a flag only one side knows is never passed to the
# other. So a missing flag is invisible until a script hits it: the
# audit found `search content --limit` by passing it to Python and
# getting exit 2, by hand. Comparing the surfaces directly is a
# different question — "can the same command line be typed at both" —
# and it is the one a caller actually cares about.
#
# It found four undocumented divergences the first time it ran, across
# three commands that were all reported as byte-identical.
#
# One command per read-only family, with placeholder arguments: both
# implementations parse `--help` before validating positionals, so the
# arguments only have to be present, not real.
FLAG_COMMANDS=(
    "project list"
    "project show x"
    "file cat p f"
    "file ls p"
    "file set p f"
    "entity stats"
    "entity explore K/r"
    "entity trace K/r"
    "entity search q"
    "entity paths A/b C/d"
    "entity dead-imports"
    "entity observations K/r"
    "search content q"
    "source snapshot p f"
    "source revisions p f"
    "vcs log p"
    "vcs show p c"
    "vcs branch p"
    "vcs diff p f"
    "summary"
)

# Flag divergences that are deliberate, as `<command>|<flag>` pairs.
# Every one of these is argued in the README; the pair form means a
# *new* flag on an already-diverging command is still caught.
FLAGS_EXPECTED=(
    # Writes. A read-only port cannot have them.
    "vcs branch p|-d"
    "vcs branch p|--delete"
    # Needs the staging session, which is a write and six environment
    # fallbacks. See `vcs log has no session-scoped default`.
    "vcs diff p f|--staged"
    "vcs log p|--session"
    # Dead unless --no-fts is passed too, and what it selects is this
    # port's default behaviour.
    "search content q|-i"
    "search content q|--ignore-case"
    # Would have to be accepted and ignored, which is worse than absent:
    # a flag promising different matching that delivers none.
    "search content q|--no-fts"
    # The one place this port is deliberately more capable: Python's FTS
    # path applies no limit at all.
    "search content q|-l"
    "search content q|--limit"
    # Python always colours and has no flag; this one does.
    "summary|--no-color"
    # `file set` is the first write and is deliberately partial. Every
    # one of these is a guard or a feature named in the README's
    # *The first write*, and the command is behind
    # TEMPLEDB_RS_ALLOW_WRITES because that list is not visible from a
    # command line. Listed one flag at a time rather than waved at,
    # so porting any of them shows up here as a surprise — which is
    # exactly what happened to `--verify` and `--allow-mojibake`, both
    # of which were here for one commit and are now implemented.
    #
    # Nothing to skip without `edit_intents`, whose table is outside
    # the mirrored subset.
    "file set p f|--skip-intent"
    # Staging and committing both need sessions, which the port has no
    # part of yet.
    "file set p f|-s"
    "file set p f|--stage"
    "file set p f|--session"
    "file set p f|--commit"
    "file set p f|--commit-msg"
    "file set p f|-m"
)

# `project list`'s filters, compared by which projects they select.
#
# The command's output format is a deliberate divergence — Python has a
# name column and a heading, and under either filter it prints two
# tables — so the bytes will never match and the row above sits in
# EXPECTED. The *slug set* is the portable part and is exactly what the
# flags change, so that is what this compares. Same move as the summary
# panes: when the whole output cannot be diffed, diff the sub-unit that
# carries the meaning.
PROJECT_FILTERS=(
    "--nix-only"
    "--category package"
    "--category home-module"
    "--category nixos-module"
    # Two that select nothing, so an empty result is compared rather
    # than skipped. A filter that silently matched everything would
    # pass every row above and fail these.
    "--category service"
    "--category desktop-app"
    "--nix-only --category package"
    "--nix-only --category nixos-module"
)

# The slugs in a `project list` table, from either implementation.
#
# The first column is the slug in all three tables involved — Python's
# "Nix Projects" and "Projects", and this port's single one — so the
# duplicated rows in Python's output fold away under `sort -u`.
#
# Structure cannot be used to find the rows, because the two formats do
# not share any: Python underlines its header with a dashed rule and
# this port prints none. Keying on the rule made the port's slugs come
# back empty and six of these comparisons "fail" while the port was in
# fact correct. So the furniture is dropped by name instead, which both
# formats make unambiguous — titles and header cells are the only
# first-column tokens that are not slugs.
project_slugs() {
    awk '/^-+$/ || !NF { next }
         # Pythons empty-result sentence, which is prose on stdout and
         # would otherwise read as a project called "No". This port
         # prints a bare header and puts its count on stderr.
         /^No projects found/ { next }
         $1 == "slug" || $1 == "SLUG" || $1 == "Projects" || $1 == "Nix" { next }
         { print $1 }' | sort -u
}

project_filter_parity() {
    local args a b agree=0
    for args in "${PROJECT_FILTERS[@]}"; do
        # shellcheck disable=SC2086
        a=$("$RS" project list $args 2>/dev/null | project_slugs)
        # shellcheck disable=SC2086
        b=$(run_b project list $args 2>/dev/null | project_slugs)
        if [[ $a == "$b" ]]; then
            agree=$((agree + 1))
        else
            unexpected=$((unexpected + 1))
            printf 'DIFFERS   project list %s\n' "$args"
            diff <(printf '%s\n' "$a") <(printf '%s\n' "$b") | head -6
        fi
    done
    printf 'filters   %d of %d project-list filters select the same projects\n' \
        "$agree" "${#PROJECT_FILTERS[@]}"
}

# The long and short options named on stdin, one per line.
#
# Reads only the options section, and only the option tokens at the
# *start* of each line — the two help formats differ everywhere else,
# and both mention other flags in their descriptions. Python's
# `-i, --ignore-case  Case insensitive (only with --no-fts)` is exactly
# the line that a naive grep for `--[a-z]*` gets wrong.
parse_flags() {
    awk '
        tolower($0) ~ /^ *(options|flags):/ { f = 1; next }
        f && /^[^[:space:]]/ { f = 0 }
        f && /^[[:space:]]+-/ {
            line = $0
            sub(/^[[:space:]]+/, "", line)
            sub(/  +.*$/, "", line)         # cut the description
            gsub(/<[^>]*>/, "", line)       # clap metavars
            gsub(/\{[^}]*\}/, "", line)     # argparse choice lists
            gsub(/[A-Z_][A-Z_]+/, "", line) # argparse metavars
            n = split(line, parts, /[ ,]+/)
            for (i = 1; i <= n; i++)
                if (parts[i] ~ /^--?[a-zA-Z]/) print parts[i]
        }' | sort -u
}

flag_parity() {
    local cmd flag side pair e is_exp matched agree=0 a b
    for cmd in "${FLAG_COMMANDS[@]}"; do
        matched=yes
        # Unquoted on purpose: the rows above are argument lists. The
        # other side is `run_b`, a function, so it cannot be passed as
        # a command word.
        # shellcheck disable=SC2086
        a=$("$RS" $cmd --help 2>&1 | parse_flags)
        # shellcheck disable=SC2086
        b=$(run_b $cmd --help 2>&1 | parse_flags)
        while read -r side flag; do
            [[ -n $flag ]] || continue
            pair="$cmd|$flag"
            is_exp=no
            for e in "${FLAGS_EXPECTED[@]}"; do
                [[ $pair == "$e" ]] && is_exp=yes
            done
            if [[ $is_exp == yes ]]; then
                expected=$((expected + 1))
            else
                matched=no
                unexpected=$((unexpected + 1))
                printf 'DIFFERS   flags %-24s %s-only: %s\n' "$cmd" "$side" "$flag"
            fi
        done < <(
            comm -13 <(printf '%s\n' "$a") <(printf '%s\n' "$b") | sed 's/^/other /'
            comm -23 <(printf '%s\n' "$a") <(printf '%s\n' "$b") | sed 's/^/port /'
        )
        [[ $matched == yes ]] && agree=$((agree + 1))
    done
    printf 'flags     %d of %d commands agree (deliberate divergences allowed)\n' \
        "$agree" "${#FLAG_COMMANDS[@]}"
}

# One pane's body: everything after its header, up to the next rule.
#
# The `──` run is U+2500 twice and the muted prose uses an em dash, so
# matching the rule cannot catch a `(not ported — ...)` line by mistake.
summary_pane() {
    awk -v p="$2" 'index($0, p) { f = 1; next } f && /──/ { exit } f { print }' "$1"
}

# Compare every claimed pane, over several captures.
#
# Retrying is not defensiveness about a flaky test — it is the only
# correct way to compare two processes reading a database that a third
# is writing. Two sources of difference have nothing to do with the
# port:
#
#   * **The clock.** The ingestion pane renders ages, so a minute
#     boundary falling between the two invocations turns `0min ago` into
#     `1min ago`. `TEMPLEDB_RS_NOW` cannot help — it pins this port's
#     clock and not Python's, which would make the comparison worthless
#     rather than merely noisy.
#   * **The writers.** Seven ingest adapters run on timers and the
#     `ok_count` column the pane prints increments on every run. During
#     active ingestion that pane can legitimately differ on *every*
#     attempt.
#
# So agreement is tracked per pane and across attempts: a pane that
# matched on **any** attempt is agreeing, and only a pane that differed
# on every one is reported. That is sound because a genuine divergence
# is not intermittent — it differs every time — while a moving row
# differs only on the attempts that straddle the write.
#
# An earlier version required *all* panes to agree before it stopped
# retrying, which meant one continuously-churning pane consumed the
# attempts and the rest were reported against whatever the last capture
# happened to hold. It reported a false failure the first time ingestion
# was busy during a run.
summary_parity() {
    local attempt pane a b e
    local -A ever_agreed=()
    for attempt in 1 2 3; do
        local outstanding=0
        "$RS" summary >"$sumfile_a" 2>/dev/null
        run_b summary >"$sumfile_b" 2>/dev/null
        for pane in "${SUMMARY_PANES[@]}"; do
            [[ -n ${ever_agreed[$pane]:-} ]] && continue
            a=$(summary_pane "$sumfile_a" "$pane")
            b=$(summary_pane "$sumfile_b" "$pane")
            if [[ $a == "$b" ]]; then
                ever_agreed[$pane]=yes
            else
                outstanding=$((outstanding + 1))
            fi
        done
        (( outstanding == 0 )) && break
    done

    for pane in "${SUMMARY_UNPORTED[@]}"; do
        printf 'unported  summary %s\n' "$pane"
    done

    local -a diffs=()
    for pane in "${SUMMARY_PANES[@]}"; do
        [[ -n ${ever_agreed[$pane]:-} ]] || diffs+=("$pane")
    done

    local identical=$(( ${#SUMMARY_PANES[@]} - ${#diffs[@]} ))
    for pane in "${diffs[@]}"; do
        local is_exp=no
        for e in "${SUMMARY_EXPECTED[@]}"; do
            [[ $pane == "$e" ]] && is_exp=yes
        done
        if [[ $is_exp == yes ]]; then
            expected=$((expected + 1))
            printf 'expected  summary %s\n' "$pane"
        else
            unexpected=$((unexpected + 1))
            printf 'DIFFERS   summary %s\n' "$pane"
            diff <(summary_pane "$sumfile_a" "$pane") \
                 <(summary_pane "$sumfile_b" "$pane") | head -8
        fi
    done
    printf 'summary   %d of %d claimed panes identical, %d not ported\n' \
        "$identical" "${#SUMMARY_PANES[@]}" "${#SUMMARY_UNPORTED[@]}"
}

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
    # Both sides are this port, so every pane must agree — the
    # reconciliation tie-break is in the SQL for both dialects.
    SUMMARY_EXPECTED=()
    # Likewise the flag surface: it is the same binary twice, so this
    # stage can only ever report agreement here. Kept rather than
    # skipped so the three modes produce the same shape of report.
    FLAGS_EXPECTED=()
    ;;
regress)
    [[ -x $ARG ]] || { echo "regress mode needs an executable old binary" >&2; exit 2; }
    label="old build"
    run_b() { "$ARG" "$@"; }
    # Every difference is interesting when comparing two builds of this
    # port: the point is to notice unintended ones.
    EXPECTED=()
    SUMMARY_EXPECTED=()
    # A flag this build dropped, or one the old build never had, is
    # exactly the kind of unintended change this mode is for.
    FLAGS_EXPECTED=()
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
sumfile_a=$(mktemp) sumfile_b=$(mktemp)
trap 'rm -f "$errfile_a" "$errfile_b" "$sumfile_a" "$sumfile_b"' EXIT

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

summary_parity
flag_parity
project_filter_parity

printf '\ntempledb-rs vs %s, %d invocations: %d stdout-identical (%d differing on stderr only), %d deliberate, %d unexpected\n' \
    "$label" "${#INVOCATIONS[@]}" "$pass" "$stderr_only" "$expected" "$unexpected"
exit $((unexpected > 125 ? 125 : unexpected))
