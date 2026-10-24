//! Which VCS session a command is running in — when the caller says so,
//! and never otherwise.
//!
//! # Resolve never, accept always
//!
//! Python's `VcsService.get_current_session()` resolves through seven
//! steps and **two of them insert a row**:
//!
//! | # | source | behaviour |
//! |---|---|---|
//! | 0 | `--session <id\|name>` | explicit pin; outranks both env vars |
//! | 1 | `TEMPLEDB_SESSION_ID` | must name a row with `ended_at IS NULL` |
//! | 2 | instance cache | per-process memoisation |
//! | 3 | `TEMPLEDB_SESSION` | a declared *name*; **creates if absent** |
//! | 4 | `os.getsid(0)` | `(author, host, sid)` started within 24 hours |
//! | 5 | `os.getppid()` | the same lookup on the parent pid |
//! | 6 | — | **creates** `<host>-<sid>-<short-ts>` |
//!
//! This module is steps 0 and 1, and only the non-writing halves of
//! them. It reads `TEMPLEDB_SESSION_ID`, it accepts an explicit id, it
//! validates liveness, and it does nothing else. With no session
//! identified it returns `None` and every session-scoped behaviour falls
//! back to the published view and says so.
//!
//! The argument is that the four automatic steps are all heuristics for
//! "same human, same terminal, recently", and each one was added because
//! the previous one fanned out — step 4 keys on the session leader
//! rather than the parent because `x=$(templedb ...)` forks a subshell
//! with a fresh pid, and step 3 exists because agent wrappers spawn
//! every tool call under its own session leader anyway. A port that
//! reimplements the heuristics inherits a tuning problem with no way to
//! validate it: there is no byte diff for "did these two invocations
//! agree about which session they were in".
//!
//! # Why not resolve read-only — steps 4 and 5 without step 6
//!
//! This is the obvious middle option and it is rejected explicitly,
//! because it looks free. Doing the two pid lookups without the creating
//! fallback finds a session when one happens to exist and finds none
//! when it does not — so the same command, in the same terminal, answers
//! differently depending on whether the Python implementation has run
//! there in the last 24 hours. The behaviour would be correct,
//! unpredictable, and impossible to test. Refusing to resolve at all is
//! *worse* in capability and *better* in every property this port holds.
//!
//! # What it costs, stated plainly
//!
//! **`--session <name>` closes permanently**, not until later. The name
//! path creates when absent, and a port that creates session rows has
//! given up the read-only guarantee that the whole architecture rests
//! on. Accepting a name that happens to resolve and refusing one that
//! does not would be worse again: a flag that works or writes depending
//! on data. If name support turns out to be load-bearing for how agents
//! actually use the tool, that is an argument for revisiting the
//! decision, not for quietly adding the write.
//!
//! # The shape, which is the clock's
//!
//! One function, called at the CLI entry point, returning an
//! `Option<SessionId>` that is threaded in as an argument. Identical to
//! [`Clock`](crate::Clock): the instant and the identity are resolved
//! once at the edge and passed down, never read ambiently from a query
//! module. The write-path design document asks for exactly that shape
//! for a different reason — session *creation* inside a transaction
//! committed the caller's transaction out from under it, so a later
//! per-file failure left a commit row with no file states. This port
//! cannot hit that bug because it never creates, but resolving before
//! the transaction opens is the same rule and costs nothing.

use crate::store::Store;
use crate::value::Value;
use anyhow::{bail, Context, Result};
use std::fmt;

/// The variable naming a session by id. Explicit, validated, read-only —
/// and so the one this port honours.
pub const SESSION_ID_ENV: &str = "TEMPLEDB_SESSION_ID";

/// The variable naming a session by *name*, which this port does not
/// resolve because resolving it creates. See [`ignored_name`].
pub const SESSION_NAME_ENV: &str = "TEMPLEDB_SESSION";

/// A `vcs_sessions.id` that was live when it was resolved.
///
/// A newtype rather than a bare `i64` because the whole point of this
/// module is that the number came from somewhere specific: an
/// `Option<SessionId>` in a signature says "the caller may have named a
/// live session", where `Option<i64>` says nothing at all and invites a
/// call site to pass a pid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionId(i64);

impl SessionId {
    /// The id, for the one thing that needs it: a query parameter.
    pub fn get(self) -> i64 {
        self.0
    }

    /// A `SessionId` without a lookup behind it, for this crate's tests.
    ///
    /// There is deliberately no public constructor. Outside this crate
    /// the only way to hold one is to have called [`resolve`], so the
    /// type carries a fact — *this id named a live row* — rather than
    /// merely a number. `vcs log`'s session view and the pending count
    /// both take one, and neither should be reachable with a pid or a
    /// `vcs_commits.session_id` read back out of a row.
    #[cfg(test)]
    pub(crate) const fn in_test(id: i64) -> Self {
        SessionId(id)
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Where an explicit session came from, so the refusals can name it.
///
/// Python prints the same two strings for the same reason — a message
/// that says `TEMPLEDB_SESSION_ID=1163 references no live session` tells
/// the caller which of the two things to go and change, and one that
/// says only "no live session" does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Origin {
    Flag,
    Env,
}

impl Origin {
    fn label(self) -> &'static str {
        match self {
            Origin::Flag => "--session",
            Origin::Env => SESSION_ID_ENV,
        }
    }
}

/// What the caller asked for, before any database is consulted.
///
/// Split out from [`resolve`] so the precedence and the parsing can be
/// tested without a store — which is the same reason the placeholder
/// rewriting and the `Value` getters are separable. The lookup is one
/// query; everything interesting about this module is in here.
fn requested(pin: Option<&str>, env: Option<&str>) -> Result<Option<(Origin, i64)>> {
    // An explicit `--session` outranks the environment, and when it is
    // set the environment is not consulted at all — Python's precedence
    // exactly, and for Python's stated reason: a stray
    // `TEMPLEDB_SESSION_ID` in a wrapper must not shadow what the
    // caller typed.
    //
    // Both are trimmed, and an empty value is treated as unset. Python
    // calls `.strip()` on the env vars and tests the result for truth,
    // so `TEMPLEDB_SESSION_ID=` and `TEMPLEDB_SESSION_ID='  '` both
    // fall through there; the same reading `conn.rs` gives an empty
    // `TEMPLEDB_PATH`.
    let (origin, raw) = match pin.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => (Origin::Flag, p),
        None => match env.map(str::trim).filter(|s| !s.is_empty()) {
            Some(v) => (Origin::Env, v),
            None => return Ok(None),
        },
    };

    // The two origins parse differently, and that is Python's doing.
    //
    // The flag is classified with `override.isdigit()`: digits mean an
    // id and **anything else means a name**, which is the creating path.
    // So `--session my-agent` is not a malformed id over there, it is a
    // different feature — and the one feature this port declines. It
    // has to be refused by name rather than handed to a parser, or the
    // message would read as "that is not a number" when the real answer
    // is "names are not supported here".
    //
    // The variable is parsed with `int()`, which accepts a sign. So
    // `TEMPLEDB_SESSION_ID=-5` is a well-formed id over there that
    // matches no row, where `--session -5` is a *name*. Reproduced
    // rather than tidied, because a script that sets the variable gets
    // the same answer from both implementations.
    if origin == Origin::Flag && !raw.bytes().all(|b| b.is_ascii_digit()) {
        bail!(
            "--session takes a session id, not a name: {raw:?}\n\
             Resolving a name is a write in the Python implementation — it \
             creates the session when none matches — so this port does not \
             accept one. Pass an id from `templedb vcs session list --active`."
        );
    }

    // Python's `.isdigit()` is true for non-ASCII digits too, so
    // `--session ١٢` reaches its `int()` and resolves to 12. Not
    // reproduced: the refusal above catches it and says names are not
    // accepted, which is the wrong reason for the right answer on an
    // input no caller has ever typed. Likewise `int("1_0")` is 10 in
    // Python and an error here.
    //
    // Single quotes rather than `{raw:?}`, so this message is
    // byte-identical to Python's on stderr. Python formats with `!r`,
    // and `repr` of a `str` single-quotes it — switching to double
    // quotes only when the value itself contains a single quote, which
    // is not reproduced. The README's position is that error messages
    // are the port's own and are not diffed; this one is free, reachable
    // by a script that exports a typo, and worth having identical.
    let id = raw
        .parse::<i64>()
        .map_err(|_| anyhow::anyhow!("{} must be an integer, got '{raw}'", origin.label()))?;

    Ok(Some((origin, id)))
}

/// The session the caller named, if they named one.
///
/// `Ok(None)` is the ordinary case and means "no session identified" —
/// not an error, and not a reason to guess. `Err` means the caller named
/// one that cannot be used, which is a different thing and must not be
/// silently downgraded to `None`: a pinned session that has ended is a
/// stale wrapper or a script holding a dead id, and answering from the
/// published view instead would hide it.
///
/// Exit status for both refusals is [`USAGE`](crate::USAGE), which is
/// Python's: its `ValidationError` and `ResourceNotFoundError` both
/// leave the CLI decorator as `return 1`. Verified at the command line —
/// a bogus `TEMPLEDB_SESSION_ID` makes `templedb vcs log` exit 1 with
/// empty stdout.
pub fn resolve(store: &dyn Store, pin: Option<&str>) -> Result<Option<SessionId>> {
    let env = std::env::var(SESSION_ID_ENV).ok();
    let Some((origin, id)) = requested(pin, env.as_deref())? else {
        return Ok(None);
    };
    lookup(store, origin, id)
}

/// The liveness check, which is the whole database half of this module.
///
/// `ended_at IS NULL` is the point, and it is load-bearing rather than
/// decorative. Python's lookup accepted an ended session until
/// 2026-10-07, contradicting both its own error message and
/// `CheckoutRepository.current_session_id()`, which had always required
/// liveness. The split let staging adopt a dead session that
/// checkout-role resolution then refused — one of the ways "ending a
/// session strands the next publish" showed up.
fn lookup(store: &dyn Store, origin: Origin, id: i64) -> Result<Option<SessionId>> {
    let rows = store
        .query(
            "SELECT id FROM vcs_sessions WHERE id = ?1 AND ended_at IS NULL",
            &[Value::from(id)],
        )
        .context("looking up the pinned session")?;

    match rows.first() {
        Some(row) => Ok(Some(SessionId(row.get_i64(0)?))),
        // Python's message, and then this port's own second line. The
        // Python one carries the same guidance as a `solution=` keyword
        // its CLI never prints, which is a hint written and then thrown
        // away.
        None => bail!(
            "{}={id} references no live session\n\
             Pick a live id from `templedb vcs session list --active`, or \
             unset it to read the published view.",
            origin.label()
        ),
    }
}

/// [`resolve`], against a write transaction rather than a [`Store`].
///
/// The writer owns a `postgres::Client` outside the `Store` trait —
/// `mirror`'s precedent, and the reason `Store` can still promise that a
/// `&dyn Store` cannot write — so a command that writes cannot reach the
/// resolver above. Two entry points over one shared [`requested`] rather
/// than two resolvers: the precedence and the parsing are the part worth
/// having in one place, and the lookup is a single statement either way.
///
/// Resolving against the write target is the point, not an artefact.
/// `file set` writes the scratch database and the readers point at the
/// parity one, so a session validated against the reader's database and
/// then stamped into the writer's would be a liveness claim about the
/// wrong server — the same bug the exact-slug resolution exists to
/// avoid, one column over.
pub fn resolve_tx(
    tx: &mut postgres::Transaction<'_>,
    pin: Option<&str>,
) -> Result<Option<SessionId>> {
    let env = std::env::var(SESSION_ID_ENV).ok();
    let Some((origin, id)) = requested(pin, env.as_deref())? else {
        return Ok(None);
    };
    let row = tx
        .query_opt(
            "SELECT id FROM vcs_sessions WHERE id = $1 AND ended_at IS NULL",
            &[&id],
        )
        .context("looking up the pinned session in the write target")?;
    match row {
        Some(row) => Ok(Some(SessionId(row.get(0)))),
        None => bail!(
            "{}={id} references no live session in the write target\n\
             Pick a live id from `templedb vcs session list --active`, or \
             unset it to record the intent with no session.",
            origin.label()
        ),
    }
}

/// `TEMPLEDB_SESSION`, when it is set and would have decided the answer.
///
/// This is the variable an agent wrapper exports, so a port running
/// under an agent will routinely see it — and will ignore it, because
/// the name path creates. Ignoring a variable that changes the answer is
/// the failure mode worth being loud about, so the CLI prints a warning.
///
/// A warning rather than a refusal, which was the open question. Erroring
/// would be loud and correct and would make this port unusable in
/// exactly the environment it is developed in; a warning says the same
/// thing without that. The cost is that it is one line on stderr, which
/// is easy to miss — accepted, because the alternative was measured
/// against the one caller that exists.
///
/// Returns `None` when a session *was* explicitly named, because then
/// Python ignores the variable too: steps 0 and 1 both outrank step 3,
/// so the two implementations agree and there is nothing to warn about.
/// Warning anyway would train the reader to ignore the warning.
pub fn ignored_name(resolved: Option<SessionId>) -> Option<String> {
    if resolved.is_some() {
        return None;
    }
    std::env::var(SESSION_NAME_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture's live session. `lookup` is exercised through the
    /// store; `requested` is pure and tested without one.
    const LIVE: i64 = 7;
    const ENDED: i64 = 8;

    #[test]
    fn nothing_named_is_not_an_error() {
        assert_eq!(requested(None, None).unwrap(), None);
    }

    /// An exported-but-empty variable falls back, because Python's
    /// `.strip()` and truth test treat `''` as unset. Whitespace too.
    #[test]
    fn an_empty_or_blank_value_reads_as_unset() {
        assert_eq!(requested(None, Some("")).unwrap(), None);
        assert_eq!(requested(None, Some("   ")).unwrap(), None);
        assert_eq!(requested(Some(""), Some("")).unwrap(), None);
        // And a blank *flag* falls through to the variable rather than
        // refusing, which is what "an empty value is unset" has to mean
        // to be one rule.
        assert_eq!(
            requested(Some("  "), Some("12")).unwrap(),
            Some((Origin::Env, 12))
        );
    }

    #[test]
    fn the_variable_is_read_when_no_flag_is_given() {
        assert_eq!(
            requested(None, Some("1163")).unwrap(),
            Some((Origin::Env, 1163))
        );
        // Surrounding whitespace is stripped, as Python's is.
        assert_eq!(
            requested(None, Some(" 1163 ")).unwrap(),
            Some((Origin::Env, 1163))
        );
    }

    /// Python's precedence: when the flag is set the environment is not
    /// consulted at all, so a stray variable in a wrapper cannot shadow
    /// what the caller typed.
    #[test]
    fn the_flag_outranks_the_variable() {
        assert_eq!(
            requested(Some("55"), Some("1163")).unwrap(),
            Some((Origin::Flag, 55))
        );
    }

    /// The one capability that closes permanently. The message has to
    /// say *names are not accepted* rather than "that is not a number",
    /// because over there it is a different feature and not a typo.
    #[test]
    fn a_flag_naming_a_name_is_refused_as_a_name() {
        let err = requested(Some("my-agent"), None).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not a name"), "{msg}");
        assert!(msg.contains("my-agent"), "{msg}");
        // Not the integer-parse message, which would be the wrong
        // diagnosis.
        assert!(!msg.contains("must be an integer"), "{msg}");
    }

    /// `--session -5` is a *name* to Python (`"-5".isdigit()` is false),
    /// so it goes down the creating path there and is refused here.
    /// `TEMPLEDB_SESSION_ID=-5` is a well-formed id to Python's `int()`
    /// and therefore here too — it simply matches no row. The asymmetry
    /// is reproduced rather than tidied.
    #[test]
    fn a_sign_means_different_things_to_the_flag_and_the_variable() {
        assert!(requested(Some("-5"), None).is_err());
        assert_eq!(
            requested(None, Some("-5")).unwrap(),
            Some((Origin::Env, -5))
        );
    }

    /// Python's own wording, because this one is reachable by a script
    /// that exports a typo and the information is the same.
    #[test]
    fn a_non_integer_variable_is_a_parse_error_naming_the_variable() {
        let err = requested(None, Some("notanint")).unwrap_err();
        let msg = format!("{err}");
        // Byte-identical to Python's, single quotes included — verified
        // at the command line against `templedb vcs log`. Asserted whole
        // rather than by `contains`, because the agreement is the point.
        assert_eq!(
            msg,
            "TEMPLEDB_SESSION_ID must be an integer, got 'notanint'"
        );
    }

    #[test]
    fn a_live_session_resolves_to_its_id() {
        let c = crate::testdb::fixture();
        let got = lookup(&c, Origin::Env, LIVE).unwrap();
        assert_eq!(got, Some(SessionId(LIVE)));
        assert_eq!(got.unwrap().get(), LIVE);
    }

    /// The 2026-10-07 fix, from the other side: an ended session is not
    /// a session. Without `ended_at IS NULL` this returns `Some` and
    /// every session-scoped read answers from a dead identity.
    #[test]
    fn an_ended_session_is_refused_rather_than_accepted() {
        let c = crate::testdb::fixture();
        let err = lookup(&c, Origin::Env, ENDED).unwrap_err();
        assert!(
            format!("{err}").contains("references no live session"),
            "{err}"
        );
    }

    #[test]
    fn an_unknown_id_is_refused_and_the_message_names_its_origin() {
        let c = crate::testdb::fixture();
        for (origin, expected) in [
            (Origin::Env, "TEMPLEDB_SESSION_ID=99999 references"),
            (Origin::Flag, "--session=99999 references"),
        ] {
            let err = lookup(&c, origin, 99999).unwrap_err();
            assert!(format!("{err}").starts_with(expected), "{err}");
        }
    }

    /// A refusal is not a decline. `None` means "nobody named a
    /// session"; a pinned id that has ended has to stay an error, or a
    /// stale wrapper silently reads the published view and looks right.
    #[test]
    fn a_dead_pin_is_an_error_and_not_a_fallback_to_none() {
        let c = crate::testdb::fixture();
        assert!(lookup(&c, Origin::Env, ENDED).is_err());
        assert_eq!(lookup(&c, Origin::Env, LIVE).unwrap(), Some(SessionId(7)));
    }

    /// Both refusals exit 1, which is Python's: its `ValidationError`
    /// and `ResourceNotFoundError` both leave the CLI decorator as
    /// `return 1`. No `Coded` anywhere in this module, so this is a
    /// statement about what *not* having one buys.
    #[test]
    fn every_refusal_exits_one() {
        let c = crate::testdb::fixture();
        assert_eq!(
            crate::code_of(&lookup(&c, Origin::Env, ENDED).unwrap_err()),
            crate::USAGE
        );
        assert_eq!(
            crate::code_of(&requested(Some("my-agent"), None).unwrap_err()),
            crate::USAGE
        );
        assert_eq!(
            crate::code_of(&requested(None, Some("x")).unwrap_err()),
            crate::USAGE
        );
    }

    /// The warning is for the case where Python would have used the
    /// name and this port will not. When an id *was* resolved, Python
    /// ignores the name too, so there is no disagreement to report.
    #[test]
    fn the_name_variable_is_only_worth_warning_about_when_nothing_was_pinned() {
        // Cannot set the environment from a test without racing every
        // other test in the binary, so the branch that reads it is
        // covered by the guard clause alone — which is the half with the
        // reasoning in it.
        assert_eq!(ignored_name(Some(SessionId(7))), None);
    }
}
