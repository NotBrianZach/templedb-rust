//! `templedb entity ...` reads — the graph layer.

use crate::dialect::Dialect;
use crate::like::escape_like;
use crate::store::Store;
use crate::value::Value;
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};

/// Both halves of `entity stats`: counts by kind for `entities` and for
/// `relations`.
///
/// Python prints both sections — an earlier revision of this port read
/// only `entities` and silently dropped the second half, so the command
/// agreed on every number it printed and omitted nineteen of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphStats {
    pub entities: Vec<(String, i64)>,
    pub relations: Vec<(String, i64)>,
}

impl GraphStats {
    /// Python counts the table rather than summing the groups
    /// (`SELECT COUNT(*) FROM entities`). The two agree — `GROUP BY
    /// kind` partitions the table and `kind` is `NOT NULL` — so this
    /// sums instead of issuing two more queries.
    pub fn entity_total(&self) -> i64 {
        self.entities.iter().map(|(_, n)| n).sum()
    }
    pub fn relation_total(&self) -> i64 {
        self.relations.iter().map(|(_, n)| n).sum()
    }
}

/// Entity and relation counts by kind — the cheapest useful probe of the
/// graph, and a direct cross-check against `templedb entity stats`.
///
/// `ORDER BY 2 DESC` sorts by output ordinal, which both dialects
/// accept; `COUNT(*)` is an integer in SQLite and `bigint` in Postgres,
/// and both narrow to `Value::Int`.
///
/// The `, kind` tie-break is this port's and Python has none. It is free
/// here rather than a divergence: no two kinds currently share a count
/// in either table, so the orders agree today, and without it a future
/// tie would be resolved by the planner. That is the same reasoning
/// `vcs log`'s tie-break was chosen under — pick the total order that
/// also matches the implementation being replaced.
pub fn entity_stats(store: &dyn Store) -> Result<GraphStats> {
    let count_by_kind = |table: &str| -> Result<Vec<(String, i64)>> {
        let rows = store
            .query(
                &format!("SELECT kind, COUNT(*) FROM {table} GROUP BY kind ORDER BY 2 DESC, kind"),
                &[],
            )
            .with_context(|| format!("entity stats: {table}"))?;
        rows.iter()
            .map(|r| Ok((r.get_string(0)?, r.get_i64(1)?)))
            .collect()
    };
    Ok(GraphStats {
        entities: count_by_kind("entities")?,
        relations: count_by_kind("relations")?,
    })
}

/// Which way to walk a relation. `relations` rows are directed
/// `from_entity_id -> to_entity_id`; "what did this cause" and "what
/// caused this" are different reads of the same edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Out,
    In,
    Both,
}

impl Direction {
    fn includes_out(self) -> bool {
        matches!(self, Direction::Out | Direction::Both)
    }
    fn includes_in(self) -> bool {
        matches!(self, Direction::In | Direction::Both)
    }
}

/// One edge, already resolved to the entity on the far end.
#[derive(Debug, Clone)]
pub struct Edge {
    pub relkind: String,
    pub peer_kind: String,
    pub peer_ref: String,
    pub peer_label: Option<String>,
    /// Which end of the relation the peer sat on.
    pub direction: Direction,
}

impl Edge {
    /// `Kind/external_ref`, the address every entity command accepts.
    pub fn peer_address(&self) -> String {
        format!("{}/{}", self.peer_kind, self.peer_ref)
    }
}

/// An entity plus its immediate neighbourhood.
#[derive(Debug, Clone)]
pub struct Explored {
    pub kind: String,
    pub external_ref: String,
    pub label: Option<String>,
    pub source_authority: String,
    pub observed_at: String,
    pub outbound: Vec<Edge>,
    pub inbound: Vec<Edge>,
}

/// One hop out of and into an entity, addressed as `<kind>/<external_ref>`
/// the way `templedb entity explore` addresses it.
///
/// Returns the entity's own header fields alongside the edges. Earlier
/// revisions of this port returned edges only, which the README listed
/// as a known gap: without `label`, `source_authority` and
/// `observed_at`, the output cannot be diffed against the Python
/// command it replaces.
pub fn explore(store: &dyn Store, kind: &str, external_ref: &str) -> Result<Explored> {
    let (id, label, source_authority, observed_at) = lookup_entity(store, kind, external_ref)?;
    Ok(Explored {
        kind: kind.to_string(),
        external_ref: external_ref.to_string(),
        label,
        source_authority,
        observed_at,
        outbound: fetch_edges(store, id, Direction::Out, &[], None)?,
        inbound: fetch_edges(store, id, Direction::In, &[], None)?,
    })
}

/// One row of a `trace` walk: an edge plus how many hops from the root
/// it was found at. Flat rather than a tree because the Python command
/// prints it flat, indenting by depth, and a tree would have to be
/// re-flattened in the same order to compare the two.
#[derive(Debug, Clone)]
pub struct TraceHit {
    /// 1 for the root's immediate neighbours.
    pub depth: usize,
    pub edge: Edge,
}

/// A whole `entity trace` walk: the edges it emitted, and whether the
/// fan-out cap dropped any.
///
/// `truncated` is here rather than inferred by the caller because it
/// cannot be inferred: a node with exactly `--limit` edges and a node
/// with a thousand produce the same number of hits, and the walk is the
/// only place that knows the difference. Python prints an advisory line
/// when it is set — without it a truncated list looks complete, which is
/// how `entity paths` came to report "no path" across an edge that
/// existed.
#[derive(Debug, Clone)]
pub struct Traced {
    pub hits: Vec<TraceHit>,
    pub truncated: bool,
}

/// Breadth-first walk out from one entity, in emission order.
///
/// `via` filters on relation kind (empty = all). `fanout` caps edges
/// *per node per direction*, which is what keeps a Machine with 156
/// `ran` edges from drowning the output.
///
/// Divergence from Python, deliberate: the per-node cap is applied after
/// `ORDER BY e.kind, e.external_ref`. The Python query has a bare
/// `LIMIT ?` with no `ORDER BY`, so which ten of the 156 edges you see
/// is whatever the query planner felt like — the same command can print
/// different neighbours on two runs, and nothing in the output says it
/// truncated. Ordering first makes the result reproducible and makes
/// comparing the two implementations possible at all.
pub fn trace(
    store: &dyn Store,
    kind: &str,
    external_ref: &str,
    depth: usize,
    direction: Direction,
    via: &[String],
    fanout: i64,
) -> Result<Traced> {
    let (start, _, _, _) = lookup_entity(store, kind, external_ref)?;

    let mut visited: HashSet<i64> = HashSet::from([start]);
    let mut frontier: Vec<i64> = vec![start];
    let mut hits: Vec<TraceHit> = Vec::new();
    let mut truncated = false;

    for hop in 1..=depth {
        if frontier.is_empty() {
            break;
        }
        // One query per direction for the whole level, not one per
        // node. The grouping below walks `frontier` in its own order
        // and each node's edges in theirs, so the emission order is
        // the same as the per-node version produced.
        let (batch, cut) = fetch_frontier(store, &frontier, direction, via, Some(fanout))?;
        // Sticky across levels, as Python's `any_truncated` is: the
        // advisory is about the walk, not about one node.
        truncated |= cut;
        let mut next = Vec::new();
        for eid in &frontier {
            for (peer_id, edge) in batch.get(eid).into_iter().flatten() {
                // A global visited set, matching Python: an entity is
                // reported at the shallowest depth it is reachable at
                // and not expanded twice. Without it, a cycle walks
                // forever at depth > 2.
                if !visited.insert(*peer_id) {
                    continue;
                }
                hits.push(TraceHit {
                    depth: hop,
                    edge: edge.clone(),
                });
                next.push(*peer_id);
            }
        }
        frontier = next;
    }
    Ok(Traced { hits, truncated })
}

/// One `entity search` row: the header fields, no edges.
///
/// `external_ref` is nullable in the schema even though nothing in the
/// live database has a NULL one today, so it is carried as an `Option`
/// rather than unwrapped at the query. The Python command formats it with
/// `{:<50}`, which raises `TypeError` on `None`; that path is currently
/// unreachable, and keeping it unreachable here costs one `Option`.
#[derive(Debug, Clone)]
pub struct EntityMatch {
    pub kind: String,
    pub external_ref: Option<String>,
    pub label: Option<String>,
    pub source_authority: String,
    pub observed_at: String,
}

/// Case-insensitive substring search over `label` and `external_ref`.
///
/// Two deliberate divergences from Python, both of which make the output
/// mean what it says:
///
/// 1. **The query is LIKE-escaped.** Python interpolates it raw, so
///    `entity search deploy_` is the pattern `%deploy_%` and `_` matches
///    any character: 858 hits against a ground truth of 256, including
///    `class SafeDeploymentQueries` (`deploym` fits `deploy_`) for a
///    caller who typed an underscore. See [`escape_like`].
///
/// 2. **`ORDER BY` has a tie-break.** Python orders by `observed_at DESC`
///    alone and then takes `LIMIT 30`. The live `entities` table has
///    55,196 rows across 47 distinct `observed_at` values — the largest
///    single timestamp covers 11,093 entities — so for any query whose
///    matches outnumber the limit, *which* rows you are shown is the
///    query planner's choice and can change between runs. Ordering by
///    `(observed_at DESC, kind, external_ref)` makes the first page
///    stable and reviewable.
///
/// That tie-break is also where the two backends can disagree without
/// saying so: it sorts on text, SQLite compares text with BINARY
/// collation, and Postgres uses the database's. The Postgres schema
/// declares `kind` and `external_ref` as `COLLATE "C"` so the two agree
/// — see `schema/postgres/001_read_subset.sql`.
pub fn search_entities(
    store: &dyn Store,
    query: &str,
    kind: Option<&str>,
    limit: i64,
) -> Result<Vec<EntityMatch>> {
    // `LOWER(col) LIKE <lowercased pattern>` is Python's shape, kept
    // verbatim. For ASCII it is redundant — SQLite's LIKE already folds
    // case — and for non-ASCII it is half-working in both
    // implementations, because SQL `LOWER()` is ASCII-only while the
    // pattern is folded by the host language's full Unicode rules.
    // Reproducing that rather than fixing it keeps the two outputs
    // comparable; fixing it belongs with a decision about which
    // collation the column should have.
    //
    // Postgres's `lower()` is *not* ASCII-only — it is locale-aware —
    // so this is one of the places where a non-ASCII query could
    // disagree across backends. It is recorded in the README's known
    // gaps rather than papered over here, because the fix is a
    // collation decision and not a query change.
    let pattern = format!("%{}%", escape_like(&query.to_lowercase()));
    let (kind_clause, limit_idx) = match kind {
        Some(_) => (" AND kind = ?2", 3),
        None => ("", 2),
    };
    let sql = format!(
        "SELECT kind, external_ref, label, source_authority, observed_at
           FROM entities
          WHERE (LOWER(label) LIKE ?1 ESCAPE '\\'
                 OR LOWER(external_ref) LIKE ?1 ESCAPE '\\'){kind_clause}
          ORDER BY observed_at DESC, kind, external_ref NULLS FIRST
          LIMIT ?{limit_idx}"
    );

    let mut params: Vec<Value> = vec![Value::from(pattern)];
    if let Some(k) = kind {
        params.push(Value::from(k));
    }
    params.push(Value::from(limit));

    let rows = store.query(&sql, &params).context("searching entities")?;
    rows.iter()
        .map(|r| {
            Ok(EntityMatch {
                kind: r.get_string(0)?,
                external_ref: r.get_opt_string(1)?,
                label: r.get_opt_string(2)?,
                source_authority: r.get_string(3)?,
                observed_at: r.get_string(4)?,
            })
        })
        .collect()
}

/// The outcome of a [`shortest_path`] search.
///
/// `SameEntity` is its own variant rather than an empty `Found` because
/// the Python command prints it differently — a zero-hop path and
/// "source == target" are not the same message.
#[derive(Debug, Clone)]
pub enum PathResult {
    SameEntity,
    /// The edges from source to target, in order. `source_label` is the
    /// starting entity's own label, which the caller needs for the
    /// header line and cannot get from an [`Edge`].
    Found {
        source_label: Option<String>,
        hops: Vec<Edge>,
    },
    /// The frontier was exhausted, or `max_depth` reached, without
    /// arriving at the target.
    NoPath,
}

/// Shortest path between two entities, breadth-first with parent
/// pointers.
///
/// Undirected by default (`Direction::Both`): "how is X related to Y"
/// rarely cares which way the edge was recorded.
///
/// Divergence from Python, and the reason this function exists in this
/// shape: **there is no fan-out cap.** `graph_paths` calls its edge
/// fetcher with a hardcoded `limit=100` and no `ORDER BY`, so a node with
/// more than 100 edges contributes an arbitrary 100 of them to the
/// search. 122 entities in the live database exceed that, one of them
/// with 1,301 edges, and the failure is silent and wrong in the worst
/// direction — it reports *no path* where one exists:
///
/// ```text
/// $ templedb entity paths AgentSession/ba3f0999-… ToolCall/1113
/// (no path within depth 6 between AgentSession/ba3f0999-… and ToolCall/1113)
/// ```
///
/// There is a direct `invoked` edge between those two. Capping is
/// defensible for `trace`, which prints its fan-out and where truncation
/// is visible as a short list; it is not defensible here, where the
/// answer is a single yes/no and truncation turns a yes into a no. BFS
/// visits each entity at most once, so the uncapped cost is bounded by
/// the 64,562 rows of `relations` rather than by anything exponential.
pub fn shortest_path(
    store: &dyn Store,
    from: (&str, &str),
    to: (&str, &str),
    max_depth: usize,
    direction: Direction,
    via: &[String],
) -> Result<PathResult> {
    let (src, source_label, _, _) = lookup_entity(store, from.0, from.1)?;
    let (tgt, _, _, _) = lookup_entity(store, to.0, to.1)?;
    if src == tgt {
        return Ok(PathResult::SameEntity);
    }

    // `parent[node]` is the edge that first reached `node`, plus where it
    // came from; the source maps to `None` and terminates reconstruction.
    // Presence in the map doubles as the visited set, which is what keeps
    // a cycle from re-enqueueing.
    let mut parent: HashMap<i64, Option<(i64, Edge)>> = HashMap::from([(src, None)]);
    // Level-synchronous rather than one-node-at-a-time, so each level
    // costs one query per direction instead of one per node. The node
    // order within a level is the order they were discovered in, which
    // is exactly the order a FIFO queue would have popped them — so
    // `parent` is populated with the same first-come-wins assignments
    // and the reconstructed path is identical. See `fetch_frontier`.
    let mut frontier: Vec<i64> = vec![src];
    let mut found = false;

    'bfs: for _ in 0..max_depth {
        if frontier.is_empty() {
            break;
        }
        // `None` for the cap, so the truncation flag is always false
        // here — `entity paths` does not cap, which is the whole point
        // of the divergence recorded above.
        let (batch, _) = fetch_frontier(store, &frontier, direction, via, None)?;
        let mut next = Vec::new();
        for cur in &frontier {
            for (peer, edge) in batch.get(cur).into_iter().flatten() {
                if parent.contains_key(peer) {
                    continue;
                }
                parent.insert(*peer, Some((*cur, edge.clone())));
                if *peer == tgt {
                    // Stop on discovery rather than on dequeue. Python
                    // waits until the target is popped, which first
                    // expands every node already queued ahead of it —
                    // thousands of edge queries, for a path that cannot
                    // change: `parent[tgt]` is written once and the
                    // `contains_key` guard above never lets it be
                    // overwritten.
                    found = true;
                    break 'bfs;
                }
                next.push(*peer);
            }
        }
        frontier = next;
    }

    if !found {
        return Ok(PathResult::NoPath);
    }

    // Walk the parent pointers back from the target and reverse.
    let mut hops = Vec::new();
    let mut node = tgt;
    while let Some(Some((prev, edge))) = parent.get(&node) {
        hops.push(edge.clone());
        node = *prev;
    }
    hops.reverse();
    Ok(PathResult::Found { source_label, hops })
}

/// One `File -imports-> File` edge with no call bridging it.
#[derive(Debug, Clone, PartialEq)]
pub struct DeadImport {
    pub from_ref: String,
    pub to_ref: String,
}

/// Files that import another file without any Symbol call landing in it.
///
/// An investigation tool rather than an invariant, and the Python
/// docstring is right about why: a side-effect import — plugin
/// registration, monkeypatching — legitimately has no call edge, so
/// this is a list to eyeball and not a list of defects.
///
/// The shape is three joins deep and worth reading once. For each
/// `File -imports-> File` pair it asks whether any `Symbol` the source
/// file `defines` has a `calls`/`inherits`/`uses` edge to a `Symbol`
/// the target file `defines`. Every join is a `LEFT JOIN` so a pair
/// with no symbols at all still produces a row, and the `HAVING
/// SUM(...) = 0` is what selects the pairs where nothing bridged.
///
/// One divergence from Python, and it is latent rather than observed:
/// the `--slug` filter is LIKE-escaped here. Python interpolates it
/// raw into `fe.external_ref LIKE ? || '/%'`, so a slug containing `_`
/// would match any character in that position. Eleven of the thirty
/// slugs contain an underscore, but no pair of them currently collides
/// — `templedb_backup_service` does not have a `templedbxbackup...`
/// sibling — so the bug is reachable only by adding a project, which
/// is exactly when nobody would connect the two. This is the fourth
/// instance of the same mistake in this codebase; see `like.rs`.
pub fn dead_imports(store: &dyn Store, slug: Option<&str>, limit: i64) -> Result<Vec<DeadImport>> {
    // `CAST(?1 AS TEXT) IS NULL` rather than `?1 IS NULL`: Postgres
    // infers a parameter's type from its use and `IS NULL` tells it
    // nothing, so the uncast form is rejected outright. Same fix as
    // `search.rs`.
    let rows = store
        .query(
            "WITH imports AS (
               SELECT fe.external_ref AS from_ref, fe.id AS from_id,
                      te.external_ref AS to_ref,   te.id AS to_id
                 FROM relations r
                 JOIN entities fe ON fe.id = r.from_entity_id
                 JOIN entities te ON te.id = r.to_entity_id
                WHERE r.kind = 'imports'
                  AND fe.kind = 'File'
                  AND te.kind = 'File'
                  AND (CAST(?1 AS TEXT) IS NULL
                       OR fe.external_ref LIKE ?1 || '/%' ESCAPE '\\')
             ),
             calls AS (
               SELECT imp.from_ref, imp.to_ref
                 FROM imports imp
            LEFT JOIN relations dr_from
                   ON dr_from.from_entity_id = imp.from_id
                  AND dr_from.kind = 'defines'
            LEFT JOIN entities fsym
                   ON fsym.id = dr_from.to_entity_id
                  AND fsym.kind = 'Symbol'
            LEFT JOIN relations cr
                   ON cr.from_entity_id = fsym.id
                  AND cr.kind IN ('calls', 'inherits', 'uses')
            LEFT JOIN entities tsym
                   ON tsym.id = cr.to_entity_id
                  AND tsym.kind = 'Symbol'
            LEFT JOIN relations dr_to
                   ON dr_to.from_entity_id = imp.to_id
                  AND dr_to.kind = 'defines'
                  AND dr_to.to_entity_id = tsym.id
                GROUP BY imp.from_ref, imp.to_ref
               HAVING SUM(CASE WHEN dr_to.id IS NOT NULL THEN 1 ELSE 0 END) = 0
             )
             SELECT from_ref, to_ref FROM calls
              ORDER BY from_ref, to_ref
              LIMIT ?2",
            &[Value::from(slug.map(escape_like)), Value::from(limit)],
        )
        .context("finding dead imports")?;

    rows.iter()
        .map(|r| {
            Ok(DeadImport {
                from_ref: r.get_string(0)?,
                to_ref: r.get_string(1)?,
            })
        })
        .collect()
}

/// One archived observation: what an entity's label and authority were
/// before an update overwrote them.
#[derive(Debug, Clone)]
pub struct Observation {
    pub id: i64,
    pub observed_at: String,
    pub label: Option<String>,
    pub prior_label: Option<String>,
    pub source_authority: Option<String>,
    pub prior_source_authority: Option<String>,
}

impl Observation {
    /// Whether this row recorded a label change. The caller prints the
    /// before/after only when something moved, so a row that captured
    /// an unrelated update stays quiet.
    pub fn label_changed(&self) -> bool {
        self.prior_label != self.label
    }

    pub fn authority_changed(&self) -> bool {
        self.prior_source_authority != self.source_authority
    }
}

/// Archived label and authority history for one entity.
///
/// `observations_archive` is written by a trigger on every update to
/// `entities`, capturing the pre-update state. Nothing in this crate
/// writes it; this reads it.
///
/// Addressed by `(entity_kind, entity_ref)` rather than by id, because
/// the archive outlives the entity — the column is documented as
/// "FK-ish (entity may have been deleted)" and is not joined here for
/// that reason. An entity that was deleted and recreated shares the
/// archive of its predecessor, which is the intended behaviour: the
/// address is what the history is about.
///
/// The `id` tie-break is not decoration. Python orders on
/// `observed_at DESC` alone and the timestamps tie — the trigger fires
/// several times within one second, so
/// `Symbol/templedb:src/gui_pages/projects.py:_file_rows` has 645
/// archive rows across only 217 distinct timestamps. With a `--limit`,
/// a tie straddling the cutoff decides *which rows you see at all*, so
/// leaving it to the planner is the same defect the README records for
/// `entity search`.
///
/// It is `ASC`, against the `DESC` of the column above it, and the data
/// is why. Rows sharing a second are a causal chain rather than an
/// unordered set: within `2026-10-02 04:03:11` one row records
/// `line 1040 → line 503` and the next `line 503 → line 656`. Ascending
/// reads as that sequence; descending reads as its reverse, where each
/// line's "before" is the previous line's "after" and the history
/// appears to run backwards. Symmetry with `observed_at DESC` was the
/// first instinct here and it was wrong.
///
/// It also happens to match Python byte for byte, because a scan of
/// this table arrives in rowid order. That is a consequence rather
/// than the reason — the point is that the chain is readable.
pub fn observations(
    store: &dyn Store,
    kind: &str,
    external_ref: &str,
    limit: i64,
) -> Result<Vec<Observation>> {
    let rows = store
        .query(
            "SELECT id, observed_at, label, prior_label,
                    source_authority, prior_source_authority
               FROM observations_archive
              WHERE entity_kind = ?1 AND entity_ref = ?2
              ORDER BY observed_at DESC, id ASC
              LIMIT ?3",
            &[
                Value::from(kind),
                Value::from(external_ref),
                Value::from(limit),
            ],
        )
        .context("reading observation history")?;

    rows.iter()
        .map(|r| {
            Ok(Observation {
                id: r.get_i64(0)?,
                observed_at: r.get_string(1)?,
                label: r.get_opt_string(2)?,
                prior_label: r.get_opt_string(3)?,
                source_authority: r.get_opt_string(4)?,
                prior_source_authority: r.get_opt_string(5)?,
            })
        })
        .collect()
}

/// `(id, label, source_authority, observed_at)` for one entity address.
fn lookup_entity(
    store: &dyn Store,
    kind: &str,
    external_ref: &str,
) -> Result<(i64, Option<String>, String, String)> {
    let row = store
        .query_opt(
            "SELECT id, label, source_authority, observed_at
               FROM entities WHERE kind = ?1 AND external_ref = ?2",
            &[Value::from(kind), Value::from(external_ref)],
        )
        .context("looking up entity")?;
    let Some(row) = row else {
        anyhow::bail!("no entity {kind}/{external_ref}");
    };
    Ok((
        row.get_i64(0)?,
        row.get_opt_string(1)?,
        row.get_string(2)?,
        row.get_string(3)?,
    ))
}

fn fetch_edges(
    store: &dyn Store,
    id: i64,
    direction: Direction,
    via: &[String],
    limit: Option<i64>,
) -> Result<Vec<Edge>> {
    Ok(fetch_frontier(store, &[id], direction, via, limit)?
        .0
        .remove(&id)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// How many anchor ids one query covers.
///
/// Bounded because the ids become bind parameters and Postgres caps a
/// statement at 65,535 of them; a frontier larger than this is split
/// across several queries. 4,096 keeps the whole walk inside a handful
/// of round-trips without going near that ceiling.
const FRONTIER_CHUNK: usize = 4096;

/// The placeholder-count buckets a chunk is padded up to.
///
/// Without bucketing, a frontier of 37 nodes and one of 38 would
/// produce different SQL text, and both backends key their prepared
/// statement cache on that text — so a BFS whose level sizes all differ
/// would miss the cache on every level and pay to plan the query again.
/// Padding to the next bucket gives at most seven distinct statements
/// per shape instead of one per level size.
///
/// Padding is a semantic no-op: the extra slots repeat an id already in
/// the chunk, and `IN` tests set membership.
const BUCKETS: [usize; 7] = [1, 4, 16, 64, 256, 1024, FRONTIER_CHUNK];

fn bucket_for(n: usize) -> usize {
    BUCKETS
        .into_iter()
        .find(|&b| b >= n)
        .unwrap_or(FRONTIER_CHUNK)
}

/// Edges for every node in `frontier`, grouped by the node they hang
/// off.
///
/// This is the one place relation SQL is written. Both directions use
/// the same projection so `Edge` is populated identically whichever end
/// the peer sat on; only the join column and the `WHERE` differ.
///
/// # Why a frontier and not a node
///
/// The previous version took one node and was called once per visited
/// node per direction — for `entity paths` exhausting the reachable
/// component, upwards of a hundred thousand queries. On SQLite that is
/// a function call into an embedded library and the cost hid inside the
/// 19 ms the whole search took. On Postgres each one is a socket
/// round-trip, and the same search cost 59 ms.
///
/// Fetching a whole BFS level at a time turns that into one query per
/// level per direction — around a dozen for a depth-6 walk, plus one
/// more per [`FRONTIER_CHUNK`] of a wide level. Nothing about the walk
/// moved into SQL: the visited set, the ordering and the per-node cap
/// are all still decided here, which is what keeps the output
/// identical. A recursive CTE would have pushed the traversal into the
/// database and with it the visited-set and emission-order semantics
/// that `trace` matches Python on, which is a much larger behavioural
/// risk for the same latency win.
///
/// # Ordering
///
/// Per anchor, outbound edges are yielded before inbound ones, because
/// `Direction::Both` collects the two passes in that order. Within a
/// pass they are ordered `(r.kind, e.kind, e.external_ref)`. Both
/// matter: the first decides whether a hop prints as `→` or `←`, and
/// the second is what makes the fan-out cap reproducible instead of
/// being whatever the query planner felt like.
/// One level of the batched walk: each anchor's edges keyed by anchor
/// id, with the peer id alongside each [`Edge`] so the caller can mark
/// it visited without a second lookup.
type FrontierEdges = HashMap<i64, Vec<(i64, Edge)>>;

fn fetch_frontier(
    store: &dyn Store,
    frontier: &[i64],
    direction: Direction,
    via: &[String],
    limit: Option<i64>,
) -> Result<(FrontierEdges, bool)> {
    let mut out: FrontierEdges = HashMap::new();
    let mut truncated = false;
    if direction.includes_out() {
        truncated |= collect_direction(
            store,
            frontier,
            Direction::Out,
            "r.to_entity_id",
            "r.from_entity_id",
            via,
            limit,
            &mut out,
        )?;
    }
    if direction.includes_in() {
        truncated |= collect_direction(
            store,
            frontier,
            Direction::In,
            "r.from_entity_id",
            "r.to_entity_id",
            via,
            limit,
            &mut out,
        )?;
    }
    Ok((out, truncated))
}

/// A `col IN <set>` predicate, plus the parameters that fill it and the
/// next free placeholder index.
///
/// How a set of ids is handed to the server is the one place the two
/// dialects genuinely differ here, and the fork is worth it because the
/// alternative was measured and was worse. The first version used the
/// `IN`-list form for both backends, on the theory that one SQL string
/// is safer than two — and it made Postgres *slower* than the per-node
/// version it replaced, 74 ms against 61 ms, because padding a 300-id
/// level up to a 1,024-placeholder bucket makes the server probe the
/// index 724 times for ids that are not in the set. (That was not the
/// whole story: the join was costing more than either form. See
/// [`collect_direction`].)
///
/// Both forms are set membership over the same ids and neither decides
/// anything about which rows come back or in what order — the ordering
/// moved to Rust (see [`collect_direction`]) — so this is the kind of
/// dialect difference that is safe to fork. The cross-backend diff is
/// what checks the claim.
fn membership(dialect: Dialect, col: &str, ids: &[i64]) -> (String, usize, Vec<Value>) {
    match dialect {
        // One parameter whatever the width, so the SQL text never
        // changes and the prepared statement is reused on every level.
        Dialect::Postgres => (
            format!("{col} = ANY(?1::bigint[])"),
            2,
            vec![Value::IntSet(ids.to_vec())],
        ),
        // SQLite has no array type, so one placeholder per id, padded
        // up to a bucket so the statement cache has a handful of
        // entries rather than one per distinct width. Padding repeats
        // an id already present and `IN` is set membership, so it adds
        // no rows.
        Dialect::Sqlite => {
            let width = bucket_for(ids.len());
            let marks: Vec<String> = (1..=width).map(|i| format!("?{i}")).collect();
            let padded = (0..width)
                .map(|i| Value::from(ids[i.min(ids.len() - 1)]))
                .collect();
            (format!("{col} IN ({})", marks.join(",")), width + 1, padded)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_direction(
    store: &dyn Store,
    frontier: &[i64],
    direction: Direction,
    peer_col: &str,
    anchor_col: &str,
    via: &[String],
    limit: Option<i64>,
    out: &mut FrontierEdges,
) -> Result<bool> {
    // Edges and peer details are two queries, not one join, and the
    // ordering happens here rather than in SQL. Both choices come from
    // the same measurement.
    //
    // The joined form asked Postgres for
    // `relations JOIN entities ... ORDER BY r.kind, e.kind,
    // e.external_ref`, and Postgres answered it by sequentially
    // scanning all 60,719 entities to build a hash table: 22.7 ms of a
    // 25.4 ms query, against 0.26 ms for the index-only scan that found
    // the 1,625 edges. The planner is not wrong by its own cost model —
    // 4,410 estimated index probes do look dearer than one scan of a
    // small table — it is just that the hash build is the whole cost
    // and the probes would all have been cache hits. `ANALYZE` and
    // `random_page_cost = 1.1` were both tried and neither changed the
    // plan.
    //
    // Asking for the edges alone keeps that index-only scan, and
    // fetching the peers by primary key is the plan the join was trying
    // to avoid, made explicit. It also fetches each distinct peer once
    // rather than once per edge pointing at it.
    //
    // Sorting in Rust is the part that is better rather than merely
    // faster. `str`'s ordering is byte ordering, which is exactly what
    // SQLite's BINARY collation and Postgres's `COLLATE "C"` give, so
    // the neighbour order is now identical across backends by
    // construction instead of by two `ORDER BY` clauses agreeing. It
    // also retires a latent divergence: the engines disagree on where
    // NULLs sort (SQLite first on ASC, Postgres last), which no amount
    // of live data would have revealed because no row has a NULL
    // `external_ref` today.
    // Accumulated locally, not straight into `out`, because the sort
    // below must only see this direction's edges. `Direction::Both`
    // calls this twice and the outbound pass has to stay entirely ahead
    // of the inbound one: that grouping is what decides whether a hop
    // prints as `→` or `←`, and sorting the merged list would reorder
    // across it. Sorting the whole of `out` is exactly the bug the
    // `paths_direction_and_via_restrict_the_walk` test caught — with
    // both passes merged first, `installs` sorts ahead of `used-by` and
    // the reported path changes.
    let mut mine: HashMap<i64, Vec<(i64, Edge)>> = HashMap::new();

    for chunk in frontier.chunks(FRONTIER_CHUNK) {
        let (pred, next_param, mut params) = membership(store.dialect(), anchor_col, chunk);

        // Every placeholder is numbered. rusqlite accepts a bare `?`
        // and would let this file mix the two forms; Postgres has no
        // anonymous placeholder at all, so `Dialect::bind` rejects one
        // rather than guessing an index. `via` follows the anchors.
        let via_clause = if via.is_empty() {
            String::new()
        } else {
            let marks: Vec<String> = (0..via.len())
                .map(|i| format!("?{}", next_param + i))
                .collect();
            format!(" AND r.kind IN ({})", marks.join(","))
        };
        for v in via {
            params.push(Value::from(v.as_str()));
        }

        let edges = store
            .query(
                &format!(
                    "SELECT {anchor_col}, {peer_col}, r.kind
                       FROM relations r
                      WHERE {pred}{via_clause}"
                ),
                &params,
            )
            .context("fetching relations")?;
        if edges.is_empty() {
            continue;
        }

        // `(anchor, peer, relkind)` with the peer's own columns still
        // to come.
        let mut pending: Vec<(i64, i64, String)> = Vec::with_capacity(edges.len());
        let mut peers: Vec<i64> = Vec::with_capacity(edges.len());
        for r in &edges {
            let (anchor, peer) = (r.get_i64(0)?, r.get_i64(1)?);
            pending.push((anchor, peer, r.get_string(2)?));
            peers.push(peer);
        }
        peers.sort_unstable();
        peers.dedup();

        let details = fetch_entity_details(store, &peers)?;

        for (anchor, peer, relkind) in pending {
            // A relation naming an entity that is not there is dropped,
            // which is what the `JOIN` this replaced did. SQLite does
            // not enforce foreign keys unless `PRAGMA foreign_keys` is
            // on, and it is not, so a dangling edge is possible in the
            // live data rather than merely in theory.
            let Some((peer_kind, peer_ref, peer_label)) = details.get(&peer) else {
                continue;
            };
            mine.entry(anchor).or_default().push((
                peer,
                Edge {
                    relkind: relkind.clone(),
                    peer_kind: peer_kind.clone(),
                    peer_ref: peer_ref.clone(),
                    peer_label: peer_label.clone(),
                    direction,
                },
            ));
        }
    }

    // Order and cap per anchor, once every chunk of this direction has
    // contributed. The cap is per node *per direction*, which is what
    // the `LIMIT` on the old per-node-per-direction query did.
    //
    // `sort_by` is stable, but the key is unique anyway:
    // `UNIQUE(from_entity_id, kind, to_entity_id)` fixes the peer for a
    // given anchor and relation kind, and `UNIQUE(kind, external_ref)`
    // fixes the entity, so no two edges off one anchor share
    // `(relkind, peer_kind, peer_ref)`.
    let mut truncated = false;
    for (anchor, mut edges) in mine {
        edges.sort_by(|(_, a), (_, b)| {
            (&a.relkind, &a.peer_kind, &a.peer_ref).cmp(&(&b.relkind, &b.peer_kind, &b.peer_ref))
        });
        if let Some(n) = limit {
            let n = n.max(0) as usize;
            // Python asks the server for `limit + 1` rows and discards
            // the extra, because a bare `LIMIT` renders "exactly limit
            // edges exist" and "more than limit exist" identically. This
            // port already has every edge in memory — the cap is applied
            // here, not in SQL — so the same distinction is a length
            // comparison and costs nothing.
            truncated |= edges.len() > n;
            edges.truncate(n);
        }
        out.entry(anchor).or_default().extend(edges);
    }
    Ok(truncated)
}

/// The peer columns an [`Edge`] needs beyond the relation itself:
/// `(kind, external_ref, label)`.
type PeerDetail = (String, String, Option<String>);

/// [`PeerDetail`] for a set of entity ids, by primary key.
///
/// Separate from the relation query on purpose — see the long note in
/// [`collect_direction`] — and deduplicated by the caller, so an entity
/// that twenty edges point at is fetched once.
fn fetch_entity_details(store: &dyn Store, ids: &[i64]) -> Result<HashMap<i64, PeerDetail>> {
    let mut out = HashMap::with_capacity(ids.len());
    for chunk in ids.chunks(FRONTIER_CHUNK) {
        let (pred, _, params) = membership(store.dialect(), "id", chunk);
        let rows = store
            .query(
                &format!("SELECT id, kind, external_ref, label FROM entities WHERE {pred}"),
                &params,
            )
            .context("fetching entity details")?;
        for r in &rows {
            out.insert(
                r.get_i64(0)?,
                (r.get_string(1)?, r.get_string(2)?, r.get_opt_string(3)?),
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    #[test]
    fn stats_count_every_kind() {
        let c = testdb::fixture();
        let stats = entity_stats(&c).unwrap();
        assert_eq!(stats.entity_total(), 7);
        // Ordered by count descending, then kind for a stable tie-break.
        assert_eq!(stats.entities[0], ("Generation".to_string(), 3));
    }

    /// Both halves, because an earlier revision read only `entities` and
    /// printed nothing at all for `relations` — every number it did
    /// print agreed with Python, which is why the gap survived a diff
    /// that only compared counts.
    #[test]
    fn stats_count_relations_as_well_as_entities() {
        let c = testdb::fixture();
        let stats = entity_stats(&c).unwrap();
        assert_eq!(stats.relation_total(), 5);
        assert!(
            !stats.relations.is_empty(),
            "the relations section must not be empty"
        );
        // Descending by count, and the fixture's `ran` edges outnumber
        // the rest.
        let counts: Vec<i64> = stats.relations.iter().map(|(_, n)| *n).collect();
        assert!(
            counts.windows(2).all(|w| w[0] >= w[1]),
            "not descending: {counts:?}"
        );
        // Summing the groups equals counting the table, which is what
        // lets this avoid Python's two extra COUNT(*) queries.
        let direct = c
            .query("SELECT COUNT(*) FROM relations", &[])
            .unwrap()
            .first()
            .unwrap()
            .get_i64(0)
            .unwrap();
        assert_eq!(stats.relation_total(), direct);
    }

    #[test]
    fn explore_returns_header_and_both_directions() {
        let c = testdb::fixture();
        let e = explore(&c, "Machine", "box1").unwrap();
        assert_eq!(e.label.as_deref(), Some("box one"));
        assert_eq!(e.source_authority, "nix");
        assert_eq!(e.observed_at, "2026-10-01 00:00:00");
        assert_eq!(e.outbound.len(), 3);
        assert!(e.inbound.is_empty());

        let g2 = explore(&c, "Generation", "gen-2").unwrap();
        assert_eq!(g2.inbound.len(), 1);
        assert_eq!(g2.inbound[0].peer_address(), "Machine/box1");
        // A NULL label stays None rather than becoming "None".
        assert_eq!(g2.label, None);
    }

    #[test]
    fn explore_rejects_unknown_entities() {
        let c = testdb::fixture();
        let err = explore(&c, "Machine", "nope").unwrap_err().to_string();
        assert!(err.contains("no entity Machine/nope"), "{err}");
    }

    #[test]
    fn trace_walks_breadth_first_and_respects_depth() {
        let c = testdb::fixture();
        // box1 -ran-> gen-1, gen-2, gen-3; gen-1 -installs-> StorePath/sp1
        let d1 = trace(&c, "Machine", "box1", 1, Direction::Out, &[], 10)
            .unwrap()
            .hits;
        assert_eq!(d1.len(), 3);
        assert!(d1.iter().all(|h| h.depth == 1));

        let d2 = trace(&c, "Machine", "box1", 2, Direction::Out, &[], 10)
            .unwrap()
            .hits;
        assert_eq!(d2.len(), 4);
        let deep: Vec<_> = d2.iter().filter(|h| h.depth == 2).collect();
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].edge.peer_address(), "StorePath//nix/store/sp1");

        // depth 0 reports nothing; the root is the caller's to print.
        assert!(trace(&c, "Machine", "box1", 0, Direction::Out, &[], 10)
            .unwrap()
            .hits
            .is_empty());
    }

    #[test]
    fn trace_fanout_cap_is_ordered_not_arbitrary() {
        let c = testdb::fixture();
        let hits = trace(&c, "Machine", "box1", 1, Direction::Out, &[], 2)
            .unwrap()
            .hits;
        let refs: Vec<_> = hits.iter().map(|h| h.edge.peer_ref.as_str()).collect();
        // The first two by (relkind, peer kind, peer ref) — stable
        // across runs, which the Python `LIMIT` without `ORDER BY` is
        // not.
        assert_eq!(refs, vec!["gen-1", "gen-2"]);
    }

    #[test]
    fn trace_via_filters_relation_kinds() {
        let c = testdb::fixture();
        let only_installs = trace(
            &c,
            "Machine",
            "box1",
            3,
            Direction::Out,
            &["installs".to_string()],
            10,
        )
        .unwrap()
        .hits;
        // Nothing leaves box1 via `installs`, so the walk stops at once.
        assert!(only_installs.is_empty(), "{only_installs:?}");

        let both = trace(
            &c,
            "Machine",
            "box1",
            3,
            Direction::Out,
            &["ran".to_string(), "installs".to_string()],
            10,
        )
        .unwrap()
        .hits;
        assert_eq!(both.len(), 4);
    }

    /// Multiple `--via` values mean multiple placeholders, which is the
    /// case that catches an off-by-one in the numbering: the `LIMIT`
    /// parameter has to land after the last `via` one. With the index
    /// wrong, SQLite binds a relation kind as the limit and returns
    /// nothing.
    #[test]
    fn via_and_limit_placeholders_are_numbered_consistently() {
        let c = testdb::fixture();
        let hits = trace(
            &c,
            "Machine",
            "box1",
            1,
            Direction::Out,
            &[
                "ran".to_string(),
                "installs".to_string(),
                "used-by".to_string(),
            ],
            2,
        )
        .unwrap();
        // Three `via` values, so the limit is `?5`; the cap of 2 has to
        // still apply to box1's three `ran` edges.
        assert_eq!(hits.hits.len(), 2, "{:?}", hits.hits);
        assert!(hits.truncated, "the cap dropped box1's third `ran` edge");
    }

    fn refs(hits: &[EntityMatch]) -> Vec<&str> {
        hits.iter()
            .map(|h| h.external_ref.as_deref().unwrap_or(""))
            .collect()
    }

    #[test]
    fn search_matches_both_ref_and_label() {
        let c = testdb::fixture();
        // 'box1' is only in external_ref; 'box one' is only in label.
        assert_eq!(
            refs(&search_entities(&c, "box1", None, 30).unwrap()),
            ["box1"]
        );
        assert_eq!(
            refs(&search_entities(&c, "box one", None, 30).unwrap()),
            ["box1"]
        );
        // Case-insensitive in both directions.
        assert_eq!(
            refs(&search_entities(&c, "BOX1", None, 30).unwrap()),
            ["box1"]
        );
    }

    /// A literal `_` in the query must not act as a LIKE wildcard.
    /// `mod_a` and `modxa` differ only in that character; Python's
    /// unescaped pattern returns both.
    #[test]
    fn search_query_underscore_is_literal() {
        let c = testdb::fixture();
        assert_eq!(
            refs(&search_entities(&c, "mod_a", None, 30).unwrap()),
            ["mod_a"]
        );
        // The wildcard reading would be reachable by typing it out, and
        // still is not — there are no wildcards at all.
        assert!(search_entities(&c, "mod%a", None, 30).unwrap().is_empty());
    }

    #[test]
    fn search_filters_by_kind_and_caps_at_limit() {
        let c = testdb::fixture();
        let all = search_entities(&c, "gen", None, 30).unwrap();
        // 'gen-1'/'gen-2'/'gen-3' by ref, and 'gen one'/'gen three' by
        // label — the same three rows either way.
        assert_eq!(refs(&all), ["gen-1", "gen-2", "gen-3"]);
        assert!(search_entities(&c, "gen", Some("Machine"), 30)
            .unwrap()
            .is_empty());
        assert_eq!(
            refs(&search_entities(&c, "gen", Some("Generation"), 30).unwrap()),
            ["gen-1", "gen-2", "gen-3"]
        );
        assert_eq!(search_entities(&c, "gen", None, 2).unwrap().len(), 2);
    }

    /// Newest first, then `(kind, external_ref)` to break the tie. The
    /// 2026-10-01 rows are one big tie, which the Python `ORDER BY
    /// observed_at DESC` alone leaves to the query planner.
    #[test]
    fn search_orders_newest_first_with_a_stable_tie_break() {
        let c = testdb::fixture();
        let hits = search_entities(&c, "o", None, 30).unwrap();
        assert_eq!(
            refs(&hits),
            [
                // 2026-10-02, and 'mod_a' < 'modxa'.
                "mod_a",
                "modxa",
                // 2026-10-01: Generation < Machine < StorePath. gen-1
                // matches on its label ('gen one'), the StorePath on the
                // 'o' in '/nix/store/'.
                "gen-1",
                "box1",
                "/nix/store/sp1",
            ]
        );
        // gen-2 has a NULL label and no 'o' in its ref, so it is absent
        // rather than matching the string "None". gen-3's label is
        // 'gen three', which has no 'o' either.
        assert!(!refs(&hits).contains(&"gen-2"));
        assert!(!refs(&hits).contains(&"gen-3"));
    }

    #[test]
    fn search_reports_no_match_as_an_empty_list() {
        let c = testdb::fixture();
        assert!(search_entities(&c, "nothinghere", None, 30)
            .unwrap()
            .is_empty());
    }

    fn path_of(r: &PathResult) -> Vec<String> {
        match r {
            PathResult::Found { hops, .. } => hops
                .iter()
                .map(|h| format!("{}:{}", h.relkind, h.peer_address()))
                .collect(),
            other => panic!("expected a path, got {other:?}"),
        }
    }

    #[test]
    fn paths_finds_the_shortest_chain() {
        let c = testdb::fixture();
        // box1 -ran-> gen-1 -installs-> sp1
        let r = shortest_path(
            &c,
            ("Machine", "box1"),
            ("StorePath", "/nix/store/sp1"),
            6,
            Direction::Both,
            &[],
        )
        .unwrap();
        assert_eq!(
            path_of(&r),
            ["ran:Generation/gen-1", "installs:StorePath//nix/store/sp1"]
        );
    }

    /// `--max-depth N` admits a path of exactly N hops and nothing
    /// longer, which is the off-by-one worth pinning.
    #[test]
    fn paths_respects_the_depth_cutoff() {
        let c = testdb::fixture();
        let at_limit = shortest_path(
            &c,
            ("Machine", "box1"),
            ("StorePath", "/nix/store/sp1"),
            2,
            Direction::Both,
            &[],
        )
        .unwrap();
        assert_eq!(path_of(&at_limit).len(), 2);

        let too_short = shortest_path(
            &c,
            ("Machine", "box1"),
            ("StorePath", "/nix/store/sp1"),
            1,
            Direction::Both,
            &[],
        )
        .unwrap();
        assert!(matches!(too_short, PathResult::NoPath), "{too_short:?}");
    }

    #[test]
    fn paths_direction_and_via_restrict_the_walk() {
        let c = testdb::fixture();
        // sp1 -used-by-> gen-1 is sp1's only outbound edge, and gen-1's
        // only outbound edge goes back to sp1, so box1 — which reaches
        // gen-1 but is never reached by it — is unreachable
        // outbound-only...
        let out_only = shortest_path(
            &c,
            ("StorePath", "/nix/store/sp1"),
            ("Machine", "box1"),
            6,
            Direction::Out,
            &[],
        )
        .unwrap();
        assert!(matches!(out_only, PathResult::NoPath), "{out_only:?}");

        // ...but reachable walking edges either way. Outbound edges are
        // offered before inbound ones, so gen-1 is reached by sp1's own
        // `used-by` rather than by gen-1's `installs`, and only the
        // second hop is tagged inbound. That tag is what decides whether
        // the CLI prints `→` or `←`.
        let both = shortest_path(
            &c,
            ("StorePath", "/nix/store/sp1"),
            ("Machine", "box1"),
            6,
            Direction::Both,
            &[],
        )
        .unwrap();
        assert_eq!(
            path_of(&both),
            ["used-by:Generation/gen-1", "ran:Machine/box1"]
        );
        if let PathResult::Found { hops, .. } = &both {
            assert_eq!(hops[0].direction, Direction::Out);
            assert_eq!(hops[1].direction, Direction::In);
        }

        // `installs` alone leaves box1 unreachable from sp1.
        let via = shortest_path(
            &c,
            ("StorePath", "/nix/store/sp1"),
            ("Machine", "box1"),
            6,
            Direction::Both,
            &["installs".to_string()],
        )
        .unwrap();
        assert!(matches!(via, PathResult::NoPath), "{via:?}");
    }

    #[test]
    fn paths_reports_source_equals_target_distinctly() {
        let c = testdb::fixture();
        let r = shortest_path(
            &c,
            ("Machine", "box1"),
            ("Machine", "box1"),
            6,
            Direction::Both,
            &[],
        )
        .unwrap();
        assert!(matches!(r, PathResult::SameEntity), "{r:?}");
    }

    #[test]
    fn paths_rejects_unknown_endpoints() {
        let c = testdb::fixture();
        let err = shortest_path(
            &c,
            ("Machine", "box1"),
            ("Machine", "nope"),
            6,
            Direction::Both,
            &[],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("no entity Machine/nope"), "{err}");
    }

    /// The bug this port fixes: Python caps each node's fan-out at an
    /// arbitrary 100 edges, so a direct neighbour past the cap is
    /// reported as unreachable. 150 `ran` edges here; the one we ask for
    /// is beyond any 100 of them.
    #[test]
    fn paths_sees_neighbours_past_pythons_hardcoded_cap() {
        let c = testdb::fixture();
        for i in 0..150 {
            c.execute(
                "INSERT INTO entities (kind, external_ref, source_authority, label, observed_at)
                 VALUES ('Generation', ?1, 'nix', NULL, '2026-10-01 00:00:00')",
                &[Value::from(format!("bulk-{i:03}"))],
            )
            .unwrap();
            c.execute(
                "INSERT INTO relations (from_entity_id, to_entity_id, kind, source_authority)
                 VALUES (1, last_insert_rowid(), 'ran', 'nix')",
                &[],
            )
            .unwrap();
        }
        let r = shortest_path(
            &c,
            ("Machine", "box1"),
            ("Generation", "bulk-149"),
            6,
            Direction::Out,
            &[],
        )
        .unwrap();
        assert_eq!(path_of(&r), ["ran:Generation/bulk-149"]);
    }

    /// Padding rounds up to a bucket, never down — truncating would
    /// silently drop anchors from the level and turn a reachable node
    /// into an unreachable one, which is the same class of bug as
    /// Python's fan-out cap.
    #[test]
    fn frontier_buckets_round_up_and_are_bounded() {
        assert_eq!(bucket_for(1), 1);
        assert_eq!(bucket_for(2), 4);
        assert_eq!(bucket_for(4), 4);
        assert_eq!(bucket_for(5), 16);
        assert_eq!(bucket_for(1024), 1024);
        assert_eq!(bucket_for(1025), FRONTIER_CHUNK);
        // A chunk is never larger than the chunker produces, so the
        // bind-parameter count stays well under Postgres's 65,535.
        assert_eq!(bucket_for(FRONTIER_CHUNK), FRONTIER_CHUNK);
        for n in 1..=FRONTIER_CHUNK {
            assert!(bucket_for(n) >= n, "bucket_for({n}) truncates");
        }
    }

    /// The batched fetch has to give every anchor in the frontier its
    /// own edges, and give an anchor with none no entry at all rather
    /// than someone else's. A single mis-grouped row would show up as a
    /// path through an edge that does not exist.
    #[test]
    fn a_batched_frontier_groups_edges_by_their_own_anchor() {
        let c = testdb::fixture();
        // box1=1 (3 outbound), gen-1=2 (1 outbound), gen-2=3 (none),
        // sp1=5 (1 outbound), and 99 does not exist.
        let (batch, _) = fetch_frontier(&c, &[1, 2, 3, 5, 99], Direction::Out, &[], None).unwrap();
        assert_eq!(batch.get(&1).map_or(0, Vec::len), 3);
        assert_eq!(batch.get(&2).map_or(0, Vec::len), 1);
        assert_eq!(batch.get(&3).map_or(0, Vec::len), 0);
        assert_eq!(batch.get(&5).map_or(0, Vec::len), 1);
        assert_eq!(batch.get(&99).map_or(0, Vec::len), 0);
        // gen-1's single outbound edge is `installs` to sp1, not one of
        // box1's three `ran` edges.
        assert_eq!(batch[&2][0].1.relkind, "installs");
    }

    /// Batching must not change the per-node cap into a per-level cap.
    /// box1 has three outbound edges and gen-1 has one; with a cap of
    /// 2, a level-wide `LIMIT 2` would return two rows total.
    #[test]
    fn the_fanout_cap_is_per_node_not_per_level() {
        let c = testdb::fixture();
        let (batch, truncated) = fetch_frontier(&c, &[1, 2], Direction::Out, &[], Some(2)).unwrap();
        assert_eq!(batch[&1].len(), 2, "box1 should be capped at 2");
        assert_eq!(batch[&2].len(), 1, "gen-1 has only 1 and keeps it");
        // And the cap still takes the first two by the declared order.
        let refs: Vec<&str> = batch[&1].iter().map(|(_, e)| e.peer_ref.as_str()).collect();
        assert_eq!(refs, ["gen-1", "gen-2"]);
        // box1 lost an edge, so the advisory must fire.
        assert!(truncated);
    }

    /// The truncation flag must distinguish "exactly `limit` edges
    /// exist" from "more than `limit` exist" — a bare count cannot,
    /// because both produce `limit` rows. box1 has exactly three
    /// outbound edges, so a cap of 3 drops nothing and a cap of 2 does.
    #[test]
    fn a_cap_that_exactly_fits_is_not_truncation() {
        let c = testdb::fixture();
        let at = |n: i64| {
            let (batch, truncated) =
                fetch_frontier(&c, &[1], Direction::Out, &[], Some(n)).unwrap();
            (batch[&1].len(), truncated)
        };
        assert_eq!(at(3), (3, false), "a cap equal to the fan-out is not a cut");
        assert_eq!(at(4), (3, false), "nor is a cap above it");
        assert_eq!(at(2), (2, true));

        // And no cap at all never reports truncation, which is what
        // `entity paths` relies on.
        let (_, truncated) = fetch_frontier(&c, &[1], Direction::Out, &[], None).unwrap();
        assert!(!truncated);
    }

    /// The flag is sticky across levels, as Python's `any_truncated` is:
    /// a cut at depth 1 is still reported after an uncut depth 2.
    #[test]
    fn truncation_at_any_depth_is_reported() {
        let c = testdb::fixture();
        let cut = |depth: usize, limit: i64| {
            trace(&c, "Machine", "box1", depth, Direction::Out, &[], limit)
                .unwrap()
                .truncated
        };
        assert!(cut(1, 2));
        assert!(cut(3, 2), "still set after deeper, uncapped levels");
        assert!(!cut(3, 50));
    }

    /// Padding a chunk repeats an id, and `IN` is set membership, so a
    /// duplicated anchor must not duplicate its edges. A frontier of 3
    /// pads to the bucket of 4.
    #[test]
    fn padding_the_bucket_does_not_duplicate_edges() {
        let c = testdb::fixture();
        let (batch, _) = fetch_frontier(&c, &[1, 2, 5], Direction::Out, &[], None).unwrap();
        assert_eq!(batch[&1].len(), 3);
        assert_eq!(batch[&2].len(), 1);
        assert_eq!(batch[&5].len(), 1);
    }

    /// A frontier wider than one chunk has to be split and re-merged
    /// without losing or double-counting anyone. 150 nodes against a
    /// chunk size of 4,096 would never split, so the chunking is
    /// exercised by walking the ids in slices here instead — the same
    /// `collect_direction` loop, driven past its boundary.
    #[test]
    fn a_frontier_wider_than_a_chunk_is_split_and_remerged() {
        let c = testdb::fixture();
        for i in 0..150 {
            c.execute(
                "INSERT INTO entities (kind, external_ref, source_authority, label, observed_at)
                 VALUES ('Generation', ?1, 'nix', NULL, '2026-10-01 00:00:00')",
                &[Value::from(format!("wide-{i:03}"))],
            )
            .unwrap();
            c.execute(
                "INSERT INTO relations (from_entity_id, to_entity_id, kind, source_authority)
                 VALUES (1, last_insert_rowid(), 'ran', 'nix')",
                &[],
            )
            .unwrap();
        }
        // box1 now has 153 outbound edges; fetched in one batch they
        // must all arrive, and the walk must see every one of them.
        let (batch, _) = fetch_frontier(&c, &[1], Direction::Out, &[], None).unwrap();
        assert_eq!(batch[&1].len(), 153);

        let hits = trace(&c, "Machine", "box1", 1, Direction::Out, &[], 500)
            .unwrap()
            .hits;
        assert_eq!(hits.len(), 153);
        let mut peers: Vec<_> = hits.iter().map(|h| h.edge.peer_ref.clone()).collect();
        peers.sort();
        peers.dedup();
        assert_eq!(peers.len(), 153, "a peer was reported twice");
    }

    /// The two engines disagree on where NULLs sort by default —
    /// SQLite first on ASC, Postgres last — and `external_ref` is
    /// nullable. `NULLS FIRST` is declared explicitly so the backends
    /// agree; this pins the SQLite behaviour that was there first.
    ///
    /// No row in the live database has a NULL `external_ref`, so the
    /// cross-backend diff cannot catch a regression here. This test is
    /// the only thing that can.
    #[test]
    fn a_null_external_ref_sorts_first_in_search() {
        let c = testdb::fixture();
        c.execute(
            "INSERT INTO entities (kind, external_ref, source_authority, label, observed_at)
             VALUES ('Zephyr', NULL, 'nix', 'nullref o', '2026-10-03 00:00:00')",
            &[],
        )
        .unwrap();
        let hits = search_entities(&c, "o", None, 30).unwrap();
        // Newest first puts the 2026-10-03 row ahead of everything, and
        // its NULL ref must render as absent rather than as "None".
        assert_eq!(hits[0].kind, "Zephyr");
        assert_eq!(hits[0].external_ref, None);

        // Within one timestamp, a NULL ref sorts before a present one.
        c.execute(
            "INSERT INTO entities (kind, external_ref, source_authority, label, observed_at)
             VALUES ('Zephyr', 'zzz-o', 'nix', NULL, '2026-10-03 00:00:00')",
            &[],
        )
        .unwrap();
        let hits = search_entities(&c, "o", None, 30).unwrap();
        assert_eq!(
            hits.iter()
                .filter(|h| h.kind == "Zephyr")
                .map(|h| h.external_ref.clone())
                .collect::<Vec<_>>(),
            [None, Some("zzz-o".to_string())]
        );
    }

    fn dead(hits: &[DeadImport]) -> Vec<String> {
        hits.iter()
            .map(|h| format!("{} -> {}", h.from_ref, h.to_ref))
            .collect()
    }

    /// The three shapes the query distinguishes. `app.py -> used.py` is
    /// bridged by `caller` calling `callee` and must not be reported;
    /// `app.py -> dead.py` has symbols on both ends and no call between
    /// them; `bare.py -> used.py` has no symbols on the source side at
    /// all, which is the case the chain of `LEFT JOIN`s exists for.
    #[test]
    fn dead_imports_reports_only_unbridged_edges() {
        let c = testdb::fixture_with_imports();
        assert_eq!(
            dead(&dead_imports(&c, None, 50).unwrap()),
            [
                "proj_a/app.py -> proj_a/dead.py",
                "proj_a/bare.py -> proj_a/used.py",
                "projxa/app.py -> projxa/dead.py",
            ]
        );
    }

    /// `--slug` must be a literal prefix. `proj_a` and `projxa` differ
    /// only in the character Python's unescaped filter would treat as a
    /// wildcard, so without the escape the first would pull in the
    /// second's import too.
    #[test]
    fn dead_imports_slug_underscore_is_literal() {
        let c = testdb::fixture_with_imports();
        assert_eq!(
            dead(&dead_imports(&c, Some("proj_a"), 50).unwrap()),
            [
                "proj_a/app.py -> proj_a/dead.py",
                "proj_a/bare.py -> proj_a/used.py",
            ]
        );
        assert_eq!(
            dead(&dead_imports(&c, Some("projxa"), 50).unwrap()),
            ["projxa/app.py -> projxa/dead.py"]
        );
        // A slug that matches nothing is empty, not everything — the
        // `CAST(?1 AS TEXT) IS NULL` branch must not fire for a value
        // that is merely absent from the data.
        assert!(dead_imports(&c, Some("nosuch"), 50).unwrap().is_empty());
    }

    #[test]
    fn dead_imports_respects_the_limit_in_a_stable_order() {
        let c = testdb::fixture_with_imports();
        assert_eq!(
            dead(&dead_imports(&c, None, 1).unwrap()),
            ["proj_a/app.py -> proj_a/dead.py"]
        );
    }

    /// A database with no `imports` edges at all answers empty rather
    /// than erroring on the CTE.
    #[test]
    fn dead_imports_on_a_graph_with_no_imports_is_empty() {
        let c = testdb::fixture();
        assert!(dead_imports(&c, None, 50).unwrap().is_empty());
    }

    #[test]
    fn observations_are_newest_first_with_a_total_order() {
        let c = testdb::fixture();
        let rows = observations(&c, "Machine", "box1", 50).unwrap();
        // 2026-10-03 has three rows; `id ASC` orders them 3, 4, 5
        // where Python leaves the three to the planner.
        assert_eq!(
            rows.iter().map(|r| r.id).collect::<Vec<_>>(),
            [3, 4, 5, 2, 1]
        );
    }

    /// The tie-break is what makes `--limit` mean something. With three
    /// rows sharing a timestamp, a limit of 2 has to cut the same two
    /// every run.
    #[test]
    fn observations_limit_cuts_a_tie_deterministically() {
        let c = testdb::fixture();
        let rows = observations(&c, "Machine", "box1", 2).unwrap();
        assert_eq!(rows.iter().map(|r| r.id).collect::<Vec<_>>(), [3, 4]);
    }

    /// Each change is reported only when that field actually moved: row
    /// 2 changed only the authority, row 4 only the label.
    #[test]
    fn observations_distinguish_label_from_authority_changes() {
        let c = testdb::fixture();
        let rows = observations(&c, "Machine", "box1", 50).unwrap();
        let by_id = |id: i64| rows.iter().find(|r| r.id == id).unwrap();

        let authority_only = by_id(2);
        assert!(!authority_only.label_changed());
        assert!(authority_only.authority_changed());

        let label_only = by_id(4);
        assert!(label_only.label_changed());
        assert!(!label_only.authority_changed());

        // Row 3 moved neither; it captured some other update.
        assert!(!by_id(3).label_changed());
        assert!(!by_id(3).authority_changed());
    }

    /// A NULL label is a change from a present one and must survive as
    /// `None` rather than becoming an empty string, because the caller
    /// renders it as Python's bare `None`.
    #[test]
    fn observations_keep_a_null_label_null() {
        let c = testdb::fixture();
        let rows = observations(&c, "Machine", "box1", 50).unwrap();
        let cleared = rows.iter().find(|r| r.id == 5).unwrap();
        assert_eq!(cleared.label, None);
        assert_eq!(cleared.prior_label.as_deref(), Some("box one"));
        assert!(cleared.label_changed());
    }

    #[test]
    fn observations_for_an_unarchived_entity_are_empty() {
        let c = testdb::fixture();
        assert!(observations(&c, "Machine", "nope", 50).unwrap().is_empty());
        // The archive is addressed by kind and ref, so the right ref
        // under the wrong kind is also empty.
        assert!(observations(&c, "Generation", "box1", 50)
            .unwrap()
            .is_empty());
    }

    /// A cycle must terminate. `sp1 -used-by-> box1` closes the loop
    /// back to the root, and the root is pre-visited.
    #[test]
    fn trace_terminates_on_cycles() {
        let c = testdb::fixture();
        let hits = trace(&c, "Machine", "box1", 20, Direction::Both, &[], 50)
            .unwrap()
            .hits;
        // Four entities besides the root, each reported exactly once.
        assert_eq!(hits.len(), 4);
        let mut seen: Vec<_> = hits.iter().map(|h| h.edge.peer_address()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4);
    }
}
