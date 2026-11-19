//! Python's `Command.format_table`, reproduced byte for byte.
//!
//! This is the first renderer here whose subject is a *Python method*
//! rather than a command. `cli/core.py:348` is what `search files`
//! prints through, and `search files` is the first command this port
//! implements that uses it, so the table had to be ported before the
//! command could be diffed at all.
//!
//! It is deliberately not shared with `project list`, which prints its
//! own two-column form and is documented as the one implemented command
//! that is **not** a byte-diff candidate. Routing that through here
//! would close a gap this README argues is not worth closing, and it
//! would do it from a step that is about something else.
//!
//! # What is reproduced, and why each part matters
//!
//! ```text
//! <blank>
//! Files matching 'justfile' (1 results)
//! <blank>
//! project_slug  file_path type_name
//! ---------------------------------
//! templedb-rust justfile  justfile_
//! <blank>
//! ```
//!
//! (`_` marks a trailing space, which is real.)
//!
//!   * The header labels are the raw column *keys* — `project_slug`,
//!     not `Project`. Python passes `['project_slug', ...]` and formats
//!     them directly, so `lines_of_code` appears with its underscores.
//!   * Widths are `max(len(key), max(len(str(value))))` per column, and
//!     **every** cell is left-padded to its width including the last
//!     one, so most rows end in spaces. A renderer that trims them
//!     produces a diff on every row with a short final cell.
//!   * The rule is `'-' * len(header)` — the width of the *joined
//!     header line*, not the sum of the column widths. The two differ
//!     by the single-space separators, so the rule is longer than a
//!     naive sum by `columns - 1`.
//!   * `str(None)` is the literal `None`. Python has no NULL rendering
//!     of its own here, and 576 of the 2,780 live `lines_of_code`
//!     values are NULL, so this is the common case rather than an edge.
//!   * The title is emitted as one list element `"\n{title}\n"` and the
//!     list is `'\n'.join`ed, so it contributes a blank line before and
//!     after. The final element is `''`, which with `print`'s own
//!     newline makes the output end in exactly two.
//!
//! Width is measured in `char`s, not bytes: Python's `len()` counts code
//! points and its `:<` pads to that count. No live path is non-ASCII
//! (measured: 0 of 2,780), so the two agree today on real data; the
//! distinction is kept because getting it wrong would be invisible until
//! the first non-ASCII path, and then wrong on every row of that table.
//! Neither implementation accounts for East Asian display width, and
//! this one does not improve on it — the contract is Python's bytes.

/// Render `rows` as Python's ASCII table.
///
/// `columns` are the header labels *and* the cell order; each row must
/// have one cell per column. Returns the string `print` is given, i.e.
/// without the trailing newline `print` adds.
///
/// An empty `rows` returns the empty string, exactly as Python's early
/// `return ""` does. Every caller checks for no results first and prints
/// its own line, so this is the unreachable-by-construction branch
/// rather than the no-results message.
pub fn format_table(rows: &[Vec<String>], columns: &[&str], title: &str) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let mut widths: Vec<usize> = columns.iter().map(|c| c.chars().count()).collect();
    for row in rows {
        debug_assert_eq!(row.len(), columns.len(), "row width must match columns");
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let pad = |cell: &str, width: usize| {
        let mut out = cell.to_string();
        for _ in cell.chars().count()..width {
            out.push(' ');
        }
        out
    };
    let join = |cells: Vec<String>| cells.join(" ");

    let header = join(
        columns
            .iter()
            .enumerate()
            .map(|(i, c)| pad(c, widths[i]))
            .collect(),
    );

    let mut lines = vec![format!("\n{title}\n"), header.clone()];
    lines.push("-".repeat(header.chars().count()));
    for row in rows {
        lines.push(join(
            row.iter()
                .enumerate()
                .map(|(i, cell)| pad(cell, widths[i]))
                .collect(),
        ));
    }
    // Python appends `''` so the join ends the last row with a newline;
    // `print` then adds the second one.
    lines.push(String::new());
    lines.join("\n")
}

/// `str(x)` for a nullable integer column: the number, or the literal
/// `None`.
///
/// A free function rather than a `format!` at each call site because
/// `None` is not a formatting choice — it is Python's `str(None)`
/// leaking into a table this port diffs, and naming it keeps that
/// visible.
pub fn py_str_opt_i64(v: Option<i64>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "None".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(r: &[&[&str]]) -> Vec<Vec<String>> {
        r.iter()
            .map(|row| row.iter().map(|c| c.to_string()).collect())
            .collect()
    }

    /// The exact bytes `templedb search files justfile` prints, captured
    /// from the Python binary on 2026-11-17. If this test and the sweep
    /// ever disagree, the sweep is right — but this one fails in
    /// `cargo test`, with no database and no interpreter.
    #[test]
    fn matches_pythons_bytes_for_a_one_row_table() {
        let out = format_table(
            &rows(&[&["templedb-rust", "justfile", "justfile"]]),
            &["project_slug", "file_path", "type_name"],
            "Files matching 'justfile' (1 results)",
        );
        assert_eq!(
            out,
            "\nFiles matching 'justfile' (1 results)\n\
             \n\
             project_slug  file_path type_name\n\
             ---------------------------------\n\
             templedb-rust justfile  justfile \n"
        );
    }

    /// The rule spans the separators too, so it is longer than the sum
    /// of the column widths by `columns - 1`.
    #[test]
    fn the_rule_measures_the_joined_header_not_the_columns() {
        let out = format_table(&rows(&[&["a", "b", "c"]]), &["x", "y", "z"], "t");
        let rule = out.lines().nth(4).unwrap();
        // Three columns one character wide, plus the two separators.
        assert_eq!(rule, "-----");
        assert_eq!(rule.len(), (1 + 1 + 1) + 2);
    }

    /// A short final cell keeps its padding. Trimming here would diff on
    /// most rows of most tables.
    #[test]
    fn the_last_column_is_padded_too() {
        let out = format_table(&rows(&[&["x", "1"], &["y", "1000"]]), &["a", "b"], "t");
        assert!(out.contains("\nx 1   \n"), "{out:?}");
        assert!(out.contains("\ny 1000\n"), "{out:?}");
    }

    /// The header label sets a floor on the width even when every value
    /// is shorter — which is the usual case for `lines_of_code`.
    #[test]
    fn the_header_label_sets_a_minimum_width() {
        let out = format_table(&rows(&[&["65"]]), &["lines_of_code"], "t");
        assert!(out.contains("\nlines_of_code\n"), "{out:?}");
        assert!(out.contains("\n-------------\n"), "{out:?}");
        assert!(out.ends_with("\n65           \n"), "{out:?}");
    }

    /// Blank line before the title, blank line after it, and two
    /// newlines at the end once `print` has added its own.
    #[test]
    fn the_title_and_the_tail_carry_their_blank_lines() {
        let out = format_table(&rows(&[&["v"]]), &["c"], "Title (1 results)");
        assert!(out.starts_with("\nTitle (1 results)\n\nc\n-\n"), "{out:?}");
        assert!(out.ends_with("v\n"), "{out:?}");
        assert_eq!(format!("{out}\n").matches("\n\n").count(), 2, "{out:?}");
    }

    /// Width is counted in code points. No live path is non-ASCII, so
    /// this pins the choice rather than a current behaviour.
    #[test]
    fn width_counts_characters_not_bytes() {
        let out = format_table(&rows(&[&["é"], &["ab"]]), &["c"], "t");
        // `é` is two bytes and one char, so it pads to width 2.
        assert!(out.contains("\né \n"), "{out:?}");
    }

    #[test]
    fn no_rows_is_the_empty_string() {
        assert_eq!(format_table(&[], &["a"], "t"), "");
    }

    #[test]
    fn a_null_integer_renders_as_pythons_none() {
        assert_eq!(py_str_opt_i64(None), "None");
        assert_eq!(py_str_opt_i64(Some(0)), "0");
        assert_eq!(py_str_opt_i64(Some(-1)), "-1");
    }
}
