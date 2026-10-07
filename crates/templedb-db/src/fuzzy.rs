//! Project name resolution, ported from `src/cli/fuzzy_matcher.py`.
//!
//! Every Python command that takes a project accepts a fuzzy name, so a
//! port that demands exact slugs is not a replacement — `templedb file
//! cat templedb src/x.py` works there and must work here.
//!
//! The scoring function is reproduced exactly, because the scores decide
//! which of several candidates wins and a reimplementation that is
//! merely "similar" picks a different project. Two things around it are
//! *not* reproduced, because they are bugs:
//!
//! 1. **Self-ambiguity.** Python indexes each project under two keys,
//!    `slug` and `"{name} ({slug})"`, then declares failure whenever
//!    more than one *key* matches — without checking whether the keys
//!    name the same project. So `qa-run` (exactly one project:
//!    `qa-runner`) is rejected as ambiguous because it matches both
//!    `qa-runner` and `QA Runner (qa-runner)`. Eleven of the thirty
//!    projects here have `name != slug` and are therefore reachable only
//!    by their exact slug, which is the opposite of what fuzzy matching
//!    is for. This module collapses candidates by project id before
//!    deciding ambiguity.
//!
//! 2. **`None` as a searchable name.** The second key is built with an
//!    f-string, so a project with a NULL `name` gets indexed under the
//!    literal `"None (slug)"`. `templedb source revisions None
//!    flake.nix` resolves to `templedb-backup-service` today. Here a
//!    missing name contributes no key.

use crate::projects::{list_projects, Project};
use anyhow::Result;
use rusqlite::Connection;

/// Python's `FuzzyMatcher.simple_score`, reproduced including the odd
/// bits (the `0.8 + ratio * 0.15` band can exceed the 0.95 reserved for
/// a case-insensitive exact match; that is how the original behaves and
/// it only ever reorders candidates relative to each other).
///
/// Lengths and the match position are counted in characters rather than
/// bytes, which is what Python's `len`/`str.index` do. On ASCII slugs
/// the two agree; on anything else, bytes would score differently.
pub fn simple_score(pattern: &str, candidate: &str) -> f64 {
    let p = pattern.to_lowercase();
    let c = candidate.to_lowercase();

    let Some(byte_pos) = c.find(&p) else {
        return 0.0;
    };
    if pattern == candidate {
        return 1.0;
    }
    if p == c {
        return 0.95;
    }
    let cand_chars = candidate.chars().count() as f64;
    if c.starts_with(&p) {
        let length_ratio = pattern.chars().count() as f64 / cand_chars;
        return 0.8 + length_ratio * 0.15;
    }
    // `byte_pos` indexes the lowercased haystack; convert to a character
    // offset the way `str.index` reports it.
    let char_pos = c[..byte_pos].chars().count() as f64;
    let position_score = 1.0 - char_pos / cand_chars;
    0.4 + position_score * 0.2
}

const MIN_SCORE: f64 = 0.1;
const MAX_RESULTS: usize = 10;

/// Outcome of resolving a user-supplied project name.
#[derive(Debug, Clone)]
pub enum ProjectLookup {
    Found(Project),
    NotFound,
    /// More than one distinct project matched. Carries the display
    /// strings and scores so the caller can print the same ●/○ list the
    /// Python version does.
    Ambiguous(Vec<(String, f64)>),
}

/// Resolve `pattern` to a single project by exact slug, then by score.
pub fn match_project(conn: &Connection, pattern: &str) -> Result<ProjectLookup> {
    // Candidates are built in slug order, not the `lines DESC` order
    // `list_projects` returns, so that equal-scoring ties break the same
    // way on every run. Python relies on its repository's ordering for
    // this and does not document it.
    let mut projects = list_projects(conn)?;
    projects.sort_by(|a, b| a.slug.cmp(&b.slug));

    // Exact slug wins outright and silently.
    if let Some(p) = projects.iter().find(|p| p.slug == pattern) {
        return Ok(ProjectLookup::Found(p.clone()));
    }

    let mut scored: Vec<(f64, usize, String, usize)> = Vec::new();
    for (idx, p) in projects.iter().enumerate() {
        let mut keys = vec![p.slug.clone()];
        // A NULL name contributes nothing. See the module note.
        if let Some(name) = p.name.as_deref() {
            if name != p.slug {
                keys.push(format!("{name} ({})", p.slug));
            }
        }
        for key in keys {
            let score = simple_score(pattern, &key);
            if score >= MIN_SCORE {
                let len = key.chars().count();
                scored.push((score, len, key, idx));
            }
        }
    }

    // Python: sort by (-score, len(value)), stable, then truncate to 10.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    scored.truncate(MAX_RESULTS);

    // Collapse to distinct projects. This is the fix: two keys for one
    // project are one match, not an ambiguity.
    let mut distinct: Vec<usize> = Vec::new();
    for (_, _, _, idx) in &scored {
        if !distinct.contains(idx) {
            distinct.push(*idx);
        }
    }

    match distinct.len() {
        0 => Ok(ProjectLookup::NotFound),
        1 => Ok(ProjectLookup::Found(projects[distinct[0]].clone())),
        _ => Ok(ProjectLookup::Ambiguous(
            scored.into_iter().map(|(s, _, key, _)| (key, s)).collect(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    /// Pin the scoring bands against values computed from the Python
    /// source. If these drift, the two implementations pick different
    /// projects for the same input.
    #[test]
    fn score_matches_python_bands() {
        assert_eq!(simple_score("alpha", "alpha"), 1.0);
        assert_eq!(simple_score("Alpha", "alpha"), 0.95);
        assert_eq!(simple_score("zzz", "alpha"), 0.0);
        // prefix: 0.8 + (3/5)*0.15
        assert!((simple_score("alp", "alpha") - 0.89).abs() < 1e-12);
        // contains at char 1 of 5: 0.4 + (1 - 1/5)*0.2
        assert!((simple_score("lph", "alpha") - 0.56).abs() < 1e-12);
    }

    /// Character, not byte, lengths — `len("é") == 1` in Python.
    #[test]
    fn score_counts_characters() {
        // "éx" is 3 bytes but 2 characters; prefix band = 0.8 + 1/2*0.15
        assert!((simple_score("é", "éx") - 0.875).abs() < 1e-12);
    }

    #[test]
    fn exact_slug_wins_without_scoring() {
        let c = testdb::fixture();
        let m = match_project(&c, "beta").unwrap();
        assert!(matches!(&m, ProjectLookup::Found(p) if p.slug == "beta"));
    }

    /// The Python self-ambiguity bug. `alph` matches both `alpha` and
    /// `Alpha Project (alpha)`; those are one project, so it resolves.
    #[test]
    fn two_keys_for_one_project_are_not_ambiguous() {
        let c = testdb::fixture();
        let m = match_project(&c, "alph").unwrap();
        match m {
            ProjectLookup::Found(p) => assert_eq!(p.slug, "alpha"),
            other => panic!("expected a unique match, got {other:?}"),
        }
    }

    /// Genuine ambiguity still reports as such.
    #[test]
    fn two_projects_are_ambiguous() {
        let c = testdb::fixture();
        // 'e' appears in alpha, beta and empty.
        match match_project(&c, "e").unwrap() {
            ProjectLookup::Ambiguous(v) => assert!(v.len() >= 2, "{v:?}"),
            other => panic!("expected ambiguity, got {other:?}"),
        }
    }

    /// A NULL `name` must not be searchable as the string "None".
    #[test]
    fn null_name_contributes_no_candidate() {
        let c = testdb::fixture();
        assert!(matches!(
            match_project(&c, "None").unwrap(),
            ProjectLookup::NotFound
        ));
    }

    #[test]
    fn unmatched_pattern_is_not_found() {
        let c = testdb::fixture();
        assert!(matches!(
            match_project(&c, "zzzz").unwrap(),
            ProjectLookup::NotFound
        ));
    }
}
