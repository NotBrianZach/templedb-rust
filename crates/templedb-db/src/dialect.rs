//! Which SQL dialect a [`Store`](crate::Store) speaks, and the one
//! syntactic difference that can be handled mechanically.
//!
//! Almost every query in this crate is portable as written: the joins,
//! `COALESCE`, `UNION ALL`, `LIKE ... ESCAPE`, `GROUP BY` on a primary
//! key, and `ORDER BY (revision = 'current') DESC` all mean the same
//! thing to both engines. Two things do not, and they are handled in
//! opposite ways:
//!
//! * **Placeholders.** SQLite writes `?1`, Postgres writes `$1`. This is
//!   pure syntax with no semantic content, so it is rewritten here and
//!   the query modules keep one copy of each SQL string.
//!
//! * **Full-text search.** FTS5's `MATCH` / `snippet()` / `ORDER BY
//!   rank` has no Postgres equivalent that returns the same rows, let
//!   alone the same snippet text — the tokenisers stem differently, so
//!   the *hit set* differs and not merely its order. That cannot be
//!   rewritten and is not attempted here; see `search.rs`.
//!
//! The lesson encoded in the first bullet is that a dialect difference
//! worth centralising is one with no behavioural content. Anything that
//! changes which rows come back belongs in the query module, where it is
//! visible, rather than behind a rewriter that makes it look free.

use anyhow::{bail, Result};

/// The engine behind a [`Store`](crate::Store).
///
/// Carried as data rather than being a type parameter because the CLI
/// picks a backend at runtime from the environment, and because the two
/// or three queries that genuinely differ should read as a `match` in
/// the module that owns them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    Postgres,
}

impl Dialect {
    /// Translate a query written with SQLite's `?N` placeholders.
    ///
    /// For [`Dialect::Sqlite`] this is the identity, so the SQL in the
    /// query modules stays the text you can paste into the `sqlite3`
    /// CLI. Checking a query against the live database by hand is how
    /// most of this port's parity work gets done, and a query builder
    /// that produced SQL nobody could paste would have cost more than
    /// it saved.
    pub fn bind(self, sql: &str) -> Result<String> {
        match self {
            Dialect::Sqlite => Ok(sql.to_string()),
            Dialect::Postgres => rewrite_placeholders(sql),
        }
    }
}

/// Rewrite `?N` to `$N`, leaving single-quoted literals alone.
///
/// The literal tracking is not hypothetical caution about SQL this
/// crate does not contain — `search.rs` passes `'[', ']', ' … '` to
/// `snippet()` and `source.rs` carries `ESCAPE '\'`. None of those hold
/// a `?` today, so a blind `replace` would work today and break on the
/// first query that interpolates a user-visible `?` into a literal. The
/// quote state machine is six lines; finding that bug later is not.
///
/// A bare `?` is rejected rather than numbered. rusqlite accepts
/// mixed anonymous and numbered parameters in one statement and
/// Postgres has no anonymous form at all, so inferring an index here
/// would mean guessing at exactly the point where the two engines stop
/// agreeing. The query modules write every placeholder numbered.
fn rewrite_placeholders(sql: &str) -> Result<String> {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.char_indices().peekable();
    let mut in_literal = false;

    while let Some((idx, ch)) = chars.next() {
        // A doubled '' inside a literal toggles off and straight back
        // on, which lands in the right state without a special case.
        // Neither engine treats a backslash as escaping a quote here:
        // SQLite has no string escapes, and Postgres with
        // `standard_conforming_strings` on (the default since 9.1) does
        // not either. So `ESCAPE '\'` closes its literal correctly.
        if ch == '\'' {
            in_literal = !in_literal;
            out.push(ch);
            continue;
        }
        if in_literal || ch != '?' {
            out.push(ch);
            continue;
        }

        let mut digits = String::new();
        while let Some(&(_, d)) = chars.peek() {
            if d.is_ascii_digit() {
                digits.push(d);
                chars.next();
            } else {
                break;
            }
        }
        if digits.is_empty() {
            bail!(
                "anonymous `?` placeholder at byte {idx} cannot be translated to Postgres; \
                 write it numbered (`?1`, `?2`, ...)"
            );
        }
        out.push('$');
        out.push_str(&digits);
    }

    if in_literal {
        bail!("unterminated string literal in SQL");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_is_the_identity() {
        let sql = "SELECT a FROM t WHERE b = ?1 AND c LIKE ?2 ESCAPE '\\'";
        assert_eq!(Dialect::Sqlite.bind(sql).unwrap(), sql);
    }

    #[test]
    fn numbered_placeholders_become_dollar_signs() {
        assert_eq!(
            Dialect::Postgres
                .bind("SELECT a FROM t WHERE b = ?1 AND c = ?2 LIMIT ?3")
                .unwrap(),
            "SELECT a FROM t WHERE b = $1 AND c = $2 LIMIT $3"
        );
    }

    /// Two-digit indices exist: `fetch_edges` builds one placeholder per
    /// `--via` value, and nothing caps that at nine.
    #[test]
    fn multi_digit_indices_survive() {
        assert_eq!(
            Dialect::Postgres.bind("IN (?10, ?11, ?2)").unwrap(),
            "IN ($10, $11, $2)"
        );
    }

    /// The reason the quote state machine exists.
    #[test]
    fn question_marks_inside_literals_are_untouched() {
        assert_eq!(
            Dialect::Postgres
                .bind("SELECT snippet(t, 1, '[?1]', ']', ' … ', 16) WHERE x = ?1")
                .unwrap(),
            "SELECT snippet(t, 1, '[?1]', ']', ' … ', 16) WHERE x = $1"
        );
    }

    /// A doubled quote closes and reopens, so a `?` after it is still
    /// inside the literal.
    #[test]
    fn doubled_quotes_keep_literal_state() {
        assert_eq!(
            Dialect::Postgres
                .bind("SELECT 'it''s ?1 fine', ?1")
                .unwrap(),
            "SELECT 'it''s ?1 fine', $1"
        );
    }

    /// `ESCAPE '\'` must not swallow the closing quote. If it did, every
    /// placeholder after it would be treated as literal text and left as
    /// `?N`, which Postgres rejects — a loud failure, but in the LIKE
    /// queries rather than here.
    #[test]
    fn a_backslash_literal_does_not_escape_the_closing_quote() {
        assert_eq!(
            Dialect::Postgres
                .bind("WHERE p LIKE ?2 ESCAPE '\\' AND q = ?3")
                .unwrap(),
            "WHERE p LIKE $2 ESCAPE '\\' AND q = $3"
        );
    }

    #[test]
    fn anonymous_placeholders_are_rejected_with_their_position() {
        let err = Dialect::Postgres
            .bind("SELECT a FROM t LIMIT ?")
            .unwrap_err()
            .to_string();
        assert!(err.contains("anonymous"), "{err}");
        assert!(err.contains("numbered"), "{err}");
    }

    #[test]
    fn unterminated_literal_is_an_error() {
        assert!(Dialect::Postgres.bind("SELECT 'oops").is_err());
    }
}
