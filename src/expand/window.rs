//! CTE-based expansion for window function metrics.
//!
//! Window metrics wrap another metric in a SQL window function (e.g., `AVG(total_qty)`
//! OVER (PARTITION BY ... ORDER BY ...)). The expansion generates:
//! 1. A CTE holding the rows the query gives with each window metric replaced by
//!    its inner metric, expanded as an ordinary query
//! 2. An outer SELECT that applies the window function over the CTE results
//!
//! PARTITION BY EXCLUDING is resolved at expansion time: the partition columns
//! are all queried dimensions MINUS the excluded dimensions. With no PARTITION
//! BY at all, the window runs over every result row, as in Snowflake.

use std::collections::{HashMap, HashSet};

use crate::model::{
    AccessModifier, Dimension, Metric, NullsOrder, SemanticViewDefinition, SortOrder,
};

use super::resolution::{quote_ident, quote_stored_ident};
use super::select_spec::{FromSource, GroupBy, SelectItem, SelectSpec};
use super::types::{ExpandError, MetricName, QueryRequest};

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
/// spec, which the query-time guard in [`expand_over_result`] insists are
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

/// Refuse a window metric queried without a dimension its spec names: every
/// `PARTITION BY EXCLUDING`, `PARTITION BY` and `ORDER BY` reference must be a
/// queried dimension (`WindowMetricRequiredDimension`).
///
/// The matching is quote- and dotted-aware: each reference is resolved
/// through the shared bare-AND-dotted resolver (`resolution::dim_ref_key`),
/// the same key the queried dims use, so a reference written quoted
/// (`"Region"`) or dotted (`o."order date"`) classifies identically to its
/// bare spelling (TECH-DEBT #28/#30).
fn check_required_dimensions(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
) -> Result<(), ExpandError> {
    let queried_dim_keys: HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    for met in resolved_mets {
        let Some(ref ws) = met.window_spec else {
            continue;
        };
        let refs = ws
            .excluding_dims
            .iter()
            .map(|d| (d, "PARTITION BY EXCLUDING"))
            .chain(ws.partition_dims.iter().map(|d| (d, "PARTITION BY")))
            .chain(ws.order_by.iter().map(|ob| (&ob.expr, "ORDER BY")));
        for (dim, reason) in refs {
            if !queried_dim_keys.contains(&super::resolution::dim_ref_key(def, dim)) {
                return Err(ExpandError::WindowMetricRequiredDimension {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    dimension_name: dim.clone(),
                    reason: reason.to_string(),
                });
            }
        }
    }
    Ok(())
}

/// The `OVER (...)` text of a window metric over `__sv_agg`, whose columns are
/// the queried dimensions under their stored names.
///
/// With neither `PARTITION BY` nor `PARTITION BY EXCLUDING` the window runs
/// over every result row, as in Snowflake (fixture 4, W2/W5): `SUM(m) OVER
/// (ORDER BY d)` is a running total across the result, not one partition per
/// row. `PARTITION BY EXCLUDING` partitions by every queried dimension but the
/// excluded ones.
fn over_clause(
    def: &SemanticViewDefinition,
    resolved_dims: &[&Dimension],
    ws: &crate::model::WindowSpec,
) -> String {
    let partition_cols: Vec<String> =
        if ws.partition_dims.is_empty() && ws.excluding_dims.is_empty() {
            Vec::new()
        } else if ws.partition_dims.is_empty() {
            // Keyed through the shared resolver, so a quoted or dotted EXCLUDING
            // reference still matches its declared dimension.
            let excluding_set: HashSet<String> = ws
                .excluding_dims
                .iter()
                .map(|d| super::resolution::dim_ref_key(def, d))
                .collect();
            resolved_dims
                .iter()
                .filter(|d| !excluding_set.contains(&crate::ident::normalize_ident_part(&d.name)))
                .map(|d| quote_stored_ident(&d.name))
                .collect()
        } else {
            // Each listed dimension's resolved column, so a quoted or dotted
            // reference binds to the `__sv_agg` column, not a doubled-quote or
            // dotted non-column.
            ws.partition_dims
                .iter()
                .map(|d| window_dim_column(def, d))
                .collect()
        };
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
    over_parts.join(" ")
}

/// The query a window is computed over, built by [`inner_request`].
struct InnerRequest {
    /// The definition it reads: the inner metrics public, and a copy of an
    /// inner metric carrying a window's own `USING`.
    def: SemanticViewDefinition,
    /// Its metrics: the queried non-window ones, then each window's inner one.
    metrics: Vec<MetricName>,
    /// The `__sv_agg` column each window metric reads, by its position in
    /// `resolved_mets`.
    window_columns: HashMap<usize, String>,
}

/// The query a window is computed over (see [`expand_over_result`]).
///
/// # Errors
///
/// [`ExpandError::UnknownMetric`] for an inner metric no metric is named, and
/// [`ExpandError::WindowOverWindow`] for one that is a window metric.
fn inner_request(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_mets: &[&Metric],
) -> Result<InnerRequest, ExpandError> {
    let mut inner_def = def.clone();
    let mut inner_metrics: Vec<MetricName> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut add = |name: &str, inner_metrics: &mut Vec<MetricName>| {
        if seen.insert(crate::ident::normalize_ident_part(name)) {
            inner_metrics.push(MetricName::new(name));
        }
    };
    for met in resolved_mets.iter().filter(|m| !m.is_window()) {
        add(&met.name, &mut inner_metrics);
    }
    let mut window_columns: HashMap<usize, String> = HashMap::new();
    for (i, met) in resolved_mets.iter().enumerate() {
        let Some(ref ws) = met.window_spec else {
            continue;
        };
        let inner = def
            .metrics
            .iter()
            .find(|m| crate::ident::ident_matches(&m.name, &ws.inner_metric))
            .ok_or_else(|| {
                let available: Vec<String> = def.metrics.iter().map(|m| m.name.clone()).collect();
                ExpandError::UnknownMetric {
                    view_name: view_name.to_string(),
                    name: ws.inner_metric.clone(),
                    suggestion: crate::util::suggest_closest(&ws.inner_metric, &available),
                    available,
                }
            })?;
        if inner.is_window() {
            return Err(ExpandError::WindowOverWindow {
                view_name: view_name.to_string(),
                metric_name: met.name.clone(),
                inner_metric: inner.name.clone(),
            });
        }
        let column = if !met.using_relationships.is_empty() && inner.using_relationships.is_empty()
        {
            let name = format!("__sv_window_{i}");
            inner_def.metrics.push(Metric {
                name: name.clone(),
                using_relationships: met.using_relationships.clone(),
                ..inner.clone()
            });
            name
        } else {
            inner.name.clone()
        };
        add(&column, &mut inner_metrics);
        window_columns.insert(i, column);
    }
    let reads: HashSet<String> = inner_metrics
        .iter()
        .map(|m| crate::ident::normalize_ident_part(m.as_ref()))
        .collect();
    for m in &mut inner_def.metrics {
        if reads.contains(&crate::ident::normalize_ident_part(&m.name)) {
            m.access = AccessModifier::Public;
        }
    }
    Ok(InnerRequest {
        def: inner_def,
        metrics: inner_metrics,
        window_columns,
    })
}

/// Expand a query holding a window metric over the rows the rest of the query
/// gives, as Snowflake computes it (Phase 7 fixture 4, W2-W6).
///
/// # Strategy
///
/// 1. **CTE `__sv_agg`**: the query with each window metric replaced by the
///    metric it is computed over — the queried dimensions, the queried
///    non-window metrics and each window's inner metric, under the same
///    filter — expanded by `expand_inner` exactly as it would be on its own.
///    So every check and plan an ordinary query takes applies unchanged: a
///    dimension across a bridge counts each row once per group, a `USING`
///    follows its path (TECH-DEBT #109), a role reaches back across a
///    role-played table and a filter follows the metrics' role (#111).
///
/// 2. **Outer SELECT**: each window function over those rows, partitioned
///    and ordered by `__sv_agg`'s dimension columns; every other column is
///    read as it is. No GROUP BY: a window function is a row-level operation.
///
/// The inner metrics are made public in the definition the inner query reads,
/// because a window over a `PRIVATE` metric is queryable where its inner
/// metric alone is not. A window metric's own `USING` reaches an inner metric
/// that names none through a copy of that metric carrying it.
pub(super) fn expand_over_result(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
    resolved_dims: &[&Dimension],
    resolved_mets: &[&Metric],
    expand_inner: impl FnOnce(&SemanticViewDefinition, &QueryRequest) -> Result<String, ExpandError>,
) -> Result<String, ExpandError> {
    check_required_dimensions(view_name, def, resolved_dims, resolved_mets)?;

    let InnerRequest {
        def: inner_def,
        metrics: inner_metrics,
        window_columns,
    } = inner_request(view_name, def, resolved_mets)?;

    let inner_req = QueryRequest {
        dimensions: req.dimensions.clone(),
        metrics: inner_metrics,
        facts: Vec::new(),
        where_clause: req.where_clause.clone(),
    };
    let inner_sql = expand_inner(&inner_def, &inner_req)?;

    let mut items: Vec<SelectItem> = resolved_dims
        .iter()
        .map(|d| {
            SelectItem::new(
                quote_stored_ident(&d.name),
                None,
                quote_stored_ident(&d.name),
            )
        })
        .collect();
    for (i, met) in resolved_mets.iter().enumerate() {
        let expr = match (&met.window_spec, window_columns.get(&i)) {
            (Some(ws), Some(column)) => {
                let mut args = vec![quote_stored_ident(column)];
                args.extend(ws.extra_args.iter().cloned());
                format!(
                    "{}({}) OVER ({})",
                    ws.window_function,
                    args.join(", "),
                    over_clause(def, resolved_dims, ws)
                )
            }
            _ => quote_stored_ident(&met.name),
        };
        let output_type = if met.is_window() {
            met.output_type.clone()
        } else {
            None
        };
        items.push(SelectItem::new(
            expr,
            output_type,
            quote_stored_ident(&met.name),
        ));
    }

    let mut sql = String::with_capacity(inner_sql.len() + 256);
    sql.push_str("WITH __sv_agg AS (\n");
    sql.push_str(&inner_sql);
    sql.push_str("\n)\n");
    sql.push_str(
        &SelectSpec {
            where_clause: None,
            distinct: false,
            items,
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

    /// Regression for the `fuzz_sql_expand` crash (issue #145): a window
    /// metric whose `inner_metric` is a quoted identifier carrying an embedded
    /// quote (`"a""b"`, logical name `a"b`) that resolves to no declared
    /// metric. The CTE builder once emitted the normalized name as a BARE
    /// expression, leaking a lone `"`. The inner query now names the inner
    /// metric as a member, so a reference to no declared metric is refused as
    /// an unknown metric, with no SQL at all.
    #[test]
    fn window_over_no_declared_metric_is_refused() {
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

        let err = expand("test_view", &def, &req)
            .expect_err("a window over no declared metric is refused");
        assert!(
            matches!(&err, ExpandError::UnknownMetric { name, .. } if name == "\"a\"\"b\""),
            "got: {err}"
        );
    }

    /// Single window metric with 3 dims -- CTE with GROUP BY all dims,
    /// outer SELECT with window function and PARTITION BY (all minus excluded).
    #[test]
    fn test_window_single_metric_three_dims() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(s.quantity)")
            .with_dimension("date", "date", None)
            .with_dimension("year", "year", None)
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            .with_window_metric(
                "avg_7_w",
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
            metrics: vec![MetricName::new("total_qty_w"), MetricName::new("avg_7_w")],
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

    /// A window metric beside an aggregate metric: the aggregate is a column of
    /// `__sv_agg` beside the window's inner metric, read as it is (Snowflake,
    /// fixture 4 W2). It used to be refused as mixing the two.
    #[test]
    fn test_window_beside_an_aggregate_metric() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_metric("avg_price", "AVG(price)", None)
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w"), MetricName::new("avg_price")],
        };

        let sql = expand("test_view", &def, &req).unwrap();
        assert!(
            sql.contains("AVG(price) AS \"avg_price\""),
            "the aggregate is computed in __sv_agg: {sql}"
        );
        assert!(
            sql.contains("\"avg_price\" AS \"avg_price\""),
            "and read from it as it is: {sql}"
        );
        assert!(
            sql.contains("AVG(\"total_qty\") OVER (PARTITION BY \"store\" ORDER BY \"date\" ASC NULLS LAST) AS \"total_qty_w\""),
            "the window runs over the inner metric's column: {sql}"
        );
    }

    /// Missing required dimension (EXCLUDING dim not in query) returns error.
    #[test]
    fn test_window_missing_required_dim() {
        let def = minimal_def("sales", "store", "store", "total_qty", "SUM(quantity)")
            .with_dimension("date", "date", None)
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
    /// fan-out — but the CTE's inner aggregate was computed OVER the
    /// already-fanned join, so it was inflated before the window function
    /// ran. This fixture has the metric grain on `c` and dims on `a`, where
    /// a->c is `ManyToOne`, so `c`'s rows fan out.
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
            .with_window_metric(
                "total_balance_w",
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
            metrics: vec![MetricName::new("total_balance_w")],
        };

        // The inner aggregate goes through every check an ordinary query
        // does: the window runs over exactly the rows `total_balance` gives on
        // its own, here each customer once per account group (Phase 7.3, PR
        // 4c), never over the fanned join.
        let sql = expand("test_view", &def, &req).unwrap();
        let plain = expand(
            "test_view",
            &def,
            &QueryRequest {
                metrics: vec![MetricName::new("total_balance")],
                ..req.clone()
            },
        )
        .unwrap();
        assert!(
            sql.contains(&format!("WITH __sv_agg AS (\n{plain}\n)")),
            "the window must run over the plain query's rows: {sql}"
        );
    }

    /// Explicit PARTITION BY uses listed dims directly.
    #[test]
    fn test_partition_by_explicit() {
        let def = orders_view()
            .with_dimension("store", "o.store_id", Some("o"))
            .with_dimension("date", "o.sale_date", Some("o"))
            .with_metric("total_qty", "SUM(o.qty)", Some("o"))
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
            .with_window_metric(
                "total_qty_w",
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
            metrics: vec![MetricName::new("total_qty_w")],
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
