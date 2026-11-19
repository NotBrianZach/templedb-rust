//! Which directory on disk a project's content lives in — the read half
//! of the checkout-role resolver.
//!
//! # Why this exists, and what it repairs
//!
//! `file where` shipped on 2026-11-05 with its two mirror paths built by
//! hand: `~/.config/templedb/checkouts/<slug>/<path>` and
//! `~/.config/templedb/edit-workspaces/<slug>/<path>`. That was a
//! faithful reading of the Python command *as it was then*, and the
//! README argued at length that the second probe could never fire
//! because the real layout has a session directory in between.
//!
//! Both halves are now wrong, and the sweep is what said so. Measured
//! 2026-11-15 against the live database, `file where` differed from
//! Python on **three of its four tree-reading rows**, in two directions
//! at once:
//!
//!   * the `edit-work` line fires on Python and not here, for the 3 of
//!     26 projects that have an adoptable edit tree whose directory
//!     still exists (`templedb`, `templedb-rust`, `system_config`);
//!   * `woofsledger`'s `checkout ` line fires on Python and not here,
//!     because its canonical tree is not at the canonical path;
//!   * `nixos_anywhere_vultr_woofs/.envrc`'s `checkout ` line fires
//!     *here* and not on Python, which is the same mistake in the
//!     opposite direction — this port invented a mirror.
//!
//! Python's own comment says what changed: "The old hardcoded pair got
//! the edit workspace wrong in a way that could never match … so the
//! probe omitted the session component, `.exists()` was always False,
//! and step 4 skips missing mirrors silently." Upstream fixed the bug
//! this port had carefully reproduced, and reproduced *with* a
//! measurement — "none of the 24 project directories under that root
//! holds a file at depth 1" — which was true and said nothing about
//! whether Python still probed that root.
//!
//! That is the third time a reproduced bug has gone stale here and the
//! first time one has gone stale in a direction a measurement of the
//! *data* could not see. The lesson is narrow and worth keeping: a
//! reproduced bug needs a check against the other implementation's
//! behaviour, not against the state its behaviour happens to depend on.
//!
//! # Both roots come from the table, not from a path
//!
//! `checkouts` is the 22nd table in the mirrored subset and it arrives
//! as an ordinary reader's table: this module is the reader. Python
//! resolves both labels through `CheckoutRepository.resolve`, which
//! classifies rows by migration 113's `kind` column —
//!
//! | kind | meaning |
//! |------|---------|
//! | `canonical` | the materialised tree `publish` owns |
//! | `edit` | a writable working tree |
//! | `scratch` | throwaway; never resolved *to*, only ever named |
//!
//! — and picks one per [`Purpose`]. Live shape, measured 2026-11-15:
//! 96 of 250 rows are active, 9 `canonical` / 80 `edit` / 7 `scratch`,
//! and no row has a NULL `kind`.
//!
//! # What is deliberately *not* ported
//!
//! `resolve` has four purposes and this has two. `PURPOSE_WRITE` exists
//! to refuse — it raises rather than handing a writer the published
//! tree — and `PURPOSE_SCAN` exists to decline to adopt a stale tree,
//! because a whole-tree scan records an absent file as a deletion. Both
//! belong to the tree *layer* of the porting order's step 27, which
//! writes, and neither has a reader in this port to verify it against.
//! Porting a refusal nothing calls is how an untested branch gets
//! trusted. [`Purpose`] therefore has two variants and not four, so a
//! future caller has to add the branch rather than discover that a
//! guard it assumed was there is not.
//!
//! Python's three warnings are also not reproduced. They go to stderr,
//! where the README already records that the wording is this port's
//! own, and one of them (`_warn_if_behind`) hashes up to 200 files to
//! decide whether to print — a measurement that would be this module's
//! only reason to touch a filesystem.

use crate::session::SessionId;
use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};

/// One `checkouts` row, at the columns resolution reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkout {
    pub id: i64,
    /// Absolute path to the tree. Not validated here — whether it still
    /// exists is the `is_dir` predicate's question, and the answer
    /// changes between two runs of the same command.
    pub path: String,
    /// migration 113's classification. `Option` because the column is
    /// nullable, even though no live row is NULL: Python compares it
    /// with `==`, so a NULL simply matches no bucket and falls through
    /// to the last-resort branch. Coalescing it to `scratch` here would
    /// read the same today and differ the moment a row arrived without
    /// one.
    pub kind: Option<String>,
    /// Which VCS session owns the tree, if any. 69 of the 96 active
    /// rows carry one.
    pub session_id: Option<i64>,
}

/// Which tree a caller is asking for.
///
/// Two of Python's four, and the two it is safe for a reader to have —
/// see the module note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// `publish`'s materialised tree. Python's `PURPOSE_BUILD`.
    Build,
    /// A writable working tree, preferring this session's own. Python's
    /// `PURPOSE_EDIT`, and its default.
    Edit,
}

/// Active checkout rows for one project, newest first.
///
/// # The tie-break Python does not have
///
/// Python orders by `checkout_at DESC` alone and then takes `[0]` from
/// whichever bucket it filtered into, so a tie decides which *directory
/// a command reads* — not merely the order of a listing. This adds
/// `id DESC`.
///
/// Measured 2026-11-15: **zero** of the 96 active rows share a
/// `(project_id, checkout_at)` with another, so the tie-break selects
/// nothing today and costs no parity. It is here anyway, and the reason
/// is the one the README records under `intent list`: "stay inside the
/// deterministic window" is not a strategy, because the window is a
/// property of live data. `checkout_at` has second granularity and
/// `templedb edit` writes a row per workspace, so two agents
/// provisioning in the same second is an ordinary event rather than an
/// exotic one — and when it happens Python picks arbitrarily between
/// two trees that may hold different bytes.
pub fn active(store: &dyn Store, project_id: i64) -> Result<Vec<Checkout>> {
    let rows = store
        .query(
            "SELECT id, checkout_path, kind, session_id
               FROM checkouts
              WHERE project_id = ?1 AND is_active = 1
              ORDER BY checkout_at DESC, id DESC",
            &[Value::from(project_id)],
        )
        .context("listing a project's checkouts")?;

    rows.iter()
        .map(|r| {
            Ok(Checkout {
                id: r.get_i64(0)?,
                path: r.get_string(1)?,
                kind: r.get_opt_string(2)?,
                session_id: r.get_opt_i64(3)?,
            })
        })
        .collect()
}

/// Which of `rows` a caller asking for `purpose` should use.
///
/// Pure, and that is the point: every input that is not a database row
/// is a parameter, so the branch table below is testable without a
/// filesystem, without an environment and without a session. Python's
/// version reads `os.path.isdir`, `os.environ` and two more tables from
/// inside the decision.
///
/// * `is_dir` — `os.path.isdir(checkout_path)`. Python calls this
///   `extant` and applies it to every bucket *except* the very last
///   fallback, which is `(extant(rows) or rows)[0]` — if no row's
///   directory exists it returns the newest row anyway, naming a
///   directory that is not there. Reproduced: the caller skips a mirror
///   that does not exist, so the two agree on an absent line.
/// * `session` — the session this process is in, or `None`. Python
///   calls `current_session_id()` here; see [`current_session`].
/// * `is_live` — whether a *row's* `session_id` still names an
///   unfinished session. Separate from `session` because an owned tree
///   and an abandoned tree are different answers: the first is
///   returned, the second is adoptable.
pub fn resolve<'a>(
    rows: &'a [Checkout],
    purpose: Purpose,
    session: Option<SessionId>,
    is_dir: &dyn Fn(&str) -> bool,
    is_live: &dyn Fn(i64) -> bool,
) -> Option<&'a Checkout> {
    if rows.is_empty() {
        // Python returns None here for every purpose but WRITE, which
        // refuses. 14 of the 26 live projects are in this state.
        return None;
    }
    let extant = |c: &&Checkout| is_dir(&c.path);
    let of_kind = |k: &'static str| move |c: &&Checkout| c.kind.as_deref() == Some(k);

    let canonical = || rows.iter().filter(of_kind("canonical")).find(extant);
    // Python's `(extant(rows) or rows)[0]`: the newest row whose
    // directory exists, and the newest row of any kind if none does.
    // This is the one branch a `scratch` row can win, which is why
    // `scratch` is not filtered out of `rows` on the way in.
    let newest = || rows.iter().find(extant).or_else(|| rows.first());

    match purpose {
        Purpose::Build => canonical().or_else(newest),
        Purpose::Edit => {
            let edits: Vec<&Checkout> =
                rows.iter().filter(of_kind("edit")).filter(extant).collect();

            // 1. A tree this session owns. Returned even if it is far
            //    behind the database: `templedb edit` reuses a
            //    workspace without re-materialising it, so an owned
            //    tree can be arbitrarily stale, and Python warns here
            //    rather than declining.
            if let Some(sid) = session {
                if let Some(mine) = edits.iter().find(|c| c.session_id == Some(sid.get())) {
                    return Some(mine);
                }
            }

            // 2. A tree nobody live is using, adopted. Python warns
            //    when there is more than one and takes the newest
            //    either way, so the two arms differ only in the
            //    warning — which is stderr and is not reproduced.
            if let Some(free) = edits
                .iter()
                .find(|c| c.session_id.is_none_or(|s| !is_live(s)))
            {
                return Some(free);
            }

            // 3. Every edit tree belongs to another live session, or
            //    there are none. Both fall back to the canonical tree,
            //    which is correct for a project nobody is editing and
            //    is the data-loss path `PURPOSE_WRITE` exists to
            //    refuse — a distinction that costs this module nothing
            //    because it has no writer.
            canonical().or_else(newest)
        }
    }
}

/// The directory `purpose` resolves to, reproducing
/// `SyncManager.get_checkout_path`.
///
/// `None` is Python's `raise ValueError`, because that is what its one
/// caller here does with it: `file where` catches the exception and
/// `continue`s, so a project with no resolvable tree has an absent
/// mirror rather than an error.
///
/// # The `repo_url` fallback, and the guard inside it
///
/// A project with no active checkout row falls back to `repo_url`,
/// "for projects whose working directory IS the repo". 14 of the 26
/// live projects take this path. The guard is that `repo_url` is not
/// always a filesystem path — `neko-lan-host` carries
/// `templedb://neko-lan-host`, and 13 of the 26 projects have a `://`
/// in theirs — so a URL is refused rather than turned into a directory
/// that cannot exist. Reproduced including the test, which is
/// `'://' not in str(repo_url)` and therefore also rejects a path with
/// a `://` anywhere in it rather than only a scheme prefix.
pub fn checkout_root(
    rows: &[Checkout],
    repo_url: Option<&str>,
    purpose: Purpose,
    session: Option<SessionId>,
    is_dir: &dyn Fn(&str) -> bool,
    is_live: &dyn Fn(i64) -> bool,
) -> Option<String> {
    if let Some(c) = resolve(rows, purpose, session, is_dir, is_live) {
        return Some(c.path.clone());
    }
    match repo_url {
        Some(u) if !u.contains("://") => Some(u.to_string()),
        _ => None,
    }
}

/// `projects.repo_url` by id, for [`checkout_root`]'s fallback.
///
/// A query of its own rather than a field on [`Project`](crate::Project)
/// because `SyncManager` reads it the same way — `SELECT id, slug,
/// repo_url FROM projects WHERE slug = ?` — and because the fuzzy
/// matcher that produced the id does not select it.
pub fn repo_url(store: &dyn Store, project_id: i64) -> Result<Option<String>> {
    let row = store
        .query_opt(
            "SELECT repo_url FROM projects WHERE id = ?1",
            &[Value::from(project_id)],
        )
        .context("reading a project's repo_url")?;
    row.map(|r| r.get_opt_string(0))
        .transpose()
        .map(Option::flatten)
}

/// Which sessions among `ids` are still live, in one query.
///
/// Python asks per row (`_session_is_live`), which is a query inside a
/// loop; the answer is a set membership either way and the candidate
/// list is at most a few dozen rows.
///
/// A session that does not exist at all is not live, which is the same
/// answer Python's `bool(row) and row['ended_at'] is None` gives — and
/// it matters, because `vcs session prune` deletes rows whose
/// `checkouts.session_id` still names them.
pub fn live_sessions(store: &dyn Store, ids: &[i64]) -> Result<Vec<i64>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // Built rather than bound as an array: the two dialects spell an
    // `IN` over a parameter list differently and the list is this
    // port's own integers, never user text. `i64` has no SQL-injectable
    // spelling, which is the property that makes the interpolation
    // safe rather than merely convenient.
    let list = ids
        .iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(",");
    let rows = store
        .query(
            &format!(
                "SELECT id FROM vcs_sessions
                  WHERE id IN ({list}) AND ended_at IS NULL"
            ),
            &[],
        )
        .context("checking which sessions are live")?;
    rows.iter().map(|r| r.get_i64(0)).collect()
}

/// The session this process is in, for resolution only — Python's
/// `CheckoutRepository.current_session_id`.
///
/// # Why this reads `TEMPLEDB_SESSION` where [`crate::session`] refuses
///
/// `session.rs` declines the name variable on the grounds that
/// resolving it *creates*: `VcsService.get_current_session` inserts a
/// row for a name it does not find, and a port that creates session
/// rows has given up the read-only guarantee the architecture rests on.
///
/// This is a different Python function with an overlapping job and no
/// such side effect. Its docstring says so outright — "Deliberately NOT
/// `VCSService.get_current_session`, which creates a session when none
/// exists. Resolving a path must not have the side effect of opening a
/// session" — and its body is two `SELECT`s. So the name is resolvable
/// here, read-only, and declining it would make this port probe a
/// different directory from Python for any caller using the name
/// variable, which the agent wrapper this is usually run under does
/// export.
///
/// The two are not in tension: `session.rs` refuses a *write*, and this
/// accepts a *read*. Stated because the two functions are one grep
/// apart and the next reader will find the other one first.
pub fn current_session(
    store: &dyn Store,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Option<SessionId>> {
    if let Some(raw) = env(crate::session::SESSION_ID_ENV) {
        // Python wraps `int(sid)` in the same `try` as the query and
        // falls through to the name on a `ValueError`, so a malformed
        // id is not an error here — unlike `session::resolve`, where
        // the caller named it explicitly and deserves to be told.
        if let Ok(id) = raw.trim().parse::<i64>() {
            let row = store
                .query_opt(
                    "SELECT id FROM vcs_sessions WHERE id = ?1 AND ended_at IS NULL",
                    &[Value::from(id)],
                )
                .context("validating TEMPLEDB_SESSION_ID for checkout resolution")?;
            if let Some(r) = row {
                return Ok(Some(SessionId::from_live_row(r.get_i64(0)?)));
            }
        }
    }
    if let Some(name) = env(crate::session::SESSION_NAME_ENV) {
        // `ORDER BY started_at DESC LIMIT 1` with no tie-break, and
        // this one is left alone: `name` is unique in practice because
        // `vcs session start --name` is how rows get one, and adding a
        // tie-break would be a divergence with no measurement behind
        // it. The id branch above is what the sweep pins, so this is
        // the branch no byte diff reaches.
        let row = store
            .query_opt(
                "SELECT id FROM vcs_sessions
                  WHERE name = ?1 AND ended_at IS NULL
                  ORDER BY started_at DESC LIMIT 1",
                &[Value::from(name)],
            )
            .context("resolving TEMPLEDB_SESSION for checkout resolution")?;
        if let Some(r) = row {
            return Ok(Some(SessionId::from_live_row(r.get_i64(0)?)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    fn c(id: i64, path: &str, kind: &str, session: Option<i64>) -> Checkout {
        Checkout {
            id,
            path: path.to_string(),
            kind: Some(kind.to_string()),
            session_id: session,
        }
    }

    /// Every directory exists and no session is live: the common
    /// fixture for the branch table.
    fn all_dirs(_: &str) -> bool {
        true
    }
    fn none_live(_: i64) -> bool {
        false
    }

    #[test]
    fn build_prefers_the_canonical_tree_over_a_newer_edit_one() {
        // Newest first, which is the order `active` returns.
        let rows = vec![
            c(3, "/edit/new", "edit", None),
            c(2, "/canon", "canonical", None),
            c(1, "/scratch", "scratch", None),
        ];
        let got = resolve(&rows, Purpose::Build, None, &all_dirs, &none_live).unwrap();
        assert_eq!(got.path, "/canon");
    }

    /// No canonical row at all — a project that has only ever been
    /// edited. Python falls through rather than failing, because
    /// publishing is how a canonical tree comes to exist.
    #[test]
    fn build_falls_back_to_the_newest_extant_row() {
        let rows = vec![
            c(2, "/edit/new", "edit", None),
            c(1, "/edit/old", "edit", None),
        ];
        let got = resolve(&rows, Purpose::Build, None, &all_dirs, &none_live).unwrap();
        assert_eq!(got.path, "/edit/new");
    }

    /// The last-resort branch returns a row whose directory does not
    /// exist, which is Python's `(extant(rows) or rows)[0]`. The caller
    /// then finds nothing there and skips the mirror, so reproducing
    /// the pointless answer is what makes the two agree.
    #[test]
    fn build_returns_a_missing_directory_rather_than_nothing() {
        let rows = vec![c(1, "/gone", "canonical", None)];
        let got = resolve(&rows, Purpose::Build, None, &|_| false, &none_live).unwrap();
        assert_eq!(got.path, "/gone");
    }

    /// A `scratch` row is never resolved *to* by name — but it does win
    /// the last-resort branch, which is the one place its path can be
    /// handed out.
    #[test]
    fn scratch_wins_only_the_last_resort_branch() {
        let rows = vec![c(1, "/scratch", "scratch", None)];
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/scratch"
        );
        // With a canonical tree present, the scratch row loses.
        let rows = vec![
            c(2, "/scratch", "scratch", None),
            c(1, "/canon", "canonical", None),
        ];
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/canon"
        );
    }

    /// Step 1: this session's own tree outranks a newer unowned one.
    #[test]
    fn edit_prefers_the_tree_this_session_owns() {
        let rows = vec![
            c(3, "/edit/someone-else", "edit", None),
            c(2, "/edit/mine", "edit", Some(77)),
            c(1, "/canon", "canonical", None),
        ];
        let mine = SessionId::in_test(77);
        assert_eq!(
            resolve(&rows, Purpose::Edit, Some(mine), &all_dirs, &none_live)
                .unwrap()
                .path,
            "/edit/mine"
        );
        // With no session the same rows adopt the newest unowned tree.
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/edit/someone-else"
        );
    }

    /// Step 2: a tree whose session has ended is adoptable; one whose
    /// session is live is not, and resolution falls past it to the
    /// canonical tree.
    #[test]
    fn edit_adopts_an_abandoned_tree_and_not_a_live_one() {
        let rows = vec![
            c(2, "/edit/busy", "edit", Some(5)),
            c(1, "/canon", "canonical", None),
        ];
        // 5 is live: not adoptable, so the canonical tree answers.
        let live5 = |s: i64| s == 5;
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &live5)
                .unwrap()
                .path,
            "/canon"
        );
        // 5 has ended: adopted.
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/edit/busy"
        );
    }

    /// An edit tree whose directory is gone is not a candidate at all.
    /// This is the half that was failing in the live comparison: 26 of
    /// `bza`'s 80 edit rows are active and none of their directories
    /// survives, so Python prints no `edit-work` line for it.
    #[test]
    fn edit_ignores_a_row_whose_directory_is_gone() {
        let rows = vec![
            c(2, "/edit/pruned", "edit", None),
            c(1, "/canon", "canonical", None),
        ];
        let only_canon = |p: &str| p == "/canon";
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &only_canon, &none_live)
                .unwrap()
                .path,
            "/canon"
        );
    }

    /// A NULL `kind` matches no bucket. No live row has one; the column
    /// is nullable, so both implementations are reachable in principle
    /// and they agree — it falls to the last-resort branch.
    #[test]
    fn a_null_kind_matches_no_bucket() {
        let rows = vec![Checkout {
            id: 1,
            path: "/unclassified".to_string(),
            kind: None,
            session_id: None,
        }];
        assert_eq!(
            resolve(&rows, Purpose::Edit, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/unclassified"
        );
        assert_eq!(
            resolve(&rows, Purpose::Build, None, &all_dirs, &none_live)
                .unwrap()
                .path,
            "/unclassified"
        );
    }

    #[test]
    fn no_rows_resolves_to_nothing() {
        assert!(resolve(&[], Purpose::Edit, None, &all_dirs, &none_live).is_none());
        assert!(resolve(&[], Purpose::Build, None, &all_dirs, &none_live).is_none());
    }

    /// The `repo_url` fallback and the `://` guard, which is the whole
    /// of `get_checkout_path` beyond resolution.
    #[test]
    fn repo_url_answers_only_when_it_is_a_path() {
        let probe =
            |u: Option<&str>| checkout_root(&[], u, Purpose::Edit, None, &all_dirs, &none_live);
        assert_eq!(
            probe(Some("/home/zach/src/thing")).as_deref(),
            Some("/home/zach/src/thing")
        );
        // `templedb://neko-lan-host` is the live row this guard exists
        // for, and `https://` is the ordinary case.
        assert_eq!(probe(Some("templedb://neko-lan-host")), None);
        assert_eq!(probe(Some("https://github.com/x/y")), None);
        assert_eq!(probe(None), None);
        // A resolved row outranks the fallback.
        let rows = vec![c(1, "/canon", "canonical", None)];
        assert_eq!(
            checkout_root(
                &rows,
                Some("/repo"),
                Purpose::Edit,
                None,
                &all_dirs,
                &none_live
            )
            .as_deref(),
            Some("/canon")
        );
    }

    #[test]
    fn active_rows_come_back_newest_first_and_skip_inactive_ones() {
        let c = testdb::fixture();
        let rows = super::active(&c, 1).unwrap();
        // alpha has three active rows and one with is_active = 0.
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            vec![
                "/home/t/.config/templedb/edit-workspaces/alpha/owned",
                "/home/t/.config/templedb/edit-workspaces/alpha/free",
                "/home/t/.config/templedb/checkouts/alpha",
            ]
        );
        assert_eq!(rows[0].session_id, Some(7));
        assert_eq!(rows[1].session_id, Some(8));
        assert_eq!(rows[2].kind.as_deref(), Some("canonical"));
        // beta has none, which is the 14-of-27 case.
        assert!(super::active(&c, 2).unwrap().is_empty());
    }

    /// Session 7 is live in the fixture and session 8 has ended, so the
    /// two adoption branches are both reachable from the real query.
    #[test]
    fn live_sessions_reports_only_unfinished_ones() {
        let c = testdb::fixture();
        assert_eq!(super::live_sessions(&c, &[7, 8]).unwrap(), vec![7]);
        // A session id with no row is not live. `vcs session prune`
        // deletes rows that `checkouts.session_id` still names.
        assert!(super::live_sessions(&c, &[9999]).unwrap().is_empty());
        assert!(super::live_sessions(&c, &[]).unwrap().is_empty());
    }

    /// Resolution against the fixture's own rows, which is the one test
    /// here that exercises the query and the branch table together.
    #[test]
    fn the_fixtures_rows_resolve_the_way_the_branch_table_says() {
        let c = testdb::fixture();
        let rows = super::active(&c, 1).unwrap();
        let live = super::live_sessions(&c, &[7, 8]).unwrap();
        let is_live = |s: i64| live.contains(&s);
        let path = |purpose, session| {
            resolve(&rows, purpose, session, &all_dirs, &is_live)
                .unwrap()
                .path
                .clone()
        };
        // Session 7 owns `owned`; nobody else sees it, and they adopt
        // `free` because session 8 has ended.
        assert!(path(Purpose::Edit, Some(SessionId::in_test(7))).ends_with("/owned"));
        assert!(path(Purpose::Edit, Some(SessionId::in_test(9))).ends_with("/free"));
        assert!(path(Purpose::Edit, None).ends_with("/free"));
        // BUILD takes the canonical tree regardless of the session.
        assert!(path(Purpose::Build, Some(SessionId::in_test(7))).ends_with("/checkouts/alpha"));
    }

    #[test]
    fn repo_url_is_read_by_id() {
        let c = testdb::fixture();
        assert_eq!(
            super::repo_url(&c, 1).unwrap().as_deref(),
            Some("git@example:alpha.git")
        );
        // beta's is NULL, and project 9999 has no row at all. Both are
        // `None`, and the caller treats them the same way Python's
        // `if self.repo_url:` does.
        assert_eq!(super::repo_url(&c, 2).unwrap(), None);
        assert_eq!(super::repo_url(&c, 9999).unwrap(), None);
    }

    /// Both env branches, and the precedence between them.
    #[test]
    fn current_session_reads_the_id_then_the_name() {
        let c = testdb::fixture();
        let env = |pairs: Vec<(&'static str, &'static str)>| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let id = |e: &dyn Fn(&str) -> Option<String>| {
            super::current_session(&c, e).unwrap().map(|s| s.get())
        };

        assert_eq!(id(&env(vec![("TEMPLEDB_SESSION_ID", "7")])), Some(7));
        // Ended: not accepted, and there is no name to fall back to.
        assert_eq!(id(&env(vec![("TEMPLEDB_SESSION_ID", "8")])), None);
        // Malformed: Python's `int()` raises inside the same `try` as
        // the query, so this falls through to the name rather than
        // erroring — which is the opposite of `session::resolve`, where
        // the caller named it and is told.
        assert_eq!(
            id(&env(vec![
                ("TEMPLEDB_SESSION_ID", "not-a-number"),
                ("TEMPLEDB_SESSION", "alpha-work")
            ])),
            Some(10)
        );
        // The name alone, and a name whose session has ended.
        assert_eq!(id(&env(vec![("TEMPLEDB_SESSION", "alpha-work")])), Some(10));
        assert_eq!(id(&env(vec![("TEMPLEDB_SESSION", "finished")])), None);
        assert_eq!(id(&env(vec![("TEMPLEDB_SESSION", "no-such-name")])), None);
        // The id wins when both are set and both resolve.
        assert_eq!(
            id(&env(vec![
                ("TEMPLEDB_SESSION_ID", "7"),
                ("TEMPLEDB_SESSION", "alpha-work")
            ])),
            Some(7)
        );
        // And an id that resolves to nothing falls through to a name
        // that does, which is Python's order rather than a refusal.
        assert_eq!(
            id(&env(vec![
                ("TEMPLEDB_SESSION_ID", "8"),
                ("TEMPLEDB_SESSION", "alpha-work")
            ])),
            Some(10)
        );
        assert_eq!(id(&env(vec![])), None);
    }
}
