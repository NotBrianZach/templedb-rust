//! Differential test of [`templedb_db::classify_path`] and
//! [`templedb_db::is_ignored_path`] against the thing they reproduce:
//! CPython importing `importer.scanner` and calling the same two
//! functions.
//!
//! # Why an oracle and not a table
//!
//! `classify.rs`'s unit tests pin the cases someone thought to write
//! down, and they caught nothing — every one of them was written from a
//! reading of `scanner.py` and three were *wrong*, in the direction a
//! reading is always wrong: they agreed with what the source looks like
//! it says. `readme` and `ReadMe` were asserted as `readme` because the
//! guard is `p.upper() == 'README'`; they are `None`, because the guard
//! is only consulted for paths a case-*sensitive* pattern already
//! matched. The pattern count was asserted at 240 from a glance at the
//! list; it is 179. The skip list was called five patterns; it is six,
//! because one source line holds two.
//!
//! So the unit tests are regression guards on an answer this file
//! establishes, rather than the establishment of it. That ordering is
//! the whole reason this test exists: the table is 179 ordered regexes
//! with four guards and eight unanchored entries, and the failure mode
//! is not "a pattern is missing" — it is "a pattern is there and
//! answers a slightly different question", which no amount of reading
//! finds.
//!
//! # The corpus, and why it is generated rather than sampled
//!
//! **24,757 paths** from a cross product: 229 literal basenames and
//! extensions the table names, under twelve directory prefixes, with
//! eight suffix decorations plus an Emacs-lock form. The size is
//! `debug_assert`ed in [`corpus`] rather than only written here, for
//! `gate_counts.rs`'s reason: the README quotes it.
//!
//! The prefixes exist because six entries
//! care about the directory (`supabase/functions`, `.vscode`,
//! `.github/workflows`, `.circleci`, and the two `(^|/)` groups) and
//! because `README`'s guard reads the whole path. The decorations exist
//! because `is_ignored_path` is about suffixes and because the
//! interesting overlaps between the two functions are decorated paths
//! — `.env~` is ignored *and* `env_file`.
//!
//! Sampling the live database instead was considered and rejected as
//! the *only* corpus. Measured 2026-11-19: `project_files` holds 2,786
//! paths and they are overwhelmingly `.py`, `.ts`, `.md` and `.nix` —
//! they reach **74 of the 179 entries and 66 of the 164 distinct type
//! names**, so 98 names would go untested, including every one of the
//! four guards' negative branches. The live paths are measured anyway,
//! in the README, as a separate one-off — but they cannot be this
//! test's corpus for the reason every oracle here is hermetic:
//! `cargo test` must not need a 498 MB SQLite file.
//!
//! What makes the generated corpus checkable rather than merely large
//! is [`every_type_name_in_the_table_is_reached`], which asserts that
//! the corpus produces every `type_name` the table can produce. A
//! corpus that drifted away from the table would otherwise keep
//! passing while covering less, which is the failure
//! `sweep_coverage.rs` exists to prevent one layer up.
//!
//! # Why it is `#[ignore]`d
//!
//! It shells out to `python3` *and* needs `importer.scanner` on the
//! path, which is a stronger requirement than the other two oracles
//! have — they need only the standard library. `cargo test` must pass
//! on a machine with neither, so:
//!
//! ```bash
//! cargo test -p templedb-db --test classify_parity -- --ignored
//! ```
//!
//! `just check` and `nix flake check` both run the ignored set.
//!
//! The `PYTHONPATH` is read out of the `templedb` launcher on `PATH`
//! rather than guessed, because that launcher is a generated Nix
//! wrapper whose second line is the export. Deriving it is the only way
//! to be sure the oracle imports the *same* `scanner.py` the `templedb`
//! being diffed against in the sweep imports — a second copy in a
//! developer's checkout would make this test pass against code that is
//! not the reference.

use std::io::Write;
use std::process::{Command, Stdio};

/// The oracle. Imports the reference implementation rather than
/// restating it — there is no transcription here on purpose, because a
/// transcribed oracle only ever proves that two transcriptions agree.
const ORACLE: &str = r#"
import sys
from importer.scanner import (classify_path, is_ignored_path,
                              FILE_TYPE_PATTERNS, SKIP_FILE_PATTERNS,
                              GENERIC_TEXT_TYPE)

# Line one is the shape of the tables, so a port that lost or gained an
# entry fails on a count rather than on a sampled answer.
print(f"{len(FILE_TYPE_PATTERNS)}\t{len(SKIP_FILE_PATTERNS)}\t{GENERIC_TEXT_TYPE}")

for line in sys.stdin:
    path = line.rstrip("\n")
    t = classify_path(path)
    print(f"{'-' if t is None else t}\t{'1' if is_ignored_path(path) else '0'}")
"#;

/// Every literal basename or extension `FILE_TYPE_PATTERNS` names, plus
/// the near-misses that make the unanchored entries interesting.
///
/// Ordered the way the table is, so a reader comparing the two can walk
/// them together. `JUSTFILE` and `readme` are in here as near-misses
/// rather than as probes — both are `None`, and both looked like hits
/// when this corpus was first written.
#[rustfmt::skip]
const ATOMS: &[&str] = &[
    // Binary assets.
    "logo.svg", "logo.png", "a.jpg", "a.jpeg", "a.gif", "a.webp", "a.ico",
    "f.woff", "f.woff2", "f.ttf", "f.otf", "f.eot", "doc.pdf", "m.wasm",
    "key.gpg", "key.asc", "s.mp3", "s.wav", "s.ogg", "s.flac",
    "v.mp4", "v.webm", "v.mov", "v.mkv",
    "a.zip", "a.tar", "a.tar.gz", "a.tgz", "a.tar.bz2", "a.7z",
    // SQL.
    "schema.sql", "001_migration.sql", "report.sql", "MIGRATION.sql",
    // React / TS / JS.
    "App.jsx", "App.tsx", "a.test.ts", "a.test.js", "a.spec.ts", "a.spec.js",
    "index.ts", "index.js", "jest.config.js", "webpack.config.js",
    "vite.config.js", "rollup.config.js", "a.cjs", "a.mjs",
    // Shell config.
    ".bashrc", ".bash_profile", ".zshrc", ".profile", "a.fish", ".aliases",
    // Git.
    ".gitignore", ".gitattributes", ".gitmodules", ".gitconfig",
    // Editors.
    ".editorconfig", ".prettierrc", ".prettierrc.json", ".eslintrc",
    ".eslintrc.cjs", ".vimrc", "init.vim", ".rubocop.yml", ".rubocop.yaml",
    ".stylelintrc", ".flake8", ".babelrc", ".browserslistrc", "settings.json",
    // Emacs.
    ".spacemacs", "a.el",
    // Version managers.
    ".nvmrc", ".ruby-version", ".python-version", ".node-version",
    // CI.
    ".travis.yml", ".gitlab-ci.yml", "ci.yml", "config.yml", "Jenkinsfile",
    "dependabot.yml",
    // Languages.
    "main.rs", "main.go", "a.rb", "A.java", "a.kt", "a.swift", "a.c", "a.cpp",
    "a.cs", "a.php", "a.scala", "a.clj", "a.ex", "a.exs", "a.erl", "a.hs",
    "a.lua", "deploy_logic.pl", "a.pro", "a.plt", "a.pl", "a.r",
    // Packaging.
    "requirements.txt", "Pipfile", "pyproject.toml", "setup.py", "Cargo.toml",
    "Cargo.lock", "Gemfile", "Gemfile.lock", "go.mod", "go.sum", "flake.lock",
    "package-lock.json", "pnpm-lock.yaml", "yarn.lock", "other.lock",
    "composer.json", ".npmrc", ".yarnrc", "package.json",
    // Build tools.
    "Makefile", "Rakefile", "build.gradle", "pom.xml", "CMakeLists.txt",
    "tsconfig.json", "a.mk",
    // IaC.
    "a.tf", "a.tfvars", "playbook.yml", "inventory.yml", "k8s.yml",
    "Chart.yml",
    // Database / testing.
    "schema.prisma", "pytest.ini",
    // Styles and markup.
    "a.css", "a.scss", "a.html", "a.htm", "header.shtml", "header.shtm",
    "header.shtml.template", "nginx.conf.template",
    // Docker.
    "Dockerfile", "Dockerfile.dev", "docker-compose.yml", ".dockerignore",
    // Nix.
    "flake.nix", "shell.nix", "default.nix", "a.nix",
    // Platform.
    "Procfile", "vercel.json", "netlify.toml", "railway.json",
    // Config / env, including the unanchored near-misses.
    ".env", ".env.production", ".environment",
    // Deployment.
    "a.service", "ecosystem.config.js", "deploy.sh", "deploy.py",
    "templedb-deploy.sh", "deployment.yml", "deployment-prod.yaml",
    // Task runners.
    "justfile", "Justfile", "JUSTFILE", "Taskfile.yml", "Caddyfile",
    "Vagrantfile", "Brewfile", "CODEOWNERS",
    // Documentation, with the three README spellings that disagree.
    "README", "readme", "ReadMe", "README.md", "README.md.bak",
    "CHANGELOG.md", "LICENSE", "LICENSE.txt", "CONTRIBUTING.md",
    "SECURITY.md",
    // Misc.
    "robots.txt", ".htaccess",
    // Generic configs.
    "a.yml", "a.yaml", "a.jsonc", "a.json", "a.toml", "a.ini", "a.cfg",
    "a.conf", "a.properties",
    // Interfaces.
    "a.proto", "a.graphql", "a.xml",
    // Scripts.
    "a.sh", "a.bash", "a.zsh", "a.ps1", "a.bat", "a.cmd", "a.py", "a.jl",
    "a.zig",
    // Plain text, the catch-alls.
    "a.md", "a.rst", "a.adoc", "a.org", "a.tex", "a.patch", "a.diff",
    "a.csv", "notes.txt", "notes.text",
    // Names the table has no entry for at all, which is the `None` case
    // `file retype` turns into `text_file`.
    "Session.vim", "a.unknownextension", "noextension", "a.ml", "a.erb",
    // And two non-ASCII names, because `\d`, `.lower()` and `.upper()`
    // are all Unicode-aware on Python's side.
    "École_Ä.md", "ПРИВЕТ", "a.tmp.١٢٣",
];

/// The directory prefixes. Six entries in the table care about one.
#[rustfmt::skip]
const PREFIXES: &[&str] = &[
    "",
    "src/",
    "a/b/",
    "docs/",
    "tools/",
    "deep/nest/ed/path/",
    "supabase/functions/hello/",
    ".vscode/",
    "migrations/",
    ".github/workflows/",
    ".circleci/",
    "LICENSES/",
];

/// Suffix decorations, which is how `is_ignored_path` gets exercised and
/// how the overlap between the two functions gets reached.
#[rustfmt::skip]
const DECORATIONS: &[&str] = &[
    "",
    "~",
    ".orig",
    ".rej",
    ".bak",
    ".template",
    ".tmp.123",
    ".tmp.456.abcdef",
];

/// The corpus: every (prefix, atom, decoration) triple, plus the empty
/// path and the two Emacs-lock shapes, which no decoration produces
/// because the marker is a *prefix* on the basename.
fn corpus() -> Vec<String> {
    let mut out = Vec::with_capacity(PREFIXES.len() * ATOMS.len() * DECORATIONS.len() + 8);
    for prefix in PREFIXES {
        for atom in ATOMS {
            for decoration in DECORATIONS {
                out.push(format!("{prefix}{atom}{decoration}"));
            }
            // `(^|/)\.#` is a basename prefix, so it has to be built
            // rather than appended.
            out.push(format!("{prefix}.#{atom}"));
        }
        out.push(format!("{prefix}.DS_Store"));
        out.push(format!("{prefix}~"));
    }
    out.push(String::new());
    // The size is asserted rather than described, because the README
    // quotes it and a number in prose decays — the standing lesson of
    // `gate_counts.rs`. 229 atoms under 12 prefixes with 8 decorations,
    // plus one `.#` form per (prefix, atom), plus `.DS_Store` and `~`
    // per prefix, plus the empty path.
    debug_assert_eq!(
        out.len(),
        PREFIXES.len() * (ATOMS.len() * (DECORATIONS.len() + 1) + 2) + 1
    );
    out
}

/// Both answers, for every path in the corpus, as CPython gives them.
struct Oracle {
    patterns: usize,
    skips: usize,
    generic: String,
    answers: Vec<(String, bool)>,
}

/// The `PYTHONPATH` the `templedb` launcher on `PATH` exports.
///
/// The launcher is a generated Nix wrapper and its second line is
/// `export PYTHONPATH='…'`. Read rather than guessed; see the module
/// header for why a guess would weaken the test rather than only
/// inconvenience it.
fn reference_pythonpath() -> String {
    let which = Command::new("sh")
        .arg("-c")
        .arg("command -v templedb")
        .output()
        .expect("sh must be available");
    assert!(
        which.status.success(),
        "the Python `templedb` must be on PATH: it owns the `scanner.py` this test diffs against"
    );
    let launcher = String::from_utf8(which.stdout).unwrap().trim().to_string();
    let script = std::fs::read_to_string(&launcher)
        .unwrap_or_else(|e| panic!("cannot read the templedb launcher at {launcher}: {e}"));
    for line in script.lines() {
        if let Some(rest) = line.strip_prefix("export PYTHONPATH=") {
            return rest.trim_matches('\'').trim_matches('"').to_string();
        }
    }
    panic!("no `export PYTHONPATH=` line in {launcher}; the wrapper's shape has changed");
}

fn ask_cpython(paths: &[String]) -> Oracle {
    // A newline in a path would make the line protocol lie, and the one
    // input where Python's `$` and Rust's disagree is exactly a
    // trailing newline — so refusing it here is also what keeps that
    // divergence out of this harness and in the unit test that argues
    // it. See `classify.rs`.
    for p in paths {
        assert!(
            !p.contains('\n'),
            "the corpus must not contain a newline: {p:?}"
        );
    }
    let mut input = String::new();
    for p in paths {
        input.push_str(p);
        input.push('\n');
    }

    let mut child = Command::new("python3")
        .arg("-c")
        .arg(ORACLE)
        .env("PYTHONPATH", reference_pythonpath())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 must be on PATH for this test");

    // The write goes on its own thread, which `clock_parity.rs` and
    // `difflib_parity.rs` do not need and this one does. They send
    // 2,000 lines and get 2,000 back, which fits in a 64 KB pipe
    // buffer; this corpus is ~19,000 lines in and ~400 KB out. Writing
    // it all before reading deadlocks — the child blocks writing stdout
    // to a full pipe while the parent blocks writing stdin to a full
    // one. Found by a 600-second timeout rather than by reasoning.
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child.wait_with_output().unwrap();
    writer
        .join()
        .expect("the stdin writer thread panicked")
        .expect("writing the corpus to the oracle failed");
    assert!(
        output.status.success(),
        "oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .expect("oracle printed nothing")
        .split('\t')
        .collect();
    let answers: Vec<(String, bool)> = lines
        .map(|l| {
            let (t, ig) = l.split_once('\t').expect("malformed oracle line");
            (t.to_string(), ig == "1")
        })
        .collect();
    assert_eq!(
        answers.len(),
        paths.len(),
        "oracle returned {} answers for {} paths",
        answers.len(),
        paths.len()
    );
    Oracle {
        patterns: header[0].parse().unwrap(),
        skips: header[1].parse().unwrap(),
        generic: header[2].to_string(),
        answers,
    }
}

/// The port's two functions agree with CPython's on every path in the
/// corpus, and the two tables are the same size.
///
/// One test rather than three, because they share a subprocess and the
/// corpus takes about a second to run through CPython. The assertions
/// are separated in the failure messages instead.
#[test]
#[ignore = "needs python3 and importer.scanner as the reference oracle"]
fn both_classifiers_match_cpythons_scanner() {
    let paths = corpus();
    let oracle = ask_cpython(&paths);

    assert_eq!(
        oracle.patterns,
        templedb_db::pattern_count(),
        "FILE_TYPE_PATTERNS has {} entries upstream and {} here — an entry was lost, \
         gained, or upstream changed",
        oracle.patterns,
        templedb_db::pattern_count()
    );
    assert_eq!(
        oracle.skips,
        templedb_db::skip_pattern_count(),
        "SKIP_FILE_PATTERNS has {} entries upstream and {} here",
        oracle.skips,
        templedb_db::skip_pattern_count()
    );
    assert_eq!(oracle.generic, templedb_db::GENERIC_TEXT_TYPE);

    let mut type_mismatches = Vec::new();
    let mut ignore_mismatches = Vec::new();
    for (path, (want_type, want_ignored)) in paths.iter().zip(&oracle.answers) {
        let got = templedb_db::classify_path(path).unwrap_or("-");
        if got != want_type.as_str() {
            type_mismatches.push(format!("{path:?}: python {want_type} != rust {got}"));
        }
        let got_ignored = templedb_db::is_ignored_path(path);
        if got_ignored != *want_ignored {
            ignore_mismatches.push(format!(
                "{path:?}: python {want_ignored} != rust {got_ignored}"
            ));
        }
    }

    let show = |v: &Vec<String>| v.iter().take(20).cloned().collect::<Vec<_>>().join("\n  ");
    assert!(
        type_mismatches.is_empty(),
        "classify_path disagrees on {} of {} paths:\n  {}",
        type_mismatches.len(),
        paths.len(),
        show(&type_mismatches)
    );
    assert!(
        ignore_mismatches.is_empty(),
        "is_ignored_path disagrees on {} of {} paths:\n  {}",
        ignore_mismatches.len(),
        paths.len(),
        show(&ignore_mismatches)
    );
}

/// `type_name`s the table declares and no path can ever receive,
/// because an earlier unguarded entry matches a superset of what theirs
/// matches.
///
/// **One name, and it is upstream dead code rather than a gap in this
/// corpus.** `ecosystem\.config\.js$` → `pm2_config` is entry 135;
/// `\.js$` → `javascript` is entry 29, has no guard, and matches every
/// string the later pattern matches, since `ecosystem.config.js` ends
/// in `.js` by construction. So `classify_path` returns `javascript`
/// for a PM2 ecosystem config and `pm2_config` is unreachable.
///
/// Measured on the live database 2026-11-19: `file_types` carries
/// `pm2_config` as id 131 in category `infrastructure`, **0 of the
/// 2,786 `project_files` rows hold it**, and there is exactly one
/// `ecosystem.config.js` — which `file retype` therefore proposes to
/// move to `javascript`, correctly as far as this table is concerned.
/// Reported upstream, not fixed here; fixing it would mean reordering
/// `FILE_TYPE_PATTERNS`, which is a change to what every command
/// answers.
///
/// A second entry is shadowed the same way and is **harmless**:
/// `templedb-deploy\.sh$` (138) is covered by `deploy\.sh$` (136),
/// which is unanchored at the start and yields the same
/// `deployment_script`. It costs no exclusion here because its name is
/// reachable through the entry that shadows it, which is exactly the
/// difference between a redundant rule and a dead one.
const UNREACHABLE_TYPE_NAMES: &[&str] = &["pm2_config"];

/// The corpus reaches every `type_name` CPython's table can produce,
/// except the one that is unreachable by construction.
///
/// Without this the test above degrades silently: an atom deleted from
/// [`ATOMS`] takes a table entry out of coverage and nothing fails. The
/// authority for "what can the table produce" is CPython — the set of
/// names in the oracle's own `FILE_TYPE_PATTERNS` — rather than this
/// port's table, so a port that dropped an entry *and* the atom that
/// reached it still fails here.
///
/// Measured: the corpus reaches 163 of the table's 164 distinct names,
/// and the 164th is [`UNREACHABLE_TYPE_NAMES`]. The exclusion is
/// asserted in *both* directions — a name on that list that becomes
/// reachable fails this test too, so upstream reordering the table
/// shows up as a surprise rather than as a silence. Same discipline as
/// the sweep's EXPECTED list.
///
/// For contrast, and measured the same day: the 2,786 live
/// `project_files` paths reach **74 entries and 66 distinct names**, so
/// a corpus sampled from the database would leave 98 of the names
/// untested.
#[test]
#[ignore = "needs python3 and importer.scanner as the reference oracle"]
fn every_type_name_in_the_table_is_reached() {
    // Ask CPython for the table's own name set, so the expectation
    // cannot be a list maintained here.
    let names = Command::new("python3")
        .arg("-c")
        .arg(
            "from importer.scanner import FILE_TYPE_PATTERNS\n\
             print('\\n'.join(sorted({n for _, n, _ in FILE_TYPE_PATTERNS})))",
        )
        .env("PYTHONPATH", reference_pythonpath())
        .output()
        .expect("python3 must be on PATH for this test");
    assert!(
        names.status.success(),
        "{}",
        String::from_utf8_lossy(&names.stderr)
    );
    let expected: Vec<String> = String::from_utf8(names.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert!(!expected.is_empty());

    let paths = corpus();
    let reached: std::collections::BTreeSet<&str> = paths
        .iter()
        .filter_map(|p| templedb_db::classify_path(p))
        .collect();

    let missed: Vec<&String> = expected
        .iter()
        .filter(|n| !reached.contains(n.as_str()))
        .filter(|n| !UNREACHABLE_TYPE_NAMES.contains(&n.as_str()))
        .collect();
    assert!(
        missed.is_empty(),
        "{} of {} type names are unreachable from this corpus — add an atom for each: {:?}",
        missed.len(),
        expected.len(),
        missed
    );

    // The other direction. A name on the exclusion list that the corpus
    // *does* reach means upstream reordered the table and the dead
    // entry came alive, which is a finding and not something to absorb.
    let revived: Vec<&&str> = UNREACHABLE_TYPE_NAMES
        .iter()
        .filter(|n| reached.contains(**n))
        .collect();
    assert!(
        revived.is_empty(),
        "{revived:?} is on UNREACHABLE_TYPE_NAMES and the corpus reached it — upstream's \
         FILE_TYPE_PATTERNS has been reordered, which changes what every command answers"
    );

    // And the exclusion must still name something the table declares,
    // so a name deleted upstream does not sit here forever exempting
    // nothing. Same rule `sweep_coverage.rs` applies to its exemptions.
    for name in UNREACHABLE_TYPE_NAMES {
        assert!(
            expected.iter().any(|n| n == name),
            "{name:?} is excluded here but is no longer a type_name in FILE_TYPE_PATTERNS"
        );
    }
}
