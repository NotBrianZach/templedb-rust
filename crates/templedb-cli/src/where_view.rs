//! `templedb file where` — the mirror-drift diagnostic.
//!
//! # The first command in this port that reads a filesystem
//!
//! Everything before it is a function of the database. This one hashes
//! real files, which is why it waited on the sweep's tree fence rather
//! than on the write path: the two binaries run sequentially, so a tree
//! edited between the invocations is a difference neither
//! implementation produced. The fence is in `scripts/parity-sweep.sh`
//! and the reasoning for *detect* rather than *pin* is written up there.
//!
//! It reads and mutates nothing, so it does not compete with the Python
//! implementation for authority over anything — which is why it is here
//! before the tree *layer* of step 27, and not after it.
//!
//! The split follows `summary`'s: the database half is
//! `templedb_db::files::current_hash`, and everything that touches the
//! machine is here, in the binary, where the sweep can see it.
//!
//! # Four decisions, two of them about reproducing something wrong
//!
//! **1. Both roots come from the `checkouts` table, and the hardcoded
//! pair they replace was wrong in both directions.** This entry used to
//! say the `edit-work` probe was "reproduced and can never fire",
//! because Python probed
//! `~/.config/templedb/edit-workspaces/<slug>/<path>` and the real
//! layout has a session directory in between. That was a correct
//! reading of Python in September and a stale one by 2026-11-05, when
//! this command shipped: upstream had already moved both labels onto
//! `CheckoutRepository.resolve`, and its comment says why — "the probe
//! omitted the session component, `.exists()` was always False, and
//! step 4 skips missing mirrors silently. The copy most likely to be
//! drifted … was therefore the one mirror `where` never reported."
//!
//! The measurement that made the old entry feel safe is the part worth
//! keeping, because it was *true and irrelevant*: "none of the 24
//! project directories under that root holds a file at depth 1" is a
//! fact about the data, and the question was whether Python still
//! probed that root. A reproduced bug needs a check against the other
//! implementation's *behaviour*, not against the state its behaviour
//! used to depend on.
//!
//! Measured 2026-11-15, before the fix: `file where` differed from
//! Python on **three of the sweep's four tree-reading rows**, and not
//! only by the missing line.
//!
//! | row | before |
//! |-----|--------|
//! | `templedb-rust README.md` | `edit-work` line missing |
//! | `templedb integrations/emacs/templedb-agent.el` | `edit-work` line missing |
//! | `system_config emacs.d/layers/templedb/packages.el` | `edit-work` line missing |
//! | `woofsledger .gitignore` | `checkout ` line missing — its canonical tree is not at the canonical path |
//! | `nixos_anywhere_vultr_woofs .envrc` | `checkout ` line **invented** — this port hashed a directory Python does not resolve |
//!
//! The last two were not sweep rows and were found by walking the
//! first six files of every project through both binaries. A missed
//! mirror and an invented one are the same defect in opposite
//! directions, and the invented one is the worse of the two: the
//! command exists to make drift visible, and it was reporting drift
//! between the database and a tree no other command reads.
//!
//! Resolution now lives in [`templedb_db::Purpose`] and
//! [`templedb_db::checkout_root`]; what is left here is the dedup, the
//! no-checkout fallback and the probe itself.
//!
//! **2. The `where` docstring's depth cap does not exist.** It says the
//! overlay walk is capped "to avoid runaway walks (spacemacs private/
//! is shallow anyway)". `os.walk` is called with no depth limit, so the
//! walk is bounded only by what is under the three roots. Reproduced as
//! written in code, not as described in prose.
//!
//! **3. A symlink cycle is guarded here and is not guarded there.**
//! `os.walk(followlinks=True)` with no `seen` set will walk a cycle
//! until it runs out of path length. This keeps a set of canonicalised
//! directories and visits each once. That is a deliberate divergence
//! and it cannot show up in a diff: the two outputs differ only in the
//! case where Python produces no output at all, because it is still
//! walking. A hang is not a line to compare.
//!
//! **4. No `--no-color`.** Python always paints and has no flag, and
//! this prints the same escapes in the same places. `summary` has a
//! `--no-color` that Python lacks and pays for it with a permanent row
//! in the sweep's flag allow-list; there is no reason to buy a second
//! one here, and a byte-diff of coloured output is a byte-diff.

use std::collections::BTreeSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// One probed location and what was found there.
pub struct Mirror {
    /// The label Python prints, padded as Python pads it. `"checkout "`
    /// and `"edit-work"` are nine characters by hand, not by format
    /// width, so they are carried as literals rather than computed.
    pub label: String,
    pub path: PathBuf,
}

/// What a probe found.
pub enum Found {
    Match(String),
    Drift(String),
    /// Python prints the `Exception` text in parentheses. The port's
    /// wording is its own, which makes this the one line in the command
    /// that is not byte-comparable — the same class as the error
    /// messages the README lists as a known gap, and reachable only for
    /// a file that exists and cannot be read.
    Unreadable(String),
}

/// The two TempleDB roots, in Python's order, from the roots
/// [`templedb_db::checkout_root`] resolved.
///
/// `build_root` is Python's `PURPOSE_BUILD` and `edit_root` its
/// `PURPOSE_EDIT`; `None` for either is the `ValueError` Python's
/// `get_checkout_path` raises, which its loop swallows with a
/// `continue`.
///
/// # Two behaviours that are easy to leave out
///
/// **Dedup on the resolved root, not on the label.** The two purposes
/// are often the same directory — any project with no edit tree
/// resolves both to the canonical one — and Python keys a `seen_roots`
/// set on `str(root)`, so the second label is dropped rather than
/// printed as a duplicate of the first. Without this, every project
/// without an edit workspace would grow a redundant `edit-work` line
/// that Python does not print, which is exactly the class of invented
/// mirror decision 1 above is about.
///
/// **The no-checkout fallback is the canonical path.** With neither
/// purpose resolving — no active `checkouts` row and a `repo_url` that
/// is a URL — Python falls back to
/// `~/.config/templedb/checkouts/<slug>/<path>` "so `where` still
/// reports something rather than DB-only". So the hardcoded path this
/// function used to be built on survives as the last resort, which is
/// also why the old implementation agreed with Python as often as it
/// did.
///
/// `$HOME` rather than a home-directory crate, because Python builds
/// that fallback from `Path.home()` and the two must agree about what
/// home is. `HOME` unset is an error in the caller, not a fallback to
/// the passwd entry: a port that guessed differently from Python would
/// probe a different tree and report drift that is not there.
pub fn templedb_mirrors(
    home: &Path,
    slug: &str,
    path: &str,
    build_root: Option<&str>,
    edit_root: Option<&str>,
) -> Vec<Mirror> {
    let mut out = Vec::new();
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (label, root) in [("checkout ", build_root), ("edit-work", edit_root)] {
        let Some(root) = root else { continue };
        if !seen.insert(root) {
            continue;
        }
        out.push(Mirror {
            label: label.to_string(),
            path: Path::new(root).join(path),
        });
    }
    if out.is_empty() {
        out.push(Mirror {
            label: "checkout ".to_string(),
            path: home
                .join(".config/templedb/checkouts")
                .join(slug)
                .join(path),
        });
    }
    out
}

/// The three overlay roots Python probes, in its order.
pub fn overlay_roots(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".emacs.d"),
        PathBuf::from("/etc/nixos"),
        home.join(".config/systemd/user"),
    ]
}

/// Overlay hits: any file whose *basename* matches, resolved, kept only
/// if it lands in `/nix/store`.
///
/// # Why basename and not path
///
/// Python matches `Path(path).name` against each directory's file list,
/// so `file where templedb src/foo.el` will match a `foo.el` anywhere
/// under `~/.emacs.d` regardless of its directory. That is the intent —
/// the question is "which copy does Emacs load" and the overlay does
/// not preserve the project's layout — but it means the command can
/// report drift between two files that were never the same file. Kept,
/// because it is the behaviour, and noted because the output does not
/// say so.
///
/// # Why the order is the walk's and not sorted
///
/// Python appends hits in `os.walk` order, which is `readdir` order,
/// which is neither sorted nor stable across filesystems. Rust's
/// `read_dir` goes through the same `getdents`, so a top-down walk that
/// pushes subdirectories in `read_dir` order visits in the same order
/// on the same filesystem. Sorting here would be tidier and would
/// differ from Python on any directory whose entries are not already
/// ordered — so it is not sorted, and the sweep's tree fence is what
/// makes depending on this safe: both binaries read the same tree, and
/// a row whose tree moved is reported as churned rather than compared.
pub fn overlay_mirrors(home: &Path, file_path: &str) -> Vec<Mirror> {
    let basename = match Path::new(file_path).file_name() {
        Some(b) => b.to_owned(),
        // A path ending in a separator has no basename. Python's
        // `Path(path).name` is `''` here and matches nothing, since no
        // directory lists an empty filename.
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    let mut seen_real: BTreeSet<PathBuf> = BTreeSet::new();
    // Decision 3: Python has no cycle guard and this does.
    let mut visited_dirs: BTreeSet<PathBuf> = BTreeSet::new();

    for root in overlay_roots(home) {
        if !root.exists() {
            continue;
        }
        let mut stack = vec![root];
        while let Some(dir) = stack.pop() {
            let real_dir = fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
            if !visited_dirs.insert(real_dir) {
                continue;
            }
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            // `read_dir` order, reversed onto a stack so that popping
            // restores it. `os.walk` recurses into `dirnames` in the
            // order `scandir` produced them.
            let mut subdirs = Vec::new();
            let mut hit: Option<PathBuf> = None;
            for entry in entries.flatten() {
                let p = entry.path();
                // `followlinks=True`: a symlink to a directory is
                // descended into, which is the point — the interesting
                // overlay entries are symlinks into the store.
                if p.is_dir() {
                    subdirs.push(p);
                } else if entry.file_name() == basename {
                    hit = Some(p);
                }
            }
            if let Some(hit) = hit {
                if let Ok(real) = fs::canonicalize(&hit) {
                    if real.is_file()
                        && real.starts_with("/nix/store")
                        && seen_real.insert(real.clone())
                    {
                        // `~/<relative>` when it is under home, the
                        // absolute path otherwise. Python labels the
                        // *probe* path and hashes the *resolved* one.
                        let label = match hit.strip_prefix(home) {
                            Ok(rel) => format!("~/{}", rel.display()),
                            Err(_) => hit.display().to_string(),
                        };
                        out.push(Mirror { label, path: real });
                    }
                }
            }
            for p in subdirs.into_iter().rev() {
                stack.push(p);
            }
        }
    }
    out
}

/// sha256 of a file's bytes, streamed.
///
/// Python does `hashlib.sha256(mpath.read_bytes())`, which reads the
/// whole file first. Streaming gives the same digest and does not hold
/// a 100 MB blob to hash it; the only observable difference would be
/// which error an unreadable file reports, and that line is already
/// not byte-comparable.
pub fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    // Byte-by-byte rather than `{:x}`: sha2 0.11's `Array` does not
    // implement `LowerHex`. Same expression `fileset.rs` and
    // `commit.rs` use, for the same reason.
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Probe one mirror. `None` means "not present", which Python skips
/// silently — "noisier than helpful, and their absence is expected for
/// most slug/path pairs".
pub fn probe(m: &Mirror, db_hash: &str) -> Option<Found> {
    if !m.path.exists() {
        return None;
    }
    match hash_file(&m.path) {
        Err(e) => Some(Found::Unreadable(e.to_string())),
        Ok(h) if h == db_hash => Some(Found::Match(h)),
        Ok(h) => Some(Found::Drift(h)),
    }
}

/// The whole of stdout, plus the exit status.
///
/// Returned as lines rather than printed so that the nineteen-test
/// precedent `summary_view` set applies here too: the rendering is a
/// function of facts and is testable without a filesystem.
pub fn render(
    slug: &str,
    path: &str,
    db_hash: Option<&str>,
    probed: &[(String, Found)],
) -> (Vec<String>, i32) {
    let mut out = Vec::new();
    out.push(format!("file:  {slug}/{path}"));

    let Some(db_hash) = db_hash else {
        // Python's f-string has no placeholder in it, so the `DB:`
        // label here is three characters and not the ten-wide one
        // below. Reproduced as the inconsistency it is.
        out.push("  DB:  (no current row — file is deleted or never existed)".to_string());
        return (out, 2);
    };

    out.push(format!(
        "  {:<10} {}  current",
        "DB",
        &db_hash[..12.min(db_hash.len())]
    ));

    let mut any_drift = false;
    for (label, found) in probed {
        match found {
            Found::Match(h) => {
                out.push(format!("  {label} {}  \x1b[32m✓ match\x1b[0m", &h[..12]));
            }
            Found::Drift(h) => {
                any_drift = true;
                out.push(format!(
                    "  {label} {}  \x1b[31m✗ drift\x1b[0m (fs={})",
                    &h[..12],
                    &h[..12]
                ));
            }
            Found::Unreadable(e) => {
                any_drift = true;
                out.push(format!(
                    "  {label} ????????????  \x1b[33munreadable\x1b[0m ({e})"
                ));
            }
        }
    }

    if any_drift {
        out.push(String::new());
        out.push(
            "  \x1b[33mdrift detected\x1b[0m — run 'templedb project checkout <slug> \
             <path> --writable --force' to publish DB → checkouts, or 'templedb file \
             set' from a trusted mirror to update DB."
                .to_string(),
        );
        return (out, 1);
    }
    (out, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(c: char) -> String {
        // `repeat_n` rather than `repeat().take()`: rustc 1.99's
        // `clippy::manual_repeat_n` warns on the latter, which is
        // toolchain drift rather than a change here — this line is
        // unchanged since the command landed and the lint is new.
        std::iter::repeat_n(c, 64).collect()
    }

    #[test]
    fn missing_db_row_prints_one_line_and_exits_2() {
        let (lines, code) = render("alpha", "gone.txt", None, &[]);
        assert_eq!(code, 2);
        assert_eq!(
            lines,
            vec![
                "file:  alpha/gone.txt",
                "  DB:  (no current row — file is deleted or never existed)",
            ]
        );
    }

    /// A clean file exits 0 and prints no advisory — and **the two hash
    /// columns are one character out of line.**
    ///
    /// This assertion was written the other way round first, on the
    /// assumption that a command printing a table aligns it. It does
    /// not: the `DB` row pads its label to ten with `{:<10}` and the
    /// mirror rows carry nine-character labels written out by hand, so
    /// the database hash starts at column 13 and every mirror hash at
    /// 12. Reproduced, like `summary`'s padding of an already-painted
    /// string, because it is on stdout — and pinned here rather than
    /// left implicit, since the obvious tidy-up is a one-character edit
    /// that would silently break the byte-diff.
    #[test]
    fn matching_mirror_exits_0_and_the_columns_are_one_apart() {
        let db = h('a');
        let (lines, code) = render(
            "alpha",
            "README.md",
            Some(&db),
            &[("checkout ".to_string(), Found::Match(db.clone()))],
        );
        assert_eq!(code, 0);
        assert_eq!(lines.len(), 3);
        let db_col = lines[1].find("aaaaaaaaaaaa").unwrap();
        let fs_col = lines[2].find("aaaaaaaaaaaa").unwrap();
        assert_eq!(db_col, 13);
        assert_eq!(fs_col, 12);
        assert!(lines[2].contains("✓ match"));
    }

    /// Drift exits 1 and prints the filesystem hash twice — once in the
    /// column and once in the `(fs=…)` parenthetical. That is Python's
    /// output and it is redundant; reproduced rather than tidied.
    #[test]
    fn drift_exits_1_and_repeats_the_hash() {
        let (lines, code) = render(
            "alpha",
            "README.md",
            Some(&h('a')),
            &[("checkout ".to_string(), Found::Drift(h('b')))],
        );
        assert_eq!(code, 1);
        assert!(lines[2].contains("✗ drift"));
        assert_eq!(lines[2].matches("bbbbbbbbbbbb").count(), 2);
        assert_eq!(lines[3], "");
        assert!(lines[4].contains("drift detected"));
    }

    /// An unreadable mirror counts as drift, so it exits 1 and prints
    /// the advisory, with twelve literal `?` where the hash goes.
    #[test]
    fn unreadable_mirror_is_drift() {
        let (lines, code) = render(
            "alpha",
            "README.md",
            Some(&h('a')),
            &[("checkout ".to_string(), Found::Unreadable("EACCES".into()))],
        );
        assert_eq!(code, 1);
        assert!(lines[2].contains("????????????"));
        assert!(lines.last().unwrap().contains("drift detected"));
    }

    /// Both probes hang off the roots resolution handed over, including
    /// the session directory the old hardcoded `edit-work` path omitted
    /// — which is the whole of the 2026-11-15 repair at this seam.
    #[test]
    fn both_probes_come_from_the_resolved_roots() {
        let ms = templedb_mirrors(
            Path::new("/home/x"),
            "proj",
            "a/b.txt",
            Some("/home/x/.config/templedb/checkouts/proj"),
            Some("/home/x/.config/templedb/edit-workspaces/proj/agent-7"),
        );
        assert_eq!(
            ms.iter().map(|m| m.path.clone()).collect::<Vec<_>>(),
            vec![
                PathBuf::from("/home/x/.config/templedb/checkouts/proj/a/b.txt"),
                PathBuf::from("/home/x/.config/templedb/edit-workspaces/proj/agent-7/a/b.txt"),
            ]
        );
    }

    /// Two purposes resolving to one directory print one line. Any
    /// project with no edit tree is in this state, so without the
    /// dedup the port would invent an `edit-work` line on most of them.
    #[test]
    fn one_root_resolved_twice_is_one_mirror() {
        let ms = templedb_mirrors(
            Path::new("/home/x"),
            "proj",
            "a.txt",
            Some("/trees/proj"),
            Some("/trees/proj"),
        );
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].label, "checkout ");
    }

    /// Neither purpose resolves — no active `checkouts` row and a
    /// `repo_url` that is a URL. Python still reports the canonical
    /// path rather than DB-only, which is also why the hardcoded
    /// implementation agreed with it as often as it did.
    #[test]
    fn no_resolved_root_falls_back_to_the_canonical_path() {
        let ms = templedb_mirrors(Path::new("/home/x"), "proj", "a.txt", None, None);
        assert_eq!(
            ms.iter().map(|m| m.path.clone()).collect::<Vec<_>>(),
            vec![PathBuf::from(
                "/home/x/.config/templedb/checkouts/proj/a.txt"
            )]
        );
    }

    /// An edit tree with no canonical one beside it prints `edit-work`
    /// alone, and the fallback must not fire — it is keyed on "no
    /// mirrors at all", not on "no checkout label".
    #[test]
    fn an_edit_root_alone_is_not_topped_up_with_the_fallback() {
        let ms = templedb_mirrors(
            Path::new("/home/x"),
            "proj",
            "a.txt",
            None,
            Some("/w/proj/s"),
        );
        assert_eq!(ms.len(), 1);
        assert_eq!(ms[0].label, "edit-work");
        assert_eq!(ms[0].path, PathBuf::from("/w/proj/s/a.txt"));
    }

    #[test]
    fn labels_are_nine_characters_so_the_columns_line_up() {
        let ms = templedb_mirrors(
            Path::new("/home/x"),
            "proj",
            "a.txt",
            Some("/a"),
            Some("/b"),
        );
        assert_eq!(ms.len(), 2);
        for m in &ms {
            assert_eq!(m.label.chars().count(), 9, "{:?}", m.label);
        }
    }

    /// A path with no basename matches nothing rather than matching
    /// every directory, which is what Python's `''` does.
    #[test]
    fn trailing_separator_has_no_basename_and_probes_nothing() {
        assert!(overlay_mirrors(Path::new("/nonexistent-home"), "a/b/").is_empty());
    }

    /// Hashing agrees with the reference vector, because the whole
    /// command is a comparison of this number against one the database
    /// holds.
    #[test]
    fn hash_file_matches_sha256_of_the_bytes() {
        let dir = std::env::temp_dir().join(format!("tdbrs-where-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("empty");
        fs::write(&f, b"").unwrap();
        assert_eq!(
            hash_file(&f).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        fs::write(&f, b"abc").unwrap();
        assert_eq!(
            hash_file(&f).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        fs::remove_dir_all(&dir).ok();
    }
}
