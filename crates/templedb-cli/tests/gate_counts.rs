//! Do the numbers README.md prints still describe this tree?
//!
//! # The gap this closes
//!
//! *Known gaps* has carried **nothing in the gate counts tests** since
//! 2026-11-17, and it is the second instance of the shape the entry
//! above it names first: a check that exists and is not wired in checks
//! nothing. The evidence is four wrong numbers in a row. The *Status*
//! paragraph said 393 and the figure was 409; step 29's entry said 411
//! and the figure was 409; step 30 said 429, which was right when it
//! was written and was 438 a day later, because step 31 added nine unit
//! tests in `intent.rs` and corrected the *total* without revisiting
//! the breakdown it was a breakdown of. None of the four was off by
//! much. That is the point rather than a mitigation: a number nothing
//! checks decays at whatever rate the tree changes, and the reader has
//! no way to tell a current one from a stale one.
//!
//! So this counts, inside `cargo test`, with no database and no
//! interpreter, and compares what it counted against what the README
//! says. It counts itself too, which is the only way the number for
//! this file can be right.
//!
//! # Why it reads the prose, and not a declaration
//!
//! The same argument `sweep_coverage.rs` makes about the sweep's
//! arrays: parse the authoritative artifact rather than keep a second
//! copy of it, because a test carrying its own expected values passes
//! while the artifact says something else. Here the artifact is the
//! prose. A machine-readable block of counts in an HTML comment would
//! be easier to parse and would check the wrong thing — nobody reads
//! the comment, and the sentence a reader believes would go on
//! decaying beside it.
//!
//! The cost is that the anchors below are fragments of English, so
//! rewording a sentence breaks a test. That failure is deliberate and
//! the message says what to do: a claim that moves has to be re-aimed,
//! and the alternative is a claim that quietly stops being checked.
//! One convention follows from it, and it is why this paragraph does
//! not say "five": **a gate-checked number is written as a numeral**,
//! because the parser looks for digits.
//!
//! # Why only the prose above *Porting order*
//!
//! Everything below that heading is a step entry, and a step entry is a
//! measurement taken on a date. Step 22's "351 unit tests" is not a
//! claim about this tree and must not be rewritten to agree with it —
//! this README's method is to leave a superseded number in place with
//! the correction beside it, and a gate that forced those numbers
//! forward would destroy the record it is supposed to protect. So the
//! region this reads stops at the heading, and the claims it checks are
//! the present-tense ones.
//!
//! # The counting rule, and the one it replaces
//!
//! A test is a line whose trimmed text is exactly `#[test]`. That
//! reproduces `cargo test --workspace -- --list | grep -c ': test'`
//! exactly — 502 on 2026-11-17, before this file existed, and per
//! target as well as in total: 381 library tests in `templedb-db`, 51
//! in `templedb-cli`, and 6 + 59 + 3 + 2 across the four integration
//! targets.
//!
//! The method step 31 used by hand does not reproduce it. A plain
//! `grep -c '#\[test\]'` counts 503, and the extra one is three
//! paragraphs of `sweep_coverage.rs`'s own module doc saying "the sweep
//! should become a `#[test]`, and it cannot". Step 31 reported that
//! count as agreeing with its delta; it was one high, for the reason
//! that a file about testing talks about tests. Hence the attribute has
//! to be alone on its line, and hence the sample in
//! [`the_counting_rule_is_the_attribute_alone_on_its_line`] has a
//! mention in it.
//!
//! # What this still does not check
//!
//! The weaker claim, stated here rather than left to be inferred from a
//! passing test:
//!
//! - **The sweep's invocation count.** Three of its rows are discovered
//!   at run time by a search of the live database, so "101
//!   invocations" is not a function of this tree and cannot be
//!   asserted from it. The sweep prints the number it actually ran.
//! - **Every count describing a database.** Mirrored tables, mirrored
//!   columns, digested rows, "32 of the 1,346 live `created_at`
//!   values" — these describe a 483 MB SQLite file, and a hermetic
//!   test has no access to it and no business asserting about it.
//! - **What a test covers.** `#[test]` counting is counting, exactly as
//!   `sweep_coverage.rs` checks that a row exists and not that it is a
//!   good row.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The repository root, from this crate's manifest.
///
/// The same mechanism `sweep_coverage.rs` uses to find the sweep
/// script, and it works inside `nix flake check` for the same reason:
/// the flake's source filter drops only `target/`, so README.md and
/// both crates are in the sandbox.
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");

/// Which end of the anchor the number sits on.
#[derive(Clone, Copy)]
enum Side {
    /// `438 tests run by ...` — the digits end where the anchor begins.
    Before,
    /// `unit tests (438)` — the digits begin where the anchor ends.
    After,
}

/// What a claimed number is a count of.
#[derive(Clone, Copy)]
enum Of {
    /// Every test `cargo test` runs by default: all targets, less the
    /// `#[ignore]`d ones.
    DefaultRun,
    /// Every `#[test]` in these targets, `#[ignore]`d or not.
    AllIn(&'static [&'static str]),
    /// Only the `#[ignore]`d ones in these targets.
    IgnoredIn(&'static [&'static str]),
}

/// One number in the README, and what the tree says it should be.
struct Claim {
    /// The English immediately beside the number, after whitespace
    /// normalisation. Must occur exactly once in the region.
    anchor: &'static str,
    side: Side,
    of: Of,
}

/// A test target, named as this file names it: a crate's `src/` tree
/// collapses to the crate name, and an integration test is its file
/// stem.
const DB: &str = "templedb-db";
const CLI: &str = "templedb-cli";

/// The total: every test `cargo test` runs without `--ignored`.
const TOTAL: &str = " tests run by `cargo test`";

/// The four anchors that must add up to [`TOTAL`], in the order the
/// sentence states them.
///
/// Named separately because they have a second job: *as prose* they are
/// a breakdown, and a breakdown has to sum. Step 30's figure was
/// consistent that way and stale against the tree; step 31's was
/// current in the total and stale in the parts. The two checks catch
/// different halves and neither subsumes the other.
const BREAKDOWN: &[&str] = &[
    " in `templedb-db`'s library",
    " in `templedb-cli`'s,",
    " in `tests/sweep_coverage.rs`",
    " in `tests/gate_counts.rs`",
];

/// Every present-tense count in README.md that is a function of this
/// source tree.
const CLAIMS: &[Claim] = &[
    // --- the Status paragraph -----------------------------------------
    Claim {
        anchor: TOTAL,
        side: Side::Before,
        of: Of::DefaultRun,
    },
    Claim {
        anchor: BREAKDOWN[0],
        side: Side::Before,
        of: Of::AllIn(&[DB]),
    },
    Claim {
        anchor: BREAKDOWN[1],
        side: Side::Before,
        of: Of::AllIn(&[CLI]),
    },
    Claim {
        anchor: BREAKDOWN[2],
        side: Side::Before,
        of: Of::AllIn(&["sweep_coverage"]),
    },
    Claim {
        anchor: BREAKDOWN[3],
        side: Side::Before,
        of: Of::AllIn(&["gate_counts"]),
    },
    Claim {
        anchor: " more are `#[ignore]`d because they need `python3`",
        side: Side::Before,
        of: Of::IgnoredIn(&["difflib_parity", "clock_parity", "classify_parity"]),
    },
    Claim {
        anchor: " drive CPython's `difflib`",
        side: Side::Before,
        of: Of::AllIn(&["difflib_parity"]),
    },
    Claim {
        anchor: " drive CPython's `sqlite3`",
        side: Side::Before,
        of: Of::AllIn(&["clock_parity"]),
    },
    // The third oracle, 2026-11-19. `no_test_target_is_unaccounted_for`
    // is what required this entry rather than a reader noticing: adding
    // `classify_parity.rs` made that test fail by name, which is the
    // check working as the *Known gaps* entry above described it.
    Claim {
        anchor: " drive CPython's `importer.scanner`",
        side: Side::Before,
        of: Of::AllIn(&["classify_parity"]),
    },
    Claim {
        anchor: " more are `#[ignore]`d because they need a Postgres",
        side: Side::Before,
        of: Of::IgnoredIn(&["write_parity"]),
    },
    // --- the Checks section's split table -----------------------------
    //
    // The same total, stated a second time 1,800 lines away. It was
    // 296 on 2026-11-17 against an actual 438, having been written when
    // the first number was right — which is the whole argument for
    // checking both statements rather than one.
    Claim {
        anchor: "unit tests (",
        side: Side::After,
        of: Of::DefaultRun,
    },
];

/// The `#[test]` and `#[ignore]` attributes in one target.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct Counts {
    tests: usize,
    ignored: usize,
}

impl Counts {
    fn add(&mut self, other: Counts) {
        self.tests += other.tests;
        self.ignored += other.ignored;
    }
}

/// Count the attributes in one file's text.
///
/// The rule is in this function and nowhere else: a line that trims to
/// exactly `#[test]`, and a line that trims to something starting
/// `#[ignore` — which covers both the bare form and
/// `#[ignore = "reason"]`. A doc comment or a string mentioning either
/// one has other characters on the line and is not counted.
fn count_attrs(text: &str) -> Counts {
    let mut counts = Counts::default();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "#[test]" {
            counts.tests += 1;
        } else if trimmed.starts_with("#[ignore") {
            counts.ignored += 1;
        }
    }
    counts
}

/// Every `.rs` file under a directory.
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("a directory entry").path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// Counts per test target.
///
/// A crate's whole `src/` tree is one entry, because each crate here
/// has exactly one target in it — `templedb-db` a lib, `templedb-cli` a
/// bin. A crate holding both would make its entry the sum of two
/// targets, and the README's wording would have to say so; nothing here
/// detects that, and it is recorded rather than guarded because the
/// `Cargo.toml` parse it would need is more machinery than the mistake
/// is worth.
fn counts_by_target() -> BTreeMap<String, Counts> {
    let crates_dir = Path::new(ROOT).join("crates");
    let mut crate_dirs: Vec<PathBuf> = std::fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", crates_dir.display()))
        .map(|e| e.expect("a directory entry").path())
        .filter(|p| p.is_dir())
        .collect();
    crate_dirs.sort();

    let mut out: BTreeMap<String, Counts> = BTreeMap::new();
    for dir in crate_dirs {
        let crate_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .expect("a crate directory name")
            .to_string();

        let src = dir.join("src");
        if src.is_dir() {
            let mut files = Vec::new();
            walk(&src, &mut files);
            files.sort();
            let mut acc = Counts::default();
            for file in &files {
                acc.add(count_attrs(&read(file)));
            }
            assert!(
                out.insert(crate_name.clone(), acc).is_none(),
                "two targets named {crate_name}"
            );
        }

        let tests = dir.join("tests");
        if !tests.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        walk(&tests, &mut files);
        files.sort();
        for file in &files {
            let target = file
                .file_stem()
                .and_then(|n| n.to_str())
                .expect("a test file stem")
                .to_string();
            assert!(
                out.insert(target.clone(), count_attrs(&read(file)))
                    .is_none(),
                "two targets named {target}"
            );
        }
    }
    out
}

/// README.md down to `## Porting order`, with every run of whitespace
/// collapsed to one space.
///
/// The normalisation is what makes an anchor a sentence rather than a
/// line: this README is hard-wrapped at about 70 columns, so `51 in
/// \`templedb-cli\`` has a newline in the middle of it and a literal
/// search for the phrase finds nothing.
fn readme_region() -> String {
    let path = Path::new(ROOT).join("README.md");
    let text = read(&path);
    let heading = "\n## Porting order";
    let end = text.find(heading).unwrap_or_else(|| {
        panic!(
            "no `{}` heading in README.md. This test reads the prose above it and \
             deliberately ignores the step entries below it — see this file's module doc.",
            heading.trim()
        )
    });
    let region = text[..end].split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        region.len() > 20_000,
        "the README region parsed to {} bytes, which is too short to be the \
         Status and Checks sections — a region that parsed to nothing would \
         fail every claim below for the wrong reason",
        region.len()
    );
    region
}

/// Where the anchor is, insisting there is exactly one.
///
/// Two occurrences would make the number checked depend on which one
/// came first in the file, which is the kind of accident this test
/// exists to catch rather than commit.
fn locate(region: &str, anchor: &str) -> usize {
    let hits: Vec<usize> = region.match_indices(anchor).map(|(i, _)| i).collect();
    assert_eq!(
        hits.len(),
        1,
        "the anchor {anchor:?} occurs {} times in README.md above `## Porting order`, \
         and a claim has to name exactly one place.\n\
         Zero means the sentence was reworded — re-aim the anchor in CLAIMS.\n\
         Two or more means the phrase is no longer distinctive — lengthen it.",
        hits.len()
    );
    hits[0]
}

/// The number the README states beside an anchor.
///
/// Digits and `,` only, both one byte, so the byte arithmetic is safe
/// on a region that otherwise holds multibyte prose.
fn claimed(region: &str, anchor: &str, side: Side) -> usize {
    let at = locate(region, anchor);
    let digits: String = match side {
        Side::Before => {
            let head = &region[..at];
            let taken = head
                .chars()
                .rev()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .count();
            head[head.len() - taken..].to_string()
        }
        Side::After => {
            let tail = &region[at + anchor.len()..];
            let taken = tail
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == ',')
                .count();
            tail[..taken].to_string()
        }
    };
    digits
        .replace(',', "")
        .parse()
        .unwrap_or_else(|_| panic!("no number beside {anchor:?} in README.md"))
}

/// What the source tree says a claim's number should be.
fn measured(by_target: &BTreeMap<String, Counts>, of: Of) -> usize {
    let get = |target: &str| -> Counts {
        *by_target.get(target).unwrap_or_else(|| {
            panic!(
                "no test target named {target:?}. Known: {:?}",
                by_target.keys().collect::<Vec<_>>()
            )
        })
    };
    match of {
        Of::DefaultRun => by_target.values().map(|c| c.tests - c.ignored).sum(),
        Of::AllIn(targets) => targets.iter().map(|t| get(t).tests).sum(),
        Of::IgnoredIn(targets) => targets.iter().map(|t| get(t).ignored).sum(),
    }
}

/// Every claim names exactly one place in the prose.
#[test]
fn every_claim_appears_exactly_once_in_the_readme() {
    let region = readme_region();
    for claim in CLAIMS {
        locate(&region, claim.anchor);
    }
}

/// Every number matches what the tree holds.
///
/// The test the *Known gaps* entry asks for. It reports all the
/// mismatches at once rather than the first, because the breakdown
/// moves together: adding a test to `intent.rs` changes two of these
/// numbers and failing on one of them would hide the other.
#[test]
fn every_readme_count_matches_what_the_source_holds() {
    let region = readme_region();
    let by_target = counts_by_target();
    let mut wrong = Vec::new();
    for claim in CLAIMS {
        let says = claimed(&region, claim.anchor, claim.side);
        let is = measured(&by_target, claim.of);
        if says != is {
            wrong.push(format!(
                "README says {says} beside {:?}, tree holds {is}",
                claim.anchor
            ));
        }
    }
    assert!(
        wrong.is_empty(),
        "README.md's counts no longer describe this tree:\n  {}\n\
         Recount with `cargo test --workspace -- --list | grep -c ': test'` if you \
         want the total a second way, then fix the prose. Per-target counts are:\n  {:?}",
        wrong.join("\n  "),
        by_target
    );
}

/// The breakdown adds up to the total it is a breakdown of.
///
/// Prose against prose, no tree involved, and the one check that would
/// have caught step 30's figure the day it was written: 429 was 372 +
/// 51 + 6 and the parts were each measured, so the sum was consistent
/// and wrong together. This catches the other half — a total edited
/// without its parts, which is what step 31 did.
#[test]
fn the_readme_breakdown_sums_to_its_own_total() {
    let region = readme_region();
    let total = claimed(&region, TOTAL, Side::Before);
    let parts: usize = BREAKDOWN
        .iter()
        .map(|anchor| claimed(&region, anchor, Side::Before))
        .sum();
    assert_eq!(
        total, parts,
        "the README's total and its breakdown disagree: {total} against {parts}. \
         Both are prose, so this is a half-finished edit rather than drift."
    );
}

/// Every target in the tree is counted by some claim.
///
/// The direction an exemption list cannot check by being consulted, and
/// the same one `sweep_coverage.rs`'s second test covers: a new
/// integration test file would otherwise be invisible here, and the
/// total would silently stop being the total.
#[test]
fn no_test_target_is_unaccounted_for() {
    let by_target = counts_by_target();
    let mut named = Vec::new();
    for claim in CLAIMS {
        match claim.of {
            Of::AllIn(targets) | Of::IgnoredIn(targets) => named.extend_from_slice(targets),
            Of::DefaultRun => {}
        }
    }
    let unaccounted: Vec<&String> = by_target
        .iter()
        .filter(|(target, counts)| counts.tests > 0 && !named.contains(&target.as_str()))
        .map(|(target, _)| target)
        .collect();
    assert!(
        unaccounted.is_empty(),
        "these test targets hold tests and no README claim counts them: {unaccounted:?}.\n\
         Add the count to the Status paragraph and a Claim here, or the total stops \
         being a total."
    );
}

/// The walk found the tree it was pointed at.
///
/// A walk that found nothing would make every measured count 0, and
/// `every_readme_count_matches_what_the_source_holds` would report ten
/// confident mismatches with the README as the suspect. Fail here
/// instead, where the message names the cause.
#[test]
fn the_source_walk_finds_both_crates_and_every_integration_target() {
    let by_target = counts_by_target();
    for expected in [
        DB,
        CLI,
        "sweep_coverage",
        "gate_counts",
        "write_parity",
        "difflib_parity",
        "clock_parity",
    ] {
        let counts = by_target
            .get(expected)
            .unwrap_or_else(|| panic!("no target {expected:?} in {:?}", by_target.keys()));
        assert!(counts.tests > 0, "{expected} parsed to zero tests");
    }
    let total: usize = by_target.values().map(|c| c.tests).sum();
    assert!(
        total > 400,
        "{total} tests found in total, which is too few to be this tree"
    );
}

/// No target reports more ignored tests than tests.
///
/// Cheap, and it is the invariant that fails if the two rules ever stop
/// describing the same attribute pair — an `#[ignore]` counted from a
/// line the `#[test]` rule rejects.
#[test]
fn no_target_has_more_ignored_tests_than_tests() {
    for (target, counts) in counts_by_target() {
        assert!(
            counts.ignored <= counts.tests,
            "{target} has {} `#[ignore]` attributes and {} `#[test]` ones",
            counts.ignored,
            counts.tests
        );
    }
}

/// The counting rule is the attribute alone on its line.
///
/// A unit test of the mechanism everything above depends on, and the
/// case it exists for is the third line: step 31's hand count was one
/// high because `sweep_coverage.rs` *writes about* `#[test]` in its
/// module doc.
#[test]
fn the_counting_rule_is_the_attribute_alone_on_its_line() {
    const SAMPLE: &str = concat!(
        "    #[test]\n",
        "    fn indented_counts() {}\n",
        "//! a doc comment mentioning `#[test]`, which does not\n",
        "#[test]\n",
        "#[ignore = \"needs a server\"]\n",
        "fn ignored_counts_as_both() {}\n",
        "// #[test] commented out, which does not\n",
        "let s = \"#[test]\";\n",
    );
    assert_eq!(
        count_attrs(SAMPLE),
        Counts {
            tests: 2,
            ignored: 1
        }
    );
}
