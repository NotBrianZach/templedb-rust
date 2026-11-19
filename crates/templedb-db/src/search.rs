//! `templedb search ...` reads.
//!
//! The division of labour here is the point. SQL narrows the candidate
//! files with a `LIKE` that both engines read identically; everything
//! that decides the answer — which files match, their order, the
//! excerpt — happens in [`crate::textsearch`], once, for both
//! backends. Neither engine's full-text index is consulted.
//!
//! That is not the arrangement this port started with, and the reason
//! for the change is in `textsearch.rs`: FTS5 and Postgres `tsvector`
//! disagree about a quarter of the hit set on this corpus, because one
//! stems with Porter and the other with Porter2. A command whose
//! answer depends on which database it is pointed at, with nothing in
//! the output to say so, is the failure this port exists to remove.

use crate::dialect::Dialect;
use crate::like::escape_like;
use crate::store::Store;
use crate::textsearch::{bm25, occurrences, snippet, CorpusStats, Query};
use crate::value::Value;
use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub project: String,
    pub path: String,
    pub snippet: String,
}

/// One row of `search files`, which matches on the *path* and reads no
/// content at all.
///
/// `lines_of_code` stays an `Option` rather than being coalesced,
/// because Python prints `str(None)` — the literal word `None` — for the
/// 576 of 2,780 rows that have no count, and that is in the table being
/// diffed. Coalescing to 0 here would be the same divergence this README
/// records for `project list`, reintroduced in a command where it is
/// avoidable.
#[derive(Debug, Clone)]
pub struct FileNameHit {
    pub project: String,
    pub path: String,
    pub type_name: String,
    pub lines_of_code: Option<i64>,
}

/// How many candidate files are read before ranking.
///
/// Ranking needs every candidate's text, so this is the one place the
/// command's memory use is bounded. The live corpus has 2,063 current
/// text files in total, so the cap cannot bite today; it exists so that
/// a corpus ten times larger degrades by dropping documents loudly
/// rather than by reading gigabytes silently. [`search_content`]
/// reports when it truncates.
const MAX_CANDIDATES: usize = 20_000;

/// Full-text search over current file contents.
///
/// Returns the hits in descending relevance, with `(project, path)` as
/// the tie-break so two runs agree and so the two backends agree. The
/// candidate query's own `ORDER BY` fixes the order ties are broken in
/// before the sort ever sees them.
///
/// The second return value is the number of candidates dropped by
/// [`MAX_CANDIDATES`], so the caller can say so rather than present a
/// truncated search as a complete one.
pub fn search_content(
    store: &dyn Store,
    pattern: &str,
    project: Option<&str>,
    limit: i64,
) -> Result<(Vec<SearchHit>, usize)> {
    let query = Query::parse(pattern);
    if query.is_empty() {
        anyhow::bail!("empty search query");
    }

    // One case-insensitive substring test per term, ANDed.
    //
    // # The prefilter does not have to agree with the matcher
    //
    // It only has to be *no narrower*. The authority on what matches is
    // `textsearch`, which folds ASCII only; the prefilter may fold more
    // and merely hand over candidates that the matcher then discards.
    // That asymmetry is what lets the operator differ per dialect:
    //
    //   * SQLite's `LIKE` is already case-insensitive for ASCII, so it
    //     matches the matcher exactly and needs no help.
    //   * Postgres's `LIKE` is case-sensitive, so it reads a
    //     pre-lowercased copy of the column instead. `lower()` under
    //     the database collation folds the full Unicode range, so the
    //     prefilter is broader than the matcher — which is the safe
    //     direction.
    //
    // The pre-lowercased column is also what makes this affordable on
    // Postgres, and the two obvious alternatives were measured first:
    // `ILIKE` on `content_text` costs 1,341 ms for `run` (the trigram
    // index is not even used, because `%run%` is one trigram present in
    // nearly every document, so a sequential scan wins and
    // case-insensitive matching is slow per byte), and
    // `LOWER(content_text) LIKE` costs 1,237 ms even for a rare term
    // because the function call hides the column from the planner.
    // Against `text_lower`, case-sensitive `LIKE` does `run` in 249 ms.
    //
    // # `CAST(?1 AS TEXT)`
    //
    // Not decoration. Postgres resolves a parameter's type from how it
    // is used, and `$1 IS NULL` says nothing — it rejects the statement
    // outright with "could not determine data type of parameter $1".
    // `CAST` tells it, and `CAST(x AS TEXT)` is the spelling both
    // engines accept; Postgres's own `?1::text` is a syntax error in
    // SQLite. The predicate had been written the uncast way since
    // before there was a Postgres backend to reject it.
    //
    // `?1` is the project filter so the per-term placeholders can be
    // numbered from 2 without the count depending on whether a project
    // was given.
    // The column the prefilter reads is the one thing here that is not
    // the same on both backends. `text_lower` is a generated column
    // that only the mirror's schema has; SQLite reads the original,
    // because its `LIKE` already folds case.
    let filter_col = match store.dialect() {
        Dialect::Sqlite => "cb.content_text",
        Dialect::Postgres => "cb.text_lower",
    };
    let mut params: Vec<Value> = vec![Value::from(project)];
    let mut clauses = String::new();
    for (i, term) in query.terms.iter().enumerate() {
        clauses.push_str(&format!(
            "\n            AND {filter_col} LIKE ?{} ESCAPE '\\'",
            i + 2
        ));
        // The term is a literal, not a pattern. `_` is in a third of
        // the identifiers in this corpus and LIKE would read it as "any
        // character", which is the same bug the README records for
        // `entity search` and `file ls`.
        //
        // On Postgres the pattern is additionally escaped the way the
        // *stored* text is, because `text_lower` is generated from the
        // escaped `content_text` — so a term containing U+0000 or
        // U+0001 has to be looked for in its stored form or the
        // prefilter silently drops the candidate. `pgtext::escape`
        // leaves every other term untouched, so this is a no-op for
        // real queries and correct for the ones it is not. The LIKE
        // escaping happens *after*, since U+0001 is not a LIKE
        // metacharacter and the order would not matter — but reading
        // it in one direction only is clearer.
        let term: &str = match store.dialect() {
            Dialect::Sqlite => term,
            Dialect::Postgres => &crate::pgtext::escape(term),
        };
        params.push(Value::from(format!("%{}%", escape_like(term))));
    }

    let rows = store
        .query(
            &format!(
                "SELECT p.slug, pf.file_path, cb.content_text
                   FROM file_contents fc
                   JOIN project_files pf ON pf.id = fc.file_id
                   JOIN projects p       ON p.id = pf.project_id
                   JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
                  WHERE fc.is_current = 1
                    AND pf.status = 'active'
                    AND cb.content_text IS NOT NULL
                    AND (CAST(?1 AS TEXT) IS NULL OR p.slug = ?1){clauses}
                  ORDER BY p.slug, pf.file_path"
            ),
            &params,
        )
        .context("searching file contents")?;

    let dropped = rows.len().saturating_sub(MAX_CANDIDATES);
    let corpus = corpus_stats(store)?;

    // Term frequencies and match positions per candidate, in one pass.
    struct Candidate {
        project: String,
        path: String,
        text: String,
        tf: Vec<usize>,
        spans: Vec<(usize, usize)>,
        chars: usize,
    }
    let mut candidates = Vec::with_capacity(rows.len().min(MAX_CANDIDATES));
    let mut df = vec![0usize; query.terms.len()];

    for row in rows.iter().take(MAX_CANDIDATES) {
        let text = row.get_string(2)?;
        let mut tf = Vec::with_capacity(query.terms.len());
        let mut spans = Vec::new();
        for (i, term) in query.terms.iter().enumerate() {
            let hits = occurrences(&text, term);
            tf.push(hits.len());
            if !hits.is_empty() {
                df[i] += 1;
            }
            spans.extend(hits.into_iter().map(|at| (at, at + term.len())));
        }
        // Not an edge case — this is where the prefilter being
        // *broader* than the matcher gets reconciled. Postgres `ILIKE`
        // folds the full Unicode range, so `search content ECOLE` hands
        // over a file containing `école`; the matcher folds ASCII only
        // and does not count it, so the file is dropped here and the
        // two backends agree. On SQLite the branch is unreachable.
        if tf.contains(&0) {
            continue;
        }
        spans.sort_unstable();
        candidates.push(Candidate {
            project: row.get_string(0)?,
            path: row.get_string(1)?,
            chars: text.chars().count(),
            text,
            tf,
            spans,
        });
    }

    // Score, then order by score descending with `(project, path)`
    // breaking ties. `sort_by` is stable and the candidate query
    // already returned them in `(slug, file_path)` order, so the
    // tie-break is implicit — but it is spelled out anyway, because
    // relying on a stable sort over an upstream `ORDER BY` is the kind
    // of invariant that survives until someone parallelises the loop.
    let mut scored: Vec<(f64, Candidate)> = candidates
        .into_iter()
        .map(|c| (bm25(&c.tf, &df, c.chars, corpus), c))
        .collect();
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.partial_cmp(sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| (&a.project, &a.path).cmp(&(&b.project, &b.path)))
    });

    let hits = scored
        .into_iter()
        .take(limit.max(0) as usize)
        .map(|(_, c)| SearchHit {
            project: c.project,
            path: c.path,
            snippet: snippet(&c.text, &c.spans),
        })
        .collect();
    Ok((hits, dropped))
}

/// Python's hardcoded `LIMIT 100` on `search files`, reproduced.
///
/// Unlike `search content`'s `--limit`, this is not a flag. Python's FTS
/// path applies no limit at all, which is what made a flag there the one
/// place this port is deliberately more capable; here Python *has* a
/// limit, so a flag would be a surface divergence bought for nothing.
const FILE_NAME_LIMIT: usize = 100;

/// Search file *paths*, reading no content.
///
/// Returns the hits in `project_files.id` order and a flag saying
/// whether the [`FILE_NAME_LIMIT`] cut the list short.
///
/// # The `ORDER BY` Python does not have
///
/// Python's query is `WHERE file_path LIKE ? LIMIT 100` with no
/// `ORDER BY` whatsoever, so *which* hundred rows it prints is the query
/// planner's choice. That is worse than an unstable order: it changes
/// which files you are told about. Measured 2026-11-17 over 55 patterns,
/// 48 of them matching more than 100 rows, SQLite returns exactly the
/// `ORDER BY id` sequence for every one of them — its plan is
/// `SCAN pf`, a rowid scan, and the view's leading table is
/// `project_files`. So `ORDER BY id` is what Python prints today, said
/// out loud.
///
/// Spelling it out is not tidiness. This port has two backends and the
/// sweep compares them against each other: a Postgres sequential scan
/// returns heap order, which after a `mirror` is insert order and after
/// anything else is not. Without a total key the two backends would
/// disagree about which hundred rows `LIMIT 100` keeps, and `id` is the
/// only total key this view carries that also reproduces Python.
///
/// # The pattern is escaped, and Python's is not
///
/// Python interpolates `f"%{pattern}%"` raw, so `_` means "any
/// character". Measured on the live corpus: 1,112 of the 2,780 paths
/// contain a literal underscore, and `search files get_` returns ten
/// rows of which **zero** contain `get_` — every hit is a wildcard
/// artefact. `set_` returns 321 against 5, `api_` 128 against 4. Same
/// bug `entity search` and the `file ls` prefix had, both since fixed
/// upstream; this is the copy that is still live. Escaping it is a
/// deliberate difference and the sweep carries a row for it.
pub fn search_files(
    store: &dyn Store,
    pattern: &str,
    project: Option<&str>,
) -> Result<(Vec<FileNameHit>, bool)> {
    // `?1` is the project filter, so the pattern's placeholder number
    // does not depend on whether one was given — the same arrangement
    // `search_content` uses, and `CAST(?1 AS TEXT)` for the same reason:
    // Postgres cannot infer a parameter's type from `$1 IS NULL` alone.
    //
    // One row past the cap is read so the caller can say the list was
    // cut, rather than a second `COUNT(*)` over the same predicate.
    //
    // # `LOWER(file_path)`, and why the pattern is ASCII-folded
    //
    // Python writes a bare `file_path LIKE ?` and gets
    // case-insensitive matching for free, because SQLite's `LIKE` folds
    // ASCII case. Postgres's does not. Left bare, this query found
    // `bza/NIX_FLAKE_GUIDE.md` for `search files flake` on SQLite and
    // not on Postgres — caught by the cross-backend sweep on
    // 2026-11-17, which is the failure that stage exists for and the
    // one a byte-diff against Python could never see, since Python has
    // only the one engine.
    //
    // `LOWER(col) LIKE <folded pattern>` is `entity search`'s shape and
    // it agrees on both: SQLite's `LOWER()` is ASCII-only, and so is
    // Postgres's on a `COLLATE "C"` column — measured, both return
    // `École_Ä.md` for `ÉCOLE_Ä.md`.
    //
    // The fold is `to_ascii_lowercase`, *not* `to_lowercase`, and that
    // is the one place this diverges from `entity search`. There Python
    // itself calls `.lower()`, so the port reproduces full Unicode
    // folding on the pattern against an ASCII-only `LOWER()` on the
    // column — a half-working arrangement the README records rather
    // than fixes. Here Python folds nothing, and SQLite's `LIKE` does
    // not fold non-ASCII either (`'ÉCOLE' LIKE '%école%'` is false), so
    // folding the pattern beyond ASCII would make this port match where
    // Python does not.
    let params = vec![
        Value::from(project),
        Value::from(format!("%{}%", escape_like(&pattern.to_ascii_lowercase()))),
    ];
    let rows = store
        .query(
            &format!(
                "SELECT id, project_slug, file_path, type_name, lines_of_code
                   FROM files_with_types_view
                  WHERE LOWER(file_path) LIKE ?2 ESCAPE '\\'
                    AND (CAST(?1 AS TEXT) IS NULL OR project_slug = ?1)
                  ORDER BY id
                  LIMIT {}",
                FILE_NAME_LIMIT + 1
            ),
            &params,
        )
        .context("searching file names")?;

    let truncated = rows.len() > FILE_NAME_LIMIT;
    let hits = rows
        .iter()
        .take(FILE_NAME_LIMIT)
        .map(|row| {
            Ok(FileNameHit {
                project: row.get_string(1)?,
                path: row.get_string(2)?,
                type_name: row.get_string(3)?,
                lines_of_code: row.get_opt_i64(4)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((hits, truncated))
}

/// Document count and total length over every file the search can see.
///
/// Corpus-wide rather than over the candidates, because `idf` computed
/// against the candidate set is zero whenever every candidate contains
/// every term — which is every single-term query.
///
/// `LENGTH()` counts characters in both engines, and the sum is `CAST`
/// to `BIGINT` so neither `AVG`'s return type nor its rounding enters
/// into it: SQLite's `AVG` is a float that `CAST` truncates, Postgres's
/// is a `numeric` that `CAST` rounds, and dividing in Rust avoids
/// having to care.
fn corpus_stats(store: &dyn Store) -> Result<CorpusStats> {
    let row = store
        .query_opt(
            "SELECT COUNT(*),
                    CAST(COALESCE(SUM(LENGTH(cb.content_text)), 0) AS BIGINT)
               FROM file_contents fc
               JOIN project_files pf ON pf.id = fc.file_id
               JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
              WHERE fc.is_current = 1
                AND pf.status = 'active'
                AND cb.content_text IS NOT NULL",
            &[],
        )
        .context("measuring the corpus")?;
    let Some(row) = row else {
        return Ok(CorpusStats {
            docs: 0,
            total_chars: 0,
        });
    };
    Ok(CorpusStats {
        docs: row.get_i64(0)?,
        total_chars: row.get_i64(1)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::testdb;

    fn hits(
        c: &crate::SqliteStore,
        pattern: &str,
        project: Option<&str>,
        limit: i64,
    ) -> Vec<(String, String)> {
        super::search_content(c, pattern, project, limit)
            .unwrap()
            .0
            .into_iter()
            .map(|h| (h.project, h.path))
            .collect()
    }

    /// The regression this port was built around: two projects own a
    /// `README.md`, only one contains the term. The original bug joined
    /// the FTS table on `file_path`, which is not unique across
    /// projects; this implementation never touches the FTS table at
    /// all, but the property still has to hold.
    #[test]
    fn hits_are_attributed_to_the_owning_project_only() {
        let c = testdb::fixture();
        assert_eq!(
            hits(&c, "distinctiveword", None, 100),
            [("alpha".to_string(), "README.md".to_string())]
        );
        // And the project filter must not pass for the project whose
        // README never contained the term.
        assert!(hits(&c, "distinctiveword", Some("beta"), 100).is_empty());
    }

    #[test]
    fn limit_is_applied() {
        let c = testdb::fixture();
        assert_eq!(hits(&c, "shared", None, 1).len(), 1);
        assert_eq!(hits(&c, "shared", None, 10).len(), 2);
    }

    /// Several terms are ANDed: a file must contain all of them.
    #[test]
    fn multiple_terms_all_have_to_match() {
        let c = testdb::fixture();
        // alpha's README has both words, beta's has only `shared`.
        assert_eq!(
            hits(&c, "distinctiveword shared", None, 10),
            [("alpha".to_string(), "README.md".to_string())]
        );
        assert!(hits(&c, "distinctiveword nosuchword", None, 10).is_empty());
    }

    /// Matching is substring, which is the deliberate break from FTS5.
    /// `distinctivewor` is a prefix of a word in the corpus and no
    /// token at all.
    #[test]
    fn matching_is_substring_not_tokenised() {
        let c = testdb::fixture();
        assert_eq!(hits(&c, "istinctivewor", None, 10).len(), 1);
    }

    /// Case-insensitive on both the query and the content.
    #[test]
    fn matching_folds_case() {
        let c = testdb::fixture();
        assert_eq!(hits(&c, "DISTINCTIVEWORD", None, 10).len(), 1);
    }

    /// A literal `_` must not act as a LIKE wildcard in the prefilter.
    /// The fixture has `fn main() {}` — `fn_main` would match it if the
    /// underscore were a wildcard.
    #[test]
    fn query_underscores_are_literal() {
        let c = testdb::fixture();
        assert!(hits(&c, "fn_main", None, 10).is_empty());
        assert_eq!(hits(&c, "fn main", None, 10).len(), 1);
    }

    /// Binary blobs have NULL `content_text` and must not be searched
    /// or crash the scan.
    #[test]
    fn binary_blobs_are_skipped() {
        let c = testdb::fixture();
        assert!(hits(&c, "PNG", None, 10).is_empty());
    }

    /// Deleted files are not searchable even though their content row
    /// is still current.
    #[test]
    fn deleted_files_are_not_searched() {
        let c = testdb::fixture();
        // `gone.txt` is status 'deleted' and shares alpha's main.rs
        // blob, so a hit on `second` must name only the active file.
        let found = hits(&c, "second", None, 10);
        assert_eq!(found, [("alpha".to_string(), "src/main.rs".to_string())]);
    }

    #[test]
    fn an_empty_query_is_an_error() {
        let c = testdb::fixture();
        assert!(super::search_content(&c, "   ", None, 10).is_err());
    }

    #[test]
    fn no_match_is_an_empty_list_not_an_error() {
        let c = testdb::fixture();
        assert!(hits(&c, "nothinghereatall", None, 10).is_empty());
    }

    /// Ranking has to put the file that mentions the term more first,
    /// and the snippet has to bracket it.
    #[test]
    fn results_are_ranked_and_bracketed() {
        let c = testdb::fixture();
        let out = super::search_content(&c, "shared", None, 10).unwrap().0;
        assert_eq!(out.len(), 2);
        // beta's README is `shared\n` (7 chars); alpha's is
        // `distinctiveword shared\n` (23). Same single mention, so the
        // shorter file scores higher under BM25's length normalisation.
        assert_eq!(out[0].project, "beta");
        for h in &out {
            assert!(h.snippet.contains("[shared]"), "{:?}", h.snippet);
        }
    }

    /// Nothing is dropped on a corpus this size, and the count is
    /// reported rather than inferred.
    #[test]
    fn nothing_is_silently_truncated() {
        let c = testdb::fixture();
        let (_, dropped) = super::search_content(&c, "shared", None, 10).unwrap();
        assert_eq!(dropped, 0);
    }

    mod files {
        use crate::testdb;

        fn paths(
            c: &crate::SqliteStore,
            pattern: &str,
            project: Option<&str>,
        ) -> Vec<(String, String)> {
            super::super::search_files(c, pattern, project)
                .unwrap()
                .0
                .into_iter()
                .map(|h| (h.project, h.path))
                .collect()
        }

        /// The order is `project_files.id`, which the fixture makes
        /// distinguishable from alphabetical on purpose: alpha's `.rs`
        /// files are ids 1, 13, 14, 15 for `src/main.rs`, `src/hot.rs`,
        /// `src/live.rs`, `src/pin.rs`, so id order and path order
        /// differ in the first element.
        #[test]
        fn results_are_ordered_by_id_not_by_path() {
            let c = testdb::fixture();
            let got: Vec<String> = paths(&c, ".rs", Some("alpha"))
                .into_iter()
                .map(|(_, p)| p)
                .collect();
            assert_eq!(
                got,
                ["src/main.rs", "src/hot.rs", "src/live.rs", "src/pin.rs"]
            );
        }

        /// The INNER JOIN on `file_types` is the live view's, and it
        /// means a file with no resolved type is not findable. beta's
        /// `README.md` has a NULL `file_type_id`; alpha's does not. No
        /// live row can show this — all 2,780 resolve a type.
        #[test]
        fn a_file_with_no_resolved_type_is_invisible() {
            let c = testdb::fixture();
            assert_eq!(
                paths(&c, "README", None),
                [("alpha".to_string(), "README.md".to_string())]
            );
            assert!(paths(&c, "README", Some("beta")).is_empty());
        }

        /// There is no `status` filter in the view, so a deleted file is
        /// listed. `file ls` excludes `gone.txt` and this does not — an
        /// upstream behaviour reproduced for parity, not endorsed.
        #[test]
        fn deleted_files_are_listed() {
            let c = testdb::fixture();
            assert_eq!(
                paths(&c, "gone", None),
                [("alpha".to_string(), "gone.txt".to_string())]
            );
        }

        /// `_` and `%` are literals here and wildcards in Python.
        /// `s_c` would match `src/` if the underscore were a wildcard,
        /// and `R%E` would match `README.md`.
        #[test]
        fn pattern_metacharacters_are_literal() {
            let c = testdb::fixture();
            assert!(paths(&c, "s_c", None).is_empty());
            assert!(paths(&c, "R%E", None).is_empty());
            // The same queries without the metacharacter do match, so
            // the empties above are the escaping and not a typo.
            assert_eq!(paths(&c, "src", Some("alpha")).len(), 4);
            assert_eq!(paths(&c, "RE", Some("alpha")).len(), 1);
        }

        /// The project filter is an exact slug: Python's `search`
        /// module never resolves one, so an unmatched name selects
        /// nothing rather than erroring.
        #[test]
        fn an_unmatched_project_selects_nothing_and_is_not_an_error() {
            let c = testdb::fixture();
            assert!(paths(&c, "README", Some("alph")).is_empty());
            assert!(paths(&c, "README", Some("no-such-project")).is_empty());
        }

        /// The unreferenced `file_types` row must not widen anything —
        /// the join is driven from `project_files`.
        #[test]
        fn an_unreferenced_file_type_adds_no_rows() {
            let c = testdb::fixture();
            // alpha's seven files all resolve a type; beta's and
            // empty's resolve none. So the whole view is seven rows and
            // `''` matches every one of them.
            assert_eq!(paths(&c, "", None).len(), 7);
        }

        /// `lines_of_code` survives as NULL rather than being
        /// coalesced, because Python prints the word `None`.
        #[test]
        fn a_missing_line_count_stays_null() {
            let c = testdb::fixture();
            let hits = super::super::search_files(&c, "README.md", Some("alpha"))
                .unwrap()
                .0;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].lines_of_code, None);
            assert_eq!(hits[0].type_name, "markdown");
        }

        #[test]
        fn no_match_is_an_empty_list_not_an_error() {
            let c = testdb::fixture();
            assert!(paths(&c, "nothinghereatall", None).is_empty());
        }

        /// ASCII case is folded, which is what SQLite's bare `LIKE`
        /// gives Python and what `LOWER()` has to give Postgres. Both
        /// directions are here because the fixture has both cases:
        /// `src/main.rs` is lower and `README.md` is upper, so one
        /// query folds down to the column and the other folds the
        /// column down to it.
        #[test]
        fn ascii_case_is_folded_in_both_directions() {
            let c = testdb::fixture();
            assert_eq!(paths(&c, "MAIN.RS", Some("alpha")).len(), 1);
            assert_eq!(paths(&c, "readme.md", Some("alpha")).len(), 1);
            assert_eq!(paths(&c, "ReAdMe", Some("alpha")).len(), 1);
        }

        /// The cap is not reached on a seven-row fixture, and the flag
        /// says so rather than being left to be inferred.
        #[test]
        fn the_cap_is_reported_and_is_not_reached_here() {
            let c = testdb::fixture();
            let (hits, truncated) = super::super::search_files(&c, "", None).unwrap();
            assert_eq!(hits.len(), 7);
            assert!(!truncated);
        }
    }
}
