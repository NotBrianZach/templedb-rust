//! LIKE-pattern escaping, in one place.
//!
//! Every command that accepts a user string and feeds it to `LIKE` needs
//! this, and the repeated bug is forgetting it: `_` means "any
//! character" to SQLite, and 9,846 of the 55,196 `entities` rows in the
//! live database have one in `external_ref`. A caller who types a
//! literal `_` and gets wildcard matches has no way to tell from the
//! output that it happened.
//!
//! This started as three identical private copies — `files.rs`,
//! `source.rs`, and then `entities.rs` wanted a fourth. The pattern is
//! always the same: escape here, and declare `ESCAPE '\'` on the
//! operator in the SQL. One without the other is worse than neither,
//! because the backslashes then match literally.

/// Backslash-escape the three LIKE metacharacters (`%`, `_`, `\`) so the
/// string matches itself.
///
/// The SQL using the result must say `ESCAPE '\'`; SQLite has no default
/// escape character, so an unescaped-but-backslashed pattern looks for
/// the backslashes.
pub(crate) fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::escape_like;

    #[test]
    fn metacharacters_are_escaped_and_nothing_else_is() {
        assert_eq!(escape_like("plain"), "plain");
        assert_eq!(escape_like("a_b"), "a\\_b");
        assert_eq!(escape_like("100%"), "100\\%");
        assert_eq!(escape_like("c:\\x"), "c:\\\\x");
        // Paths and hashes are the common inputs; neither gains noise.
        assert_eq!(escape_like("src/main.rs"), "src/main.rs");
        assert_eq!(escape_like("ABCD1234"), "ABCD1234");
    }

    /// The escaped form has to survive being embedded in `%...%`, which
    /// is how `entity search` uses it.
    #[test]
    fn escaped_form_nests_inside_a_contains_pattern() {
        assert_eq!(format!("%{}%", escape_like("mod_a")), "%mod\\_a%");
    }
}
