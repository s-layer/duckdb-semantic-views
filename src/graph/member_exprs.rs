//! CREATE-time checks on what a member expression may hold (TECH-DEBT #95).
//!
//! Two shapes used to be stored and only failed, or quietly did nothing, at
//! query time:
//!
//! - an aggregate in a **dimension** (`o.d AS SUM(o.amount)`). A dimension is
//!   a row-level value the query groups by, and `GROUP BY` cannot hold an
//!   aggregate, so no query could use it. Snowflake rejects it at `CREATE`.
//!   The same holds for a **fact**, a row-level value a metric aggregates (an
//!   aggregate of an aggregate is a binder error); the CREATE reference already
//!   said facts refuse one. An aggregate inside a subquery
//!   (`(SELECT max(t) FROM tiers)`) is a scalar per row and stays legal.
//! - a prepared-statement **parameter** in any member (`o.p AS $1`). A member
//!   is expanded inside the view's own query, where no parameter has a value,
//!   so every query that reached it was refused at bind (#105). Parameters
//!   belong in the WHERE clause of a `SEMANTIC_VIEW(...)` query.
//!
//! Both checks run in `ddl::define::enrich_definition_for_create`, the choke
//! point every `CREATE` form (the AS body and the YAML forms) passes through.

use crate::errors::ParseError;
use crate::expr_tokens::{call_is_windowed, find_parameters, scan_function_heads, subquery_spans};
use crate::model::SemanticViewDefinition;

/// Reject an aggregate in a dimension or fact expression and a parameter in any
/// member expression.
pub fn validate_member_expressions(def: &SemanticViewDefinition) -> Result<(), ParseError> {
    let row_level = def
        .dimensions
        .iter()
        .map(|d| ("dimension", &d.name, &d.expr))
        .chain(def.facts.iter().map(|f| ("fact", &f.name, &f.expr)));
    for (kind, name, expr) in row_level {
        if let Some(func) = aggregate_outside_subqueries(expr) {
            return Err(ParseError::positionless(format!(
                "semantic view: {kind} '{name}' must not contain the aggregate function \
                 '{func}'. A {kind} is a row-level value; define a metric for the \
                 aggregate."
            )));
        }
    }

    let members = def
        .dimensions
        .iter()
        .map(|d| ("dimension", &d.name, &d.expr))
        .chain(def.facts.iter().map(|f| ("fact", &f.name, &f.expr)))
        .chain(def.metrics.iter().flat_map(|m| {
            // A window metric's spec is emitted into the query on its own, so
            // its parts are scanned alongside the expression.
            let spec = m.window_spec.iter().flat_map(|ws| {
                ws.extra_args
                    .iter()
                    .chain(ws.order_by.iter().map(|o| &o.expr))
                    .chain(ws.frame_clause.iter())
            });
            std::iter::once(&m.expr)
                .chain(spec)
                .map(move |expr| ("metric", &m.name, expr))
        }));
    for (kind, name, expr) in members {
        if let Some((_, parameter)) = find_parameters(expr).into_iter().next() {
            return Err(ParseError::positionless(format!(
                "semantic view: {kind} '{name}' holds the prepared statement parameter \
                 {parameter}, which has no value inside the view's definition. Use \
                 parameters in the WHERE clause of a SEMANTIC_VIEW(...) query instead."
            )));
        }
    }

    Ok(())
}

/// The first known aggregate called in `expr` outside its subqueries and not
/// windowed (`SUM(x) OVER (…)` is one value per row), if any.
fn aggregate_outside_subqueries(expr: &str) -> Option<&'static str> {
    let spans = subquery_spans(expr);
    scan_function_heads(expr).into_iter().find_map(|head| {
        if spans
            .iter()
            .any(|&(start, end)| start <= head.start && head.start < end)
            || call_is_windowed(expr, &head)
        {
            return None;
        }
        super::derived_metrics::aggregate_name(&head.last_part_key())
    })
}

#[cfg(test)]
mod tests {
    use super::validate_member_expressions;
    use crate::model::{
        Dimension, Fact, Metric, SemanticViewDefinition, TableRef, WindowOrderBy, WindowSpec,
    };

    fn def_with(dims: &[(&str, &str)]) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                ..Default::default()
            }],
            dimensions: dims
                .iter()
                .map(|(name, expr)| Dimension {
                    name: (*name).to_string(),
                    expr: (*expr).to_string(),
                    source_table: Some("o".to_string()),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn err(def: &SemanticViewDefinition) -> String {
        validate_member_expressions(def).unwrap_err().message
    }

    #[test]
    fn an_aggregate_dimension_is_rejected() {
        let msg = err(&def_with(&[("revenue", "SUM(o.amount)")]));
        assert!(
            msg.contains("dimension 'revenue'") && msg.contains("'sum'"),
            "{msg}"
        );
    }

    #[test]
    fn a_qualified_or_nested_aggregate_dimension_is_rejected() {
        let msg = err(&def_with(&[("d", "coalesce(main.max(o.x), 0)")]));
        assert!(
            msg.contains("dimension 'd'") && msg.contains("'max'"),
            "{msg}"
        );
    }

    #[test]
    fn an_aggregate_inside_a_subquery_dimension_is_kept() {
        let def = def_with(&[("tier", "(SELECT max(t) FROM tiers)")]);
        assert!(validate_member_expressions(&def).is_ok());
    }

    #[test]
    fn a_dimension_that_only_looks_like_an_aggregate_is_kept() {
        // A column named like an aggregate, the name in a string, and a
        // quoted function name holding a dot are not calls of the aggregate.
        let def = def_with(&[
            ("c", "o.count"),
            ("s", "'sum(x)'"),
            ("q", "\"main.sum\"(o.x)"),
            ("u", "upper(o.region)"),
        ]);
        assert!(validate_member_expressions(&def).is_ok());
    }

    #[test]
    fn an_aggregate_fact_is_rejected() {
        let mut def = def_with(&[]);
        def.facts.push(Fact {
            name: "f".to_string(),
            expr: "o.x / count(o.y)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        let msg = err(&def);
        assert!(msg.contains("fact 'f'") && msg.contains("'count'"), "{msg}");
    }

    #[test]
    fn an_aggregate_inside_a_subquery_fact_is_kept() {
        let mut def = def_with(&[]);
        def.facts.push(Fact {
            name: "f".to_string(),
            expr: "o.x / (SELECT sum(t) FROM totals)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        assert!(validate_member_expressions(&def).is_ok());
    }

    #[test]
    fn a_parameter_dimension_is_rejected() {
        for p in ["$1", "?", "?2", "$name"] {
            let msg = err(&def_with(&[("p", &format!("o.x + {p}"))]));
            assert!(
                msg.contains("dimension 'p'") && msg.contains(&format!("parameter {p},")),
                "{p}: {msg}"
            );
        }
    }

    #[test]
    fn a_parameter_in_a_subquery_dimension_is_rejected() {
        let msg = err(&def_with(&[(
            "p",
            "(SELECT max(t) FROM tiers WHERE t > $1)",
        )]));
        assert!(msg.contains("parameter $1,"), "{msg}");
    }

    #[test]
    fn a_parameter_fact_is_rejected() {
        let mut def = def_with(&[]);
        def.facts.push(Fact {
            name: "f".to_string(),
            expr: "o.x * $1".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        let msg = err(&def);
        assert!(
            msg.contains("fact 'f'") && msg.contains("parameter $1,"),
            "{msg}"
        );
    }

    #[test]
    fn a_parameter_metric_is_rejected() {
        let mut def = def_with(&[]);
        def.metrics.push(Metric {
            name: "m".to_string(),
            expr: "SUM(o.x) * ?".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        let msg = err(&def);
        assert!(
            msg.contains("metric 'm'") && msg.contains("parameter ?,"),
            "{msg}"
        );
    }

    #[test]
    fn dollar_text_that_is_not_a_parameter_is_kept() {
        // A dollar-quoted string, a `$` inside a name, a `?` in a string and a
        // `$1` in a comment hold no parameter.
        let def = def_with(&[
            ("a", "$$x $1 y$$"),
            ("b", "o.price$usd$"),
            ("c", "'?'"),
            ("d", "o.x -- $1\n"),
        ]);
        assert!(validate_member_expressions(&def).is_ok());
    }

    fn with_fact(def: &mut SemanticViewDefinition, name: &str, expr: &str) {
        def.facts.push(Fact {
            name: name.to_string(),
            expr: expr.to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
    }

    #[test]
    fn a_windowed_aggregate_is_kept() {
        // A windowed aggregate is one value per row, as `LAG(...) OVER (...)`
        // is; only a grouping aggregate is refused.
        let mut def = def_with(&[
            ("n", "COUNT(*) OVER (PARTITION BY o.customer_id)"),
            ("w", "sum(o.x) /* c */ OVER w"),
            ("f", "sum(o.x) FILTER (WHERE o.y > 0) OVER ()"),
        ]);
        with_fact(
            &mut def,
            "share",
            "o.amount / SUM(o.amount) OVER (PARTITION BY o.region)",
        );
        assert!(
            validate_member_expressions(&def).is_ok(),
            "{:?}",
            validate_member_expressions(&def)
        );
    }

    #[test]
    fn a_grouping_aggregate_beside_a_windowed_one_is_rejected() {
        let mut def = def_with(&[]);
        with_fact(&mut def, "f", "SUM(o.x) OVER () - max(o.x)");
        let msg = err(&def);
        assert!(msg.contains("fact 'f'") && msg.contains("'max'"), "{msg}");
    }

    #[test]
    fn a_filtered_aggregate_without_over_is_rejected() {
        let msg = err(&def_with(&[("d", "sum(o.x) FILTER (WHERE o.y > 0)")]));
        assert!(msg.contains("'sum'"), "{msg}");
    }

    #[test]
    fn duckdb_aggregates_beyond_the_standard_ones_are_rejected() {
        for (expr, func) in [
            ("arg_max(o.product, o.amount)", "arg_max"),
            ("first(o.x)", "first"),
            ("quantile_cont(o.x, 0.5)", "quantile_cont"),
            ("count_if(o.x > 0)", "count_if"),
            ("list(o.x)", "list"),
        ] {
            let msg = err(&def_with(&[("d", expr)]));
            assert!(msg.contains(&format!("'{func}'")), "{expr}: {msg}");
        }
    }

    /// Every aggregate the bundled `DuckDB` registers is on the list, so a
    /// dimension or fact cannot slip one past CREATE by name. (It needs the
    /// bundled engine, which the `extension` build does not link.)
    #[test]
    #[cfg(not(feature = "extension"))]
    fn every_duckdb_aggregate_is_known() {
        let conn = duckdb::Connection::open_in_memory().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT function_name FROM duckdb_functions() \
                 WHERE function_type = 'aggregate' ORDER BY 1",
            )
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert!(names.len() > 50, "{names:?}");
        // DuckDB lists its window-only functions as aggregates too. They take
        // `OVER` and yield one value per row, so they are not refused.
        const WINDOW_ONLY: &[&str] = &[
            "cume_dist",
            "dense_rank",
            "fill",
            "first_value",
            "lag",
            "last_value",
            "lead",
            "nth_value",
            "ntile",
            "percent_rank",
            "rank",
            "rank_dense",
            "row_number",
        ];
        let missing: Vec<&String> = names
            .iter()
            .filter(|n| !WINDOW_ONLY.contains(&n.as_str()))
            .filter(|n| crate::graph::derived_metrics::aggregate_name(&n.to_lowercase()).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "aggregates missing from the list: {missing:?}"
        );
    }

    fn window_metric(ws: WindowSpec) -> SemanticViewDefinition {
        let mut def = def_with(&[("region", "o.region")]);
        def.metrics.push(Metric {
            name: "m".to_string(),
            expr: "LAG(base) OVER ()".to_string(),
            window_spec: Some(ws),
            ..Default::default()
        });
        def
    }

    #[test]
    fn a_parameter_in_a_window_spec_is_rejected() {
        let specs = [
            WindowSpec {
                extra_args: vec!["$1".to_string()],
                ..Default::default()
            },
            WindowSpec {
                frame_clause: Some("ROWS BETWEEN $1 PRECEDING AND CURRENT ROW".to_string()),
                ..Default::default()
            },
            WindowSpec {
                order_by: vec![WindowOrderBy {
                    expr: "o.x + ?".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        ];
        for ws in specs {
            let msg = err(&window_metric(ws.clone()));
            assert!(
                msg.contains("metric 'm'") && msg.contains("parameter"),
                "{ws:?}: {msg}"
            );
        }
    }

    #[test]
    fn an_aggregate_metric_is_kept() {
        let mut def = def_with(&[("region", "o.region")]);
        def.metrics.push(Metric {
            name: "m".to_string(),
            expr: "SUM(o.x)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        assert!(validate_member_expressions(&def).is_ok());
    }
}
