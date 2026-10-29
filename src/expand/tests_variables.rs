//! View variables (Phase 6.5): Snowflake's worked example and the binding
//! rules, with NUMBERS. Each test binds the variables as a query would
//! (`variables::bind`), expands, puts the values in place of the
//! `__sv_param(<n>)` calls as the shim does, and runs the SQL against an
//! in-memory `DuckDB`.

use super::*;
use crate::model::{Dimension, Fact, Metric, SemanticViewDefinition, TableRef, Variable};
use crate::variables::{bind, Values};

/// Snowflake's `sales_analysis` data: four sales.
fn sales_db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE sales (sale_id INTEGER, region VARCHAR, amount INTEGER);
         INSERT INTO sales VALUES (1, 'West', 1000), (2, 'West', 2500), (3, 'East', 500),
                                  (4, 'East', 3000);",
    )
    .expect("fixture data");
    con
}

fn var(name: &str, data_type: Option<&str>, default: Option<&str>) -> Variable {
    Variable {
        name: name.to_string(),
        data_type: data_type.map(str::to_string),
        default: default.map(str::to_string),
        comment: None,
    }
}

/// Snowflake's `sales_analysis` view, from the VARIABLES guide.
fn sales_analysis() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "sales".to_string(),
            table: "sales".to_string(),
            pk_columns: vec!["sale_id".to_string()],
            ..Default::default()
        }],
        variables: vec![
            var("min_amount", Some("INTEGER"), Some("1000")),
            var("multiplier", Some("DOUBLE"), Some("1.0")),
            var("target_region", Some("VARCHAR"), None),
        ],
        facts: vec![Fact {
            name: "adjusted_amount".to_string(),
            expr: "amount * multiplier".to_string(),
            source_table: Some("sales".to_string()),
            ..Default::default()
        }],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: Some("sales".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "sale_category".to_string(),
                expr: "CASE WHEN amount >= min_amount THEN 'HIGH' ELSE 'LOW' END".to_string(),
                source_table: Some("sales".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![
            Metric {
                name: "total_sales".to_string(),
                expr: "SUM(amount)".to_string(),
                source_table: Some("sales".to_string()),
                ..Default::default()
            },
            Metric {
                name: "weighted_sales".to_string(),
                expr: "SUM(amount * multiplier)".to_string(),
                source_table: Some("sales".to_string()),
                ..Default::default()
            },
            Metric {
                name: "adjusted_total".to_string(),
                expr: "SUM(sales.adjusted_amount)".to_string(),
                source_table: Some("sales".to_string()),
                ..Default::default()
            },
        ],
        ..Default::default()
    }
}

/// Bind `set` (name, SQL literal) as a query would, expand the request and
/// run it, the literals in place of the `__sv_param` calls. Rows sorted.
fn run(
    def: &SemanticViewDefinition,
    set: &[(&str, &str)],
    dims: &[&str],
    metrics: &[&str],
    where_clause: Option<&str>,
) -> Result<Vec<Vec<String>>, String> {
    let names: Vec<String> = set.iter().map(|(n, _)| (*n).to_string()).collect();
    let bound = bind(
        "sales_analysis",
        def.clone(),
        Values::Query {
            names: &names,
            first_slot: 1,
        },
    )?;
    let req = QueryRequest {
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: metrics.iter().map(|m| MetricName::new(*m)).collect(),
        facts: vec![],
        where_clause: where_clause.map(|w| bound.apply(w)),
    };
    let mut sql = expand("sales_analysis", &bound.def, &req).map_err(|e| e.to_string())?;
    for (i, (_, literal)) in set.iter().enumerate() {
        sql = sql.replace(&format!("__sv_param({})", i + 1), literal);
    }
    let con = sales_db();
    let mut stmt = con.prepare(&sql).map_err(|e| format!("{e}\n{sql}"))?;
    let mut rows: Vec<Vec<String>> = stmt
        .query_map([], |row| {
            let n = row.as_ref().column_count();
            (0..n)
                .map(|i| {
                    let v: duckdb::types::Value = row.get(i)?;
                    Ok(format!("{v:?}"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| format!("{e}\n{sql}"))?
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{e}\n{sql}"))?;
    rows.sort();
    Ok(rows)
}

/// Snowflake's worked example: `VARIABLES min_amount = 2000,
/// target_region => 'West', multiplier = 1.5`, with `target_region` used
/// only in the query's WHERE. West's 1000 sale is LOW and its 2500 sale HIGH
/// against 2000, each weighted by 1.5.
#[test]
fn snowflakes_worked_example() {
    let rows = run(
        &sales_analysis(),
        &[
            ("min_amount", "2000"),
            ("target_region", "'West'"),
            ("multiplier", "1.5"),
        ],
        &["region", "sale_category"],
        &["weighted_sales"],
        Some("region = target_region"),
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            vec!["Text(\"West\")", "Text(\"HIGH\")", "Double(3750.0)"],
            vec!["Text(\"West\")", "Text(\"LOW\")", "Double(1500.0)"],
        ]
    );
}

/// A variable left unset takes its default, cast to its type.
#[test]
fn an_unset_variable_takes_its_default() {
    let rows = run(
        &sales_analysis(),
        &[("target_region", "'East'")],
        &["sale_category"],
        &["weighted_sales", "total_sales"],
        Some("region = target_region"),
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            vec!["Text(\"HIGH\")", "Double(3000.0)", "HugeInt(3000)"],
            vec!["Text(\"LOW\")", "Double(500.0)", "HugeInt(500)"],
        ]
    );
}

/// A metric over a fact that uses a variable takes the query's value.
#[test]
fn a_metric_over_a_fact_uses_the_value() {
    let rows = run(
        &sales_analysis(),
        &[("target_region", "''"), ("multiplier", "2")],
        &["region"],
        &["adjusted_total"],
        None,
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            vec!["Text(\"East\")", "Double(7000.0)"],
            vec!["Text(\"West\")", "Double(7000.0)"],
        ]
    );
}

/// A query that leaves a variable with no default unset fails, as in
/// Snowflake, even when no member it asks for uses the variable.
#[test]
fn an_unset_variable_with_no_default_fails_the_query() {
    let e = run(&sales_analysis(), &[], &["region"], &["total_sales"], None).unwrap_err();
    assert!(e.contains("variable 'target_region' has no default"), "{e}");
}

#[test]
fn an_unknown_or_twice_set_variable_is_refused() {
    let e = run(
        &sales_analysis(),
        &[("target_region", "'x'"), ("multipler", "2")],
        &["region"],
        &["total_sales"],
        None,
    )
    .unwrap_err();
    assert!(
        e.contains("has no variable 'multipler'") && e.contains("Did you mean 'multiplier'?"),
        "{e}"
    );
    let e = run(
        &sales_analysis(),
        &[("target_region", "'x'"), ("TARGET_REGION", "'y'")],
        &["region"],
        &["total_sales"],
        None,
    )
    .unwrap_err();
    assert!(e.contains("is set twice"), "{e}");
}

/// An unqualified name is the variable, a qualified one the column, as in
/// Snowflake.
#[test]
fn a_qualified_name_is_the_column() {
    let mut def = sales_analysis();
    def.variables
        .push(var("amount", Some("INTEGER"), Some("1")));
    def.metrics.push(Metric {
        name: "mixed".to_string(),
        expr: "SUM(sales.amount * amount)".to_string(),
        source_table: Some("sales".to_string()),
        ..Default::default()
    });
    let rows = run(
        &def,
        &[("target_region", "''"), ("amount", "10")],
        &[],
        &["mixed"],
        None,
    )
    .unwrap();
    assert_eq!(rows, vec![vec!["HugeInt(70000)"]]);
}

/// A materialization covering a member that uses a variable, directly or
/// through another member, is not used: its table was computed with values
/// the query may not have.
#[test]
fn a_materialization_over_a_variable_is_not_used() {
    let mut def = sales_analysis();
    def.materializations = vec![
        crate::model::Materialization {
            name: "by_region".to_string(),
            table: "mat_by_region".to_string(),
            dimensions: vec!["region".to_string()],
            metrics: vec!["adjusted_total".to_string()],
        },
        crate::model::Materialization {
            name: "plain".to_string(),
            table: "mat_plain".to_string(),
            dimensions: vec!["region".to_string()],
            metrics: vec!["total_sales".to_string()],
        },
    ];
    let names = vec!["target_region".to_string()];
    let bound = bind(
        "v",
        def,
        Values::Query {
            names: &names,
            first_slot: 1,
        },
    )
    .unwrap();
    let kept: Vec<&str> = bound
        .def
        .materializations
        .iter()
        .map(|m| m.name.as_str())
        .collect();
    assert_eq!(kept, vec!["plain"]);
}

/// A probe needs no value: each variable is its default, as `create_probe`
/// binds it, or a NULL of its type when it has none. A default keeps a member
/// whose function refuses a NULL constant (`quantile_cont`) bindable.
#[test]
fn a_probe_types_a_member_without_a_value() {
    let mut def = sales_analysis();
    def.dimensions.push(Dimension {
        name: "by_region".to_string(),
        expr: "region = target_region".to_string(),
        source_table: Some("sales".to_string()),
        ..Default::default()
    });
    let def = crate::variables::for_probe(&def);
    let dim = &def.dimensions[1].expr;
    assert!(dim.contains("(CAST((1000) AS INTEGER))"), "{dim}");
    let fact = &def.facts[0].expr;
    assert!(fact.contains("(CAST((1.0) AS DOUBLE))"), "{fact}");
    let no_default = &def.dimensions[2].expr;
    assert!(
        no_default.contains("(CAST(NULL AS VARCHAR))"),
        "{no_default}"
    );
}

/// A derived metric reads a variable like any metric: CREATE's derived-metric
/// check knows the variable's name, and the expansion takes the query's value.
#[test]
fn a_derived_metric_reads_a_variable() {
    let mut def = sales_analysis();
    def.metrics.push(Metric {
        name: "scaled".to_string(),
        expr: "total_sales * multiplier".to_string(),
        ..Default::default()
    });
    crate::graph::validate_derived_metrics(&def).expect("CREATE accepts the derived metric");
    let rows = run(
        &def,
        &[("target_region", "''"), ("multiplier", "3")],
        &["region"],
        &["scaled"],
        None,
    )
    .unwrap();
    assert_eq!(
        rows,
        vec![
            vec!["Text(\"East\")", "Double(10500.0)"],
            vec!["Text(\"West\")", "Double(10500.0)"],
        ]
    );
}

/// The variables `x` and `last`, bound as a query that sets neither.
fn bound_x_and_last() -> crate::variables::Bound {
    let mut def = sales_analysis();
    def.variables.push(var("x", Some("INTEGER"), Some("1")));
    def.variables.push(var("last", Some("INTEGER"), Some("2")));
    bind(
        "v",
        def,
        Values::Query {
            names: &["target_region".to_string()],
            first_slot: 1,
        },
    )
    .unwrap()
}

/// A lambda's parameter is not the variable of the same name.
#[test]
fn a_lambda_parameter_is_not_the_variable() {
    let out = bound_x_and_last().apply("len(list_filter([1, 2], lambda x: x > 1)) + x");
    assert_eq!(
        out,
        "len(list_filter([1, 2], lambda x: x > 1)) + (CAST((1) AS INTEGER))"
    );
}

/// A keyword is not the variable of the same name.
#[test]
fn a_keyword_is_not_the_variable() {
    let out = bound_x_and_last().apply("first(region ORDER BY region NULLS LAST) || last");
    assert_eq!(
        out,
        "first(region ORDER BY region NULLS LAST) || (CAST((2) AS INTEGER))"
    );
}

/// A quote inside a comment does not hide the names after it.
#[test]
fn a_comment_does_not_hide_a_variable() {
    let out = bound_x_and_last().apply("-- don't\nregion > x");
    assert!(out.ends_with("\nregion > (CAST((1) AS INTEGER))"), "{out}");
}

/// A variable's type is one type: it can't close the cast it is written into
/// and carry a subquery past the rule defaults follow (the YAML form takes
/// the type as written).
#[test]
fn a_type_is_one_type() {
    let mut failures = Vec::new();
    for (t, want) in [
        (
            "INTEGER) + (SELECT max(x) FROM other",
            "its brackets do not nest",
        ),
        ("INTEGER) + (1", "its brackets do not nest"),
        ("DECIMAL(10, 2) + (SELECT 1)", "holds a subquery"),
        ("INTEGER; DROP TABLE t", "holds a ';'"),
    ] {
        let mut def = sales_analysis();
        def.variables.push(var("k", Some(t), Some("1")));
        match crate::variables::validate(&def) {
            Err(e) if e.contains("variable 'k' has an invalid type") && e.contains(want) => {}
            other => failures.push(format!("{t}: {other:?}")),
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
    let mut def = sales_analysis();
    def.variables
        .push(var("k", Some("DECIMAL(10, 2)"), Some("1")));
    crate::variables::validate(&def).expect("a type with arguments");
}
