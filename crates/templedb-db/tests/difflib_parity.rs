//! Differential test of [`templedb_db::unified_diff`] against CPython's
//! `difflib`, which is the implementation it exists to match.
//!
//! # Why this is a separate test and `#[ignore]`d
//!
//! It shells out to `python3`. `cargo test` must pass on a machine
//! without a Python interpreter, so this does not run by default:
//!
//! ```bash
//! cargo test -p templedb-db --test difflib_parity -- --ignored
//! ```
//!
//! # Why fuzzing and not a table of cases
//!
//! The unit tests in `difflib.rs` pin the behaviours that were
//! understood well enough to write down. The risk with reimplementing
//! an algorithm from its source is the behaviour nobody thought to write
//! down — a tie-break that only shows up when two alignments are equally
//! long, an autojunk threshold crossed only by a file over 200 lines,
//! a hunk boundary that lands exactly on `2n`. A table of cases tests
//! the author's imagination. Generating thousands of edit pairs and
//! asking CPython what it would print tests the algorithm.
//!
//! The generator is deliberately hostile in three specific ways, each
//! aimed at a part of the algorithm that small random input will not
//! reach:
//!
//! - **A skewed alphabet.** Lines are drawn so a handful recur
//!   constantly — `""`, `"}"`, `"pass"` — because autojunk only fires
//!   on an element occupying more than 1% of positions, and uniformly
//!   random lines never do.
//! - **Sizes that straddle 200.** 199, 200 and 201 lines are all
//!   generated, because that is where autojunk switches on and the two
//!   implementations would otherwise agree on every input below it.
//! - **Clustered and distant edits.** Hunk grouping turns on a run of
//!   unchanged lines longer than `2n`, so edits are placed both close
//!   enough to merge and far enough apart to split.

use std::io::Write;
use std::process::{Command, Stdio};

/// Sentinels for the wire format. Generated lines are drawn from
/// [`ALPHABET`], which contains neither, so they cannot collide.
const CASE: &str = "===CASE===";
const SEP: &str = "===SEP===";

/// The oracle. Mirrors templedb's call exactly: `splitlines()` without
/// keepends, `lineterm=''`, `n=3`, and difflib's defaults for `isjunk`
/// (none) and `autojunk` (on).
const ORACLE: &str = r#"
import sys, difflib
CASE, SEP = "===CASE===", "===SEP==="
blob = sys.stdin.read()
cases = blob.split(CASE + "\n")[1:]
out = []
for case in cases:
    a_text, b_text = case.split(SEP + "\n")
    # The trailing newline the harness adds to delimit the section is
    # not part of the content; splitlines ignores it either way.
    a, b = a_text.splitlines(), b_text.splitlines()
    out.append(list(difflib.unified_diff(a, b, fromfile="old",
                                         tofile="new", lineterm="")))
for lines in out:
    sys.stdout.write(CASE + "\n")
    for l in lines:
        sys.stdout.write(l + "\n")
"#;

/// Lines the generator draws from.
///
/// Weighted by repetition in the list rather than by a separate weight
/// table: `""` and `"}"` appear many times so they are drawn often, which
/// is what pushes them over autojunk's 1% threshold on a long sequence.
/// The last few are deliberately exotic — a CR and a form feed are line
/// boundaries to Python's `splitlines` and not to Rust's `lines`, so
/// their presence tests [`templedb_db::splitlines`] against the real
/// thing rather than against its docstring.
const ALPHABET: &[&str] = &[
    "",
    "",
    "",
    "",
    "}",
    "}",
    "}",
    "pass",
    "pass",
    "return",
    "{",
    "if x:",
    "else:",
    "import os",
    "def f():",
    "    x = 1",
    "    y = 2",
    "# comment",
    "alpha",
    "beta",
    "gamma",
    "delta",
    "epsilon",
    "zeta",
    "x",
    "y",
    "z",
    "héllo",
    "wörld",
    "\ttabbed",
    "a\rb",
    "page\x0cbreak",
];

/// Deterministic PRNG. A fixed seed means a failure is reproducible and
/// a passing run is not luck that varies per machine; `rand` would be a
/// dependency for sixteen bits of arithmetic.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        // Numerical Recipes' constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// One `(a, b)` pair of file texts.
fn generate(rng: &mut Lcg, len: usize, edits: usize, clustered: bool) -> (String, String) {
    let a: Vec<&str> = (0..len)
        .map(|_| ALPHABET[rng.below(ALPHABET.len())])
        .collect();
    let mut b = a.clone();

    // Edits are applied back to front so earlier indices stay valid.
    let mut sites: Vec<usize> = if clustered {
        // All within a short span, so the hunks merge.
        let base = if len > 20 { rng.below(len - 20) } else { 0 };
        (0..edits)
            .map(|_| base + rng.below(20.min(len.max(1))))
            .collect()
    } else {
        (0..edits).map(|_| rng.below(len.max(1))).collect()
    };
    sites.sort_unstable();
    sites.reverse();

    for site in sites {
        if b.is_empty() {
            break;
        }
        let at = site.min(b.len() - 1);
        match rng.below(3) {
            0 => {
                b.remove(at);
            }
            1 => b.insert(at, ALPHABET[rng.below(ALPHABET.len())]),
            _ => b[at] = ALPHABET[rng.below(ALPHABET.len())],
        }
    }
    (join(&a), join(&b))
}

fn join(lines: &[&str]) -> String {
    let mut s = String::new();
    for l in lines {
        s.push_str(l);
        s.push('\n');
    }
    s
}

/// Ask CPython for the diff of every case, in order.
fn oracle(cases: &[(String, String)]) -> Vec<Vec<String>> {
    let mut input = String::new();
    for (a, b) in cases {
        input.push_str(CASE);
        input.push('\n');
        input.push_str(a);
        input.push_str(SEP);
        input.push('\n');
        input.push_str(b);
    }

    let mut child = Command::new("python3")
        .arg("-c")
        .arg(ORACLE)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("python3 should be on PATH; this test is #[ignore]d for that reason");
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(input.as_bytes())
        .expect("writing cases to python3");
    let out = child.wait_with_output().expect("python3 should exit");
    assert!(out.status.success(), "the oracle failed: {:?}", out.status);

    let text = String::from_utf8(out.stdout).expect("difflib output is utf-8");
    text.split(&format!("{CASE}\n"))
        .skip(1)
        .map(|chunk| chunk.lines().map(str::to_string).collect())
        .collect()
}

/// Run every case through both implementations and compare.
fn compare(cases: Vec<(String, String)>, label: &str) {
    let expected = oracle(&cases);
    assert_eq!(
        expected.len(),
        cases.len(),
        "{label}: the oracle returned {} diffs for {} cases",
        expected.len(),
        cases.len()
    );

    for (i, ((a_text, b_text), want)) in cases.iter().zip(&expected).enumerate() {
        let a = templedb_db::splitlines(a_text);
        let b = templedb_db::splitlines(b_text);
        let got = templedb_db::unified_diff(&a, &b, "old", "new", 3);
        assert_eq!(
            &got, want,
            "{label}: case {i} disagrees with CPython difflib\n\
             --- a ---\n{a_text}\n--- b ---\n{b_text}"
        );
    }
}

/// The main sweep: sizes either side of the autojunk threshold, both
/// clustered and scattered edits, at several edit densities.
#[test]
#[ignore = "needs python3 as the reference oracle"]
fn unified_diff_matches_cpython_difflib() {
    let mut rng = Lcg(0x5eed_1234);
    let mut cases = Vec::new();

    // 0 and 1 are the degenerate covers; 199/200/201 straddle autojunk.
    for &len in &[0usize, 1, 2, 5, 17, 50, 150, 199, 200, 201, 300, 500] {
        for &edits in &[0usize, 1, 2, 5, 20] {
            for &clustered in &[false, true] {
                for _ in 0..6 {
                    cases.push(generate(&mut rng, len, edits, clustered));
                }
            }
        }
    }
    assert!(
        cases.len() > 700,
        "expected a real sweep, got {}",
        cases.len()
    );
    compare(cases, "sweep");
}

/// Pairs sharing long repeated blocks, which is where the
/// longest-match tie-break and the adjacent-block merge decide the
/// answer. Random edits rarely produce two equally good alignments;
/// these are built to.
#[test]
#[ignore = "needs python3 as the reference oracle"]
fn repeated_blocks_align_the_same_way() {
    let block = ["def f():", "    pass", "", "}"];
    let mut cases = Vec::new();

    for reps in 1..=8usize {
        let a: Vec<&str> = block
            .iter()
            .cycle()
            .take(reps * block.len())
            .copied()
            .collect();
        // One more copy of the same block: which repetition difflib
        // decides is the "new" one is a pure tie-break.
        let b: Vec<&str> = block
            .iter()
            .cycle()
            .take((reps + 1) * block.len())
            .copied()
            .collect();
        cases.push((join(&a), join(&b)));
        cases.push((join(&b), join(&a)));

        // And with the duplication in the middle rather than appended.
        let mut c = a.clone();
        let mid = c.len() / 2;
        for (k, line) in block.iter().enumerate() {
            c.insert(mid + k, line);
        }
        cases.push((join(&a), join(&c)));
    }
    compare(cases, "repeated blocks");
}

/// Real file pairs, which is the case the generator cannot invent: the
/// actual distribution of lines in this repository's own source.
///
/// Each file is diffed against edited copies of itself — a line removed
/// near the top, a block moved, the whole thing reversed — because the
/// interesting property is the line *distribution*, not the edit.
#[test]
#[ignore = "needs python3 as the reference oracle"]
fn real_source_files_diff_identically() {
    let mut cases = Vec::new();

    for path in [
        "src/difflib.rs",
        "src/entities.rs",
        "src/vcs.rs",
        "src/lib.rs",
        "src/store.rs",
    ] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines: Vec<&str> = templedb_db::splitlines(&text);
        if lines.len() < 20 {
            continue;
        }

        // A line dropped near the top: every subsequent line number
        // shifts, which is the shape a real edit usually has.
        let mut dropped = lines.clone();
        dropped.remove(5);
        cases.push((join(&lines), join(&dropped)));

        // A block moved from the front to the back. difflib and Myers
        // disagree most sharply on moves.
        let mut moved = lines[10..].to_vec();
        moved.extend_from_slice(&lines[..10]);
        cases.push((join(&lines), join(&moved)));

        // Reversed: almost no alignment survives, so the matcher is
        // driven to its worst case.
        let mut reversed = lines.clone();
        reversed.reverse();
        cases.push((join(&lines), join(&reversed)));

        // Every tenth line changed, scattered through a long file.
        let marked: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                if i % 10 == 0 {
                    format!("// CHANGED {i}")
                } else {
                    l.to_string()
                }
            })
            .collect();
        let marked: Vec<&str> = marked.iter().map(String::as_str).collect();
        cases.push((join(&lines), join(&marked)));
    }

    assert!(
        !cases.is_empty(),
        "no source files were readable; run from the crate directory"
    );
    compare(cases, "real sources");
}
