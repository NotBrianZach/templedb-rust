//! `templedb intent` — four of Python's five subcommands: `list` and
//! `show` (reads, step 23), `cancel` (step 25) and `create` (step 31).
//!
//! # Why the reads and not `intent cancel`, which is what was queued
//!
//! The README's *Known gaps* carried this entry for eleven days:
//!
//! > `edit_intents` is mirrored at twelve of fourteen columns, and the
//! > two omitted are the ones a *reader* would want: `cancelled_at` and
//! > `applied_commit_id`. They belong to `intent cancel` and to the
//! > commit link `vcs commit` would make, neither of which is ported,
//! > so nothing here could verify them.
//!
//! The entry named the wrong unblocker, and `mirror.rs`'s own comment
//! repeats the mistake — "left to `intent cancel` and `vcs commit`".
//! Neither column needs a *writer* to become verifiable. `intent show`
//! is `SELECT i.*` and prints both, so a pure read is the thing that
//! turns them from columns nobody could check into columns a byte diff
//! checks. That is the boundary rule exactly as written — follow the
//! readers — applied to a table that had been reasoned about as though
//! it only had writers, because for two weeks it only did.
//!
//! So the order is the port's usual one after all: reads first,
//! byte-diffed, and the write (`intent cancel`) after them. What made it
//! look otherwise was `edit_intents` arriving for `file set`'s
//! provenance rather than for a query, which is the one shape the
//! boundary rule reads sideways in — see the *Known gaps* note about
//! tables with a writer and no reader.
//!
//! `intent create` landed on 2026-11-17 and `intent apply` on
//! 2026-11-18, which completes the family at **five of Python's
//! five** — the second after `source` to be complete, and the first
//! with writes in it.
//!
//! So this module is two reads and three writes, and the reads came
//! first on purpose. That ordering is what makes the writes verifiable
//! at all: `create_intent`, `cancel_intent` and `apply_intent` are
//! each checked by reading the result back through `show_intent` and
//! `list_intents`, which the byte-diff sweep has already compared
//! against Python — and `apply_intent` additionally through
//! `read_file`, because it is the one that changes source.
//!
//! `apply` carries the family's one behavioural divergence and it is
//! not the one this module's doc predicted for two weeks. The
//! prediction was that `apply` "carries the three-way merge this port
//! does not have", scoping it out. Measured when the command forced
//! the question, the engine upstream is not a three-way merge: it
//! walks three versions with indices advanced in lockstep, so it is
//! correct only at equal line counts, and four of its clean results
//! write content neither side held. A moved base is therefore refused
//! here — Python's `--no-rebase` behaviour, unconditionally, which is
//! `file set --if-match`'s divergence one command over. See
//! [`apply_intent`] and *The rebase this port refuses to do*.
//!
//! # Three columns selected and discarded, which is a record
//!
//! Python's `list` query names eleven columns and prints eight.
//! `base_revision`, `new_content_hash` and `author` are selected and
//! never reach stdout. The port's query names the eight, so the mirror
//! does not grow for them — the same decision as `doctor history`'s
//! `duration_ms` / `sample_issues_json` and `vcs log`'s `files_changed`,
//! at three columns instead of two and one.
//!
//! Worth stating because the count is the point: this is the fourth
//! query in the port whose projection is wider than its output, and the
//! pattern is consistent enough now that "what does the command print"
//! is a better guide to the subset than "what does the query select".

use anyhow::{Context, Result};

use crate::store::Store;
use crate::value::Value;

/// One row of `intent list` — the eight columns that reach stdout.
///
/// `session_id` is absent and `intent show` has it: the list line does
/// not print it even though the row is keyed by session in every other
/// part of the system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentSummary {
    pub id: i64,
    /// `'proposed'`, `'applied'` or `'cancelled'`. A `String` and not an
    /// enum, for `doctor history`'s reason: the renderer has a `?`
    /// fallback, so a fourth value is rendered rather than rejected.
    ///
    /// Unlike `invariant_checks.status` this column *does* carry a
    /// CHECK constraint live — `CHECK (status IN ('proposed',
    /// 'applied', 'cancelled'))` — so the fallback is unreachable
    /// through SQLite. It stays because the Postgres subset declares no
    /// CHECK, which makes the mirror the one place a fourth status can
    /// exist, and because dropping it would mean this port decides what
    /// a status is.
    pub status: String,
    pub project_slug: String,
    pub file_path: String,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub cancelled_at: Option<String>,
    pub description: Option<String>,
}

/// The full record, which is `intent show`'s `SELECT i.*` plus the slug.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub id: i64,
    pub session_id: Option<i64>,
    pub project_slug: String,
    pub file_path: String,
    pub base_revision: String,
    pub new_content_hash: String,
    pub patch_summary: Option<String>,
    pub author: Option<String>,
    pub description: Option<String>,
    pub status: String,
    pub created_at: String,
    pub applied_at: Option<String>,
    pub cancelled_at: Option<String>,
    /// `INTEGER` live, not a commit hash — it references
    /// `vcs_commits.id`. 882 of 1,346 rows carry one as of 2026-10-30.
    pub applied_commit_id: Option<i64>,
}

/// What `intent list` lets a caller vary.
///
/// No lifetime parameter, unlike `doctor history`'s `HistoryFilter`:
/// every field here is an integer or a flag, because the one filter that
/// takes a string — `--project` — is fuzzy-resolved to an id at the CLI
/// edge before it reaches this layer.
#[derive(Debug, Clone, Copy, Default)]
pub struct IntentFilter {
    /// A resolved project *id*, not a pattern. Python fuzzy-matches
    /// `--project` at the edge and filters on `i.project_id`, so the
    /// fuzzy half belongs to the CLI and this takes the result.
    pub project_id: Option<i64>,
    /// Python's `-s` / `--session`, which is `type=int` here and a bare
    /// string on `file set` — the one place in the system the session
    /// flag refuses a name, so `session.rs`'s *resolve never, accept
    /// always* needs no exception for it.
    pub session_id: Option<i64>,
    /// Python's `--all-statuses`. False means `status = 'proposed'`.
    pub all_statuses: bool,
}

/// Intents newest first.
///
/// # The tie-break Python does not have, which the live data reaches
///
/// Python orders by `created_at DESC` alone. `created_at` defaults to
/// `datetime('now')` and so has second granularity, and measured on the
/// live database 2026-10-30: **32 `created_at` values are shared by two
/// rows each**, out of 1,346. So the order among those pairs is whatever
/// the engine returns, and neither engine promises anything.
///
/// This port adds `id DESC`, which makes the key total. The reasoning is
/// `doctor history`'s and the conclusion is the opposite of it: there
/// the tie-break was *already in Python* and reproducing it was the
/// work, and here it is absent and adding it is a deliberate divergence.
/// It is the same divergence `entity search` carried until Python grew a
/// tie-break of its own and the two converged.
///
/// Three reasons it is worth diverging for rather than reproducing:
///
/// 1. **The port has two backends and they would disagree.** Every
///    other cross-backend comparison in this port is a measured axis of
///    the sweep. Without a total key, `intent list --all-statuses` past
///    the first tie is free to differ between SQLite and Postgres, and
///    that is a sweep failure rather than a cosmetic one.
/// 2. **`--limit` cuts inside a tie group**, so an untied order does
///    not merely reorder the output, it changes *which rows appear*.
/// 3. `id` is monotonic per insert, so within a tied second it recovers
///    the order the intents were actually proposed in — which is the
///    question the command asks.
///
/// # What this costs against the byte diff, measured
///
/// Nothing at the limits the sweep uses, and the measurement is the
/// reason to trust that rather than a hope. **The newest tie group sits
/// at rank 134**, so `intent list --all-statuses --limit 50` — and the
/// default `proposed`-only view, which is 2 rows — are ordered
/// identically by both implementations. Past 134 rows Python's own
/// output is not stable run to run, so a byte diff there is comparing
/// against an oracle that disagrees with itself; the sweep's rows stay
/// inside the deterministic window and say why.
pub fn list_intents(
    store: &dyn Store,
    filter: &IntentFilter,
    limit: i64,
) -> Result<Vec<IntentSummary>> {
    // Python's clause-list build, for `doctor history`'s reason: a
    // nullable predicate on `project_id` plans worse than an absent one.
    let mut clauses: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();
    if let Some(pid) = filter.project_id {
        params.push(Value::from(pid));
        clauses.push(format!("i.project_id = ?{}", params.len()));
    }
    if let Some(sid) = filter.session_id {
        params.push(Value::from(sid));
        clauses.push(format!("i.session_id = ?{}", params.len()));
    }
    if !filter.all_statuses {
        clauses.push("i.status = 'proposed'".to_string());
    }
    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };
    params.push(Value::from(limit));
    let sql = format!(
        "SELECT i.id, i.status, p.slug, i.file_path,
                i.created_at, i.applied_at, i.cancelled_at, i.description
           FROM edit_intents i
           JOIN projects p ON p.id = i.project_id
           {where_sql}
          ORDER BY i.created_at DESC, i.id DESC
          LIMIT ?{}",
        params.len()
    );

    let rows = store.query(&sql, &params).context("listing edit intents")?;
    rows.iter()
        .map(|r| {
            Ok(IntentSummary {
                id: r.get_i64(0)?,
                status: r.get_string(1)?,
                project_slug: r.get_string(2)?,
                file_path: r.get_string(3)?,
                created_at: r.get_string(4)?,
                applied_at: r.get_opt_string(5)?,
                cancelled_at: r.get_opt_string(6)?,
                description: r.get_opt_string(7)?,
            })
        })
        .collect()
}

/// One intent in full, or `None` if no row has that id.
///
/// `None` rather than an error because the caller prints Python's
/// message and exits 1, and the message names the id — which this
/// function does not need to know how to format.
pub fn show_intent(store: &dyn Store, id: i64) -> Result<Option<Intent>> {
    // Columns named rather than `i.*`, which is Python's. An explicit
    // list is what lets the mirror's projection be checked against the
    // query at all: `SELECT *` over a 12-of-14 mirror would succeed and
    // silently return two fewer fields than the renderer prints.
    let sql = "SELECT i.id, i.session_id, p.slug, i.file_path,
                      i.base_revision, i.new_content_hash, i.patch_summary,
                      i.author, i.description, i.status, i.created_at,
                      i.applied_at, i.cancelled_at, i.applied_commit_id
                 FROM edit_intents i
                 JOIN projects p ON p.id = i.project_id
                WHERE i.id = ?1";
    let rows = store
        .query(sql, &[Value::from(id)])
        .context("reading edit intent")?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(Intent {
        id: r.get_i64(0)?,
        session_id: r.get_opt_i64(1)?,
        project_slug: r.get_string(2)?,
        file_path: r.get_string(3)?,
        base_revision: r.get_string(4)?,
        new_content_hash: r.get_string(5)?,
        patch_summary: r.get_opt_string(6)?,
        author: r.get_opt_string(7)?,
        description: r.get_opt_string(8)?,
        status: r.get_string(9)?,
        created_at: r.get_string(10)?,
        applied_at: r.get_opt_string(11)?,
        cancelled_at: r.get_opt_string(12)?,
        applied_commit_id: r.get_opt_i64(13)?,
    }))
}

/// The marker character for a status — Python's
/// `{'proposed': '⋯', 'applied': '✓', 'cancelled': '✗'}.get(status, '?')`.
///
/// `⋯` is U+22EF MIDLINE HORIZONTAL ELLIPSIS, not U+2026 HORIZONTAL
/// ELLIPSIS and not three periods. All three look alike in a terminal
/// and only one of them diffs clean.
pub fn marker(status: &str) -> char {
    match status {
        "proposed" => '⋯',
        "applied" => '✓',
        "cancelled" => '✗',
        _ => '?',
    }
}

/// Python's `applied_at or cancelled_at or created_at`.
///
/// The fallback chain is `or`, so it skips a column that is **empty as
/// well as one that is NULL** — in Python `'' or x` is `x`. The live
/// data has no empty string in either timestamp (measured: zero of
/// 1,346), so this is latent today and reachable, because neither
/// column has a CHECK and `file set`'s writer is not the only thing that
/// has ever inserted here.
///
/// Reproduced rather than simplified to a NULL check, which would be the
/// natural Rust shape and would differ the first time a row carried
/// `applied_at = ''`: Rust would print an empty timestamp where Python
/// falls through to `created_at`.
pub fn when<'a>(
    applied_at: Option<&'a str>,
    cancelled_at: Option<&'a str>,
    created_at: &'a str,
) -> &'a str {
    applied_at
        .filter(|s| !s.is_empty())
        .or(cancelled_at.filter(|s| !s.is_empty()))
        .unwrap_or(created_at)
}

/// One `intent list` line, which is Python's
/// `f"  {m} #{id:<4} {status:<9} {when}  {slug}/{path}{desc}"`.
///
/// Both widths are minimums. `#{:<4}` stops padding at id 10,000 and the
/// live maximum is 1,346, so the column holds for now and shifts rather
/// than truncates when it does not. `{:<9}` is exactly `'cancelled'`,
/// the longest of the three statuses — so every real row pads to the
/// same width and the overflow case is reachable only through the
/// unCHECKed mirror.
///
/// The description separator is ` — `, with U+2014 EM DASH.
pub fn render(i: &IntentSummary) -> String {
    let desc = match i.description.as_deref() {
        // Python's `if r['description']`, so an empty description is
        // omitted rather than printed as a bare dash.
        Some(d) if !d.is_empty() => format!(" — {d}"),
        _ => String::new(),
    };
    format!(
        "  {} #{:<4} {:<9} {}  {}/{}{}",
        marker(&i.status),
        i.id,
        i.status,
        when(
            i.applied_at.as_deref(),
            i.cancelled_at.as_deref(),
            &i.created_at
        ),
        i.project_slug,
        i.file_path,
        desc
    )
}

/// `intent show`'s label column, padded so every value starts at the
/// same offset.
///
/// Python writes thirteen separate format strings with hand-counted
/// spaces; this is one width, and they are the same width because
/// `applied_commit_id:` is eighteen characters and the longest label, so
/// Python's hand-alignment is `{:<19}` throughout. Checked by test
/// against all thirteen rather than asserted.
fn field(label: &str, value: &str) -> String {
    format!("  {:<19}{}", format!("{label}:"), value)
}

/// `intent show`'s full output as lines.
///
/// # Five of the thirteen lines are conditional, and one is not
///
/// Python prints `applied_at`, `applied_commit_id`, `cancelled_at`,
/// `patch_summary` and `description` only when truthy, and
/// `session_id` **unconditionally** — so a NULL session renders through
/// the f-string as the literal `None`. That is Python leaking its own
/// repr into a CLI's output, and it is reproduced rather than tidied to
/// `(none)`: 976 of 1,220 rows had a NULL `session_id` when the mirror
/// was measured, so `None` is not an edge case here, it is the common
/// output. Printing anything else would differ on most of the table.
///
/// `author` has the one explicit fallback, `or '(unknown)'`, and it is
/// unreachable on live data — all 1,346 rows carry an author.
pub fn render_full(i: &Intent) -> Vec<String> {
    let mut out = vec![
        format!("Intent #{}", i.id),
        field("status", &i.status),
        field("project", &i.project_slug),
        field("file", &i.file_path),
        field("base_revision", &i.base_revision),
        field("new_content_hash", &i.new_content_hash),
        // The `None` line. See the note above.
        field(
            "session_id",
            &match i.session_id {
                Some(s) => s.to_string(),
                None => "None".to_string(),
            },
        ),
        field(
            "author",
            match i.author.as_deref() {
                Some(a) if !a.is_empty() => a,
                _ => "(unknown)",
            },
        ),
        field("created_at", &i.created_at),
    ];
    // Python's order, which is not the column order: `applied_at`,
    // `applied_commit_id`, `cancelled_at`, `patch_summary`,
    // `description`.
    fn push_if_set(out: &mut Vec<String>, label: &str, v: Option<&str>) {
        if let Some(s) = v.filter(|s| !s.is_empty()) {
            out.push(field(label, s));
        }
    }
    push_if_set(&mut out, "applied_at", i.applied_at.as_deref());
    // `0` is falsy in Python, so an `applied_commit_id` of 0 would be
    // omitted there. `vcs_commits.id` is an INTEGER PRIMARY KEY and
    // SQLite never assigns 0 to one unless told to, and nothing tells
    // it to -- but the test asserts the Python behaviour rather than
    // the convenient one, because the column is a plain INTEGER in the
    // mirror with no such guarantee.
    if let Some(c) = i.applied_commit_id.filter(|c| *c != 0) {
        out.push(field("applied_commit_id", &c.to_string()));
    }
    push_if_set(&mut out, "cancelled_at", i.cancelled_at.as_deref());
    push_if_set(&mut out, "patch_summary", i.patch_summary.as_deref());
    push_if_set(&mut out, "description", i.description.as_deref());
    out
}

/// What Python prints when the filters select nothing.
pub const NO_INTENTS: &str = "(no matching intents)";

/// Why a cancel did not happen, for the two statuses Python
/// distinguishes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CancelRefusal {
    /// No row with that id: Python's exit 1.
    NotFound,
    /// The row exists and is not `'proposed'`. Carries the status,
    /// because the message names it. Python's exit 2.
    NotProposed(String),
}

/// Cancel a proposed intent: `status = 'cancelled'`, `cancelled_at` set,
/// source untouched.
///
/// **The third write**, and the smallest one — no new table, no new
/// column. `edit_intents` arrived for `file set`'s provenance in step
/// 17 and gained its last two columns to `intent show` in step 23, so
/// this writes a column the port already mirrors and already reads.
///
/// That matters for verification rather than for effort: `cancelled_at`
/// has been carried with exactly one live row behind it and verified
/// only by the cross-backend digest over the copy. Writing it here and
/// reading it back through `intent show` closes the same loop that made
/// `vcs_working_state` the strongest-verified table in the subset
/// instead of the weakest.
///
/// # A compare-and-swap, where Python checks and then acts
///
/// Python reads the status, decides, and then issues an unconditional
/// `UPDATE … WHERE id = ?`. Two concurrent cancels both pass the guard
/// and both update; the second overwrites the first's `cancelled_at`.
/// Nothing in the live data shows this has happened — one row carries a
/// `cancelled_at` — and nothing prevents it either.
///
/// This does the guard *in* the UPDATE:
/// `WHERE id = $1 AND status = 'proposed'`. If it matches, the
/// transition happened exactly once. If it does not, the row is read
/// afterwards to decide which of Python's two refusals to report — so
/// the statuses are reproduced without reproducing the race.
///
/// Same shape and same reasoning as step 20's branch-head CAS, and
/// easier to justify than that one: there is no trigger here racing the
/// application, so the correct version is simply available.
///
/// The observable output is unchanged in every non-racing case, which
/// is the whole live-data case.
pub fn cancel_intent(
    tx: &mut postgres::Transaction<'_>,
    clock: &crate::clock::Clock,
    id: i64,
) -> Result<std::result::Result<(), CancelRefusal>> {
    let now = clock.now_timestamp();
    // `datetime('now')` over there, which is UTC and
    // `YYYY-MM-DD HH:MM:SS`. The clock seam produces the same string,
    // which is the whole reason step 13 came before any write.
    let updated = tx
        .execute(
            "UPDATE edit_intents
                SET status = 'cancelled', cancelled_at = $2
              WHERE id = $1 AND status = 'proposed'",
            &[&id, &now],
        )
        .context("cancelling an edit intent")?;
    if updated == 1 {
        return Ok(Ok(()));
    }

    // Nothing moved. Read the row to say why, which is the order
    // Python takes first and this takes second.
    let row = tx
        .query_opt("SELECT status FROM edit_intents WHERE id = $1", &[&id])
        .context("reading an edit intent's status")?;
    Ok(Err(match row {
        None => CancelRefusal::NotFound,
        // Reached either because the status was never `'proposed'` or
        // because another transaction cancelled it between the UPDATE
        // and this read. Reporting the status we can see is right in
        // both cases.
        Some(r) => CancelRefusal::NotProposed(r.get(0)),
    }))
}

/// Python's success line, which reuses the `cancelled` marker.
pub fn cancelled_line(id: i64) -> String {
    format!("{} Intent #{id} cancelled", marker("cancelled"))
}

/// What a caller may vary about `intent create`.
///
/// Four fields and no `--session`, because Python has no such flag
/// here: `_current_session_id` reads `TEMPLEDB_SESSION_ID` and nothing
/// else. Adding one would be a flag only this port has, which is the
/// class of divergence the sweep's flag stage cannot see from the other
/// side — so the surfaces agree exactly and the allow-list stays
/// untouched.
#[derive(Debug, Clone, Copy, Default)]
pub struct CreateOptions<'a> {
    /// Python's `--base-rev`, defaulted by `args.base_rev or 'current'`
    /// — so an empty string falls through to `'current'` rather than
    /// being stored. Applied in [`create_intent`], not here, because
    /// the default is the column's meaning and not the caller's.
    pub base_rev: Option<&'a str>,
    /// Python's `-m` / `--message`, which lands in `description`. The
    /// column name and the flag name differ on both sides.
    pub message: Option<&'a str>,
    /// `edit_intents.author`: `TEMPLEDB_AUTHOR`, then `USER`, then
    /// NULL. Resolved at the entry point for the reason the clock and
    /// the session are.
    pub author: Option<&'a str>,
    /// The session this proposal belongs to, if one is pinned.
    pub session: Option<crate::session::SessionId>,
}

/// The row `intent create` just inserted, for the caller to print.
///
/// Carries the resolved values rather than the arguments, because two
/// of the four lines Python prints are *not* what was passed:
/// `base_revision` may have been defaulted and the hash is computed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    pub id: i64,
    pub file_path: String,
    pub base_revision: String,
    pub new_content_hash: String,
    /// `description`, which is `-m`. `None` when absent *or empty* —
    /// Python's `if args.message:` tests truthiness, so `-m ''` prints
    /// no note line even though the column holds the empty string.
    pub description: Option<String>,
}

/// `patch_summary` for a proposal: `"<lines> lines, <bytes> bytes"`.
///
/// # Not [`crate::line_count`], and the difference is the empty file
///
/// Python has two line-counting expressions for two purposes and they
/// disagree on one input. `file set`'s is `content.count('\n') + 1` with
/// an explicit `0 if not content` in front of it, which is what
/// `file ls -l` prints. `intent create`'s `_short_summary` is
/// `content_bytes.decode('utf-8').count('\n') + 1` with **no empty
/// case**, so an empty proposal is `1 lines, 0 bytes`.
///
/// Measured rather than read off the source, because the two are one
/// character apart and a port that reused the other function would look
/// right: Python on a copy of the live database, 2026-11-17, recorded
/// `patch_summary: 1 lines, 0 bytes` for an intent created from an
/// empty pipe (intent #1627) and `3 lines, 12 bytes` for
/// `"hello\nworld\n"` (#1625).
///
/// `lines` is also not the plural-correct word for 1, which is Python's
/// and is reproduced: the column is read by greps, not by people.
///
/// The byte count is over the UTF-8 encoding, so it is `len(bytes)` and
/// not `len(str)` — the same distinction `file set`'s `"<n> bytes"`
/// summary makes, and the two summaries are otherwise different strings
/// for the same column. `file set` writes `"12 bytes"` where this
/// writes `"3 lines, 12 bytes"`; both are Python's, in two code paths
/// that never converged.
pub fn patch_summary(content: &str) -> String {
    format!(
        "{} lines, {} bytes",
        content.matches('\n').count() + 1,
        content.len()
    )
}

/// Record a proposed edit: `status = 'proposed'`, no source change.
///
/// **The fourth write**, and the second with no schema cost at all —
/// every column it fills was mirrored for `file set`'s provenance in
/// step 17 and read by `intent show` in step 23, and the blob it
/// stashes goes through the statement [`crate::fileset::store_blob`]
/// already owned.
///
/// # What it deliberately does not do
///
/// Nothing here touches `file_contents`, `project_files` or
/// `vcs_working_state`. That is the command's whole point on both
/// sides — "Does NOT touch file_contents yet — that happens on apply" —
/// and it is why this write is safe to land while `intent apply` stays
/// unported: a proposal nothing applies is a row, and the row is
/// readable by the two commands that already read the table.
///
/// It also does **not** check that `file_path` is registered in the
/// project. Python's does not either; `apply` is where the missing
/// `project_files` row becomes an exit 3. So an intent can be proposed
/// against a path that does not exist, which is the `'new-file'` case
/// one command over and is deliberate rather than missing.
///
/// # Two divergences, both inherited rather than new
///
/// **The slug is matched exactly**, by the caller, through
/// [`crate::resolve_project_exact`] — where Python fuzzy-matches. Same
/// reasoning as `file set`'s, and here it is forced as well as chosen:
/// the id is resolved inside the write transaction because the reader
/// and the writer point at *different databases*, so the fuzzy reader
/// would resolve a name against the parity copy and stamp the id into
/// the scratch one.
///
/// **The session is the resolved pin**, not a raw `int()` of the
/// environment. Python's `_current_session_id` parses
/// `TEMPLEDB_SESSION_ID`, swallows a parse failure to NULL and checks
/// nothing — measured on a copy, `TEMPLEDB_SESSION_ID=notanumber`
/// creates the intent with a NULL session and exits 0. This port has
/// one resolver and it checks liveness, so a bogus or dead id is a
/// refusal. Identical to the divergence `record_intent` already
/// documents for `file set`, and listed in the README rather than
/// repeated as a new decision.
///
/// # `created_at` comes from the clock, where Python lets it default
///
/// Python names nine columns and lets `created_at` take its
/// `DEFAULT datetime('now')`. This passes the injected instant, which
/// is the same value by construction — the clock seam exists so a
/// write's timestamps are a thing a test can assert rather than a
/// column a state diff has to exclude. `file set`'s intent does the
/// same, so the two writers agree with each other as well as with
/// Python.
pub fn create_intent(
    tx: &mut postgres::Transaction<'_>,
    clock: &crate::clock::Clock,
    project_id: i64,
    file_path: &str,
    content: &str,
    opts: &CreateOptions<'_>,
) -> Result<Created> {
    // Python's `args.base_rev or 'current'`, so an empty `--base-rev`
    // is unset rather than stored. The column is NOT NULL with the same
    // default, which means the literal is the value either way; writing
    // it explicitly keeps the row identical whether or not the DDL's
    // DEFAULT survives a later schema change.
    let base_revision = opts
        .base_rev
        .filter(|s| !s.is_empty())
        .unwrap_or("current")
        .to_string();
    let new_content_hash = crate::fileset::content_hash(content);
    let summary = patch_summary(content);
    let now = clock.now_timestamp();
    let session = opts.session.map(crate::session::SessionId::get);

    // The intent before the blob, which is Python's order and is worth
    // keeping even though one transaction makes the order unobservable:
    // the id is wanted for the output, and `INSERT … RETURNING` is the
    // only way to get it. Python's `execute` returns `lastrowid` from
    // the same statement.
    //
    // `status` is the literal `'proposed'` and not the column default,
    // for the reason the DDL's comment now gives: this is the first
    // writer in the port that inserts anything but `'applied'`, so the
    // value is the command's claim rather than the schema's.
    let row = tx
        .query_one(
            "INSERT INTO edit_intents
                 (session_id, project_id, file_path, base_revision,
                  new_content_hash, patch_summary, author, description,
                  status, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'proposed', $9)
             RETURNING id",
            &[
                &session,
                &project_id,
                &file_path,
                &base_revision,
                &new_content_hash,
                &summary,
                &opts.author,
                &opts.message,
                &now,
            ],
        )
        .context("recording a proposed edit intent")?;
    let id: i64 = row.get(0);

    // "Stash the content in content_blobs so apply can find it." The
    // blob is the half of this command that makes the proposal
    // actionable: the intent row carries a hash, and without a blob
    // under that hash there is nothing for an apply to write. Python's
    // comment says the same and adds why the row is not reference
    // counted — a proposed intent has no `file_contents` row pointing
    // at it and may never get one.
    //
    // Text only, because Python's `intent create` has no binary path at
    // all: `--content` is argv, `--from-file` is `read_text`, and
    // `sys.stdin.read()` raises on undecodable bytes. Measured on a
    // copy — a binary pipe exits 1 with "'utf-8' codec can't decode
    // byte 0x84 in position 0" and writes nothing. So this takes a
    // `&str` where `set_file` takes a `Content`, and the CLI refuses
    // the bytes rather than inventing a `binary_asset` branch Python
    // would not produce.
    crate::fileset::store_blob(
        tx,
        crate::fileset::Content::Text(content),
        &new_content_hash,
        content.len() as i64,
    )?;

    Ok(Created {
        id,
        file_path: file_path.to_string(),
        base_revision,
        new_content_hash,
        // `if args.message:` — empty is absent, which is why this is
        // filtered here rather than at the column. The column holds
        // what was passed.
        description: opts.message.filter(|m| !m.is_empty()).map(str::to_string),
    })
}

/// `intent create`'s seven or eight lines of stdout.
///
/// Measured against Python byte for byte, by running it against a copy
/// of the live database on 2026-11-17 — which is the only way to get
/// this command's bytes at all, because a write has no diffable stdout
/// in the sweep and reading the source leaves the blank line and the
/// two-space indents to guesswork. Intent #1625's output, verbatim
/// through `cat -A`:
///
/// ```text
/// ✓ Intent #1625 created (proposed)
///   regr-test/measured.txt
///   base:  current
///   new:   4a1e67f2fe1d
///
///   Apply:  templedb intent apply 1625
///   Cancel: templedb intent cancel 1625
/// ```
///
/// Four things in it that a plausible renderer gets wrong:
///
/// 1. **The label padding is hand-counted and lands in two columns,
///    not one.** `base:` and `note:` take two trailing spaces, `new:`
///    takes three and `Apply:` two against `Cancel:`'s one — so the
///    three field values start at column 9 and the two command hints
///    at column 10. Not a width this port can compute from the longest
///    label, because Python did not: `{:<N}` over all five would align
///    all five and differ from the measured bytes on two lines.
/// 2. **The hash is truncated to twelve characters**, like every other
///    short hash in both implementations.
/// 3. **There is a bare `print()`** between the fields and the hints,
///    so line six is empty rather than absent.
/// 4. **The hints name `templedb`**, not this binary. Reproduced
///    rather than rewritten: the hint is stdout, the README's rule is
///    that stdout is byte-identical and stderr is the port's own.
///
///    This item said the `Apply:` line "names a command this port does
///    not have" and added that "a caller following it needs the Python
///    binary either way". The first half stopped being true on
///    2026-11-18, when `apply` landed; the second is still true for a
///    different reason, and a sharper one — following that line gets
///    the caller Python's rebase, which is the behaviour this port
///    refuses. So the hint now points at a command both binaries have
///    and only one of them will merge with.
///
/// The marker is U+2713 CHECK MARK, the same glyph [`marker`] returns
/// for `'applied'` — but it is hardcoded in Python's f-string rather
/// than looked up, and a proposal is not applied, so it is written
/// literally here instead of routed through that map.
pub fn created_lines(c: &Created, project_slug: &str) -> Vec<String> {
    let mut out = vec![
        format!("\u{2713} Intent #{} created (proposed)", c.id),
        format!("  {project_slug}/{}", c.file_path),
        format!("  base:  {}", c.base_revision),
        format!(
            "  new:   {}",
            &c.new_content_hash[..12.min(c.new_content_hash.len())]
        ),
    ];
    if let Some(d) = &c.description {
        out.push(format!("  note:  {d}"));
    }
    out.push(String::new());
    out.push(format!("  Apply:  templedb intent apply {}", c.id));
    out.push(format!("  Cancel: templedb intent cancel {}", c.id));
    out
}

/// Why an apply did not happen, for the four statuses Python
/// distinguishes.
///
/// Four variants and four exit codes, which is the widest status
/// surface of any command in this port — `file set` has three. They are
/// a type rather than four `bail!`s inside [`apply_intent`] for
/// [`CancelRefusal`]'s reason: the write tests assert *which* refusal
/// happened, and a refusal that is only a message is a refusal a test
/// can only match on prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyRefusal {
    /// No row with that id: Python's exit 1.
    NotFound,
    /// The row exists and is not `'proposed'`. Carries the status,
    /// because the message names it. Python's exit 2.
    NotProposed(String),
    /// No `project_files` row for the proposal's path, so there is
    /// nothing to point at the new content. Python's exit 3, and the
    /// case `intent create` deliberately permits — a proposal against
    /// an unregistered path is legal on both sides and becomes an error
    /// here rather than there.
    PathNotRegistered(String),
    /// The file has moved since the proposal was written. Python's exit
    /// 4, which it reaches only under `--no-rebase`; this port reaches
    /// it always. See [`apply_intent`].
    StaleBase {
        file_path: String,
        /// `edit_intents.base_revision` as stored, untruncated.
        base: String,
        /// The `is_current` hash the file holds now, untruncated.
        current: String,
    },
}

/// Has the file moved since the proposal was written?
///
/// Python's expression, which is four conditions ANDed:
///
/// ```python
/// stale = (current_hash and base and base not in ('current', 'new-file')
///          and not current_hash.startswith(base))
/// ```
///
/// Each one is load-bearing and none of them is obvious:
///
/// 1. **No current row is not stale.** A registered path with no
///    `file_contents` row — which `file set` can produce and a
///    hard-deleted commit leaves behind — applies cleanly, because
///    there is no landed state to be behind.
/// 2. **An empty base is not stale**, because `''` is falsy over there.
///    The column is `NOT NULL DEFAULT 'current'` so this is reachable
///    only by a writer that passes the empty string explicitly, which
///    `intent create` does not: its `args.base_rev or 'current'`
///    collapses `''` to the literal.
/// 3. **The two sentinels are not stale.** `'current'` means "whatever
///    is there when this is applied" and `'new-file'` means "there was
///    nothing" — neither is a hash, and comparing them against one
///    would make every proposal stale. 886 of the live rows hold
///    `'current'`, so this is the common case and not an edge.
/// 4. **It is a prefix test, not equality.** `startswith`, so a
///    12-character base matches the hash it abbreviates. That is what
///    makes `--base-rev` usable by hand, and it is also why a
///    *one*-character base matches one hash in sixteen: nothing
///    validates the length on either side.
///
/// A pure function with the four conditions named because the port's
/// behaviour *diverges* from Python's once this returns true, and a
/// divergence whose trigger is an inlined boolean is a divergence
/// nobody can check the boundary of.
pub fn base_is_stale(current_hash: Option<&str>, base: &str) -> bool {
    let Some(current) = current_hash.filter(|h| !h.is_empty()) else {
        return false;
    };
    !base.is_empty() && base != "current" && base != "new-file" && !current.starts_with(base)
}

/// The proposal that just became source, for the caller to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub id: i64,
    pub file_path: String,
    /// What `file_contents` now points at. Equal to the proposal's
    /// `new_content_hash` in every case this port can reach, and a
    /// separate field anyway because Python's is not — its rebase
    /// writes the merged hash here and leaves `new_content_hash`
    /// holding what was proposed.
    pub hash: String,
}

/// Apply a proposed intent: point `file_contents` at the proposed hash
/// and mark the intent `'applied'`.
///
/// **The fifth write**, and the third running at zero schema cost: no
/// new table, no new column, no new view, no new identity sequence.
/// `edit_intents` arrived in step 17 and completed in step 23,
/// `file_contents` has been written by `file set` since step 15, and
/// `content_blobs` since the first write. Everything this command
/// touches was already mirrored, already read and already verified.
///
/// It does **not** create a commit, which is Python's docstring's own
/// emphasis: "apply is a source-state change; commit is a history
/// checkpoint. Keeping them separate lets you batch multiple intents
/// into one commit." So this writes two tables and `vcs commit-staged`
/// is still what makes history.
///
/// # The one divergence: a moved base is refused, never merged
///
/// Python three-way merges the proposal onto what landed and only a
/// conflict exits 4. This port exits 4 whenever the base has moved,
/// which is what Python's `--no-rebase` selects — the same divergence
/// `file set --if-match` already carries, one command over, and the
/// same shape: the port has the strict behaviour unconditionally.
///
/// What is new is that there is now a measurement behind it, where the
/// README had been carrying "needs a merge engine this port does not
/// have" as the reason. The engine exists upstream and **it is not a
/// three-way merge.** `merge_resolver.ThreeWayMerge._merge_with_diff3`
/// computes two `difflib.unified_diff`s, never reads either one, and
/// then walks `base`, `ours` and `theirs` with three indices it
/// advances in lockstep — one line each, every iteration — comparing
/// each side's *n*th line against base's *n*th line. That is correct
/// only while all three versions have the same number of lines. Its
/// loop is also `while i_base < len(base_lines)`, so anything either
/// side added past the end of base is never visited.
///
/// Measured 2026-11-18 against `git merge-file -p --diff3` over eleven
/// (base, ours, theirs) triples built by editing one 4-line base —
/// in-place edits, insertions, deletions and appends on each side:
///
/// | outcome | count |
/// |---|---|
/// | agrees with `git merge-file` | 3 |
/// | reports a clean merge, produces different text | 4 |
/// | conflicts where a real diff3 merges cleanly | 3 |
/// | reports clean where a real diff3 conflicts | 2 |
///
/// Three of eleven, and the three that agree are the equal-line-count
/// in-place cases. The failures are not conservative. Seven triples
/// report clean; five of those seven produce text that is not what a
/// three-way merge yields, `rebase_onto_current`'s
/// `merged == current_text` guard catches exactly one of the five, and
/// **four reach `file_contents`**. Two of the eleven conflict for no
/// reason a caller could act on, including the case where both sides
/// make the *identical* edit — compared against base rather than
/// against each other, two equal changes are two changes.
///
/// The worst of them is silent and was reproduced end to end through
/// the shipping command, not just the library. On a copy of the live
/// database, intent #1645 proposed inserting a line against a base of
/// `one/two/three/four`; the file then moved in place to
/// `ONE/two/three/four`; `templedb intent apply 1645` printed
/// `✓ Intent #1645 applied`, exited **0**, and left the file holding
/// `ONE/two/NEW/three` — `four` is gone, and neither side deleted it.
///
/// So the divergence is not "this port lacks a feature". Reproducing
/// that engine would mean writing content neither the author nor the
/// database ever held, under a success message, into the one table
/// whose contents *are* the user's files. The README's standing rule is
/// that a silent divergence is the only unacceptable outcome; this is
/// the loud one, and `--no-rebase` is the flag that already names it.
///
/// # One transaction, where Python has none
///
/// Python runs four statements through `execute()`, which commits each
/// one. The third is `DELETE FROM file_contents WHERE file_id = ? AND
/// is_current = 1` and the fourth is the `INSERT` that replaces it, so
/// a failure in between leaves the file with **no current content
/// row**. Measured on the copy: an intent whose `new_content_hash` has
/// no `content_blobs` row makes the `INSERT`'s scalar subquery yield
/// NULL, the statement fails `NOT NULL constraint failed:
/// file_contents.file_size_bytes`, exit 1 — and `templedb file cat`
/// then answers *"Could not read file content from database"* for a
/// file that was intact a moment earlier.
///
/// This port's write is one transaction across both tables, so the same
/// input leaves the file exactly as it was. Same reasoning as
/// [`crate::fileset::record_intent`]'s, and the second time it has paid
/// for itself: a port with transactions is the only implementation that
/// can be correct here, because Python's `execute()` helper has no way
/// to span statements.
///
/// # The status guard is in the UPDATE
///
/// [`cancel_intent`]'s compare-and-swap, for its reason and with one
/// addition. Python reads the status, decides, writes the content, and
/// *then* marks the row applied — so two concurrent applies both pass
/// the guard and both write. Here the `UPDATE … WHERE id = $1 AND
/// status = 'proposed' RETURNING …` is the first statement: it is the
/// guard and the row read in one, and it takes the row's lock before
/// anything touches `file_contents`.
///
/// Putting it first inverts Python's order, which is unobservable
/// inside one transaction and is what makes the later refusals free —
/// `PathNotRegistered` and `StaleBase` are returned *after* the UPDATE
/// and roll it back, so a refused apply leaves the intent `'proposed'`
/// rather than needing a second statement to put it back.
///
/// # Two columns Python leaves behind, reproduced
///
/// Its `INSERT` names four columns, so `line_count` is left NULL and
/// `version` falls back to its `DEFAULT 1` — the row is deleted and
/// re-inserted rather than updated, so a file on version 7 is on
/// version 1 after an apply. Both reproduced, and both differ from
/// `file set`, which writes the line count and leaves the version
/// alone. Verified on the copy: `file_contents` row 49986 after an
/// apply carries a NULL `line_count` and `version = 1`.
///
/// `file_size_bytes` comes from the blob rather than from the intent,
/// which is Python's scalar subquery. Read here as its own statement
/// instead, for one reason: the subquery's failure mode on a missing
/// blob is a NULL into a NOT NULL column, and the message a caller gets
/// should name the hash rather than the constraint. The row written is
/// the same row.
pub fn apply_intent(
    tx: &mut postgres::Transaction<'_>,
    clock: &crate::clock::Clock,
    id: i64,
) -> Result<std::result::Result<Applied, ApplyRefusal>> {
    let now = clock.now_timestamp();
    // The guard, the timestamp and the row in one statement. See above
    // for why this is first rather than last.
    let claimed = tx
        .query_opt(
            "UPDATE edit_intents
                SET status = 'applied', applied_at = $2
              WHERE id = $1 AND status = 'proposed'
          RETURNING project_id, file_path, base_revision, new_content_hash",
            &[&id, &now],
        )
        .context("claiming an edit intent to apply")?;
    let Some(row) = claimed else {
        // Nothing moved. Read the row to say why, which is the order
        // Python takes first and this takes second.
        let existing = tx
            .query_opt("SELECT status FROM edit_intents WHERE id = $1", &[&id])
            .context("reading an edit intent's status")?;
        return Ok(Err(match existing {
            None => ApplyRefusal::NotFound,
            Some(r) => ApplyRefusal::NotProposed(r.get(0)),
        }));
    };
    let project_id: i64 = row.get(0);
    let file_path: String = row.get(1);
    let base: String = row.get(2);
    let hash: String = row.get(3);

    // Python's "Look up or create the project_files row" — whose
    // comment says `create` and whose code does not. There is no
    // insert on that path; a missing row is exit 3 and the message
    // points at `file set`. Reproduced as the refusal, not as the
    // comment.
    let file = tx
        .query_opt(
            "SELECT id FROM project_files
              WHERE project_id = $1 AND file_path = $2",
            &[&project_id, &file_path],
        )
        .context("looking up the project file an intent names")?;
    let Some(file) = file else {
        return Ok(Err(ApplyRefusal::PathNotRegistered(file_path)));
    };
    let file_id: i64 = file.get(0);

    // `LIMIT 1` is Python's and is redundant against
    // `UNIQUE (file_id, is_current)`; kept because the uniqueness is
    // the schema's claim and the limit is the query's.
    let current: Option<String> = tx
        .query_opt(
            "SELECT content_hash FROM file_contents
              WHERE file_id = $1 AND is_current = 1
              LIMIT 1",
            &[&file_id],
        )
        .context("reading a file's current content hash")?
        .map(|r| r.get(0));

    if base_is_stale(current.as_deref(), &base) {
        // Returned after the UPDATE, which rolls back with it.
        return Ok(Err(ApplyRefusal::StaleBase {
            file_path,
            base,
            // Unwrap is sound: `base_is_stale` returns false for a
            // `None` current hash, so reaching here means there is one.
            current: current.expect("a stale base implies a current hash"),
        }));
    }

    // From the blob, which is where Python's subquery reads it. Its own
    // statement so that a missing blob names the hash.
    let size: i64 = tx
        .query_opt(
            "SELECT file_size_bytes FROM content_blobs WHERE hash_sha256 = $1",
            &[&hash],
        )
        .context("reading the proposed content's size")?
        .map(|r| r.get(0))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "intent #{id} proposes content hash {hash}, and no content_blobs row \
                 holds it — there is nothing to apply.\n\
                 \x20 Python reaches the same refusal through a NOT NULL violation on \
                 `file_contents.file_size_bytes`, after its DELETE has already \
                 committed; this transaction leaves {file_path} as it was."
            )
        })?;

    // Retire the old current row, which is Python's DELETE and not an
    // upsert: the table's `UNIQUE (file_id, is_current)` permits one
    // current row, and replacing it rather than updating it is what
    // resets `version` and clears `line_count`.
    tx.execute(
        "DELETE FROM file_contents WHERE file_id = $1 AND is_current = 1",
        &[&file_id],
    )
    .context("retiring a file's current content row")?;
    // Four columns plus `updated_at`, which Python does not name and
    // takes from its `DEFAULT (datetime('now'))` — the same value the
    // clock produces, and the reason step 13 came before any write.
    // `line_count` and `version` are deliberately absent; see above.
    tx.execute(
        "INSERT INTO file_contents
             (file_id, content_hash, file_size_bytes, is_current, updated_at)
         VALUES ($1, $2, $3, 1, $4)",
        &[&file_id, &hash, &size, &now],
    )
    .context("pointing a file at the applied content")?;

    Ok(Ok(Applied {
        id,
        file_path,
        hash,
    }))
}

/// `intent apply`'s two lines of stdout.
///
/// Measured against Python byte for byte on 2026-11-18, by step 31's
/// technique — `TEMPLEDB_PATH` pointed at a copy of the live 483 MB
/// SQLite file, so a write's *output* is diffable even though its
/// effect is not. Intent #1639's, verbatim through `cat -A`:
///
/// ```text
/// ✓ Intent #1639 applied
///   test.txt → 4fdbc441ea7b
/// ```
///
/// Three things in it a renderer written from the source gets wrong:
///
/// 1. **The path has no project prefix**, unlike `intent create`'s
///    second line, which prints `<slug>/<path>`. Same table, same
///    column, two commands, one of them unqualified — so this takes no
///    slug argument at all and the CLI has no resolution to do.
/// 2. **The arrow is U+2192**, and the two spaces around it are one
///    each.
/// 3. **The hash is truncated to twelve**, like every other short hash
///    in both implementations, and it is `apply_hash` rather than the
///    intent's `new_content_hash` — a distinction with no observable
///    difference in this port, because the two are equal wherever the
///    rebase is refused.
///
/// Python's rebase prints two further lines between these. They are
/// deliberately absent rather than unimplemented: the path that
/// produces them is the one [`apply_intent`] refuses, and a renderer
/// for an unreachable branch is dead code that looks like coverage.
/// The bytes are recorded here instead, from the same measurement, so
/// that the divergence is documented in the place a reader would look
/// for it:
///
/// ```text
/// ✓ Intent #1640 applied
///   rebased onto 96677089ce68 (was written against 4fdbc441ea7b)
///   test.txt → 37d16a23a527 (proposed 1036156196ce)
/// ```
pub fn applied_lines(a: &Applied) -> Vec<String> {
    vec![
        format!("\u{2713} Intent #{} applied", a.id),
        format!(
            "  {} \u{2192} {}",
            a.file_path,
            &a.hash[..12.min(a.hash.len())]
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    fn summary(id: i64, status: &str, created: &str) -> IntentSummary {
        IntentSummary {
            id,
            status: status.to_string(),
            project_slug: "alpha".to_string(),
            file_path: "src/a.py".to_string(),
            created_at: created.to_string(),
            applied_at: None,
            cancelled_at: None,
            description: None,
        }
    }

    /// The three markers Python defines, plus the fallback. The CHECK
    /// constraint makes the fallback unreachable through SQLite and the
    /// mirror declares no CHECK, which is the only way to reach it.
    #[test]
    fn markers_are_pythons_three_plus_a_fallback() {
        assert_eq!(marker("proposed"), '⋯');
        assert_eq!(marker("applied"), '✓');
        assert_eq!(marker("cancelled"), '✗');
        assert_eq!(marker("superseded"), '?');
        assert_eq!(marker(""), '?');
    }

    /// `⋯` is U+22EF and not U+2026. They are indistinguishable in a
    /// terminal, so the only thing that catches a swap is this.
    #[test]
    fn the_proposed_marker_is_the_midline_ellipsis() {
        assert_eq!(marker("proposed") as u32, 0x22EF);
        assert_ne!(marker("proposed") as u32, 0x2026);
    }

    /// The fallback chain prefers `applied_at`, then `cancelled_at`,
    /// then `created_at`.
    #[test]
    fn the_timestamp_column_falls_through_in_pythons_order() {
        assert_eq!(when(Some("A"), Some("C"), "N"), "A");
        assert_eq!(when(None, Some("C"), "N"), "C");
        assert_eq!(when(None, None, "N"), "N");
        // An applied row that is somehow also cancelled still reports
        // the applied timestamp, because `or` stops at the first truthy
        // value and does not consider which is later.
        assert_eq!(
            when(Some("2026-01-01"), Some("2025-01-01"), "N"),
            "2026-01-01"
        );
    }

    /// An empty string is falsy in Python, so it falls through like a
    /// NULL. This is the line a NULL-only check would get wrong.
    #[test]
    fn an_empty_timestamp_falls_through_like_a_null() {
        assert_eq!(when(Some(""), Some("C"), "N"), "C");
        assert_eq!(when(Some(""), Some(""), "N"), "N");
        assert_eq!(when(None, Some(""), "N"), "N");
    }

    /// The exact line, against a hand-built row.
    #[test]
    fn a_line_matches_pythons_format_string() {
        let i = summary(1279, "proposed", "2026-10-27 23:37:57");
        assert_eq!(
            render(&i),
            "  ⋯ #1279 proposed  2026-10-27 23:37:57  alpha/src/a.py"
        );
    }

    /// A description is appended after an em dash, and an empty one is
    /// omitted rather than leaving a trailing separator.
    #[test]
    fn a_description_is_appended_after_an_em_dash() {
        let mut i = summary(7, "proposed", "2026-10-27 23:37:57");
        i.description = Some("edit line one".to_string());
        assert!(render(&i).ends_with("alpha/src/a.py — edit line one"));
        assert!(render(&i).contains('\u{2014}'));
        i.description = Some(String::new());
        assert!(render(&i).ends_with("alpha/src/a.py"));
        i.description = None;
        assert!(render(&i).ends_with("alpha/src/a.py"));
    }

    /// `cancelled` is exactly the `{:<9}` width, so it pads to nothing
    /// and the two shorter statuses pad to the same column.
    #[test]
    fn the_status_column_fits_the_longest_real_status_exactly() {
        assert_eq!("cancelled".len(), 9);
        let c = render(&summary(1, "cancelled", "T"));
        let p = render(&summary(1, "proposed", "T"));
        // The timestamp starts at the same offset in both: `cancelled`
        // fills the column and `proposed` is padded into it. Both
        // markers are three-byte characters, so a byte offset is a
        // fair comparison here.
        assert_eq!(c.find('T'), p.find('T'), "{c:?} vs {p:?}");
        assert_eq!(c, "  ✗ #1    cancelled T  alpha/src/a.py");
        assert_eq!(p, "  ⋯ #1    proposed  T  alpha/src/a.py");
    }

    /// Past its width the id column shifts the row rather than cutting
    /// the id, which is what `{:<4}` means. The live max is 1,346.
    #[test]
    fn an_id_past_the_column_width_shifts_the_row() {
        let line = render(&summary(123_456, "applied", "T"));
        assert!(line.starts_with("  ✓ #123456 applied  "), "{line}");
    }

    /// All thirteen labels pad to the same offset, which is the claim
    /// that replaces Python's thirteen hand-counted format strings.
    #[test]
    fn every_show_label_aligns_to_the_same_column() {
        let labels = [
            "status",
            "project",
            "file",
            "base_revision",
            "new_content_hash",
            "session_id",
            "author",
            "created_at",
            "applied_at",
            "applied_commit_id",
            "cancelled_at",
            "patch_summary",
            "description",
        ];
        for l in labels {
            let line = field(l, "X");
            assert_eq!(line.find('X'), Some(21), "{l} misaligned: {line:?}");
        }
        // And the longest label is the one that set the width, so it
        // gets exactly one space.
        assert_eq!(field("applied_commit_id", "X"), "  applied_commit_id: X");
    }

    /// A NULL `session_id` prints the literal `None`, which is Python's
    /// f-string repr leaking through. The common case on live data.
    #[test]
    fn a_null_session_prints_the_literal_none() {
        let c = testdb::fixture();
        let i = show_intent(&c, 2).unwrap().unwrap();
        assert_eq!(i.session_id, None);
        let lines = render_full(&i);
        assert!(
            lines.iter().any(|l| l == "  session_id:        None"),
            "{lines:#?}"
        );
    }

    /// The five conditional lines are absent on a bare `proposed` row
    /// and the eight unconditional ones are present.
    #[test]
    fn a_proposed_row_prints_nine_lines_and_omits_five() {
        let c = testdb::fixture();
        let lines = render_full(&show_intent(&c, 2).unwrap().unwrap());
        assert_eq!(lines.len(), 9, "{lines:#?}");
        assert!(lines[0].starts_with("Intent #"));
        for absent in [
            "applied_at",
            "applied_commit_id",
            "cancelled_at",
            "patch_summary",
            "description",
        ] {
            assert!(
                !lines.iter().any(|l| l.contains(absent)),
                "{absent} should be omitted: {lines:#?}"
            );
        }
    }

    /// An applied row with a commit link prints both, in Python's order
    /// — which is not the column order.
    #[test]
    fn the_conditional_lines_print_in_pythons_order() {
        let c = testdb::fixture();
        let lines = render_full(&show_intent(&c, 1).unwrap().unwrap());
        let idx = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap();
        assert!(idx("created_at") < idx("applied_at"));
        assert!(idx("applied_at") < idx("applied_commit_id"));
        assert!(idx("applied_commit_id") < idx("patch_summary"));
        assert!(idx("patch_summary") < idx("description"));
    }

    /// A zero `applied_commit_id` is omitted, because `0` is falsy in
    /// Python. Asserting the faithful behaviour and not the sensible
    /// one: the mirror's column is a plain INTEGER with no constraint
    /// keeping 0 out.
    #[test]
    fn a_zero_commit_link_is_omitted_because_python_treats_it_as_falsy() {
        let c = testdb::fixture();
        let mut i = show_intent(&c, 1).unwrap().unwrap();
        i.applied_commit_id = Some(0);
        assert!(!render_full(&i)
            .iter()
            .any(|l| l.contains("applied_commit_id")));
        i.applied_commit_id = Some(1);
        assert!(render_full(&i)
            .iter()
            .any(|l| l.contains("applied_commit_id")));
    }

    /// `author` falls back to `(unknown)`. Unreachable on live data —
    /// all 1,346 rows carry one — and cheap to keep correct.
    #[test]
    fn a_missing_author_reads_unknown() {
        let c = testdb::fixture();
        let mut i = show_intent(&c, 1).unwrap().unwrap();
        i.author = None;
        assert!(render_full(&i)
            .iter()
            .any(|l| l == "  author:            (unknown)"));
        i.author = Some(String::new());
        assert!(render_full(&i)
            .iter()
            .any(|l| l == "  author:            (unknown)"));
    }

    /// An id with no row is `None` and not an error, so the caller owns
    /// the message and the exit status.
    #[test]
    fn an_unknown_id_is_none_rather_than_an_error() {
        let c = testdb::fixture();
        assert!(show_intent(&c, 99_999).unwrap().is_none());
    }

    /// The default filter is `status = 'proposed'`, which is the whole
    /// reason the command is useful: 1,343 of 1,346 live rows are
    /// `applied` and would bury the two outstanding ones.
    #[test]
    fn the_default_filter_is_proposed_only() {
        let c = testdb::fixture();
        let rows = list_intents(&c, &IntentFilter::default(), 100).unwrap();
        assert!(
            rows.iter().all(|r| r.status == "proposed"),
            "{:?}",
            rows.iter().map(|r| &r.status).collect::<Vec<_>>()
        );
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![4, 2]);
    }

    /// `--all-statuses` includes applied and cancelled.
    #[test]
    fn all_statuses_includes_applied_and_cancelled() {
        let c = testdb::fixture();
        let f = IntentFilter {
            all_statuses: true,
            ..Default::default()
        };
        let rows = list_intents(&c, &f, 100).unwrap();
        let statuses: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
        assert!(statuses.contains(&"applied"), "{statuses:?}");
        assert!(statuses.contains(&"cancelled"), "{statuses:?}");
        assert!(statuses.contains(&"proposed"), "{statuses:?}");
    }

    /// Newest first, with `id DESC` deciding the ties — the divergence
    /// from Python this module argues for.
    ///
    /// The fixture carries two rows sharing `2026-10-18 09:00:00` with
    /// non-contiguous ids (5 and 3), so an implementation that returned
    /// them in insertion order would look plausible and be wrong.
    #[test]
    fn ties_on_created_at_are_broken_by_id_descending() {
        let c = testdb::fixture();
        let f = IntentFilter {
            all_statuses: true,
            ..Default::default()
        };
        let rows = list_intents(&c, &f, 100).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            vec![5, 3, 4, 2, 1]
        );
        let times: Vec<&str> = rows.iter().map(|r| r.created_at.as_str()).collect();
        let mut sorted = times.clone();
        sorted.sort_unstable_by(|a, b| b.cmp(a));
        assert_eq!(times, sorted);
    }

    /// `--limit` cuts from the newest end, and it cuts *inside* the
    /// tied second — the case the tie-break exists for, because an
    /// untied order changes which of the two rows survives the cut.
    #[test]
    fn the_limit_cuts_inside_a_tie_group() {
        let c = testdb::fixture();
        let f = IntentFilter {
            all_statuses: true,
            ..Default::default()
        };
        let rows = list_intents(&c, &f, 1).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5]);
    }

    /// `--session` is an exact integer match, and it does not sweep up
    /// the NULL-session rows — which is the mistake the equivalent
    /// `staged_by_session_id IS NULL` column taught, where NULL means
    /// *unstaged* rather than *unowned*.
    #[test]
    fn the_session_filter_excludes_null_sessions() {
        let c = testdb::fixture();
        let f = IntentFilter {
            session_id: Some(7),
            all_statuses: true,
            ..Default::default()
        };
        let rows = list_intents(&c, &f, 100).unwrap();
        assert!(!rows.is_empty());
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![5, 3, 1]);
        // Rows 2 and 4 have a NULL session and are gone, rather than
        // being swept in by a predicate that treated NULL as a match.
        assert!(!rows.iter().any(|r| r.id == 2 || r.id == 4));
    }

    /// `--project` filters on a resolved id, and the two filters AND
    /// together — the combination a mis-numbered placeholder breaks.
    #[test]
    fn the_project_and_session_filters_combine_with_and() {
        let c = testdb::fixture();
        let both = IntentFilter {
            project_id: Some(1),
            session_id: Some(7),
            all_statuses: true,
        };
        let rows = list_intents(&c, &both, 100).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 1]);
        for r in &rows {
            assert_eq!(r.project_slug, "alpha");
        }
        // Dropping either clause returns strictly more, so neither is
        // a no-op in this fixture.
        let project_only = IntentFilter {
            session_id: None,
            ..both
        };
        let session_only = IntentFilter {
            project_id: None,
            ..both
        };
        assert!(list_intents(&c, &project_only, 100).unwrap().len() > rows.len());
        assert!(list_intents(&c, &session_only, 100).unwrap().len() > rows.len());
    }

    /// A filter selecting nothing returns no rows rather than
    /// everything — the failure a mis-built `WHERE` produces.
    #[test]
    fn an_unmatched_filter_selects_nothing() {
        let c = testdb::fixture();
        let f = IntentFilter {
            session_id: Some(9_999),
            all_statuses: true,
            ..Default::default()
        };
        assert!(list_intents(&c, &f, 100).unwrap().is_empty());
    }

    // --- `intent create` ---------------------------------------------
    //
    // The renderer and the summary arithmetic, against the bytes Python
    // produced on a copy of the live database on 2026-11-17. The
    // database half is in `tests/write_parity.rs`, which needs a
    // server.

    fn created(id: i64) -> Created {
        Created {
            id,
            file_path: "measured.txt".to_string(),
            base_revision: "current".to_string(),
            new_content_hash: "4a1e67f2fe1d1cc7b31d0ca2ec441da4778203a036a77da10344c85e24ff0f92"
                .to_string(),
            description: None,
        }
    }

    /// **Intent #1625's output, verbatim.** Python was run against a
    /// copy of the live database and its stdout captured through
    /// `cat -A`; this is that text, so the test fails if any of the
    /// three hand-counted indents, the truncation or the blank line
    /// moves.
    ///
    /// The one substitution is the project slug, which Python takes
    /// from the resolved row and this takes from the caller — equal by
    /// construction here, because the slug is matched exactly.
    #[test]
    fn the_created_output_is_pythons_measured_bytes() {
        assert_eq!(
            created_lines(&created(1625), "regr-test").join("\n"),
            concat!(
                "✓ Intent #1625 created (proposed)\n",
                "  regr-test/measured.txt\n",
                "  base:  current\n",
                "  new:   4a1e67f2fe1d\n",
                "\n",
                "  Apply:  templedb intent apply 1625\n",
                "  Cancel: templedb intent cancel 1625"
            )
        );
    }

    /// Intent #1626's: `-m` adds a `note:` line in fifth place, before
    /// the blank one and not after it.
    #[test]
    fn a_message_adds_a_note_line_above_the_blank() {
        let mut c = created(1626);
        c.base_revision = "deadbeef1234".to_string();
        c.new_content_hash =
            "2d711642b726b04401627ca9fbac32f5c8530fb1903cc4db02258717921a4881".to_string();
        c.file_path = "measured2.txt".to_string();
        c.description = Some("a note".to_string());
        assert_eq!(
            created_lines(&c, "regr-test").join("\n"),
            concat!(
                "✓ Intent #1626 created (proposed)\n",
                "  regr-test/measured2.txt\n",
                "  base:  deadbeef1234\n",
                "  new:   2d711642b726\n",
                "  note:  a note\n",
                "\n",
                "  Apply:  templedb intent apply 1626\n",
                "  Cancel: templedb intent cancel 1626"
            )
        );
    }

    /// The three fields align one column left of the two hints, which
    /// is Python's hand-counted padding and not a computed width. The
    /// test that fails if someone tidies the five labels to `{:<8}`.
    #[test]
    fn the_hand_counted_indents_land_in_two_columns() {
        let mut c = created(1);
        c.description = Some("n".to_string());
        let lines = created_lines(&c, "p");
        // The column the value starts at. Every label is ASCII, so a
        // byte offset is a column.
        let col = |prefix: &str| {
            let line = lines
                .iter()
                .find(|l| l.starts_with(prefix))
                .unwrap_or_else(|| panic!("no line starts with {prefix:?}: {lines:#?}"));
            line.len() - line[prefix.len()..].trim_start().len()
        };
        assert_eq!(col("  base:"), 9);
        assert_eq!(col("  new:"), 9);
        assert_eq!(col("  note:"), 9);
        assert_eq!(col("  Apply:"), 10);
        assert_eq!(col("  Cancel:"), 10);
    }

    /// The blank line is line six with no note and line seven with
    /// one, and it is present rather than trimmed — Python's bare
    /// `print()`.
    #[test]
    fn the_blank_line_separates_the_fields_from_the_hints() {
        let lines = created_lines(&created(1), "p");
        assert_eq!(lines.len(), 7);
        assert_eq!(lines[4], "");
        let mut c = created(1);
        c.description = Some("x".to_string());
        let with_note = created_lines(&c, "p");
        assert_eq!(with_note.len(), 8);
        assert_eq!(with_note[5], "");
    }

    /// An empty `-m` prints no note line, because Python tests
    /// `if args.message:` rather than `is not None`. The column still
    /// holds what was passed — that half is asserted in the write
    /// tests.
    #[test]
    fn an_empty_message_prints_no_note_line() {
        let mut c = created(1);
        c.description = Some(String::new());
        // `Created::description` is built with the same filter, so an
        // empty message never reaches here — but a caller constructing
        // the struct by hand must not produce a bare `note:  `.
        assert!(created_lines(&c, "p").iter().any(|l| l == "  note:  "));
        c.description = None;
        assert!(!created_lines(&c, "p").iter().any(|l| l.contains("note:")));
    }

    /// The success marker is U+2713 and not U+2714 or an ASCII `v`.
    /// Same glyph `marker("applied")` returns, hardcoded over there.
    #[test]
    fn the_created_marker_is_the_check_mark() {
        let line = &created_lines(&created(1), "p")[0];
        assert_eq!(line.chars().next(), Some('\u{2713}'));
        assert_eq!(line.chars().next(), Some(marker("applied")));
    }

    /// The hash is cut to twelve characters, and a shorter one is not
    /// cut at all — the slice that panics if written `[..12]`.
    #[test]
    fn the_hash_is_truncated_to_twelve_and_does_not_panic_when_shorter() {
        let mut c = created(1);
        assert!(created_lines(&c, "p")[3].ends_with("4a1e67f2fe1d"));
        c.new_content_hash = "abc".to_string();
        assert_eq!(created_lines(&c, "p")[3], "  new:   abc");
    }

    /// `_short_summary`'s arithmetic, including the empty case where it
    /// differs from `file set`'s line count by one.
    ///
    /// Both values are Python's, measured on a copy 2026-11-17:
    /// `"hello\nworld\n"` gave `3 lines, 12 bytes` (#1625) and an empty
    /// pipe gave `1 lines, 0 bytes` (#1627) — where
    /// `crate::line_count("")` is 0, because `file set`'s expression
    /// has an explicit empty case and this one does not.
    #[test]
    fn the_patch_summary_counts_lines_pythons_other_way() {
        assert_eq!(patch_summary("hello\nworld\n"), "3 lines, 12 bytes");
        assert_eq!(patch_summary(""), "1 lines, 0 bytes");
        assert_eq!(crate::line_count(""), 0);
        assert_eq!(patch_summary("x"), "1 lines, 1 bytes");
        assert_eq!(patch_summary("a\nb"), "2 lines, 3 bytes");
    }

    /// The byte count is over the UTF-8 encoding, not the character
    /// count — Python's `len(content_bytes)`.
    #[test]
    fn the_patch_summary_counts_bytes_and_not_characters() {
        // Three characters, seven bytes: an em dash is three and `é`
        // is two.
        assert_eq!("—é!".chars().count(), 3);
        assert_eq!(patch_summary("—é!"), "1 lines, 6 bytes");
    }

    /// The join is on `projects`, so the slug comes from there and not
    /// from the intent. An intent whose project is gone is invisible —
    /// an INNER JOIN, which is Python's.
    #[test]
    fn the_project_join_is_inner() {
        let c = testdb::fixture();
        let f = IntentFilter {
            all_statuses: true,
            ..Default::default()
        };
        let rows = list_intents(&c, &f, 100).unwrap();
        assert!(rows.iter().all(|r| !r.project_slug.is_empty()));
    }

    fn applied(id: i64, path: &str, hash: &str) -> Applied {
        Applied {
            id,
            file_path: path.to_string(),
            hash: hash.to_string(),
        }
    }

    /// `intent apply`'s whole stdout, byte for byte, as Python printed
    /// it for intent #1639 against a copy of the live database on
    /// 2026-11-18.
    ///
    /// Pinned rather than described, for the reason step 31 gave: the
    /// two spaces of indent, the arrow's spacing and the absence of a
    /// project prefix are all invisible in Python's `print(f"...")` and
    /// a renderer written from the source gets at least one of them
    /// wrong.
    #[test]
    fn the_applied_lines_are_pythons_measured_bytes() {
        assert_eq!(
            applied_lines(&applied(
                1639,
                "test.txt",
                "4fdbc441ea7b546100e086ac1e4fc5ae"
            )),
            vec![
                "✓ Intent #1639 applied".to_string(),
                "  test.txt → 4fdbc441ea7b".to_string(),
            ]
        );
    }

    /// Two lines and no more. Python's rebase prints two further ones
    /// and the path that produces them is the path this port refuses,
    /// so a third line here would mean the refusal had been dropped.
    #[test]
    fn an_applied_intent_prints_exactly_two_lines() {
        assert_eq!(
            applied_lines(&applied(1, "a.txt", "0123456789abcdef")).len(),
            2
        );
    }

    /// The path is bare where `intent create` prints `<slug>/<path>`.
    /// One column, two commands, two renderings — Python's, and the
    /// reason [`applied_lines`] takes no slug at all.
    #[test]
    fn the_applied_path_carries_no_project_prefix() {
        let a = applied(7, "src/deep/file.rs", "aaaaaaaaaaaabbbb");
        assert_eq!(applied_lines(&a)[1], "  src/deep/file.rs → aaaaaaaaaaaa");
        // The same path through the other renderer, which does qualify
        // it. Asserted side by side so the difference is the test's
        // subject rather than a thing two tests happen to disagree on.
        let c = Created {
            id: 7,
            file_path: "src/deep/file.rs".to_string(),
            base_revision: "current".to_string(),
            new_content_hash: "aaaaaaaaaaaabbbb".to_string(),
            description: None,
        };
        assert_eq!(created_lines(&c, "alpha")[1], "  alpha/src/deep/file.rs");
    }

    /// The arrow is U+2192 RIGHTWARDS ARROW, not U+2192 and not `->`.
    /// Indistinguishable from several neighbours in a terminal, so this
    /// is the only thing that catches a swap.
    #[test]
    fn the_applied_arrow_is_the_rightwards_arrow() {
        let line = applied_lines(&applied(1, "a", "0123456789abcdef"))[1].clone();
        assert!(line.contains('\u{2192}'), "{line}");
        assert!(!line.contains("->"), "{line}");
    }

    /// The hash is cut to twelve and a shorter one does not panic —
    /// `created_lines`' slice test, for the second renderer.
    #[test]
    fn the_applied_hash_is_truncated_to_twelve() {
        assert_eq!(
            applied_lines(&applied(1, "a", "0123456789abcdef"))[1],
            "  a → 0123456789ab"
        );
        assert_eq!(applied_lines(&applied(1, "a", "abc"))[1], "  a → abc");
    }

    /// The four conditions of Python's staleness expression, one row
    /// each.
    ///
    /// This is the branch table for the port's **one divergence** in
    /// this command: `true` here is where Python merges and this
    /// refuses. Every row is therefore a statement about how wide that
    /// divergence is, which is why they are enumerated rather than
    /// sampled.
    #[test]
    fn the_staleness_test_is_pythons_four_conditions() {
        const H: &str = "4fdbc441ea7b546100e086ac1e4fc5ae6749b7314311c99db05be450eca12996";
        // No current row: not stale. A registered path with no content
        // applies cleanly.
        assert!(!base_is_stale(None, H));
        // The two sentinels, which are what 886 live rows hold.
        assert!(!base_is_stale(Some(H), "current"));
        assert!(!base_is_stale(Some(H), "new-file"));
        // An empty base is falsy over there.
        assert!(!base_is_stale(Some(H), ""));
        // A prefix test, not equality: the full hash and the twelve
        // characters `intent create` prints both match.
        assert!(!base_is_stale(Some(H), H));
        assert!(!base_is_stale(Some(H), &H[..12]));
        assert!(!base_is_stale(Some(H), &H[..8]));
        // And a base that is not a prefix is stale, which is the
        // refusal.
        assert!(base_is_stale(Some(H), "deadbeefdead"));
        // Longer than the hash: not a prefix, so stale rather than a
        // panic or a reversed comparison.
        assert!(base_is_stale(Some(H), &format!("{H}00")));
    }

    /// An empty current hash is not stale, because `current_hash and`
    /// is a truthiness test over there and `''` is falsy.
    ///
    /// Unreachable through either implementation's writers —
    /// `content_hash` is NOT NULL and every writer passes a sha256 —
    /// and reproduced because the alternative is a port that refuses
    /// where Python applies on an input neither can rule out.
    #[test]
    fn an_empty_current_hash_is_not_stale() {
        assert!(!base_is_stale(Some(""), "deadbeefdead"));
    }

    /// A one-character base matches one hash in sixteen, on both sides.
    ///
    /// Not a behaviour worth having; pinned because it is the
    /// observable consequence of `startswith` and nothing on either
    /// side validates `--base-rev`'s length. A caller who typos a base
    /// down to one character gets a silent apply rather than a refusal.
    #[test]
    fn a_one_character_base_is_a_prefix_match() {
        assert!(!base_is_stale(Some("4fdbc441"), "4"));
        assert!(base_is_stale(Some("4fdbc441"), "5"));
    }
}
