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
}
