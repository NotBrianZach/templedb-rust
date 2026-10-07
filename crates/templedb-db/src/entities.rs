//! `templedb entity ...` reads — the graph layer.

use crate::like::escape_like;
use anyhow::{Context, Result};
use rusqlite::{params_from_iter, Connection};
use std::collections::{HashMap, HashSet, VecDeque};

/// Entity counts by kind — the cheapest useful probe of the graph, and
/// a direct cross-check against `templedb entity stats`.
pub fn entity_stats(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut stmt =
        conn.prepare("SELECT kind, COUNT(*) FROM entities GROUP BY kind ORDER BY 2 DESC, kind")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("entity stats")
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
pub fn explore(conn: &Connection, kind: &str, external_ref: &str) -> Result<Explored> {
    let (id, label, source_authority, observed_at) = lookup_entity(conn, kind, external_ref)?;
    Ok(Explored {
        kind: kind.to_string(),
        external_ref: external_ref.to_string(),
        label,
        source_authority,
        observed_at,
        outbound: fetch_edges(conn, id, Direction::Out, &[], None)?,
        inbound: fetch_edges(conn, id, Direction::In, &[], None)?,
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
    conn: &Connection,
    kind: &str,
    external_ref: &str,
    depth: usize,
    direction: Direction,
    via: &[String],
    fanout: i64,
) -> Result<Vec<TraceHit>> {
    let (start, _, _, _) = lookup_entity(conn, kind, external_ref)?;

    let mut visited: HashSet<i64> = HashSet::from([start]);
    let mut frontier: Vec<i64> = vec![start];
    let mut hits: Vec<TraceHit> = Vec::new();

    for hop in 1..=depth {
        if frontier.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for &eid in &frontier {
            for (peer_id, edge) in fetch_edges_with_ids(conn, eid, direction, via, Some(fanout))? {
                // A global visited set, matching Python: an entity is
                // reported at the shallowest depth it is reachable at
                // and not expanded twice. Without it, a cycle walks
                // forever at depth > 2.
                if !visited.insert(peer_id) {
                    continue;
                }
                hits.push(TraceHit { depth: hop, edge });
                next.push(peer_id);
            }
        }
        frontier = next;
    }
    Ok(hits)
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
pub fn search_entities(
    conn: &Connection,
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
          ORDER BY observed_at DESC, kind, external_ref
          LIMIT ?{limit_idx}"
    );

    let mut params: Vec<rusqlite::types::Value> = vec![pattern.into()];
    if let Some(k) = kind {
        params.push(k.to_string().into());
    }
    params.push(limit.into());

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(params), |r| {
        Ok(EntityMatch {
            kind: r.get(0)?,
            external_ref: r.get(1)?,
            label: r.get(2)?,
            source_authority: r.get(3)?,
            observed_at: r.get(4)?,
        })
    })?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("searching entities")
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
    conn: &Connection,
    from: (&str, &str),
    to: (&str, &str),
    max_depth: usize,
    direction: Direction,
    via: &[String],
) -> Result<PathResult> {
    let (src, source_label, _, _) = lookup_entity(conn, from.0, from.1)?;
    let (tgt, _, _, _) = lookup_entity(conn, to.0, to.1)?;
    if src == tgt {
        return Ok(PathResult::SameEntity);
    }

    // `parent[node]` is the edge that first reached `node`, plus where it
    // came from; the source maps to `None` and terminates reconstruction.
    // Presence in the map doubles as the visited set, which is what keeps
    // a cycle from re-enqueueing.
    let mut parent: HashMap<i64, Option<(i64, Edge)>> = HashMap::from([(src, None)]);
    let mut queue: VecDeque<(i64, usize)> = VecDeque::from([(src, 0)]);
    let mut found = false;

    'bfs: while let Some((cur, depth)) = queue.pop_front() {
        if depth >= max_depth {
            // Nodes at the cutoff are reported but not expanded, so
            // `--max-depth N` admits paths of exactly N hops.
            continue;
        }
        for (peer, edge) in fetch_edges_with_ids(conn, cur, direction, via, None)? {
            if parent.contains_key(&peer) {
                continue;
            }
            parent.insert(peer, Some((cur, edge)));
            if peer == tgt {
                // Stop on discovery rather than on dequeue. Python waits
                // until the target is popped, which first expands every
                // node already queued ahead of it — thousands of edge
                // queries, for a path that cannot change: `parent[tgt]`
                // is written once and the `contains_key` guard above
                // never lets it be overwritten.
                found = true;
                break 'bfs;
            }
            queue.push_back((peer, depth + 1));
        }
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

/// `(id, label, source_authority, observed_at)` for one entity address.
fn lookup_entity(
    conn: &Connection,
    kind: &str,
    external_ref: &str,
) -> Result<(i64, Option<String>, String, String)> {
    conn.query_row(
        "SELECT id, label, source_authority, observed_at
           FROM entities WHERE kind = ?1 AND external_ref = ?2",
        (kind, external_ref),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            anyhow::anyhow!("no entity {kind}/{external_ref}")
        }
        other => anyhow::Error::new(other),
    })
}

fn fetch_edges(
    conn: &Connection,
    id: i64,
    direction: Direction,
    via: &[String],
    limit: Option<i64>,
) -> Result<Vec<Edge>> {
    Ok(fetch_edges_with_ids(conn, id, direction, via, limit)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// The one place relation SQL is written. Both directions use the same
/// projection so `Edge` is populated identically whichever end the peer
/// sat on; only the join column and the `WHERE` differ.
fn fetch_edges_with_ids(
    conn: &Connection,
    id: i64,
    direction: Direction,
    via: &[String],
    limit: Option<i64>,
) -> Result<Vec<(i64, Edge)>> {
    let mut rows = Vec::new();
    if direction.includes_out() {
        rows.extend(one_direction(
            conn,
            id,
            Direction::Out,
            "r.to_entity_id",
            "r.from_entity_id",
            via,
            limit,
        )?);
    }
    if direction.includes_in() {
        rows.extend(one_direction(
            conn,
            id,
            Direction::In,
            "r.from_entity_id",
            "r.to_entity_id",
            via,
            limit,
        )?);
    }
    Ok(rows)
}

fn one_direction(
    conn: &Connection,
    id: i64,
    direction: Direction,
    peer_col: &str,
    anchor_col: &str,
    via: &[String],
    limit: Option<i64>,
) -> Result<Vec<(i64, Edge)>> {
    // `via` is interpolated as placeholders, never as values — the
    // relation kinds come from a `--via` flag on the command line.
    let placeholders = if via.is_empty() {
        String::new()
    } else {
        format!(" AND r.kind IN ({})", vec!["?"; via.len()].join(","))
    };
    let sql = format!(
        "SELECT e.id, r.kind, e.kind, e.external_ref, e.label
           FROM relations r
           JOIN entities e ON e.id = {peer_col}
          WHERE {anchor_col} = ?{placeholders}
          ORDER BY r.kind, e.kind, e.external_ref
          {}",
        match limit {
            Some(_) => "LIMIT ?",
            None => "",
        }
    );

    let mut params: Vec<rusqlite::types::Value> = vec![id.into()];
    for v in via {
        params.push(v.clone().into());
    }
    if let Some(n) = limit {
        params.push(n.into());
    }

    // `prepare_cached`, not `prepare`: a BFS calls this once per visited
    // node per direction — tens of thousands of times for a deep
    // `entity paths` — while the SQL text varies only over
    // (direction, via.len(), capped or not), a handful of shapes. Without
    // the cache the query planner re-runs on every hop.
    let mut stmt = conn.prepare_cached(&sql)?;
    let out = stmt
        .query_map(params_from_iter(params), |r| {
            Ok((
                r.get(0)?,
                Edge {
                    relkind: r.get(1)?,
                    peer_kind: r.get(2)?,
                    peer_ref: r.get(3)?,
                    peer_label: r.get(4)?,
                    direction,
                },
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("fetching relations")?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdb;

    #[test]
    fn stats_count_every_kind() {
        let c = testdb::fixture();
        let stats = entity_stats(&c);
        let stats = stats.unwrap();
        let total: i64 = stats.iter().map(|(_, n)| n).sum();
        assert_eq!(total, 7);
        // Ordered by count descending, then kind for a stable tie-break.
        assert_eq!(stats[0], ("Generation".to_string(), 3));
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
        let d1 = trace(&c, "Machine", "box1", 1, Direction::Out, &[], 10).unwrap();
        assert_eq!(d1.len(), 3);
        assert!(d1.iter().all(|h| h.depth == 1));

        let d2 = trace(&c, "Machine", "box1", 2, Direction::Out, &[], 10).unwrap();
        assert_eq!(d2.len(), 4);
        let deep: Vec<_> = d2.iter().filter(|h| h.depth == 2).collect();
        assert_eq!(deep.len(), 1);
        assert_eq!(deep[0].edge.peer_address(), "StorePath//nix/store/sp1");

        // depth 0 reports nothing; the root is the caller's to print.
        assert!(trace(&c, "Machine", "box1", 0, Direction::Out, &[], 10)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn trace_fanout_cap_is_ordered_not_arbitrary() {
        let c = testdb::fixture();
        let hits = trace(&c, "Machine", "box1", 1, Direction::Out, &[], 2).unwrap();
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
        .unwrap();
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
        .unwrap();
        assert_eq!(both.len(), 4);
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
                [format!("bulk-{i:03}")],
            )
            .unwrap();
            c.execute(
                "INSERT INTO relations (from_entity_id, to_entity_id, kind, source_authority)
                 VALUES (1, last_insert_rowid(), 'ran', 'nix')",
                [],
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

    /// A cycle must terminate. `sp1 -used-by-> box1` closes the loop
    /// back to the root, and the root is pre-visited.
    #[test]
    fn trace_terminates_on_cycles() {
        let c = testdb::fixture();
        let hits = trace(&c, "Machine", "box1", 20, Direction::Both, &[], 50).unwrap();
        // Four entities besides the root, each reported exactly once.
        assert_eq!(hits.len(), 4);
        let mut seen: Vec<_> = hits.iter().map(|h| h.edge.peer_address()).collect();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 4);
    }
}
