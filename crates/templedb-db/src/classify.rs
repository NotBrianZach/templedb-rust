//! `importer.scanner`'s path classifier, which is the substance of
//! `file retype`.
//!
//! Two functions and two tables. [`classify_path`] maps a
//! project-relative path to a `file_types.type_name`, or to `None` for a
//! name it does not recognise; [`is_ignored_path`] is true for paths the
//! scanner declares out of scope. Both are pure, path-only and read
//! nothing — no database, no filesystem, no clock.
//!
//! # The first module here whose subject is neither a command nor a query
//!
//! `table.rs` was the first whose subject is a Python *method* rather
//! than a command. This is the first whose subject is a Python *module
//! constant*: a 179-entry ordered list of `(regex, type_name,
//! test_func)` triples that the scanner, the importer and this command
//! consult and that no SQL touches. It is carried here rather than in
//! `templedb-cli` because
//! `file retype`'s answer is a function of it and the query together,
//! and splitting the two would put half the command's semantics on the
//! other side of a crate boundary.
//!
//! # Order is the specification, not an implementation detail
//!
//! Python's comment on the list says, in its own words, that order
//! matters and that more specific patterns come first. The list is
//! walked top to bottom with the **first** match winning, and that is
//! load-bearing in places a reader would not guess:
//!
//! - `schema\.sql$` precedes `\.sql$`, so `schema.sql` is `sql_schema`
//!   and not `sql_file`.
//! - `\.test\.(ts|js)$` precedes `\.ts$`, so `a.test.ts` is
//!   `test_file`.
//! - `_logic\.pl$` precedes `\.pl$`, so templedb's own six SWI-Prolog
//!   rule sets are `prolog_source` and not `perl`.
//! - `\.svg$` is **first**, ahead of every text rule, which is why an
//!   SVG — text as far as any parser is concerned — classifies as a
//!   binary asset.
//! - The five generic config rules and the five plain-text rules are
//!   last, and `\.te?xt$` is dead last.
//!
//! So this module reproduces the list as a list, in order, rather than
//! as a `match` on extension. A `match` would be faster and would be
//! unable to express `continue`.
//!
//! # `continue`, not `return`, when a guard fails
//!
//! Four of the 179 entries carry a `test_func`, and Python's loop body
//! is:
//!
//! ```python
//! if re.search(pattern, rel_path):
//!     if test_func and not test_func(rel_path):
//!         continue
//!     return type_name
//! ```
//!
//! A failed guard falls through to the *rest of the list* rather than
//! returning `None`. That is the only reason three of the four guards
//! produce anything:
//!
//! - `\.sql$` + `'migration' in p.lower()` → `sql_migration`, and when
//!   the guard fails the next entry is a bare `\.sql$` → `sql_file`.
//!   Python's own comment on that second entry is `# Catch all SQL
//!   files`.
//! - `\.ts$` + `'supabase/functions' not in p` → `typescript`, and when
//!   the guard fails the entry below it is
//!   `supabase/functions/.*\.(ts|js)$` → `edge_function`. The pair is
//!   written as a guard plus a fallthrough rather than as two ordered
//!   patterns, and it only works because of the `continue`.
//! - `settings\.json$` + `'.vscode' in p` → `vscode_settings`, falling
//!   through to `package\.json$`'s neighbours and eventually to
//!   `\.json$` → `config_json`.
//!
//! The fourth is `README$` + `p.upper() == 'README'`, and it is the one
//! that repays reading twice, because the guard and the pattern
//! disagree about case and the pattern wins.
//!
//! `p.upper() == 'README'` is written as though any casing were
//! acceptable — and the guard does accept any casing. But it is only
//! consulted for a path the *pattern* already matched, and `README$` is
//! case-sensitive. So the guard's tolerance is unreachable: measured
//! against CPython, `README` is `readme` while **`readme` and `ReadMe`
//! are `None`**. A port that read the guard and concluded
//! "case-insensitive basename `README`" would be wrong on two of those
//! three inputs, and a port that read only the pattern would be right
//! by accident.
//!
//! The pattern is also **unanchored at the start**, so `docs/README`
//! matches it and then fails the guard, because the guard compares the
//! whole relative path rather than its basename. The `continue` then
//! reaches `README\.md`, which needs the extension, and nothing below
//! matches an extensionless name — so `docs/README` classifies as
//! nothing at all. All of this is measured against CPython in
//! `classify_parity.rs` rather than reasoned about here.
//!
//! # Why `regex` rather than a hand-written matcher
//!
//! The patterns are simple — most are a literal suffix, some an
//! alternation of suffixes, a few an unanchored substring, a handful a
//! basename with `(^|/)` — and a hand-written matcher over fifteen or so
//! shapes is expressible. It was rejected on verification grounds rather
//! than on effort.
//!
//! Compiling the pattern strings means the table in this file can be
//! diffed against `scanner.py`'s table line by line, and a transcription
//! error is a visible textual difference. A shape-classified table turns
//! all 179 entries into judgment calls, each of which is a place to be
//! subtly wrong — `(^|/)Dockerfile\.` is a path *segment* prefix and not
//! a basename prefix, `\.prettierrc` is unanchored at both ends,
//! `\.env` matches `.environment` — and none of those mistakes looks
//! like anything in a review. The `README` guard above is the proof
//! rather than the hypothetical: it was transcribed from the source
//! correctly and *described* wrongly in the first draft of this file,
//! and only the oracle caught it.
//!
//! The cost is four crates (`regex`, `regex-automata`, `regex-syntax`,
//! `aho-corasick`) that were not in `Cargo.lock`, which is the first
//! dependency this port has added for a reason other than "already in
//! the lock file". Stated rather than slipped in: see the declaration in
//! the workspace manifest.
//!
//! # Where Python's regex dialect and Rust's differ, and why it does not bite
//!
//! Three differences exist between `re` and the `regex` crate across
//! this table's feature set, and all three are inert here:
//!
//! 1. **`$`.** Python's matches at end of string *or* immediately before
//!    a trailing newline; Rust's (without `(?m)`) matches only at end of
//!    haystack. So `"a.py\n"` is `python` to Python and `None` here. No
//!    live `project_files.file_path` contains a newline — measured, 0 of
//!    2,786 on 2026-11-19 — and a path that did would be unopenable
//!    on the filesystem the column describes. Pinned by a test so the
//!    difference is recorded rather than assumed away.
//! 2. **Alternation semantics.** Python backtracks leftmost-first; the
//!    `regex` crate is leftmost-first for the overall match but uses a
//!    finite automaton. Inert because this module only ever asks
//!    *whether* a pattern matches, never *what* it matched, and the two
//!    engines agree on that for every regular pattern.
//! 3. **`\d`.** Unicode-aware by default in both, so
//!    `is_ignored_path`'s `\.tmp\.\d+` accepts Devanagari digits on
//!    both sides. Reproduced rather than narrowed to `[0-9]`.
//!
//! # `is_ignored_path` is a *policy* list and the port carries it as one
//!
//! `SKIP_FILE_PATTERNS` is **six** patterns — not five, and the
//! miscount is available to anyone reading the source, because the last
//! line holds two of them (`r'\.orig$', r'\.rej$'`). They name
//! atomic-write debris, Emacs lock symlinks, editor backups,
//! `.DS_Store` and merge/patch debris. Python's comment is explicit
//! that these are "policy, not accident", and `file retype` is the one
//! caller that *reports* them rather than skipping them silently — it
//! prints them as debris and points at `file rm`.
//!
//! The two functions are **not** complementary and nothing here treats
//! them as such. Measured: `.env~` is ignored *and* classifies as
//! `env_file`, because `\.env` is unanchored; `src/main.rs.orig` is
//! ignored and classifies as nothing; `src/main.rs` is neither. All
//! four quadrants are populated, so `file retype` has to ask both
//! questions rather than deriving one from the other. See
//! `classify_parity.rs`, which runs both functions over every live path
//! and over a crafted corpus.

use regex::Regex;
use std::sync::OnceLock;

/// `scanner.py`'s `GENERIC_TEXT_TYPE`.
///
/// Reached by `file retype` for a path [`classify_path`] cannot name at
/// all. Python's comment on the constant is about `scan_directory`
/// rather than about this command: returning `None` means "I do not
/// recognise this", which is not the same as "this is not part of the
/// project", and conflating the two is how a `justfile` disappeared from
/// a commit that reported success with two files.
pub const GENERIC_TEXT_TYPE: &str = "text_file";

/// `scanner.py`'s `FILE_TYPE_PATTERNS`, in order, as
/// `(pattern, type_name, guard)`.
///
/// Transcribed from
/// `importer/scanner.py:21`–`276` so that a line-by-line diff against
/// that list is possible. The section comments are Python's own, kept
/// for the same reason: they are how a reader finds the entry they want
/// in 240 lines of regex.
#[rustfmt::skip]
const FILE_TYPE_PATTERNS: &[(&str, &str, Option<Guard>)] = &[
    // Binary assets (SVG is technically text/xml but treat uniformly with other assets)
    (r"\.svg$", "image_svg", None),
    (r"\.png$", "image_png", None),
    (r"\.jpe?g$", "image_jpg", None),
    (r"\.gif$", "image_gif", None),
    (r"\.webp$", "image_webp", None),
    (r"\.ico$", "image_ico", None),
    (r"\.(woff2?|ttf|otf|eot)$", "font", None),
    (r"\.pdf$", "pdf", None),
    (r"\.wasm$", "wasm", None),
    (r"\.gpg$", "binary_encrypted", None),
    (r"\.asc$", "text_asc", None),
    (r"\.(mp3|wav|ogg|flac)$", "audio", None),
    (r"\.(mp4|webm|mov|mkv)$", "video", None),
    (r"\.(zip|tar|tar\.gz|tgz|tar\.bz2|7z)$", "archive", None),

    // SQL files
    (r"schema\.sql$", "sql_schema", None),
    (r"\.sql$", "sql_migration", Some(Guard::PathHoldsMigration)),
    (r"\.sql$", "sql_file", None),  // Catch all SQL files

    // React/Component files
    (r"\.jsx$", "jsx_component", None),
    (r"\.tsx$", "tsx_component", None),

    // TypeScript/JavaScript
    (r"\.test\.(ts|js)$", "test_file", None),
    (r"\.spec\.(ts|js)$", "spec_file", None),
    (r"\.ts$", "typescript", Some(Guard::NotASupabaseFunction)),
    (r"supabase/functions/.*\.(ts|js)$", "edge_function", None),
    (r"jest\.config\.js$", "jest_config", None),
    (r"webpack\.config\.js$", "webpack_config", None),
    (r"vite\.config\.js$", "vite_config", None),
    (r"rollup\.config\.js$", "rollup_config", None),
    (r"\.cjs$", "javascript", None),
    (r"\.mjs$", "javascript", None),
    (r"\.js$", "javascript", None),

    // Shell configurations
    (r"\.bashrc$", "bash_config", None),
    (r"\.bash_profile$", "bash_profile", None),
    (r"\.zshrc$", "zsh_config", None),
    (r"\.profile$", "shell_profile", None),
    (r"\.fish$", "fish_config", None),
    (r"\.aliases$", "shell_aliases", None),

    // Git configurations
    (r"\.gitignore$", "git_ignore", None),
    (r"\.gitattributes$", "git_attributes", None),
    (r"\.gitmodules$", "git_modules", None),
    (r"\.gitconfig$", "git_config", None),

    // Editor configurations
    (r"\.editorconfig$", "editor_config", None),
    (r"\.prettierrc", "prettier_config", None),
    (r"\.eslintrc", "eslint_config", None),
    (r"\.vimrc$", "vim_config", None),
    (r"init\.vim$", "nvim_config", None),
    (r"\.rubocop\.ya?ml$", "rubocop", None),
    (r"\.stylelintrc", "stylelint", None),
    (r"\.flake8$", "flake8_config", None),
    (r"\.babelrc", "babel_config", None),
    (r"\.browserslistrc$", "browserslist", None),
    (r"settings\.json$", "vscode_settings", Some(Guard::PathHoldsVscode)),

    // Emacs Lisp
    (r"\.spacemacs$", "emacs_config", None),
    (r"\.el$", "emacs_lisp", None),

    // Version managers
    (r"\.nvmrc$", "nvm_rc", None),
    (r"\.ruby-version$", "ruby_version", None),
    (r"\.python-version$", "python_version", None),
    (r"\.node-version$", "node_version", None),

    // CI/CD configurations
    (r"\.travis\.ya?ml$", "travis_ci", None),
    (r"\.gitlab-ci\.ya?ml$", "gitlab_ci", None),
    (r"\.github/workflows/.*\.ya?ml$", "github_workflow", None),
    (r"\.circleci/config\.ya?ml$", "circleci_config", None),
    (r"Jenkinsfile$", "jenkins_file", None),
    (r"dependabot\.ya?ml$", "dependabot", None),

    // Programming languages
    (r"\.rs$", "rust", None),
    (r"\.go$", "go", None),
    (r"\.rb$", "ruby", None),
    (r"\.java$", "java", None),
    (r"\.kt$", "kotlin", None),
    (r"\.swift$", "swift", None),
    (r"\.c$", "c", None),
    (r"\.cpp$", "cpp", None),
    (r"\.cs$", "csharp", None),
    (r"\.php$", "php", None),
    (r"\.scala$", "scala", None),
    (r"\.clj$", "clojure", None),
    (r"\.ex$", "elixir", None),
    (r"\.erl$", "erlang", None),
    (r"\.hs$", "haskell", None),
    (r"\.lua$", "lua", None),
    // `.pl` is ambiguous between Perl and Prolog. templedb's own six are
    // Prolog — src/services/*_logic.pl, SWI-Prolog rule sets that are the
    // source of truth for deploy/env/nixos/test/merge/graph decisions — so
    // the naming convention discriminates before the Perl fallback.
    // Classification is path-only by design here, so a content sniff for
    // `:- ` is not available.
    (r"_logic\.pl$", "prolog_source", None),
    (r"\.(pro|plt)$", "prolog_source", None),
    (r"\.pl$", "perl", None),
    (r"\.r$", "r", None),

    // Package managers and dependencies
    (r"requirements\.txt$", "pip_requirements", None),
    (r"Pipfile$", "pipfile", None),
    (r"pyproject\.toml$", "poetry_config", None),
    (r"setup\.py$", "setup_py", None),
    (r"Cargo\.toml$", "cargo_toml", None),
    (r"Cargo\.lock$", "cargo_lock", None),
    (r"Gemfile$", "gemfile", None),
    (r"Gemfile\.lock$", "gemfile_lock", None),
    (r"go\.mod$", "go_mod", None),
    (r"go\.sum$", "go_sum", None),
    (r"flake\.lock$", "nix_flake_lock", None),
    (r"package-lock\.json$", "npm_lock", None),
    (r"pnpm-lock\.yaml$", "pnpm_lock", None),
    (r"yarn\.lock$", "yarn_lock", None),
    (r"\.lock$", "lock_file", None),
    (r"composer\.json$", "composer_json", None),
    (r"\.npmrc$", "npm_rc", None),
    (r"\.yarnrc$", "yarn_rc", None),
    (r"package\.json$", "package_json", None),

    // Build tools
    (r"Makefile$", "makefile", None),
    (r"Rakefile$", "rakefile", None),
    (r"build\.gradle$", "gradle_build", None),
    (r"pom\.xml$", "maven_pom", None),
    (r"CMakeLists\.txt$", "cmake", None),
    (r"tsconfig\.json$", "tsconfig", None),

    // Infrastructure as Code
    (r"\.tf$", "terraform", None),
    (r"\.tfvars$", "terraform_vars", None),
    (r"playbook\.ya?ml$", "ansible_playbook", None),
    (r"inventory\.ya?ml$", "ansible_inventory", None),
    (r"k8s\.ya?ml$", "kubernetes", None),
    (r"Chart\.ya?ml$", "helm_chart", None),

    // Database
    (r"schema\.prisma$", "prisma_schema", None),

    // Testing
    (r"pytest\.ini$", "pytest_config", None),

    // Styles
    (r"\.css$", "css", None),
    (r"\.scss$", "scss", None),
    (r"\.html$", "html", None),
    (r"\.htm$", "html", None),
    // Server-side includes. Same content as .html as far as this table is
    // concerned, and load-bearing: woofspetsalon.com is five pages that
    // each `<!--#include file="/header.shtml">` three partials, and those
    // four files were the only ones a commit of the site refused, so the
    // tracked copy would have rendered every page with no header and no
    // navbar. A name this table has never heard of is silently left out
    // of the commit, which is the worst place to learn a new extension.
    (r"\.shtml$", "html", None),
    (r"\.shtm$", "html", None),
    // The partial's own template is the same content type. Deliberately
    // specific rather than a generic `\.template$` rule: that would also
    // claim nginx.conf.template and friends and call them HTML.
    //
    // Reachable, and checked rather than assumed: `\.shtml$` two
    // entries up is anchored at the end, so it does not match
    // `x.shtml.template`, and nothing else above here does either. The
    // near-miss the comment is about — `nginx.conf.template`, which
    // classifies as nothing at all — is pinned by a test.
    (r"\.shtml\.template$", "html", None),

    // Docker
    (r"Dockerfile$", "docker_file", None),
    (r"docker-compose\.ya?ml$", "docker_compose", None),

    // Nix
    (r"flake\.nix$", "nix_flake", None),
    (r"shell\.nix$", "nix_shell", None),
    (r"default\.nix$", "nix_default", None),
    (r"\.nix$", "nix_file", None),

    // Platform-specific
    (r"Procfile$", "procfile", None),
    (r"vercel\.json$", "vercel_config", None),
    (r"netlify\.toml$", "netlify_config", None),
    (r"railway\.json$", "railway_config", None),

    // Configuration
    (r"\.env", "env_file", None),

    // Deployment files
    (r"\.service$", "systemd_service", None),
    (r"ecosystem\.config\.js$", "pm2_config", None),
    (r"deploy\.sh$", "deployment_script", None),
    (r"deploy\.py$", "deployment_script", None),
    (r"templedb-deploy\.sh$", "deployment_script", None),
    (r"deployment.*\.ya?ml$", "deployment_config", None),

    // Task runners and extensionless build files. `Makefile` and `Dockerfile`
    // were always here; these are the ones that were not, and an unmatched
    // name is dropped silently — see `scan_directory`.
    (r"(^|/)[Jj]ustfile$", "justfile", None),
    (r"(^|/)Taskfile\.ya?ml$", "taskfile", None),
    (r"(^|/)Caddyfile$", "caddyfile", None),
    (r"(^|/)Vagrantfile$", "vagrantfile", None),
    (r"(^|/)Brewfile$", "brewfile", None),
    (r"(^|/)CODEOWNERS$", "codeowners", None),
    (r"(^|/)Dockerfile\.", "docker_file", None),   // Dockerfile.dev, .prod, …
    (r"(^|/)\.dockerignore$", "docker_ignore", None),
    (r"\.mk$", "makefile", None),

    // Documentation
    (r"README$", "readme", Some(Guard::WholePathIsReadme)),
    (r"README\.md", "markdown", None),
    (r"CHANGELOG\.md$", "changelog", None),
    (r"LICENSE", "license", None),                 // LICENSE, LICENSE.txt, …
    (r"CONTRIBUTING\.md$", "contributing", None),
    (r"SECURITY\.md$", "security_policy", None),

    // Misc
    (r"robots\.txt$", "robots_txt", None),
    (r"\.htaccess$", "htaccess", None),

    // Generic configs (catch-all - must be last)
    (r"\.ya?ml$", "config_yaml", None),
    (r"\.jsonc$", "config_json", None),
    (r"\.json$", "config_json", None),
    (r"\.toml$", "config_toml", None),
    (r"\.(ini|cfg|conf|properties)$", "config_file", None),

    // Interface definitions
    (r"\.proto$", "protobuf", None),
    (r"\.graphql$", "graphql", None),
    (r"\.xml$", "xml", None),

    // Scripts
    (r"\.sh$", "shell_script", None),
    (r"\.(bash|zsh)$", "shell_script", None),
    (r"\.ps1$", "powershell", None),
    (r"\.(bat|cmd)$", "batch_script", None),
    (r"\.py$", "python", None),
    (r"\.exs?$", "elixir", None),
    (r"\.jl$", "julia", None),
    (r"\.zig$", "zig", None),

    // Documentation and plain text (catch-all)
    (r"\.md$", "markdown", None),
    (r"\.(rst|adoc|org)$", "document", None),
    (r"\.tex$", "latex", None),
    (r"\.(patch|diff)$", "patch", None),
    (r"\.csv$", "csv", None),
    (r"\.te?xt$", "text_file", None),
];

/// `scanner.py`'s `SKIP_FILE_PATTERNS`.
///
/// Python's comment is kept because it is the justification: these are
/// "policy, not accident", named deliberately the way `SKIP_DIRS` names
/// `node_modules`, so they are skipped silently rather than reported as
/// unrecognised. `SKIP_DIRS` itself is **not** ported here — it belongs
/// to `scan_directory`, which walks a tree, and nothing in this port
/// walks one for classification.
#[rustfmt::skip]
const SKIP_FILE_PATTERNS: &[&str] = &[
    r"\.tmp\.\d+(\.[0-9a-f]+)?$",   // foo.ts.tmp.241132.1784033891676
    r"(^|/)\.#",                    // Emacs lock symlinks
    r"~$",                          // editor backups
    r"(^|/)\.DS_Store$",
    r"\.orig$", r"\.rej$",          // merge/patch debris
];

/// The four `test_func` lambdas, as a closed enum rather than as
/// function pointers.
///
/// An enum because there are exactly four and the set is Python's: a
/// `fn(&str) -> bool` field would let a future entry carry a guard that
/// has no counterpart upstream, which is the one thing this table must
/// not be able to express.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Guard {
    /// `lambda p: 'migration' in p.lower()`.
    ///
    /// `.lower()` is Python's full Unicode fold and not
    /// `to_ascii_lowercase`, which matters for exactly the inputs
    /// `entity search` cares about; reproduced as `to_lowercase` for
    /// that reason. No live path exercises the difference.
    PathHoldsMigration,
    /// `lambda p: 'supabase/functions' not in p`. Case-sensitive, and
    /// the only *negative* guard in the table.
    NotASupabaseFunction,
    /// `lambda p: '.vscode' in p`. Note the leading dot, and note that
    /// it is a substring of the whole path rather than a directory
    /// test — `my.vscode-notes/settings.json` satisfies it.
    PathHoldsVscode,
    /// `lambda p: p.upper() == 'README'`.
    ///
    /// The whole relative path, not the basename. See the module header:
    /// this is why `docs/README` classifies as nothing at all.
    WholePathIsReadme,
}

impl Guard {
    fn holds(self, rel_path: &str) -> bool {
        match self {
            Guard::PathHoldsMigration => rel_path.to_lowercase().contains("migration"),
            Guard::NotASupabaseFunction => !rel_path.contains("supabase/functions"),
            Guard::PathHoldsVscode => rel_path.contains(".vscode"),
            Guard::WholePathIsReadme => rel_path.to_uppercase() == "README",
        }
    }
}

/// The compiled tables, built once.
///
/// `OnceLock` rather than a `lazy_static`-style macro or a per-call
/// compile. 240 patterns is ~1 ms to compile and `file retype` calls
/// [`classify_path`] once per active row — 2,413 times on the live
/// database today — so compiling per call would be the dominant cost of
/// the command by three orders of magnitude.
///
/// A panic on an unparseable pattern is deliberate and is the one place
/// this module can fail: the patterns are compile-time constants in this
/// file, so a failure here is a transcription bug in the table above and
/// not anything a caller did. `classify_parity.rs` compiles both tables
/// in a test, so the panic is reachable from `cargo test` rather than
/// only from a live run.
fn tables() -> &'static (Vec<Regex>, Vec<Regex>) {
    static TABLES: OnceLock<(Vec<Regex>, Vec<Regex>)> = OnceLock::new();
    TABLES.get_or_init(|| {
        let compile = |p: &str| {
            Regex::new(p).unwrap_or_else(|e| panic!("classify.rs: pattern {p:?} is not valid: {e}"))
        };
        (
            FILE_TYPE_PATTERNS
                .iter()
                .map(|(p, _, _)| compile(p))
                .collect(),
            SKIP_FILE_PATTERNS.iter().copied().map(compile).collect(),
        )
    })
}

/// The `file_types.type_name` for a project-relative path, or `None` for
/// a name the table does not recognise.
///
/// `scanner.py`'s `classify_path`. Python's docstring records why there
/// is only one of these now: `commit.py` used to carry a second, much
/// smaller extension map and fall back to `SELECT id FROM file_types
/// LIMIT 1` for anything it missed, which is why every `.rs` file in
/// templedb-rust is recorded as the first row of that table. Repairing
/// that is what `file retype` exists for.
///
/// Pure and path-only: it reads no file, so it cannot sniff content.
/// The `.pl` comment in the table above is the place that costs
/// something.
pub fn classify_path(rel_path: &str) -> Option<&'static str> {
    let (patterns, _) = tables();
    for (compiled, (_, type_name, guard)) in patterns.iter().zip(FILE_TYPE_PATTERNS) {
        if compiled.is_match(rel_path) {
            // `continue`, not `return None`. See the module header —
            // three of the four guards only produce a type at all
            // because of this.
            if let Some(guard) = guard {
                if !guard.holds(rel_path) {
                    continue;
                }
            }
            return Some(type_name);
        }
    }
    None
}

/// True for a path `SKIP_FILE_PATTERNS` declares out of scope.
///
/// `scanner.py`'s `is_ignored_path`. Not the complement of
/// [`classify_path`] and not a subset of it either: `a.py.orig` is
/// ignored and classifies as nothing, `a.orig` is ignored and
/// classifies as nothing, and `.DS_Store` is ignored while
/// `src/.DS_Store` is too. The two functions answer different
/// questions and `file retype` asks both.
pub fn is_ignored_path(rel_path: &str) -> bool {
    let (_, skips) = tables();
    skips.iter().any(|r| r.is_match(rel_path))
}

/// The number of entries in the ported `FILE_TYPE_PATTERNS`, for the
/// parity oracle to compare against CPython's `len()`.
///
/// Exported because a table that silently lost an entry would classify
/// almost everything correctly — the lost entry is one type among 240 —
/// and the only cheap way to notice is to count. The oracle compares
/// this against `len(scanner.FILE_TYPE_PATTERNS)` rather than against a
/// number written here, so the figure cannot go stale the way a prose
/// count does.
pub fn pattern_count() -> usize {
    FILE_TYPE_PATTERNS.len()
}

/// The same, for `SKIP_FILE_PATTERNS`.
pub fn skip_pattern_count() -> usize {
    SKIP_FILE_PATTERNS.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both tables compile.
    ///
    /// The only test in this module that could ever fail from a change
    /// to the table rather than from a change to a function, which is
    /// why it is first: a bad pattern panics inside [`tables`], and
    /// without this the panic would first be reached by a live run.
    #[test]
    fn both_tables_compile() {
        let (patterns, skips) = tables();
        assert_eq!(patterns.len(), FILE_TYPE_PATTERNS.len());
        assert_eq!(skips.len(), SKIP_FILE_PATTERNS.len());
        assert_eq!(pattern_count(), patterns.len());
        assert_eq!(skip_pattern_count(), skips.len());
    }

    /// Every `type_name` in the table is a plausible `file_types.type_name`.
    ///
    /// Not a parity assertion — it is a guard against a transcription
    /// that put a regex in the name column or a name in the regex
    /// column, which is the one transcription error that would compile,
    /// classify everything and be wrong about every answer.
    #[test]
    fn no_type_name_looks_like_a_pattern() {
        for (pattern, type_name, _) in FILE_TYPE_PATTERNS {
            assert!(!type_name.is_empty(), "{pattern}");
            assert!(
                type_name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "{type_name:?} from {pattern:?} is not a bare snake_case name"
            );
        }
    }

    /// Order decides the answer in five places a reader would not guess.
    ///
    /// Each of these is a pair where a later entry also matches, so
    /// sorting the table — by specificity, by extension, by anything —
    /// would change the answer.
    #[test]
    fn the_first_matching_entry_wins() {
        // `schema\.sql$` before the two `\.sql$` rules.
        assert_eq!(classify_path("schema.sql"), Some("sql_schema"));
        assert_eq!(classify_path("db/schema.sql"), Some("sql_schema"));
        // `\.test\.(ts|js)$` before `\.ts$`.
        assert_eq!(classify_path("a.test.ts"), Some("test_file"));
        assert_eq!(classify_path("a.spec.js"), Some("spec_file"));
        // `_logic\.pl$` before `\.pl$`.
        assert_eq!(
            classify_path("src/services/deploy_logic.pl"),
            Some("prolog_source")
        );
        assert_eq!(classify_path("script.pl"), Some("perl"));
        // An SVG is text and classifies as a binary asset, because its
        // entry is first in the whole table.
        assert_eq!(classify_path("logo.svg"), Some("image_svg"));
        // `flake\.lock$` before the generic `\.lock$`.
        assert_eq!(classify_path("flake.lock"), Some("nix_flake_lock"));
        assert_eq!(classify_path("Cargo.lock"), Some("cargo_lock"));
        assert_eq!(classify_path("other.lock"), Some("lock_file"));
    }

    /// A failed guard falls through to the rest of the table.
    ///
    /// The behaviour the module header calls out as the reason three of
    /// the four guards produce anything. Written as four pairs, because
    /// each guard's fallthrough lands somewhere different.
    #[test]
    fn a_failed_guard_continues_rather_than_returning_none() {
        // `migration` in the path → sql_migration; otherwise the
        // catch-all `\.sql$` one line down.
        assert_eq!(classify_path("migrations/001_x.sql"), Some("sql_migration"));
        assert_eq!(classify_path("queries/report.sql"), Some("sql_file"));
        // The guard is `.lower()`, so the case of the path does not
        // matter.
        assert_eq!(classify_path("MIGRATION/x.sql"), Some("sql_migration"));

        // A `.ts` under `supabase/functions` fails the negative guard
        // and lands on the entry below it.
        assert_eq!(classify_path("src/a.ts"), Some("typescript"));
        assert_eq!(
            classify_path("supabase/functions/hello/index.ts"),
            Some("edge_function")
        );
        // …and the same path with `.js` never reaches the guard at all,
        // because `\.ts$` does not match it: `\.js$` is *below*
        // `supabase/functions/.*\.(ts|js)$`, so it is `edge_function`.
        assert_eq!(
            classify_path("supabase/functions/hello/index.js"),
            Some("edge_function")
        );

        // `.vscode` in the path → vscode_settings; otherwise the
        // generic `\.json$`.
        assert_eq!(
            classify_path(".vscode/settings.json"),
            Some("vscode_settings")
        );
        assert_eq!(classify_path("app/settings.json"), Some("config_json"));
    }

    /// `README`'s guard is case-insensitive and its pattern is not, so
    /// only the uppercase spelling at the project root is `readme`.
    ///
    /// Two findings in one test, both measured against CPython and both
    /// wrong in the first draft of this module. The guard is
    /// `p.upper() == 'README'`, which reads as "any casing"; it is
    /// consulted only for paths `README$` already matched, and that
    /// pattern is case-sensitive, so the tolerance is dead. And the
    /// guard compares the whole relative path rather than the basename,
    /// so a `README` one directory down classifies as nothing at all —
    /// the one case in this table where a perfectly ordinary path
    /// returns `None`.
    #[test]
    fn readme_needs_the_uppercase_spelling_at_the_root_and_nothing_else_is_one() {
        assert_eq!(classify_path("README"), Some("readme"));
        // The guard would accept these; the pattern never lets them
        // reach it. Nothing below matches an extensionless name.
        assert_eq!(classify_path("readme"), None);
        assert_eq!(classify_path("ReadMe"), None);
        // Matches `README$`, fails the guard, continues — and again
        // nothing below matches an extensionless name.
        assert_eq!(classify_path("docs/README"), None);
        // With an extension it is caught by `README\.md` (unanchored,
        // so `README.md.bak` too) and then by the generic markdown rule.
        assert_eq!(classify_path("docs/README.md"), Some("markdown"));
        assert_eq!(classify_path("README.md"), Some("markdown"));
        assert_eq!(classify_path("docs/README.rst"), Some("document"));
    }

    /// Eight entries are unanchored substrings and match more than
    /// their name suggests.
    ///
    /// `\.env`, `LICENSE`, `README\.md`, `\.prettierrc`, `\.eslintrc`,
    /// `\.babelrc`, `\.stylelintrc` and `(^|/)Dockerfile\.` have no `$`.
    /// Reproduced, and pinned here because "the pattern for `.env` also
    /// claims `.environment`" is the sort of thing a reimplementation
    /// tidies without noticing.
    #[test]
    fn the_unanchored_entries_match_more_than_their_names_suggest() {
        assert_eq!(classify_path(".env"), Some("env_file"));
        assert_eq!(classify_path(".env.production"), Some("env_file"));
        // `.environment` — the pattern has no `$`.
        assert_eq!(classify_path(".environment"), Some("env_file"));
        assert_eq!(classify_path("LICENSE"), Some("license"));
        assert_eq!(classify_path("LICENSE.txt"), Some("license"));
        // `LICENSES/MIT.txt` — a *directory* called LICENSES.
        assert_eq!(classify_path("LICENSES/MIT.txt"), Some("license"));
        assert_eq!(classify_path(".prettierrc.json"), Some("prettier_config"));
        assert_eq!(classify_path("Dockerfile.dev"), Some("docker_file"));
        assert_eq!(classify_path("Dockerfile"), Some("docker_file"));
    }

    /// `(^|/)` entries are path-*segment* anchored, not basename
    /// anchored.
    #[test]
    fn the_segment_anchored_entries_accept_a_subdirectory() {
        assert_eq!(classify_path("justfile"), Some("justfile"));
        assert_eq!(classify_path("Justfile"), Some("justfile"));
        assert_eq!(classify_path("tools/justfile"), Some("justfile"));
        // `JUSTFILE` is not in the character class — Python's is
        // `[Jj]ustfile`, two cases and not three.
        assert_eq!(classify_path("JUSTFILE"), None);
        assert_eq!(classify_path("ops/Caddyfile"), Some("caddyfile"));
        assert_eq!(classify_path(".dockerignore"), Some("docker_ignore"));
    }

    /// An unrecognised name is `None`, which is the input `file retype`
    /// turns into [`GENERIC_TEXT_TYPE`].
    #[test]
    fn an_unrecognised_name_is_none() {
        assert_eq!(classify_path("Session.vim"), None);
        assert_eq!(classify_path("a.unknownextension"), None);
        assert_eq!(classify_path("noextension"), None);
        assert_eq!(classify_path(""), None);
        assert_eq!(GENERIC_TEXT_TYPE, "text_file");
    }

    /// `is_ignored_path` is its own question and is neither the
    /// complement of `classify_path` nor a subset of it.
    #[test]
    fn ignored_and_classifiable_are_independent() {
        // Atomic-write debris, with and without the hex suffix.
        assert!(is_ignored_path("a.ts.tmp.241132"));
        assert!(is_ignored_path("a.ts.tmp.241132.1784033891676"));
        assert!(!is_ignored_path("a.ts.tmp"));
        // Emacs lock symlinks, at the root and in a subdirectory.
        assert!(is_ignored_path(".#foo.el"));
        assert!(is_ignored_path("src/.#foo.el"));
        assert!(!is_ignored_path("src/a.#b"));
        // Editor backups.
        assert!(is_ignored_path("a.py~"));
        assert!(is_ignored_path("~"));
        assert!(!is_ignored_path("a~.py"));
        assert!(is_ignored_path(".DS_Store"));
        assert!(is_ignored_path("src/.DS_Store"));
        assert!(is_ignored_path("a.orig"));
        assert!(is_ignored_path("a.rej"));
        assert!(!is_ignored_path("src/main.rs"));

        // All four quadrants, measured against CPython. Ignored *and*
        // classifiable happens when the classifying pattern is one of
        // the unanchored ones, because the `~` is then not in its way.
        assert!(is_ignored_path(".env~"));
        assert_eq!(classify_path(".env~"), Some("env_file"));
        // Ignored and unclassifiable, which is the common case for
        // debris: `\.orig$` is the suffix, so no type rule matches.
        assert!(is_ignored_path("src/main.rs.orig"));
        assert_eq!(classify_path("src/main.rs.orig"), None);
        // Not ignored and classifiable.
        assert!(!is_ignored_path("src/main.rs"));
        assert_eq!(classify_path("src/main.rs"), Some("rust"));
        // Not ignored and unclassifiable — the input `file retype`
        // turns into `text_file`.
        assert!(!is_ignored_path("Session.vim"));
        assert_eq!(classify_path("Session.vim"), None);
    }

    /// Python's `$` matches before a trailing newline and Rust's does
    /// not, and this is the test that records it.
    ///
    /// Inert on the live data — 0 of 2,786 `project_files.file_path`
    /// values contain a newline, measured 2026-11-19 — and the
    /// divergence is asserted here rather than argued away, because a
    /// reader who assumes `re` and `regex` agree on `$` will be wrong
    /// about this one input and nothing else in the table would tell
    /// them.
    #[test]
    fn a_trailing_newline_is_the_one_place_the_two_regex_dialects_differ() {
        assert_eq!(classify_path("a.py"), Some("python"));
        // Python's `re.search(r'\.py$', 'a.py\n')` matches; Rust's does
        // not. The fallthrough reaches nothing, so this is `None`.
        assert_eq!(classify_path("a.py\n"), None);
        // Same shape in the skip table.
        assert!(is_ignored_path("a.orig"));
        assert!(!is_ignored_path("a.orig\n"));
    }

    /// `\.shtml\.template$` is reachable, which the table's own comment
    /// had to be corrected about.
    #[test]
    fn the_shtml_template_entry_fires() {
        assert_eq!(classify_path("header.shtml"), Some("html"));
        assert_eq!(classify_path("header.shtm"), Some("html"));
        assert_eq!(classify_path("header.shtml.template"), Some("html"));
        // And the near-miss that makes the entry necessary: the generic
        // rules below it do not claim a `.template`.
        assert_eq!(classify_path("nginx.conf.template"), None);
    }

    /// The five patterns that end the table are the catch-alls, and
    /// `\.te?xt$` is last.
    ///
    /// A regression guard on the one property the whole table depends
    /// on: if any generic rule migrated upward, dozens of specific
    /// types would stop being reachable at once.
    #[test]
    fn the_catch_alls_are_still_at_the_bottom() {
        let last = FILE_TYPE_PATTERNS.last().unwrap();
        assert_eq!(last.0, r"\.te?xt$");
        assert_eq!(last.1, "text_file");
        // `requirements.txt` is `pip_requirements` and not `text_file`,
        // which is only true while the generic text rule stays below it.
        assert_eq!(classify_path("requirements.txt"), Some("pip_requirements"));
        assert_eq!(classify_path("notes.txt"), Some("text_file"));
        assert_eq!(classify_path("CMakeLists.txt"), Some("cmake"));
        assert_eq!(classify_path("robots.txt"), Some("robots_txt"));
    }
}
