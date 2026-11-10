//! CTE-based expansion for window function metrics.
//!
//! Window metrics wrap another metric in a SQL window function (e.g., `AVG(total_qty)`
//! OVER (PARTITION BY ... ORDER BY ...)). The expansion generates:
//! 1. A CTE that aggregates the inner metric(s) by ALL queried dimensions
//! 2. An outer SELECT that applies the window function over the CTE results
//!
//! PARTITION BY EXCLUDING is resolved at expansion time: the partition columns
//! are all queried dimensions MINUS the excluded dimensions. With no PARTITION
//! BY at all, the window runs over every result row, as in Snowflake.

use std::collections::{HashMap, HashSet};

use crate::model::{Metric, NullsOrder, SemanticViewDefinition, SortOrder};

use super::join_resolver::{push_join_clauses, resolve_joins_pkfk};
use super::resolution::{quote_ident, quote_stored_ident};
use super::select_spec::{
    push_from_anchor, push_from_base, push_group_by_ordinals, FromSource, GroupBy, SelectItem,
    SelectSpec,
};
use super::types::{ExpandError, ResolvedDim};

/// Resolve a window dimension reference (bare, dotted, or quoted) to the
/// `__sv_agg` CTE-alias column it must emit in the OVER clause — i.e.
/// `quote_stored_ident` of the *declared* dimension's stored name, the exact
/// alias the CTE SELECT assigned it (a quoted stored name is stripped to its
/// logical value and quoted once, EXP-7). Falls back to quoting the raw
/// reference when it resolves to no declared dimension (fail-clean).
///
/// The outer query runs over the CTE, so referencing the alias is safe (no
/// physical column shadows it, unlike the semi-additive CTE's own SELECT which
/// must repeat expressions — E-1). Emitting the resolved alias keeps the OVER
/// clause bound to the CTE column even when the reference is written quoted
/// (`"order date"`) or dotted (`o."order date"`) — spellings that previously
/// emitted a doubled-quote or dotted non-column (TECH-DEBT #28/#30).
fn window_dim_column(def: &SemanticViewDefinition, reference: &str) -> String {
    super::resolution::find_dimension(def, reference)
        .map_or_else(|| quote_ident(reference), |d| quote_stored_ident(&d.name))
}

/// The dimensions a window metric cannot be queried without: every
/// `PARTITION BY EXCLUDING`, `PARTITION BY` and `ORDER BY` reference in its
/// spec, which the query-time guard in [`expand_window_metrics`] insists are
/// queried alongside it (`WindowMetricRequiredDimension`).
///
/// Each reference is resolved (bare, dotted or quoted) to its declared
/// dimension's stored name and deduplicated by identity, so the result is a
/// valid `dimensions` request on its own — the read-side member-type probe
/// (TECH-DEBT #51) uses it to type a window metric, which has no shape it can
/// be queried in alone. A reference that resolves to no declared dimension is
/// passed through raw so that expansion reports it. Empty for a non-window
/// metric.
pub(crate) fn window_required_dimensions(
    def: &SemanticViewDefinition,
    met: &Metric,
) -> Vec<String> {
    let Some(ws) = met.window_spec.as_ref() else {
        return Vec::new();
    };
    let refs = ws
        .excluding_dims
        .iter()
        .chain(ws.partition_dims.iter())
        .chain(ws.order_by.iter().map(|ob| &ob.expr));
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for r in refs {
        let name =
            super::resolution::find_dimension(def, r).map_or_else(|| r.clone(), |d| d.name.clone());
        if seen.insert(crate::ident::normalize_ident_part(&name)) {
            out.push(name);
        }
    }
    out
}

/// Generate CTE-based expansion SQL for queries containing window function metrics.
///
/// Called from `expand()` when all resolved metrics are window metrics.
/// Receives already-resolved dims, metrics, expressions, and scoped aliases.
///
/// # Strategy
///
/// 1. **CTE `__sv_agg`**: Aggregates all inner metrics by ALL queried dimensions
///    (standard GROUP BY). This pre-aggregates the data so that window functions
///    operate on grouped results, not raw rows.
///
/// 2. **Outer SELECT**: Applies each window function over the CTE results with
///    computed PARTITION BY (all queried dims minus EXCLUDING dims, the listed
///    dims of an explicit PARTITION BY, or none: every result row) and ORDER BY.
///    No GROUP BY in the outer query -- window functions are row-level operations.
///
/// # Grain
///
/// `grain_anchor` re-anchors the CTE in step 1 at a declared table other than the
/// base table — see [`super::per_grain::window_cte_anchor`], which decides it and
/// documents when it declines. Step 2 is unaffected: the window function runs over
/// the already-grouped CTE, so only the inner aggregate is grain-sensitive.
/// `None` keeps the base-anchored shape, byte-identical to before.
#[allow(clippy::too_many_lines, clippy::unnecessary_wraps)]
pub(super) fn expand_window_metrics(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[ResolvedDim],
    resolved_mets: &[&Metric],
    resolved_exprs: &HashMap<String, String>,
    where_clause: Option<&super::where_clause::ResolvedWhere>,
    grain_anchor: Option<&str>,
) -> Result<String, ExpandError> {
    // 1. Validate required dimensions for each window metric.
    //
    // Both the inner-*metric* keying (below) and this dimension-side matching
    // are quote- and dotted-aware: every EXCLUDING / PARTITION BY / ORDER BY
    // dimension reference is resolved through the shared bare-AND-dotted
    // resolver (`resolution::dim_ref_key`), the same key the queried dims use,
    // so a reference written quoted (`"Region"`) or dotted (`o."order date"`)
    // classifies identically to its bare spelling. This closes the window half
    // of the dimension-side pass whose semi-additive half landed as #30
    // (TECH-DEBT #28/#30). A dotted ORDER BY reference is accepted at CREATE
    // (Phase 48 / D-08) but previously failed this bare-only check at query
    // time; a quoted reference previously emitted a doubled-quote non-column in
    // the OVER clause (uncaught because the pre-#30 tests never executed the
    // window query, only inspected DDL / EXPLAIN text).
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|rd| crate::ident::normalize_ident_part(&rd.dim.name))
        .collect();

    for met in resolved_mets {
        let Some(ref ws) = met.window_spec else {
            continue;
        };
        // Check EXCLUDING dims are all in the query
        for excl_dim in &ws.excluding_dims {
            if !queried_dim_keys.contains(&super::resolution::dim_ref_key(def, excl_dim)) {
                return Err(ExpandError::WindowMetricRequiredDimension {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    dimension_name: excl_dim.clone(),
                    reason: "PARTITION BY EXCLUDING".to_string(),
                });
            }
        }
        // Check explicit PARTITION BY dims are all in the query
        for part_dim in &ws.partition_dims {
            if !queried_dim_keys.contains(&super::resolution::dim_ref_key(def, part_dim)) {
                return Err(ExpandError::WindowMetricRequiredDimension {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    dimension_name: part_dim.clone(),
                    reason: "PARTITION BY".to_string(),
                });
            }
        }
        // Check ORDER BY dims are all in the query
        for ob in &ws.order_by {
            if !queried_dim_keys.contains(&super::resolution::dim_ref_key(def, &ob.expr)) {
                return Err(ExpandError::WindowMetricRequiredDimension {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    dimension_name: ob.expr.clone(),
                    reason: "ORDER BY".to_string(),
                });
            }
        }
    }

    // 2. Collect distinct inner metric names needed for the CTE.
    let mut inner_metric_set: HashSet<String> = HashSet::new();
    let mut inner_metric_order: Vec<String> = Vec::new();
    for met in resolved_mets {
        let Some(ref ws) = met.window_spec else {
            continue;
        };
        // Canonical identifier key (quote-stripped + case-folded) so a quoted
        // inner-metric reference (`"Total_Qty"`) keys, and later aliases,
        // identically to its stored `total_qty` — TECH-DEBT #28 Slice 3.
        let key = crate::ident::normalize_ident_part(&ws.inner_metric);
        if inner_metric_set.insert(key.clone()) {
            inner_metric_order.push(key);
        }
    }

    // Resolve inner metric expressions via resolved_exprs.
    let mut inner_metric_exprs: HashMap<String, String> = HashMap::new();
    for inner_name in &inner_metric_order {
        let expr = resolved_exprs.get(inner_name).cloned().unwrap_or_else(|| {
            // Fall back to finding the metric definition directly, then — when
            // the reference resolves to no declared metric — to the reference
            // itself as a column, `quote_ident`'d (fail-clean, matching
            // `window_dim_column`). `inner_name` is a canonical identifier key,
            // so emitting it BARE leaked a literal `"` for a quoted-with-embedded
            // -quote reference (`"a""b"` → normalized `a"b`), producing
            // structurally invalid SQL — the alias side and the outer OVER
            // reference already quote it, so the expression side must too
            // (fuzz_sql_expand crash, issue #145).
            def.metrics
                .iter()
                .find(|m| crate::ident::ident_matches(&m.name, inner_name))
                .map_or_else(|| quote_ident(inner_name), |m| m.expr.clone())
        });
        inner_metric_exprs.insert(inner_name.clone(), expr);
    }

    // 3. Build CTE __sv_agg
    let mut sql = String::with_capacity(512);
    sql.push_str("WITH __sv_agg AS (\n    SELECT\n");

    let mut cte_select_items: Vec<String> = Vec::new();

    // Dimension columns in CTE
    for rd in resolved_dims {
        let dim = rd.dim;
        let mut base_expr = super::facts::inline_dimension_facts(&dim.expr, &def.facts);
        if let Some(ref scoped) = rd.scoped_alias {
            if let Some(ref st) = dim.source_table {
                base_expr = super::join_resolver::rewrite_to_scoped_alias(&base_expr, st, scoped);
            }
        }
        let item = SelectItem::new(
            base_expr,
            dim.output_type.clone(),
            quote_stored_ident(&dim.name),
        );
        cte_select_items.push(format!("        {}", item.render()));
    }

    // Inner metric aggregated columns in CTE
    for inner_name in &inner_metric_order {
        let expr = &inner_metric_exprs[inner_name];
        // The inner metric expression is already an aggregate (e.g., SUM(s.quantity))
        // so we include it directly in the CTE SELECT with GROUP BY.
        cte_select_items.push(format!("        {} AS {}", expr, quote_ident(inner_name)));
    }

    sql.push_str(&cte_select_items.join(",\n"));

    // CTE FROM clause + JOINs.
    let dims: Vec<&crate::model::Dimension> = resolved_dims.iter().map(|rd| rd.dim).collect();
    // Tables named only by the predicate must be joined inside the CTE, where
    // the filter is applied.
    let where_tables: Vec<String> = where_clause
        .map(|w| w.source_tables.clone())
        .unwrap_or_default();
    // Anchored at the inner aggregate's own grain when the base-anchored FROM
    // would fan it (TECH-DEBT #36): the joins then reach out from THAT table to
    // the queried dimensions, rather than from the base table inward. Only the
    // CTE moves — the outer window SELECT still runs over `__sv_agg` unchanged,
    // because the window function is not grain-sensitive.
    let resolved_joins = if let Some(anchor) = grain_anchor {
        push_from_anchor(&mut sql, def, anchor, "\n    ");
        // Off the base table, the facts the inner metrics and the dimensions
        // inline must be joined here explicitly: `anchor_joins` walks only the
        // tables it is handed, where `resolve_joins_pkfk` below collects them
        // itself (reviewbot, #250 -- a child-anchored `__sv_agg` emitted
        // `FROM li` with no join for a parent fact).
        let needs = super::per_grain::anchor_needs(def, resolved_mets, &dims, &where_tables);
        // The window path declines every role-played query (`window_cte_anchor`
        // is strict), so it never has a dimension role to honour and passes
        // none; `needs.scoped` is empty for the same reason, since a `USING`
        // that scopes a reached fact declines the anchor. Were that to change,
        // `__sv_agg`'s SELECT list would need the same scoped-alias rewrite the
        // grain CTEs do.
        super::per_grain::anchor_joins(
            def,
            anchor,
            &dims,
            &needs.bare,
            &std::collections::HashMap::new(),
            &std::collections::HashMap::new(),
            &needs.scoped,
        )
    } else {
        push_from_base(&mut sql, def, "\n    ");
        resolve_joins_pkfk(def, &dims, resolved_mets, &where_tables)
    };
    push_join_clauses(&mut sql, &resolved_joins, def, "\n    LEFT JOIN ");

    // Inside the CTE and before its GROUP BY, so the inner aggregates the
    // window function reads are computed over only the matching rows. On the
    // outer query the predicate would filter the window's OUTPUT instead of its
    // input, which for a running total or rank is a different number entirely.
    if let Some(w) = where_clause {
        sql.push_str("\n    WHERE ");
        sql.push_str(&w.sql);
    }

    // CTE GROUP BY (all dimension columns)
    if !resolved_dims.is_empty() {
        push_group_by_ordinals(&mut sql, resolved_dims.len(), "\n    ", "        ");
    }

    sql.push_str("\n)\n");

    // 4. Build the outer SELECT over the aggregation CTE.
    let mut outer_items: Vec<SelectItem> = Vec::new();

    // Dimension columns: reference CTE aliases (outer query over the CTE, so
    // referencing the alias is safe here).
    for rd in resolved_dims {
        outer_items.push(SelectItem::new(
            quote_stored_ident(&rd.dim.name),
            None,
            quote_stored_ident(&rd.dim.name),
        ));
    }

    // Window metric columns
    for met in resolved_mets {
        let Some(ref ws) = met.window_spec else {
            continue;
        };

        // Build the function call: window_function(inner_metric_alias, extra_args...)
        // The canonical key (computed once here) is the same one the CTE column
        // above was aliased with, so a quoted inner-metric reference aliases and
        // references identically.
        let inner_key = crate::ident::normalize_ident_part(&ws.inner_metric);
        let inner_alias = quote_ident(&inner_key);
        let mut func_args = vec![inner_alias];
        for arg in &ws.extra_args {
            func_args.push(arg.clone());
        }
        let func_call = format!("{}({})", ws.window_function, func_args.join(", "));

        // Compute PARTITION BY columns. With neither `PARTITION BY` nor
        // `PARTITION BY EXCLUDING` the window runs over every result row, as in
        // Snowflake (fixture 4, W2/W5): `SUM(m) OVER (ORDER BY d)` is a running
        // total across the result, not one partition per row.
        let partition_cols: Vec<String> =
            if ws.partition_dims.is_empty() && ws.excluding_dims.is_empty() {
                Vec::new()
            } else if ws.partition_dims.is_empty() {
                // PARTITION BY EXCLUDING: all queried dims minus excluding_dims.
                // Key the excluded set through the shared resolver so a quoted/dotted
                // EXCLUDING reference still matches its declared dimension.
                let excluding_set: HashSet<String> = ws
                    .excluding_dims
                    .iter()
                    .map(|d| super::resolution::dim_ref_key(def, d))
                    .collect();
                resolved_dims
                    .iter()
                    .filter(|rd| {
                        !excluding_set.contains(&crate::ident::normalize_ident_part(&rd.dim.name))
                    })
                    .map(|rd| quote_stored_ident(&rd.dim.name))
                    .collect()
            } else {
                // Explicit PARTITION BY: emit each listed dim's resolved CTE alias so
                // a quoted/dotted reference binds to the CTE column, not a
                // doubled-quote/dotted non-column.
                ws.partition_dims
                    .iter()
                    .map(|d| window_dim_column(def, d))
                    .collect()
            };

        // Build OVER clause
        let mut over_parts: Vec<String> = Vec::new();
        if !partition_cols.is_empty() {
            over_parts.push(format!("PARTITION BY {}", partition_cols.join(", ")));
        }
        if !ws.order_by.is_empty() {
            let order_items: Vec<String> = ws
                .order_by
                .iter()
                .map(|ob| {
                    let dir = match ob.order {
                        SortOrder::Asc => "ASC",
                        SortOrder::Desc => "DESC",
                    };
                    let nulls = match ob.nulls {
                        NullsOrder::First => "NULLS FIRST",
                        NullsOrder::Last => "NULLS LAST",
                    };
                    format!("{} {} {}", window_dim_column(def, &ob.expr), dir, nulls)
                })
                .collect();
            over_parts.push(format!("ORDER BY {}", order_items.join(", ")));
        }
        if let Some(ref frame) = ws.frame_clause {
            over_parts.push(frame.clone());
        }

        let over_clause = over_parts.join(" ");
        let window_expr = format!("{func_call} OVER ({over_clause})");

        outer_items.push(SelectItem::new(
            window_expr,
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }

    // Window functions are row-level ⇒ no GROUP BY in the outer query.
    sql.push_str(
        &SelectSpec {
            where_clause: None,
            distinct: false,
            items: outer_items,
            from: FromSource::Named("__sv_agg".to_string()),
            group_by: GroupBy::None,
        }
        .render(),
    );

    Ok(sql)
}

#[cfg(test)]
mod tests {
    use crate::expand::test_helpers::{minimal_def, orders_view, TestFixtureExt};
    use crate::expand::{expand, DimensionName, ExpandError, MetricName, QueryRequest};
    use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};

    /// Mirror of the `fuzz_sql_expand` quote/paren-balance oracle
    /// (`fuzz/fuzz_targets/fuzz_sql_expand.rs`): walk the text honoring `''`
    /// string and `""` identifier escapes. Balanced input fragments must yield
    /// balanced output — an odd bare `"` in the generated SQL is the exact
    /// structural corruption the fuzzer trips on.
    fn quotes_balanced(sql: &str) -> bool {
        let bytes = sql.as_bytes();
        let mut in_string = false;
        let mut in_ident = false;
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if in_string {
                if b == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        i += 2;
                        continue;
                    }
                    in_string = false;
                }
            } else if in_ident {
                if b == b'"' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                        i += 2;
                        continue;
                    }
                    in_ident = false;
                }
            } else if b == b'\'' {
                in_string = true;
            } else if b == b'"' {
                in_ident = true;
            }
            i += 1;
        }
        !in_string && !in_ident
    }

    /// Regression for the `fuzz_sql_expand` crash (issue #145): a window
    /// metric whose `inner_metric` is a quoted identifier carrying an embedded
    /// quote (`"a""b"`, logical name `a"b`) that resolves to no declared
    /// metric. The CTE builder fell back to emitting the normalized name as a
    /// BARE expression (`a"b AS "a""b"`), leaking a lone `"` and producing
    /// structurally invalid SQL — the balance oracle panicked. The alias side
    /// and the outer OVER reference already went through `quote_ident`, so the
    /// expression side must too (fail-clean, matching `window_dim_column`).
    #[test]
    fn window_unresolved_quoted_inner_metric_emits_balanced_sql() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    // Quoted identifier with an embedded quote, naming no metric.
                    inner_metric: "\"a\"\"b\"".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec![],
                    order_by: vec![],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req)
            .expect("window metric with an unresolved inner metric should still expand");
        assert!(
            quotes_balanced(&sql),
            "generated SQL must have balanced quotes (no bare stored-name quote leak): {sql}"
        );
        // The inner reference must be quoted on BOTH sides of the CTE alias,
        // never emitted bare.
        assert!(
            !sql.contains("a\"b AS"),
            "inner-metric reference must not be emitted as a bare identifier: {sql}"
        );
        assert!(
            sql.contains("\"a\"\"b\" AS \"a\"\"b\""),
            "inner-metric reference must be quote_ident'd on both sides: {sql}"
        );
    }

    /// Single window metric with 3 dims -- CTE with GROUP BY all dims,
    /// outer SELECT with window function and PARTITION BY (all minus excluded).
    #[test]
    fn test_window_single_metric_three_dims() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("date", "date", None)
            .with_dimension("year", "year", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("store"),
                DimensionName::new("date"),
                DimensionName::new("year"),
            ],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_agg AS"), "Should have CTE: {sql}");
        assert!(sql.contains("GROUP BY"), "CTE should have GROUP BY: {sql}");
        // PARTITION BY should exclude "date" -- only store and year
        assert!(
            sql.contains("PARTITION BY \"store\", \"year\""),
            "Should partition by non-excluded dims: {sql}"
        );
        assert!(
            sql.contains("ORDER BY \"date\" ASC NULLS LAST"),
            "Should have ORDER BY date: {sql}"
        );
        assert!(
            sql.contains("AVG(\"total_qty\")"),
            "Should have AVG window function: {sql}"
        );
        assert!(
            sql.contains("FROM __sv_agg"),
            "Outer should reference CTE: {sql}"
        );
    }

    /// A window metric whose inner-metric reference is written QUOTED and
    /// mixed-case (`"Total_Qty"`) against an unquoted base metric `total_qty`.
    /// The CTE aggregate column and the outer window reference must both resolve
    /// to the canonical key `"total_qty"` so the alias and the reference agree
    /// (TECH-DEBT #28 Slice 3). Before quote-aware keying the inner name kept
    /// its quote characters, so the def lookup missed the base metric and the
    /// emitted alias/reference were a doubly-quoted, non-existent column.
    #[test]
    fn test_window_quoted_inner_metric() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("date", "date", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "\"Total_Qty\"".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        // CTE aggregates the base metric under the canonical key alias.
        assert!(
            sql.contains("SUM(s.quantity) AS \"total_qty\""),
            "CTE should alias the base metric to the canonical key: {sql}"
        );
        // Outer window references that exact alias — quotes stripped, folded.
        assert!(
            sql.contains("AVG(\"total_qty\")"),
            "window should reference the canonical inner alias: {sql}"
        );
        // No doubly-quoted residue from keeping the reference's quote chars.
        assert!(
            !sql.contains("\"\"total_qty\"\""),
            "inner-metric quotes must be stripped, not doubled: {sql}"
        );
    }

    /// Window metric excluding 2 dims -- PARTITION BY only remaining dim.
    #[test]
    fn test_window_excluding_two_dims() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_dimension("year", "year", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string(), "year".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("store"),
                DimensionName::new("date"),
                DimensionName::new("year"),
            ],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        // Only "store" should be in PARTITION BY
        assert!(
            sql.contains("PARTITION BY \"store\""),
            "Should partition by only non-excluded dim: {sql}"
        );
        // "date" and "year" should NOT be in PARTITION BY
        assert!(
            !sql.contains("PARTITION BY \"store\", \"date\""),
            "Excluded dims should not be in PARTITION BY: {sql}"
        );
    }

    /// Window metric with frame clause -- RANGE BETWEEN included in OVER.
    #[test]
    fn test_window_with_frame_clause() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: Some(
                        "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW".to_string(),
                    ),
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW"),
            "Should include frame clause in OVER: {sql}"
        );
    }

    /// Window metric with extra args -- LAG(metric, 30).
    #[test]
    fn test_window_with_extra_args() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "LAG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec!["30".to_string()],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("LAG(\"total_qty\", 30)"),
            "Should include extra args in function call: {sql}"
        );
    }

    /// Two window metrics sharing same CTE but different EXCLUDING sets.
    #[test]
    fn test_two_window_metrics_different_excluding() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_dimension("year", "year", None)
            .with_metric("avg_7", "SUM(quantity)", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            )
            .with_window_spec(
                "avg_7",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "avg_7".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string(), "year".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("store"),
                DimensionName::new("date"),
                DimensionName::new("year"),
            ],
            metrics: vec![MetricName::new("total_qty"), MetricName::new("avg_7")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(sql.contains("WITH __sv_agg AS"), "Should have CTE: {sql}");
        // First metric: PARTITION BY store, year (excluding date)
        assert!(
            sql.contains("PARTITION BY \"store\", \"year\""),
            "First metric should partition by store, year: {sql}"
        );
        // Second metric: PARTITION BY store (excluding date, year)
        assert!(
            sql.contains("PARTITION BY \"store\" ORDER"),
            "Second metric should partition by store only: {sql}"
        );
    }

    /// Mixing window + regular aggregate metric returns error.
    #[test]
    fn test_window_aggregate_mixing_error() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_metric("avg_price", "AVG(price)", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty"), MetricName::new("avg_price")],
        };

        let result = expand("test_view", &def, &req);
        assert!(result.is_err(), "Should error on mixing: {:?}", result);
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("cannot mix window function metrics"),
            "Error message should mention mixing: {msg}"
        );
        assert!(
            msg.contains("total_qty"),
            "Should mention window metric name: {msg}"
        );
        assert!(
            msg.contains("avg_price"),
            "Should mention aggregate metric name: {msg}"
        );
    }

    /// Missing required dimension (EXCLUDING dim not in query) returns error.
    #[test]
    fn test_window_missing_required_dim() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        // Query with only 'store' -- missing 'date' which is required by EXCLUDING and ORDER BY
        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let result = expand("test_view", &def, &req);
        assert!(
            result.is_err(),
            "Should error on missing required dim: {:?}",
            result
        );
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("requires dimension 'date'"),
            "Should mention missing dim: {msg}"
        );
    }

    /// SG-6 (code review 2026-07-02): window metrics get the standard
    /// fan-trap check. The previous unconditional `is_window()` skip was
    /// based on the incorrect premise that the CTE pre-aggregation handles
    /// fan-out — but the CTE's inner aggregate is computed OVER the
    /// already-fanned join, so it is inflated before the window function
    /// runs. This fixture (metric grain on `c`, dims on `a`, where a->c is
    /// `ManyToOne`) fans `c`'s rows and must now error.
    #[test]
    fn test_fan_trap_checks_window_metrics() {
        // Multi-table view: window metric crosses many-to-one boundary
        // in the fan-out direction.
        let def = orders_view()
            .with_table("customers", "customers", &[])
            .clear_dimensions()
            .clear_metrics()
            .with_table("c", "customers", &["id"])
            .with_table("a", "accounts", &["id"])
            .with_dimension("acct_name", "a.name", Some("a"))
            .with_dimension("date", "a.report_date", Some("a"))
            .with_metric("total_balance", "SUM(a.balance)", Some("c"))
            .with_window_spec(
                "total_balance",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_balance".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            )
            .with_pkfk_join("cust_acct", "a", "c", &["customer_id"], &["id"]);

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("acct_name"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_balance")],
        };

        // Window metrics are checked like any other aggregate: fan-out error.
        let result = expand("test_view", &def, &req);
        assert!(
            matches!(result, Err(ExpandError::FanTrap { .. })),
            "Window metric over a fanning join must be a fan trap error, got: {result:?}"
        );
    }

    /// Explicit PARTITION BY uses listed dims directly.
    #[test]
    fn test_partition_by_explicit() {
        let def = orders_view()
            .with_dimension("store", "o.store_id", Some("o"))
            .with_dimension("date", "o.sale_date", Some("o"))
            .with_metric("total_qty", "SUM(o.qty)", Some("o"))
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec!["store".to_string()],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        // PARTITION BY should use "store" directly, not exclude anything
        assert!(
            sql.contains("PARTITION BY \"store\""),
            "Should use explicit PARTITION BY store: {sql}"
        );
        assert!(
            !sql.contains("EXCLUDING"),
            "Should not contain EXCLUDING: {sql}"
        );
    }

    /// Explicit PARTITION BY dims are required in the query.
    #[test]
    fn test_partition_by_explicit_missing_dim_error() {
        let def = orders_view()
            .with_dimension("store", "o.store_id", Some("o"))
            .with_dimension("date", "o.sale_date", Some("o"))
            .with_metric("total_qty", "SUM(o.qty)", Some("o"))
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec!["store".to_string()],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        let result = expand("test_view", &def, &req);
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("requires dimension 'store'"),
            "Should mention missing partition dim: {msg}"
        );
    }

    /// TECH-DEBT #28/#30 (window half): a DOTTED OVER ORDER BY reference
    /// (`s.date`) — accepted at CREATE via D-08 — must resolve to its declared
    /// dimension and emit that dimension's CTE alias (`"date"`), not error the
    /// required-dimension check and not emit the raw dotted text as a
    /// non-column (`"s.date"`). Before the window dimension-side pass, the
    /// bare-only `to_ascii_lowercase` check failed to match the dotted
    /// reference against the queried dim and returned a required-dimension
    /// error even though `date` was queried.
    #[test]
    fn test_window_dotted_order_by_resolves_to_cte_alias() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("date", "s.sale_date", Some("s"))
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec!["store".to_string()],
                    // DOTTED reference — the crux.
                    order_by: vec![WindowOrderBy {
                        expr: "s.date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };

        // Must not error the required-dimension check (dotted `s.date` resolves).
        let sql = expand("test_view", &def, &req).expect("dotted ORDER BY must resolve");
        assert!(
            sql.contains("ORDER BY \"date\""),
            "dotted ORDER BY must emit the resolved CTE alias: {sql}"
        );
        assert!(
            !sql.contains("\"s.date\""),
            "dotted ORDER BY text must not leak as a quoted non-column: {sql}"
        );
    }

    /// TECH-DEBT #28/#30 (window half) + EXP-7 (code-review 2026-07-18): a
    /// DOTTED-AND-QUOTED OVER ORDER BY reference (`s."order date"`) to a
    /// quoted-stored dimension must emit the dimension's CTE alias consistently,
    /// never the raw dotted text as a doubled-quote non-column
    /// (`"s.""order date"""`). The stored name (`"order date"`) is stripped to
    /// its logical value and quoted once, so the CTE alias and the OVER ORDER BY
    /// reference are both `"order date"` (one pair of quotes) — not the
    /// triple-quoted `"""order date"""` that re-quoting the stored name used to
    /// produce (EXP-7).
    #[test]
    fn test_window_dotted_quoted_order_by_matches_cte_alias() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("\"order date\"", "s.\"order date\"", Some("s"))
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec!["store".to_string()],
                    order_by: vec![WindowOrderBy {
                        expr: "s.\"order date\"".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );

        let req = QueryRequest {
            where_clause: None,
            facts: vec![],
            dimensions: vec![
                DimensionName::new("store"),
                DimensionName::new("order date"),
            ],
            metrics: vec![MetricName::new("total_qty")],
        };

        let sql = expand("test_view", &def, &req).expect("dotted-quoted ORDER BY must resolve");
        // The CTE aliases the dim column with one pair of quotes and the OVER
        // ORDER BY references that identical token, so the query binds.
        assert!(
            sql.contains("AS \"order date\""),
            "CTE must alias the quoted dim with a single pair of quotes: {sql}"
        );
        assert!(
            sql.contains("ORDER BY \"order date\""),
            "ORDER BY must reference the dimension's CTE alias: {sql}"
        );
        // EXP-7: the stored name's quotes must not be re-quoted into a
        // triple-quoted, literally-quote-named column.
        assert!(
            !sql.contains("\"\"\"order date\"\"\""),
            "stored-name quotes must not be doubled into a triple-quoted alias: {sql}"
        );
        // The raw dotted reference must NOT be emitted as a non-column.
        assert!(
            !sql.contains("\"s.\"\"order date\"\"\""),
            "dotted-quoted ORDER BY text must not leak as a non-column: {sql}"
        );
    }

    // -----------------------------------------------------------------------
    // Pre-aggregation `where_clause`
    //
    // The predicate must land inside `__sv_agg`, before its GROUP BY, so the
    // inner aggregates the window function reads are computed over only the
    // matching rows. On the outer query it would filter the window's OUTPUT --
    // for a running total or a rank, a different number entirely.
    // -----------------------------------------------------------------------

    #[test]
    fn where_clause_is_injected_inside_the_agg_cte_not_the_outer_query() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("date", "date", None)
            .with_window_spec(
                "total_qty",
                WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    extra_args: vec![],
                    excluding_dims: vec!["date".to_string()],
                    partition_dims: vec![],
                    order_by: vec![WindowOrderBy {
                        expr: "date".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    }],
                    frame_clause: None,
                },
            );
        let req = QueryRequest {
            where_clause: Some("date >= DATE '2024-01-01'".to_string()),
            facts: vec![],
            dimensions: vec![DimensionName::new("store"), DimensionName::new("date")],
            metrics: vec![MetricName::new("total_qty")],
        };
        let sql = expand("test_view", &def, &req).unwrap();

        let cte_end = sql.find("\n)\n").expect("the agg CTE must close");
        let cte = &sql[..cte_end];
        let outer = &sql[cte_end..];

        let where_at = cte.find("WHERE");
        let group_at = cte.find("GROUP BY");
        assert!(
            where_at.is_some(),
            "predicate must be inside __sv_agg: {sql}"
        );
        assert!(
            group_at.is_none() || where_at < group_at,
            "WHERE must precede the CTE GROUP BY: {cte}"
        );
        assert!(
            !outer.contains("WHERE"),
            "predicate must not filter the window output: {outer}"
        );
    }
}
