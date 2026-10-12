//! Matching, ranking and snippet rendering for `search content`, in
//! Rust rather than in either engine's full-text extension.
//!
//! # Why this is not an index lookup
//!
//! The obvious implementation is to let each backend's full-text index
//! answer the query — FTS5's `MATCH` on SQLite, `tsvector @@` on
//! Postgres — and that is what this port did until it was measured.
//! The two do not agree, and not by a little. Against the same 2,063
//! text files:
//!
//! | query | FTS5 | Postgres |
//! |---|---|---|
//! | `deployment` | 356 | 449 |
//! | `checkout` | 273 | 244 |
//! | `generated` | 602 | 532 |
//! | `queries` | 635 | 532 |
//! | `session` | 459 | 467 |
//!
//! FTS5 tokenises with the original Porter algorithm and Postgres's
//! `english` configuration uses Porter2, so they disagree about which
//! words share a stem. `deployment` and `deploy` are one term to
//! Postgres and two to FTS5; `generated` and `queries` are one count to
//! Postgres and two to FTS5. A command built on those would answer a
//! quarter differently depending on which database it was pointed at,
//! with nothing in the output to say so.
//!
//! So the index does not decide what matches. SQL narrows the
//! candidates with a `LIKE` that both engines read identically, and
//! everything that decides the answer — which files match, their order,
//! the excerpt text — happens here, once, for both backends. That is
//! the only arrangement in which `search content` means one thing.
//!
//! # What changed as a result
//!
//! Semantics are now substring, not stemmed. `search content run`
//! finds `run`, `running` *and* `runtime`, where FTS5 found the first
//! two (one stem) and not the third (a different token). This is a
//! deliberate break from both the Python implementation and this
//! port's own earlier behaviour, and it is the right direction for the
//! corpus: the thing being searched is source code, where `deploy_` is
//! an identifier fragment and English stemming of it is noise. It is
//! also the only semantics that can be reproduced exactly without
//! shipping a stemmer that has to agree with two others.
//!
//! Case folding is ASCII-only, which is what SQLite's `LOWER()` does
//! and what Postgres's does under `COLLATE "C"`. Matching the SQL
//! prefilter's folding rules is what keeps the prefilter from
//! discarding a candidate this module would have matched.

/// A parsed query: the terms a file must contain, all of them.
///
/// Whitespace separates terms and double quotes group one. There are no
/// operators — no `OR`, no `NEAR`, no prefix `*` — because FTS5's query
/// language and Postgres's are themselves different languages, and
/// supporting either would put the dialect back into the part of the
/// command that decides the answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// Lowercased, in the order written. Order has no effect on which
    /// files match; it only decides which term's occurrence anchors the
    /// first snippet window.
    pub terms: Vec<String>,
}

impl Query {
    /// Split a user query into terms.
    ///
    /// An unterminated quote runs to the end of the string rather than
    /// being an error, matching what every shell-adjacent tool does
    /// with one and avoiding an error message for a typo that has an
    /// obvious reading.
    pub fn parse(raw: &str) -> Self {
        let mut terms = Vec::new();
        let mut current = String::new();
        let mut quoted = false;
        for ch in raw.chars() {
            match ch {
                '"' => {
                    quoted = !quoted;
                    // A closing quote ends the term even if the next
                    // character is not whitespace, so `"a"b` is two
                    // terms rather than one surprising one.
                    if !quoted && !current.is_empty() {
                        terms.push(std::mem::take(&mut current));
                    }
                }
                c if c.is_whitespace() && !quoted => {
                    if !current.is_empty() {
                        terms.push(std::mem::take(&mut current));
                    }
                }
                c => current.extend(c.to_lowercase()),
            }
        }
        if !current.is_empty() {
            terms.push(current);
        }
        Self { terms }
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }
}

/// Fold one byte ASCII-only, the way SQLite's `LIKE` does.
///
/// Deliberately not `char::to_lowercase`. This module is the authority
/// on what matches, and the SQL prefilter only has to be *no narrower*
/// than it — Postgres's `ILIKE` folds the full Unicode range and hands
/// over more than this accepts, which `search.rs` then discards. If the
/// authority folded more than SQLite's prefilter does, the asymmetry
/// would run the other way and SQLite would miss files.
///
/// Working a byte at a time also keeps the returned offsets valid in
/// the original string, which lowercasing into a new `String` would
/// not: `'İ'` lowercases to two characters and every offset after it
/// would shift.
fn fold(b: u8) -> u8 {
    b.to_ascii_lowercase()
}

/// Byte offsets of every occurrence of `needle` in `haystack`,
/// ASCII-case-insensitively.
///
/// Occurrences do not overlap: after a match the scan resumes past it,
/// so `aa` in `aaa` is found once. That is the count the snippet needs
/// — overlapping spans would render as nested brackets — and it is the
/// same convention `REPLACE`-based counting uses in SQL, which keeps
/// the door open to computing the frequency server-side later.
///
/// Offsets land on character boundaries without needing a check. An
/// all-ASCII needle cannot match inside a multi-byte sequence, because
/// every continuation byte is `>= 0x80` and no ASCII byte is; a
/// non-ASCII needle cannot either, because UTF-8 is self-synchronising.
pub fn occurrences(haystack: &str, needle: &str) -> Vec<usize> {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    if n.is_empty() || n.len() > h.len() {
        return Vec::new();
    }
    // Scan for the first byte in either case before comparing the rest.
    // The naive version compared every position with a zipped closure
    // over the whole needle, and this function runs over every byte of
    // every candidate file — tens of megabytes per query — so the inner
    // loop is worth keeping to two comparisons.
    let (lo, up) = (fold(n[0]), n[0].to_ascii_uppercase());
    let mut out = Vec::new();
    let mut start = 0;
    let last = h.len() - n.len();
    while start <= last {
        let b = h[start];
        if b != lo && b != up {
            start += 1;
            continue;
        }
        if h[start + 1..start + n.len()]
            .iter()
            .zip(&n[1..])
            .all(|(a, b)| fold(*a) == fold(*b))
        {
            out.push(start);
            start += n.len();
        } else {
            start += 1;
        }
    }
    out
}

/// Corpus-wide figures the score needs but a candidate cannot supply.
///
/// Read from SQL over every indexed file, not over the candidates: an
/// `idf` computed against the candidate set would be zero whenever
/// every candidate contains every term, which is the normal case for a
/// single-term query.
#[derive(Debug, Clone, Copy)]
pub struct CorpusStats {
    /// Number of current, active, text files.
    pub docs: i64,
    /// Total length of all of them, in characters.
    pub total_chars: i64,
}

impl CorpusStats {
    fn avg_len(&self) -> f64 {
        if self.docs == 0 {
            return 1.0;
        }
        (self.total_chars as f64 / self.docs as f64).max(1.0)
    }
}

/// Okapi BM25's usual constants. `k1` damps the value of repetition,
/// `b` decides how much a long file is penalised for having more room
/// to contain the term.
const K1: f64 = 1.2;
const B: f64 = 0.75;

/// BM25 for one document.
///
/// `tf` is per term, in the query's order; `df` likewise — the number
/// of candidate documents containing that term, which is the whole
/// corpus's document frequency because a document the prefilter dropped
/// cannot contain the term.
///
/// Length is counted in characters rather than tokens. Tokenising
/// 112 MB of source to get a token count, per invocation, would cost
/// more than the ranking is worth, and the normalisation only needs a
/// consistent measure of "how much room did this file have".
pub fn bm25(tf: &[usize], df: &[usize], doc_chars: usize, corpus: CorpusStats) -> f64 {
    let n = corpus.docs.max(1) as f64;
    let norm = K1 * (1.0 - B + B * (doc_chars as f64 / corpus.avg_len()));
    tf.iter()
        .zip(df)
        .map(|(&tf, &df)| {
            if tf == 0 {
                return 0.0;
            }
            let df = (df as f64).min(n);
            let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
            idf * (tf as f64 * (K1 + 1.0)) / (tf as f64 + norm)
        })
        .sum()
}

/// Characters of context kept either side of a match in a snippet.
const CONTEXT: usize = 48;
/// How many windows one snippet may show before it stops.
const MAX_WINDOWS: usize = 3;
/// The widest one window may grow to by merging nearby matches.
///
/// Bounds the whole snippet at roughly `MAX_WINDOWS * MAX_WINDOW_CHARS`,
/// which is what keeps a hit on a file that mentions the term a hundred
/// times from printing most of the file.
const MAX_WINDOW_CHARS: usize = 160;
/// Placed between windows, and at either end when text was dropped.
const ELLIPSIS: &str = " … ";

/// One snippet window: the byte range it covers and the matches that
/// fall inside it.
type Window = (usize, usize, Vec<(usize, usize)>);

/// Render a one-line excerpt with the matches bracketed.
///
/// Deliberately not an imitation of FTS5's `snippet()`. That function
/// works in tokens and counts them against a budget; reproducing it
/// byte-for-byte would mean reimplementing its window selection, and
/// the output would still only match one of the two backends this has
/// to serve. The shape is kept — `[`, `]` and ` … ` are the delimiters
/// the Python implementation passed to `snippet()` — so the output
/// still reads the way callers expect even though the windows differ.
///
/// Always single-line: newlines and runs of whitespace collapse to one
/// space, because the CLI prints this inside an indented line and a
/// multi-line excerpt would break `grep` over the output.
pub fn snippet(text: &str, matches: &[(usize, usize)]) -> String {
    if matches.is_empty() {
        return collapse(&clamped_slice(text, 0, CONTEXT * 2));
    }

    // Merge matches whose context overlaps, so two hits a few
    // characters apart produce one window rather than two with ` … `
    // between them — but stop merging at `MAX_WINDOW_CHARS`.
    //
    // The cap is not hypothetical tidiness. Without it, merging is
    // transitive: every match within `CONTEXT` of the previous one
    // extends the same window, so a file that names the search term
    // fifty times in close succession renders as one window spanning
    // all fifty. `search content checkout` produced a 2,000-character
    // "snippet" per hit that way, which is not an excerpt.
    let mut windows: Vec<Window> = Vec::new();
    let mut stopped_early = false;
    for (i, &(start, end)) in matches.iter().enumerate() {
        let from = back_off(text, start, CONTEXT);
        let to = forward(text, end, CONTEXT);
        let merged = match windows.last_mut() {
            Some(last)
                if from <= last.1
                    && count_chars(text, last.0, to.max(last.1)) <= MAX_WINDOW_CHARS =>
            {
                last.1 = last.1.max(to);
                last.2.push((start, end));
                true
            }
            _ => false,
        };
        if merged {
            continue;
        }
        if windows.len() == MAX_WINDOWS {
            // More matches exist than the snippet will show.
            stopped_early = i < matches.len();
            break;
        }
        windows.push((from, to, vec![(start, end)]));
    }

    let last_end = windows.last().map_or(0, |w| w.1);
    let truncated_tail = stopped_early || last_end < text.len();
    let mut parts = Vec::new();
    for (from, to, inner) in &windows {
        let mut piece = String::new();
        let mut cursor = *from;
        for &(ms, me) in inner {
            // A match can sit outside the window if it was merged in
            // and then the window was clamped; skip rather than panic
            // on the slice.
            if ms < cursor || me > *to {
                continue;
            }
            piece.push_str(&text[cursor..ms]);
            piece.push('[');
            piece.push_str(&text[ms..me]);
            piece.push(']');
            cursor = me;
        }
        piece.push_str(&text[cursor..*to]);
        parts.push(collapse(&piece));
    }

    let mut out = String::new();
    if windows[0].0 > 0 {
        out.push_str(ELLIPSIS.trim_start());
    }
    out.push_str(&parts.join(ELLIPSIS));
    if truncated_tail {
        out.push_str(ELLIPSIS.trim_end());
    }
    out
}

/// Collapse every run of whitespace to a single space and trim.
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_space = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            in_space = true;
            continue;
        }
        if in_space && !out.is_empty() {
            out.push(' ');
        }
        in_space = false;
        out.push(ch);
    }
    out
}

/// Step back `chars` characters from `at`, landing on a boundary.
fn back_off(text: &str, at: usize, chars: usize) -> usize {
    text[..at]
        .char_indices()
        .rev()
        .take(chars)
        .last()
        .map_or(at, |(i, _)| i)
}

/// Step forward `chars` characters from `at`, landing on a boundary.
fn forward(text: &str, at: usize, chars: usize) -> usize {
    text[at..]
        .char_indices()
        .nth(chars)
        .map_or(text.len(), |(i, _)| at + i)
}

/// `text[..]` up to `chars` characters, on a boundary.
fn clamped_slice(text: &str, from: usize, chars: usize) -> String {
    text[from..forward(text, from, chars)].to_string()
}

/// Characters, not bytes, between two byte offsets. The window cap is
/// in characters so a snippet of CJK text is bounded by what it looks
/// like rather than by how it encodes.
fn count_chars(text: &str, from: usize, to: usize) -> usize {
    text[from..to.min(text.len())].chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_splits_on_whitespace_and_lowercases() {
        assert_eq!(
            Query::parse("Foo BAR  baz").terms,
            vec!["foo", "bar", "baz"]
        );
        assert!(Query::parse("   ").is_empty());
    }

    #[test]
    fn quotes_group_a_phrase_including_its_spaces() {
        assert_eq!(
            Query::parse("\"fn main\" rust").terms,
            vec!["fn main", "rust"]
        );
        // An unterminated quote runs to the end rather than erroring.
        assert_eq!(Query::parse("\"fn main").terms, vec!["fn main"]);
        // A closing quote ends the term.
        assert_eq!(Query::parse("\"a\"b").terms, vec!["a", "b"]);
    }

    #[test]
    fn occurrences_are_case_insensitive_and_non_overlapping() {
        assert_eq!(occurrences("Run run RUN", "run"), vec![0, 4, 8]);
        // Non-overlapping: the scan resumes past a match, so `aaa`
        // holds one `aa` and `aaaa` holds two. Overlapping spans would
        // render as nested brackets in a snippet.
        assert_eq!(occurrences("aaa", "aa"), vec![0]);
        assert_eq!(occurrences("aaaa", "aa"), vec![0, 2]);
        assert!(occurrences("abc", "zz").is_empty());
        assert!(occurrences("abc", "").is_empty());
        // Needle longer than haystack must not panic on the range.
        assert!(occurrences("a", "abc").is_empty());
        // A near-miss on the first byte must not stall the scan.
        assert_eq!(occurrences("rax run", "run"), vec![4]);
    }

    /// Substring, not stemmed or tokenised — this is the behaviour
    /// change, so it is pinned rather than left implied. FTS5 matched
    /// the first two and not the third.
    #[test]
    fn matching_is_substring_not_word_or_stem() {
        assert_eq!(occurrences("run", "run").len(), 1);
        assert_eq!(occurrences("running", "run").len(), 1);
        assert_eq!(occurrences("runtime", "run").len(), 1);
        assert_eq!(occurrences("prerun", "run").len(), 1);
        // And the reverse: a stemmed engine would match `run` for the
        // query `running`; substring does not.
        assert!(occurrences("run", "running").is_empty());
    }

    /// Offsets must stay valid in the original string. Lowercasing into
    /// a new `String` would shift everything after a character whose
    /// lowercase form is longer.
    #[test]
    fn offsets_survive_multibyte_text() {
        let text = "héllo RUN wörld";
        let hits = occurrences(text, "run");
        assert_eq!(hits.len(), 1);
        let at = hits[0];
        assert!(text.is_char_boundary(at));
        assert_eq!(&text[at..at + 3], "RUN");
    }

    /// Non-ASCII case folding is not attempted, because the SQL
    /// prefilter's `LOWER()` does not attempt it either. Matching more
    /// than the prefilter passes through would mean missing files.
    #[test]
    fn folding_is_ascii_only() {
        assert!(occurrences("ÉCOLE", "école").is_empty());
        assert_eq!(occurrences("école", "école").len(), 1);
    }

    #[test]
    fn bm25_prefers_more_mentions_and_shorter_files() {
        let corpus = CorpusStats {
            docs: 1000,
            total_chars: 1_000_000,
        };
        let many = bm25(&[10], &[50], 1000, corpus);
        let few = bm25(&[1], &[50], 1000, corpus);
        assert!(many > few, "{many} vs {few}");

        let short = bm25(&[5], &[50], 500, corpus);
        let long = bm25(&[5], &[50], 50_000, corpus);
        assert!(short > long, "{short} vs {long}");

        // A term in almost every file is worth less than a rare one.
        let rare = bm25(&[3], &[2], 1000, corpus);
        let common = bm25(&[3], &[990], 1000, corpus);
        assert!(rare > common, "{rare} vs {common}");

        // No matches scores zero rather than NaN.
        assert_eq!(bm25(&[0], &[0], 1000, corpus), 0.0);
    }

    /// An empty corpus must not divide by zero.
    #[test]
    fn bm25_survives_an_empty_corpus() {
        let empty = CorpusStats {
            docs: 0,
            total_chars: 0,
        };
        assert!(bm25(&[1], &[1], 10, empty).is_finite());
    }

    #[test]
    fn snippet_brackets_the_match_and_stays_on_one_line() {
        let text = "fn main() {\n    let x = run();\n}\n";
        let m = occurrences(text, "run");
        let s = snippet(text, &[(m[0], m[0] + 3)]);
        assert!(s.contains("[run]"), "{s}");
        assert!(!s.contains('\n'), "{s}");
        // Whitespace runs collapse, so the indentation does not survive
        // as a gap.
        assert!(!s.contains("  "), "{s}");
    }

    #[test]
    fn snippet_marks_where_it_cut() {
        let long = format!("{}needle{}", "a".repeat(500), "b".repeat(500));
        let m = occurrences(&long, "needle");
        let s = snippet(&long, &[(m[0], m[0] + 6)]);
        assert!(s.starts_with("…"), "{s}");
        assert!(s.ends_with("…"), "{s}");
        assert!(s.contains("[needle]"), "{s}");
        // And it is bounded rather than returning the whole file.
        assert!(s.chars().count() < 200, "{} chars", s.chars().count());
    }

    /// Text shorter than the window is shown whole, with no ellipsis
    /// implying something was withheld.
    #[test]
    fn a_short_file_gets_no_ellipsis() {
        let text = "let run = 1;";
        let m = occurrences(text, "run");
        let s = snippet(text, &[(m[0], m[0] + 3)]);
        assert_eq!(s, "let [run] = 1;");
    }

    /// Two matches far apart get two windows joined by an ellipsis;
    /// two matches close together get one window.
    #[test]
    fn windows_merge_when_their_context_overlaps() {
        let near = "run and run again";
        let m = occurrences(near, "run");
        let s = snippet(near, &[(m[0], m[0] + 3), (m[1], m[1] + 3)]);
        assert_eq!(s, "[run] and [run] again");
        assert_eq!(s.matches(" … ").count(), 0);

        let far = format!("run{}run", " ".repeat(400));
        let m = occurrences(&far, "run");
        let s = snippet(&far, &[(m[0], m[0] + 3), (m[1], m[1] + 3)]);
        assert_eq!(s.matches(" … ").count(), 1, "{s}");
    }

    /// The defect this cap exists for. Merging is transitive, so
    /// without a width limit a file naming the term repeatedly at close
    /// range collapses into one window spanning all of them —
    /// `search content checkout` rendered a 2,000-character "snippet"
    /// per hit, which is most of the file rather than an excerpt.
    #[test]
    fn densely_repeated_matches_do_not_merge_without_bound() {
        // 60 mentions, each a few characters apart, so every one is
        // within CONTEXT of its neighbour.
        let text = "checkout ".repeat(60);
        let m: Vec<(usize, usize)> = occurrences(&text, "checkout")
            .into_iter()
            .map(|i| (i, i + 8))
            .collect();
        assert_eq!(m.len(), 60);
        let s = snippet(&text, &m);
        // Bounded by MAX_WINDOWS windows of MAX_WINDOW_CHARS, plus the
        // brackets the matches inside contribute and the ellipses.
        let budget = MAX_WINDOWS * (MAX_WINDOW_CHARS * 2) + 16;
        assert!(
            s.chars().count() < budget,
            "{} chars, budget {budget}",
            s.chars().count()
        );
        // And it still says it cut, rather than looking complete.
        assert!(s.ends_with("…"), "{s}");
    }

    /// A file with more matches than `MAX_WINDOWS` must not render an
    /// unbounded snippet.
    #[test]
    fn snippet_window_count_is_capped() {
        let text = (0..50)
            .map(|i| format!("needle{}", "x".repeat(200 + i)))
            .collect::<String>();
        let m: Vec<(usize, usize)> = occurrences(&text, "needle")
            .into_iter()
            .map(|i| (i, i + 6))
            .collect();
        assert!(m.len() > MAX_WINDOWS);
        let s = snippet(&text, &m);
        assert!(s.matches(" … ").count() <= MAX_WINDOWS, "{s}");
    }

    /// No matches at all still yields a readable head of the file
    /// rather than an empty string, which is what the caller shows for
    /// a file the prefilter passed but no term landed in. That should
    /// not happen, but an empty line in the output would be a worse way
    /// to find out.
    #[test]
    fn no_matches_falls_back_to_the_head_of_the_file() {
        let s = snippet("hello world", &[]);
        assert_eq!(s, "hello world");
    }
}
