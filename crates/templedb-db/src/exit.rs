//! Exit codes, and the one error type that carries a non-default one.
//!
//! Python's read commands use three statuses and they mean different
//! things to a caller:
//!
//! | code | meaning | where |
//! |---|---|---|
//! | 1 | bad input, or a project that does not resolve | everywhere |
//! | 2 | the thing you named does not exist | `entity explore` / `trace` / `paths`, `source snapshot` / `revisions` |
//! | 3 | a valid negative answer | `entity paths`: no path within depth N |
//!
//! Until this module existed every error in the port left through one
//! `anyhow` path and exited 1, so a script could not tell `entity
//! explore Machin/zMothership2` — a typo in the *kind*, which is bad
//! input — from `entity explore Machine/zMothership3`, which is a
//! question about data that was asked correctly and answered no. Python
//! distinguishes them and the README carried the gap as *Error exits
//! are all 1*.
//!
//! Three is already matched, because it is the one a script is most
//! likely to branch on and it is not an error at all: `entity paths`
//! prints its answer on stdout and returns [`NO_PATH`] through the
//! normal `Ok` path. Only [`NOT_FOUND`] needs to travel out of an
//! error, and that is what [`Coded`] is for.
//!
//! # Why a type rather than a code threaded through signatures
//!
//! The five sites that need exit 2 are three functions deep in two
//! query modules, and every one of them is reached by a CLI arm that
//! already returns `anyhow::Result`. Threading a code through would
//! mean a bespoke error enum per module and a `match` per call site to
//! collapse it again — the sites that *do not* care, which is nearly
//! all of them, would pay for the two that do.
//!
//! Attaching the code to the error instead means `?` keeps working
//! unchanged and [`code_of`] asks once, in `main`. The cost is that the
//! code is discoverable only by downcast rather than visible in the
//! type, which is the right trade for a property exactly one caller
//! reads.

use std::fmt;

/// Bad input: an address that does not parse, a project that does not
/// resolve, a blob that is gone. The default for any error with no
/// [`Coded`] in its chain, matching Python's bare `return 1`.
pub const USAGE: i32 = 1;

/// The entity, snapshot or revision named by the caller does not exist.
/// A well-formed question with a negative answer, which is why Python
/// gives it a status of its own.
pub const NOT_FOUND: i32 = 2;

/// `entity paths` found no path within `--max-depth`. Not an error:
/// returned through `Ok` from the command arm, never through [`Coded`].
pub const NO_PATH: i32 = 3;

/// An error that remembers which exit status it should produce.
///
/// Constructed through [`Coded::not_found`] rather than freely, because
/// [`USAGE`] is the default for everything else and a second way to
/// spell it would invite drift.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Coded {
    code: i32,
    message: String,
}

impl Coded {
    /// The named thing does not exist: exit [`NOT_FOUND`].
    pub fn not_found(message: impl Into<String>) -> Self {
        Coded {
            code: NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for Coded {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Coded {}

/// The status to exit with for an error on its way out of `main`.
///
/// Searches the whole chain, so a [`Coded`] keeps its status through
/// any number of `.context(...)` layers added on the way up. That
/// matters: the query modules add context freely and none of those call
/// sites should have to know that the error underneath carries a code.
pub fn code_of(err: &anyhow::Error) -> i32 {
    err.downcast_ref::<Coded>()
        .map_or(USAGE, |coded| coded.code)
}

/// `bail!` with an exit status of [`NOT_FOUND`].
///
/// Same shape as `anyhow::bail!` — a format string and arguments — so
/// converting a site is a one-word edit and the message is unchanged.
#[macro_export]
macro_rules! bail_not_found {
    ($($arg:tt)*) => {
        return Err(::anyhow::Error::new(
            $crate::Coded::not_found(format!($($arg)*))
        ))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn a_plain_error_exits_one() {
        let err = anyhow::anyhow!("no database at /nope");
        assert_eq!(code_of(&err), USAGE);
    }

    #[test]
    fn a_coded_error_carries_its_status() {
        let err = anyhow::Error::new(Coded::not_found("no entity Machine/nope"));
        assert_eq!(code_of(&err), NOT_FOUND);
        assert_eq!(err.to_string(), "no entity Machine/nope");
    }

    /// The property the whole design rests on. `lookup_entity` is three
    /// frames below the CLI arm and the frames in between add context;
    /// if downcasting stopped at the outermost layer the code would be
    /// silently lost and every error would exit 1 again — which is the
    /// bug this module fixes, reintroduced invisibly.
    #[test]
    fn context_layers_do_not_hide_the_status() {
        let err = Err::<(), _>(anyhow::Error::new(Coded::not_found(
            "no entity Machine/nope",
        )))
        .context("looking up the source")
        .context("entity paths")
        .unwrap_err();
        assert_eq!(code_of(&err), NOT_FOUND);
        // The context is still on the front of the chain, so the
        // message a user sees is not weakened by the downcast working.
        assert_eq!(err.to_string(), "entity paths");
        assert_eq!(
            format!("{err:#}"),
            "entity paths: looking up the source: no entity Machine/nope"
        );
    }

    #[test]
    fn the_macro_produces_the_same_error_as_the_constructor() {
        fn site() -> anyhow::Result<()> {
            let (kind, reference) = ("Machine", "nope");
            bail_not_found!("no entity {kind}/{reference}");
        }
        let err = site().unwrap_err();
        assert_eq!(code_of(&err), NOT_FOUND);
        assert_eq!(err.to_string(), "no entity Machine/nope");
    }
}
