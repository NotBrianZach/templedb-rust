//! The one place SQLite text and Postgres text are not the same type.
//!
//! # The problem
//!
//! SQLite `TEXT` may contain U+0000. Postgres `text` may not — the
//! server rejects it outright with
//! `invalid byte sequence for encoding "UTF8": 0x00`, and there is no
//! encoding, collation or cast that changes this. It is not a quirk of
//! the client library; `text` is a NUL-terminated representation at the
//! protocol level.
//!
//! The live database contains exactly one such row, and it is not a
//! misclassified binary:
//! `bza/frontend/lib/suggest/normalize.ts` contains
//! `.join('\0')`, which is an ordinary and sensible thing for a
//! TypeScript file to do. So the column really is text, the
//! classification is right, and there is nothing upstream to fix. The
//! mirror simply cannot store the bytes as they are.
//!
//! # The decision
//!
//! Escape on the way in and unescape on the way out, in the Postgres
//! backend, so that **every reader sees the original bytes**. The
//! alternatives were weighed in the README's *Known gaps* and all three
//! fail on something the mirror exists for:
//!
//! - *Skip the row* and *move the text to `content_blob`* both make
//!   `file cat` of that one file behave differently on the two
//!   backends, and both change `content_blobs`'s cross-backend digest —
//!   which is the thing `statediff` exists to compare and which the
//!   write path's verification is built on.
//! - *Store `content_text` as `BYTEA`* is faithful but reaches every
//!   text query: the `LIKE` prefilter, the `text_lower` generated
//!   column and its trigram index, `file cat`, and the diffs.
//!
//! Escaping keeps the digest equal because the digest is computed over
//! rows read through [`Store`](crate::Store), and this unescapes there.
//! The cost is real and is only this: **the bytes in Postgres are not
//! always the bytes in SQLite**, so a `psql` session sees the escape.
//! Every path through this crate does not.
//!
//! # The encoding
//!
//! U+0001 is the escape introducer:
//!
//! | original | stored |
//! |---|---|
//! | `U+0000` | `U+0001` `0` |
//! | `U+0001` | `U+0001` `1` |
//!
//! Two properties matter and neither is an accident.
//!
//! **The introducer is escaped too.** Picking a "rare" marker and
//! assuming it never occurs would be wrong here, and measurably so:
//! the live `content_blobs` has 1 row containing U+0000, 2 containing
//! U+0001, 2 containing U+0002 and 4 containing U+001F. Control
//! characters are not hypothetical in this data. Escaping the
//! introducer makes the transform injective regardless of how often it
//! appears, so the marker's rarity buys small diffs rather than
//! correctness.
//!
//! **Text containing neither character is returned untouched**, as the
//! same allocation-free [`Cow::Borrowed`]. That is 4,715 of the 4,718
//! text rows: three rows have different bytes in the mirror and the
//! rest are byte-identical, which keeps this off the hot path and keeps
//! the blast radius of a mistake down to rows that contain control
//! characters.

use std::borrow::Cow;

/// The escape introducer, U+0001 (START OF HEADING).
const ESC: char = '\u{1}';

/// What follows the introducer for each escaped character.
const NUL_TAG: char = '0';
const ESC_TAG: char = '1';

/// True if `s` needs either transform — the fast path's predicate.
fn is_plain(s: &str) -> bool {
    !s.as_bytes().iter().any(|&b| b == 0 || b == 1)
}

/// Encode for storage in a Postgres `text` column.
///
/// The result never contains U+0000, which is the whole point, and is
/// the input unchanged whenever the input contains neither U+0000 nor
/// U+0001.
pub fn escape(s: &str) -> Cow<'_, str> {
    if is_plain(s) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\0' => {
                out.push(ESC);
                out.push(NUL_TAG);
            }
            ESC => {
                out.push(ESC);
                out.push(ESC_TAG);
            }
            other => out.push(other),
        }
    }
    Cow::Owned(out)
}

/// Decode a value read out of a Postgres `text` column.
///
/// Inverse of [`escape`] for anything [`escape`] produced.
///
/// A sequence this encoding cannot produce — a lone trailing
/// introducer, or an introducer followed by anything other than `0` or
/// `1` — is passed through as written rather than treated as an error.
/// Those bytes can only come from something other than `mirror`
/// writing to the mirror, and in that case the honest thing is to
/// return what is stored: a hard error here would turn a foreign write
/// into a failure of every read of the table.
pub fn unescape(s: &str) -> Cow<'_, str> {
    // A string with no introducer cannot have been escaped, and the
    // common case is that there is none.
    if !s.as_bytes().contains(&1) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != ESC {
            out.push(ch);
            continue;
        }
        match chars.peek() {
            Some(&NUL_TAG) => {
                chars.next();
                out.push('\0');
            }
            Some(&ESC_TAG) => {
                chars.next();
                out.push(ESC);
            }
            // Not ours. Keep the introducer and carry on; the next
            // iteration handles whatever follows.
            _ => out.push(ESC),
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 4,715-row case: untouched, and not reallocated.
    #[test]
    fn ordinary_text_is_borrowed_unchanged() {
        for s in [
            "",
            "hello",
            "fn main() {}\n",
            "tabs\tand\nnewlines\r\n",
            "héllo → 世界",
            // U+0002 and U+001F occur in the live data and are not
            // special to this encoding.
            "ctrl\u{2}and\u{1f}",
        ] {
            let e = escape(s);
            assert!(matches!(e, Cow::Borrowed(_)), "{s:?} should not allocate");
            assert_eq!(e, s);
            let u = unescape(s);
            assert!(matches!(u, Cow::Borrowed(_)), "{s:?} should not allocate");
            assert_eq!(u, s);
        }
    }

    /// The row that forced this to exist.
    #[test]
    fn the_live_nul_round_trips() {
        let original = "parts.map(p => p.toLowerCase()).join('\0')\n";
        let stored = escape(original);
        assert!(!stored.contains('\0'), "Postgres would reject {stored:?}");
        assert_eq!(stored, "parts.map(p => p.toLowerCase()).join('\u{1}0')\n");
        assert_eq!(unescape(&stored), original);
    }

    /// The introducer is escaped too, so a file that already contains
    /// U+0001 round-trips rather than being corrupted. Two live rows
    /// do.
    #[test]
    fn the_introducer_is_itself_escaped() {
        let original = "soh:\u{1}end";
        let stored = escape(original);
        assert_eq!(stored, "soh:\u{1}1end");
        assert_eq!(unescape(&stored), original);
    }

    /// The case that breaks a naive encoding: text that contains the
    /// escape *sequence* already. Without escaping the introducer,
    /// `\u{1}0` would unescape to a NUL that was never there.
    #[test]
    fn text_resembling_an_escape_is_not_mistaken_for_one() {
        let original = "looks like \u{1}0 but is not";
        let stored = escape(original);
        // The introducer got doubled, so the `0` is no longer a tag.
        assert_eq!(stored, "looks like \u{1}10 but is not");
        assert_eq!(unescape(&stored), original);
        assert!(!unescape(&stored).contains('\0'));
    }

    /// Adjacent and repeated NULs, and a NUL at each boundary.
    #[test]
    fn nuls_at_the_edges_and_in_runs() {
        for original in [
            "\0",
            "\0\0\0",
            "\0leading",
            "trailing\0",
            "a\0b\0c",
            "\0\u{1}\0",
            "\u{1}\0\u{1}",
        ] {
            let stored = escape(original);
            assert!(!stored.contains('\0'), "{original:?} -> {stored:?}");
            assert_eq!(unescape(&stored), original, "{original:?}");
        }
    }

    /// Round-trip over a wide spread of generated strings, including
    /// multibyte characters next to escapes — the combination where an
    /// implementation that indexed bytes instead of chars would split a
    /// character.
    #[test]
    fn round_trip_holds_over_generated_strings() {
        let alphabet = ['a', '\0', '\u{1}', '0', '1', 'é', '世', '\n', '\u{1f}'];
        // Every string of length 3 over that alphabet: 729 cases, which
        // covers every adjacency that matters.
        for i in 0..alphabet.len() {
            for j in 0..alphabet.len() {
                for k in 0..alphabet.len() {
                    let original: String = [alphabet[i], alphabet[j], alphabet[k]].iter().collect();
                    let stored = escape(&original);
                    assert!(
                        !stored.contains('\0'),
                        "{original:?} still has a NUL as {stored:?}"
                    );
                    assert_eq!(unescape(&stored), original, "{original:?}");
                }
            }
        }
    }

    /// Escaping is idempotent only in the sense that it is injective:
    /// escaping twice and unescaping twice returns the original, which
    /// is what makes a double-escape a recoverable mistake rather than
    /// a silent one.
    #[test]
    fn double_escaping_is_recoverable() {
        let original = "a\0b\u{1}c";
        let once = escape(original).into_owned();
        let twice = escape(&once).into_owned();
        assert_ne!(once, twice);
        assert_eq!(unescape(&unescape(&twice)), original);
    }

    /// A malformed introducer — one this encoding cannot produce — is
    /// returned as written rather than erroring, so a foreign write to
    /// the mirror degrades to wrong-looking text in one row instead of
    /// failing every read of the table.
    #[test]
    fn a_malformed_sequence_passes_through() {
        assert_eq!(unescape("\u{1}"), "\u{1}");
        assert_eq!(unescape("\u{1}x"), "\u{1}x");
        assert_eq!(unescape("ends with \u{1}"), "ends with \u{1}");
        // And the valid tags still work after one.
        assert_eq!(unescape("\u{1}x\u{1}0"), "\u{1}x\0");
    }
}
