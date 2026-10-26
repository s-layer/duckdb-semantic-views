//! CTE-based expansion for semi-additive metrics.
//!
//! When any requested metric has a non-empty `non_additive_by` field AND at least
//! one of its NA dims is NOT in the queried dimensions, the expansion wraps the
//! base query in a CTE that uses `RANK()` to select snapshot rows before
//! aggregation. `RANK()` — not `ROW_NUMBER()` — so that when the fact grain is
//! finer than the queried dims (e.g. several accounts per customer, each with a
//! row at the same latest date), ALL rows tied at the snapshot ordering value
//! share rank 1 and aggregate together, deterministically (SG-4).
//!
//! **Effectively-regular classification (Snowflake semantics):**
//! A semi-additive metric whose ALL NA dims are present in the queried dimensions
//! is treated as "effectively regular" for that query -- no CTE needed, standard
//! aggregation applies. This matches Snowflake's behavior: "When the non-additive
//! dimension is included in the query, the metric is calculated as a standard
//! additive metric."
//!
//! The strategy for active semi-additive metrics:
//! - Single CTE (`__sv_snapshot`) containing all raw columns needed
//! - `RANK() OVER (PARTITION BY non-NA-dims ORDER BY NA-dims) AS __sv_rn`
//!   (the `__sv_rn` column name predates the RANK switch and is pinned by
//!   sqllogictests -- keep it). The window's ORDER BY reverses each NA dim's
//!   declared direction so `RANK() = 1` lands on the LAST ordering value of the
//!   declared sort (ties included — see the RANK note above) — the default (ASC)
//!   therefore selects the latest snapshot and DESC the earliest, matching
//!   Snowflake (F-1, code-review 2026-07-16).
//! - Outer SELECT: regular/effectively-regular metrics use plain aggregation,
//!   active semi-additive metrics use `SUM(CASE WHEN __sv_rn = 1 THEN raw_val END)`
//!
//! Because the CTE decomposes every metric into an inner-expression capture plus
//! an outer re-aggregation, every metric in the query must be a single bare
//! aggregate call `FUNC(args)`. Anything else (arithmetic-wrapped, `COUNT(*)`,
//! DISTINCT, `COALESCE`-wrapped, multi-aggregate derived metrics) cannot be
//! decomposed and is rejected with a clear error instead of silently mangled
//! (SG-5).
//!
//! When multiple semi-additive metrics have different NON ADDITIVE BY dimensions,
//! each gets its own `__sv_rn_N` column in the CTE.
//!
//! **Where the CTE is anchored.** By default `FROM <base table>`, outward. When
//! every queried metric sits at one NON-base grain that the base-anchored join
//! would fan, [`super::per_grain::snapshot_cte_anchor`] returns that table and
//! the CTE anchors there instead — otherwise `RANK()` would rank rows the join
//! has already multiplied, tying every copy at rank 1 so the outer `SUM` adds
//! the winning value once per base row. Probed against Snowflake (TECH-DEBT
//! #36): it computes the snapshot inside the metric's own-grain aggregation.
//! The NA dims' source tables ride the same extra-sources channel either way,
//! which matters most when re-anchored — an *active* NA dim is never in the
//! queried dimensions, so nothing else would join the table its ORDER BY
//! names.

use std::collections::{HashMap, HashSet};

use crate::model::{Metric, NonAdditiveDim, NullsOrder, SemanticViewDefinition, SortOrder};

use super::join_resolver::{push_join_clauses, resolve_joins_pkfk};
use super::resolution::{quote_ident, quote_stored_ident};
use super::select_spec::{
    push_from_anchor, push_from_base, FromSource, GroupBy, SelectItem, SelectSpec,
};
use super::types::{ExpandError, ResolvedDim};

/// Resolve a NON ADDITIVE BY dim reference — bare (`report_date`), dotted
/// (`o.report_date`), or either with quoted parts (`o."order date"`) — to the
/// canonical match key of its declared dimension's stored name.
///
/// Thin alias over the shared [`super::resolution::dim_ref_key`] (which the
/// window dimension-side matcher uses too). Routing every NA-vs-dimension
/// comparison in this module through this one key is what makes a dotted NA
/// reference classify, partition, order, and join identically to the
/// equivalent bare dimension (#30) — a bare `to_ascii_lowercase` could never
/// match a dotted reference against the stored bare dimension name, so a
/// dotted NA dim was previously always classified active and its raw text
/// emitted as a quoted non-column. Unresolvable references fall back to the
/// normalized raw text (fail-clean); see `dim_ref_key`.
pub(super) fn na_dim_match_key(def: &SemanticViewDefinition, na_dimension: &str) -> String {
    super::resolution::dim_ref_key(def, na_dimension)
}

/// Resolve the role-playing scoped alias for a semi-additive NON ADDITIVE BY
/// dimension, in the context of one owning metric's `USING` clause — the same
/// resolution the queried-dimension path performs via
/// [`super::role_playing::find_using_context`] (`sql_gen.rs`). Returns:
///
/// - `Ok(None)` — the NA dim resolves to a dimension whose source table is not
///   a role-playing target (reached by at most one relationship), or it has no
///   source table / does not resolve: the snapshot orders by the raw declared
///   expression, unchanged.
/// - `Ok(Some(alias))` — the NA dim's table is role-playing and the metric's
///   `USING` disambiguates to exactly one relationship: the snapshot must order
///   by `alias.col` (e.g. `a__dep_airport`), the *same* scoped instance the
///   metric joins. Ordering by the bare table instead would rank by whichever
///   role the join resolver picked first (the first-declared relationship),
///   ignoring `USING` — a silent wrong-role snapshot (F-18 / code-review
///   2026-07-16 T-15).
/// - `Err(AmbiguousPath)` — role-playing target with no single `USING` context:
///   the snapshot end is genuinely ambiguous. Fail loud, matching the
///   queried-dim path's identical rejection (`phase32_role_playing.test`).
///
/// A single-metric context (`from_ref`) is passed deliberately: each metric's
/// own `USING` drives the role of its NA dims, so two metrics that share an NA
/// dim set but differ in `USING` resolve to different aliases and are split
/// into separate `RANK()` groups (folded into the group key by
/// [`collect_na_groups`]).
fn na_dim_scoped_alias(
    view_name: &str,
    def: &SemanticViewDefinition,
    met: &Metric,
    na_dimension: &str,
) -> Result<Option<String>, ExpandError> {
    let Some(dim) = super::resolution::find_dimension(def, na_dimension) else {
        // Unresolvable references are handled by the fail-clean `quote_ident`
        // arm at the ORDER BY site; no role to resolve here.
        return Ok(None);
    };
    super::role_playing::find_using_context(view_name, def, dim, std::slice::from_ref(&met))
}

/// Returns true when `met` is an ACTIVE semi-additive metric for a query over
/// `queried_dim_keys` (each the canonical [`crate::ident::normalize_ident_part`]
/// key of a queried dimension's stored name): it has a NON ADDITIVE BY clause
/// and at least one of its NA dims is NOT in the queried dimension set, so it
/// takes the `RANK`-CTE snapshot path. When ALL NA dims are queried, the
/// metric is "effectively regular" (Snowflake semantics) and takes the
/// standard aggregation path.
///
/// Each NA dim is resolved to its dimension key via [`na_dim_match_key`] so a
/// dotted/quoted NA reference matches the queried dimension it names (#30).
///
/// This is THE routing predicate — shared by `expand()` (CTE dispatch),
/// `expand_semi_additive` (per-metric classification), and the fan-trap check
/// (which must skip exactly the metrics that take the CTE path, SG-6) so the
/// three cannot drift.
pub(super) fn is_active_semi_additive(
    def: &SemanticViewDefinition,
    met: &Metric,
    queried_dim_keys: &HashSet<String>,
) -> bool {
    !met.non_additive_by.is_empty()
        && met
            .non_additive_by
            .iter()
            .any(|na| !queried_dim_keys.contains(&na_dim_match_key(def, &na.dimension)))
}

/// Generate CTE-based expansion SQL for queries containing semi-additive metrics.
///
/// Called from `expand()` when `has_active_semi_additive` is true.
/// Receives already-resolved dims, metrics, expressions, and scoped aliases.
/// The two halves of a snapshot computation, ready for either caller.
///
/// [`expand_semi_additive`] assembles them as
/// `WITH __sv_snapshot AS (<inner_sql>) SELECT <outer_exprs> FROM __sv_snapshot`.
/// The per-grain planner nests the same pair inside one grain CTE, as
/// `SELECT <outer_exprs> FROM (<inner_sql>)`. One builder for both is what stops
/// the paths drifting on the parts that carry the correctness — the `RANK` tie
/// semantics (SG-4), the decomposition rejection (SG-5), dotted NA resolution
/// (#30) and the role-playing ORDER BY scoping (T-15).
pub(super) struct SnapshotBlock {
    /// The inner SELECT: dimension columns, each metric's captured raw value,
    /// the rank column(s), and the FROM / JOIN / WHERE that feed them. Indented
    /// for a one-level nesting; SQL is whitespace-insensitive, so a caller that
    /// nests it deeper does not need to re-indent.
    pub(super) inner_sql: String,
    /// One outer re-aggregation per metric, in `resolved_mets` order, unaliased.
    pub(super) outer_exprs: Vec<String>,
}

/// Build both halves of the snapshot for `resolved_mets`.
///
/// `dim_aliases[i]` is the already-quoted alias the `i`th dimension takes in the
/// inner SELECT — the stored name for the base-anchored path, `__sv_d{i}` for a
/// grain CTE. `anchor` re-points the FROM at a metric's own grain when
/// [`super::per_grain::snapshot_cte_anchor`] found a safe one.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_snapshot_block(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[ResolvedDim],
    dim_aliases: &[String],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    where_clause: Option<&super::where_clause::ResolvedWhere>,
    anchor: Option<&str>,
) -> Result<SnapshotBlock, ExpandError> {
    // `dim_aliases` is positionally parallel to `resolved_dims`; state it here so
    // a caller that builds the two separately fails on this line rather than on
    // an opaque index panic inside the column emitter.
    debug_assert_eq!(
        resolved_dims.len(),
        dim_aliases.len(),
        "dim_aliases must be parallel to resolved_dims"
    );

    // Canonical keys (quote-stripped + folded) so a quoted stored dim name
    // matches, and so a dotted/quoted NA reference resolves against it (#30).
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|rd| crate::ident::normalize_ident_part(&rd.dim.name))
        .collect();

    let is_active_semi =
        |met: &Metric| -> bool { is_active_semi_additive(def, met, &queried_dim_keys) };

    // 1. Identify distinct NON ADDITIVE BY dimension sets for ACTIVE metrics only.
    let na_groups = collect_na_groups(view_name, def, resolved_mets, &queried_dim_keys)?;

    // 2. SG-5: validate and decompose every metric expression BEFORE emitting
    //    any SQL. The snapshot decomposes each metric into an inner-expression
    //    capture plus an outer re-aggregation, which is only sound for a single
    //    bare aggregate call. Anything else was previously mangled silently
    //    (dropped arithmetic, star/DISTINCT arguments emitted as broken columns)
    //    -- reject it with a clear error instead.
    let decomposed = decompose_metrics(view_name, resolved_mets, resolved_exprs, &is_active_semi)?;

    let mut sql = String::with_capacity(512);
    sql.push_str("    SELECT\n");

    let mut cte_select_items: Vec<String> = Vec::new();

    // Dimension columns (returns the rendered dimension EXPRESSIONS the RANK()
    // window clauses must repeat, never the aliases — E-1).
    let dim_cte_exprs =
        push_cte_dimension_columns(resolved_dims, dim_aliases, &mut cte_select_items, def);

    // Metric raw columns -- the validated inner expression of each metric's
    // aggregate call (decomposed above).
    for (met_idx, met) in resolved_mets.iter().enumerate() {
        let inner = &decomposed[met_idx].1;
        if is_active_semi(met) {
            // Active semi-additive: snapshot-filtered in the outer SELECT
            cte_select_items.push(format!("        {inner} AS \"__sv_semi_{met_idx}\""));
        } else {
            // Regular or effectively-regular: aggregated over all rows
            cte_select_items.push(format!("        {inner} AS \"__sv_reg_{met_idx}\""));
        }
    }

    // Snapshot rank columns (one per active NA group). RANK() so that rows tied
    // on all NA ordering keys share rank 1 and ALL aggregate (SG-4);
    // ROW_NUMBER() would keep one arbitrary tied row per partition.
    for (group_idx, group) in na_groups.iter().enumerate() {
        let rn_alias = if na_groups.len() == 1 {
            "__sv_rn".to_string()
        } else {
            format!("__sv_rn_{}", group_idx + 1)
        };
        cte_select_items.push(snapshot_rank_column(
            def,
            resolved_dims,
            &dim_cte_exprs,
            group,
            &rn_alias,
        ));
    }

    sql.push_str(&cte_select_items.join(",\n"));

    match anchor {
        Some(a) => push_from_anchor(&mut sql, def, a, "\n    "),
        None => push_from_base(&mut sql, def, "\n    "),
    }

    // SG-9: the snapshot ORDER BY references each active NA dim's raw expression
    // even when that dim is not queried, so the NA dims' source tables must be
    // joined too. They ride the resolver's extra-alias parameter, which appends
    // each with its path intermediaries (aliases already joined are skipped).
    let mut na_dim_sources = collect_na_dim_source_tables(def, &na_groups);
    // A `where_clause` member's table must be joined here too — the predicate is
    // applied inside this SELECT, so its aliases have to be in this FROM.
    if let Some(w) = where_clause {
        na_dim_sources.extend(w.source_tables.iter().cloned());
    }
    let dims: Vec<&crate::model::Dimension> = resolved_dims.iter().map(|rd| rd.dim).collect();
    let resolved_joins = match anchor {
        // Re-anchored: walk out from the metric's own table. The NA-dim and
        // `where_clause` tables ride the same extra-sources channel they do on
        // the base-anchored path, so the snapshot ORDER BY still binds. So do
        // the tables of the facts the metrics and dimensions inline, which
        // `resolve_joins_pkfk` collects itself but `anchor_joins` does not, and
        // the `USING`-scoped instances a metric's inlined fact was requalified
        // to (reviewbot, #250: a per-grain snapshot group emitted `ap__dep`
        // with no join for it). No dimension roles: `is_eligible` and
        // `snapshot_cte_anchor` withhold the scoped-alias rescue here.
        Some(a) => {
            let needs = super::per_grain::anchor_needs(def, resolved_mets, &dims, &na_dim_sources);
            super::per_grain::anchor_joins(
                def,
                a,
                &dims,
                &needs.bare,
                &std::collections::HashMap::new(),
                &needs.scoped,
            )
        }
        None => resolve_joins_pkfk(def, &dims, resolved_mets, &na_dim_sources),
    };
    push_join_clauses(&mut sql, &resolved_joins, def, "\n    LEFT JOIN ");

    // The predicate goes INSIDE this SELECT, which puts it before the `RANK()`
    // in the select list is evaluated. That ordering is the whole point:
    // filtering changes which row wins the snapshot, and "applied before the
    // metrics are computed" has to mean before the snapshot is picked. Outside,
    // it would pick the snapshot from unfiltered rows and then discard some of
    // them — a different, wrong answer.
    if let Some(w) = where_clause {
        sql.push_str("\n    WHERE ");
        sql.push_str(&w.sql);
    }

    let outer_exprs = resolved_mets
        .iter()
        .enumerate()
        .map(|(met_idx, met)| {
            outer_metric_expr(
                view_name,
                met,
                met_idx,
                &decomposed[met_idx].0,
                is_active_semi(met),
                &na_groups,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(SnapshotBlock {
        inner_sql: sql,
        outer_exprs,
    })
}

pub(super) fn expand_semi_additive(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[ResolvedDim],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    where_clause: Option<&super::where_clause::ResolvedWhere>,
    anchor: Option<&str>,
) -> Result<String, ExpandError> {
    // The base-anchored path names each dimension column by its stored name, so
    // the outer SELECT can reference the alias directly.
    let dim_aliases: Vec<String> = resolved_dims
        .iter()
        .map(|rd| quote_stored_ident(&rd.dim.name))
        .collect();
    let block = build_snapshot_block(
        view_name,
        def,
        resolved_dims,
        &dim_aliases,
        resolved_mets,
        resolved_exprs,
        where_clause,
        anchor,
    )?;

    let mut sql = String::with_capacity(512);
    sql.push_str("WITH __sv_snapshot AS (\n");
    sql.push_str(&block.inner_sql);
    sql.push_str("\n)\n");

    // === Outer SELECT over the snapshot CTE ===
    let mut outer_items: Vec<SelectItem> = Vec::new();

    // Dimension columns: reference the CTE aliases (an outer query over the CTE,
    // so referencing the alias is safe — no physical column shadows it here).
    for rd in resolved_dims {
        outer_items.push(SelectItem::new(
            quote_stored_ident(&rd.dim.name),
            None,
            quote_stored_ident(&rd.dim.name),
        ));
    }

    for (met, expr) in resolved_mets.iter().zip(block.outer_exprs) {
        outer_items.push(SelectItem::new(
            expr,
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }

    // Dimensions present ⇒ ordinal GROUP BY over them; a metrics-only snapshot
    // query is a global aggregate with no GROUP BY.
    let group_by = if resolved_dims.is_empty() {
        GroupBy::None
    } else {
        GroupBy::Ordinals(resolved_dims.len())
    };

    sql.push_str(
        &SelectSpec {
            where_clause: None,
            distinct: false,
            items: outer_items,
            from: FromSource::Named("__sv_snapshot".to_string()),
            group_by,
        }
        .render(),
    );

    Ok(sql)
}

/// Validate and decompose every metric's aggregate expression for the snapshot
/// CTE (SG-5), returning `(agg_func, inner_expr)` per metric in `resolved_mets`
/// order.
///
/// Each metric must be a single bare aggregate call the CTE can split into an
/// inner-expression capture plus an outer re-aggregation
/// ([`parse_snapshot_aggregate`]); anything else is rejected with a typed error
/// rather than silently mangled. The error variant distinguishes the offending
/// metric's role: an ACTIVE semi-additive metric yields
/// [`ExpandError::SemiAdditiveUnsupportedExpression`], while a regular metric
/// co-queried alongside one yields
/// [`ExpandError::SemiAdditiveCoQueryUnsupported`] (naming the semi-additive
/// metric it cannot share the snapshot CTE with).
fn decompose_metrics(
    view_name: &str,
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    is_active_semi: &dyn Fn(&Metric) -> bool,
) -> Result<Vec<(String, String)>, ExpandError> {
    let semi_metric_name = resolved_mets
        .iter()
        .find(|m| is_active_semi(m))
        .map_or_else(String::new, |m| m.name.clone());
    let mut decomposed: Vec<(String, String)> = Vec::with_capacity(resolved_mets.len());
    for met in resolved_mets {
        let resolved_expr = resolved_exprs
            .get(&crate::ident::normalize_ident_part(&met.name))
            .cloned()
            .unwrap_or_else(|| met.expr.clone());
        match parse_snapshot_aggregate(&resolved_expr) {
            Ok(parts) => decomposed.push(parts),
            Err(reason) => {
                return Err(if is_active_semi(met) {
                    ExpandError::SemiAdditiveUnsupportedExpression {
                        view_name: view_name.to_string(),
                        metric_name: met.name.clone(),
                        metric_expr: resolved_expr,
                        reason,
                    }
                } else {
                    ExpandError::SemiAdditiveCoQueryUnsupported {
                        view_name: view_name.to_string(),
                        metric_name: met.name.clone(),
                        metric_expr: resolved_expr,
                        semi_metric_name: semi_metric_name.clone(),
                        reason,
                    }
                });
            }
        }
    }
    Ok(decomposed)
}

/// Emit each queried dimension's column into the snapshot CTE's SELECT list
/// (appending to `cte_select_items`) and return the parallel list of rendered
/// dimension EXPRESSIONS.
///
/// The returned expressions -- not the CTE select aliases -- are what the
/// `RANK()` window PARTITION/ORDER clauses must repeat: inside the CTE's own
/// SELECT, `DuckDB` resolves a window-clause identifier to a same-named physical
/// FROM-clause column before the lateral select alias, so `PARTITION BY
/// "region"` with `upper(o.region) AS region` silently partitioned on the raw
/// column and produced wrong snapshot sums (E-1, code-review 2026-07-11). The
/// standard path defends against the same shadowing with GROUP BY ordinals;
/// this is the CTE-path equivalent.
/// `dim_aliases[i]` is the already-quoted alias for the `i`th dimension. The
/// base-anchored caller passes the stored dimension name; the per-grain planner
/// passes its positional `__sv_d{i}` column, which is why this is a parameter
/// rather than derived from the dimension.
fn push_cte_dimension_columns(
    resolved_dims: &[ResolvedDim],
    dim_aliases: &[String],
    cte_select_items: &mut Vec<String>,
    def: &SemanticViewDefinition,
) -> Vec<String> {
    let mut dim_cte_exprs: Vec<String> = Vec::with_capacity(resolved_dims.len());
    for (idx, rd) in resolved_dims.iter().enumerate() {
        let dim = rd.dim;
        let mut base_expr = super::facts::inline_dimension_facts(&dim.expr, &def.facts);
        if let Some(ref scoped) = rd.scoped_alias {
            if let Some(ref st) = dim.source_table {
                base_expr = super::join_resolver::rewrite_to_scoped_alias(&base_expr, st, scoped);
            }
        }
        let item = SelectItem::new(base_expr, dim.output_type.clone(), dim_aliases[idx].clone());
        cte_select_items.push(format!("        {}", item.render()));
        // The window PARTITION/ORDER clauses must repeat this EXPRESSION, never
        // the select alias (E-1) -- see the doc comment.
        dim_cte_exprs.push(item.rendered_expr());
    }
    dim_cte_exprs
}

/// Build one `RANK() OVER (...) AS "<rn_alias>"` snapshot-rank column for a
/// single NA group, as an 8-space-indented CTE SELECT-list line.
///
/// `RANK()` (not `ROW_NUMBER()`) so rows tied on all NA ordering keys share rank 1
/// and ALL aggregate together (SG-4). The window partitions by the queried
/// dimension EXPRESSIONS excluding this group's NA dims, and orders by each NA
/// dim via [`na_order_item`]; both use expressions rather than CTE aliases
/// (E-1).
fn snapshot_rank_column(
    def: &SemanticViewDefinition,
    resolved_dims: &[ResolvedDim],
    dim_cte_exprs: &[String],
    group: &NaGroup,
    rn_alias: &str,
) -> String {
    // PARTITION BY: all queried dims (NA dims not in query are not in
    // resolved_dims, so this naturally partitions by all queried dims).
    // Key each NA dim through the shared resolver so a QUERIED NA dim
    // named with a dotted/quoted reference is still excluded from the
    // partition (#30) — a bare lowercase compare missed it.
    let na_dim_keys: Vec<String> = group
        .na_dims
        .iter()
        .map(|nd| na_dim_match_key(def, &nd.dimension))
        .collect();

    // Partition by the dimension EXPRESSIONS, not the CTE aliases (E-1).
    let partition_dims: Vec<String> = resolved_dims
        .iter()
        .zip(dim_cte_exprs)
        .filter(|(rd, _)| !na_dim_keys.contains(&crate::ident::normalize_ident_part(&rd.dim.name)))
        .map(|(_, expr)| expr.clone())
        .collect();

    let partition_clause = if partition_dims.is_empty() {
        String::new()
    } else {
        format!("PARTITION BY {}", partition_dims.join(", "))
    };

    // ORDER BY: the NA dims with their specified sort order, always as
    // EXPRESSIONS (E-1: a queried NA dim previously ordered by its CTE
    // alias, which DuckDB binds to a same-named physical column first).
    // A queried NA dim uses its rendered CTE expression; an unqueried NA
    // dim uses its raw definition expression (it has no CTE column). That
    // raw expression may reference a non-base table -- its join is
    // guaranteed below (SG-9).
    let order_items: Vec<String> = group
        .na_dims
        .iter()
        .zip(&group.na_dim_scoped)
        .map(|(nd, scoped)| na_order_item(def, resolved_dims, dim_cte_exprs, nd, scoped.as_ref()))
        .collect();

    let order_clause = order_items.join(", ");

    let window_spec = if partition_clause.is_empty() {
        format!("ORDER BY {order_clause}")
    } else {
        format!("{partition_clause} ORDER BY {order_clause}")
    };

    format!("        RANK() OVER ({window_spec}) AS \"{rn_alias}\"")
}

/// Render one NA dimension's `ORDER BY` item for the snapshot `RANK()` window:
/// `<dim_expr> <dir> <nulls>`.
///
/// #30: the NA dim reference (bare OR dotted, quoted or not) resolves to its
/// declared dimension via the shared resolver, so a dotted `o."order date"`
/// orders by the same expression a bare `order_date` would. A QUERIED NA dim
/// repeats its CTE expression (E-1); an UNQUERIED NA dim uses its raw
/// definition expression, rewritten to the role-playing scoped alias when its
/// metric's `USING` disambiguates (T-15 / F-18). F-1: the direction is the
/// REVERSE of the declared one (NULLS kept) so `RANK() = 1` lands on the LAST
/// value of the declared sort.
fn na_order_item(
    def: &SemanticViewDefinition,
    resolved_dims: &[ResolvedDim],
    dim_cte_exprs: &[String],
    nd: &NonAdditiveDim,
    scoped: Option<&String>,
) -> String {
    // #30: resolve the NA dim reference (bare OR dotted, quoted or
    // not) to its declared dimension via the shared resolver, so a
    // dotted `o."order date"` orders by the same expression a bare
    // `order_date` would — not the raw dotted text emitted as a
    // quoted non-column.
    let dim_expr = super::resolution::find_dimension(def, &nd.dimension).map_or_else(
        // Unresolvable: Phase 47 (`body_parser::mod`) hard-errors at
        // CREATE on any NA dim that names no declared dimension, so
        // only a malformed/unknown residual reaches this arm.
        // Emit a quoted non-column that fails cleanly at bind time
        // (F-18), never the aliased-column shape E-1 fixed.
        || quote_ident(&nd.dimension),
        |d| {
            // A QUERIED NA dim repeats its CTE expression (E-1: the
            // rendered expression, never the alias). An UNQUERIED NA
            // dim uses its raw definition expression — it has no CTE
            // column, and its source table is guaranteed joined
            // below (SG-9). Match the resolved declared dimension
            // against the queried set by canonical key so a
            // dotted/quoted NA reference finds its queried dim.
            let key = crate::ident::normalize_ident_part(&d.name);
            resolved_dims
                .iter()
                .zip(dim_cte_exprs)
                .find(|(rd, _)| crate::ident::normalize_ident_part(&rd.dim.name) == key)
                .map_or_else(
                    || {
                        // UNQUERIED NA dim. Rewrite its declared
                        // expression to the role-playing scoped alias
                        // when its metric's USING disambiguates
                        // (T-15 / F-18) — the SAME instance the metric
                        // joins (e.g. `a.city` -> `a__dep_airport.city`).
                        // Without this the snapshot ordered by the bare
                        // table, whose join edge the resolver picks
                        // first (the first-declared relationship),
                        // ignoring USING — a silent wrong-role snapshot.
                        // Non-role-playing dims resolve to `None` and
                        // keep the raw expression (unchanged path).
                        // TECH-DEBT #54: an UNQUERIED NA dim renders its
                        // declared expression here rather than a CTE column, so
                        // it needs the same fact inlining every other dimension
                        // emitter applies — otherwise the snapshot orders by an
                        // un-inlined fact name.
                        let mut e = super::facts::inline_dimension_facts(&d.expr, &def.facts);
                        if let (Some(sc), Some(st)) = (scoped, d.source_table.as_ref()) {
                            e = super::join_resolver::rewrite_to_scoped_alias(&e, st, sc);
                        }
                        e
                    },
                    |(_, expr)| expr.clone(),
                )
        },
    );
    // Snowflake semi-additive semantics (F-1, code-review
    // 2026-07-16): the rows are sorted by the NA dims and the values
    // from the LAST ordering value of that sort are aggregated (with
    // RANK(), every row tied at that value is included) — so the
    // default (ASC) selects the LATEST snapshot and DESC selects the
    // earliest. We pick the snapshot with `RANK() = 1`, which is the
    // FIRST ordering value of this window's ORDER BY, so we emit the
    // REVERSE of the declared direction: the first value of the
    // reversed sort is the last value of the declared sort. NULLS
    // ordering is kept as
    // declared (not reversed) so that under the default (NULLS LAST)
    // a NULL key never outranks a real snapshot — matching the
    // "latest non-NULL wins" intent; declare NULLS FIRST to let a
    // NULL key win.
    let dir = match nd.order {
        SortOrder::Asc => "DESC",
        SortOrder::Desc => "ASC",
    };
    let nulls = match nd.nulls {
        NullsOrder::First => "NULLS FIRST",
        NullsOrder::Last => "NULLS LAST",
    };
    format!("{dim_expr} {dir} {nulls}")
}

/// Build the outer-SELECT aggregate column for one metric over the snapshot
/// CTE.
///
/// An ACTIVE semi-additive metric aggregates only rows at its snapshot rank --
/// `FUNC(CASE WHEN "<rn_col>" = 1 THEN "__sv_semi_<idx>" END)` -- where every
/// row tied at rank 1 contributes (RANK semantics, SG-4). A regular or
/// effectively-regular metric aggregates over all rows: `FUNC("__sv_reg_<idx>")`.
/// The outer re-aggregation for one metric, without an alias — the part both
/// snapshot callers share. An active semi-additive metric re-aggregates only its
/// rank-1 rows; everything else re-aggregates every captured row.
fn outer_metric_expr(
    view_name: &str,
    met: &Metric,
    met_idx: usize,
    agg_func: &str,
    is_active_semi: bool,
    na_groups: &[NaGroup],
) -> Result<String, ExpandError> {
    if is_active_semi {
        let rn_col = get_rn_column_for_metric(view_name, &met.name, met_idx, na_groups)?;
        Ok(format!(
            "{agg_func}(CASE WHEN \"{rn_col}\" = 1 THEN \"__sv_semi_{met_idx}\" END)"
        ))
    } else {
        Ok(format!("{agg_func}(\"__sv_reg_{met_idx}\")"))
    }
}

/// A group of metrics sharing the same NON ADDITIVE BY dimension set.
///
/// Replaces the tuple `(Vec<NonAdditiveDim>, Vec<usize>)` with named fields
/// for readability.
struct NaGroup {
    /// The actual `NonAdditiveDim` entries for this group.
    na_dims: Vec<NonAdditiveDim>,
    /// Role-playing scoped alias for each NA dim, parallel to `na_dims`,
    /// resolved from the group's metrics' `USING` context (`None` when the NA
    /// dim's table is not a role-playing target). Homogeneous across the
    /// group's metrics by construction — it is folded into the group key, so
    /// two metrics whose NA dims resolve to different roles land in different
    /// groups (separate `RANK()` columns).
    na_dim_scoped: Vec<Option<String>>,
    /// Indices into `resolved_mets` that belong to this group.
    metric_indices: Vec<usize>,
}

/// Group metrics by their NON ADDITIVE BY dimension sets.
/// Only includes ACTIVE semi-additive metrics (those with at least one NA dim
/// not in the queried dimensions).
///
/// Returns `Err` if an NA dim on a role-playing table cannot be disambiguated
/// by its metric's `USING` context (propagated from [`na_dim_scoped_alias`]).
fn collect_na_groups(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_mets: &[&Metric],
    queried_dim_keys: &HashSet<String>,
) -> Result<Vec<NaGroup>, ExpandError> {
    // Parallel `keys`/`groups`: a metric joins an existing group when its full
    // key (dims + polarity + role) matches, else it starts a new one. Keeping
    // the key beside the group (rather than inside `NaGroup`) avoids threading a
    // grouping-only field through the struct used downstream.
    let mut keys: Vec<Vec<String>> = Vec::new();
    let mut groups: Vec<NaGroup> = Vec::new();
    for (idx, met) in resolved_mets.iter().enumerate() {
        // Skip regular and effectively-regular metrics (shared routing predicate)
        if !is_active_semi_additive(def, met, queried_dim_keys) {
            continue;
        }
        // Key the group on the resolved dimension keys, each NA dim's sort
        // polarity, AND each NA dim's role-playing scoped alias (#30 for the
        // resolver; polarity added so metrics that share NA dimension names but
        // differ in ASC/DESC or NULLS placement do NOT collapse onto one RANK()
        // column — the group's ORDER BY is taken from the FIRST metric
        // registered, so merging different polarities would silently snapshot
        // the other metric at the wrong end; the scoped alias added so metrics
        // that share NA dims but resolve them to different role-playing
        // instances via different USING clauses likewise stay on separate RANK()
        // columns). Two metrics share a rank column only when their NA dims
        // resolve to the same dimensions in the same order with identical
        // polarity and identical role. `order`/`nulls` are `Copy` enums with a
        // stable `Debug`; the NUL delimiter can never occur inside a normalized
        // identifier key or a scoped alias.
        let scoped: Vec<Option<String>> = met
            .non_additive_by
            .iter()
            .map(|nd| na_dim_scoped_alias(view_name, def, met, &nd.dimension))
            .collect::<Result<_, _>>()?;
        let key: Vec<String> = met
            .non_additive_by
            .iter()
            .zip(&scoped)
            .map(|(nd, sc)| {
                format!(
                    "{}\u{0}{:?}\u{0}{:?}\u{0}{:?}",
                    na_dim_match_key(def, &nd.dimension),
                    nd.order,
                    nd.nulls,
                    sc
                )
            })
            .collect();
        if let Some(pos) = keys.iter().position(|k| *k == key) {
            groups[pos].metric_indices.push(idx);
        } else {
            keys.push(key);
            groups.push(NaGroup {
                na_dims: met.non_additive_by.clone(),
                na_dim_scoped: scoped,
                metric_indices: vec![idx],
            });
        }
    }
    Ok(groups)
}

/// Collect the (lowercased) source-table aliases of every active NA dim,
/// resolved against the view's declared dimensions (SG-9).
///
/// A NA dim that lives on a non-queried, non-base table contributes its
/// source alias here so the snapshot CTE joins that table (the resolver adds
/// path intermediaries); otherwise its raw expression in the snapshot ORDER
/// BY would reference an unjoined table and fail at bind time. Base-table
/// dims (`source_table == None`) and unresolvable names contribute nothing.
///
/// A NA dim that resolved to a role-playing scoped alias (`na_dim_scoped` is
/// `Some`) contributes nothing either: its ORDER BY references the *scoped*
/// instance (`a__dep_airport`), which the owning metric's `USING` clause
/// already joins. Adding the bare table alias here would emit a redundant
/// second join to the same table on whichever edge the resolver picks first —
/// exactly the wrong-role instance the T-15 fix routes the ORDER BY away from.
fn collect_na_dim_source_tables(
    def: &SemanticViewDefinition,
    na_groups: &[NaGroup],
) -> Vec<String> {
    let mut sources: Vec<String> = Vec::new();
    for group in na_groups {
        for (nd, scoped) in group.na_dims.iter().zip(&group.na_dim_scoped) {
            // Role-playing NA dim: the scoped join is emitted by the metric's
            // USING (see the doc comment); don't add a bare-table duplicate.
            if scoped.is_some() {
                continue;
            }
            // #30: resolve bare OR dotted/quoted NA references through the
            // shared resolver so a dotted NA dim on a non-base table still
            // contributes its source table to the snapshot join.
            let Some(dim) = super::resolution::find_dimension(def, &nd.dimension) else {
                continue;
            };
            if let Some(ref st) = dim.source_table {
                let alias = st.to_ascii_lowercase();
                if !sources.contains(&alias) {
                    sources.push(alias);
                }
            }
        }
    }
    sources
}

/// The source tables of a query's active NA dimensions, for the anchor decision
/// in `sql_gen` — which has to be made BEFORE the fan-trap fence runs, and so
/// cannot wait for [`expand_semi_additive`] to build its groups.
///
/// A malformed NA reference yields an empty list rather than an error. That does
/// **not** decline the re-anchor — `snapshot_cte_anchor` cannot tell "no NA
/// tables" from "could not resolve them", and may still return an anchor. It
/// cannot mislead, though: [`expand_semi_additive`] rebuilds the same groups and
/// propagates the real error before the anchor is ever used for emission, so the
/// query fails on the NA reference itself rather than on anything decided here.
pub(super) fn na_dim_source_tables(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_mets: &[&Metric],
    queried_dim_keys: &HashSet<String>,
) -> Vec<String> {
    collect_na_groups(view_name, def, resolved_mets, queried_dim_keys)
        .map(|groups| collect_na_dim_source_tables(def, &groups))
        .unwrap_or_default()
}

/// The source tables of a query's active NA dimensions that resolved to a
/// role-playing scoped alias (`na_dim_scoped` is `Some`), for the anchor
/// decision in `sql_gen`. The snapshot ORDER BY names that scoped instance
/// (`a__r_bal`), which only the base-anchored path joins, as it threads the
/// owning metric's `USING`; a snapshot re-anchored at the metric's own table
/// joined nothing for it, so a query with no dimension on that table failed to
/// bind. Counting these tables as touched makes
/// `per_grain::snapshot_cte_anchor` decline, as it does when such a table is
/// queried.
pub(super) fn na_dim_scoped_source_tables(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_mets: &[&Metric],
    queried_dim_keys: &HashSet<String>,
) -> Vec<String> {
    let Ok(groups) = collect_na_groups(view_name, def, resolved_mets, queried_dim_keys) else {
        return Vec::new();
    };
    let mut sources: Vec<String> = Vec::new();
    for group in &groups {
        for (nd, scoped) in group.na_dims.iter().zip(&group.na_dim_scoped) {
            if scoped.is_none() {
                continue;
            }
            let Some(dim) = super::resolution::find_dimension(def, &nd.dimension) else {
                continue;
            };
            if let Some(ref st) = dim.source_table {
                let alias = st.to_ascii_lowercase();
                if !sources.contains(&alias) {
                    sources.push(alias);
                }
            }
        }
    }
    sources
}

/// A `(metric index, NA dimension name, lowercased source table)` triple —
/// the index being into `resolved_mets` —
/// for every ACTIVE NA dimension that contributes a **bare** join to the
/// snapshot CTE — the same entries [`collect_na_dim_source_tables`] turns into
/// join targets, paired back to the metrics whose rows that join multiplies.
///
/// EXP-9 (code-review 2026-08-03): the fan-trap fence pairs each metric against
/// the *queried* dimensions, but the table the snapshot joins because of an
/// **un-queried** NA dimension is joined exactly like a queried dimension's and
/// fans the metric's grain exactly the same way. Inside the snapshot that is
/// worse than an inflated `SUM`: `RANK` ties across the fanned duplicates of one
/// source row are indistinguishable from ties across distinct rows, so the CTE
/// cannot dedupe them and the metric is added once per duplicate. Handing the
/// pairs to [`crate::expand::fan_trap::check_fan_traps`] lets it apply the check
/// it already applies to dimensions, and name the NA dimension in the error.
///
/// Role-scoped NA dims (`na_dim_scoped` is `Some`) are excluded for the same
/// reason they contribute no bare join: their ORDER BY references the instance
/// the owning metric's `USING` already joins (T-15).
///
/// A malformed NA reference yields an empty list rather than an error, matching
/// [`na_dim_source_tables`] — [`expand_semi_additive`] rebuilds the same groups
/// and reports the real error before emission.
pub(super) fn na_dim_fence_members(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_mets: &[&Metric],
    queried_dim_keys: &HashSet<String>,
) -> Vec<NaFenceMember> {
    let Ok(groups) = collect_na_groups(view_name, def, resolved_mets, queried_dim_keys) else {
        return Vec::new();
    };
    let mut members = Vec::new();
    for group in &groups {
        for (nd, scoped) in group.na_dims.iter().zip(&group.na_dim_scoped) {
            if scoped.is_some() {
                continue;
            }
            let Some(dim) = super::resolution::find_dimension(def, &nd.dimension) else {
                continue;
            };
            let Some(ref st) = dim.source_table else {
                continue; // Base-table NA dim: nothing extra is joined.
            };
            for &met_idx in &group.metric_indices {
                members.push(NaFenceMember {
                    metric_idx: met_idx,
                    dim_name: dim.name.clone(),
                    source_table_raw: st.clone(),
                    source_table_key: st.to_ascii_lowercase(),
                });
            }
        }
    }
    members
}

/// One active NA dimension paired with a metric whose rows its join multiplies
/// (EXP-9). See [`na_dim_fence_members`].
pub(super) struct NaFenceMember {
    /// Index into the `resolved_mets` slice passed to `na_dim_fence_members`.
    pub(super) metric_idx: usize,
    /// The NA dimension's declared name, for the error message.
    pub(super) dim_name: String,
    /// The source table **as declared**, preserving case and quoting. Used for
    /// the error payload so it reads like the definition the user wrote — the
    /// spelling the queried-dimension branch of the fence already reports.
    pub(super) source_table_raw: String,
    /// The lowercased alias, for path and cardinality lookups (which are keyed
    /// on the folded spelling throughout the graph).
    pub(super) source_table_key: String,
}

/// Aggregate functions the snapshot CTE knows how to decompose into an
/// inner-expression capture (CTE column) plus an outer re-aggregation.
const SNAPSHOT_AGG_FUNCS: [&str; 5] = ["SUM", "COUNT", "AVG", "MIN", "MAX"];

/// Validate and decompose a metric expression for the snapshot CTE (SG-5).
///
/// Accepts exactly one top-level aggregate call `FUNC(args)` where `FUNC` is
/// SUM/COUNT/AVG/MIN/MAX (any case), `args` is neither `*` nor
/// DISTINCT-qualified, and no expression text precedes or follows the call.
/// Returns `(FUNC as written, inner argument text)` on success, or a
/// human-readable reason why the expression cannot be decomposed.
///
/// The matching-paren scan is parenthesis-depth and quote aware
/// (single-quoted SQL strings, double-quoted identifiers), so arguments
/// containing parens or quotes classify correctly.
fn parse_snapshot_aggregate(expr: &str) -> Result<(String, String), String> {
    let trimmed = expr.trim();
    let Some(open) = trimmed.find('(') else {
        return Err("the expression is not an aggregate function call".to_string());
    };
    let func_name = trimmed[..open].trim();
    if func_name.is_empty()
        || !func_name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(
            "the expression does not start with a single aggregate function call".to_string(),
        );
    }
    if !SNAPSHOT_AGG_FUNCS.contains(&func_name.to_ascii_uppercase().as_str()) {
        return Err(format!(
            "aggregate function '{func_name}' cannot be decomposed for snapshot aggregation \
             (supported: SUM, COUNT, AVG, MIN, MAX)"
        ));
    }
    let Some(close) = find_matching_paren(trimmed, open) else {
        return Err("unbalanced parentheses in the expression".to_string());
    };
    let trailing = trimmed[close + 1..].trim();
    if !trailing.is_empty() {
        return Err(format!(
            "expression text follows the aggregate call: '{trailing}'"
        ));
    }
    let inner = trimmed[open + 1..close].trim();
    if inner.is_empty() {
        return Err("the aggregate call has no argument".to_string());
    }
    if inner == "*" {
        return Err("star aggregates like COUNT(*) are not supported".to_string());
    }
    if starts_with_distinct_keyword(inner) {
        return Err("DISTINCT aggregates are not supported".to_string());
    }
    Ok((func_name.to_string(), inner.to_string()))
}

/// Byte index of the `)` matching the `(` at byte offset `open`, or `None`
/// when unbalanced. Parens inside single-quoted strings, double-quoted
/// identifiers and `$tag$` literals are ignored, along with the doubled-quote
/// escapes (`''`, `""`).
///
/// EXP-17: this was a hand-rolled `Mode::{Normal,SingleQuote,DoubleQuote}`
/// matcher that never learned `$tag$` (which parse-side scanners gained in
/// PARSE-1), so a `)` inside a dollar-quoted literal closed the match early and
/// mis-sliced the aggregate's argument. It now defers to [`QuoteState`], the
/// crate's one scanner.
pub(super) fn find_matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut quotes = crate::util::QuoteState::default();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        let (next, is_live_code) = quotes.step(bytes, i);
        if is_live_code {
            match bytes[i] {
                b'(' => depth += 1,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        i = next;
    }
    None
}

/// True when `inner` begins with the `DISTINCT` keyword at a word boundary
/// (case-insensitive). UTF-8 safe: `get(..8)` returns `None` rather than
/// panicking when byte 8 is not a char boundary.
fn starts_with_distinct_keyword(inner: &str) -> bool {
    let Some(prefix) = inner.get(..8) else {
        return false;
    };
    prefix.eq_ignore_ascii_case("DISTINCT")
        && inner[8..]
            .chars()
            .next()
            .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
}

/// Get the snapshot rank column name (`__sv_rn` / `__sv_rn_N`) for a given
/// metric index, erroring when the metric is in no group.
///
/// The naming must agree with the aliases `build_snapshot_block` emits for the
/// `RANK()` columns: unsuffixed when there is a single group, `_N` (1-based)
/// otherwise.
///
/// EXP-18 (code review 2026-08-04): a metric index in NO group used to return
/// `"__sv_rn"` under a bare `// fallback` comment — group 1's rank column. The
/// metric would then be snapshotted at another group's NA ordering and return a
/// wrong number with no error, which is the exact failure #129/#32 were filed
/// for; `collect_na_groups` folds polarity and role into the group key precisely
/// to keep two metrics off one rank column. The `na_groups.len() == 1` shortcut
/// was the wider hole of the two: it returned before testing membership at all,
/// so in the common single-group case ANY stray index was aliased.
///
/// A caller cannot provoke this — `collect_na_groups` and the `is_active_semi`
/// closure share `is_active_semi_additive` over the same `resolved_mets` index
/// space, so an active semi-additive metric always has a group. The error exists
/// so a future divergence between those two sides fails loudly rather than
/// silently changing a number.
fn get_rn_column_for_metric(
    view_name: &str,
    met_name: &str,
    met_idx: usize,
    na_groups: &[NaGroup],
) -> Result<String, ExpandError> {
    for (group_idx, group) in na_groups.iter().enumerate() {
        if group.metric_indices.contains(&met_idx) {
            return Ok(if na_groups.len() == 1 {
                "__sv_rn".to_string()
            } else {
                format!("__sv_rn_{}", group_idx + 1)
            });
        }
    }
    Err(ExpandError::SemiAdditiveRankColumnUnresolved {
        view_name: view_name.to_string(),
        metric_name: met_name.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use crate::expand::test_helpers::{minimal_def, orders_view, TestFixtureExt};
    use crate::expand::{expand, DimensionName, ExpandError, MetricName, QueryRequest};
    use crate::model::{NullsOrder, SortOrder};

    use super::{find_matching_paren, get_rn_column_for_metric, parse_snapshot_aggregate, NaGroup};

    /// EXP-18 (code review 2026-08-04): a metric index belonging to NO NA group
    /// used to get `"__sv_rn"` back under a bare `// fallback` comment — group
    /// 1's rank column. The metric is then snapshotted at another group's NA
    /// ordering and returns a wrong number with no error, which is exactly the
    /// failure #129/#32 were filed for; the group key carries polarity and role
    /// precisely to keep two metrics off one rank column.
    ///
    /// The invariant holds today — `collect_na_groups` and the `is_active_semi`
    /// closure share `is_active_semi_additive` over the same `resolved_mets`
    /// index space, so every metric reaching here has a group — so this is a
    /// guard on a *currently unreachable* state, not a live wrong-number bug.
    /// Its value is that a future divergence between those two sides fails loud
    /// instead of silently aliasing rank columns.
    /// The two silent-alias paths get one test each, so a break in either
    /// reports on its own instead of the first halting the run before the
    /// second is reached.
    fn na_group(indices: Vec<usize>) -> NaGroup {
        NaGroup {
            na_dims: vec![],
            na_dim_scoped: vec![],
            metric_indices: indices,
        }
    }

    /// EXP-18, the wider of the two holes: `na_groups.len() == 1` returned
    /// `"__sv_rn"` BEFORE testing membership, so in the common single-group case
    /// any stray index was aliased onto that group's rank column without ever
    /// reaching the `// fallback` line.
    #[test]
    fn test_get_rn_column_single_group_rejects_non_member() {
        let one = vec![na_group(vec![0])];
        // Control: the actual member still gets the unsuffixed name, which is
        // what `build_snapshot_block` aliases the lone RANK() column to.
        assert_eq!(
            get_rn_column_for_metric("v", "m0", 0, &one).unwrap(),
            "__sv_rn"
        );
        assert!(
            matches!(
                get_rn_column_for_metric("v", "m1", 1, &one),
                Err(ExpandError::SemiAdditiveRankColumnUnresolved { .. })
            ),
            "a metric in no NA group must not alias onto the group's rank column"
        );
    }

    /// EXP-18, the `// fallback` line itself: with several groups, an index in
    /// none of them fell off the loop and took group 1's column.
    #[test]
    fn test_get_rn_column_multi_group_rejects_non_member() {
        let two = vec![na_group(vec![0]), na_group(vec![1])];
        // Controls: members keep their own 1-based suffixed columns.
        assert_eq!(
            get_rn_column_for_metric("v", "m0", 0, &two).unwrap(),
            "__sv_rn_1"
        );
        assert_eq!(
            get_rn_column_for_metric("v", "m1", 1, &two).unwrap(),
            "__sv_rn_2"
        );
        assert!(
            matches!(
                get_rn_column_for_metric("v", "m2", 2, &two),
                Err(ExpandError::SemiAdditiveRankColumnUnresolved { .. })
            ),
            "the post-loop fallback must error, not return __sv_rn"
        );
    }

    /// EXP-17: this matcher knew `'…'` and `"…"` but not `$tag$…$tag$`, so a
    /// `)` inside a dollar-quoted literal closed the match early and
    /// `build_snapshot_block`'s SG-5 decomposition mis-sliced the aggregate's
    /// argument. Parse-side scanners learned `$tag$` in PARSE-1; this one did not.
    #[test]
    fn find_matching_paren_ignores_a_close_paren_inside_a_dollar_quoted_literal() {
        // The `)` at index 10 is literal text, not the closing paren.
        let s = "sum($tag$a)b$tag$)";
        assert_eq!(find_matching_paren(s, 3), Some(s.len() - 1));

        // Untagged `$$…$$` form.
        let s = "sum($$a)b$$)";
        assert_eq!(find_matching_paren(s, 3), Some(s.len() - 1));
    }

    /// Control: the regions this matcher already handled must keep working.
    #[test]
    fn find_matching_paren_still_ignores_quoted_regions_and_nests() {
        let s = "sum('a)b')";
        assert_eq!(find_matching_paren(s, 3), Some(s.len() - 1));
        let s = "sum(\"a)b\")";
        assert_eq!(find_matching_paren(s, 3), Some(s.len() - 1));
        let s = "sum(coalesce(a, b))";
        assert_eq!(find_matching_paren(s, 3), Some(s.len() - 1));
    }

    /// Single table, one semi-additive metric with NA dim NOT in query.
    /// Expects CTE with RANK and conditional aggregation.
    #[test]
    fn test_semi_additive_single_metric_single_dim() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_snapshot AS"), "SQL: {sql}");
        assert!(sql.contains("RANK() OVER"), "Should contain RANK: {sql}");
        assert!(
            !sql.contains("ROW_NUMBER"),
            "ROW_NUMBER drops snapshot ties (SG-4): {sql}"
        );
        // E-1: the window clause repeats the dimension EXPRESSION, never the
        // CTE alias (an alias matching a physical column binds to the column).
        assert!(
            sql.contains("PARTITION BY customer_id"),
            "Should partition by queried dim expression: {sql}"
        );
        // F-1: the RANK ORDER BY reverses the declared direction (keeps NULLS),
        // so a `DESC NULLS FIRST` declaration emits `ASC NULLS FIRST`.
        assert!(
            sql.contains("ASC NULLS FIRST"),
            "Declared DESC must emit reversed ASC in the RANK ORDER BY: {sql}"
        );
        assert!(
            sql.contains("CASE WHEN \"__sv_rn\" = 1"),
            "Should have CASE WHEN for semi-additive: {sql}"
        );
        assert!(
            sql.contains("\"__sv_semi_0\""),
            "Should have semi-additive CTE alias: {sql}"
        );
    }

    /// E-1 regression (code-review 2026-07-11): a dimension whose expression
    /// differs from its bare column must be repeated as an EXPRESSION in the
    /// RANK() PARTITION BY. Referencing the CTE alias instead binds to the
    /// same-named physical column (DuckDB resolves window-clause identifiers
    /// to FROM-clause columns before lateral select aliases), silently
    /// partitioning on the raw column: `upper(region) AS region` over rows
    /// 'us'/'US' produced two partitions and summed both snapshot rows.
    #[test]
    fn test_semi_additive_expr_dim_repeats_expression_not_alias() {
        let def = minimal_def(
            "accounts",
            "region",
            "upper(region)",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("region")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("PARTITION BY upper(region)"),
            "PARTITION BY must repeat the dimension expression: {sql}"
        );
        assert!(
            !sql.contains("PARTITION BY \"region\""),
            "PARTITION BY must not reference the CTE alias: {sql}"
        );
    }

    /// E-1 regression, ORDER BY arm: a QUERIED NA dim (mixed NA group where
    /// another NA dim is absent from the query, keeping the metric active)
    /// must also be ordered by its expression, not its CTE alias.
    #[test]
    fn test_semi_additive_queried_na_dim_orders_by_expression() {
        let def = minimal_def(
            "accounts",
            "region",
            "upper(region)",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[
                ("report_date", SortOrder::Desc, NullsOrder::First),
                ("region", SortOrder::Asc, NullsOrder::Last),
            ],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("region")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        // F-1: `region ASC NULLS LAST` declared → `DESC NULLS LAST` emitted
        // (direction reversed, NULLS kept); the point of this test is that the
        // EXPRESSION (not the CTE alias) is what gets ordered.
        assert!(
            sql.contains("upper(region) DESC NULLS LAST"),
            "queried NA dim must order by its expression, not its alias: {sql}"
        );
        assert!(
            !sql.contains("\"region\" ASC"),
            "queried NA dim must not order by the CTE alias: {sql}"
        );
    }

    /// Semi-additive metric with ALL NA dims in query -> effectively regular.
    /// No CTE should be generated.
    #[test]
    fn test_effectively_regular_all_na_dims_in_query() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("customer_id"),
                DimensionName::new("report_date"),
            ],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            !sql.contains("WITH __sv_snapshot"),
            "Should NOT have CTE when all NA dims in query: {sql}"
        );
        assert!(!sql.contains("RANK"), "Should NOT have RANK: {sql}");
        // Standard expansion path
        assert!(
            sql.contains("GROUP BY"),
            "Should have standard GROUP BY: {sql}"
        );
    }

    /// NA dim NOT in query -> CTE generated with PARTITION BY queried dims.
    #[test]
    fn test_semi_additive_na_dim_not_in_query() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("PARTITION BY customer_id"),
            "Should partition by queried dim expression (E-1): {sql}"
        );
        // report_date is the NA dim, should appear in ORDER BY with its
        // expression. F-1: declared `DESC NULLS FIRST` emits reversed
        // `ASC NULLS FIRST`.
        assert!(
            sql.contains("ORDER BY report_date ASC NULLS FIRST"),
            "Should order by NA dim expression (direction reversed per F-1): {sql}"
        );
    }

    /// Mixed regular + semi-additive metrics.
    #[test]
    fn test_mixed_regular_and_semi_additive() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "amount",
            "SUM(amount)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("balance", "SUM(balance)", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("amount"), MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_snapshot"), "Should have CTE: {sql}");
        // Regular metric gets __sv_reg_0 alias
        assert!(
            sql.contains("\"__sv_reg_0\""),
            "Regular metric should have reg alias: {sql}"
        );
        // Semi-additive metric gets __sv_semi_1 alias
        assert!(
            sql.contains("\"__sv_semi_1\""),
            "Semi-additive metric should have semi alias: {sql}"
        );
        // Outer SELECT: regular uses plain aggregation
        assert!(
            sql.contains("SUM(\"__sv_reg_0\")"),
            "Regular metric should use plain SUM: {sql}"
        );
        // Outer SELECT: semi-additive uses CASE WHEN
        assert!(
            sql.contains("SUM(CASE WHEN \"__sv_rn\" = 1 THEN \"__sv_semi_1\" END)"),
            "Semi-additive should use CASE WHEN: {sql}"
        );
    }

    /// Regular-only metrics -> no CTE, flat SELECT.
    #[test]
    fn test_no_semi_additive_no_cte() {
        let def = orders_view();
        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("region")],
            metrics: vec![MetricName::new("total_revenue")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            !sql.contains("WITH __sv_snapshot"),
            "Regular-only should NOT have CTE: {sql}"
        );
        assert!(!sql.contains("RANK"), "Regular-only should NOT rank: {sql}");
    }

    /// SG-5: shapes the snapshot CTE can decompose.
    #[test]
    fn test_parse_snapshot_aggregate_accepts_single_aggregate_calls() {
        assert_eq!(
            parse_snapshot_aggregate("SUM(a.balance)"),
            Ok(("SUM".to_string(), "a.balance".to_string()))
        );
        // Function case is preserved so previously-valid emissions stay
        // byte-identical.
        assert_eq!(
            parse_snapshot_aggregate("sum(amount)"),
            Ok(("sum".to_string(), "amount".to_string()))
        );
        assert_eq!(
            parse_snapshot_aggregate("AVG( amount )"),
            Ok(("AVG".to_string(), "amount".to_string()))
        );
        assert_eq!(
            parse_snapshot_aggregate("COUNT(id)"),
            Ok(("COUNT".to_string(), "id".to_string()))
        );
        assert_eq!(
            parse_snapshot_aggregate("MIN(price)"),
            Ok(("MIN".to_string(), "price".to_string()))
        );
        assert_eq!(
            parse_snapshot_aggregate("MAX(price)"),
            Ok(("MAX".to_string(), "price".to_string()))
        );
        // Parens inside a single-quoted string literal do not fool the scan.
        assert_eq!(
            parse_snapshot_aggregate("SUM(CASE WHEN note = ')' THEN amount ELSE 0 END)"),
            Ok((
                "SUM".to_string(),
                "CASE WHEN note = ')' THEN amount ELSE 0 END".to_string()
            ))
        );
        // Parens inside a double-quoted identifier do not fool the scan.
        assert_eq!(
            parse_snapshot_aggregate("SUM(\"weird)col\")"),
            Ok(("SUM".to_string(), "\"weird)col\"".to_string()))
        );
        // A column merely named like DISTINCT is not the DISTINCT keyword.
        assert_eq!(
            parse_snapshot_aggregate("SUM(distinctive_col)"),
            Ok(("SUM".to_string(), "distinctive_col".to_string()))
        );
    }

    /// SG-5: every previously-silently-mangled shape must now be rejected
    /// with a reason.
    #[test]
    fn test_parse_snapshot_aggregate_rejects_undecomposable_shapes() {
        // Arithmetic-wrapped: the `* 0.1` was silently DROPPED before.
        let err = parse_snapshot_aggregate("SUM(amount) * 0.1").unwrap_err();
        assert!(err.contains("follows the aggregate call"), "got: {err}");
        // COUNT(*): the `*` was emitted as a broken star-alias CTE column.
        let err = parse_snapshot_aggregate("COUNT(*)").unwrap_err();
        assert!(err.contains("star aggregates"), "got: {err}");
        // DISTINCT (either case).
        let err = parse_snapshot_aggregate("COUNT(DISTINCT x)").unwrap_err();
        assert!(err.contains("DISTINCT"), "got: {err}");
        let err = parse_snapshot_aggregate("count(distinct x)").unwrap_err();
        assert!(err.contains("DISTINCT"), "got: {err}");
        // COALESCE-wrapped: only the outermost call is considered.
        let err = parse_snapshot_aggregate("COALESCE(SUM(x), 0)").unwrap_err();
        assert!(err.contains("cannot be decomposed"), "got: {err}");
        // Multi-aggregate (inlined derived metric shape).
        let err = parse_snapshot_aggregate("SUM(revenue) - SUM(cost)").unwrap_err();
        assert!(err.contains("follows the aggregate call"), "got: {err}");
        // Leading expression text.
        let err = parse_snapshot_aggregate("1 + SUM(x)").unwrap_err();
        assert!(
            err.contains("does not start with a single aggregate"),
            "got: {err}"
        );
        // Not an aggregate at all (previously fell back to a hardcoded SUM).
        let err = parse_snapshot_aggregate("revenue - cost").unwrap_err();
        assert!(err.contains("not an aggregate function call"), "got: {err}");
        let err = parse_snapshot_aggregate("42").unwrap_err();
        assert!(err.contains("not an aggregate function call"), "got: {err}");
        // Unsupported aggregate function.
        let err = parse_snapshot_aggregate("STRING_AGG(x, ',')").unwrap_err();
        assert!(err.contains("cannot be decomposed"), "got: {err}");
        // Malformed input never panics.
        let err = parse_snapshot_aggregate("SUM(").unwrap_err();
        assert!(err.contains("unbalanced parentheses"), "got: {err}");
        let err = parse_snapshot_aggregate("SUM()").unwrap_err();
        assert!(err.contains("no argument"), "got: {err}");
    }

    /// SG-5: a regular metric with an arithmetic-wrapped aggregate co-queried
    /// with an active semi-additive metric errors instead of silently
    /// dropping the arithmetic.
    #[test]
    fn test_co_query_arithmetic_wrapped_metric_errors() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("discounted", "SUM(amount) * 0.1", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("discounted"), MetricName::new("balance")],
        };

        let result = expand("test_view", &def, &req);
        match result {
            Err(ExpandError::SemiAdditiveCoQueryUnsupported {
                metric_name,
                semi_metric_name,
                ..
            }) => {
                assert_eq!(metric_name, "discounted");
                assert_eq!(semi_metric_name, "balance");
            }
            other => panic!("expected SemiAdditiveCoQueryUnsupported, got: {other:?}"),
        }
    }

    /// SG-5: `COUNT(*)` co-queried with an active semi-additive metric errors
    /// instead of emitting a star-alias CTE column that COUNTs an arbitrary
    /// (NULL-sensitive) join column.
    #[test]
    fn test_co_query_count_star_errors() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("row_count", "COUNT(*)", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("row_count"), MetricName::new("balance")],
        };

        let result = expand("test_view", &def, &req);
        assert!(
            matches!(
                result,
                Err(ExpandError::SemiAdditiveCoQueryUnsupported { .. })
            ),
            "COUNT(*) co-query must error, got: {result:?}"
        );
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("cannot be co-queried with semi-additive metric 'balance'"),
            "message should name both metrics: {msg}"
        );
        assert!(
            msg.contains("separately"),
            "message should suggest querying separately: {msg}"
        );
    }

    /// SG-5: DISTINCT aggregate co-query errors.
    #[test]
    fn test_co_query_count_distinct_errors() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("uniq_customers", "COUNT(DISTINCT customer_id)", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![
                MetricName::new("uniq_customers"),
                MetricName::new("balance"),
            ],
        };

        let result = expand("test_view", &def, &req);
        assert!(
            matches!(
                result,
                Err(ExpandError::SemiAdditiveCoQueryUnsupported { .. })
            ),
            "COUNT(DISTINCT ...) co-query must error, got: {result:?}"
        );
    }

    /// SG-5: COALESCE-wrapped aggregate co-query errors.
    #[test]
    fn test_co_query_coalesce_wrapped_errors() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("safe_total", "COALESCE(SUM(amount), 0)", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("safe_total"), MetricName::new("balance")],
        };

        let result = expand("test_view", &def, &req);
        assert!(
            matches!(
                result,
                Err(ExpandError::SemiAdditiveCoQueryUnsupported { .. })
            ),
            "COALESCE-wrapped co-query must error, got: {result:?}"
        );
    }

    /// SG-5: a derived metric (inlines to a multi-aggregate expression)
    /// co-queried with an active semi-additive metric errors instead of
    /// falling back to a hardcoded SUM over a mangled column.
    #[test]
    fn test_co_query_derived_metric_errors() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_metric("revenue", "SUM(amount)", None)
        .with_metric("cost_total", "SUM(cost)", None)
        .with_metric("profit", "revenue - cost_total", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("profit"), MetricName::new("balance")],
        };

        let result = expand("test_view", &def, &req);
        match result {
            Err(ExpandError::SemiAdditiveCoQueryUnsupported {
                metric_name,
                metric_expr,
                ..
            }) => {
                assert_eq!(metric_name, "profit");
                // The error carries the INLINED expression the CTE would
                // have had to decompose.
                assert!(
                    metric_expr.contains("SUM(amount)") && metric_expr.contains("SUM(cost)"),
                    "expected inlined derived expression, got: {metric_expr}"
                );
            }
            other => panic!("expected SemiAdditiveCoQueryUnsupported, got: {other:?}"),
        }
    }

    /// SG-5: the active semi-additive metric's OWN expression is validated
    /// too — an arithmetic-wrapped NON ADDITIVE BY metric errors instead of
    /// silently dropping the arithmetic.
    #[test]
    fn test_semi_additive_metric_own_expression_validated() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance) * 2",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let result = expand("test_view", &def, &req);
        match result {
            Err(ExpandError::SemiAdditiveUnsupportedExpression { metric_name, .. }) => {
                assert_eq!(metric_name, "balance");
            }
            other => panic!("expected SemiAdditiveUnsupportedExpression, got: {other:?}"),
        }
    }

    /// SG-9: a NON ADDITIVE BY dim living on a non-queried, non-base table
    /// must get its source table joined into the snapshot CTE — otherwise the
    /// snapshot ORDER BY references an unjoined alias and fails at bind time.
    /// Topology: base `a AS accounts` with `a(date_id) REFERENCES d`, NA dim
    /// `report_date = d.report_date`, query by a BASE dim only.
    #[test]
    fn test_na_dim_on_non_queried_table_joins_its_source() {
        let mut def = minimal_def(
            "accounts",
            "customer_id",
            "a.customer_id",
            "balance",
            "SUM(a.balance)",
        );
        def.tables[0].alias = "a".to_string();
        def.tables[0].pk_columns = vec!["id".to_string()];
        def.metrics[0].source_table = Some("a".to_string());
        let def = def
            .with_table("d", "dates", &["id"])
            .with_dimension("report_date", "d.report_date", Some("d"))
            .with_non_additive_by(
                "balance",
                &[("report_date", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join("acct_date", "a", "d", &["date_id"], &["id"]);

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("LEFT JOIN \"dates\" AS \"d\" ON \"a\".\"date_id\" = \"d\".\"id\""),
            "Snapshot CTE must join the NA dim's source table: {sql}"
        );
        // F-1: declared `DESC NULLS FIRST` emits reversed `ASC NULLS FIRST`.
        assert!(
            sql.contains("ORDER BY d.report_date ASC NULLS FIRST"),
            "Snapshot ORDER BY must reference the joined alias: {sql}"
        );
    }

    /// #30: a DOTTED NON ADDITIVE BY reference (`d.report_date`) to a
    /// non-queried NA dim must resolve to that declared dimension and order by
    /// its EXPRESSION (`d.report_date`), never the raw dotted text emitted as a
    /// quoted non-column (`"d.report_date"`). Before #30 the bare-name `.find`
    /// missed the dotted reference and emitted the quoted qualifier, which
    /// failed at bind time.
    #[test]
    fn test_dotted_na_dim_orders_by_resolved_expression() {
        let mut def = minimal_def(
            "accounts",
            "customer_id",
            "a.customer_id",
            "balance",
            "SUM(a.balance)",
        );
        def.tables[0].alias = "a".to_string();
        def.tables[0].pk_columns = vec!["id".to_string()];
        def.metrics[0].source_table = Some("a".to_string());
        let def = def
            .with_table("d", "dates", &["id"])
            .with_dimension("report_date", "d.report_date", Some("d"))
            // DOTTED reference — the crux of #30.
            .with_non_additive_by(
                "balance",
                &[("d.report_date", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join("acct_date", "a", "d", &["date_id"], &["id"]);

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_snapshot"), "Should have CTE: {sql}");
        // F-1: declared `DESC NULLS FIRST` emits reversed `ASC NULLS FIRST`,
        // ordering by the resolved dimension EXPRESSION.
        assert!(
            sql.contains("ORDER BY d.report_date ASC NULLS FIRST"),
            "dotted NA dim must order by its resolved expression: {sql}"
        );
        // The raw dotted text must NOT be emitted as a quoted non-column.
        assert!(
            !sql.contains("\"d.report_date\""),
            "dotted NA text must not leak as a quoted non-column: {sql}"
        );
        // SG-9: the NA dim's source table is still joined.
        assert!(
            sql.contains("LEFT JOIN \"dates\" AS \"d\""),
            "dotted NA dim source table must be joined: {sql}"
        );
    }

    /// #30: a DOTTED NON ADDITIVE BY reference whose dim IS queried classifies
    /// as effectively-regular (Snowflake semantics) — no snapshot CTE. Before
    /// #30 the dotted reference never matched the queried bare dim name, so the
    /// metric was wrongly kept active and took the CTE path.
    #[test]
    fn test_dotted_na_dim_queried_is_effectively_regular() {
        let mut def = minimal_def(
            "accounts",
            "customer_id",
            "a.customer_id",
            "balance",
            "SUM(a.balance)",
        );
        def.tables[0].alias = "a".to_string();
        def.metrics[0].source_table = Some("a".to_string());
        let def = def
            .with_dimension("report_date", "a.report_date", Some("a"))
            .with_non_additive_by(
                "balance",
                &[("a.report_date", SortOrder::Desc, NullsOrder::First)],
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("customer_id"),
                // The dotted NA dim IS queried (by its bare name).
                DimensionName::new("report_date"),
            ],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            !sql.contains("WITH __sv_snapshot"),
            "queried dotted NA dim must be effectively regular (no CTE): {sql}"
        );
        assert!(!sql.contains("RANK"), "Should NOT have RANK: {sql}");
        assert!(
            sql.contains("GROUP BY"),
            "Should take the standard aggregation path: {sql}"
        );
    }

    /// #30 unit-level: the shared routing predicate resolves a dotted/quoted NA
    /// reference against the queried dimension keys.
    #[test]
    fn test_is_active_semi_additive_resolves_dotted_reference() {
        use super::is_active_semi_additive;
        use std::collections::HashSet;

        let mut def = minimal_def(
            "accounts",
            "customer_id",
            "a.customer_id",
            "balance",
            "SUM(a.balance)",
        );
        def.tables[0].alias = "a".to_string();
        def.metrics[0].source_table = Some("a".to_string());
        let def = def
            .with_dimension("report_date", "a.report_date", Some("a"))
            .with_non_additive_by(
                "balance",
                &[("a.report_date", SortOrder::Desc, NullsOrder::First)],
            );
        let met = &def.metrics[0];

        // report_date queried (by bare name) → dotted NA resolves to it → NOT active.
        let queried: HashSet<String> = ["customer_id", "report_date"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        assert!(
            !is_active_semi_additive(&def, met, &queried),
            "dotted NA dim present in query must classify effectively-regular"
        );

        // report_date absent → active.
        let queried_without: HashSet<String> =
            ["customer_id"].iter().map(|s| (*s).to_string()).collect();
        assert!(
            is_active_semi_additive(&def, met, &queried_without),
            "dotted NA dim absent from query must classify active"
        );
    }

    /// Metrics-only (no dims) semi-additive -> global aggregate with CTE.
    #[test]
    fn test_metrics_only_global_aggregate() {
        let def = minimal_def(
            "accounts",
            "report_date",
            "report_date",
            "balance",
            "SUM(balance)",
        )
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("WITH __sv_snapshot"),
            "Global aggregate should have CTE: {sql}"
        );
        // No PARTITION BY when no dims
        assert!(
            !sql.contains("PARTITION BY"),
            "No dims -> no PARTITION BY: {sql}"
        );
        // No GROUP BY when no dims
        assert!(!sql.contains("GROUP BY"), "No dims -> no GROUP BY: {sql}");
        assert!(
            sql.contains("CASE WHEN \"__sv_rn\" = 1"),
            "Should have CASE WHEN: {sql}"
        );
    }

    /// SG-6 (code review 2026-07-02): a semi-additive metric whose NA dims
    /// are ALL in the queried dimensions is "effectively regular" — it takes
    /// the standard aggregation path (never the snapshot CTE), so it MUST
    /// get the standard fan-trap check. The previous unconditional
    /// `non_additive_by`-non-empty skip let this query silently inflate.
    /// (The CTE-path skip is pinned by
    /// `fan_trap::tests::test_check_fan_traps_semi_additive_cte_path_skipped`.)
    #[test]
    fn test_fan_trap_checks_effectively_regular_semi_additive() {
        // Multi-table view where the metric on table c queried with a dim on
        // table a causes a fan trap (a -> c is many-to-one; the dim on a
        // means traversing c -> a, the fan-out direction). The metric's only
        // NA dim (acct_name) IS queried, so it acts as a regular aggregate.
        let def = orders_view()
            .with_table("customers", "customers", &[])
            .clear_dimensions()
            .clear_metrics()
            .with_table("c", "customers", &["id"])
            .with_table("a", "accounts", &["id"])
            .with_dimension("acct_name", "a.name", Some("a"))
            .with_metric("total_balance", "SUM(a.balance)", Some("c"))
            .with_non_additive_by(
                "total_balance",
                &[("acct_name", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join("cust_acct", "a", "c", &["customer_id"], &["id"]);

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("acct_name")],
            metrics: vec![MetricName::new("total_balance")],
        };

        // Effectively-regular semi-additive metrics get the standard check.
        let result = expand("test_view", &def, &req);
        assert!(
            matches!(result, Err(ExpandError::FanTrap { .. })),
            "Effectively-regular semi-additive metric over a fanning join \
             must be a fan trap error, got: {result:?}"
        );
    }

    /// Multi-table JOIN in CTE -- semi-additive metric on joined table.
    #[test]
    fn test_semi_additive_multi_table_join() {
        let def = orders_view()
            .with_table("accounts", "accounts", &[])
            .clear_dimensions()
            .clear_metrics()
            .with_table("a", "accounts", &["id"])
            .with_table("c", "customers", &["id"])
            .with_dimension("customer_name", "c.name", Some("c"))
            .with_dimension("report_date", "a.report_date", Some("a"))
            .with_metric("total_balance", "SUM(a.balance)", Some("a"))
            .with_non_additive_by(
                "total_balance",
                &[("report_date", SortOrder::Desc, NullsOrder::First)],
            )
            .with_pkfk_join("acct_cust", "a", "c", &["customer_id"], &["id"]);

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_name")],
            metrics: vec![MetricName::new("total_balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_snapshot"), "Should have CTE: {sql}");
        assert!(sql.contains("LEFT JOIN"), "CTE should include JOIN: {sql}");
        assert!(
            sql.contains("PARTITION BY c.name"),
            "Should partition by the customer_name expression (E-1): {sql}"
        );
    }

    /// F-1: a declared `DESC NULLS FIRST` NA dim emits the reversed
    /// `ASC NULLS FIRST` in the RANK ORDER BY (so `RANK() = 1` lands on the last
    /// row of the declared DESC sort = the earliest snapshot).
    #[test]
    fn test_desc_nulls_first_emits_reversed_asc() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("ASC NULLS FIRST"),
            "Declared DESC must emit reversed ASC in the RANK ORDER BY: {sql}"
        );
        assert!(
            !sql.contains("DESC NULLS FIRST"),
            "Declared direction must not leak un-reversed into the RANK ORDER BY: {sql}"
        );
    }

    /// F-1: a declared `ASC NULLS LAST` NA dim (the default direction) emits the
    /// reversed `DESC NULLS LAST` in the RANK ORDER BY (so `RANK() = 1` lands on
    /// the last row of the declared ASC sort = the latest snapshot).
    #[test]
    fn test_asc_nulls_last_emits_reversed_desc() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Asc, NullsOrder::Last)],
        );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("DESC NULLS LAST"),
            "Declared ASC must emit reversed DESC in the RANK ORDER BY: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // T-15 / F-18 (code-review 2026-07-16): semi-additive × role-playing.
    // An UNQUERIED NON ADDITIVE BY dim on a role-playing table must snapshot
    // by the SAME role the metric's USING clause pins, not by whichever join
    // edge the resolver picks first.
    // -----------------------------------------------------------------------

    /// Role-playing airports reached by `dep_airport(departure_code)` and
    /// `arr_airport(arrival_code)`; a semi-additive metric on flights that
    /// pins one role via `USING`, with the airport `city` dimension as its
    /// (unqueried) NON ADDITIVE BY snapshot key. Query by the base `carrier`
    /// only, so `city` is unqueried and the snapshot's RANK ORDER BY resolves
    /// through the role-playing path.
    fn role_playing_semi_def(using: &str) -> crate::model::SemanticViewDefinition {
        let mut def = minimal_def("f", "carrier", "f.carrier", "latest_bal", "SUM(f.amount)");
        def.tables[0].pk_columns = vec!["flight_id".to_string()];
        def.metrics[0].source_table = Some("f".to_string());
        def.with_table("a", "airports", &["airport_code"])
            .with_dimension("city", "a.city", Some("a"))
            .with_pkfk_join(
                "dep_airport",
                "f",
                "a",
                &["departure_code"],
                &["airport_code"],
            )
            .with_pkfk_join(
                "arr_airport",
                "f",
                "a",
                &["arrival_code"],
                &["airport_code"],
            )
            .with_using_relationship("latest_bal", &[using])
            .with_non_additive_by("latest_bal", &[("city", SortOrder::Asc, NullsOrder::Last)])
    }

    fn carrier_only_req() -> QueryRequest {
        QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("carrier")],
            metrics: vec![MetricName::new("latest_bal")],
        }
    }

    /// The snapshot ORDER BY must reference the metric's SCOPED airport
    /// instance (`a__arr_airport.city`), and no redundant bare `a` join to
    /// airports may be emitted. Before the fix the ORDER BY ordered by the bare
    /// `a.city` and a second `airports AS a` join was added on the
    /// first-declared edge (`departure_code`) — the wrong role.
    #[test]
    fn test_role_playing_na_dim_orders_by_scoped_alias() {
        let sql = expand(
            "rp_view",
            &role_playing_semi_def("arr_airport"),
            &carrier_only_req(),
        )
        .expect("expand");
        // The metric's scoped instance carries the snapshot ordering.
        assert!(
            sql.contains("ORDER BY a__arr_airport.city"),
            "snapshot must order by the USING-scoped alias, not the bare table: {sql}"
        );
        // The bare-table order (the wrong-role snapshot) must be gone.
        assert!(
            !sql.contains("ORDER BY a.city"),
            "must NOT order by the bare table alias (wrong-role snapshot): {sql}"
        );
        // No redundant second join to airports on the bare alias.
        assert!(
            !sql.contains("AS \"a\" ON"),
            "must NOT emit a redundant bare `a` join (the USING scoped join covers it): {sql}"
        );
        assert!(
            sql.contains("AS \"a__arr_airport\" ON \"f\".\"arrival_code\""),
            "the scoped airport join (arrival edge) must be present: {sql}"
        );
    }

    /// The opposite role selects the departure edge — proving the ORDER BY
    /// tracks the metric's USING, not a fixed first-declared relationship.
    #[test]
    fn test_role_playing_na_dim_tracks_using_dep() {
        let sql = expand(
            "rp_view",
            &role_playing_semi_def("dep_airport"),
            &carrier_only_req(),
        )
        .expect("expand");
        assert!(
            sql.contains("ORDER BY a__dep_airport.city"),
            "USING(dep_airport) must order by the departure-scoped alias: {sql}"
        );
    }

    /// A role-playing NA dim with NO USING context on its metric is genuinely
    /// ambiguous (which airport instance?). It must fail loud with the same
    /// `AmbiguousPath` error the queried-dim role-playing path raises — not
    /// silently snapshot by the first-declared edge (the pre-fix behaviour).
    #[test]
    fn test_role_playing_na_dim_without_using_is_ambiguous() {
        let mut def = role_playing_semi_def("dep_airport");
        // Drop the USING that disambiguated the role.
        def.metrics
            .iter_mut()
            .find(|m| m.name == "latest_bal")
            .unwrap()
            .using_relationships
            .clear();
        match expand("rp_view", &def, &carrier_only_req()) {
            Err(ExpandError::AmbiguousPath { dimension_name, .. }) => {
                assert_eq!(dimension_name, "city")
            }
            other => panic!("expected AmbiguousPath for the role-playing NA dim, got: {other:?}"),
        }
    }

    /// Two active semi-additive metrics sharing the same NA dim (`city`) but
    /// pinning DIFFERENT roles via USING must land on SEPARATE RANK() columns —
    /// `collect_na_groups` folds the resolved scoped alias into the group key.
    /// Collapsing them would order both snapshots by one role, silently giving
    /// the other metric the wrong-role snapshot.
    #[test]
    fn test_role_playing_different_using_separate_rank_columns() {
        let mut def = role_playing_semi_def("arr_airport");
        def = def
            .with_metric("latest_bal_dep", "SUM(f.amount)", Some("f"))
            .with_using_relationship("latest_bal_dep", &["dep_airport"])
            .with_non_additive_by(
                "latest_bal_dep",
                &[("city", SortOrder::Asc, NullsOrder::Last)],
            );
        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("carrier")],
            metrics: vec![
                MetricName::new("latest_bal"),
                MetricName::new("latest_bal_dep"),
            ],
        };
        let sql = expand("rp_view", &def, &req).expect("expand");
        assert!(
            sql.contains("__sv_rn_1") && sql.contains("__sv_rn_2"),
            "different-role NA metrics must get separate RANK columns: {sql}"
        );
        assert!(
            sql.contains("ORDER BY a__arr_airport.city")
                && sql.contains("ORDER BY a__dep_airport.city"),
            "each RANK must order by its own role's scoped alias: {sql}"
        );
    }

    /// Data-level tests: execute the generated snapshot SQL against an
    /// in-memory `DuckDB`. The `extension` feature swaps the bundled API for
    /// loadable-extension stubs, so these are gated like catalog.rs's tests.
    #[cfg(not(feature = "extension"))]
    mod execution {
        use super::*;

        fn run_by_first_col(sql: &str) -> Vec<(String, f64)> {
            let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
            con.execute_batch(
                "CREATE TABLE accounts (customer_id VARCHAR, report_date DATE, balance DOUBLE);
                 INSERT INTO accounts VALUES
                     ('alice', DATE '2024-01-01', 999.0),
                     ('alice', DATE '2024-01-02', 100.0),
                     ('alice', DATE '2024-01-02', 150.0),
                     ('bob',   NULL,              40.0),
                     ('bob',   DATE '2024-01-01', 300.0);",
            )
            .expect("setup");
            let wrapped = format!("SELECT * FROM ({sql}) ORDER BY 1");
            let mut stmt = con.prepare(&wrapped).expect("prepare generated SQL");
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                })
                .expect("query");
            rows.collect::<Result<Vec<_>, _>>().expect("rows")
        }

        // Default direction (ASC) selects the LATEST snapshot per F-1 (Snowflake
        // semantics), so these "latest wins" data tests declare ASC.
        fn snapshot_def(nulls: NullsOrder) -> crate::model::SemanticViewDefinition {
            minimal_def(
                "accounts",
                "customer_id",
                "customer_id",
                "balance",
                "SUM(balance)",
            )
            .with_dimension("report_date", "report_date", None)
            .with_non_additive_by("balance", &[("report_date", SortOrder::Asc, nulls)])
        }

        fn snapshot_req() -> QueryRequest {
            QueryRequest {
                where_clause: None,
                facts: vec![],
                dimensions: vec![DimensionName::new("customer_id")],
                metrics: vec![MetricName::new("balance")],
            }
        }

        /// SG-4 data-level: alice has TWO rows tied at the latest date
        /// (2024-01-02: 100.0 and 150.0). RANK gives both rows rank 1, so
        /// the snapshot sum is 250.0 — deterministically. `ROW_NUMBER` kept
        /// one arbitrary tied row (100.0 or 150.0, run-to-run
        /// nondeterministic). bob (NULLS LAST variant): the latest non-NULL
        /// date wins -> 300.0.
        #[test]
        fn test_ties_at_snapshot_all_aggregate() {
            let sql = expand(
                "test_view",
                &snapshot_def(NullsOrder::Last),
                &snapshot_req(),
            )
            .expect("expand");
            let rows = run_by_first_col(&sql);
            assert_eq!(rows.len(), 2, "two customers: {rows:?}");
            assert_eq!(rows[0].0, "alice");
            assert!(
                (rows[0].1 - 250.0).abs() < 1e-9,
                "both tied rows must aggregate (100 + 150), got: {rows:?}"
            );
            assert_eq!(rows[1].0, "bob");
            assert!(
                (rows[1].1 - 300.0).abs() < 1e-9,
                "NULLS LAST: latest non-NULL date wins for bob, got: {rows:?}"
            );
        }

        /// TC-7 data-level: with NULLS FIRST, bob's NULL-dated row outranks the
        /// dated row -> 40.0 (F-1: the RANK ORDER BY keeps the declared NULLS
        /// ordering, so NULLS FIRST lets the NULL key win regardless of the
        /// direction reversal).
        #[test]
        fn test_nulls_first_null_row_wins() {
            let sql = expand(
                "test_view",
                &snapshot_def(NullsOrder::First),
                &snapshot_req(),
            )
            .expect("expand");
            let rows = run_by_first_col(&sql);
            assert_eq!(rows[1].0, "bob");
            assert!(
                (rows[1].1 - 40.0).abs() < 1e-9,
                "NULLS FIRST: the NULL-dated row must win for bob, got: {rows:?}"
            );
        }

        /// F-1 data-level: the polarity is inverted from the default. Declaring
        /// `DESC` selects the EARLIEST snapshot (mirror of the ASC/latest
        /// default), verified end-to-end on the same fixture as the latest-wins
        /// test above. alice earliest = 2024-01-01 (999.0); bob earliest
        /// non-NULL (NULLS LAST) = 2024-01-01 (300.0).
        #[test]
        fn test_desc_selects_earliest_snapshot() {
            let def = minimal_def(
                "accounts",
                "customer_id",
                "customer_id",
                "balance",
                "SUM(balance)",
            )
            .with_dimension("report_date", "report_date", None)
            .with_non_additive_by(
                "balance",
                &[("report_date", SortOrder::Desc, NullsOrder::Last)],
            );

            let sql = expand("test_view", &def, &snapshot_req()).expect("expand");
            let rows = run_by_first_col(&sql);
            assert_eq!(rows.len(), 2, "two customers: {rows:?}");
            assert_eq!(rows[0].0, "alice");
            assert!(
                (rows[0].1 - 999.0).abs() < 1e-9,
                "DESC selects the earliest snapshot for alice (2024-01-01=999), got: {rows:?}"
            );
            assert_eq!(rows[1].0, "bob");
            assert!(
                (rows[1].1 - 300.0).abs() < 1e-9,
                "DESC + NULLS LAST: earliest non-NULL date wins for bob (300), got: {rows:?}"
            );
        }

        /// SG-9 data-level: NA dim on a non-queried, non-base table. The
        /// snapshot CTE joins `dates`, the ORDER BY binds to `d.report_date`,
        /// and each customer's balance snapshots at their latest date.
        /// customer 10: `date_id` 2 (2024-01-02) is latest -> 150.0
        /// customer 20: only `date_id` 1 -> 300.0
        #[test]
        fn test_na_dim_join_executes() {
            let mut def = minimal_def(
                "accounts",
                "customer_id",
                "a.customer_id",
                "balance",
                "SUM(a.balance)",
            );
            def.tables[0].alias = "a".to_string();
            def.tables[0].pk_columns = vec!["id".to_string()];
            def.metrics[0].source_table = Some("a".to_string());
            let def = def
                .with_table("d", "dates", &["id"])
                .with_dimension("report_date", "d.report_date", Some("d"))
                .with_non_additive_by(
                    "balance",
                    &[("report_date", SortOrder::Asc, NullsOrder::First)],
                )
                .with_pkfk_join("acct_date", "a", "d", &["date_id"], &["id"]);

            let sql = expand("test_view", &def, &snapshot_req()).expect("expand");

            let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
            con.execute_batch(
                "CREATE TABLE dates (id INTEGER, report_date DATE);
                 INSERT INTO dates VALUES (1, DATE '2024-01-01'), (2, DATE '2024-01-02');
                 CREATE TABLE accounts (id INTEGER, customer_id INTEGER, date_id INTEGER, balance DOUBLE);
                 INSERT INTO accounts VALUES
                     (1, 10, 1, 100.0),
                     (2, 10, 2, 150.0),
                     (3, 20, 1, 300.0);",
            )
            .expect("setup");
            let wrapped = format!("SELECT * FROM ({sql}) ORDER BY 1");
            let mut stmt = con.prepare(&wrapped).expect("prepare generated SQL");
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, f64>(1)?)))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows");
            assert_eq!(rows.len(), 2, "rows: {rows:?}");
            assert_eq!(rows[0].0, 10);
            assert!((rows[0].1 - 150.0).abs() < 1e-9, "rows: {rows:?}");
            assert_eq!(rows[1].0, 20);
            assert!((rows[1].1 - 300.0).abs() < 1e-9, "rows: {rows:?}");
        }

        /// #30 data-level: the same NA-dim-on-a-joined-table scenario as
        /// `test_na_dim_join_executes`, but the NON ADDITIVE BY reference is
        /// DOTTED (`d.report_date`). It must resolve to the same dimension and
        /// produce identical results — proving the dotted path expands to a
        /// bindable, correct snapshot query end-to-end (not the quoted
        /// non-column that failed at bind time before #30).
        #[test]
        fn test_dotted_na_dim_join_executes() {
            let mut def = minimal_def(
                "accounts",
                "customer_id",
                "a.customer_id",
                "balance",
                "SUM(a.balance)",
            );
            def.tables[0].alias = "a".to_string();
            def.tables[0].pk_columns = vec!["id".to_string()];
            def.metrics[0].source_table = Some("a".to_string());
            let def = def
                .with_table("d", "dates", &["id"])
                .with_dimension("report_date", "d.report_date", Some("d"))
                // DOTTED NA reference — the crux of #30.
                .with_non_additive_by(
                    "balance",
                    &[("d.report_date", SortOrder::Asc, NullsOrder::First)],
                )
                .with_pkfk_join("acct_date", "a", "d", &["date_id"], &["id"]);

            let sql = expand("test_view", &def, &snapshot_req()).expect("expand");

            let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
            con.execute_batch(
                "CREATE TABLE dates (id INTEGER, report_date DATE);
                 INSERT INTO dates VALUES (1, DATE '2024-01-01'), (2, DATE '2024-01-02');
                 CREATE TABLE accounts (id INTEGER, customer_id INTEGER, date_id INTEGER, balance DOUBLE);
                 INSERT INTO accounts VALUES
                     (1, 10, 1, 100.0),
                     (2, 10, 2, 150.0),
                     (3, 20, 1, 300.0);",
            )
            .expect("setup");
            let wrapped = format!("SELECT * FROM ({sql}) ORDER BY 1");
            let mut stmt = con.prepare(&wrapped).expect("prepare generated SQL");
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, i32>(0)?, row.get::<_, f64>(1)?)))
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows");
            assert_eq!(rows.len(), 2, "rows: {rows:?}");
            assert_eq!(rows[0].0, 10);
            assert!((rows[0].1 - 150.0).abs() < 1e-9, "rows: {rows:?}");
            assert_eq!(rows[1].0, 20);
            assert!((rows[1].1 - 300.0).abs() < 1e-9, "rows: {rows:?}");
        }

        /// T-15 / F-18 data-level: semi-additive × role-playing, end-to-end.
        /// A metric pins the ARRIVAL role via `USING (arr_airport)` and
        /// snapshots on the airport `city` (unqueried). The data is engineered
        /// so the arrival-city snapshot and the departure-city snapshot pick
        /// DIFFERENT flights:
        ///   f1: dep San Francisco / arr Boston    / 100
        ///   f2: dep London        / arr New York  / 200
        /// Default (ASC) selects the alphabetically-LAST city (F-1 latest).
        ///   arrival cities {Boston, New York} -> New York -> f2 -> 200 (correct)
        ///   departure cities {San Francisco, London} -> San Francisco -> f1 -> 100
        /// Before the fix the snapshot ordered by the bare `a.city`, whose join
        /// edge is the first-declared relationship (dep_airport), so it returned
        /// 100 — the wrong role — silently. It must return 200.
        #[test]
        fn test_role_playing_na_dim_snapshot_uses_correct_role() {
            let sql = expand(
                "rp_view",
                &role_playing_semi_def("arr_airport"),
                &carrier_only_req(),
            )
            .expect("expand");

            let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
            con.execute_batch(
                "CREATE TABLE airports (airport_code VARCHAR, city VARCHAR);
                 INSERT INTO airports VALUES
                     ('SFO','San Francisco'),('LHR','London'),
                     ('BOS','Boston'),('JFK','New York');
                 CREATE TABLE f (flight_id INTEGER, departure_code VARCHAR,
                                 arrival_code VARCHAR, carrier VARCHAR, amount DOUBLE);
                 INSERT INTO f VALUES
                     (1, 'SFO', 'BOS', 'AA', 100.0),
                     (2, 'LHR', 'JFK', 'AA', 200.0);",
            )
            .expect("setup");
            let wrapped = format!("SELECT * FROM ({sql}) ORDER BY 1");
            let mut stmt = con.prepare(&wrapped).expect("prepare generated SQL");
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                })
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows");
            assert_eq!(rows.len(), 1, "one carrier: {rows:?}");
            assert_eq!(rows[0].0, "AA");
            assert!(
                (rows[0].1 - 200.0).abs() < 1e-9,
                "arrival-role snapshot must win (New York -> 200), not departure (100): {rows:?}"
            );
        }

        /// Regression (Copilot review on #129): two active semi-additive metrics
        /// that share the SAME NA dimension but have OPPOSITE polarity
        /// (`ASC` = latest vs `DESC` = earliest) must snapshot at different ends.
        /// `collect_na_groups` keys the group on dimension name AND polarity, so
        /// they get SEPARATE RANK() columns; keying on the dimension alone would
        /// collapse them onto one column built from the first metric's polarity,
        /// silently giving the other metric the wrong-end snapshot.
        ///
        /// alice: latest (ASC) = 2024-01-02 tie (100+150=250); earliest (DESC)
        /// = 2024-01-01 (999). The two answers differ, so a collapse to one
        /// column would make `earliest_bal` wrongly read 250.
        #[test]
        fn test_same_na_dim_opposite_polarity_separate_rank_columns() {
            let def = minimal_def(
                "accounts",
                "customer_id",
                "customer_id",
                "latest_bal",
                "SUM(balance)",
            )
            .with_dimension("report_date", "report_date", None)
            .with_metric("earliest_bal", "SUM(balance)", None)
            .with_non_additive_by(
                "latest_bal",
                &[("report_date", SortOrder::Asc, NullsOrder::Last)],
            )
            .with_non_additive_by(
                "earliest_bal",
                &[("report_date", SortOrder::Desc, NullsOrder::Last)],
            );

            let req = QueryRequest {
                where_clause: None,
                facts: vec![],
                dimensions: vec![DimensionName::new("customer_id")],
                metrics: vec![
                    MetricName::new("latest_bal"),
                    MetricName::new("earliest_bal"),
                ],
            };

            let sql = expand("test_view", &def, &req).expect("expand");
            // Two distinct snapshot rank columns must be emitted.
            assert!(
                sql.contains("__sv_rn_1") && sql.contains("__sv_rn_2"),
                "opposite-polarity NA metrics must get separate RANK columns: {sql}"
            );

            let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
            con.execute_batch(
                "CREATE TABLE accounts (customer_id VARCHAR, report_date DATE, balance DOUBLE);
                 INSERT INTO accounts VALUES
                     ('alice', DATE '2024-01-01', 999.0),
                     ('alice', DATE '2024-01-02', 100.0),
                     ('alice', DATE '2024-01-02', 150.0);",
            )
            .expect("setup");
            let wrapped = format!("SELECT * FROM ({sql}) ORDER BY 1");
            let mut stmt = con.prepare(&wrapped).expect("prepare generated SQL");
            let rows = stmt
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, f64>(1)?,
                        row.get::<_, f64>(2)?,
                    ))
                })
                .expect("query")
                .collect::<Result<Vec<_>, _>>()
                .expect("rows");
            assert_eq!(rows.len(), 1, "rows: {rows:?}");
            assert_eq!(rows[0].0, "alice");
            // latest_bal (ASC → latest): 2024-01-02 tie = 250.0
            assert!(
                (rows[0].1 - 250.0).abs() < 1e-9,
                "latest_bal must snapshot the latest date (250): {rows:?}"
            );
            // earliest_bal (DESC → earliest): 2024-01-01 = 999.0. A collapse to
            // one RANK column would make this read 250.0 instead.
            assert!(
                (rows[0].2 - 999.0).abs() < 1e-9,
                "earliest_bal must snapshot the earliest date (999), not share latest_bal's column: {rows:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Pre-aggregation `where_clause`
    //
    // The predicate must land INSIDE `__sv_snapshot`, which puts it before the
    // `RANK()` is evaluated. That ordering is the whole point: filtering
    // changes which row wins the snapshot. On the outer query the snapshot
    // would be picked from unfiltered rows and only then narrowed -- a
    // different, wrong answer.
    // -----------------------------------------------------------------------

    #[test]
    fn where_clause_is_injected_inside_the_snapshot_cte_before_the_rank() {
        let def = minimal_def(
            "accounts",
            "customer_id",
            "customer_id",
            "balance",
            "SUM(balance)",
        )
        .with_dimension("report_date", "report_date", None)
        .with_non_additive_by(
            "balance",
            &[("report_date", SortOrder::Desc, NullsOrder::First)],
        );

        let req = QueryRequest {
            where_clause: Some("report_date >= DATE '2024-01-01'".to_string()),
            facts: vec![],
            dimensions: vec![DimensionName::new("customer_id")],
            metrics: vec![MetricName::new("balance")],
        };

        let sql = expand("test_view", &def, &req).unwrap();

        let cte_end = sql.find("\n)\n").expect("the snapshot CTE must close");
        let cte = &sql[..cte_end];
        let outer = &sql[cte_end..];

        assert!(
            cte.contains("WHERE (report_date) >= DATE '2024-01-01'"),
            "predicate must be inside the snapshot CTE: {sql}"
        );
        assert!(
            !outer.contains("WHERE"),
            "predicate must NOT be on the outer query, where it would filter \
             after the snapshot was already chosen: {outer}"
        );
        // The RANK lives in the CTE's select list, so a WHERE in the same CTE
        // is evaluated first -- the filter decides which rows compete.
        assert!(cte.contains("RANK() OVER"), "RANK stays in the CTE: {cte}");
    }
}
