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
    # `file where` — the first rows in this script whose answer depends
    # on a filesystem, and the first consumers of the tree fence below.
    # Eight rows, chosen so that each of the command's branches is one.
    #
    # The first four were written when both mirror paths were hardcoded.
    # Three of them were FAILING by 2026-11-15, because Python had moved
    # both labels onto checkout-role resolution and this port had not;
    # they pass unchanged now and are left exactly as they were, because
    # a row that keeps its arguments through a rewrite of what it tests
    # is the only kind that can report a regression.
    #
    #   drift in the checkout, and nothing else present
    "file where templedb-rust README.md"
    #   an overlay hit, which is the branch the `os.walk` order matters
    #   for -- this file is in the checkout *and* resolved through
    #   ~/.emacs.d into the store, and both copies drift from the DB
    "file where templedb integrations/emacs/templedb-agent.el"
    #   an overlay hit that drifts while the checkout matches. Also the
    #   row that pins Python's basename matching: the overlay copy here
    #   is the *templedb* layer's packages.el, a different file that
    #   happens to share a name, and the command reports drift between
    #   two files that were never the same one.
    "file where emacs-config emacs.d/layers/projdb/packages.el"
    #   everything agrees: exit 0 and no advisory line
    "file where system_config emacs.d/layers/templedb/packages.el"
    # Two rows for checkout-role resolution, added 2026-11-15 when the
    # hardcoded mirror paths were replaced by it. Both were measured
    # differing before that change, and they are the two *directions* of
    # the same defect -- a row that only ever caught the missing half
    # would have passed while this port invented a mirror.
    #
    # The four rows above already cover an adopted edit tree, which is
    # the branch the old `edit-work` probe could never reach: three of
    # them resolve one, and that is why three of them were failing.
    #
    #   no active `checkouts` row -- 14 of the 26 projects -- so Python's
    #   `repo_url` fallback answers, and its tree is nowhere near
    #   ~/.config/templedb. The old implementation MISSED this mirror.
    "file where woofsledger src/db.py"
    #   the opposite error: the old implementation INVENTED a `checkout `
    #   line here by hashing ~/.config/templedb/checkouts/<slug>/<path>,
    #   which exists and is not what resolution picks. This project has
    #   `scratch` and `edit` rows and no `canonical` one, so it is also
    #   the live case for the last-resort branch, where a `scratch` tree
    #   -- never resolved to by name -- is what answers.
    "file where nixos_anywhere_vultr_woofs .envrc"
    # The two exit-2 rows are deliberately NOT fenced, and that is the
    # point of having them: Python returns before the probe loop when
    # the database has no current row, so neither of these reads a tree
    # at all. If a future change made them read one, the unfenced row
    # is what would start flapping -- a fenced row would hide it.
    "file where templedb no/such/file.txt"
    "file where no-such-project-ever README.md"
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
    # The default view: published plus the pinned session's commits. Both
    # implementations are pinned to the same live session by
    # TEMPLEDB_SESSION_ID below, which is what makes this row diffable at
    # all — unpinned, Python guesses from the process tree and creates a
    # row, and this port declines.
    #
    # `templedb` is normally the *left-half-only* case: it has far more
    # commits than `-n 10` shows, so a session's own ones are almost
    # always below the cutoff and the default view equals the published
    # one. The union's right half is covered by the rows the pin block
    # appends, against a project discovered to have some. Both cases
    # matter and neither can be hardcoded, because which sessions own
    # what changes daily.
    "vcs log templedb -n 10"
    "vcs log templedb --published -n 10"
    # Python's `--session` is a deprecated no-op — the view it asks for
    # is the default — and so is this port's. The row exists because
    # "accepted and ignored" is exactly the claim that rots silently: a
    # version that made the flag mean something would still pass every
    # other row here.
    "vcs log templedb --session -n 10"
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
    # `search files`, both forms. They print DIFFERENT COLUMNS -- the
    # filtered one drops `project_slug` and gains `lines_of_code` -- so
    # neither form's rows exercise the other's table, and both are here.
    #
    # The first two are the unfiltered form at one row and at 25 across
    # five projects, which is what checks the ORDER BY: Python has none,
    # and these rows are byte-identical only because `ORDER BY id` is
    # what its planner happens to produce. See the README.
    "search files justfile"
    "search files flake"
    # The filtered form. `README.md` is the one templedb-rust file with
    # a `lines_of_code`, and `.rs` is 28 files where all but one are
    # NULL -- which Python prints as the literal word `None`, so this
    # row is what pins `py_str_opt_i64`.
    "search files README.md -p templedb-rust"
    "search files .rs -p templedb-rust"
    # Over the hardcoded hundred: 536 files match and both sides print
    # the first 100 in id order. Differs on stderr only, where this port
    # says the list was cut and Python's `(100 results)` does not.
    "search files .py"
    # A deleted file, listed as present. The view has no `status`
    # filter, unlike `file ls` and `search content`; `src/main.py` is
    # deleted from templedb-rust and `file ls` does not show it. An
    # upstream behaviour reproduced rather than endorsed -- this row is
    # here so that it changing upstream shows up here.
    "search files src/main.py -p templedb-rust"
    # No match, exit 0, one line of stdout -- not an error on either
    # side.
    "search files zzz-no-such-file-ever"
    # An unresolvable project, and the same no-match line. Python's
    # `search` module is the ONE command family that never calls the
    # fuzzy matcher, so `-p` here is an exact slug and a miss selects
    # nothing instead of failing. Every other project argument in this
    # sweep resolves fuzzily; this row is why that is a per-command
    # fact rather than a port-wide one.
    "search files README -p no-such-project-ever"
    # The LIKE-metacharacter divergence, in EXPECTED below.
    "search files get_"
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
    # `doctor history`, all four of its shapes. The default limit
    # matters on its own: 30 rows cuts *inside* the tied second a full
    # `doctor entities` pass leaves behind (~44 rows share a
    # timestamp), so a missing `id DESC` tie-break returns an arbitrary
    # subset here and the row would fail intermittently rather than
    # never.
    "doctor history"
    "doctor history --limit 5"
    "doctor history --violated-only"
    "doctor history --check no_undefined_names"
    "doctor history --check no_undefined_names --limit 3"
    # Selects nothing, so the "(no invariant check history yet …)"
    # branch is compared rather than skipped. An empty result that both
    # sides reach by different routes is still the same bytes.
    "doctor history --check no-such-invariant-ever"
    # `intent list` / `intent show`. The limits here are chosen from a
    # measurement and the reason is the opposite of `doctor history`'s
    # above: there the default limit cuts *inside* a tied second and
    # that is why the row is valuable, and here it must not, because
    # Python has no `id DESC` tie-break at all.
    #
    # Measured on the live database 2026-10-30: 32 `created_at` values
    # are shared by two rows each out of 1,346, and the newest tie group
    # sits at **rank 134**. So every invocation below stays inside the
    # first 134 rows, where both implementations are deterministic.
    # Past that Python disagrees with *itself* run to run, so a byte
    # diff would be comparing against an unstable oracle and a failure
    # there would say nothing about this port. See `intent.rs`.
    # The `--all-statuses` rows below are in EXPECTED, because the
    # tie-break divergence is permanent until Python grows one -- see
    # the note there, including why picking "safe" arguments was the
    # wrong way to handle it.
    #
    # The two narrow rows are NOT exempted and are still compared
    # byte-for-byte. They are the default view, `status = 'proposed'`,
    # which is 2 of 1,356 live rows -- small enough that a tie is
    # unlikely rather than impossible. If one lands they move up to
    # EXPECTED too, and that is the right failure: a new difference
    # reported once beats a row quietly exempted in advance.
    "intent list"
    "intent list --all-statuses"
    "intent list --all-statuses --limit 50"
    "intent list --limit 1"
    "intent list -p templedb --all-statuses --limit 20"
    "intent list -s 1216 --all-statuses"
    # Selects nothing, so the "(no matching intents)" branch is compared
    # rather than skipped.
    "intent list -s 999999 --all-statuses"
    # A project that does not resolve: exit 1 from the fuzzy matcher at
    # the edge, before the query is built.
    "intent list -p no-such-project-ever"
    # The full record, three shapes. #1 is the oldest row and the only
    # one in the whole table carrying a `cancelled_at`, so it is the
    # single invocation that compares that column's rendering — and it
    # also has a NULL `session_id`, so it is where the literal `None`
    # line gets diffed.
    "intent show 1"
    # Applied, with a commit link and a session. The common shape —
    # 882 of 1,346 rows carry an `applied_commit_id` — and the one that
    # exercises the column the mirror grew for this command.
    "intent show 1347"
    # Proposed: the nine-line minimum, with all five conditional lines
    # absent.
    "intent show 1279"
    # Exit 1 and a stderr-only message. Python's `intent show` returns
    # a bare 1 here where `source snapshot` returns 2 for the same
    # class of miss, so this row is what pins the inconsistency rather
    # than letting the port tidy it.
    "intent show 99999999"
    # `vcs diff`'s two working-set forms, ported 2026-10-31. Both read
    # only mirrored tables and neither touches a tree, which is what
    # made them portable -- the README had grouped `--staged` with the
    # genuinely tree-dependent form.
    #
    # regr-test is the sharp one: its single staged row is `modified`
    # with a staged hash that DIFFERS from `file_contents.is_current`,
    # so it is the row that pins Python's unused-`staged_hash` bug. If
    # upstream fixes that, this row starts differing and says so.
    "vcs diff regr-test --staged --no-color"
    "vcs diff regr-test --staged -s --no-color"
    "vcs diff qa-runner --staged --no-color"
    "vcs diff templedb --staged --no-color"
    # Colour on, which is the default and the shape a human sees.
    "vcs diff regr-test --staged"
    # The bare form: every changed row, staged or not.
    "vcs diff regr-test --no-color"
    "vcs diff qa-runner --no-color"
    "vcs diff templedb --no-color"
    # Nothing staged and nothing changed -- the two empty branches,
    # which print different text and both exit 0.
    "vcs diff bza --staged"
    "vcs diff bza"
    # No active and no default branch: `No active branch found` on
    # stderr and exit 1, which both forms share.
    "vcs diff cathedraldb --staged"
    "vcs diff cathedraldb"
    # `file retype`, ported 2026-11-19. The dry form is the default, so
    # this is the one command in the `file` family whose interesting
    # behaviour is a READ with a diffable stdout -- every other write
    # here had to be cross-read or measured on a copy. Its answer is a
    # function of `importer.scanner`'s 179-entry classifier, which is
    # pinned separately against CPython in `classify_parity.rs`; these
    # rows are what check the query, the grouping and the renderer.
    #
    # The unfiltered form is the sharp one, and it is sharp for a
    # reason the row cannot show: it finds 26 kinds of change with 11
    # tied at a count of 1, and Python cuts the list at 25 -- so the
    # cut falls INSIDE a tie group and `most_common` breaks ties by row
    # order. Python has no `ORDER BY`; this port adds `ORDER BY pf.id`,
    # which reproduces SQLite's current plan and gives Postgres a total
    # key. Without it the cross-backend sweep would disagree about
    # which rows appear, not merely about their order. See the README.
    "file retype"
    # A project narrowing, fuzzy on both sides -- unlike every write in
    # this port, which takes an exact slug. `templedb-rust` is the one
    # whose four `.rs` files carry the `SELECT id FROM file_types
    # LIMIT 1` fallback type, so this row is the bug the command exists
    # to repair, in four rows.
    "file retype templedb-rust"
    "file retype templedb"
    "file retype system_config"
    # `--from-type`, which is a condition on the LEFT JOINed column and
    # therefore silently drops rows with a NULL or dangling
    # `file_type_id`. `ansible_inventory` is the live value of that
    # `LIMIT 1` fallback row (id 126) -- Python's own docstring says
    # `api_route`, which was true before migration 138 reseeded the
    # table -- so this row exercises 44 of the 88 changes.
    "file retype --from-type ansible_inventory"
    # Both narrowings together, and two forms that find nothing: the
    # `Nothing to re-type (N active file(s) checked)` branch, which is
    # the one place the denominator is printed without a numerator.
    "file retype templedb --from-type javascript"
    "file retype templedb-rust --from-type rust"
    "file retype --from-type nonexistent_type"
    # Exit 1 on an unresolved project, which is Python's bare
    # `return 1`. stdout is empty on both sides and the stderr wording
    # is this port's own, like every other error message here.
    "file retype no-such-project-ever"
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
    # Substring not stemmed, and a different layout.
    "search content"
    # This port has an `id DESC` tie-break on `created_at` and Python
    # has none. Same divergence `entity explore` carries one line up,
    # and the same one `entity search` carried until Python grew a
    # tie-break of its own.
    #
    # These rows were NOT here when the intent reads landed on
    # 2026-10-30, and the reason they are here now is worth reading
    # before writing another sweep row. The rows were chosen to stay
    # inside the window where Python is deterministic -- measured then
    # as "the newest tie group sits at rank 134, so a --limit 50 row is
    # safe". That was true and it decayed in ONE DAY: two intents
    # landed at 2026-10-31 17:43:51 and the newest tie group moved to
    # rank 4.
    #
    # The lesson is not that the measurement was wrong. It is that
    # "stay inside the deterministic window" is not a strategy, because
    # the window is a property of live data and the data moves. A
    # divergence that is permanent until upstream changes belongs in
    # THIS list, where it is stated once, and not in a row's choice of
    # arguments, where it has to keep being true.
    "intent list --all-statuses"
    "intent list -p templedb --all-statuses"
    "intent list -s 1216 --all-statuses"
    # Python interpolates `f"%{pattern}%"` raw, so `_` is a wildcard
    # there and a literal here. Not a borderline call: `get_` returns
    # ten rows upstream and NONE of them contains `get_` -- the hits are
    # `GETTING_STARTED.md`, `get-booking-availability`,
    # `getDialpadOpts.mjs` and so on. `set_` gives 321 against 5.
    # Measured 2026-11-17: 1,112 of 2,780 live paths hold a literal
    # underscore.
    #
    # Same bug `entity search` and the `file ls` prefix had, both since
    # fixed upstream; this is the copy still live, and the row stays
    # here so that it converging is a surprise rather than a silence.
    "search files get_"
)

# Pin both implementations to the same live VCS session.
#
# The session-scoped rows above are not comparable without a pin, in
# either direction: Python resolves "your session" through seven steps,
# four of which guess from the process tree and two of which **insert a
# row**, while this port resolves only what the caller named. Pinning
# `TEMPLEDB_SESSION_ID` is step 1 on Python's side and outranks all the
# guessing — including `TEMPLEDB_SESSION`, which the agent wrapper this
# is usually run under exports.
#
# The id comes from Python's own `vcs session list --active`, because
# that is the authority on which sessions exist and this script must not
# create one. Parsing another implementation's table is normally a bad
# idea; it is defensible here because the session identity is not what is
# being diffed — it is the fixture the diff runs against.
#
# Then a project is chosen *with* it, and that is the part worth
# explaining. Any live session makes the fixed rows diffable, but a
# session owning nothing exercises only the left half of
# `session_id IS NULL OR session_id = ?` — and an implementation that
# dropped the right half entirely would still pass every such row. So the
# pair is searched for: the first (session, project) whose default view
# carries an `(unpublished)` line, which is the only visible evidence
# that the union contributed anything.
#
# Searched rather than hardcoded because it cannot be fixed in advance.
# A session's commits sink below `-n 10` as a project gains history and
# vanish entirely when it is published, so any pair written down here
# would stop covering the branch without failing.
SESSION_PROJECTS=(woofs_projects templedb-rust bza templedb nixos_anywhere_vultr_woofs)
owns_project=""
if [[ -z ${TEMPLEDB_SESSION_ID:-} ]]; then
    mapfile -t live_sessions < <(templedb vcs session list --active 2>/dev/null \
        | awk 'NR > 1 && $2 == "active" { print $1 }' | head -50)
    # Asked as "does the default view carry an `(unpublished)` line",
    # not as "does it differ from the published view". The two always
    # differ — their header labels do — and the first version of this
    # probe compared whole outputs and so reported every candidate as
    # owning commits. Probed with this port alone: the question is
    # whether the fixture reaches the branch, not whether the two
    # implementations agree about it, and asking Python would mean
    # letting it create a session for no extra information.
    for cand in "${live_sessions[@]}"; do
        for proj in "${SESSION_PROJECTS[@]}"; do
            if TEMPLEDB_SESSION_ID=$cand "$RS" vcs log "$proj" -n 10 2>/dev/null \
               | grep -q '(unpublished)$'
            then
                TEMPLEDB_SESSION_ID=$cand
                owns_project=$proj
                break 2
            fi
        done
    done
    # No pair found, which is the normal state just after a publish. The
    # newest live session still makes the fixed rows diffable.
    TEMPLEDB_SESSION_ID="${TEMPLEDB_SESSION_ID:-${live_sessions[0]:-}}"
fi

if [[ -n ${TEMPLEDB_SESSION_ID:-} ]]; then
    export TEMPLEDB_SESSION_ID
    if [[ -n $owns_project ]]; then
        # The union's right half, on a project the pinned session owns
        # commits in. Three rows, so the published view and the no-op
        # flag are diffed against the same non-trivial fixture — and so
        # that the advisory line, which only appears under `--published`
        # with a non-zero count, is diffed at all.
        INVOCATIONS+=(
            "vcs log $owns_project -n 10"
            "vcs log $owns_project --published -n 10"
            "vcs log $owns_project --session -n 10"
        )
        printf 'pinned    TEMPLEDB_SESSION_ID=%s, which owns unpublished commits in %s\n' \
            "$TEMPLEDB_SESSION_ID" "$owns_project"
    else
        # Loud, because the rows below will all pass and three of them
        # will have checked less than they look like they checked.
        printf 'pinned    TEMPLEDB_SESSION_ID=%s, which owns no unpublished commits in any of: %s\n' \
            "$TEMPLEDB_SESSION_ID" "${SESSION_PROJECTS[*]}"
        printf 'pinned    so the session view is diffed on its published half only\n'
    fi
elif [[ $MODE == python ]]; then
    # No live session, so the pin is impossible and the session-scoped
    # rows cannot be compared against Python. Said out loud and moved to
    # EXPECTED rather than skipped: a row that silently stops being
    # checked reads as a row that passed.
    #
    # Only in `python` mode. The other two compare this port against
    # itself, so an unpinned run has both sides declining to resolve and
    # the rows agree — there is nothing a missing pin can hide there.
    echo "no live VCS session to pin; the session-scoped rows cannot be diffed" >&2
    echo "  start one with \`templedb vcs session start\` to check them" >&2
    EXPECTED+=(
        "vcs log templedb -n"
        "vcs log templedb --published"
        "vcs log templedb --session"
    )
fi

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
#
# `vcs commit-staged` is deliberately NOT here, and adding it would
# break the stage rather than extend it: the name is this port's, so
# `templedb vcs commit-staged --help` fails on the other side and there
# is no surface to compare. That is the intended consequence of giving
# the staged-rows commit a name of its own instead of calling it
# `vcs commit` -- see the README's *The second write*. The stage's
# blind spot is therefore a command only Python has, which is the
# README's *Flag surfaces* note.
FLAG_COMMANDS=(
    "project list"
    "project show x"
    "file cat p f"
    "file ls p"
    # Both sides have only `-h`/`--help` here, which is the claim worth
    # pinning: `file where` was given no `--no-color` precisely so this
    # stage would report zero divergences rather than a permanent one.
    "file where p f"
    "file set p f"
    # `file rm`, 2026-11-18, and the fourth write this stage can see.
    # Neither side has a flag beyond `-h`/`--help`, which is the claim
    # worth pinning rather than a triviality: the obvious thing to give
    # this command was a `--session`, since the port requires a session
    # where Python creates one, and Python's `file rm` has no such flag.
    # Inventing it would have bought convenience and a permanent
    # FLAGS_EXPECTED entry. `file where`'s missing `--no-color` is the
    # same decision one command over.
    "file rm p f"
    # `file mark`, 2026-11-18, and the fifth write this stage can see.
    # One flag on each side -- `--edit-mode`, required on both, with the
    # same three choices -- and no allow-list entry. The choices
    # themselves are invisible here (the awk strips `{...}` and
    # `<...>`), so what this row pins is the flag NAME and its
    # requiredness; that the three values match is pinned in
    # `filemark.rs` instead.
    #
    # The tempting fourth value is `non-immutable`, which `file ls
    # --edit-mode` takes and this one must not: Python's two `choices=`
    # lists differ, and accepting it here would be a port-only flag
    # VALUE -- a divergence this stage would not even report, since it
    # compares names.
    "file mark p f"
    "entity stats"
    "entity explore K/r"
    "entity trace K/r"
    "entity search q"
    "entity paths A/b C/d"
    "entity dead-imports"
    "entity observations K/r"
    "search content q"
    # Both sides have `-p`/`--project` and nothing else. Deliberate:
    # `search content`'s `--limit` is five entries down in
    # FLAGS_EXPECTED, and `search files` was given no equivalent
    # precisely so this line reports agreement instead of a permanent
    # exemption -- Python's hundred is hardcoded here, where its FTS
    # path had no limit at all.
    "search files q"
    "source snapshot p f"
    "source revisions p f"
    "vcs log p"
    "vcs show p c"
    "vcs branch p"
    "vcs diff p f"
    "summary"
    "doctor history"
    "intent list"
    "intent show 1"
    # `intent cancel` is a write, so no INVOCATIONS row can exist for
    # it -- but its NAME is Python's, unlike `file set`'s shape and
    # unlike `vcs commit-staged` entirely, so `--help` resolves on both
    # sides and this stage CAN see it. First write in the port whose
    # surface the sweep compares at all, which closes half of the
    # blind spot noted above.
    "intent cancel 1"
    # `intent create`, 2026-11-17, for `intent cancel`'s reason and
    # with no allow-list entry: all four of Python's flags are ported
    # (`-c`/`--content`, `-f`/`--from-file`, `--base-rev`,
    # `-m`/`--message`) and none was invented, so the surfaces agree
    # exactly. Second of the port's four writes this stage can see.
    #
    # What it still cannot see is this command's one behavioural
    # divergence, because it is in a POSITIONAL: the project is matched
    # exactly here and fuzzily by Python. Blind spot 3 above, and the
    # same shape as `file set`'s.
    "intent create p f"
    # `intent apply`, 2026-11-18, and the third write this stage can
    # see. One flag on each side -- `--no-rebase` -- and no allow-list
    # entry, which took a decision rather than being free: what the
    # flag selects is this port's behaviour whether or not it is
    # passed, so the alternative was omitting it. It is here because a
    # flag that promises a refusal and delivers one is honest, unlike
    # `search content`'s `--no-fts` three entries down, which would
    # promise matching this port cannot do.
    #
    # The fourth blind spot, and this command is the first instance:
    # the stage compares the flag NAMES and can say nothing about what
    # a shared flag DOES. `--no-rebase` is a no-op here and selects a
    # different code path over there, and these surfaces agree
    # perfectly. The divergence is in the README and in the exit
    # status, not in anything this stage reads.
    "intent apply 1"
    # `file retype`, 2026-11-19. Two flags on Python's side --
    # `--from-type` and `--apply` -- of which one is ported, so this
    # command arrives WITH an allow-list entry rather than without one,
    # unlike `intent create` and `file mark`. The entry is
    # `file retype|--apply` and the argument for absent-rather-than-
    # inert is beside it in FLAGS_EXPECTED.
    #
    # It also runs straight into blind spot 5, which `file mark` named
    # a day earlier: this stage compares flag NAMES, and `--from-type`
    # takes a free-form value on both sides so there is nothing to
    # disagree about. The positional is blind spot 3 as usual, and here
    # for once it is not a divergence -- `retype` is a read, so the
    # project is fuzzy on both sides.
    "file retype"
)

# This stage compares the INTERSECTION, and the headline number
# ("N of N commands agree") counts only commands that exist on both
# sides. Five things it cannot see, stated plainly because the number
# reads like coverage:
#
#   1. Commands only PYTHON has. Measured 2026-10-31: Python has 44
#      top-level commands and ~290 subcommands against this port's 10
#      and 21. Recounted 2026-11-17 by walking `--help` -- 11
#      top-level and 29 leaves, counting the three that take no
#      subcommand (`mirror`, `state`, `summary`) as their own leaf,
#      which is the method `tests/sweep_coverage.rs` uses and which the
#      older pair of numbers evidently did not. Either way the point
#      stands and is the only thing load-bearing here: `doctor
#      entities` used to be named as *the* example of this blind spot,
#      which understated it by about 270. The blind spot is most of the
#      surface, not one command.
#   2. Commands only THIS PORT has: `vcs commit-staged`, `mirror`,
#      `state`. `templedb <name> --help` fails on the other side, so
#      there is nothing to compare. For commit-staged that is the
#      intended consequence of not calling it `vcs commit`.
#   3. POSITIONAL arity, in either direction. The awk below reads the
#      `options:`/`flags:` block and nothing else, so a positional
#      that is required on one side and optional on the other is
#      invisible. That is not hypothetical: `vcs diff`'s `file` was
#      required here and `nargs='?'` over there from step 9 until
#      2026-10-31, and this stage ran clean through all of it.
#   4. What a shared flag DOES. This stage compares flag NAMES, so two
#      surfaces can agree perfectly while one of the shared flags
#      selects a different behaviour. Added 2026-11-18 with
#      `intent apply`, whose `--no-rebase` is a no-op here and a live
#      branch over there -- the first case that makes the gap concrete
#      rather than hypothetical. Nothing in this script can close it:
#      the divergence is visible in the exit status and argued in the
#      README, and neither is a flag name.
#   5. A flag's accepted VALUES. `parse_flags` strips `{...}` and
#      `<...>` precisely so argparse's choice lists and clap's
#      metavars do not have to be spelled the same way, which means a
#      choice list that gained or lost a value is invisible. Added
#      2026-11-18 with `file mark`: Python's two `--edit-mode` lists
#      are different lists -- four values on `file ls` and three on
#      `file mark` -- so a port reusing one enum for both would accept
#      `file mark --edit-mode non-immutable`, a value upstream rejects,
#      and this stage would report agreement. Closed by construction
#      rather than by a check: `main.rs` carries two enums, and
#      `filemark.rs` pins the three.
#
# `intent cancel`, `intent create` and `intent apply` are the WRITES
# whose surfaces this stage can see, because unlike the other two their
# names are Python's.

# Flag divergences that are deliberate, as `<command>|<flag>` pairs.
# Every one of these is argued in the README; the pair form means a
# *new* flag on an already-diverging command is still caught.
FLAGS_EXPECTED=(
    # Writes. A read-only port cannot have them.
    "vcs branch p|-d"
    "vcs branch p|--delete"
    # `vcs diff p f|--staged` was here until 2026-10-31, exempted on the
    # grounds that it "needs the staging session, which is a write and
    # six environment fallbacks". Both halves were false. `_diff_staged`
    # resolves no session at all -- its predicate is
    # `staged_by_session_id IS NOT NULL`, any session -- and it writes
    # nothing. The flag is ported and the exemption is gone, which is
    # why this comment is here instead of the row: an allow-list entry
    # that outlives its reason is indistinguishable from one that still
    # holds.
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
    # `--skip-intent`, `--session`, `--verify` and `--allow-mojibake`
    # have all left this list; `-s` / `--stage` left it on 2026-10-27
    # with the auto-stage, and `--commit` / `-m` / `--commit-msg` on
    # 2026-10-29. Nine flags picked up by a per-flag list rather than a
    # waved-at exception — which is also the reason the list has to be
    # pruned by hand: a flag that becomes present on both sides stops
    # producing a difference, so a stale entry here suppresses nothing
    # and reads as a divergence that still exists.
    #
    # `--no-rebase` is the newest entry and it arrived from the other
    # direction — Python grew it, and this stage is what reported it.
    # With `--if-match`, Python no longer refuses a stale base: it
    # three-way merges the write onto what landed and only a genuine
    # overlap exits 2. That is a `ThreeWayMerge` engine this port does
    # not have, so `--if-match` here is still the strict
    # compare-and-swap Python's `--no-rebase` selects — the port's
    # behaviour is the flag's behaviour, which is why the flag itself
    # would have nothing to switch. See the README's *The rebase this
    # port does not do*.
    "file set p f|--no-rebase"
    # `file retype --apply` is the write half, and it is ABSENT from
    # this port's parser rather than accepted and ignored. The two
    # readings in this list point opposite ways and the shape of the
    # flag decides which applies:
    #
    #   - `search content --no-fts` is accepted-and-ignored territory:
    #     a flag switching OFF behaviour the port does not have.
    #     Listed above as absent, on the grounds that accepting it
    #     would promise different matching and deliver none.
    #   - `intent apply --no-rebase` is accepted: it promises a
    #     refusal this port delivers unconditionally.
    #
    # `--apply` is neither. It switches ON the only behaviour that
    # writes, so accepting it would mean `file retype --apply` exiting
    # 0 having written nothing -- a silent divergence, which is the one
    # outcome this project treats as a defect. Refused at the parser,
    # exits 2, loud.
    #
    # The blocker is the create arm and not the update:
    # `file_types.category` is `TEXT NOT NULL` with no default and this
    # port mirrors that table at two of five columns, so Python's
    # `INSERT (type_name, 'uncategorized', 'Auto-registered by file
    # retype')` needs the subset to grow. See the README's *Known gaps*
    # for the two-part fix. This entry should leave the list when that
    # lands.
    "file retype|--apply"
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
    # `--type`, which is a different column from `--category` with an
    # overlapping value. `service` is here on both flags on purpose:
    # it is the one spelling that selects different projects depending
    # on which column is read, so an implementation that confused them
    # passes every other row in this list and fails this pair.
    "--type regular"
    "--type nixos-config"
    "--type service"
    "--nix-only --type nixos-config"
    "--category package --type regular"
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

# --- the tree fence ----------------------------------------------------
#
# Rows whose answer depends on a directory, run between two digests of
# that directory so that churn cannot be reported as a divergence.
#
# This is the harness the README's step 26 asks for, and it is a fence
# rather than a pin. The difference is the whole design decision, so it
# is written down here:
#
# **Pinning was the wrong shape for these commands.** The obvious plan —
# a fixture tree in the repository, or a copy under a fake `HOME` — fails
# on a coupling that is easy to miss until you read
# `_check_checkout_matches_db`: it does not read a fixed path. It loops
# every project and calls `CheckoutRepository.resolve(id, PURPOSE_EDIT)`,
# which picks the tree from the **`checkouts` table** and the ambient
# session. So a fixture tree needs fixture rows, and those rows live in
# the SQLite file this port must not write. Point one side at a fixture
# and the sweep compares Python-on-live against Rust-on-fixture, which
# compares nothing at all.
#
# A fixture is also blind to the one check whose subject is POSIX mode:
# TempleDB's content store carries no execute bit — the justfile says so,
# and it is why the sweep is invoked as `bash scripts/...` — so a tree
# materialised from the database cannot reproduce a mode at all.
#
# **What is left is to detect, not to prevent.** Measured 2026-11-05:
# 432 non-`.git` files under `checkouts/` changed in 24 hours and 1,749
# in seven days, out of 229,389. That is 0.2% a day and it is *bursty* —
# a `publish` rewrites a few hundred files at once and nothing moves in
# between. So the digests almost always match on the first attempt, and
# when they do not, retrying converges. A pin would cost a fixture, a
# second set of table rows and an unreproducible file mode to buy
# certainty about an event that is rare and detectable.
#
# Same argument as `summary_parity` above, one substrate over: there,
# a third process writes the database and a pane that matched on any
# attempt is agreeing. Here a third process writes the tree.
#
# **Granularity is path + size + mtime, not content.** Hashing 229,389
# files to decide whether to trust one row is the wrong trade, and
# `mtime` is what `publish` moves. Symlinks are digested by their target
# rather than followed, because the overlay roots `file where` probes
# point into `/nix/store`: the store path is immutable, so the only
# thing that can change is which one the link names, and following the
# link would walk a Nix closure to learn that.
#
# A row that churns through every attempt is reported as `churned` and
# counted in neither `pass` nor `unexpected`. It must not be silent and
# it must not be a failure: the tree moved, which says nothing about
# whether the two implementations agree.
TREE_FENCE_ATTEMPTS=3

# `<invocation>|<root>[:<root>...]`, matched as a prefix of the
# invocation. Roots that do not exist are skipped rather than erroring —
# `file where` probes several that are absent for most slug/path pairs.
#
# **Every root a row reads has to be named, not just the interesting
# one.** `file where` hashes one file in the checkout and then *walks*
# all three overlay roots looking for a basename match, so the whole of
# `~/.emacs.d` is in its answer even for a row that gets no overlay hit:
# a file appearing there changes the output. Naming only the checkout
# would make the fence look present and cover a third of the read.
#
# Measured 2026-11-05: 471 ms per digest over `~/.emacs.d` (16,741
# entries), `/etc/nixos` and `~/.config/systemd/user` plus one
# checkout, so a fenced row costs about a second and the four of them
# add four to the sweep. Stated because it is the fence's whole price
# and it is charged whether or not a tree moves.
#
# **One root per row is deliberately NOT named: the tree resolution
# picks.** Since 2026-11-15 `file where`'s two mirror paths come from
# the `checkouts` table rather than from `~/.config/templedb/<root>/
# <slug>`, so the directory a row reads is chosen at run time and can be
# anywhere. Naming the enclosing root instead was measured and rejected:
# `edit-workspaces/templedb-rust` was **70,806 entries** on 2026-11-15
# and **71,822** a day later, and `edit-workspaces/templedb` is 57,046
# both days, because an edit workspace holds whatever the workspace
# holds — including the `target/` directory this very sweep is writing
# into while it runs. That one-day move of 1,016 entries is this port's
# own build. Fencing those would make every row `churned`, which is to
# say unchecked, and it would charge seconds per row to do it.
#
# What is left unfenced is therefore ONE FILE per mirror label: the
# probe is `stat` + hash of `<resolved root>/<path>`, not a walk. The
# residual risk is that single file changing inside the ~1 s between the
# two binaries, which is strictly smaller than the overlay walk the
# fence does cover, and it is the reason the digest-retry loop is a
# detector rather than a guarantee. Named in the README's *Known gaps*
# rather than left to be inferred from a passing row.
_ck="$HOME/.config/templedb/checkouts"
_overlays="$HOME/.emacs.d:/etc/nixos:$HOME/.config/systemd/user"
TREE_INVOCATIONS=(
    "file where templedb-rust README.md|$_ck/templedb-rust:$_overlays"
    "file where templedb integrations/emacs/templedb-agent.el|$_ck/templedb:$_overlays"
    "file where emacs-config emacs.d/layers/projdb/packages.el|$_ck/emacs-config:$_overlays"
    "file where system_config emacs.d/layers/templedb/packages.el|$_ck/system_config:$_overlays"
    # `woofsledger` has no checkout row, so its tree is its `repo_url`
    # and it is small enough to fence whole: 4,651 entries.
    "file where woofsledger src/db.py|$HOME/woofsledger:$_overlays"
    # Three candidate roots, because this project's resolution has three
    # extant rows to choose between and all three are cheap (140 entries
    # for the largest). The canonical path is named too even though no
    # row points there -- it is what the old implementation hashed, so
    # fencing it is what keeps this row able to report a return to that
    # behaviour rather than merely to a different answer.
    "file where nixos_anywhere_vultr_woofs .envrc|$_ck/nixos_anywhere_vultr_woofs:/tmp/woofs-verify:/tmp/woofs-fresh:$_overlays"
)
unset _ck _overlays

# Every root this script knows how to fence, by name, so a row names a
# set rather than repeating absolute paths. `$HOME` is expanded here and
# not in the rows, because the rows are compared as prefixes.
tree_roots_for() {
    local inv=$1 row root out=()
    for row in "${TREE_INVOCATIONS[@]}"; do
        [[ $inv == "${row%%|*}"* ]] || continue
        IFS=: read -r -a out <<<"${row#*|}"
        for root in "${out[@]}"; do printf '%s\n' "$root"; done
        return 0
    done
    return 1
}

# One line per entry: path, size, mtime. Symlinks by target.
tree_digest() {
    local root
    for root in "$@"; do
        [[ -e $root ]] || continue
        find "$root" -xdev -type l -printf '%p -> %l\n' 2>/dev/null
        find "$root" -xdev -type f -printf '%p %s %T@\n' 2>/dev/null
    done | sort | sha256sum | cut -d' ' -f1
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
pass=0 unexpected=0 expected=0 stderr_only=0 churned=0 fenced=0
errfile_a=$(mktemp) errfile_b=$(mktemp)
sumfile_a=$(mktemp) sumfile_b=$(mktemp)
trap 'rm -f "$errfile_a" "$errfile_b" "$sumfile_a" "$sumfile_b"' EXIT

for inv in "${INVOCATIONS[@]}"; do
    # The roots this row reads, if any. A row with none takes the
    # unfenced path below and costs nothing — `tree_roots_for` fails and
    # `roots` stays empty.
    roots=()
    mapfile -t roots < <(tree_roots_for "$inv" || true)

    if (( ${#roots[@]} )); then
        fenced=$((fenced + 1))
        before="" after="" attempt=0
        while (( attempt < TREE_FENCE_ATTEMPTS )); do
            attempt=$((attempt + 1))
            before=$(tree_digest "${roots[@]}")
            # shellcheck disable=SC2086
            a=$("$RS" $inv 2>"$errfile_a"); ea=$?
            # shellcheck disable=SC2086
            b=$(run_b $inv 2>"$errfile_b"); eb=$?
            after=$(tree_digest "${roots[@]}")
            [[ $before == "$after" ]] && break
        done
        if [[ $before != "$after" ]]; then
            # Neither a pass nor a failure: the tree moved under every
            # attempt, so this row checked nothing. Said out loud,
            # because a row that silently stops being checked reads as a
            # row that passed — the same rule the no-live-session branch
            # above follows.
            churned=$((churned + 1))
            printf 'churned   %s   (tree moved through %d attempts: %s)\n' \
                "$inv" "$TREE_FENCE_ATTEMPTS" "${roots[*]}"
            continue
        fi
    else
        # Unquoted on purpose: the rows above are argument lists.
        # shellcheck disable=SC2086
        a=$("$RS" $inv 2>"$errfile_a"); ea=$?
        # shellcheck disable=SC2086
        b=$(run_b $inv 2>"$errfile_b"); eb=$?
    fi
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
printf 'fence     %d of %d rows read a tree and ran between two digests of it; %d churned out\n' \
    "$fenced" "${#INVOCATIONS[@]}" "$churned"
exit $((unexpected > 125 ? 125 : unexpected))
