use std::collections::{HashMap, HashSet, VecDeque};

use crate::model::{Cardinality, Metric, SemanticViewDefinition};

use super::facts::{
    collect_derived_metric_grain, collect_referenced_facts, collect_transitive_metric_names,
};
use super::types::{ExpandError, FanTrapError, MetricFanTrapError, ReferencedFactFanTrapError};
use super::where_clause::WhereMember;

/// Cardinality map: `(from_lower, to_lower)` -> (worst-case cardinality,
/// name of a relationship carrying that cardinality).
type CardMap = HashMap<(String, String), (Cardinality, String)>;

/// Check for fan traps: an aggregation whose input rows are multiplied by a
/// one-to-many join boundary.
///
/// Three checks run over the same relationship/cardinality machinery:
///
/// 1. **metric × dimension**: walks the join path between each
///    (metric source, dimension source) pair and errors when any edge is
///    traversed in the fan-out direction (`ExpandError::FanTrap`). This one is
///    about the *dimension* being below the metric's own grain — per-grain
///    aggregation cannot define such a query either, so it runs unconditionally.
/// 2. **metric grain × root** (EXP-1) and 3. **metric × metric** (SG-1): both
///    exist only because the base-anchored `FROM <root>` topology computes every
///    metric over one joined row set. When the caller has routed the query to
///    the per-grain path (`per_grain == true`, see [`super::per_grain`]) each
///    metric is aggregated in its own CTE at its own grain, so neither check
///    applies and both are skipped. On the base-anchored path they error with
///    `ExpandError::RootGrainFanTrap` / `ExpandError::MetricFanTrap` exactly as
///    before.
///
/// # Fan-out direction
///
/// For an edge `(from_alias, to_alias)` with cardinality:
/// - `ManyToOne`: from->to is safe (many go to one), to->from is fan-out
/// - `OneToOne`: both directions are safe
#[allow(clippy::too_many_lines)]
pub(super) fn check_fan_traps(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    resolved_mets: &[&Metric],
    per_grain: bool,
) -> Result<(), ExpandError> {
    if def.joins.is_empty() {
        return Ok(());
    }

    let graph = build_relationship_graph(view_name, def)?;
    let card_map = build_card_map(def);

    // Undirected adjacency over the relationship edges — the path derivation
    // ALL three checks share. It is deliberately not the rooted parent tree:
    // two sibling children of one table are connected through their parent but
    // neither is an ancestor of the other, so a parent-chain walk finds no path
    // between them and the pair goes unchecked (see `GrainGraph`).
    let adjacency = build_adjacency(def);
    let root = graph.root.clone();

    // For each metric + dimension pair, check for fan-out on the join path.
    //
    // EXP-3 (code-review 2026-07-18): EVERY metric gets this check, INCLUDING
    // active semi-additive metrics that take the ROW_NUMBER-CTE snapshot path.
    // The prior SG-6 behaviour skipped them on the (self-described "unproven")
    // assumption that the CTE neutralizes fan-out by selecting one row per
    // partition. It does not: the snapshot runs over the already-fanned join,
    // and RANK ties across the fanned duplicates of one source row are
    // indistinguishable from ties across distinct fact rows, so the CTE cannot
    // dedupe them (silent double-count). Window metrics were never skipped
    // (their inner aggregate is computed over the fanned join before the window
    // function runs); semi-additive metrics are now treated the same way.
    for met in resolved_mets {
        // EXP-8: a base metric with no source table and no base-metric
        // references has an empty grain set but sits at the root grain — the
        // same substitution the EXP-1 root-grain and metric x metric loops make.
        // Without it the inner loop below never runs, and a root-grain base
        // metric paired with a fanning child dimension slips through the check.
        let met_tables = metric_grain(met, def).anchored(&root);

        for dim in resolved_dims {
            let Some(ref dim_table_raw) = dim.source_table else {
                continue; // No source table -> base table dim, skip
            };
            let dim_table = dim_table_raw.to_ascii_lowercase();

            for met_table in &met_tables {
                if *met_table == dim_table {
                    continue; // Same table, no fan-out possible
                }

                // The join path between the two tables, scanned for an edge
                // traversed in the fan-out direction.
                let Some(path) = find_path(met_table, &dim_table, &adjacency) else {
                    continue; // Not connected (legacy joins carrying no FK metadata)
                };
                if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                    return Err(ExpandError::FanTrap {
                        detail: Box::new(FanTrapError {
                            view_name: view_name.to_string(),
                            metric_name: met.name.clone(),
                            metric_table: met
                                .source_table
                                .clone()
                                .unwrap_or_else(|| met_table.clone()),
                            dimension_name: dim.name.clone(),
                            dimension_table: dim_table_raw.clone(),
                            relationship_name: rel_name,
                        }),
                    });
                }
            }
        }
    }

    // EXP-9 (code-review 2026-08-03): the loop above pairs each metric with the
    // QUERIED dimensions, but an active semi-additive metric also drags in the
    // table of its own `NON ADDITIVE BY` dimension — `collect_na_dim_source_tables`
    // joins it into the snapshot CTE exactly as a queried dimension's table is
    // joined. Un-queried, it appears in `resolved_dims` nowhere, so nothing
    // checked it: a metric at the root grain whose NA dimension sits on a
    // fanning child passed the whole fence (metric table == root, so the
    // root-grain loop skips it too) and silently double-counted, while
    // *querying* the same dimension correctly raised `FanTrap` for the identical
    // join.
    //
    // Deliberately ABOVE the `per_grain` early return: this join lives inside
    // the snapshot CTE, which both the base-anchored and the anchored/per-grain
    // emitters build (`SnapshotBlock` is shared). `snapshot_cte_anchor` re-anchors
    // the CTE but only checks that its extra tables are *reachable*, not that
    // reaching them is non-fanning, so the hazard survives re-anchoring.
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    for na in
        super::semi_additive::na_dim_fence_members(view_name, def, resolved_mets, &queried_dim_keys)
    {
        let met = resolved_mets[na.metric_idx];
        let met_tables = metric_grain(met, def).anchored(&root);
        for met_table in &met_tables {
            if *met_table == na.source_table_key {
                continue; // Same table, no fan-out possible.
            }
            // Path and cardinality lookups are keyed on the folded spelling;
            // the error payload reports the declared one, matching the
            // queried-dimension branch above.
            let Some(path) = find_path(met_table, &na.source_table_key, &adjacency) else {
                continue; // Not connected (legacy joins carrying no FK metadata).
            };
            if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                return Err(ExpandError::FanTrap {
                    detail: Box::new(FanTrapError {
                        view_name: view_name.to_string(),
                        metric_name: met.name.clone(),
                        metric_table: met
                            .source_table
                            .clone()
                            .unwrap_or_else(|| met_table.clone()),
                        dimension_name: na.dim_name,
                        dimension_table: na.source_table_raw,
                        relationship_name: rel_name,
                    }),
                });
            }
        }
    }

    // Everything below this point is a consequence of the base-anchored
    // topology: with each metric aggregated in its own per-grain CTE
    // (`per_grain`), no metric is computed over another grain's row set, so the
    // root-grain and metric × metric checks have nothing to guard.
    if per_grain {
        return Ok(());
    }

    // EXP-1 (code-review 2026-07-18): the root table is an IMPLICIT fan-trap
    // participant. Generated SQL is always anchored `FROM <root>` with LEFT
    // JOINs outward, so a metric whose grain table is a PARENT/ancestor of the
    // root across a fan-out edge is duplicated once per root row and silently
    // inflated — even when queried alone (no dimension, no second metric to
    // trigger the pairwise loops above). Treat the root as an implicit
    // dimension: for every metric grain table, walk the path to the root and
    // reject a fan-out edge. A metric at or below the root grain (the root
    // itself, or a child on the FK/"many" side) traverses only safe forward
    // edges, so this never fires for it (nor for OneToOne edges).
    for met in resolved_mets {
        let met_tables = metric_grain(met, def).anchored(&root);
        for met_table in &met_tables {
            if *met_table == root {
                continue; // At the root grain: nothing fans it.
            }
            let Some(path) = find_path(met_table, &root, &adjacency) else {
                continue; // Not connected (legacy joins carrying no FK metadata).
            };
            if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                return Err(ExpandError::RootGrainFanTrap {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    metric_table: met
                        .source_table
                        .clone()
                        .unwrap_or_else(|| met_table.clone()),
                    relationship_name: rel_name,
                });
            }
        }
    }

    // SG-1 (code review 2026-07-02): metric × metric grain check.
    // For each ordered pair (A, B) of queried metrics, treat B's source table
    // the way a dimension table is treated above: if the join path from A's
    // grain table to B's grain table crosses a fan-out edge in the direction
    // that multiplies A's rows, A's aggregate would be silently inflated.
    // Metrics with no source table (base-table metrics) sit at the root grain
    // and participate — they are the ones inflated by joins to child tables.
    let grains: Vec<Vec<String>> = resolved_mets
        .iter()
        .map(|m| metric_grain(m, def).anchored(&root))
        .collect();

    // EXP-2 (code-review 2026-07-18): a SINGLE metric whose OWN grain set spans
    // more than one table — a derived metric over base metrics on different
    // tables, or a window metric whose inner aggregate's grain differs — is
    // never checked against itself by the ordered metric × metric loop below,
    // which skips `i == j`. Folding two grains into one metric erases the
    // protection: aggregating it inflates the parent-side component over the
    // fanned join exactly as two separate metrics would. Check every pair WITHIN
    // each metric's own grain set (both traversal directions, since a fan is
    // only visible walking the parent → child leg), erroring with that metric.
    for (i, met) in resolved_mets.iter().enumerate() {
        for table_a in &grains[i] {
            for table_b in &grains[i] {
                if table_a == table_b {
                    continue;
                }
                let Some(path) = find_path(table_a, table_b, &adjacency) else {
                    continue;
                };
                if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                    return Err(ExpandError::MetricFanTrap {
                        detail: Box::new(MetricFanTrapError {
                            view_name: view_name.to_string(),
                            metric_name: met.name.clone(),
                            metric_table: table_a.clone(),
                            other_metric_name: met.name.clone(),
                            other_metric_table: table_b.clone(),
                            relationship_name: rel_name,
                        }),
                    });
                }
            }
        }
    }

    for (i, met_a) in resolved_mets.iter().enumerate() {
        // EXP-3 (code-review 2026-07-18): active semi-additive metrics are no
        // longer skipped here either — a CTE-handled metric is still inflated as
        // the potentially-multiplied side when a co-queried metric's table fans
        // it, the same reason the metric × dimension loop above now checks them.
        for (j, met_b) in resolved_mets.iter().enumerate() {
            if i == j {
                continue;
            }
            for table_a in &grains[i] {
                for table_b in &grains[j] {
                    if table_a == table_b {
                        continue; // Same grain, no fan-out possible
                    }
                    // Tables not connected by PK/FK edges (e.g. legacy joins
                    // with no FK metadata) cannot be analyzed — mirror the
                    // met×dim behavior and skip rather than error.
                    let Some(path) = find_path(table_a, table_b, &adjacency) else {
                        continue;
                    };
                    if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                        return Err(ExpandError::MetricFanTrap {
                            detail: Box::new(MetricFanTrapError {
                                view_name: view_name.to_string(),
                                metric_name: met_a.name.clone(),
                                metric_table: table_a.clone(),
                                other_metric_name: met_b.name.clone(),
                                other_metric_table: table_b.clone(),
                                relationship_name: rel_name,
                            }),
                        });
                    }
                }
            }
        }
    }

    Ok(())
}

/// Build the relationship graph, surfacing construction failure as an error.
///
/// SG-7 (code review 2026-07-02): previously a failed graph build silently
/// SKIPPED the fan-trap safety check (`return Ok(())`), so queries over
/// legacy/invalid stored definitions produced mis-aggregated results with no
/// warning. The correct bias for a safety check is loud failure.
///
/// SG-7 residual (AR-4): a build *failure* was the rare case. The common
/// legacy hazard is a definition whose relationships lack `fk_columns` — an
/// empty-FK join is silently skipped by both `RelationshipGraph::from_definition`
/// and `build_card_map`, so the graph builds *successfully but empty* and the
/// fan-trap check passes vacuously. Reject such definitions up front so an
/// un-upgradeable legacy row (which the `init_catalog` upgrade pass leaves at
/// `schema_version` 0) fails loudly here instead of under-checking.
///
/// EXP-15 (code review 2026-08-04): the same reasoning applied once more, to
/// CYCLES. Cycle detection is a CREATE-time check — `graph::validate_graph` runs
/// `toposort` — that this query-time rebuild never repeated;
/// `RelationshipGraph::from_definition` rejects only self-references. A stored
/// cyclic definition (parser-reachable, per #141/d48abee, which proved the shape
/// reaches `expand`) therefore arrived at [`fanning_edge_on_path`] intact, and
/// that function tests the FORWARD key `(a, b)` first and declares the hop safe
/// on finding it — never looking at the reverse `ManyToOne` edge `(b, a)` that
/// fans. The definition passed the fence.
///
/// d48abee fixed the resulting hang and deliberately left the Ok/Err outcome
/// unspecified. This specifies it: a cycle asserts "many `a` per `b`" and "many
/// `b` per `a`" at once, which is not a topology with a correct answer, so the
/// fence declines to certify rather than picking a direction.
fn build_relationship_graph(
    view_name: &str,
    def: &SemanticViewDefinition,
) -> Result<crate::graph::RelationshipGraph, ExpandError> {
    if def.has_incomplete_relationships() {
        return Err(ExpandError::UncheckableDefinition {
            view_name: view_name.to_string(),
            reason: "one or more relationships are missing foreign-key column metadata \
                     (a legacy pre-Phase-24 definition format)"
                .to_string(),
        });
    }
    let uncheckable = |reason: String| ExpandError::UncheckableDefinition {
        view_name: view_name.to_string(),
        reason,
    };
    let graph = crate::graph::RelationshipGraph::from_definition(def).map_err(uncheckable)?;
    // EXP-15: re-run the CREATE-time cycle check. `toposort`'s error names the
    // cycle path, which is what makes the message actionable.
    graph.toposort().map_err(uncheckable)?;
    Ok(graph)
}

/// Build the cardinality map keyed by `(from_lower, to_lower)`.
///
/// SG-16 (code review 2026-07-02): multiple named relationships may connect
/// the same table pair (role-playing). For fan detection the WORST case must
/// win: if ANY edge between the pair is `ManyToOne` (fan-capable when
/// traversed PK-side -> FK-side), the pair is treated as `ManyToOne`, and the
/// recorded relationship name is that of a `ManyToOne` edge so error messages
/// cite a relationship that actually fans. Previously the last-declared edge
/// simply overwrote earlier ones, so a `OneToOne` declared after a
/// `ManyToOne` masked the fan-out (declaration-order dependent).
fn build_card_map(def: &SemanticViewDefinition) -> CardMap {
    let mut card_map: CardMap = HashMap::new();
    for join in def.joins.iter().filter(|j| !j.fk_columns.is_empty()) {
        let key = (
            join.from_alias.to_ascii_lowercase(),
            join.table.to_ascii_lowercase(),
        );
        let rel_name = join.name.as_deref().unwrap_or(&join.from_alias).to_string();
        match card_map.entry(key) {
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert((join.cardinality, rel_name));
            }
            std::collections::hash_map::Entry::Occupied(mut e) => {
                if e.get().0 == Cardinality::OneToOne && join.cardinality == Cardinality::ManyToOne
                {
                    e.insert((join.cardinality, rel_name));
                }
            }
        }
    }
    card_map
}

/// The grain topology of a definition: the relationship graph as an undirected
/// adjacency, plus the worst-case cardinality of each edge.
///
/// Owned here, next to the fan-trap checks that define what "fanning" means, and
/// consumed by [`super::per_grain`] — the planner asks the same questions the
/// fence asks ("does the path from this grain to that one fan?"), and the
/// emitter needs the path itself to synthesize the joins inside a grain CTE. One
/// definition of the topology, two consumers.
///
/// Paths are found over the UNDIRECTED relationship graph, not the rooted
/// parent tree. Two SIBLING children of one table (`line_items` and `shipments`
/// both referencing `orders`) are connected through their shared parent, but
/// neither is an ancestor of the other — a parent-chain walk finds no path
/// between them at all, and a fan-trap check that silently skipped the pair
/// would let a metric on one be grouped by a dimension on the other, inflated
/// by the join. The undirected walk finds the real path, `li -> o -> s`, whose
/// second leg fans.
pub(super) struct GrainGraph {
    root: String,
    adjacency: HashMap<String, Vec<String>>,
    card_map: CardMap,
}

impl GrainGraph {
    /// Build from a definition, or `None` when the relationship graph cannot be
    /// rebuilt. Callers that need the failure to be loud (the fence) build the
    /// graph through [`build_relationship_graph`] instead; the per-grain planner
    /// simply declines to route, leaving the fence to raise
    /// `UncheckableDefinition`.
    pub(super) fn build(def: &SemanticViewDefinition) -> Option<Self> {
        let graph = crate::graph::RelationshipGraph::from_definition(def).ok()?;
        Some(Self {
            root: graph.root.clone(),
            adjacency: build_adjacency(def),
            card_map: build_card_map(def),
        })
    }

    /// The base-table alias (lowercased).
    pub(super) fn root(&self) -> &str {
        &self.root
    }

    /// The join path `[from, …, to]`, or `None` when the two are not connected.
    pub(super) fn path(&self, from: &str, to: &str) -> Option<Vec<String>> {
        find_path(from, to, &self.adjacency)
    }

    /// The name of a relationship on the `from` → `to` path that is traversed in
    /// the fan-out direction — i.e. aggregating `from`'s rows over this path
    /// would multiply them. `None` means the path is safe to join.
    pub(super) fn fanning_relationship(&self, from: &str, to: &str) -> Option<String> {
        let path = self.path(from, to)?;
        fanning_edge_on_path(&path, &self.card_map)
    }
}

/// Source tables (lowercased, sorted, deduped) whose rows feed `met`'s
/// aggregation — the metric's "grain".
///
/// - Base metric with `source_table`: that table.
/// - Derived metric (`source_table == None`): transitive base-metric source
///   tables via the dependency graph.
/// - Window metric: additionally the INNER metric's source tables — the inner
///   aggregate is what is computed over the joined row set (the window
///   function itself runs over the pre-aggregated CTE), so the inner metric's
///   grain is what fan-out inflates.
///
/// The list names only the tables metrics *declare*. A component that
/// aggregates at the root grain without naming it declares nothing, and shows
/// up in [`MetricGrain::at_root`] instead — see [`MetricGrain::anchored`],
/// which is what nearly every caller wants.
pub(super) fn metric_grain(met: &Metric, def: &SemanticViewDefinition) -> MetricGrain {
    let mut at_root = false;
    let mut tables: Vec<String> = if let Some(ref st) = met.source_table {
        vec![st.to_ascii_lowercase()]
    } else {
        // Derived metric: walk dependency graph to find transitive base metric source tables
        let grain = collect_derived_metric_grain(met, &def.metrics);
        at_root = grain.at_root;
        grain
            .tables
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    };
    if let Some(ref ws) = met.window_spec {
        // EXP-12: resolve the inner-metric reference through `ident_matches`,
        // the same canonical rule `expand::window` uses to emit it. A raw
        // `eq_ignore_ascii_case` compares the quote characters as data, so a
        // quoted reference (`"total"`) matched no metric, the inner aggregate's
        // table dropped out of the grain set, and the root-grain check below
        // had nothing to fire on — leaving the base-anchored `__sv_agg` CTE to
        // compute that aggregate over a fanned join.
        if !crate::ident::ident_matches(&met.name, &ws.inner_metric) {
            if let Some(inner) = def
                .metrics
                .iter()
                .find(|m| crate::ident::ident_matches(&m.name, &ws.inner_metric))
            {
                if let Some(ref st) = inner.source_table {
                    tables.push(st.to_ascii_lowercase());
                } else {
                    let grain = collect_derived_metric_grain(inner, &def.metrics);
                    at_root |= grain.at_root;
                    tables.extend(grain.tables.into_iter().map(|s| s.to_ascii_lowercase()));
                }
            }
        }
    }
    tables.sort_unstable();
    tables.dedup();
    MetricGrain { tables, at_root }
}

/// Where a metric aggregates: the tables its components name, plus whether one
/// of those components sits at the root grain without naming a table.
pub(super) struct MetricGrain {
    tables: Vec<String>,
    at_root: bool,
}

impl MetricGrain {
    /// The grain as a table list, with `root` standing in for a component that
    /// names no table.
    ///
    /// EXP-8 (code-review 2026-07-18) made this substitution for a metric whose
    /// grain set was *entirely* empty — a base metric with no source table and
    /// no base-metric references. EXP-11 (code-review 2026-08-03) generalised
    /// it: the substitution is per *component*, so a derived metric that mixes
    /// a source-less aggregate with a real-table component is at BOTH grains.
    /// Under the narrower rule the root component vanished whenever anything
    /// else contributed a table, and the fence's root-grain check and the
    /// per-grain planner both anchored the whole expression at the survivor.
    pub(super) fn anchored(&self, root: &str) -> Vec<String> {
        let mut tables = self.tables.clone();
        if (self.at_root || tables.is_empty()) && !tables.iter().any(|t| t == root) {
            tables.push(root.to_string());
            tables.sort_unstable();
        }
        tables
    }

    /// The tables the metric's components actually name, with no root
    /// substitution — for callers asking "which tables does this touch?"
    /// rather than "at what grain does this aggregate?".
    pub(super) fn named_tables(&self) -> &[String] {
        &self.tables
    }
}

/// Undirected adjacency over relationship edges (lowercased aliases), built
/// in declaration order for deterministic traversal.
fn build_adjacency(def: &SemanticViewDefinition) -> HashMap<String, Vec<String>> {
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for join in def.joins.iter().filter(|j| !j.fk_columns.is_empty()) {
        let from = join.from_alias.to_ascii_lowercase();
        let to = join.table.to_ascii_lowercase();
        if from == to {
            continue; // Self-references are rejected by graph construction
        }
        let entry = adjacency.entry(from.clone()).or_default();
        if !entry.contains(&to) {
            entry.push(to.clone());
        }
        let entry = adjacency.entry(to).or_default();
        if !entry.contains(&from) {
            entry.push(from);
        }
    }
    adjacency
}

/// Find a path between two table aliases treating relationship edges as
/// undirected (BFS). In a validated relationship tree the path is unique;
/// role-playing multi-edges between one pair are collapsed to a single
/// adjacency entry (their cardinality is collapsed to the worst case in the
/// card map). Returns `None` when the tables are not connected (e.g. legacy
/// joins carrying no FK metadata).
fn find_path(
    start: &str,
    goal: &str,
    adjacency: &HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    if start == goal {
        return Some(vec![start.to_string()]);
    }
    let mut prev: HashMap<String, String> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    let mut queue: VecDeque<String> = VecDeque::new();
    visited.insert(start.to_string());
    queue.push_back(start.to_string());
    while let Some(node) = queue.pop_front() {
        let Some(neighbors) = adjacency.get(&node) else {
            continue;
        };
        for neighbor in neighbors {
            if visited.insert(neighbor.clone()) {
                prev.insert(neighbor.clone(), node.clone());
                if neighbor == goal {
                    let mut path = vec![goal.to_string()];
                    let mut current = goal;
                    while let Some(p) = prev.get(current) {
                        path.push(p.clone());
                        current = p;
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(neighbor.clone());
            }
        }
    }
    None
}

/// Scan consecutive pairs along `path` for an edge traversed in the fan-out
/// direction, returning the fanning relationship's name.
///
/// The card map stores edges as `(from, to)` = (FK side, PK side). Traversing
/// a pair `(a, b)` when the stored edge is `(a, b)` follows the FK forward
/// (many rows -> one row: safe). Traversing when the stored edge is `(b, a)`
/// goes from the PK side to the FK side, which multiplies rows when the edge
/// is `ManyToOne`. `OneToOne` edges are safe in both directions.
///
/// **Precondition: the graph is acyclic.** The forward key is tested first and
/// wins, so if BOTH `(a, b)` and `(b, a)` are present this reports the hop safe
/// on the strength of the forward edge alone. That pair of edges is a two-cycle,
/// and no cyclic definition survives to a *verdict*: both fence entry points
/// ([`check_fan_traps`], [`validate_fact_table_path`]) build the graph via
/// [`build_relationship_graph`], which rejects cycles outright (EXP-15).
///
/// [`GrainGraph`] is the one caller that does not — it builds from
/// `RelationshipGraph::from_definition` directly, so the per-grain PLANNER can
/// ask a cyclic graph for a fan verdict and get this optimistic answer. That is
/// contained: `sql_gen` runs [`check_fan_traps`] before handing off to
/// `expand_per_grain`, so the routing decision is discarded and the query errors
/// before any SQL is emitted.
///
/// Deliberately not "fixed" by also testing the reverse key — mutually
/// contradictory cardinalities have no correct fan verdict to return, so the
/// answer is to decline the definition upstream, not to invent a direction here.
fn fanning_edge_on_path(path: &[String], card_map: &CardMap) -> Option<String> {
    for window in path.windows(2) {
        let a = &window[0];
        let b = &window[1];
        if card_map.contains_key(&(a.clone(), b.clone())) {
            // Edge is a -> b (a has FK pointing to b): forward direction,
            // no fan-out possible.
        } else if let Some((card, rel_name)) = card_map.get(&(b.clone(), a.clone())) {
            // Edge is b -> a (b has FK pointing to a). Walking a -> b means
            // traversing this edge in REVERSE: ManyToOne reverse = fan-out.
            if *card == Cardinality::ManyToOne {
                return Some(rel_name.clone());
            }
        }
    }
    None
}

/// The metric × `where_clause`-member fan-out check.
///
/// Snowflake counts `WHERE`-clause members in its same-logical-table rule
/// explicitly — "all facts and dimensions used in the query, **including those
/// specified in the WHERE clause**" — and the hazard is identical to the metric
/// × dimension one [`check_fan_traps`] guards: to filter on a table you must
/// join it, and if that join crosses a one-to-many edge in the fan-out
/// direction, the metric aggregates over multiplied rows. A filter that
/// silently inflates the number it filters is the worst version of this bug, so
/// it is rejected rather than fenced.
///
/// Kept separate from [`check_fan_traps`] rather than folded into its signature
/// so that function's existing call sites and tests are untouched; both walk the
/// same adjacency + cardinality machinery.
///
/// Skipped on the per-grain path for the same reason [`check_fan_traps`] skips
/// its base-anchored-only checks there — though today the per-grain strategy
/// rejects a `where_clause` outright, so this is belt and braces.
///
/// # What counts as "the member's table"
///
/// EXP-27 (code-review 2026-08-08). A member forces a join not only of the
/// table it is *declared* on but of every table its expression reaches through
/// a fact reference ([`WhereMember::fact_tables`], the set #207 added to
/// `source_tables` for exactly that reason). Both are joined on the member's
/// behalf and both multiply the metric's rows if the path fans, so both are
/// walked here. Checking only the declared table meant `of AS li.liq * 2` on
/// the base — whose expression lives entirely on a child table — passed the
/// fence while its child table was joined anyway, doubling every base-grain
/// metric. That shape was a loud binder error before #207 joined the table.
pub(super) fn check_where_clause_fan_traps(
    view_name: &str,
    def: &SemanticViewDefinition,
    where_members: &[WhereMember],
    resolved_mets: &[&Metric],
) -> Result<(), ExpandError> {
    if def.joins.is_empty() || where_members.is_empty() {
        return Ok(());
    }
    let graph = build_relationship_graph(view_name, def)?;
    let card_map = build_card_map(def);
    let adjacency = build_adjacency(def);
    let root = graph.root.clone();

    for met in resolved_mets {
        // The metric's own grain is the anchor, not the member's table: a
        // metric already AT the child grain is not multiplied by joining the
        // child, so the same predicate is sound for it. This is why the fence
        // is per-metric rather than a view-wide member check.
        let met_tables = metric_grain(met, def).anchored(&root);
        for member in where_members {
            // A member declared without a table is base-table grain, so it
            // forces no join of its own — but its fact chain still can.
            let joined_tables = member.table.iter().chain(member.fact_tables.iter());
            for member_table in joined_tables {
                for met_table in &met_tables {
                    if met_table == member_table {
                        continue;
                    }
                    let Some(path) = find_path(met_table, member_table, &adjacency) else {
                        continue; // Not connected (legacy joins carrying no FK metadata).
                    };
                    if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                        return Err(ExpandError::WhereClauseFanTrap {
                            view_name: view_name.to_string(),
                            metric_name: met.name.clone(),
                            member_name: member.name.clone(),
                            member_table: member_table.clone(),
                            relationship_name: rel_name,
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reject a member whose expression reaches a fact on a **fanning** table.
///
/// PAR-6 (code-review 2026-08-03, TECH-DEBT #53). Referencing a named fact
/// declared on another logical table is Snowflake's supported way to cross
/// tables, and `join_resolver` now joins that table so the inlined expression
/// binds. Joining is only sound when the path from the member's own table to
/// the fact's does not fan: a fact on a *child* table multiplies the member's
/// rows, which inflates a metric's aggregate (an order counted once per line
/// item) and duplicates a dimension's output rows.
///
/// This is the same condition [`check_where_clause_fan_traps`] applies to a
/// predicate member and [`check_fan_traps`] applies to a queried dimension,
/// over the same adjacency + cardinality machinery; only the reason for
/// needing the join differs.
///
/// Runs on every emission path. Unlike the base-anchored checks it is not
/// skipped in per-grain mode: a referenced fact is inlined *inside* its
/// member's expression, so per-grain aggregation does not separate the two the
/// way it separates two metrics at different grains — the fanning join would
/// land inside the grain CTE just the same.
pub(super) fn check_referenced_fact_fan_traps(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    resolved_mets: &[&Metric],
) -> Result<(), ExpandError> {
    if def.joins.is_empty() || def.facts.is_empty() {
        return Ok(());
    }
    let graph = build_relationship_graph(view_name, def)?;
    let card_map = build_card_map(def);
    let adjacency = build_adjacency(def);
    let root = graph.root.clone();

    // (member name, the tables the member itself sits at, its expression).
    // Dimensions are fenced alongside metrics since TECH-DEBT #54 made their
    // expressions fact-inlined: a dimension reaching a fact on a fanning table
    // duplicates the output rows, the row-level counterpart of the inflated
    // aggregate the same join gives a metric.
    let mut members: Vec<(&str, Vec<String>, &str)> = Vec::new();
    for dim in resolved_dims {
        let tables = dim
            .source_table
            .as_ref()
            .map_or_else(|| vec![root.clone()], |st| vec![st.to_ascii_lowercase()]);
        members.push((dim.name.as_str(), tables, dim.expr.as_str()));
    }
    // Every metric a queried one inlines or aggregates through — a derived
    // metric's base metrics and a window metric's inner aggregate — is fenced
    // under its own name and grain, because the join resolver joins the tables
    // THEIR fact references reach (PR #247 review: a window's inner metric over
    // a child-table fact was joined unfenced once the resolver learned to
    // follow it; the derived-metric walk had the same blind spot). The metric
    // itself comes first, so a directly-queried one is reported under its own
    // name exactly as before; its dependencies follow in declaration order, so
    // the member an error names is deterministic.
    let mut metric_members: Vec<&Metric> = Vec::new();
    for met in resolved_mets {
        let deps = collect_transitive_metric_names(met, &def.metrics);
        let reached = std::iter::once(*met).chain(
            def.metrics
                .iter()
                .filter(|m| deps.contains(&crate::ident::normalize_ident_part(&m.name))),
        );
        for m in reached {
            if !metric_members.iter().any(|seen| std::ptr::eq(*seen, m)) {
                metric_members.push(m);
            }
        }
    }
    for met in metric_members {
        members.push((
            met.name.as_str(),
            metric_grain(met, def).anchored(&root),
            met.expr.as_str(),
        ));
    }

    for (member_name, member_tables, expr) in members {
        for (fact_name, fact_table) in collect_referenced_facts(expr, &def.facts) {
            for member_table in &member_tables {
                if *member_table == fact_table {
                    continue;
                }
                let Some(path) = find_path(member_table, &fact_table, &adjacency) else {
                    continue; // Not connected (legacy joins carrying no FK metadata).
                };
                if let Some(rel_name) = fanning_edge_on_path(&path, &card_map) {
                    return Err(ExpandError::ReferencedFactFanTrap {
                        detail: Box::new(ReferencedFactFanTrapError {
                            view_name: view_name.to_string(),
                            member_name: member_name.to_string(),
                            member_table: member_table.clone(),
                            fact_name: fact_name.clone(),
                            fact_table: fact_table.clone(),
                            relationship_name: rel_name,
                        }),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Validate that all tables referenced by a fact query can be joined without
/// multiplying rows.
///
/// Snowflake constraint: "all facts and dimensions used in the query must be
/// defined in the same logical table." For our multi-table model this is
/// relaxed to: for every pair of `source_table` aliases, one must be reachable
/// from the other along the join path without traversing a many-to-one edge
/// backwards. A fact query does not aggregate, so a fan-out here does not skew
/// a total — it silently duplicates the rows returned, which is why the pair is
/// rejected outright rather than fenced.
///
/// Skips validation when there is only one unique table (trivially valid)
/// or when there are no joins (single-table view).
pub(super) fn validate_fact_table_path(
    view_name: &str,
    def: &SemanticViewDefinition,
    fact_tables: &[String],
    dim_tables: &[String],
) -> Result<(), ExpandError> {
    if def.joins.is_empty() {
        return Ok(());
    }

    // Collect all unique table aliases (case-insensitive)
    let mut all_tables: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for t in fact_tables.iter().chain(dim_tables.iter()) {
        let lower = t.to_ascii_lowercase();
        if seen.insert(lower.clone()) {
            all_tables.push(lower);
        }
    }

    if all_tables.len() <= 1 {
        return Ok(()); // Single table or empty — trivially valid
    }

    // SG-7: an unbuildable graph is an error, not a skipped check.
    build_relationship_graph(view_name, def)?;

    let adjacency = build_adjacency(def);
    let card_map = build_card_map(def);

    // For each pair, one table's rows must be enrichable from the other without
    // multiplying them — i.e. the join path between them is traversable in at
    // least one direction without crossing a many-to-one edge backwards.
    //
    // TECH-DEBT #37: this used to ask the parent tree whether one table was an
    // ancestor of the other. Ancestry is the wrong question in both directions.
    // Too strict: under fan-in the chain ran out through a sibling branch, so
    // `s -> o -> c` (many-to-one at every hop, perfectly safe) was rejected.
    // Too loose, once the tree is correctly rooted at the base table: the base
    // table becomes an ancestor of EVERY alias, including siblings it can only
    // reach by walking a many-to-one edge backwards. Checking the path's
    // direction answers both at once.
    for i in 0..all_tables.len() {
        for j in (i + 1)..all_tables.len() {
            let a = &all_tables[i];
            let b = &all_tables[j];
            let safe = find_path(a, b, &adjacency).is_some_and(|forward| {
                let backward: Vec<String> = forward.iter().rev().cloned().collect();
                fanning_edge_on_path(&forward, &card_map).is_none()
                    || fanning_edge_on_path(&backward, &card_map).is_none()
            });
            if !safe {
                return Err(ExpandError::FactPathViolation {
                    view_name: view_name.to_string(),
                    table_a: a.clone(),
                    table_b: b.clone(),
                });
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::test_helpers::{minimal_def, orders_view, TestFixtureExt};
    use crate::model::{Cardinality, NullsOrder, SortOrder, WindowSpec};

    // The directed ancestor-walk helpers now live on `crate::graph::JoinTree`,
    // where their unit tests moved too (§6.2 move 5, E-7).

    #[test]
    fn test_check_fan_traps_no_joins_ok() {
        let def = orders_view();
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(result.is_ok(), "No joins should be OK");
    }

    #[test]
    fn test_check_fan_traps_many_to_one_safe_direction() {
        // orders (root) -> customers via FK. Metric on orders, dim on customers.
        // This is ManyToOne from orders->customers (orders has FK to customers).
        // Walking from orders metric to customers dim goes forward on the FK edge = safe.
        let def = minimal_def("orders", "cust_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "name", Some("customers"))
            .with_metric("total", "sum(amount)", Some("orders"))
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        // Remove the initial minimal_def dims/metrics (they have no source_table)
        let mut def = def;
        def.dimensions
            .retain(|d| d.source_table.is_some() || d.name == "cust_name");
        def.metrics
            .retain(|m| m.source_table.is_some() || m.name == "total");
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            result.is_ok(),
            "ManyToOne forward direction should be safe, got: {result:?}"
        );
    }

    #[test]
    fn test_check_fan_traps_many_to_one_fan_out() {
        // orders (root) -> line_items. line_items has FK to orders (ManyToOne from line_items->orders).
        // Metric on orders, dimension on line_items.
        // Walking from orders to line_items reverses the ManyToOne edge = fan-out.
        let def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("line_items", "line_items", &["id"])
            .with_dimension("item_name", "name", Some("line_items"))
            .with_metric("total", "sum(amount)", Some("orders"))
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(result.is_err(), "Should detect fan-out");
        if let Err(ExpandError::FanTrap { detail }) = &result {
            assert_eq!(detail.metric_name, "total");
        } else {
            panic!("Expected FanTrap error, got: {result:?}");
        }
    }

    #[test]
    fn test_check_fan_traps_one_to_one_safe() {
        // Same fan-out scenario as above, but with OneToOne cardinality.
        let def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("line_items", "line_items", &["id"])
            .with_dimension("item_name", "name", Some("line_items"))
            .with_metric("total", "sum(amount)", Some("orders"))
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        // Mutate the join to OneToOne
        def.joins.last_mut().unwrap().cardinality = Cardinality::OneToOne;
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            result.is_ok(),
            "OneToOne should be safe regardless of direction, got: {result:?}"
        );
    }

    /// SG-6: a semi-additive metric whose NA dims are ALL in the queried
    /// dimensions is effectively regular — it takes the standard aggregation
    /// path (no `ROW_NUMBER` CTE), so it MUST get the standard fan-trap check.
    /// Previously any non-empty `non_additive_by` skipped the check
    /// unconditionally, letting this query silently inflate.
    #[test]
    fn test_check_fan_traps_semi_additive_regular_path_checked() {
        // Fan-out topology: metric on orders, dim on line_items reached by
        // reversing a ManyToOne edge. The metric's only NA dim (item_name)
        // IS queried, so the metric acts as a regular aggregate.
        let def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("line_items", "line_items", &["id"])
            .with_dimension("item_name", "name", Some("line_items"))
            .with_metric("total_sourced", "sum(amount)", Some("orders"))
            .with_non_additive_by(
                "total_sourced",
                &[("item_name", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            matches!(result, Err(ExpandError::FanTrap { .. })),
            "Effectively-regular semi-additive metric must get the standard \
             fan-trap check, got: {result:?}"
        );
    }

    /// EXP-3 (code-review 2026-07-18): an ACTIVE semi-additive metric queried
    /// with a dimension on a fanning child table must be REJECTED, not silently
    /// double-counted. The snapshot ROW_NUMBER CTE runs over the already-fanned
    /// `orders x line_items` join, and RANK ties across the fanned duplicates of
    /// one source row are indistinguishable from ties across distinct fact rows,
    /// so the CTE structurally cannot dedupe them. The prior behaviour SKIPPED
    /// the fan-trap check for active semi-additive metrics on the (unproven)
    /// assumption that the CTE neutralizes fan-out; this test previously asserted
    /// `is_ok()`. The metric now gets the standard met x dim check like any
    /// other, so the fanning dimension errors.
    #[test]
    fn test_check_fan_traps_semi_additive_active_fanning_dim_errors() {
        let def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("line_items", "line_items", &["id"])
            .with_dimension("item_name", "name", Some("line_items"))
            .with_dimension("report_date", "report_date", Some("orders"))
            .with_metric("total_sourced", "sum(amount)", Some("orders"))
            .with_non_additive_by(
                "total_sourced",
                &[("report_date", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        // Query only item_name: report_date (the NA dim) is NOT queried, so
        // the metric is ACTIVE semi-additive and takes the CTE path — and the
        // item_name dimension is on the fanning line_items child.
        let resolved_dims: Vec<&_> = def
            .dimensions
            .iter()
            .filter(|d| d.name == "item_name")
            .collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        match result {
            Err(ExpandError::FanTrap { detail }) => {
                assert_eq!(detail.metric_name, "total_sourced");
                assert_eq!(detail.dimension_name, "item_name");
            }
            other => panic!(
                "Active semi-additive metric with a fanning dimension must error (EXP-3), \
                 got: {other:?}"
            ),
        }
    }

    /// EXP-3 guard against over-rejection: an ACTIVE semi-additive metric on the
    /// base table, queried with a dimension in the SAFE (root-ward, many-to-one)
    /// direction, must still be ALLOWED. Removing the blanket skip must not turn
    /// legitimate snapshot queries into errors — only genuinely fanning ones.
    #[test]
    fn test_check_fan_traps_semi_additive_active_safe_dim_ok() {
        // root = orders; orders --(customer_id)--> customers is ManyToOne, so a
        // dim on customers is reached in the safe direction (no fan-out of the
        // orders-grain metric).
        let def = minimal_def("orders", "cust_name", "name", "total", "sum(amount)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "c.name", Some("customers"))
            .with_dimension("report_date", "report_date", Some("orders"))
            .with_metric("total_sourced", "sum(amount)", Some("orders"))
            .with_non_additive_by(
                "total_sourced",
                &[("report_date", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        // Query only cust_name: report_date (the NA dim) is NOT queried, so the
        // metric is ACTIVE semi-additive; cust_name is on the parent side.
        let resolved_dims: Vec<&_> = def
            .dimensions
            .iter()
            .filter(|d| d.name == "cust_name")
            .collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            result.is_ok(),
            "Active semi-additive metric with a safe-direction dimension must be allowed, \
             got: {result:?}"
        );
    }

    /// SG-6: window metrics get the standard fan-trap check — the inner
    /// aggregate is computed over the already-fanned join, so it is inflated
    /// before the window function runs. Previously `is_window()` skipped the
    /// check unconditionally.
    #[test]
    fn test_check_fan_traps_window_metric_fanning_dim_errors() {
        // Metric (inner aggregate) on orders (root), dim on a fanning child.
        let def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("line_items", "line_items", &["id"])
            .with_dimension("item_name", "name", Some("line_items"))
            .with_metric("total_sourced", "sum(amount)", Some("orders"))
            .with_window_spec(
                "total_sourced",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_sourced".to_string(),
                    ..Default::default()
                },
            )
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            matches!(result, Err(ExpandError::FanTrap { .. })),
            "Window metrics must get the standard fan-trap check, got: {result:?}"
        );
    }

    /// Window metric in the SAFE direction (metric on the FK/child side, dim
    /// on the referenced parent) stays allowed — guards against over-blocking
    /// after the SG-6 skip removal.
    #[test]
    fn test_check_fan_traps_window_metric_safe_direction_ok() {
        let def = minimal_def("orders", "cust_name", "name", "total", "sum(amount)")
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "name", Some("customers"))
            .with_metric("total_sourced", "sum(amount)", Some("orders"))
            .with_window_spec(
                "total_sourced",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_sourced".to_string(),
                    ..Default::default()
                },
            )
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        let mut def = def;
        def.dimensions.retain(|d| d.source_table.is_some());
        def.metrics.retain(|m| m.source_table.is_some());
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            result.is_ok(),
            "Window metric over a forward ManyToOne edge is safe, got: {result:?}"
        );
    }

    /// SG-1: two metrics at different grains, no dimensions. `order_total`
    /// aggregates the root table; `item_count` aggregates a `ManyToOne` child.
    /// Joining both source tables multiplies the root's rows per child row,
    /// silently inflating `order_total` — must error naming both metrics.
    #[test]
    fn test_check_fan_traps_metric_metric_multi_grain_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_metric("order_total", "SUM(o.amount)", Some("o"))
            .with_metric("item_count", "COUNT(*)", Some("li"))
            .with_pkfk_join("items_to_orders", "li", "o", &["order_id"], &["id"]);
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &[], &resolved_mets, false);
        match result {
            Err(ExpandError::MetricFanTrap { detail }) => {
                assert_eq!(detail.metric_name, "order_total", "inflated metric");
                assert_eq!(detail.metric_table, "o");
                assert_eq!(detail.other_metric_name, "item_count");
                assert_eq!(detail.other_metric_table, "li");
                assert_eq!(detail.relationship_name, "items_to_orders");
            }
            other => panic!("Expected MetricFanTrap, got: {other:?}"),
        }
    }

    /// SG-1 (chasm trap): metrics on two different child tables of one root.
    /// FROM root LEFT JOIN `child_a` LEFT JOIN `child_b` builds a cross product
    /// per root row — both aggregates inflate. Must error.
    #[test]
    fn test_check_fan_traps_metric_metric_chasm_trap_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_table("pay", "payments", &["id"])
            .with_metric("item_total", "SUM(li.amount)", Some("li"))
            .with_metric("payment_total", "SUM(pay.amount)", Some("pay"))
            .with_pkfk_join("items_to_orders", "li", "o", &["order_id"], &["id"])
            .with_pkfk_join("payments_to_orders", "pay", "o", &["order_id"], &["id"]);
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &[], &resolved_mets, false);
        match result {
            Err(ExpandError::MetricFanTrap { detail }) => {
                assert_eq!(detail.metric_name, "item_total");
                assert_eq!(detail.other_metric_name, "payment_total");
            }
            other => panic!("Expected MetricFanTrap for chasm trap, got: {other:?}"),
        }
    }

    /// SG-1 guard: same-grain multi-metric queries stay allowed, including a
    /// base-table metric with no `source_table` (which resolves to the root
    /// grain) and root-ward dimension joins (`ManyToOne` toward a parent never
    /// fans).
    #[test]
    fn test_check_fan_traps_metric_metric_same_grain_allowed() {
        let def = minimal_def("orders", "cust_name", "name", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "name", Some("customers"))
            .with_metric("total", "sum(amount)", Some("orders"))
            .with_metric("cnt", "count(*)", None) // base metric -> root grain
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        assert!(
            result.is_ok(),
            "Same-grain metrics with a root-ward dim join must be allowed, got: {result:?}"
        );
    }

    /// SG-1: an unqualified base-table metric (`source_table` = None) sits at
    /// the ROOT grain and participates in the metric×metric check — it is
    /// exactly the metric inflated by joining a `ManyToOne` child.
    #[test]
    fn test_check_fan_traps_metric_metric_base_metric_participates() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_metric("order_total", "SUM(amount)", None) // base -> root grain "o"
            .with_metric("item_count", "COUNT(*)", Some("li"))
            .with_pkfk_join("items_to_orders", "li", "o", &["order_id"], &["id"]);
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &[], &resolved_mets, false);
        match result {
            Err(ExpandError::MetricFanTrap { detail }) => {
                assert_eq!(detail.metric_name, "order_total");
                assert_eq!(
                    detail.metric_table, "o",
                    "base metric resolves to the root grain"
                );
                assert_eq!(detail.other_metric_name, "item_count");
            }
            other => panic!("Expected MetricFanTrap, got: {other:?}"),
        }
    }

    /// EXP-1 (code-review 2026-07-18): a metric on a PARENT/ancestor of the
    /// root table is aggregated at the root grain (the query is anchored
    /// `FROM <root>`), so its rows are duplicated once per root row and the
    /// aggregate is silently inflated. Here `orders` (root) references
    /// `customers`, and the metric is on `customers`; the dimension is on the
    /// same parent table, so the met x dim loop skips it (same table) — nothing
    /// else fires. It must now error `RootGrainFanTrap`.
    #[test]
    fn test_check_fan_traps_parent_metric_root_grain_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("c", "customers", &["id"])
            .with_dimension("segment", "c.segment", Some("c"))
            .with_metric("total_balance", "SUM(c.balance)", Some("c"))
            .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        match check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false) {
            Err(ExpandError::RootGrainFanTrap {
                view_name,
                metric_name,
                metric_table,
                relationship_name,
            }) => {
                assert_eq!(view_name, "test");
                assert_eq!(metric_name, "total_balance");
                assert_eq!(metric_table, "c");
                assert_eq!(relationship_name, "o_to_c");
            }
            other => panic!("Expected RootGrainFanTrap, got: {other:?}"),
        }
    }

    /// EXP-1: the same parent-table metric queried ALONE (no dimensions at all)
    /// still errors — the pairwise met x dim / met x met checks have nothing to
    /// pair, so only the root-grain check catches it.
    #[test]
    fn test_check_fan_traps_parent_metric_alone_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("c", "customers", &["id"])
            .with_metric("total_balance", "SUM(c.balance)", Some("c"))
            .with_pkfk_join("o_to_c", "o", "c", &["customer_id"], &["id"]);
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        assert!(
            matches!(
                check_fan_traps("test", &def, &[], &resolved_mets, false),
                Err(ExpandError::RootGrainFanTrap { .. })
            ),
            "A parent-table metric queried alone must error (EXP-1)"
        );
    }

    /// EXP-1: a metric on the ROOT table (or a child/FK-side descendant) is at
    /// or below the root grain and is NOT inflated by the root-anchored FROM —
    /// it must stay allowed. Guards the root-grain check against over-rejection.
    #[test]
    fn test_check_fan_traps_root_and_child_metrics_allowed() {
        // root = orders; line_items --(order_id)--> orders (ManyToOne child).
        let def = minimal_def("orders", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("line_items", "line_items", &["id"])
            .with_metric("order_total", "SUM(orders.amount)", Some("orders")) // root grain
            .with_metric("item_total", "SUM(line_items.amount)", Some("line_items")) // child grain
            .with_pkfk_join(
                "items_to_orders",
                "line_items",
                "orders",
                &["order_id"],
                &["id"],
            );
        // Query each metric alone: neither is fanned by the root-anchored FROM
        // (a child metric's rows appear once; the root metric is the anchor).
        for name in ["order_total", "item_total"] {
            let mets: Vec<&_> = def.metrics.iter().filter(|m| m.name == name).collect();
            let result = check_fan_traps("test", &def, &[], &mets, false);
            assert!(
                result.is_ok(),
                "Metric '{name}' at/below root grain must be allowed, got: {result:?}"
            );
        }
    }

    /// EXP-8 (code-review 2026-07-18): a base-table metric with `source_table ==
    /// None` and no base-metric references has an EMPTY grain set. The met x dim
    /// loop must map that to the root grain (as the met x met loop already
    /// does), or a root-grain base metric queried with a fanning child
    /// dimension slips through the fence and silently inflates.
    #[test]
    fn test_check_fan_traps_empty_grain_base_metric_fanning_dim_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_dimension("item_name", "li.name", Some("li"))
            // Base metric, source_table = None -> root grain, empty grain set.
            .with_metric("total", "SUM(o.amount)", None)
            .with_pkfk_join("li_o", "li", "o", &["order_id"], &["id"]);
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        match check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false) {
            Err(ExpandError::FanTrap { detail }) => {
                assert_eq!(detail.metric_name, "total");
                assert_eq!(detail.dimension_name, "item_name");
            }
            other => panic!(
                "Expected FanTrap for an empty-grain base metric with a fanning dim (EXP-8), \
                 got: {other:?}"
            ),
        }
    }

    /// EXP-2 (code-review 2026-07-18): a SINGLE derived metric whose transitive
    /// grain spans two tables across a fan-out edge bypasses the ordered
    /// met x met pair loop (which skips `i == j`). Here `ratio = order_total /
    /// item_count` mixes a root-grain metric (`order_total` on `orders`) with a
    /// child-grain metric (`item_count` on `line_items`); querying `ratio` alone
    /// inflates the numerator over the fanned join. It must error `MetricFanTrap`
    /// naming the derived metric.
    #[test]
    fn test_check_fan_traps_single_derived_multi_grain_errors() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_metric("order_total", "SUM(o.amount)", Some("o"))
            .with_metric("item_count", "COUNT(*)", Some("li"))
            .with_metric("ratio", "order_total / item_count", None) // derived, grain {o, li}
            .with_pkfk_join("li_o", "li", "o", &["order_id"], &["id"]);
        let ratio: Vec<&_> = def.metrics.iter().filter(|m| m.name == "ratio").collect();
        match check_fan_traps("test", &def, &[], &ratio, false) {
            Err(ExpandError::MetricFanTrap { detail }) => {
                assert_eq!(detail.metric_name, "ratio");
                assert_eq!(detail.relationship_name, "li_o");
            }
            other => panic!(
                "Expected MetricFanTrap for single multi-grain derived metric, got: {other:?}"
            ),
        }
    }

    /// EXP-2 guard: a single derived metric whose grain is a single table (its
    /// two base metrics live on the SAME table) is not multi-grain and must stay
    /// allowed.
    #[test]
    fn test_check_fan_traps_single_derived_single_grain_allowed() {
        let def = minimal_def("o", "d", "d", "m", "count(*)")
            .clear_dimensions()
            .clear_metrics()
            .with_table("li", "line_items", &["id"])
            .with_metric("gross", "SUM(o.amount)", Some("o"))
            .with_metric("net", "SUM(o.amount) - SUM(o.discount)", Some("o"))
            .with_metric("ratio", "gross / net", None) // derived, grain {o}
            .with_pkfk_join("li_o", "li", "o", &["order_id"], &["id"]);
        let ratio: Vec<&_> = def.metrics.iter().filter(|m| m.name == "ratio").collect();
        assert!(
            check_fan_traps("test", &def, &[], &ratio, false).is_ok(),
            "A single-grain derived metric must be allowed"
        );
    }

    /// SG-7: a definition whose relationship graph cannot be rebuilt (here: a
    /// self-referencing join) must ERROR, not silently skip the safety check.
    #[test]
    fn test_check_fan_traps_unbuildable_graph_errors() {
        let def = minimal_def("orders", "region", "region", "total", "sum(amount)").with_pkfk_join(
            "self_ref",
            "orders",
            "orders",
            &["parent_id"],
            &["id"],
        );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
        match result {
            Err(ExpandError::UncheckableDefinition { view_name, reason }) => {
                assert_eq!(view_name, "test");
                assert!(
                    reason.contains("cannot reference itself"),
                    "reason should carry the graph error: {reason}"
                );
            }
            other => panic!("Expected UncheckableDefinition, got: {other:?}"),
        }
        // Same for the fact-path validator (only reached with >= 2 tables).
        let result = validate_fact_table_path("test", &def, &["a".to_string()], &["b".to_string()]);
        assert!(
            matches!(result, Err(ExpandError::UncheckableDefinition { .. })),
            "validate_fact_table_path must also error on an unbuildable graph, got: {result:?}"
        );
    }

    /// SG-7 residual (AR-4): a legacy join lacking `fk_columns` builds an empty
    /// graph *successfully* — the pre-fix check passed vacuously and produced
    /// mis-aggregated results. It must now ERROR instead.
    #[test]
    fn test_check_fan_traps_incomplete_relationships_error() {
        let mut def = minimal_def("orders", "region", "region", "total", "sum(amount)");
        def.joins.push(crate::model::Join {
            table: "customers".to_string(),
            from_alias: "orders".to_string(),
            fk_columns: vec![], // legacy: FK metadata never captured
            ..Default::default()
        });
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        match check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false) {
            Err(ExpandError::UncheckableDefinition { view_name, reason }) => {
                assert_eq!(view_name, "test");
                assert!(
                    reason.contains("foreign-key column metadata"),
                    "reason should name the missing FK metadata: {reason}"
                );
            }
            other => panic!("Expected UncheckableDefinition, got: {other:?}"),
        }
    }

    /// EXP-15 (code review 2026-08-04): cycle detection is a CREATE-time check
    /// (`graph::validate_graph` runs `toposort`), and the query-time fence never
    /// repeated it — `RelationshipGraph::from_definition` rejects only
    /// self-references. A stored two-cycle therefore reached
    /// `fanning_edge_on_path`, which tests the FORWARD key `(a, b)` first and
    /// declares the hop safe on finding it, never looking at the reverse
    /// `ManyToOne` edge `(b, a)` that fans. The pair passed the fence.
    ///
    /// Two mutually-referencing `ManyToOne` joins are not a topology with a
    /// right answer — they assert "many a per b" and "many b per a" at once —
    /// so the fence must decline to certify rather than pick a direction.
    #[test]
    fn test_check_fan_traps_cyclic_definition_errors() {
        // o -> c AND c -> o, both ManyToOne (the `with_pkfk_join` default).
        let def = minimal_def("o", "region", "region", "total", "sum(o.amount)")
            .clear_dimensions()
            .with_table("c", "customers", &["id"])
            .with_dimension("region", "c.region", Some("c"))
            .with_pkfk_join("o_c", "o", "c", &["customer_id"], &["id"])
            .with_pkfk_join("c_o", "c", "o", &["primary_order_id"], &["id"]);
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        match check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false) {
            Err(ExpandError::UncheckableDefinition { view_name, reason }) => {
                assert_eq!(view_name, "test");
                assert!(
                    reason.contains("cycle"),
                    "reason should name the cycle: {reason}"
                );
            }
            other => panic!("Expected UncheckableDefinition, got: {other:?}"),
        }
    }

    /// EXP-15, second entry point: the fact-path validator builds the same graph
    /// and must decline the same definition. Separate test so the two report
    /// independently rather than the first failure masking the second.
    #[test]
    fn test_validate_fact_table_path_cyclic_definition_errors() {
        let def = minimal_def("o", "region", "region", "total", "sum(o.amount)")
            .with_table("c", "customers", &["id"])
            .with_pkfk_join("o_c", "o", "c", &["customer_id"], &["id"])
            .with_pkfk_join("c_o", "c", "o", &["primary_order_id"], &["id"]);
        let result = validate_fact_table_path("test", &def, &["o".to_string()], &["c".to_string()]);
        match result {
            Err(ExpandError::UncheckableDefinition { reason, .. }) => {
                assert!(
                    reason.contains("cycle"),
                    "reason should name the cycle: {reason}"
                );
            }
            other => panic!("Expected UncheckableDefinition, got: {other:?}"),
        }
    }

    /// EXP-15 control: an ordinary ACYCLIC two-table definition must still be
    /// certified. Guards the cycle check against over-rejection — a check that
    /// refuses everything would make the two tests above pass vacuously.
    #[test]
    fn test_check_fan_traps_acyclic_definition_still_allowed() {
        let def = minimal_def("o", "region", "region", "total", "sum(o.amount)")
            .clear_dimensions()
            .with_table("c", "customers", &["id"])
            .with_dimension("region", "c.region", Some("c"))
            .with_pkfk_join("o_c", "o", "c", &["customer_id"], &["id"]);
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        assert!(
            check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false).is_ok(),
            "an acyclic o -> c definition must remain checkable"
        );
    }

    /// SG-16: two named relationships between the same table pair with
    /// differing cardinalities (role-playing). The fan-out must be detected
    /// regardless of declaration order, and the error must name the
    /// relationship that actually fans (the `ManyToOne` one).
    #[test]
    fn test_check_fan_traps_role_playing_worst_case_cardinality() {
        for m2o_first in [true, false] {
            let mut def = minimal_def("orders", "item_name", "name", "total", "sum(amount)")
                .with_table("orders", "orders", &["id"])
                .with_table("line_items", "line_items", &["id"])
                .with_dimension("item_name", "name", Some("line_items"))
                .with_metric("total", "sum(amount)", Some("orders"))
                .with_pkfk_join("rel_m2o", "line_items", "orders", &["order_id"], &["id"])
                .with_pkfk_join("rel_o2o", "line_items", "orders", &["id"], &["id"]);
            def.dimensions.retain(|d| d.source_table.is_some());
            def.metrics.retain(|m| m.source_table.is_some());
            let n = def.joins.len();
            def.joins[n - 1].cardinality = Cardinality::OneToOne;
            if !m2o_first {
                def.joins.swap(n - 2, n - 1);
            }
            let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
            let resolved_mets: Vec<&_> = def.metrics.iter().collect();
            let result = check_fan_traps("test", &def, &resolved_dims, &resolved_mets, false);
            match result {
                Err(ExpandError::FanTrap { detail }) => {
                    assert_eq!(
                        detail.relationship_name, "rel_m2o",
                        "error must name the fanning relationship (m2o_first={m2o_first})"
                    );
                }
                other => panic!(
                    "Fan-out must be detected regardless of declaration order \
                     (m2o_first={m2o_first}), got: {other:?}"
                ),
            }
        }
    }

    #[test]
    fn test_validate_fact_table_path_single_table_ok() {
        let def = orders_view().with_table("orders", "orders", &["id"]);
        let fact_tables = vec!["orders".to_string()];
        let dim_tables = vec!["orders".to_string()];
        let result = validate_fact_table_path("test", &def, &fact_tables, &dim_tables);
        assert!(result.is_ok(), "Single table should be OK");
    }

    #[test]
    fn test_validate_fact_table_path_ancestor_descendant_ok() {
        let def = orders_view()
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        let fact_tables = vec!["orders".to_string()];
        let dim_tables = vec!["customers".to_string()];
        let result = validate_fact_table_path("test", &def, &fact_tables, &dim_tables);
        assert!(
            result.is_ok(),
            "Ancestor-descendant should be OK, got: {result:?}"
        );
    }

    #[test]
    fn test_validate_fact_table_path_divergent_tables_err() {
        // orders (root) -> customers and orders -> products (siblings)
        let def = orders_view()
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_table("products", "products", &["id"])
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            )
            .with_pkfk_join(
                "orders_products",
                "orders",
                "products",
                &["product_id"],
                &["id"],
            );
        // customers and products are siblings (neither ancestor of the other)
        let fact_tables = vec!["customers".to_string()];
        let dim_tables = vec!["products".to_string()];
        let result = validate_fact_table_path("test", &def, &fact_tables, &dim_tables);
        assert!(result.is_err(), "Divergent tables should fail");
        if let Err(ExpandError::FactPathViolation {
            table_a, table_b, ..
        }) = &result
        {
            assert!(
                (table_a == "customers" && table_b == "products")
                    || (table_a == "products" && table_b == "customers"),
                "Should identify the divergent tables"
            );
        } else {
            panic!("Expected FactPathViolation, got: {result:?}");
        }
    }

    /// orders references both customers and products, so customers and
    /// products can each be reached from orders without fanning out, though
    /// not from each other.
    fn two_parents_view() -> SemanticViewDefinition {
        orders_view()
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_table("products", "products", &["id"])
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            )
            .with_pkfk_join(
                "orders_products",
                "orders",
                "products",
                &["product_id"],
                &["id"],
            )
    }

    #[test]
    fn test_validate_fact_table_path_two_parents_through_their_child_ok() {
        // A fact on orders with a dimension on each parent: one row per order,
        // each joined to one customer and one product. The pair
        // (customers, products) is not reachable either way, but orders
        // reaches both, so nothing multiplies.
        let fact_tables = vec!["orders".to_string()];
        let dim_tables = vec!["customers".to_string(), "products".to_string()];
        let result =
            validate_fact_table_path("test", &two_parents_view(), &fact_tables, &dim_tables);
        assert!(
            result.is_ok(),
            "orders reaches customers and products without fanning out: {result:?}"
        );
        // The order of the tables does not decide it.
        let dim_tables = vec![
            "products".to_string(),
            "customers".to_string(),
            "orders".to_string(),
        ];
        let result = validate_fact_table_path("test", &two_parents_view(), &[], &dim_tables);
        assert!(result.is_ok(), "same set, other order: {result:?}");
    }

    #[test]
    fn test_validate_fact_table_path_two_parents_without_their_child_err() {
        // The over-widening guard: without orders, customers and products meet
        // only through it, and each customer row would come back once per order.
        let fact_tables = vec!["customers".to_string()];
        let dim_tables = vec!["products".to_string()];
        let result =
            validate_fact_table_path("test", &two_parents_view(), &fact_tables, &dim_tables);
        assert!(
            matches!(result, Err(ExpandError::FactPathViolation { .. })),
            "customers and products alone multiply rows: {result:?}"
        );
    }

    #[test]
    fn test_validate_fact_table_path_no_joins_ok() {
        let def = orders_view();
        let fact_tables = vec!["orders".to_string()];
        let dim_tables = vec!["customers".to_string()];
        let result = validate_fact_table_path("test", &def, &fact_tables, &dim_tables);
        assert!(result.is_ok(), "No joins should be OK");
    }
}
