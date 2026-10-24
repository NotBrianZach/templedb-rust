//! `file set` — the port's first write.
//!
//! Chosen by the write-path design document over `vcs commit`, and the
//! reasoning is worth keeping in view: `commit` is "the widest door in
//! the system — workspace scan, three guards, conflict detection with
//! resolution strategies, session head bookkeeping, commit metadata,
//! and per-file writes across four tables". This is one logical change,
//! no workspace scan, no resolution strategies, and its entire effect
//! is visible through read commands the port already verifies byte for
//! byte — `file cat`, `source revisions`, `search content`. That last
//! property is the one that matters, because it is the only
//! verification technique available without new machinery: the document
//! calls it *cross-read*.
//!
//! # What this does NOT do
//!
//! Stated first, because the design document's sharpest finding is that
//! **the guards are the deliverable** — "a port that reproduces the SQL
//! and not these is a regression even if every byte of output matches".
//! This reproduces the SQL and five of the seven guards. The rest are
//! absent, and the writer is behind an opt-in
//! ([`OPT_IN_ENV`](crate::OPT_IN_ENV)) precisely because that is not
//! visible from a command line:
//!
//! | absent | what it protects against |
//! |---|---|
//! | auto-staging and `--stage` | needs a staged-by session, and a decision about what an unpinned `--stage` should own |
//! | `--session` | the pin is resolved; nothing here reads it yet |
//! | `--commit` / `-m` | needs `vcs commit`, which is the step after this one |
//! | `deploy_stage_runs` | chain observability; outside the subset |
//! | binary content | Python falls back to `content_blob` for undecodable stdin. This takes text only |
//!
//! Five guards *are* here: `--if-match` optimistic concurrency, the
//! un-delete that revives a soft-deleted row, the mojibake check,
//! `--verify`, and the `edit_intents` record. Each is below with the
//! incident that produced it.
//!
//! # Postgres, not the live SQLite
//!
//! This writes the port's own disposable Postgres. Python keeps sole
//! authority over `~/.local/share/templedb/templedb.sqlite`, which is
//! the arrangement every earlier step of the port was built to
//! preserve. A wrong write here costs a `just mirror-refresh`.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::pgtext;
use crate::session::SessionId;

/// What a write did, for the caller to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetOutcome {
    /// sha256 of the content now stored.
    pub hash: String,
    /// The previous `is_current` hash, if the file had one.
    pub previous: Option<String>,
    /// True when the content was already what is stored, so the write
    /// changed nothing. Python's `deploy_stage_runs` classifies this as
    /// `noop`; the port reports it rather than recording it.
    pub unchanged: bool,
    /// True when `project_files.status` was revived from a non-active
    /// value.
    pub undeleted: bool,
    /// True when the file did not exist and a `project_files` row was
    /// created.
    pub created: bool,
    pub size_bytes: i64,
    pub line_count: i64,
    /// The `edit_intents` row this write recorded, or `None` when
    /// `--skip-intent` was passed. Python prints it as `intent: #N` "so
    /// scripted callers can inspect / cancel it later", which is the
    /// only reason it is carried out of here.
    pub intent_id: Option<i64>,
}

/// sha256 of the UTF-8 bytes, lowercase hex — the same digest Python
/// takes, over the same bytes.
pub fn content_hash(content: &str) -> String {
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    // Hex by hand rather than `{:x}` on the output: sha2 0.11 returns a
    // `hybrid_array::Array`, which does not implement `LowerHex`. The
    // zero-padded two-digit form is the one Python's `hexdigest()`
    // produces and the one `content_blobs.hash_sha256` stores.
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Python's line count: `content.count('\n') + 1`, and 0 for empty.
///
/// Not the number of lines by any ordinary definition — a file ending
/// in a newline counts the empty string after it — but it is the number
/// `file ls -l` and `source revisions` print, so it is the number to
/// reproduce.
pub fn line_count(content: &str) -> i64 {
    if content.is_empty() {
        0
    } else {
        content.matches('\n').count() as i64 + 1
    }
}

/// Sequences in `text` that are valid UTF-8 when encoded back to
/// Latin-1 — the signature of double-encoded UTF-8.
///
/// # The failure this detects
///
/// UTF-8 read as Latin-1 and written back as UTF-8 doubles every
/// non-ASCII character: an em dash's `E2 80 94` becomes
/// `C3 A2 C2 80 C2 94`. The result is **still valid UTF-8**, so every
/// decode on the way into the database accepts it, the hash matches,
/// and `--verify` passes. It renders as a stray `â` plus two C1
/// control characters that browsers draw as nothing.
///
/// `bza`'s `frontend/app/page.tsx` was corrupted this way on 2026-09-29
/// and shipped for two weeks before anyone noticed a button reading
/// `â   done` instead of `✓ done`. The cause was not TempleDB: a
/// `perl -0pi -e` one-liner reads the file as bytes, so inserting one
/// literal above U+00FF upgrades the whole buffer and re-encodes all of
/// it. Perl says "Wide character in print" and that warning went to a
/// discarded stderr. The hazard lives in throwaway shell, so the only
/// place to catch it is where content enters the database.
///
/// # Reproducing Python's regex without a regex
///
/// Python compiles three ordered alternatives over *characters* in the
/// Latin-1 range:
///
/// ```text
/// [U+00C2-U+00DF][U+0080-U+00BF]
/// [U+00E0-U+00EF][U+0080-U+00BF]{2}
/// [U+00F0-U+00F4][U+0080-U+00BF]{3}
/// ```
///
/// `finditer` scans left to right, tries the alternatives in order at
/// each position, and resumes after a match rather than inside it. That
/// ordering is reproduced here: the two-character form is tried first,
/// and a match advances the cursor past the whole run. Scanning by
/// character and not by byte is the other half — these are `char`
/// comparisons against a `str`, so a genuinely multi-byte character
/// never half-matches.
///
/// Each hit is returned as the character it would decode *back* to, so
/// a caller can say "this looks like it should be —" rather than
/// dumping bytes. A run that matches the shape but is not re-decodable
/// is ordinary text and is dropped, which is Python's `except` arm.
pub fn mojibake_runs(text: &str) -> Vec<String> {
    const CONT: std::ops::RangeInclusive<char> = '\u{80}'..='\u{bf}';
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        // Ordered exactly as Python's alternation: lead range, then how
        // many continuations it requires.
        let run = [
            ('\u{c2}'..='\u{df}', 1usize),
            ('\u{e0}'..='\u{ef}', 2),
            ('\u{f0}'..='\u{f4}', 3),
        ]
        .into_iter()
        .find_map(|(lead, conts)| {
            if !lead.contains(&chars[i]) {
                return None;
            }
            // The run needs `chars[i + 1 ..= i + conts]`, so the last
            // index touched is `i + conts`.
            if i + conts >= chars.len() {
                return None;
            }
            if (1..=conts).all(|k| CONT.contains(&chars[i + k])) {
                Some(conts + 1)
            } else {
                None
            }
        });
        match run {
            Some(len) => {
                // Latin-1 encode: every matched char is <= U+00FF by
                // construction, so this cannot fail — which is why the
                // only fallible step is the UTF-8 decode.
                let bytes: Vec<u8> = chars[i..i + len].iter().map(|&c| c as u8).collect();
                if let Ok(decoded) = String::from_utf8(bytes) {
                    out.push(decoded);
                }
                i += len;
            }
            None => i += 1,
        }
    }
    out
}

/// Refuse content that *newly* looks double-encoded.
///
/// The test is whether this write **increases** the count, not whether
/// any is present. A file that already contains such sequences —
/// genuinely, or because it is already corrupt — has to stay editable,
/// and a repair that removes them has to pass. Only going up is
/// suspicious.
///
/// `previous` is `None` for a new file, which has no baseline; Python
/// treats a failed pre-read the same way, so an unreadable base cannot
/// block a write.
pub fn check_mojibake(content: &str, previous: Option<&str>) -> Result<()> {
    let new_runs = mojibake_runs(content);
    if new_runs.is_empty() {
        return Ok(());
    }
    let prev_runs = previous.map_or_else(Vec::new, mojibake_runs);
    if new_runs.len() <= prev_runs.len() {
        return Ok(());
    }
    let added = new_runs.len() - prev_runs.len();
    // Python shows up to eight distinct decoded characters, sorted.
    let mut sample: Vec<&String> = new_runs.iter().collect();
    sample.sort();
    sample.dedup();
    let sample: Vec<String> = sample
        .into_iter()
        .take(8)
        .map(|s| format!("{s:?}"))
        .collect();
    Err(anyhow::Error::new(crate::Coded::refused(format!(
        "content looks like double-encoded UTF-8 ({added} new sequence{}; \
         the file had {}).\n\
         \x20 These decode back to: {}\n\
         \x20 That is UTF-8 read as Latin-1 and written back as UTF-8. It is \
         valid UTF-8, so nothing downstream will catch it.\n\
         \x20 Usual cause: `perl -0pi -e` inserting a literal above U+00FF, \
         which upgrades the whole buffer and re-encodes every pre-existing \
         character. Check for 'Wide character in print'.\n\
         \x20 If the sequences are genuinely what you meant, pass \
         --allow-mojibake.",
        if added == 1 { "" } else { "s" },
        prev_runs.len(),
        sample.join(" ")
    ))))
}

/// Validate an `--if-match` argument.
///
/// Python accepts a prefix of at least 8 hex characters, lowercased,
/// "because every place that shows a hash truncates it — `file where`,
/// `source snapshot --meta`, doctor output — so requiring 64 characters
/// would mean nobody uses the flag".
pub fn normalise_if_match(expected: &str) -> Result<String> {
    let exp = expected.trim().to_ascii_lowercase();
    if exp.len() < 8 || !exp.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("--if-match wants at least 8 hex chars of a sha256, got {expected:?}");
    }
    Ok(exp)
}

/// Apply the `--if-match` guard against the hash currently stored.
///
/// # Why this guard exists
///
/// `file set` replaces the **whole** file, so content fetched earlier
/// is effectively a lock on all of it: anything written between the
/// read and the write is reverted silently, with no conflict and a
/// clean `--verify` — which only checks that what you wrote is what
/// landed, not that you meant to drop the rest.
///
/// That happened twice on 2026-10-04 to `src/cli/commands/entity.py`, a
/// 5,239-line file four separate fixes were landing in, and the first
/// instance went unnoticed for three commits.
/// Both refusals exit 2, as Python's do — `Coded::conflict`. The
/// *invalid argument* case is a plain `bail!` and exits 1, which is
/// also Python's split: a malformed flag is bad input, a mismatched one
/// is a conflict.
pub fn check_if_match(expected: &str, current: Option<&str>) -> Result<()> {
    let exp = normalise_if_match(expected)?;
    let short = &exp[..exp.len().min(12)];
    match current {
        None => Err(anyhow::Error::new(crate::Coded::conflict(format!(
            "--if-match {short} given but the file has no current content — \
             it is a new file, so there is no base to match. Drop --if-match \
             if creating it is what you meant."
        )))),
        Some(cur) if !cur.starts_with(&exp) => {
            Err(anyhow::Error::new(crate::Coded::conflict(format!(
                "--if-match mismatch: expected {short}, the database \
                 currently holds {}. Something changed the file since you \
                 read it, and writing now would revert it.",
                &cur[..cur.len().min(12)]
            ))))
        }
        Some(_) => Ok(()),
    }
}

/// Read back `is_current` after the write and confirm it holds `hash`.
///
/// Python's `--verify`, and the gap it covers is narrow but real: the
/// write itself reported success, so the only thing left that could be
/// wrong is something *else* having changed the row — a trigger, or a
/// concurrent writer. Exits 2 on mismatch.
///
/// Called on a **fresh transaction after the commit**, deliberately.
/// Verifying inside the writing transaction would read the write's own
/// uncommitted effect and could only ever agree, which is a check that
/// cannot fail and therefore is not one.
pub fn verify_current(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    file_path: &str,
    hash: &str,
) -> Result<()> {
    match current_hash(tx, project_id, file_path)? {
        Some(found) if found == hash => Ok(()),
        Some(found) => Err(anyhow::Error::new(crate::Coded::conflict(format!(
            "--verify failed on {file_path}: wrote {}, but the database now \
             holds {}. Something changed the row between the write and this \
             read.",
            &hash[..12.min(hash.len())],
            &found[..12.min(found.len())]
        )))),
        None => Err(anyhow::Error::new(crate::Coded::conflict(format!(
            "--verify failed on {file_path}: wrote {} and the file now has no \
             current content at all.",
            &hash[..12.min(hash.len())]
        )))),
    }
}

/// The project id for an **exact** slug, in the write target.
///
/// Exact, where every read command accepts a fuzzy name. Python's
/// `file set` fuzzy-matches too, and this is a deliberate divergence in
/// the stricter direction: fuzzy resolution on a write is how content
/// lands in the wrong project, and the scoring function has two known
/// bugs wrapped around it (see the README's fuzzy section). A reader
/// that guesses wrong wastes a second; a writer that guesses wrong
/// overwrites a file.
///
/// Resolved inside the write transaction rather than through the read
/// `Store`, because the reader and the writer point at *different
/// databases* — `TEMPLEDB_URL` is the parity target and
/// `TEMPLEDB_WRITE_URL` is the scratch. Resolving a slug against one
/// and writing to the other would be a genuinely nasty class of bug.
pub fn resolve_project_exact(tx: &mut postgres::Transaction<'_>, slug: &str) -> Result<i64> {
    match tx.query_opt("SELECT id FROM projects WHERE slug = $1", &[&slug])? {
        Some(row) => Ok(row.get(0)),
        None => bail!(
            "no project with slug {slug:?} in the write target.\n\
             `file set` takes an exact slug, not a fuzzy name — unlike every \
             read command — because a mis-resolved write overwrites a file."
        ),
    }
}

/// What a caller may vary about a write.
///
/// A struct rather than more positional parameters: `set_file` already
/// takes five, and `(.., Some("abc"), true)` at a call site says
/// nothing about which `true` that is.
#[derive(Debug, Clone, Copy, Default)]
pub struct SetOptions<'a> {
    /// Refuse unless the current hash starts with this.
    pub if_match: Option<&'a str>,
    /// Skip the double-encoding guard.
    pub allow_mojibake: bool,
    /// Do not record an `edit_intents` row. Python's `--skip-intent`.
    pub skip_intent: bool,
    /// The session this write belongs to, if the caller pinned one.
    /// Stored on the intent and nothing else — `--stage` is what would
    /// make it mean more, and that is not ported.
    pub session: Option<SessionId>,
    /// `edit_intents.author`: `TEMPLEDB_AUTHOR`, then `USER`, then
    /// NULL. Resolved at the entry point rather than read here, for the
    /// reason the clock and the session are — a query module that reads
    /// the environment cannot be tested at a fixed value.
    pub author: Option<&'a str>,
}

/// The `is_current` content *text* for one path, for the mojibake
/// baseline.
///
/// Separate from [`current_hash`] and called only when the new content
/// has runs in it, which is Python's order too — the pre-read is a
/// whole blob and most writes never need it.
fn current_text(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    file_path: &str,
) -> Result<Option<String>> {
    let row = tx.query_opt(
        "SELECT cb.content_text
           FROM project_files pf
           JOIN file_contents fc ON fc.file_id = pf.id AND fc.is_current = 1
           JOIN content_blobs cb ON cb.hash_sha256 = fc.content_hash
          WHERE pf.project_id = $1 AND pf.file_path = $2",
        &[&project_id, &file_path],
    )?;
    // Unescaped, because the baseline has to be compared in the same
    // representation as the incoming content. Comparing a run count
    // against the escaped form would be comparing two different
    // strings.
    Ok(row
        .and_then(|r| r.get::<_, Option<String>>(0))
        .map(|s| pgtext::unescape(&s).into_owned()))
}

/// The `is_current` content hash for one path, or `None`.
fn current_hash(
    tx: &mut postgres::Transaction<'_>,
    project_id: i64,
    file_path: &str,
) -> Result<Option<String>> {
    let row = tx.query_opt(
        "SELECT fc.content_hash
           FROM project_files pf
           JOIN file_contents fc ON fc.file_id = pf.id AND fc.is_current = 1
          WHERE pf.project_id = $1 AND pf.file_path = $2",
        &[&project_id, &file_path],
    )?;
    Ok(row.map(|r| r.get::<_, String>(0)))
}

/// What the write just did, for the intent to describe.
///
/// A struct rather than four more parameters: `record_intent` would
/// otherwise take eight, and `(.., &hash, size, previous)` at the one
/// call site says nothing about which `i64` is which.
struct Wrote<'a> {
    file_path: &'a str,
    hash: &'a str,
    size_bytes: i64,
    previous: Option<&'a str>,
}

/// Record this write in `edit_intents` and return the row's id.
///
/// Guard five of seven, and the one the design document describes as
/// provenance: "who set this, when, from which session". Python's
/// `_record_and_apply_intent`, with the same column values and a
/// different failure contract — see below.
///
/// # Inside the transaction, where Python's is after it
///
/// This is the one place the port deliberately departs from Python's
/// *structure* rather than from a value, so the reasoning matters.
///
/// Python records the intent **after** the write, in its own `execute`,
/// and swallows every error: "intent recording must not break the write
/// path". The ordering is load-bearing over there and the comment says
/// why — the prior arrangement wrote the intent first and caused the
/// 2026-09-14 *intent applied but `file cat` returns nothing* bug, where
/// a failed write left a `status = 'applied'` row that survived the
/// abort and lied to every later provenance query.
///
/// This port has something Python does not: the write is already one
/// transaction across three tables. Putting the intent inside it makes
/// that bug **unreachable by construction** rather than by ordering —
/// either the write and its provenance both exist or neither does, and
/// no sequence of failures can produce the lying row. The cost is the
/// property Python chose instead: here an intent failure *does* fail the
/// write.
///
/// That trade is the right way round for this command. A silent
/// provenance gap is exactly what the stale-base overwrite of
/// 2026-10-04 had no trace of, and `file set` is opt-in, partial and
/// behind an environment variable — a caller who wants the write without
/// the record can say so with `--skip-intent`, which is louder than
/// discovering later that the row is missing.
fn record_intent(
    tx: &mut postgres::Transaction<'_>,
    now: &str,
    project_id: i64,
    wrote: &Wrote<'_>,
    opts: &SetOptions<'_>,
) -> Result<i64> {
    let Wrote {
        file_path,
        hash,
        size_bytes,
        previous,
    } = *wrote;
    // `'new-file'` rather than NULL when there is no prior content,
    // which is Python's choice and a considered one: the two cases stay
    // distinguishable from a column that is merely unfilled. Worth
    // keeping because the column spent 886 rows holding the literal
    // `'current'` — migration 123 catalogues it as a constant — so "the
    // field designed to record what an edit was made against never
    // recorded anything", and a stale-base overwrite left nothing to
    // audit.
    let base_revision = previous.unwrap_or("new-file");
    // Python's `f"{len(content_bytes)} bytes"`, byte count and not
    // character count.
    let patch_summary = format!("{size_bytes} bytes");
    // `session_id` comes from the resolved pin, not from a second read
    // of the environment — and that is a real difference. Python's
    // intent path does *not* call `get_current_session()`; it reads
    // `TEMPLEDB_SESSION_ID` directly and swallows a parse failure to
    // NULL, so it will happily record an id for a session that has
    // ended. This records a live id or NULL, because there is one
    // resolver in this port and it checks liveness.
    let session = opts.session.map(SessionId::get);
    // `description` is the literal `'file set'` on Python's side: the
    // column says which surface made the edit, and this is that
    // surface. `status` is `'applied'` because the write above has
    // already happened — in this transaction, which is what makes the
    // claim true rather than hopeful.
    let row = tx.query_one(
        "INSERT INTO edit_intents
             (session_id, project_id, file_path, base_revision,
              new_content_hash, patch_summary, author, description,
              status, created_at, applied_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'file set', 'applied', $8, $8)
         RETURNING id",
        &[
            &session,
            &project_id,
            &file_path,
            &base_revision,
            &hash,
            &patch_summary,
            &opts.author,
            &now,
        ],
    )?;
    Ok(row.get(0))
}

/// Write `content` as the current contents of `project_id`/`file_path`.
///
/// Mirrors Python's `_write_content_to_db` across three tables, in the
/// order Python uses, inside one transaction — plus the `edit_intents`
/// record, which makes four. The ordering is not incidental: the blob
/// has to exist before a `file_contents` row references its hash, and
/// the intent goes last because it asserts that the write happened.
pub fn set_file(
    tx: &mut postgres::Transaction<'_>,
    clock: &Clock,
    project_id: i64,
    file_path: &str,
    content: &str,
    opts: &SetOptions<'_>,
) -> Result<SetOutcome> {
    let previous = current_hash(tx, project_id, file_path)?;
    // `--if-match` first, because it is the cheaper refusal and because
    // a caller who got the base wrong should hear about that rather
    // than about the content of a write that was never going to land.
    if let Some(expected) = opts.if_match {
        check_if_match(expected, previous.as_deref())?;
    }
    if !opts.allow_mojibake {
        // The baseline is only read when the incoming content has runs
        // at all, so an ordinary write does not pay for a blob fetch.
        if mojibake_runs(content).is_empty() {
            // Nothing to compare against; skip the pre-read entirely.
        } else {
            let prev_text = current_text(tx, project_id, file_path)?;
            check_mojibake(content, prev_text.as_deref())?;
        }
    }

    let hash = content_hash(content);
    let size_bytes = content.len() as i64;
    let lines = line_count(content);
    let stored = pgtext::escape(content);

    // `ON CONFLICT DO NOTHING`, which is Python's `INSERT OR IGNORE`.
    // Content is addressed by its own hash, so a blob that is already
    // there is already correct and re-writing it would be a no-op that
    // churned the row.
    //
    // `encoding` and `reference_count` are absent from the mirrored
    // subset, so they are not written. Python sets them — `'utf-8'` and
    // 0, the latter so SQLite's `increment_blob_reference` trigger can
    // count the `file_contents` row that follows. Neither column is read
    // by any ported command, so the boundary still follows the readers;
    // it is recorded in the README as partial fidelity rather than left
    // to be discovered.
    tx.execute(
        "INSERT INTO content_blobs
             (hash_sha256, content_text, content_blob, content_type, file_size_bytes)
         VALUES ($1, $2, NULL, 'text', $3)
         ON CONFLICT (hash_sha256) DO NOTHING",
        &[&hash, &stored.as_ref(), &size_bytes],
    )?;

    let existing = tx.query_opt(
        "SELECT id, status FROM project_files WHERE project_id = $1 AND file_path = $2",
        &[&project_id, &file_path],
    )?;

    let (file_id, undeleted, created) = match existing {
        Some(row) => {
            let file_id: i64 = row.get(0);
            let status: Option<String> = row.get(1);
            // Un-delete. `file set` on a soft-deleted row revives it;
            // without this the new blob lands and `file ls`, `publish
            // materialize` and everything else filtering on
            // `status = 'active'` silently skips the file. Found when
            // `set bza flake.nix` looked like it succeeded and shipped
            // nothing.
            let undeleted = status.as_deref() != Some("active");
            if undeleted {
                tx.execute(
                    "UPDATE project_files SET status = 'active' WHERE id = $1",
                    &[&file_id],
                )?;
            }
            (file_id, undeleted, false)
        }
        None => {
            // `file_type_id` is left NULL rather than derived from the
            // extension. Python maps 14 extensions onto `file_types`
            // rows, and that table is outside the mirrored subset — the
            // only ported reader of the column is `project show`'s
            // `COUNT(DISTINCT file_type_id)`, which counts a NULL as
            // nothing. Inventing an id would be worse than omitting
            // one.
            let row = tx.query_one(
                "INSERT INTO project_files (project_id, file_path, status)
                 VALUES ($1, $2, 'active')
                 RETURNING id",
                &[&project_id, &file_path],
            )?;
            (row.get::<_, i64>(0), false, true)
        }
    };

    // `ON CONFLICT (file_id, is_current)`, which the mirrored schema
    // declares because the live one does. Python's `datetime('now')`
    // becomes the injected clock: the whole point of the clock seam is
    // that a write's timestamps are equal by construction rather than
    // excluded from a state diff, and excluding them would hide the
    // columns most likely to be wrong.
    let now = clock.now_timestamp();
    tx.execute(
        "INSERT INTO file_contents
             (file_id, content_hash, file_size_bytes, line_count, is_current, updated_at)
         VALUES ($1, $2, $3, $4, 1, $5)
         ON CONFLICT (file_id, is_current) DO UPDATE SET
             content_hash    = excluded.content_hash,
             file_size_bytes = excluded.file_size_bytes,
             line_count      = excluded.line_count,
             updated_at      = excluded.updated_at",
        &[&file_id, &hash, &size_bytes, &lines, &now],
    )?;

    // Last, and inside the same transaction. See `record_intent` for
    // why that differs from Python's ordering and what it buys.
    let intent_id = if opts.skip_intent {
        None
    } else {
        Some(record_intent(
            tx,
            &now,
            project_id,
            &Wrote {
                file_path,
                hash: &hash,
                size_bytes,
                previous: previous.as_deref(),
            },
            opts,
        )?)
    };

    Ok(SetOutcome {
        unchanged: previous.as_deref() == Some(hash.as_str()),
        hash,
        previous,
        undeleted,
        created,
        size_bytes,
        line_count: lines,
        intent_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digest is over the UTF-8 bytes and nothing else, so it
    /// matches what Python's `hashlib.sha256(content.encode())`
    /// produces for the same string.
    #[test]
    fn the_hash_is_sha256_of_the_utf8_bytes() {
        // `printf '' | sha256sum`
        assert_eq!(
            content_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        // `printf 'hello\n' | sha256sum`
        assert_eq!(
            content_hash("hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );
        // Multibyte hashes its bytes, not its chars.
        assert_eq!(content_hash("é").len(), 64);
        assert_ne!(content_hash("é"), content_hash("e"));
    }

    /// Python's count, including the quirk: a trailing newline adds a
    /// line, because it counts separators and adds one.
    #[test]
    fn the_line_count_reproduces_pythons_arithmetic() {
        assert_eq!(line_count(""), 0);
        assert_eq!(line_count("a"), 1);
        assert_eq!(line_count("a\n"), 2);
        assert_eq!(line_count("a\nb"), 2);
        assert_eq!(line_count("a\nb\n"), 3);
        assert_eq!(line_count("\n"), 2);
    }

    /// The incident case, built the way the incident built it: take a
    /// real string, encode it UTF-8, then read those bytes back as
    /// Latin-1. That is exactly what `perl -0pi -e` did to
    /// `page.tsx`.
    fn double_encode(s: &str) -> String {
        s.as_bytes().iter().map(|&b| b as char).collect()
    }

    /// Python's own self-check, which it runs as an `assert` at import:
    /// an em dash put through the corruption must be detected. It
    /// exists because the character classes once degraded to a
    /// two-member set that matched nothing and reported every file
    /// clean — "the worst possible behaviour for a corruption guard".
    #[test]
    fn the_em_dash_from_the_incident_is_detected() {
        let corrupted = double_encode("—");
        let runs = mojibake_runs(&corrupted);
        assert_eq!(runs, vec!["—"], "{corrupted:?}");
    }

    /// Each hit decodes back to the character it should have been, so
    /// the error message can say what was meant.
    #[test]
    fn runs_decode_back_to_the_intended_characters() {
        assert_eq!(mojibake_runs(&double_encode("✓ done")), vec!["✓"]);
        assert_eq!(mojibake_runs(&double_encode("héllo")), vec!["é"]);
        assert_eq!(mojibake_runs(&double_encode("é—✓")), vec!["é", "—", "✓"]);
        // A 4-byte character exercises the third alternative.
        assert_eq!(mojibake_runs(&double_encode("🎉")), vec!["🎉"]);
    }

    /// Clean text has no runs — including text that is merely
    /// non-ASCII, which is the false positive that would make the
    /// guard unusable.
    #[test]
    fn ordinary_text_has_no_runs() {
        for s in [
            "",
            "plain ascii\n",
            // Properly encoded non-ASCII: the chars are above U+00FF,
            // so no lead byte class can match them.
            "héllo → 世界 ✓ 🎉",
            "fn main() { println!(\"{}\", 1); }\n",
        ] {
            assert!(mojibake_runs(s).is_empty(), "false positive on {s:?}");
        }
    }

    /// A lead character with too few continuations after it is not a
    /// run, including at the very end of the string — the bounds case
    /// that would panic on an off-by-one.
    #[test]
    fn a_truncated_run_is_not_a_run_and_does_not_panic() {
        // Lead needing one continuation, with nothing after it.
        assert!(mojibake_runs("\u{c3}").is_empty());
        // Lead needing two, with one.
        assert!(mojibake_runs("\u{e2}\u{80}").is_empty());
        // Lead needing three, with two.
        assert!(mojibake_runs("\u{f0}\u{9f}\u{8e}").is_empty());
        // And a continuation with no lead.
        assert!(mojibake_runs("\u{80}\u{80}").is_empty());
    }

    /// The guard fires only when a write *increases* the count. A file
    /// that is already corrupt stays editable, and a repair passes.
    #[test]
    fn only_an_increase_is_refused() {
        let one = double_encode("é");
        let two = double_encode("éé");

        // Introducing corruption into a clean file: refused.
        assert!(check_mojibake(&one, Some("clean")).is_err());
        // Into a new file (no baseline): also refused.
        assert!(check_mojibake(&one, None).is_err());
        // Adding more to an already-corrupt file: refused.
        assert!(check_mojibake(&two, Some(&one)).is_err());
        // The same amount: allowed — an edit elsewhere in a corrupt
        // file must not be blocked.
        assert!(check_mojibake(&one, Some(&one)).is_ok());
        // Fewer: allowed, which is the repair case.
        assert!(check_mojibake(&one, Some(&two)).is_ok());
        // A repair to fully clean: allowed, and takes the early return.
        assert!(check_mojibake("clean", Some(&two)).is_ok());
    }

    /// The refusal exits 3 and names what the sequences decode back
    /// to, because "it looks like it should be —" is the only form of
    /// this message anyone can act on.
    #[test]
    fn the_refusal_exits_three_and_shows_the_intended_text() {
        let err = check_mojibake(&double_encode("✓"), Some("clean")).unwrap_err();
        assert_eq!(crate::code_of(&err), crate::NO_PATH);
        let msg = format!("{err:#}");
        assert!(msg.contains("double-encoded"), "{msg}");
        assert!(msg.contains('✓'), "{msg}");
        assert!(msg.contains("1 new sequence;"), "{msg}");
        assert!(msg.contains("--allow-mojibake"), "{msg}");
    }

    /// Plural agreement, because the message is the deliverable.
    #[test]
    fn the_refusal_counts_in_the_plural_when_it_should() {
        let err = check_mojibake(&double_encode("éé"), Some("clean")).unwrap_err();
        assert!(format!("{err:#}").contains("2 new sequences;"));
    }

    /// `--if-match` refusals exit 2, as Python's do, and a *malformed*
    /// argument exits 1 — the same split Python makes between bad input
    /// and a conflict.
    #[test]
    fn if_match_refusals_exit_two_and_bad_input_exits_one() {
        let conflict = check_if_match("deadbeef", Some("ff4938cc03ca2b40")).unwrap_err();
        assert_eq!(crate::code_of(&conflict), crate::NOT_FOUND);

        let new_file = check_if_match("deadbeef", None).unwrap_err();
        assert_eq!(crate::code_of(&new_file), crate::NOT_FOUND);

        let malformed = check_if_match("xyz", Some("ff4938cc03ca2b40")).unwrap_err();
        assert_eq!(crate::code_of(&malformed), crate::USAGE);
    }

    #[test]
    fn if_match_needs_eight_hex_characters() {
        assert!(normalise_if_match("abcdef12").is_ok());
        // Uppercase is accepted and folded, since every display path
        // truncates and some of them upcase.
        assert_eq!(normalise_if_match("ABCDEF12").unwrap(), "abcdef12");
        assert_eq!(normalise_if_match("  abcdef12  ").unwrap(), "abcdef12");
        // Too short.
        assert!(normalise_if_match("abcdef1").is_err());
        // Not hex.
        assert!(normalise_if_match("abcdefgh").is_err());
        assert!(normalise_if_match("").is_err());
    }

    #[test]
    fn if_match_accepts_a_prefix_and_rejects_a_mismatch() {
        const HASH: &str = "ff4938cc03ca2b40be9d7c85e8b7001f";
        let current = Some(HASH);
        assert!(check_if_match("ff4938cc", current).is_ok());
        assert!(check_if_match("FF4938CC03CA2B40", current).is_ok());
        // The whole hash, not a prefix of it.
        assert!(check_if_match(HASH, current).is_ok());

        let err = check_if_match("deadbeef", current).unwrap_err();
        let msg = format!("{err:#}");
        // Both hashes in the message, truncated the way every other
        // display path truncates them.
        assert!(msg.contains("deadbeef"), "{msg}");
        assert!(msg.contains("ff4938cc03ca"), "{msg}");
        assert!(msg.contains("would revert it"), "{msg}");
    }

    /// `--if-match` against a file that does not exist is a refusal
    /// with its own message, not a silent create. Python separates the
    /// two because "it is a new file, so there is no base to match" is
    /// a different mistake from a stale base.
    #[test]
    fn if_match_on_a_new_file_refuses_and_says_why() {
        let err = check_if_match("abcdef12", None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no current content"), "{msg}");
        assert!(msg.contains("new file"), "{msg}");
    }

    /// A short prefix must not match a *different* hash that merely
    /// shares its first characters by accident — `starts_with` is the
    /// right test and the length minimum is what makes it safe.
    #[test]
    fn a_prefix_matches_only_a_real_prefix() {
        let current = Some("abcdef1234567890");
        assert!(check_if_match("abcdef12", current).is_ok());
        assert!(check_if_match("abcdef13", current).is_err());
        // Longer than the stored hash cannot match.
        assert!(check_if_match("abcdef1234567890ff", current).is_err());
    }
}
