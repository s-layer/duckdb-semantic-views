//! Per-grain ("own-grain") metric aggregation — the Snowflake-parity path for
//! multi-grain queries (TECH-DEBT #35).
//!
//! # Why
//!
//! Every other aggregation strategy in this crate anchors the generated query
//! `FROM <base table>` and `LEFT JOIN`s outward, so every metric is computed
//! over one joined row set. A metric whose grain is *not* the base grain is
//! therefore aggregated over the multiplied join: a metric on a parent of the
//! base table is counted once per base row, and two metrics either side of a
//! fan-out edge inflate each other. v0.11.0 made those shapes **error**
//! (`RootGrainFanTrap` / `MetricFanTrap`) rather than silently inflate.
//!
//! Snowflake answers them instead, by computing each metric at its own grain.
//! This module does the same: each grain gets a CTE anchored at *its* table,
//! aggregating only that table's rows, and the pre-aggregated results are joined
//! on the queried dimensions.
//!
//! ```sql
//! WITH __sv_grain_0 AS (          -- the base-grain metrics
//!     SELECT o.status AS "__sv_d0", SUM(o.amount) AS "__sv_m0"
//!     FROM "orders" AS "o" GROUP BY 1
//! ), __sv_grain_1 AS (            -- the line-item-grain metrics
//!     SELECT o.status AS "__sv_d0", COUNT(*) AS "__sv_m0"
//!     FROM "line_items" AS "li"
//!     LEFT JOIN "orders" AS "o" ON "li"."order_id" = "o"."id" GROUP BY 1
//! )
//! SELECT COALESCE("__sv_grain_0"."__sv_d0", "__sv_grain_1"."__sv_d0") AS "status",
//!        "__sv_grain_0"."__sv_m0" AS "order_total",
//!        "__sv_grain_1"."__sv_m0" AS "item_count"
//! FROM __sv_grain_0
//! FULL OUTER JOIN __sv_grain_1
//!   ON "__sv_grain_0"."__sv_d0" IS NOT DISTINCT FROM "__sv_grain_1"."__sv_d0"
//! ```
//!
//! # Scope
//!
//! [`plan`] returns `None` — leaving the base-anchored path and its fan-trap
//! fence exactly as they were — unless the query BOTH needs per-grain treatment
//! (a base-anchored `FROM` would inflate it) AND is eligible for it. So
//! single-grain SQL is byte-identical to v0.11.0, and the shapes per-grain
//! cannot define keep their v0.11.0 error:
//!
//! - a dimension **below** a metric's grain (the metric's rows genuinely fan
//!   across the dimension's values) — `FanTrap`, raised by the fence;
//! - a query that reaches a **role-played** table without saying which role it
//!   means. Which of the several relationship instances a grain CTE should join
//!   is what `USING` answers, and a co-queried metric's `USING` is now honoured:
//!   the grain CTEs join the NAMED edge under its scoped alias (`a__dep`) and
//!   emit the dimension against it, matching the base-anchored path.
//!
//!   Without that context the query is still declined, and the rescue is
//!   deliberately narrow — it covers a queried DIMENSION's own table only. A
//!   `where_clause` member on a role-played table, a metric's own grain table, or
//!   a table reachable only *through* a role-played one are all still declined:
//!   only a dimension's expression is rewritten to the scoped alias, so nothing
//!   else can say which role it means without guessing.
//!
//!   Note the scope: this is asked of the QUERY, not the definition, so a
//!   definition that declares role-playing somewhere does not lose per-grain
//!   emission for queries that never reach it.
//!
//! Window metrics are no longer in that list. [`window_cte_anchor`] picks the
//! grain for an all-window query and `expand_window_metrics` anchors its
//! `__sv_agg` CTE there — the window function is not grain-sensitive, so moving
//! the CTE is the whole fix. That path does not build [`Plan`] groups; it reuses
//! only [`anchor_joins`] and the fence's per-grain mode.
//!
//! *Active* semi-additive metrics are no longer in that list either. A single
//! grain re-points `__sv_snapshot` at the metric's own table via
//! [`snapshot_cte_anchor`] — the move [`window_cte_anchor`] makes for `__sv_agg`
//! — and is left to the base-anchored emitter. A query spanning several grains
//! emits the snapshot as ONE GROUP's CTE, nested as `SELECT <outer> FROM
//! (<inner>)` so its rows are ranked before that grain aggregates them, while
//! sibling groups aggregate normally (see [`render_snapshot_group`]).

use std::collections::{HashMap, HashSet};

use crate::model::{Dimension, Join, Metric, SemanticViewDefinition};

use super::fan_trap::{metric_grain, GrainGraph};
use super::join_resolver::{push_join_clauses, ResolvedJoin};
use super::resolution::{quote_ident, quote_stored_ident};
use super::select_spec::{
    push_from_anchor, push_group_by_ordinals, FromSource, GroupBy, SelectItem, SelectSpec,
};
use super::where_clause::ResolvedWhere;

/// The CTE name for grain group `i`. Bare (unquoted) by construction — the
/// index is the only variable part — matching `__sv_agg` / `__sv_snapshot`.
fn grain_cte(index: usize) -> String {
    format!("__sv_grain_{index}")
}

/// Reference to grain group `group`'s column `column`, qualified by its CTE.
fn column_ref(group: usize, column: &str) -> String {
    format!("{}.{}", quote_ident(&grain_cte(group)), quote_ident(column))
}

/// The internal alias of the `i`th queried dimension inside every grain CTE.
/// Positional rather than the dimension's own name so a declared name can never
/// collide with the join keys (or with a metric column) inside the CTE.
fn dim_column(index: usize) -> String {
    format!("__sv_d{index}")
}

/// The internal alias of the `i`th aggregate column within one grain CTE.
fn metric_column(index: usize) -> String {
    format!("__sv_m{index}")
}

/// What a FROM anchored anywhere but the base table must join for the fact
/// expressions its metrics and dimensions inline to bind (PAR-6), and for the
/// `where_clause` members' tables: the bare tables to walk out to, and the
/// `USING`-selected relationship instances to join under their scoped aliases.
pub(super) struct AnchorNeeds<'a> {
    /// Tables joined under their own alias, each once, in first-reached order.
    pub(super) bare: Vec<String>,
    /// Relationships joined under `<table>__<rel>` (TECH-DEBT #69 / #71).
    pub(super) scoped: Vec<&'a Join>,
}

/// Compute [`AnchorNeeds`] for `metrics` (the queried ones; every metric each
/// reaches is walked) and `resolved_dims`, plus the `where_clause` tables.
///
/// The one place every anchored emitter decides bare-or-scoped — the grain
/// CTEs, the window `__sv_agg` CTE and the semi-additive snapshot (reviewbot,
/// #250: each had grown its own copy, and they disagreed). The decision is
/// made per base metric and per fact table exactly as
/// `facts::rewrite_using_scoped_refs` makes it when it requalifies that
/// metric's inlined text: a table is scoped only when exactly one distinct
/// relationship of that metric's `USING` targets it and it is not the metric's
/// own table; otherwise the text still names the bare alias, so the bare table
/// is joined. One metric's scoped instance therefore never removes the bare
/// join another metric (or a dimension) in the same FROM still names.
///
/// The planner has already established the anchor is not fanned by the paths it
/// emits, and `check_referenced_fact_fan_traps` has already rejected a fact on
/// a table that fans its member — so nothing joined here multiplies the anchor.
pub(super) fn anchor_needs<'a>(
    def: &'a SemanticViewDefinition,
    metrics: &[&Metric],
    resolved_dims: &[&Dimension],
    where_tables: &[String],
) -> AnchorNeeds<'a> {
    let mut needs = AnchorNeeds {
        bare: Vec::new(),
        scoped: Vec::new(),
    };
    let push_bare = |t: &str, bare: &mut Vec<String>| {
        if !bare.iter().any(|b| b.eq_ignore_ascii_case(t)) {
            bare.push(t.to_string());
        }
    };
    for t in where_tables {
        push_bare(t, &mut needs.bare);
    }
    // Every metric each one reaches, not just its own expression: a derived
    // metric's expression names METRICS and a window metric's names its inner
    // metric, so the fact references live in the metrics inlined into them.
    // Scanning only `met.expr` reintroduced PAR-6 inside the grain CTE for a
    // derived metric over a cross-table fact (Copilot, #200).
    let mut reached: Vec<&Metric> = Vec::new();
    for met in metrics {
        for m in metrics_reached(def, met) {
            if !reached.iter().any(|r| std::ptr::eq(*r, m)) {
                reached.push(m);
            }
        }
    }
    for m in reached {
        let roles = using_roles_by_target(def, m);
        for t in super::facts::collect_referenced_fact_tables(&m.expr, &def.facts) {
            let key = crate::ident::normalize_ident_part(&t);
            match roles.iter().find(|(target, _)| *target == key) {
                Some((_, joins)) if joins.len() == 1 => {
                    if !needs.scoped.iter().any(|j| std::ptr::eq(*j, joins[0])) {
                        needs.scoped.push(joins[0]);
                    }
                }
                _ => push_bare(&t, &mut needs.bare),
            }
        }
    }
    // TECH-DEBT #54: a grain CTE projects the queried dimension expressions as
    // its group keys, and those are fact-inlined too -- so a fact a dimension
    // reaches on another table has to be joined inside this CTE (Copilot,
    // #200). A dimension's expression is never `USING`-scoped, so always bare.
    for dim in resolved_dims {
        for t in super::facts::collect_referenced_fact_tables(&dim.expr, &def.facts) {
            push_bare(&t, &mut needs.bare);
        }
    }
    needs
}

/// A base metric's `USING` relationships grouped by the (normalized) table they
/// target, distinct by edge and skipping the metric's own table — the grouping
/// `facts::rewrite_using_scoped_refs` rewrites by. Empty for a derived metric,
/// whose `USING` (if any) that rewrite never applies.
fn using_roles_by_target<'a>(
    def: &'a SemanticViewDefinition,
    met: &Metric,
) -> Vec<(String, Vec<&'a Join>)> {
    let mut by_target: Vec<(String, Vec<&'a Join>)> = Vec::new();
    let Some(own) = met.source_table.as_deref() else {
        return by_target;
    };
    let own = crate::ident::normalize_ident_part(own);
    for rel in &met.using_relationships {
        let Some(join) = crate::graph::find_relationship(&def.joins, rel) else {
            continue;
        };
        let target = crate::ident::normalize_ident_part(&join.table);
        if target == own {
            continue;
        }
        match by_target.iter_mut().find(|(t, _)| *t == target) {
            Some((_, joins)) => {
                if !joins.iter().any(|j| std::ptr::eq(*j, join)) {
                    joins.push(join);
                }
            }
            None => by_target.push((target, vec![join])),
        }
    }
    by_target
}

/// The metrics a grain group computes, resolved from its canonical names.
fn group_metrics<'a>(def: &'a SemanticViewDefinition, group: &Group) -> Vec<&'a Metric> {
    group
        .metric_names
        .iter()
        .filter_map(|name| {
            def.metrics
                .iter()
                .find(|m| crate::ident::normalize_ident_part(&m.name) == *name)
        })
        .collect()
}

/// One grain group: the table its CTE is anchored at, the role each
/// role-played dimension table plays for its metrics, and the aggregate columns
/// computed there.
///
/// Metrics at one grain that reach a role-played dimension table through
/// DIFFERENT relationships (`departures USING (dep)`, `arrivals USING (arr)`)
/// are separate groups: each groups the dimension through its own instance, and
/// the CTEs are outer-merged on the dimension values, as Snowflake merges
/// per-metric results (Phase 6.3).
struct Group {
    /// Lowercased table alias the CTE is anchored `FROM`.
    anchor: String,
    /// Lowercased role-played dimension table -> the relationship this group's
    /// metrics reach it by, as declared. Empty when no queried dimension is on a
    /// role-played table.
    roles: HashMap<String, String>,
    /// Aggregate expressions, in emission order. The `i`th is aliased
    /// [`metric_column(i)`](metric_column).
    exprs: Vec<String>,
    /// Canonical names of the metrics contributing each expression, parallel to
    /// `exprs`. A snapshot group needs the metrics themselves — `USING`,
    /// `NON ADDITIVE BY`, and the aggregate to decompose — not the already-built
    /// expression, and storing names rather than `&Metric` keeps [`Plan`] free
    /// of a lifetime.
    metric_names: Vec<String>,
}

/// How one requested metric's output column is produced from the grain CTEs.
enum Output {
    /// The whole metric is one grain column.
    Direct { group: usize, column: usize },
    /// A derived metric assembled from component columns spanning grains: its
    /// expression with every base-metric reference already replaced by the
    /// component's qualified CTE column reference.
    Derived { expr: String },
}

/// A per-grain execution plan: the grain groups to emit as CTEs, and how each
/// requested metric's output column is built from them.
pub(super) struct Plan {
    groups: Vec<Group>,
    /// One entry per requested metric, in request order.
    outputs: Vec<Output>,
}

impl Plan {
    /// Whether the plan is a single grain with no cross-grain assembly, in which
    /// case the query is emitted flat (anchored at that grain) instead of as a
    /// one-CTE wrapper.
    fn is_single_group(&self) -> bool {
        self.groups.len() == 1
    }
}

/// Working state while partitioning a query's metrics into grain groups.
struct Partition {
    groups: Vec<Group>,
    /// [`group_key`] of a group's anchor and roles -> index into `groups`.
    index: HashMap<String, usize>,
}

/// The key one grain group is found by: its anchor plus the scoped alias of each
/// role it plays, sorted. Built from the scoped alias rather than the declared
/// relationship name because `scoped_join_alias` normalizes the spelling, so
/// `dep` and `"Dep"` key one group.
fn group_key(anchor: &str, roles: &HashMap<String, String>) -> String {
    let mut scoped: Vec<String> = roles
        .iter()
        .map(|(table, rel)| super::join_resolver::scoped_join_alias(table, rel))
        .collect();
    scoped.sort();
    let mut key = anchor.to_string();
    for alias in scoped {
        key.push('\0');
        key.push_str(&alias);
    }
    key
}

impl Partition {
    fn new() -> Self {
        Self {
            groups: Vec::new(),
            index: HashMap::new(),
        }
    }

    /// Add `expr` to the group for `anchor` and `roles` (creating it on first
    /// use) and return the (group, column) coordinates of the added column.
    fn push(
        &mut self,
        anchor: &str,
        roles: &HashMap<String, String>,
        expr: String,
        metric_name: String,
    ) -> (usize, usize) {
        let group = *self
            .index
            .entry(group_key(anchor, roles))
            .or_insert_with(|| {
                self.groups.push(Group {
                    anchor: anchor.to_string(),
                    roles: roles.clone(),
                    exprs: Vec::new(),
                    metric_names: Vec::new(),
                });
                self.groups.len() - 1
            });
        self.groups[group].exprs.push(expr);
        self.groups[group].metric_names.push(metric_name);
        (group, self.groups[group].exprs.len() - 1)
    }
}

/// Decide whether this query must be computed per-grain, and how.
///
/// Returns `None` — meaning "leave the query on the base-anchored path, with the
/// full fan-trap fence" — when the query does not need per-grain treatment, or
/// when it needs it but is not eligible (see the module docs). A `Some` plan is
/// a commitment: the caller emits it via [`expand_per_grain`] and runs the fence
/// in its per-grain mode.
pub(super) fn plan(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    where_tables: &[String],
    where_has_unqualified_member: bool,
) -> Option<Plan> {
    if def.joins.is_empty() || resolved_mets.is_empty() {
        return None; // Single-table view, or a dimensions-only query.
    }
    if !is_eligible(
        def,
        resolved_dims,
        resolved_mets,
        where_tables,
        where_has_unqualified_member,
    ) {
        return None;
    }
    let graph = GrainGraph::build(def)?;
    let root = graph.root().to_string();

    // Every queried dimension's table, in request order. Eligibility already
    // established that each carries a source table.
    let dim_tables: Vec<String> = resolved_dims
        .iter()
        .filter_map(|d| d.source_table.as_ref().map(|t| t.to_ascii_lowercase()))
        .collect();

    // Partition the metrics into grains, decomposing a metric whose own grain
    // spans several tables into one component per referenced base metric.
    let mut partition = Partition::new();
    let mut outputs: Vec<Output> = Vec::with_capacity(resolved_mets.len());
    for met in resolved_mets {
        // `anchored` supplies the root for any component that names no table —
        // both a metric with no source table at all and (EXP-11) a source-less
        // aggregate reached through a derived metric's dependency graph. The
        // latter is why this cannot read the table list directly: it would see
        // one grain for a metric that has two, and the fast path below would
        // anchor the root component at the other one.
        let grains = metric_grain(met, def).anchored(&root);
        // A metric whose components reach a role-played dimension table through
        // different relationships has no one role, so it is decomposed like a
        // multi-grain metric: each component is grouped through its own.
        let roles = metric_roles(def, resolved_dims, met);
        match (grains.len(), roles) {
            (1, Some(roles)) => {
                let expr = resolved_exprs
                    .get(&crate::ident::normalize_ident_part(&met.name))
                    .cloned()
                    .unwrap_or_else(|| met.expr.clone());
                let (group, column) = partition.push(
                    &grains[0],
                    &roles,
                    expr,
                    crate::ident::normalize_ident_part(&met.name),
                );
                outputs.push(Output::Direct { group, column });
            }
            _ => outputs.push(decompose(
                def,
                resolved_dims,
                met,
                resolved_exprs,
                &mut partition,
            )?),
        }
    }

    // TECH-DEBT #71 (D1, settled against Snowflake): every metric is aggregated
    // from its OWN home table, so a query needs this path whenever any grain is
    // not the root. Only an all-root query is left to the base-anchored path —
    // there the base table IS every metric's home, so the two agree.
    //
    // This used to fire only where a base-anchored FROM would INFLATE a metric
    // (an anchor fanning relative to the root, or two anchors either side of a
    // fan-out edge), leaving a metric on a CHILD of the base table to the
    // base-anchored `FROM base LEFT JOIN child`. That join drops every child
    // row whose foreign key is NULL and manufactures a row for every childless
    // parent, so a child metric's number depended on whether a metric of
    // another grain happened to be co-queried (MM-3).
    //
    // Phase 6.3: an all-root query still needs this path when its metrics reach
    // a role-played dimension table through different relationships — the
    // base-anchored FROM can group the dimension through only one of them.
    let anchors: Vec<&String> = partition.groups.iter().map(|g| &g.anchor).collect();
    let one_role_set = partition
        .groups
        .iter()
        .all(|g| g.roles == partition.groups[0].roles);
    if anchors.iter().all(|a| **a == root) && one_role_set {
        return None;
    }

    // Per-grain only helps when every queried dimension is reachable from every
    // anchor without fanning it. Where it is not, the fence's metric ×
    // dimension error is the right answer — decline and let it speak.
    for anchor in &anchors {
        for dim_table in &dim_tables {
            if graph.path(anchor, dim_table).is_none()
                || graph.fanning_relationship(anchor, dim_table).is_some()
            {
                return None;
            }
        }
    }

    // A query whose ONLY group is a snapshot group belongs to the base-anchored
    // semi-additive emitter, which `snapshot_cte_anchor` already re-points at the
    // metric's own grain (TECH-DEBT #36 increment 1). Declining here keeps that
    // verified single-CTE shape instead of re-routing it through a one-group
    // plan that would emit the same thing with an extra wrapper.
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    if partition.groups.len() == 1
        && is_snapshot_group(def, &partition.groups[0], &queried_dim_keys)
    {
        return None;
    }

    Some(Plan {
        groups: partition.groups,
        outputs,
    })
}

/// Whether `group`'s metrics include an ACTIVE semi-additive one, in which case
/// its CTE is the nested snapshot shape rather than a flat aggregate.
fn is_snapshot_group(
    def: &SemanticViewDefinition,
    group: &Group,
    queried_dim_keys: &HashSet<String>,
) -> bool {
    group.metric_names.iter().any(|name| {
        def.metrics
            .iter()
            .find(|m| crate::ident::normalize_ident_part(&m.name) == *name)
            .is_some_and(|m| {
                super::semi_additive::is_active_semi_additive(def, m, queried_dim_keys)
            })
    })
}

/// Split a metric whose grain spans several tables into one component per
/// referenced base metric, each computed at its own grain, and rebuild the
/// metric's expression over those component columns.
///
/// `None` when the metric cannot be decomposed — a component that is itself
/// multi-grain, or a reference that resolves to no declared metric — in which
/// case the query stays on the base-anchored path and keeps its fence error.
fn decompose(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    met: &Metric,
    resolved_exprs: &HashMap<String, String>,
    partition: &mut Partition,
) -> Option<Output> {
    // The base metrics this one transitively depends on: each is an aggregate
    // that must be computed at its own grain before the outer expression can
    // combine them.
    //
    // Walked in DECLARATION order, filtered by the dependency set, rather than
    // iterating the set itself: the set is a `HashSet`, and its order decides
    // which grain becomes `__sv_grain_0`. Iterating it directly made the
    // generated SQL vary between runs of the same query.
    let dependencies = super::facts::collect_transitive_metric_names(met, &def.metrics);
    let mut replacements: HashMap<String, String> = HashMap::new();
    for base in &def.metrics {
        let name = crate::ident::normalize_ident_part(&base.name);
        if !dependencies.contains(&name) {
            continue;
        }
        let Some(ref source) = base.source_table else {
            // EXP-11: a source-less dependency is normally an intermediate
            // DERIVED metric — no grain of its own, inlined by the reference
            // walk below. One that AGGREGATES is not: it reads columns at the
            // root grain (EXP-8), and inlining it would splice a raw aggregate
            // into the OUTER select over the grain CTEs, where its table is not
            // in scope. It needs a component CTE of its own, which nothing here
            // can build without a table to anchor it — decline, and let the
            // base-anchored path answer the query or the full fence reject it.
            if crate::graph::contains_aggregate_function(&base.expr).is_some() {
                return None;
            }
            continue;
        };
        if base.window_spec.is_some() || !base.non_additive_by.is_empty() {
            return None; // Component strategies that are base-anchored — not eligible.
        }
        let expr = resolved_exprs.get(&name).cloned()?;
        // Each component is grouped through its OWN role: a component with no
        // one role for a queried role-played table declines the whole query.
        let roles = metric_roles(def, resolved_dims, base)?;
        // A component always has an empty `non_additive_by` (rejected above), so a
        // decomposed derived metric never lands in a snapshot group.
        let (group, column) =
            partition.push(&source.to_ascii_lowercase(), &roles, expr, name.clone());
        let reference = column_ref(group, &metric_column(column));
        // A base metric may be referenced bare (`revenue`) or qualified by its
        // own source table (`o.revenue`) — the same two spellings
        // `collect_transitive_metric_names` matches. Both keys map to the
        // component column.
        replacements.insert(
            format!("{}.{}", source.to_ascii_lowercase(), name),
            reference.clone(),
        );
        replacements.insert(name, reference);
    }
    if replacements.is_empty() {
        return None;
    }
    Some(Output::Derived {
        expr: rebuild_expr(def, met, &replacements)?,
    })
}

/// How deep a chain of derived metrics [`rebuild_expr`] will follow before
/// giving up. Cycles are already rejected by `inline_derived_metrics`, which
/// runs first — this only keeps the recursion total if a malformed definition
/// ever reaches here, and mirrors the derivation-depth cap that path enforces.
const MAX_REBUILD_DEPTH: usize = 64;

/// Rebuild `met`'s expression with each base-metric reference replaced by its
/// per-grain column reference, inlining any intermediate *derived* metric it
/// references on the way (they carry no grain of their own, so there is no
/// column to read them from).
///
/// Resolution is on demand, per reference: every occurrence of an intermediate
/// metric is expanded wherever it appears, on each branch. Inlined text is
/// never rescanned — [`crate::expr_tokens::inline_references`] splices by byte
/// span — so an intermediate must arrive already fully resolved. Remembering
/// "this one was inlined earlier" across sibling branches is exactly what must
/// not happen: the second branch would then splice the metric's *raw* text,
/// leaking base-metric names into the SQL as non-existent columns.
///
/// `None` when the chain exceeds [`MAX_REBUILD_DEPTH`], leaving the query on
/// the base-anchored path with its fence error.
fn rebuild_expr(
    def: &SemanticViewDefinition,
    met: &Metric,
    replacements: &HashMap<String, String>,
) -> Option<String> {
    fn resolve(
        def: &SemanticViewDefinition,
        met: &Metric,
        replacements: &HashMap<String, String>,
        depth: usize,
    ) -> Option<String> {
        if depth > MAX_REBUILD_DEPTH {
            return None;
        }
        let mut owned: HashMap<String, String> = HashMap::new();
        for key in crate::expr_tokens::reference_keys(&met.expr) {
            if let Some(column) = replacements.get(&key) {
                owned.insert(key, column.clone());
            } else if let Some(inner) = def.metrics.iter().find(|m| {
                m.source_table.is_none() && crate::ident::normalize_ident_part(&m.name) == key
            }) {
                let inlined = resolve(def, inner, replacements, depth + 1)?;
                owned.insert(key, format!("({inlined})"));
            }
        }
        let borrowed: HashMap<String, &str> =
            owned.iter().map(|(k, v)| (k.clone(), v.as_str())).collect();
        Some(crate::expr_tokens::inline_references(&met.expr, &borrowed))
    }

    resolve(def, met, replacements, 0)
}

/// The role `dim`'s table plays for `mets`: `Ok(None)` when the table is not
/// role-played (or `dim` names no table), `Ok(Some((table, relationship)))`
/// when the metrics' `USING` names exactly one of its relationships, and `Err`
/// when the table is role-played and they name none or several.
///
/// A role is the relationship a metric's `USING` named for a role-played
/// table, as DECLARED (its stored spelling, quotes and all). Compare a
/// value with `graph::RelNameMatch`, never `==`: a `USING` entry may spell the
/// same identifier differently (`dep` / `"Dep"`). `scoped_join_alias`
/// normalizes it, so every spelling yields the same alias.
///
/// [`find_using_context`] is the base-anchored path's answer to the same
/// question, and reusing it is what keeps the two paths agreeing on which role a
/// dimension means — it returns the scoped alias (`a__dep`), from which the
/// relationship name is the suffix after `__`. Rather than re-split that string,
/// this asks the definition directly: the relationship whose target is the
/// dimension's table and whose scoped alias matches.
///
/// Only a queried DIMENSION's table is resolved. A role-played table reached
/// only as a metric's grain, or named only by a `where_clause` member, has no
/// dimension for `USING` to scope and stays ineligible — see
/// [`role_playing_affects_query`].
fn dimension_role(
    def: &SemanticViewDefinition,
    dim: &Dimension,
    mets: &[&Metric],
) -> Result<Vec<(String, String)>, ()> {
    let Some(table) = dim.source_table.as_ref().map(|t| t.to_ascii_lowercase()) else {
        return Ok(Vec::new());
    };
    let Some(scoped) =
        super::role_playing::find_using_context("", def, dim, mets).map_err(|_| ())?
    else {
        return Ok(Vec::new());
    };
    // Phase 6.3b: a table reached only THROUGH the role-played one takes the
    // role-played table's relationship, and so does every table on the chain
    // between them, so `anchor_joins` joins the whole chain off that instance.
    // `role_playing_on_path` finds no role-played table above the view's root,
    // which can itself be role-played (reached back from two child tables).
    let rp = super::role_playing::role_playing_on_path("", def, &table)
        .map_err(|_| ())?
        .unwrap_or_else(|| table.clone());
    let rel = def.joins.iter().find_map(|j| {
        let name = j.name.as_ref()?;
        (j.table.to_ascii_lowercase() == rp
            && super::join_resolver::scoped_join_alias(&table, name) == scoped)
            .then(|| name.clone())
    });
    let rel = rel.ok_or(())?;
    let mut roles = vec![(rp.clone(), rel.clone())];
    if rp != table {
        let chain = super::role_playing::descendant_chain(def, &rp, &table).ok_or(())?;
        roles.extend(
            chain
                .iter()
                .map(|edge| (edge.table.to_ascii_lowercase(), rel.clone())),
        );
    }
    Ok(roles)
}

/// The role each queried role-played dimension table plays for ONE metric (a
/// derived metric through all its components), or `None` when the metric has no
/// single role for one of them — it names none, or its components name several.
///
/// This is what lets metrics with different `USING` paths share a query
/// (Phase 6.3): each is grouped through its own role, where the base-anchored
/// path asks for one role across every metric at once.
fn metric_roles(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    met: &Metric,
) -> Option<HashMap<String, String>> {
    let mut roles: HashMap<String, String> = HashMap::new();
    for dim in resolved_dims {
        for (table, rel) in dimension_role(def, dim, &[met]).ok()? {
            roles.insert(table, rel);
        }
    }
    Some(roles)
}

/// Whether every queried metric can be grouped through a single role per
/// queried role-played table: by itself, or (a derived metric) through each of
/// its base components separately, which is how [`plan`] then partitions it.
fn every_metric_has_roles(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
) -> bool {
    resolved_mets.iter().all(|met| {
        metric_roles(def, resolved_dims, met).is_some()
            || (met.source_table.is_none()
                && metrics_reached(def, met)
                    .iter()
                    .filter(|m| m.source_table.is_some())
                    .all(|m| metric_roles(def, resolved_dims, m).is_some()))
    })
}

/// Whether role-playing is relevant to **this query** — not merely present
/// somewhere in the definition.
///
/// Which role an anchored CTE should join is exactly the question `USING`
/// answers on the base-anchored path, and neither the per-grain grain CTEs nor
/// the anchored window CTE carries that context, so both still decline a query
/// that would have to answer it.
///
/// The question is asked per query rather than per definition. A definition-wide
/// test costs *every* query against a definition that declares role-playing
/// anywhere — including queries that never reach the role-played table, whose
/// grain CTEs join nothing ambiguous and need no role context at all. Two
/// unrelated grains lost per-grain emission because some third table happened to
/// be reachable by two relationships.
///
/// A table is ambiguous here if it *is* a role-playing target or is reachable
/// only *through* one, which is what [`role_playing_on_path`] walks. Checking
/// the tables the query touches also covers the joins between them: the
/// relationship graph is a tree apart from the sanctioned role-playing
/// multi-edge, so any node on the path between two touched tables is an ancestor
/// of one of them, and the walk from that endpoint to the root passes through it.
///
/// [`role_playing_on_path`]: super::role_playing::role_playing_on_path
fn role_playing_affects_query(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    where_tables: &[String],
    allow_using_scoped: bool,
) -> bool {
    // The view name passed to `role_playing_on_path` below only decorates an
    // error discarded here: a definition too malformed to walk declines
    // per-grain, and the base-anchored path it falls back to reports the real
    // diagnostic.
    let ambiguity = |table: &String| match super::role_playing::role_playing_on_path("", def, table)
    {
        Ok(v) => v,
        Err(_) => Some(table.clone()), // Unwalkable: treat as ambiguous, decline.
    };

    // Tables that CANNOT be rescued by `USING`, so any role-playing on their
    // path declines outright. Deduplicated: several metrics commonly share a
    // grain, and each walk can rebuild the relationship graph.
    //
    // A `where_clause` member's table is joined into every grain CTE exactly
    // like a dimension's — `anchor_joins` chains both into the sources it walks
    // — but only a DIMENSION's expression is rewritten to a scoped alias, so a
    // predicate naming a role-played member has no way to say which role it
    // means and stays ineligible. A metric's grain table is likewise strict:
    // `USING` scopes the dimension a metric is grouped BY, not the table a
    // metric is aggregated AT.
    let mut strict: HashSet<String> = where_tables
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    for met in resolved_mets {
        // Named tables only: this asks which tables the query touches, so a
        // root-grain component contributes nothing — the root is the anchor and
        // is never the role-played table whose path is ambiguous.
        strict.extend(metric_grain(met, def).named_tables().iter().cloned());
    }
    if strict.iter().any(|t| ambiguity(t).is_some()) {
        return true;
    }

    // A queried dimension's table may be rescued, but only when the ambiguous
    // table is the dimension's OWN table and a co-queried metric's `USING`
    // named its role. A table reached only THROUGH a role-played ancestor is
    // never rescued: `find_using_context` rejects that case outright, because a
    // descendant cannot be scoped by a co-queried metric's `USING`, so it
    // yields no role here either.
    //
    // Phase 6.3: the role is resolved per metric, not across the query, so
    // metrics naming DIFFERENT relationships to the table are each rescued and
    // grouped through their own ([`metric_roles`]). Every metric needs one.
    //
    // The window path emits its own `__sv_agg` SELECT list and does not rewrite
    // dimension expressions to scoped aliases, so it cannot honour a role even
    // when `USING` names one. It stays strict.
    let rescued = allow_using_scoped && every_metric_has_roles(def, resolved_dims, resolved_mets);
    let dim_tables: HashSet<String> = resolved_dims
        .iter()
        .filter_map(|d| d.source_table.as_ref().map(|s| s.to_ascii_lowercase()))
        .collect();
    dim_tables.iter().any(|table| match ambiguity(table) {
        None => false,
        Some(ambiguous) => {
            !(rescued
                && (ambiguous == *table
                    || super::role_playing::descendant_chain(def, &ambiguous, table).is_some()))
        }
    })
}

/// The declared table a window query's `__sv_agg` CTE should anchor at, or
/// `None` to leave it anchored at the base table.
///
/// A window metric's inner aggregate is the grain-sensitive part: the window
/// function itself runs over the already-grouped CTE, so anchoring the CTE at
/// the inner metric's own table is the whole fix (TECH-DEBT #36). With
/// `total_balance = SUM(c.balance)` on the parent `customers`, a base-anchored
/// CTE joins `orders` and sums each customer's balance once per order — the
/// inflation the v0.11.0 fence turned into `RootGrainFanTrap`. Anchored at `c`
/// the inner aggregate sees one row per customer.
///
/// Returns `None` — leaving the query base-anchored with the full fence — when:
/// - the anchor would be the root anyway (base-anchoring is already correct);
/// - the window metrics' inner aggregates span **several** grains, which would
///   need those grains joined before the window runs. That is the next
///   increment, not this one;
/// - a queried dimension is unqualified (its binding would move with the
///   anchor), or the view role-plays, matching [`is_eligible`]'s conservatism;
/// - a queried dimension is unreachable from the anchor.
///
/// A dimension *below* the anchor's grain is deliberately NOT screened here: it
/// has no single value per group in either engine, and the fan-trap fence's
/// metric × dimension check — which per-grain mode keeps — is what reports it.
/// The table an active semi-additive metric's `__sv_snapshot` CTE should anchor
/// at, or `None` to leave it base-anchored.
///
/// The sibling of [`window_cte_anchor`], and deliberately the same shape: both
/// re-anchor a single base-anchored CTE, so both carry the same restrictions.
/// Probed against Snowflake (TECH-DEBT #36): it computes the snapshot inside the
/// metric's own-grain aggregation and joins pre-aggregated results, rather than
/// ranking rows a base-anchored join has already multiplied.
///
/// `extra_tables` carries the NA dimensions' source tables and any
/// `where_clause` members'. The NA dims matter especially: an *active*
/// semi-additive metric is by definition one whose NA dim is NOT queried, so its
/// table never appears in `resolved_dims`, yet the snapshot's `ORDER BY` names
/// it. Snowflake accepts an NA dimension declared on another logical table when
/// the reference is qualified (probed), so this must be checked for
/// reachability rather than assumed to be the metric's own table.
pub(super) fn snapshot_cte_anchor(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    extra_tables: &[String],
    where_has_unqualified_member: bool,
) -> Option<String> {
    if def.joins.is_empty() || resolved_mets.is_empty() {
        return None;
    }
    // EXP-22: an unqualified `where_clause` member rebinds with the anchor.
    if where_has_unqualified_member {
        return None;
    }
    if resolved_dims.iter().any(|d| d.source_table.is_none())
        || role_playing_affects_query(def, resolved_dims, resolved_mets, extra_tables, false)
    {
        return None;
    }
    // A metric's `USING` scopes its joins on the base-anchored path; the
    // snapshot emitter does not thread that context, so decline rather than
    // silently re-anchor onto a different relationship instance — unless the
    // relationship targets a table this query never reaches, where it scopes
    // nothing (TECH-DEBT #71; the same test `is_eligible` applies).
    if using_scopes_a_touched_table(def, resolved_dims, resolved_mets, extra_tables) {
        return None;
    }

    let graph = GrainGraph::build(def)?;
    let root = graph.root().to_string();

    // Every metric in the query must sit at one shared grain — the snapshot is a
    // SINGLE CTE, so there is only one anchor to give it. A query mixing grains
    // is the multi-grain case, which belongs to the per-grain planner.
    let mut anchor: Option<String> = None;
    for met in resolved_mets {
        let grains = metric_grain(met, def).anchored(&root);
        let [only] = grains.as_slice() else {
            return None; // One metric spanning several grains.
        };
        match &anchor {
            None => anchor = Some(only.clone()),
            Some(existing) if existing == only => {}
            Some(_) => return None,
        }
    }
    let anchor = anchor?;
    if anchor == root {
        return None; // Already correct as-is.
    }
    // Re-anchor in EITHER direction (TECH-DEBT #71). For a metric on a parent
    // of the base this removes the inflation; for a metric on a CHILD it is
    // the home-table rule itself: `FROM child` counts the child rows whose
    // foreign key is NULL (under a NULL dimension value) and yields no group
    // for a childless parent — exactly what the same metric gets when the
    // per-grain planner computes it alongside a metric of another grain.
    // Everything the CTE must reference has to be reachable from the anchor, or
    // its FROM could not bind the column: the queried dimensions...
    for dim in resolved_dims {
        let table = dim.source_table.as_ref()?.to_ascii_lowercase();
        graph.path(&anchor, &table)?;
    }
    // ...and the NA dimensions / `where_clause` members.
    for table in extra_tables {
        graph.path(&anchor, &table.to_ascii_lowercase())?;
    }
    Some(anchor)
}

pub(super) fn window_cte_anchor(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    where_tables: &[String],
    where_has_unqualified_member: bool,
) -> Option<String> {
    if def.joins.is_empty() || resolved_mets.is_empty() {
        return None;
    }
    // EXP-22: an unqualified `where_clause` member rebinds with the anchor.
    if where_has_unqualified_member {
        return None;
    }
    // Only an all-window query reaches the window emitter — mixing window and
    // aggregate metrics is rejected upstream — and only then is `__sv_agg` the
    // shape being anchored.
    if !resolved_mets.iter().all(|m| m.is_window()) {
        return None;
    }
    if resolved_dims.iter().any(|d| d.source_table.is_none())
        || role_playing_affects_query(def, resolved_dims, resolved_mets, where_tables, false)
    {
        return None;
    }
    // `USING` on a window metric, its inner aggregate or anything that reaches
    // (`using_scopes_a_touched_table` walks them all): the window emitter does
    // not thread a role into `__sv_agg`, so decline unless the relationship
    // targets a table this query never reaches (TECH-DEBT #71).
    if using_scopes_a_touched_table(def, resolved_dims, resolved_mets, where_tables) {
        return None;
    }

    let graph = GrainGraph::build(def)?;
    let root = graph.root().to_string();

    // Every window metric's INNER aggregate must sit at the same single table.
    //
    // The inner metric is deliberately what is measured, not the window metric
    // itself: DDL qualifies a window metric with a source alias
    // (`o.running_balance AS SUM(total_balance) OVER (…)`), and
    // `metric_grain` unions that alias with the inner's for its own
    // conservative fan-trap purpose. Here that union would report two grains for
    // every DDL-declared window metric and decline unconditionally. The alias on
    // a window metric is declarative — emission never references it, because the
    // window function runs over `__sv_agg`'s columns — so the inner aggregate's
    // grain is the one the CTE must anchor at.
    let mut anchor: Option<String> = None;
    for met in resolved_mets {
        let ws = met.window_spec.as_ref()?;
        // EXP-12: `ident_matches`, not raw case-folding — the anchor decision
        // must resolve the reference exactly as the fence and the emitter do,
        // or a quoted reference declines the re-anchor here while the fence
        // (which would have caught the resulting base-anchored fan) also lost
        // the grain, and the query is silently computed over the fanned join.
        if crate::ident::ident_matches(&met.name, &ws.inner_metric) {
            return None; // Self-referential; not a shape to re-anchor.
        }
        let inner = def
            .metrics
            .iter()
            .find(|m| crate::ident::ident_matches(&m.name, &ws.inner_metric))?;
        let grains = metric_grain(inner, def).anchored(&root);
        let [only] = grains.as_slice() else {
            return None; // An inner aggregate spanning several grains — decline.
        };
        match &anchor {
            None => anchor = Some(only.clone()),
            Some(existing) if existing == only => {}
            Some(_) => return None, // Two inner aggregates at different grains.
        }
    }
    let anchor = anchor?;
    if anchor == root {
        return None; // Already correct as-is.
    }
    // Re-anchor in EITHER direction (TECH-DEBT #71). On the "one" side of the
    // root this removes the inflation a base-anchored join would cause. On the
    // "many" side it is the home-table rule: `FROM child LEFT JOIN base` counts
    // a child row whose foreign key is NULL and gives a childless parent no
    // group, which is what the inner metric gets queried on its own (Snowflake,
    // D1). This used to be declined because `FROM base LEFT JOIN child` kept the
    // childless parent as a `COUNT` of 0 — the very base-anchored answer #71
    // retires.
    //
    // Each queried dimension must be joinable from the anchor, or the CTE's
    // FROM could not reach the column its SELECT names.
    for dim in resolved_dims {
        let table = dim.source_table.as_ref()?.to_ascii_lowercase();
        graph.path(&anchor, &table)?;
    }
    Some(anchor)
}

/// Whether the query's shape is one the per-grain emitter can express.
///
/// Deliberately conservative: anything outside the plain-aggregate,
/// qualified-dimension core keeps the v0.11.0 fan-trap error rather than being
/// routed through a strategy that was not designed for it.
fn is_eligible(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    where_tables: &[String],
    where_has_unqualified_member: bool,
) -> bool {
    // A dimension with no source table is an unqualified expression, resolved
    // against whatever the FROM happens to expose. That is well defined only
    // when the anchor is the base table (the case per-grain never rewrites), so
    // decline rather than re-anchor an expression whose binding would move.
    if resolved_dims.iter().any(|d| d.source_table.is_none()) {
        return false;
    }
    // EXP-22: and a `where_clause` member with no source table is the same
    // expression in a different slot. It is spliced into every grain CTE, so a
    // re-anchored FROM rebinds it — silently, whenever the anchor table happens
    // to carry a column of that name, and into two DIFFERENT tables at once in
    // a multi-grain plan.
    if where_has_unqualified_member {
        return false;
    }
    // A snapshot group cannot honour a role: `render_snapshot_group` builds its
    // `ResolvedDim`s with no scoped alias and gives `anchor_joins` an empty role
    // map, so it would join whichever instance is declared first while a sibling
    // PLAIN group in the same query joins the one `USING` named. The two grain
    // CTEs would then group by different instances of the same dimension and the
    // outer join would compare them — a silent wrong answer, not an error. So
    // the scoped-alias rescue is available only when no metric needs a snapshot.
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    let any_active_semi_additive = resolved_mets
        .iter()
        .any(|m| super::semi_additive::is_active_semi_additive(def, m, &queried_dim_keys));
    if role_playing_affects_query(
        def,
        resolved_dims,
        resolved_mets,
        where_tables,
        !any_active_semi_additive,
    ) {
        return false;
    }

    // A metric's `USING` is honoured only in the one shape this emitter threads:
    // every relationship it names targets a role-played table whose role was
    // resolved for THAT metric ([`metric_roles`], the role its group is emitted
    // with) and is therefore emitted as a scoped join. `USING` naming anything
    // else — a relationship to a table reached only one way, where the
    // base-anchored path still scopes the alias — is left on that path,
    // unchanged, rather than guessed at here.
    // TECH-DEBT #71: a `USING` relationship whose target table this query
    // never reaches scopes nothing — the base-anchored path emits its scoped
    // join, but no member names the scoped alias, so it only widens the FROM.
    // Such a relationship is not a reason to keep a child metric off its home
    // table. "Reaches" here is a queried dimension or a `where_clause` member:
    // a fact the metric itself inlines from the role-played table is joined
    // under the scoped alias its `USING` names (`anchor_needs`), so
    // that reach is threaded.
    let reached: Vec<&Metric> = resolved_mets
        .iter()
        .flat_map(|met| metrics_reached(def, met))
        .collect();
    if reached.iter().any(|m| m.is_window()) {
        return false;
    }
    // With no `USING` anywhere, every metric is vacuously threaded: skip
    // scanning every dimension's expression for facts (reviewbot, #250).
    if reached.iter().all(|m| m.using_relationships.is_empty()) {
        return true;
    }
    let touched = touched_tables(def, resolved_dims, resolved_mets, where_tables, false);
    // A relationship `rel` names is threaded when the role resolved for its
    // table is that same relationship, both matched by `graph::RelNameMatch`.
    // For a view stored before #249 with `dep` and `"dep"` to one table, both
    // share the scoped alias and `roles` holds the first-declared; a `USING`
    // naming the other one is not threaded, and the base-anchored path keeps it.
    let using_is_threaded = |m: &Metric| {
        let roles = metric_roles(def, resolved_dims, m).unwrap_or_default();
        m.using_relationships.iter().all(|rel| {
            let named = crate::graph::RelNameMatch::new(&def.joins, rel);
            def.joins.iter().any(|j| {
                named.matches_join(j)
                    && (roles
                        .get(&j.table.to_ascii_lowercase())
                        .is_some_and(|role| named.matches(role))
                        || !touched.contains(&j.table.to_ascii_lowercase()))
            })
        })
    };
    reached.into_iter().all(using_is_threaded)
}

/// `met` and every metric it reaches, transitively — a derived metric's
/// components and a window metric's inner metric — each once, in the
/// definition's declaration order (so emitted SQL is stable).
///
/// The one dependency walk the per-grain planner and the two single-CTE
/// anchors share (reviewbot, #250: it had been copied three times, and the
/// window-inner `USING` gap was a copy that stopped one level short).
fn metrics_reached<'a>(def: &'a SemanticViewDefinition, met: &Metric) -> Vec<&'a Metric> {
    let names = super::facts::collect_transitive_metric_names(met, &def.metrics);
    def.metrics
        .iter()
        .filter(|m| names.contains(&crate::ident::normalize_ident_part(&m.name)))
        .collect()
}

/// Every table (lowercased) a grain CTE for this query could have to name: the
/// queried dimensions' tables and the facts their expressions reach, the
/// `where_clause` members' tables, and — with `include_metric_facts` — the facts
/// each queried metric, or a metric it depends on, inlines. A metric's own table
/// is not included: it is the CTE's anchor, never a joined table.
fn touched_tables(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    where_tables: &[String],
    include_metric_facts: bool,
) -> HashSet<String> {
    let mut out: HashSet<String> = where_tables
        .iter()
        .map(|t| t.to_ascii_lowercase())
        .collect();
    let add_facts = |expr: &str, out: &mut HashSet<String>| {
        for alias in super::facts::collect_referenced_fact_tables(expr, &def.facts) {
            out.insert(alias.to_ascii_lowercase());
        }
    };
    for dim in resolved_dims {
        if let Some(st) = &dim.source_table {
            out.insert(st.to_ascii_lowercase());
        }
        add_facts(&dim.expr, &mut out);
    }
    for met in resolved_mets.iter().filter(|_| include_metric_facts) {
        for m in metrics_reached(def, met) {
            add_facts(&m.expr, &mut out);
        }
    }
    out
}

/// Whether any queried metric's `USING` names a relationship whose target table
/// this query reaches (see [`touched_tables`]). The single-CTE anchors cannot
/// honour a role, so for them such a relationship declines the re-anchor; one
/// whose target nothing reaches scopes nothing and is no obstacle.
fn using_scopes_a_touched_table(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    extra_tables: &[String],
) -> bool {
    // Every metric the query reaches, not only the queried ones: a `USING` on a
    // derived metric's component or a window's (transitive) inner metric scopes
    // the fact that component inlines just the same (reviewbot, #250: a window
    // over a derived metric whose component carried the `USING` was
    // re-anchored with no scoped join, and failed to bind `ap__dep`).
    let mut reached: Vec<&Metric> = Vec::new();
    for met in resolved_mets {
        for m in metrics_reached(def, met) {
            if !reached.iter().any(|r| std::ptr::eq(*r, m)) {
                reached.push(m);
            }
        }
    }
    if reached.iter().all(|m| m.using_relationships.is_empty()) {
        return false;
    }
    let touched = touched_tables(def, resolved_dims, resolved_mets, extra_tables, true);
    // Resolved with the shared `graph::find_relationship` (#249), never
    // `ident_matches`: that folds the one-part `"x.y"` and the two-part `x.y`
    // together, which are different relationship names. A name that resolves
    // to nothing is treated as scoping a touched table (decline, unchanged).
    reached.iter().any(|m| {
        m.using_relationships.iter().any(|rel| {
            crate::graph::find_relationship(&def.joins, rel)
                .is_none_or(|j| touched.contains(&j.table.to_ascii_lowercase()))
        })
    })
}

/// Emit the SQL for a per-grain [`Plan`].
///
/// Single grain: a flat `SELECT` anchored at that grain (the base-anchored shape
/// with a different anchor). Several grains: one CTE per grain, combined with
/// `FULL OUTER JOIN` on the queried dimensions — NULL-safe, so a group present
/// at one grain and absent at another survives with a NULL metric — or
/// `CROSS JOIN` when the query has no dimensions and each grain yields one row.
#[allow(clippy::too_many_arguments)]
pub(super) fn expand_per_grain(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    plan: &Plan,
    where_clause: Option<&ResolvedWhere>,
) -> Result<String, super::types::ExpandError> {
    // A cross-grain assembly always spans at least two groups, so a single-group
    // plan holds only `Direct` outputs — but check rather than assert: falling
    // through to the general emitter is correct for any plan, and a wrong
    // assumption here would panic inside DuckDB rather than mis-render.
    if plan.is_single_group()
        && plan
            .outputs
            .iter()
            .all(|o| matches!(o, Output::Direct { .. }))
    {
        return Ok(render_single_grain(
            def,
            resolved_dims,
            resolved_mets,
            plan,
            where_clause,
        ));
    }
    render_multi_grain(
        view_name,
        def,
        resolved_dims,
        resolved_mets,
        resolved_exprs,
        plan,
        where_clause,
    )
}

/// Render one grain group's CTE body: the ordinary aggregation shape, anchored
/// at this grain's own table. The sibling of [`render_snapshot_group`], which
/// handles the group that must rank its rows first.
fn render_plain_group(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    group: &Group,
    where_clause: Option<&ResolvedWhere>,
    where_tables: &[String],
) -> String {
    // The group's own roles, threaded into both the join and the dimension
    // expression so they cannot disagree about which role a table plays.
    let roles = &group.roles;
    let mut sql = String::with_capacity(256);
    sql.push_str("    SELECT\n");
    let mut items: Vec<String> = Vec::new();
    for (d, dim) in resolved_dims.iter().enumerate() {
        let item = SelectItem::new(
            dim_expr(dim, roles, def),
            dim.output_type.clone(),
            quote_ident(&dim_column(d)),
        );
        items.push(format!("        {}", item.render()));
    }
    for (m, expr) in group.exprs.iter().enumerate() {
        items.push(format!(
            "        {} AS {}",
            expr,
            quote_ident(&metric_column(m))
        ));
    }
    sql.push_str(&items.join(",\n"));
    push_from_anchor(&mut sql, def, &group.anchor, "\n    ");
    let needs = anchor_needs(def, &group_metrics(def, group), resolved_dims, where_tables);
    push_join_clauses(
        &mut sql,
        &anchor_joins(
            def,
            &group.anchor,
            resolved_dims,
            &needs.bare,
            roles,
            &needs.scoped,
        ),
        def,
        "\n    LEFT JOIN ",
    );
    // Inside the CTE, before its GROUP BY: each grain must aggregate over only
    // the matching rows. On the outer query this would instead filter the
    // already-combined result — a post-aggregation filter.
    if let Some(w) = where_clause {
        sql.push_str("\n    WHERE ");
        sql.push_str(&w.sql);
    }
    push_group_by_ordinals(&mut sql, resolved_dims.len(), "\n    ", "        ");
    sql
}

/// Render one grain group's CTE body when that group carries an ACTIVE
/// semi-additive metric.
///
/// The rows must be ranked BEFORE this grain aggregates them, so the body is
/// `SELECT <outer> FROM (<inner>)` — the same two halves
/// [`super::semi_additive::build_snapshot_block`] gives the base-anchored
/// emitter, nested instead of split across a `WITH`. Sharing that builder is
/// what keeps the `RANK` tie semantics (SG-4), the decomposition rejection
/// (SG-5) and the NA-dimension joins identical on both paths.
///
/// `build_snapshot_block` indexes its `__sv_semi_{i}` / `__sv_reg_{i}` capture
/// columns by position within the metrics it is GIVEN, so passing only this
/// group's metrics gives each group its own column namespace — two snapshot
/// groups in one query cannot collide.
fn render_snapshot_group(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    group: &Group,
    resolved_exprs: &HashMap<String, String>,
    where_clause: Option<&ResolvedWhere>,
) -> Result<String, super::types::ExpandError> {
    // No dimension here carries a scoped alias, and none can: `is_eligible`
    // withholds the scoped-alias rescue from any query with an active
    // semi-additive metric, precisely BECAUSE this renderer cannot honour a role
    // while a sibling plain group can — see the guard test
    // `role_played_dimension_with_a_semi_additive_metric_stays_ineligible`.
    let rdims: Vec<super::types::ResolvedDim> = resolved_dims
        .iter()
        .map(|dim| super::types::ResolvedDim {
            dim,
            scoped_alias: None,
        })
        .collect();
    let dim_aliases: Vec<String> = (0..resolved_dims.len())
        .map(|d| quote_ident(&dim_column(d)))
        .collect();
    let mets = group_metrics(def, group);

    let block = super::semi_additive::build_snapshot_block(
        view_name,
        def,
        &rdims,
        &dim_aliases,
        &mets,
        resolved_exprs,
        where_clause,
        Some(&group.anchor),
    )?;

    let mut sql = String::with_capacity(256);
    sql.push_str("    SELECT\n");
    let mut items: Vec<String> = Vec::new();
    // The inner SELECT already aliased each dimension to its `__sv_d{i}`
    // column, so the outer one groups by that alias and re-exposes it.
    for d in 0..resolved_dims.len() {
        items.push(format!("        {}", quote_ident(&dim_column(d))));
    }
    for (m, expr) in block.outer_exprs.iter().enumerate() {
        items.push(format!(
            "        {} AS {}",
            expr,
            quote_ident(&metric_column(m))
        ));
    }
    sql.push_str(&items.join(",\n"));
    sql.push_str("\n    FROM (\n");
    sql.push_str(&block.inner_sql);
    sql.push_str("\n    )");
    push_group_by_ordinals(&mut sql, resolved_dims.len(), "\n    ", "        ");
    Ok(sql)
}

/// The one-grain case: the ordinary aggregation shape, anchored at the metrics'
/// own table instead of the base table.
fn render_single_grain(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    plan: &Plan,
    where_clause: Option<&ResolvedWhere>,
) -> String {
    let group = &plan.groups[0];
    let roles = &group.roles;
    let where_tables = where_clause
        .map(|w| w.source_tables.clone())
        .unwrap_or_default();
    let mut items: Vec<SelectItem> = resolved_dims
        .iter()
        .map(|dim| {
            SelectItem::new(
                dim_expr(dim, roles, def),
                dim.output_type.clone(),
                quote_stored_ident(&dim.name),
            )
        })
        .collect();
    for (met, output) in resolved_mets.iter().zip(&plan.outputs) {
        let Output::Direct { column, .. } = output else {
            continue; // Caller established every output is Direct here.
        };
        items.push(SelectItem::new(
            group.exprs[*column].clone(),
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }
    let group_by = if resolved_dims.is_empty() {
        GroupBy::None
    } else {
        GroupBy::Ordinals(resolved_dims.len())
    };
    let needs = anchor_needs(
        def,
        &group_metrics(def, group),
        resolved_dims,
        &where_tables,
    );
    SelectSpec {
        // Filters the anchor's rows on their way into the aggregation, the same
        // position the base-anchored path uses.
        where_clause: where_clause.map(|w| w.sql.clone()),
        distinct: false,
        items,
        from: FromSource::AnchorTable {
            def,
            anchor: group.anchor.clone(),
            joins: anchor_joins(
                def,
                &group.anchor,
                resolved_dims,
                &needs.bare,
                roles,
                &needs.scoped,
            ),
        },
        group_by,
    }
    .render()
}

/// The general case: one CTE per grain, joined on the queried dimensions.
#[allow(clippy::too_many_arguments)]
fn render_multi_grain(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    plan: &Plan,
    where_clause: Option<&ResolvedWhere>,
) -> Result<String, super::types::ExpandError> {
    let mut sql = String::with_capacity(512);
    let where_tables = where_clause
        .map(|w| w.source_tables.clone())
        .unwrap_or_default();
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    for (i, group) in plan.groups.iter().enumerate() {
        sql.push_str(if i == 0 { "WITH " } else { ",\n" });
        sql.push_str(&grain_cte(i));
        sql.push_str(" AS (\n");
        if is_snapshot_group(def, group, &queried_dim_keys) {
            sql.push_str(&render_snapshot_group(
                view_name,
                def,
                resolved_dims,
                group,
                resolved_exprs,
                where_clause,
            )?);
            sql.push_str("\n)");
            continue;
        }
        sql.push_str(&render_plain_group(
            def,
            resolved_dims,
            group,
            where_clause,
            &where_tables,
        ));
        sql.push_str("\n)");
    }
    sql.push('\n');

    // Outer SELECT: dimensions coalesced across the grains that carry them,
    // then each metric read back from its grain column(s).
    let mut items: Vec<SelectItem> = Vec::new();
    for (d, dim) in resolved_dims.iter().enumerate() {
        items.push(SelectItem::new(
            coalesced_key(plan.groups.len(), d),
            None,
            quote_stored_ident(&dim.name),
        ));
    }
    for (met, output) in resolved_mets.iter().zip(&plan.outputs) {
        let expr = match output {
            Output::Direct { group, column } => column_ref(*group, &metric_column(*column)),
            Output::Derived { expr } => expr.clone(),
        };
        items.push(SelectItem::new(
            expr,
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }
    let rendered: Vec<String> = items
        .iter()
        .map(|item| format!("    {}", item.render()))
        .collect();
    sql.push_str("SELECT\n");
    sql.push_str(&rendered.join(",\n"));
    sql.push_str("\nFROM ");
    sql.push_str(&grain_cte(0));
    for g in 1..plan.groups.len() {
        if resolved_dims.is_empty() {
            // One row per grain: the combination is a plain product.
            sql.push_str("\nCROSS JOIN ");
            sql.push_str(&grain_cte(g));
        } else {
            sql.push_str("\nFULL OUTER JOIN ");
            sql.push_str(&grain_cte(g));
            let conditions: Vec<String> = (0..resolved_dims.len())
                .map(|d| {
                    format!(
                        "{} IS NOT DISTINCT FROM {}",
                        coalesced_key(g, d),
                        column_ref(g, &dim_column(d))
                    )
                })
                .collect();
            sql.push_str("\n    ON ");
            sql.push_str(&conditions.join("\n    AND "));
        }
    }
    Ok(sql)
}

/// The join key for dimension `d` over the first `groups` grain CTEs: a plain
/// column reference for one, `COALESCE(...)` beyond that.
///
/// `FULL OUTER JOIN` leaves the key NULL on whichever side has no matching
/// group, so each successive join — and the output column — must read the key
/// from whichever grain supplied it.
fn coalesced_key(groups: usize, d: usize) -> String {
    let refs: Vec<String> = (0..groups).map(|g| column_ref(g, &dim_column(d))).collect();
    if refs.len() == 1 {
        refs.into_iter().next().unwrap_or_default()
    } else {
        format!("COALESCE({})", refs.join(", "))
    }
}

/// The `LEFT JOIN`s a grain CTE anchored at `anchor` needs to reach every
/// queried dimension's table, plus any table named in `extra_tables`.
///
/// Each hop on the tree path from the anchor to a dimension's table is emitted
/// once, in path order, so every ON clause references only the table it
/// introduces and one already in scope. The planner has already established
/// that none of these paths fans the anchor, so the anchor's row count — and
/// therefore its aggregate — is unaffected by them.
pub(super) fn anchor_joins<'a>(
    def: &'a SemanticViewDefinition,
    anchor: &str,
    resolved_dims: &[&Dimension],
    extra_tables: &[String],
    roles: &HashMap<String, String>,
    scoped_facts: &[&'a Join],
) -> Vec<ResolvedJoin<'a>> {
    let Some(graph) = GrainGraph::build(def) else {
        return Vec::new();
    };
    let mut joins: Vec<ResolvedJoin<'a>> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    emitted.insert(anchor.to_string());
    // `extra_tables` carries the tables this CTE must reach for reasons other
    // than a queried dimension, and they are all the same reason underneath:
    // something inside this SELECT names an alias, so the alias has to be in
    // this FROM. Two contribute today — a `where_clause` member's table, since
    // the predicate is injected into every grain CTE, and (PAR-6) the table of
    // a fact a grain's metric references, since the fact's expression is
    // inlined into the aggregate this CTE computes.
    let dim_sources = resolved_dims.iter().filter_map(|d| d.source_table.clone());
    let sources: Vec<String> = dim_sources.chain(extra_tables.iter().cloned()).collect();
    for source in &sources {
        let Some(path) = graph.path(anchor, &source.to_ascii_lowercase()) else {
            continue;
        };
        for pair in path.windows(2) {
            let (from, to) = (&pair[0], &pair[1]);
            // A role-played hop is emitted under its scoped alias, joined on the
            // relationship `USING` NAMED. `edge_between` cannot be used here: it
            // returns whichever of the several edges is declared first, which is
            // the declaration-order mis-binding the eligibility gate exists to
            // keep out of this emitter.
            if let Some(rel) = roles.get(to) {
                let scoped = super::join_resolver::scoped_join_alias(to, rel);
                if !emitted.insert(scoped.clone()) {
                    continue;
                }
                match crate::graph::find_relationship(&def.joins, rel) {
                    Some(join) if join.table.eq_ignore_ascii_case(to) => {
                        joins.push(ResolvedJoin {
                            emit_alias: scoped,
                            bare_alias: to.clone(),
                            join,
                            scoped: true,
                            from_emit: None,
                        });
                    }
                    // Phase 6.3b: a table past the role-played one, joined off
                    // the role's instance of its parent (`r__dep` on `a__dep`).
                    Some(_) => {
                        if let Some(edge) = edge_between(def, from, to) {
                            joins.push(ResolvedJoin {
                                emit_alias: scoped,
                                bare_alias: to.clone(),
                                join: edge,
                                scoped: true,
                                from_emit: Some(super::join_resolver::scoped_join_alias(from, rel)),
                            });
                        }
                    }
                    None => {}
                }
                continue;
            }
            if !emitted.insert(to.clone()) {
                continue;
            }
            if let Some(join) = edge_between(def, from, to) {
                joins.push(ResolvedJoin {
                    emit_alias: to.clone(),
                    bare_alias: to.clone(),
                    join,
                    scoped: false,
                    from_emit: None,
                });
            }
        }
    }
    // TECH-DEBT #71: the role-played instances a metric's `USING` selects for a
    // fact it inlines (see [`anchor_needs`]). Each is joined under its
    // scoped alias straight off the relationship's own source table — the
    // relationship NAMES the edge, so nothing is picked by declaration order —
    // after making sure that source table is itself in scope. Two roles of one
    // table are two joins, as on the base-anchored path.
    for join in scoped_facts {
        let from = join.from_alias.to_ascii_lowercase();
        if let Some(path) = graph.path(anchor, &from) {
            for pair in path.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                // A role-played hop here is unreachable: the #69 fence rejects
                // a fact on a table reached only THROUGH a role-played table
                // (no `USING` can pick that role), so a scoped fact's source
                // table is never behind one. Skipped rather than joined bare
                // all the same, so this can never bind declaration order.
                if roles.contains_key(b) || !emitted.insert(b.clone()) {
                    continue;
                }
                if let Some(edge) = edge_between(def, a, b) {
                    joins.push(ResolvedJoin {
                        emit_alias: b.clone(),
                        bare_alias: b.clone(),
                        join: edge,
                        scoped: false,
                        from_emit: None,
                    });
                }
            }
        }
        let table = join.table.to_ascii_lowercase();
        let scoped = super::join_resolver::scoped_join_alias(
            &table,
            join.name.as_deref().unwrap_or_default(),
        );
        if emitted.insert(scoped.clone()) {
            joins.push(ResolvedJoin {
                emit_alias: scoped,
                bare_alias: table,
                join,
                scoped: true,
                from_emit: None,
            });
        }
    }
    joins
}

/// A queried dimension's expression as the grain CTEs must emit it: qualified by
/// its role's scoped alias when `USING` named one, and unchanged otherwise.
///
/// Mirrors what the base-anchored path does with [`ResolvedDim::scoped_alias`],
/// so both paths emit `a__dep.city` for the same query.
fn dim_expr(
    dim: &Dimension,
    roles: &HashMap<String, String>,
    def: &SemanticViewDefinition,
) -> String {
    // TECH-DEBT #54: facts first, then the role-playing qualifier rewrite, so a
    // fact's own qualifier is rewritten along with the rest of the expression.
    let expr = super::facts::inline_dimension_facts(&dim.expr, &def.facts);
    let Some(table) = dim.source_table.as_ref().map(|t| t.to_ascii_lowercase()) else {
        return expr;
    };
    match roles.get(&table) {
        Some(rel) => super::join_resolver::rewrite_to_scoped_alias(
            &expr,
            &table,
            &super::join_resolver::scoped_join_alias(&table, rel),
        ),
        None => expr,
    }
}

/// The relationship edge connecting two adjacent tables, in either direction.
///
/// Direction does not matter for emission: [`synthesize_on_clause`] names both
/// sides explicitly, and the caller emits hops in path order so the other side
/// is always already in scope.
fn edge_between<'a>(def: &'a SemanticViewDefinition, a: &str, b: &str) -> Option<&'a Join> {
    def.joins.iter().find(|j| {
        !j.fk_columns.is_empty() && {
            let (from, to) = (
                j.from_alias.to_ascii_lowercase(),
                j.table.to_ascii_lowercase(),
            );
            (from == a && to == b) || (from == b && to == a)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::join_resolver::synthesize_on_clause;

    /// The ON clause is synthesized from the edge regardless of which direction
    /// the path walks it — the property `anchor_joins` relies on when a grain
    /// CTE joins *up* an FK edge its anchor sits below.
    #[test]
    fn edge_between_finds_either_direction() {
        let def = crate::expand::test_helpers::minimal_def("o", "d", "d", "m", "count(*)");
        let def = crate::expand::test_helpers::TestFixtureExt::with_pkfk_join(
            crate::expand::test_helpers::TestFixtureExt::with_table(def, "c", "customers", &["id"]),
            "o_to_c",
            "o",
            "c",
            &["customer_id"],
            &["id"],
        );
        let forward = edge_between(&def, "o", "c").expect("edge o -> c");
        let reverse = edge_between(&def, "c", "o").expect("same edge, walked backwards");
        assert_eq!(forward.name, reverse.name);
        assert_eq!(
            synthesize_on_clause(forward, &def.tables),
            r#""o"."customer_id" = "c"."id""#
        );
    }

    #[test]
    fn coalesced_key_is_bare_for_one_grain_and_coalesced_beyond() {
        assert_eq!(coalesced_key(1, 0), r#""__sv_grain_0"."__sv_d0""#);
        assert_eq!(
            coalesced_key(2, 1),
            r#"COALESCE("__sv_grain_0"."__sv_d1", "__sv_grain_1"."__sv_d1")"#
        );
    }
}
