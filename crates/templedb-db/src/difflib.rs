//! CPython's `difflib.SequenceMatcher` and `unified_diff`, reimplemented.
//!
//! # Why not a diff crate
//!
//! Every `vcs` command that prints a diff renders it through
//! `difflib.unified_diff`, and difflib's matcher is not Myers. It is a
//! recursive longest-matching-block search with its own tie-breaks and
//! an "autojunk" heuristic, and it produces *different hunks* from a
//! Myers or Patience implementation — not merely differently formatted
//! output.
//!
//! That was measured before this module was written, on 28 real
//! consecutive-revision pairs from the live database, comparing
//! `difflib.unified_diff` against `diff -u`:
//!
//! | | pairs |
//! |---|---|
//! | identical output | 12 |
//! | different output | 16 |
//!
//! Six of the sixteen differ at the *same line count* — 136 vs 136,
//! 18 vs 18, 79 vs 79, 52 vs 52, 63 vs 63, 94 vs 94 — so the hunks are
//! grouped and placed differently rather than merely sized differently.
//! `similar` and `diffs` both implement Myers or Patience, so neither
//! can close that gap. Byte parity on `vcs show`, `vcs diff` and
//! `vcs diff --staged` means having this algorithm, so here it is.
//!
//! # What is reproduced, and what is left out
//!
//! The Python call site is
//! `difflib.unified_diff(old, new, fromfile=…, tofile=…, lineterm='')`,
//! which means `isjunk=None`, `autojunk=True` and `n=3`. So:
//!
//! - **`autojunk` is reproduced.** It is the part that changes the
//!   answer, and it is why this cannot be "the obvious algorithm".
//!   See [`SequenceMatcher::new`].
//! - **`isjunk` is not implemented.** With `isjunk=None` the junk set is
//!   empty, which makes `find_longest_match`'s two junk-absorbing loops
//!   unreachable and its two non-junk extension loops unconditional.
//!   Carrying a predicate no caller can set would mean four loops where
//!   two suffice, and two of them dead. If a caller ever needs it, the
//!   shape to restore is in CPython's `Lib/difflib.py`.
//!
//! `ratio`, `quick_ratio`, `get_close_matches`, `context_diff`,
//! `HtmlDiff` and `Differ` are likewise absent: nothing in the ported
//! surface calls them. `context_diff` is the one with a live Python
//! caller (`vcs diff --context`), and porting it is a day's work on top
//! of this module rather than a redesign, because it consumes the same
//! [`SequenceMatcher::grouped_opcodes`].

use std::collections::HashMap;
use std::hash::Hash;

/// A run of elements equal in both sequences: `a[a_start..][..size]`
/// equals `b[b_start..][..size]`.
///
/// Field order is the sort order, and it matters: [`Match`] derives
/// `Ord` and [`SequenceMatcher::matching_blocks`] relies on the derived
/// lexicographic comparison to reproduce Python's
/// `matching_blocks.sort()` on `(i, j, k)` tuples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Match {
    pub a_start: usize,
    pub b_start: usize,
    pub size: usize,
}

/// How one span of `a` corresponds to one span of `b`.
///
/// The variants carry both ranges even where one is empty, because the
/// unified-diff renderer needs the insertion *point* in the sequence it
/// is not taking lines from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opcode {
    pub tag: Tag,
    pub i1: usize,
    pub i2: usize,
    pub j1: usize,
    pub j2: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tag {
    Equal,
    Replace,
    Delete,
    Insert,
}

/// Longest-matching-block differ, following CPython's algorithm.
pub struct SequenceMatcher<'a, T: Hash + Eq> {
    a: &'a [T],
    b: &'a [T],
    /// Each distinct element of `b` to the ascending list of positions
    /// it occupies — minus the "popular" elements autojunk removed.
    b2j: HashMap<&'a T, Vec<usize>>,
}

impl<'a, T: Hash + Eq> SequenceMatcher<'a, T> {
    /// Index `b` and apply the autojunk heuristic.
    ///
    /// # Autojunk is not an optimisation
    ///
    /// Once `b` has 200 or more elements, any element occupying more than
    /// `len(b) / 100 + 1` positions is dropped from the index. The
    /// comment in CPython calls it a heuristic for speed, and it is, but
    /// it is **also** load-bearing for the output: a dropped element can
    /// no longer *start* a match, so a file where some line recurs often
    /// — a bare `}`, a blank line, `  pass` — gets structurally
    /// different hunks above the 200-line threshold than below it.
    ///
    /// Crucially, the popular set is *not* the junk set. Popular
    /// elements are removed from `b2j`, so no match is seeded on one,
    /// but the extension loops in [`Self::find_longest_match`] test
    /// element equality directly and will happily absorb them. A match
    /// can therefore run *through* a popular line but never *begin* at
    /// one. Getting that distinction wrong is the single easiest way to
    /// produce plausible diffs that do not match Python's, because it
    /// only shows up on files long enough for the threshold to fire.
    ///
    /// The 200 and the 1% are CPython's literals, reproduced rather than
    /// tuned. They are not a tuning parameter here — they are part of
    /// the answer being matched.
    pub fn new(a: &'a [T], b: &'a [T]) -> Self {
        let mut b2j: HashMap<&'a T, Vec<usize>> = HashMap::new();
        for (i, elt) in b.iter().enumerate() {
            b2j.entry(elt).or_default().push(i);
        }

        let n = b.len();
        if n >= 200 {
            let ntest = n / 100 + 1;
            b2j.retain(|_, idxs| idxs.len() <= ntest);
        }

        Self { a, b, b2j }
    }

    /// The longest block of equal elements in `a[alo..ahi]` /
    /// `b[blo..bhi]`.
    ///
    /// Of all maximal-length blocks it returns the one starting earliest
    /// in `a`, and of those the one starting earliest in `b`. That
    /// tie-break is why the comparison below is `>` and not `>=`, and it
    /// is observable: it decides which of two equally good alignments a
    /// diff shows.
    ///
    /// The `j2len` rolling map is CPython's trick for finding runs
    /// without a quadratic table. For each position `i` in `a` it holds,
    /// keyed by position in `b`, the length of the matching run ending at
    /// `(i, j)`. A run ending at `(i, j)` is one longer than a run
    /// ending at `(i-1, j-1)`, so each row is computed from the previous
    /// one and then replaces it.
    pub fn find_longest_match(&self, alo: usize, ahi: usize, blo: usize, bhi: usize) -> Match {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: HashMap<usize, usize> = HashMap::new();

        for i in alo..ahi {
            let mut newj2len: HashMap<usize, usize> = HashMap::new();
            if let Some(js) = self.b2j.get(&self.a[i]) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    // `b2j`'s lists are ascending, so the first position
                    // at or past `bhi` means every later one is too.
                    if j >= bhi {
                        break;
                    }
                    let k = j
                        .checked_sub(1)
                        .and_then(|jm1| j2len.get(&jm1).copied())
                        .unwrap_or(0)
                        + 1;
                    newj2len.insert(j, k);
                    if k > bestsize {
                        besti = i + 1 - k;
                        bestj = j + 1 - k;
                        bestsize = k;
                    }
                }
            }
            j2len = newj2len;
        }

        // Extend the block across elements autojunk removed from the
        // index. Without this the popular-element filter would truncate
        // real matches instead of merely declining to seed on them —
        // see the note on `new`.
        while besti > alo && bestj > blo && self.a[besti - 1] == self.b[bestj - 1] {
            besti -= 1;
            bestj -= 1;
            bestsize += 1;
        }
        while besti + bestsize < ahi
            && bestj + bestsize < bhi
            && self.a[besti + bestsize] == self.b[bestj + bestsize]
        {
            bestsize += 1;
        }

        Match {
            a_start: besti,
            b_start: bestj,
            size: bestsize,
        }
    }

    /// Every matching block, ascending, with a zero-length sentinel at
    /// the end.
    ///
    /// Recursion made explicit as a stack, as CPython has it: find the
    /// longest match in a region, then recurse on the region before it
    /// and the region after it. The stack order does not affect the
    /// result — each region is processed independently and the blocks are
    /// sorted afterwards — so `pop()` taking the most recent entry is
    /// reproduced without needing to be relied on.
    ///
    /// The trailing `(len(a), len(b), 0)` sentinel is not decoration:
    /// [`Self::opcodes`] walks the blocks and emits whatever gap
    /// precedes each one, so the sentinel is what emits the final
    /// delete/insert/replace after the last real match.
    pub fn matching_blocks(&self) -> Vec<Match> {
        let (la, lb) = (self.a.len(), self.b.len());
        let mut queue = vec![(0, la, 0, lb)];
        let mut blocks: Vec<Match> = Vec::new();

        while let Some((alo, ahi, blo, bhi)) = queue.pop() {
            let m = self.find_longest_match(alo, ahi, blo, bhi);
            if m.size == 0 {
                continue;
            }
            if alo < m.a_start && blo < m.b_start {
                queue.push((alo, m.a_start, blo, m.b_start));
            }
            if m.a_start + m.size < ahi && m.b_start + m.size < bhi {
                queue.push((m.a_start + m.size, ahi, m.b_start + m.size, bhi));
            }
            blocks.push(m);
        }
        blocks.sort();

        // Adjacent blocks are merged. The recursion can produce two
        // blocks that abut — a match extended across a popular element
        // meets the match found in the next region — and Python reports
        // one block rather than two.
        let mut merged: Vec<Match> = Vec::with_capacity(blocks.len() + 1);
        let (mut i1, mut j1, mut k1) = (0usize, 0usize, 0usize);
        for m in blocks {
            if i1 + k1 == m.a_start && j1 + k1 == m.b_start {
                k1 += m.size;
            } else {
                if k1 > 0 {
                    merged.push(Match {
                        a_start: i1,
                        b_start: j1,
                        size: k1,
                    });
                }
                (i1, j1, k1) = (m.a_start, m.b_start, m.size);
            }
        }
        if k1 > 0 {
            merged.push(Match {
                a_start: i1,
                b_start: j1,
                size: k1,
            });
        }
        merged.push(Match {
            a_start: la,
            b_start: lb,
            size: 0,
        });
        merged
    }

    /// How to turn `a` into `b`, as a contiguous cover of both sequences.
    ///
    /// Every index of `a` appears in exactly one opcode and so does every
    /// index of `b`, which is what lets the renderer walk opcodes and
    /// never need to look at what came before.
    pub fn opcodes(&self) -> Vec<Opcode> {
        let (mut i, mut j) = (0usize, 0usize);
        let mut out = Vec::new();

        for m in self.matching_blocks() {
            let (ai, bj, size) = (m.a_start, m.b_start, m.size);
            // The gap before this block. Consuming from both sequences
            // is a replace, from one a delete or an insert, from
            // neither nothing at all.
            let tag = match (i < ai, j < bj) {
                (true, true) => Some(Tag::Replace),
                (true, false) => Some(Tag::Delete),
                (false, true) => Some(Tag::Insert),
                (false, false) => None,
            };
            if let Some(tag) = tag {
                out.push(Opcode {
                    tag,
                    i1: i,
                    i2: ai,
                    j1: j,
                    j2: bj,
                });
            }
            i = ai + size;
            j = bj + size;
            if size > 0 {
                out.push(Opcode {
                    tag: Tag::Equal,
                    i1: ai,
                    i2: i,
                    j1: bj,
                    j2: j,
                });
            }
        }
        out
    }

    /// Opcodes split into hunks, each padded with up to `n` lines of
    /// context and separated by the runs of unchanged lines that are too
    /// long to print.
    ///
    /// Three edge cases carry real weight, and all three come straight
    /// from CPython:
    ///
    /// 1. **Two identical sequences produce no groups.** The single
    ///    `Equal` opcode survives the fixups below, and the final guard
    ///    rejects a group that is one `Equal` and nothing else. That is
    ///    what makes `unified_diff` of equal input empty — and so what
    ///    makes the Python caller print `No differences`.
    /// 2. **Two empty sequences produce no groups either**, via the
    ///    `codes.is_empty()` substitution. Without it the loop sees
    ///    nothing and the function would have to decide what an empty
    ///    cover means.
    /// 3. **A leading or trailing `Equal` is clipped to `n`**, which is
    ///    why a change on line 500 does not print the 499 lines above it.
    pub fn grouped_opcodes(&self, n: usize) -> Vec<Vec<Opcode>> {
        let mut codes = self.opcodes();
        if codes.is_empty() {
            codes.push(Opcode {
                tag: Tag::Equal,
                i1: 0,
                i2: 1,
                j1: 0,
                j2: 1,
            });
        }

        // Clip the leading and trailing context to `n` lines. Done by
        // mutating the first and last opcode in place, as Python does,
        // because when there is exactly one opcode both fixups apply to
        // it and the order they apply in is part of the behaviour.
        if let Some(first) = codes.first_mut() {
            if first.tag == Tag::Equal {
                first.i1 = first.i1.max(first.i2.saturating_sub(n));
                first.j1 = first.j1.max(first.j2.saturating_sub(n));
            }
        }
        if let Some(last) = codes.last_mut() {
            if last.tag == Tag::Equal {
                last.i2 = last.i2.min(last.i1 + n);
                last.j2 = last.j2.min(last.j1 + n);
            }
        }

        let nn = n + n;
        let mut groups: Vec<Vec<Opcode>> = Vec::new();
        let mut group: Vec<Opcode> = Vec::new();

        for mut code in codes {
            // A run of unchanged lines longer than twice the context
            // ends the hunk: `n` lines close the current one, `n` lines
            // open the next, and everything between is not printed.
            if code.tag == Tag::Equal && code.i2 - code.i1 > nn {
                group.push(Opcode {
                    tag: code.tag,
                    i1: code.i1,
                    i2: code.i2.min(code.i1 + n),
                    j1: code.j1,
                    j2: code.j2.min(code.j1 + n),
                });
                groups.push(std::mem::take(&mut group));
                code.i1 = code.i1.max(code.i2.saturating_sub(n));
                code.j1 = code.j1.max(code.j2.saturating_sub(n));
            }
            group.push(code);
        }

        // A group holding one `Equal` and nothing else describes no
        // change, so it is dropped rather than printed as an empty hunk.
        if !group.is_empty() && !(group.len() == 1 && group[0].tag == Tag::Equal) {
            groups.push(group);
        }
        groups
    }
}

/// One side of a `@@` header, in `ed` range notation.
///
/// The two special cases are both reachable from real input and neither
/// is intuitive: a one-line range prints as a bare line number with no
/// length, and an *empty* range prints the line *before* it — so a hunk
/// that inserts at the very top of a file is `-0,0`.
fn format_range_unified(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return beginning.to_string();
    }
    if length == 0 {
        return format!("{},{}", beginning - 1, length);
    }
    format!("{beginning},{length}")
}

/// `difflib.unified_diff` with `lineterm=''`: no line carries a newline,
/// so the caller supplies exactly one per element.
///
/// `lineterm` is not a parameter because the Python call site passes
/// `''` and the default `'\n'` is what produced the double-spacing bug
/// recorded in `vcs.py`'s own docstring — every content line carried its
/// own newline and then got another from `print`. Hard-coding the
/// behaviour that is correct for this caller is better than carrying a
/// knob whose other setting is a known defect.
///
/// Returns an empty vector when the sequences are equal, which is how
/// the caller knows to print `No differences`.
pub fn unified_diff(a: &[&str], b: &[&str], fromfile: &str, tofile: &str, n: usize) -> Vec<String> {
    let matcher = SequenceMatcher::new(a, b);
    let mut out = Vec::new();

    for group in matcher.grouped_opcodes(n) {
        if out.is_empty() {
            out.push(format!("--- {fromfile}"));
            out.push(format!("+++ {tofile}"));
        }

        let (first, last) = (group[0], group[group.len() - 1]);
        out.push(format!(
            "@@ -{} +{} @@",
            format_range_unified(first.i1, last.i2),
            format_range_unified(first.j1, last.j2)
        ));

        for code in &group {
            match code.tag {
                Tag::Equal => out.extend(a[code.i1..code.i2].iter().map(|l| format!(" {l}"))),
                Tag::Delete => out.extend(a[code.i1..code.i2].iter().map(|l| format!("-{l}"))),
                Tag::Insert => out.extend(b[code.j1..code.j2].iter().map(|l| format!("+{l}"))),
                Tag::Replace => {
                    out.extend(a[code.i1..code.i2].iter().map(|l| format!("-{l}")));
                    out.extend(b[code.j1..code.j2].iter().map(|l| format!("+{l}")));
                }
            }
        }
    }
    out
}

/// `str.splitlines()`, which is not `str::lines()`.
///
/// Python splits on a set of boundaries that `lines()` does not know
/// about — `\x0b`, `\x0c`, `\x1c`, `\x1d`, `\x1e`, `\u{85}`,
/// `\u{2028}`, `\u{2029}` as well as `\n`, `\r` and `\r\n` — and the
/// Python call site splits the file content with it. A form feed is the
/// one of those that turns up in real source: it is a conventional page
/// break in older C and Emacs Lisp, and a file containing one would
/// otherwise be diffed as having a line this port thinks is one line
/// where Python sees two, shifting every line number after it.
///
/// Like Python's, this yields nothing for an empty string, and does not
/// yield a trailing empty element for a trailing newline.
pub fn splitlines(s: &str) -> Vec<&str> {
    const BOUNDARIES: [char; 8] = [
        '\x0b', '\x0c', '\x1c', '\x1d', '\x1e', '\u{85}', '\u{2028}', '\u{2029}',
    ];

    let mut out = Vec::new();
    let mut start = 0;
    let mut it = s.char_indices().peekable();
    while let Some((i, c)) = it.next() {
        let width = if c == '\r' {
            // `\r\n` is one boundary, not two.
            if it.peek().map(|&(_, c2)| c2) == Some('\n') {
                it.next();
                2
            } else {
                1
            }
        } else if c == '\n' || BOUNDARIES.contains(&c) {
            c.len_utf8()
        } else {
            continue;
        };
        out.push(&s[start..i]);
        start = i + width;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(s: &str) -> Vec<&str> {
        splitlines(s)
    }

    /// The headline case: equal input produces no output at all, which
    /// is what makes the caller print `No differences` rather than an
    /// empty diff with headers.
    #[test]
    fn equal_sequences_produce_no_diff() {
        let a = lines("one\ntwo\nthree\n");
        assert!(unified_diff(&a, &a, "x", "y", 3).is_empty());
    }

    #[test]
    fn two_empty_sequences_produce_no_diff() {
        assert!(unified_diff(&[], &[], "x", "y", 3).is_empty());
    }

    #[test]
    fn a_single_changed_line_in_context() {
        let a = lines("a\nb\nc\nd\ne\n");
        let b = lines("a\nb\nX\nd\ne\n");
        assert_eq!(
            unified_diff(&a, &b, "old", "new", 3),
            [
                "--- old",
                "+++ new",
                "@@ -1,5 +1,5 @@",
                " a",
                " b",
                "-c",
                "+X",
                " d",
                " e",
            ]
        );
    }

    /// An insertion at the very top makes the `a` range empty, and an
    /// empty range in `ed` notation begins at the line *before* it — so
    /// `-0,0`, not `-1,0`. Reached by typing one line into a new file.
    #[test]
    fn an_empty_range_starts_at_the_line_before() {
        assert_eq!(format_range_unified(0, 0), "0,0");
        let diff = unified_diff(&[], &["only"], "old", "new", 3);
        assert_eq!(diff[2], "@@ -0,0 +1 @@");
        assert_eq!(diff[3], "+only");
    }

    /// A one-line range prints bare, with no `,1`.
    #[test]
    fn a_one_line_range_has_no_length() {
        assert_eq!(format_range_unified(4, 5), "5");
        let a = lines("x\n");
        let b = lines("y\n");
        assert_eq!(unified_diff(&a, &b, "old", "new", 3)[2], "@@ -1 +1 @@");
    }

    /// Changes far apart become separate hunks; close together, one.
    /// The boundary is a run of unchanged lines longer than `2n`.
    #[test]
    fn distant_changes_split_into_separate_hunks() {
        let mut a: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let mut b = a.clone();
        b[2] = "changed early".into();
        b[27] = "changed late".into();
        let (av, bv): (Vec<&str>, Vec<&str>) = (
            a.iter().map(String::as_str).collect(),
            b.iter().map(String::as_str).collect(),
        );
        let hunks = unified_diff(&av, &bv, "old", "new", 3)
            .iter()
            .filter(|l| l.starts_with("@@"))
            .count();
        assert_eq!(hunks, 2);

        // Bring them within 2n of each other and the two hunks merge.
        a = (0..30).map(|i| format!("line {i}")).collect();
        b = a.clone();
        b[2] = "changed".into();
        b[7] = "also changed".into();
        let (av, bv): (Vec<&str>, Vec<&str>) = (
            a.iter().map(String::as_str).collect(),
            b.iter().map(String::as_str).collect(),
        );
        let hunks = unified_diff(&av, &bv, "old", "new", 3)
            .iter()
            .filter(|l| l.starts_with("@@"))
            .count();
        assert_eq!(hunks, 1);
    }

    /// Of several equally long matches, the earliest in `a` wins, then
    /// the earliest in `b`. This is the `>` versus `>=` tie-break, and
    /// it decides which alignment a diff shows.
    #[test]
    fn the_longest_match_is_the_earliest_of_the_longest() {
        let a = ["x", "y"];
        let b = ["x", "y", "x", "y"];
        let m = SequenceMatcher::new(&a, &b).find_longest_match(0, 2, 0, 4);
        assert_eq!(
            m,
            Match {
                a_start: 0,
                b_start: 0,
                size: 2
            }
        );
    }

    /// Adjacent blocks are merged into one, and the sentinel closes the
    /// list.
    #[test]
    fn matching_blocks_are_merged_and_sentinel_terminated() {
        let a = ["a", "b", "c"];
        let b = ["a", "b", "c"];
        assert_eq!(
            SequenceMatcher::new(&a, &b).matching_blocks(),
            [
                Match {
                    a_start: 0,
                    b_start: 0,
                    size: 3
                },
                Match {
                    a_start: 3,
                    b_start: 3,
                    size: 0
                },
            ]
        );
    }

    /// The opcode cover is contiguous and total over both sequences.
    /// A gap would mean the renderer silently dropped lines.
    #[test]
    fn opcodes_cover_both_sequences_without_gaps() {
        let a = lines("a\nb\nc\nd\n");
        let b = lines("a\nX\nY\nd\ne\n");
        let codes = SequenceMatcher::new(&a, &b).opcodes();
        let (mut i, mut j) = (0, 0);
        for c in &codes {
            assert_eq!(c.i1, i, "gap in a at {c:?}");
            assert_eq!(c.j1, j, "gap in b at {c:?}");
            i = c.i2;
            j = c.j2;
        }
        assert_eq!(i, a.len());
        assert_eq!(j, b.len());
    }

    /// Autojunk fires at 200 elements, and the threshold is observable:
    /// the same pair of sequences can diff differently either side of
    /// it. This pins that the filter exists at all — a port without it
    /// passes every small-input test and then disagrees with Python on
    /// real files.
    #[test]
    fn autojunk_drops_popular_elements_only_past_200() {
        // 199 elements: "dup" appears 100 times, well over 1%, but the
        // sequence is too short for the heuristic to apply.
        let short: Vec<&str> = std::iter::repeat_n("dup", 100)
            .chain(std::iter::repeat_n("other", 99))
            .collect();
        assert!(SequenceMatcher::new(&short, &short)
            .b2j
            .contains_key(&"dup"));

        // 200 elements, so `ntest` is 3 and "dup" is dropped.
        let owned: Vec<String> = std::iter::repeat_n("dup".to_string(), 100)
            // Distinct, so nothing else is popular.
            .chain((0..100).map(|i| format!("u{i}")))
            .collect();
        let long: Vec<&str> = owned.iter().map(String::as_str).collect();
        let m = SequenceMatcher::new(&long, &long);
        assert!(!m.b2j.contains_key(&"dup"));
        assert!(m.b2j.contains_key(&"u0"));
    }

    /// A popular element cannot *seed* a match but must still be
    /// absorbed by one that reaches it. Without the extension loops, the
    /// autojunk filter would truncate real matches and the diff would
    /// report changes that are not there.
    #[test]
    fn a_match_extends_through_a_popular_element() {
        // "" is popular (100 of 201 elements); the unique anchors around
        // it are not. The match seeded on an anchor must still swallow
        // the blank lines between them.
        let mut seq: Vec<String> = Vec::new();
        for i in 0..100 {
            seq.push(format!("anchor {i}"));
            seq.push(String::new());
        }
        seq.push("tail".into());
        let v: Vec<&str> = seq.iter().map(String::as_str).collect();

        let m = SequenceMatcher::new(&v, &v);
        assert!(!m.b2j.contains_key(&""), "blank should be popular");
        // Identical input, so the whole sequence is one block — which
        // is only reachable by extending across the popular blanks.
        assert_eq!(
            m.matching_blocks()[0],
            Match {
                a_start: 0,
                b_start: 0,
                size: v.len()
            }
        );
        assert!(unified_diff(&v, &v, "x", "y", 3).is_empty());
    }

    /// Python's `splitlines`, not Rust's `lines`. A form feed is a line
    /// boundary to Python, and it appears in real source as a page
    /// break — getting this wrong shifts every line number after it.
    #[test]
    fn splitlines_matches_pythons_boundaries() {
        assert_eq!(splitlines("a\nb"), ["a", "b"]);
        assert_eq!(splitlines("a\nb\n"), ["a", "b"]);
        assert_eq!(splitlines(""), Vec::<&str>::new());
        assert_eq!(splitlines("\n"), [""]);
        // CR, LF and CRLF are each one boundary.
        assert_eq!(splitlines("a\r\nb"), ["a", "b"]);
        assert_eq!(splitlines("a\rb"), ["a", "b"]);
        // The exotic ones Python honours and `str::lines` does not.
        assert_eq!(splitlines("a\x0cb"), ["a", "b"]);
        assert_eq!(splitlines("a\x0bb"), ["a", "b"]);
        assert_eq!(splitlines("a\u{2028}b"), ["a", "b"]);
        assert_eq!(splitlines("a\u{85}b"), ["a", "b"]);
        // Multibyte content is sliced on char boundaries, not bytes.
        assert_eq!(splitlines("héllo\nwörld"), ["héllo", "wörld"]);
    }

    /// A trailing newline must not add an empty final line, because
    /// Python's `splitlines` does not and an extra element would shift
    /// the line counts in every `@@` header.
    #[test]
    fn a_trailing_newline_adds_no_empty_line() {
        assert_eq!(splitlines("a\nb\n").len(), 2);
        assert_eq!(splitlines("a\nb").len(), 2);
        // But a blank line in the middle is real.
        assert_eq!(splitlines("a\n\nb\n"), ["a", "", "b"]);
    }

    /// Deletions and insertions render from the sequence they come from.
    /// Taking a `Delete`'s lines from `b` would panic or print the wrong
    /// text, and the ranges are empty on the side that is not consumed.
    #[test]
    fn pure_deletions_and_insertions_render_from_the_right_side() {
        let a = lines("keep\ndrop\nkeep2\n");
        let b = lines("keep\nkeep2\n");
        let d = unified_diff(&a, &b, "old", "new", 3);
        assert!(d.contains(&"-drop".to_string()));
        assert!(!d
            .iter()
            .any(|l| l.starts_with('+') && !l.starts_with("+++")));

        let d = unified_diff(&b, &a, "old", "new", 3);
        assert!(d.contains(&"+drop".to_string()));
        assert!(!d
            .iter()
            .any(|l| l.starts_with('-') && !l.starts_with("---")));
    }

    /// A replace emits every deletion before any insertion, not
    /// interleaved per line.
    #[test]
    fn a_replace_groups_deletions_before_insertions() {
        let a = lines("a\nb\nc\n");
        let b = lines("x\ny\nz\n");
        assert_eq!(
            unified_diff(&a, &b, "old", "new", 3),
            [
                "--- old",
                "+++ new",
                "@@ -1,3 +1,3 @@",
                "-a",
                "-b",
                "-c",
                "+x",
                "+y",
                "+z",
            ]
        );
    }

    /// `n = 0` prints no context at all, which is the degenerate case
    /// where the `2n` hunk-splitting threshold is zero too.
    #[test]
    fn zero_context_prints_only_changed_lines() {
        let a = lines("a\nb\nc\n");
        let b = lines("a\nX\nc\n");
        assert_eq!(
            unified_diff(&a, &b, "old", "new", 0),
            ["--- old", "+++ new", "@@ -2 +2 @@", "-b", "+X"]
        );
    }
}
