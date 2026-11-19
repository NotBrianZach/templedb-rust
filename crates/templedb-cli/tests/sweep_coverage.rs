//! Does the parity sweep still cover every command this binary has?
//!
//! # The gap this closes, and the half it does not
//!
//! The README has carried *the sweep is a shell script, not a test* as
//! a known gap since the audit: `cargo test` does not run it, so
//! nothing fails if it is never invoked. The obvious reading is that
//! the sweep should become a `#[test]`, and it cannot — it needs the
//! live 776 MB SQLite database and the Python `templedb` on PATH, and a
//! test that silently skips when those are absent is the same failure
//! one layer down.
//!
//! But *coverage* is a different question from *agreement*, and it is
//! answerable with nothing but this binary. The sweep exists because
//! per-command harnesses missed drift; a command absent from the sweep
//! is a command with no harness at all, which is the same hole the
//! sweep was built to close, one level up. Nothing was checking for it:
//! `doctor history` could have been added to the CLI and never added to
//! `INVOCATIONS`, and every number the sweep prints would have gone up.
//!
//! So this runs in `cargo test`, with no database and no interpreter,
//! and asks two things:
//!
//! 1. every leaf subcommand appears in the sweep's `INVOCATIONS`, or is
//!    named in [`UNSWEEPABLE`] with a reason;
//! 2. every name in [`UNSWEEPABLE`] is still a real command.
//!
//! The second direction is the one that earns its keep. An exemption
//! list only ever suppresses, so an entry for a command that was
//! renamed or swept later goes on suppressing nothing — which is
//! exactly what happened to `-s` and `--stage` in the sweep's own flag
//! allow-list, found by hand two days after they stopped being needed.
//!
//! What this still does not check is whether the *arguments* in a row
//! reach anything interesting. `vcs log p` and `vcs log p -n 3
//! --published` both count as coverage here and only one of them
//! exercises the published view. That is a judgement the sweep's own
//! comments carry and no test can take over.

use std::collections::BTreeSet;
use std::process::Command;

/// The binary cargo just built, so the test cannot drift from a stale
/// `target/release` the way a shell invocation of the sweep can.
const BIN: &str = env!("CARGO_BIN_EXE_templedb-rs");

/// Commands the sweep cannot run, each with the reason it cannot.
///
/// Not a list of commands that are hard to test — a list of commands
/// for which a *diff against Python* is not a defined operation.
const UNSWEEPABLE: &[(&str, &str)] = &[
    (
        "mirror",
        "copies the read subset into a Postgres this port owns. Python has no \
         equivalent command, so there is nothing to diff against, and running it \
         would TRUNCATE the parity target mid-sweep.",
    ),
    (
        "state",
        "the port's own state-diff harness. Python has no equivalent; its output is \
         a digest per table and exists to be compared against another run of itself.",
    ),
    (
        "file set",
        "a write. The sweep is a diff of stdout over a shared input and the two \
         implementations write different databases, so there is no shared input. \
         Verified by cross-read instead — crates/templedb-db/tests/write_parity.rs. \
         Its flag surface IS swept, which is a separate stage.",
    ),
    (
        "file rm",
        "a write, for `file set`'s reason — different databases, no shared input, \
         verified by cross-read in write_parity.rs. Its stdout WAS measured against \
         Python on a copy of the live database and the 35 bytes are pinned in \
         filerm.rs's renderer tests, like `intent create`'s and `intent apply`'s. \
         Its flag surface is swept, and agrees with no allow-list entry because \
         neither side has a flag: the `--session` this port's strictness would \
         justify was deliberately not invented.",
    ),
    (
        "file mark",
        "a write, for `file set`'s reason — different databases, no shared input, \
         verified by cross-read in write_parity.rs. Its cross-read is the strongest \
         of the seven writes: `project_files.edit_mode` is read by `file ls -l` and \
         `file ls --edit-mode`, both of which the sweep byte-diffs against Python, so \
         the effect is observable in verified output rather than only in a column. \
         Its stdout WAS measured against Python on a copy of the live database, all \
         four transitions, and the byte counts are pinned in filemark.rs's renderer \
         tests. Its flag surface is swept and agrees with no allow-list entry.",
    ),
    (
        "vcs commit-staged",
        "a write, and a name only this port has. `templedb vcs commit-staged --help` \
         fails on the other side, which is the intended consequence of not calling \
         it `vcs commit` — see the README's *The second write*.",
    ),
    (
        "intent cancel",
        "a write, for `file set`'s reason — different databases, no shared input, \
         verified by cross-read in write_parity.rs. Unlike the other two writes its \
         NAME is Python's, so its flag surface is swept and it is the first write in \
         this port whose surface the sweep can see at all.",
    ),
    (
        "intent apply",
        "a write, for `file set`'s reason — different databases, no shared input, \
         verified by cross-read in write_parity.rs. Its stdout WAS measured against \
         Python on a copy of the live database, like `intent create`'s, and the two \
         lines are pinned in intent.rs's renderer tests. It is also the one command \
         here whose *refusals* differ from Python's by design: a moved base exits 4 \
         where Python merges, so even a sweep that could write would show a difference \
         on that row and the README argues why. Its flag surface is swept.",
    ),
    (
        "intent create",
        "a write, for `file set`'s reason — different databases, no shared input, \
         verified by cross-read in write_parity.rs. Its stdout WAS measured against \
         Python, by running Python against a copy of the live database and pinning the \
         bytes in intent.rs's renderer tests; that is a one-off measurement and not \
         something the sweep can repeat, because repeating it would mean a sweep that \
         writes. Its flag surface is swept, like `intent cancel`'s.",
    ),
];

/// Commands the sweep covers through a stage other than `INVOCATIONS`.
///
/// Separate from [`UNSWEEPABLE`] because the reason is the opposite
/// one: these *are* diffed against Python, just not as whole stdout.
/// Collapsing the two lists would let "we compare this a different
/// way" and "comparing this is not defined" wear the same word, and
/// the first is a measurement while the second is a boundary.
const SWEPT_ELSEWHERE: &[(&str, &str)] = &[(
    "summary",
    "compared one pane at a time, by the SUMMARY_PANES stage. Its whole stdout can \
     never match — two of Python's nine panes are not ported and print `(not ported)` \
     in place — so a whole-output row would be a permanent, uninformative difference. \
     Diffing the sub-unit that carries the meaning is the same move PROJECT_FILTERS \
     makes for `project list`.",
)];

/// Read the sweep script from the repository root.
fn sweep_script() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/parity-sweep.sh");
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("reading {path}: {e}. The sweep is part of the gate."))
}

/// The quoted rows of a bash array, by name.
///
/// Deliberately a parse of the script rather than a second copy of the
/// list: a test carrying its own expected commands would pass while the
/// sweep ran none of them, which is the failure being checked for.
fn array_entries(script: &str, name: &str) -> Vec<String> {
    let open = format!("\n{name}=(\n");
    let start = script
        .find(&open)
        .unwrap_or_else(|| panic!("no `{name}=(` array in parity-sweep.sh"))
        + open.len();
    let len = script[start..]
        .find("\n)")
        .unwrap_or_else(|| panic!("unterminated `{name}=(` array"));
    script[start..start + len]
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('"'))
        .map(|l| l.trim_matches('"').to_string())
        .collect()
}

/// The subcommand names listed in one `--help` output.
///
/// clap prints a `Commands:` block of `  name  description` lines,
/// ending at the first blank line. `help` is clap's own and is not a
/// command this port implements.
fn subcommands_of(path: &[&str]) -> Vec<String> {
    let out = Command::new(BIN)
        .args(path)
        .arg("--help")
        .output()
        .unwrap_or_else(|e| panic!("running {BIN} {path:?} --help: {e}"));
    assert!(
        out.status.success(),
        "{BIN} {path:?} --help exited {:?}",
        out.status.code()
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let Some(rest) = text.split_once("\nCommands:\n") else {
        return Vec::new();
    };
    rest.1
        .lines()
        .take_while(|l| !l.trim().is_empty())
        // Continuation lines of a wrapped description are indented
        // further than the name column; a name line starts at two
        // spaces and has a non-space in column three.
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.split_whitespace().next())
        .filter(|n| *n != "help")
        .map(str::to_string)
        .collect()
}

/// Every leaf command path, as the string a caller would type.
fn leaf_commands() -> Vec<String> {
    let mut leaves = Vec::new();
    let mut stack: Vec<Vec<String>> = subcommands_of(&[]).into_iter().map(|c| vec![c]).collect();
    assert!(!stack.is_empty(), "the binary reported no subcommands");
    while let Some(path) = stack.pop() {
        let borrowed: Vec<&str> = path.iter().map(String::as_str).collect();
        let children = subcommands_of(&borrowed);
        if children.is_empty() {
            leaves.push(path.join(" "));
        } else {
            for c in children {
                let mut next = path.clone();
                next.push(c);
                stack.push(next);
            }
        }
    }
    leaves.sort();
    leaves
}

/// Every leaf command is either swept or exempt, with a reason.
#[test]
fn the_sweep_covers_every_command_or_says_why_not() {
    let script = sweep_script();
    let invocations = array_entries(&script, "INVOCATIONS");
    assert!(
        invocations.len() > 40,
        "only {} invocations parsed — the array parse is probably wrong, \
         and a parse that finds nothing would pass every assertion below",
        invocations.len()
    );
    let exempt: BTreeSet<&str> = UNSWEEPABLE
        .iter()
        .chain(SWEPT_ELSEWHERE)
        .map(|(c, _)| *c)
        .collect();

    let mut missing = Vec::new();
    for leaf in leaf_commands() {
        if exempt.contains(leaf.as_str()) {
            continue;
        }
        // Prefix, because a row carries arguments after the command.
        // The trailing space keeps `vcs show` from matching
        // `vcs show-something`, and the equality arm covers a command
        // swept with no arguments at all.
        let covered = invocations
            .iter()
            .any(|i| *i == leaf || i.starts_with(&format!("{leaf} ")));
        if !covered {
            missing.push(leaf);
        }
    }
    assert!(
        missing.is_empty(),
        "these commands are in the binary and in no sweep invocation:\n  {}\n\
         Add a row to INVOCATIONS in scripts/parity-sweep.sh, or an entry to \
         UNSWEEPABLE in this file with the reason a diff against Python is not \
         defined for it.",
        missing.join("\n  ")
    );
}

/// Every exemption still names a real command.
///
/// The direction that catches a *stale* entry, which an exemption list
/// cannot catch by being consulted — it only ever suppresses.
#[test]
fn no_exemption_outlives_its_command() {
    let leaves: BTreeSet<String> = leaf_commands().into_iter().collect();
    let stale: Vec<&str> = UNSWEEPABLE
        .iter()
        .chain(SWEPT_ELSEWHERE)
        .map(|(c, _)| *c)
        .filter(|c| !leaves.contains(*c))
        .collect();
    assert!(
        stale.is_empty(),
        "UNSWEEPABLE / SWEPT_ELSEWHERE name commands this binary does not have: \
         {stale:?}.\n\
         Renamed, or removed? Either way the entry now suppresses nothing and \
         should go."
    );
}

/// A command is in one exemption list or the other, never both.
///
/// They mean opposite things — "not diffable at all" against "diffed
/// another way" — so an entry in both would make the pair meaningless
/// and would still pass every other test here.
#[test]
fn the_two_exemption_lists_do_not_overlap() {
    let unsweepable: BTreeSet<&str> = UNSWEEPABLE.iter().map(|(c, _)| *c).collect();
    let both: Vec<&str> = SWEPT_ELSEWHERE
        .iter()
        .map(|(c, _)| *c)
        .filter(|c| unsweepable.contains(c))
        .collect();
    assert!(both.is_empty(), "in both exemption lists: {both:?}");
}

/// Every exemption carries a reason long enough to be one.
///
/// Cheap, and it is the rule the list exists to enforce: an exemption
/// without an argument is how a command quietly stops being checked.
#[test]
fn every_exemption_gives_a_reason() {
    for (cmd, why) in UNSWEEPABLE.iter().chain(SWEPT_ELSEWHERE) {
        assert!(
            why.len() > 60,
            "{cmd}'s exemption is {} characters — say why a diff against Python \
             is not defined for it, not that it is awkward",
            why.len()
        );
    }
}

/// The flag-surface stage covers every read-only leaf command.
///
/// A separate list in the script and a separate question: a command can
/// be diffed byte-for-byte on its output and still accept a different
/// set of flags, which is the class of difference the audit found four
/// of. The writes are exempt here for one fewer reason than above —
/// `file set`'s surface *is* compared, because `--help` needs no write
/// target — so the only exemptions are the three commands Python does
/// not have at all.
#[test]
fn the_flag_surface_stage_covers_every_command_python_also_has() {
    let script = sweep_script();
    let flag_commands = array_entries(&script, "FLAG_COMMANDS");
    assert!(
        flag_commands.len() > 15,
        "only {} flag commands parsed",
        flag_commands.len()
    );
    // The three with no counterpart on the other side. `file set` is
    // NOT among them: its flags are compared and nine of them have
    // left the allow-list that way.
    let no_counterpart = ["mirror", "state", "vcs commit-staged"];

    let mut missing = Vec::new();
    for leaf in leaf_commands() {
        if no_counterpart.contains(&leaf.as_str()) {
            continue;
        }
        let covered = flag_commands
            .iter()
            .any(|c| *c == leaf || c.starts_with(&format!("{leaf} ")));
        if !covered {
            missing.push(leaf);
        }
    }
    assert!(
        missing.is_empty(),
        "these commands have no flag-surface row:\n  {}\n\
         Add one to FLAG_COMMANDS in scripts/parity-sweep.sh. Placeholder \
         arguments are fine — both implementations parse --help before \
         validating positionals.",
        missing.join("\n  ")
    );
}

/// The allow-list's pairs name commands that are actually swept.
///
/// `<command>|<flag>` entries are matched against a `FLAG_COMMANDS` row
/// verbatim, so a typo or a renamed command makes the entry dead —
/// present, unmatched, and silently suppressing nothing while reading
/// as a documented divergence.
#[test]
fn every_flag_allowance_names_a_swept_command() {
    let script = sweep_script();
    let flag_commands = array_entries(&script, "FLAG_COMMANDS");
    let orphans: Vec<String> = array_entries(&script, "FLAGS_EXPECTED")
        .into_iter()
        .filter(|pair| {
            let cmd = pair.split('|').next().unwrap_or_default();
            !flag_commands.iter().any(|c| c == cmd)
        })
        .collect();
    assert!(
        orphans.is_empty(),
        "FLAGS_EXPECTED entries whose command is in no FLAG_COMMANDS row: {orphans:?}"
    );
}
