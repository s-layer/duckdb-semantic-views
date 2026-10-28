use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    AccessModifier, Cardinality, Dimension, Join, Metric, SemanticViewDefinition,
};

// ---------------------------------------------------------------------------
// Test fixture definitions
// ---------------------------------------------------------------------------

/// Simple definition: base_table "orders", 3 dimensions, 3 metrics, 1 filter, no joins.
fn simple_definition() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![semantic_views::model::TableRef {
            alias: "orders".to_string(),
            table: "orders".to_string(),
            pk_columns: vec![],
            unique_constraints: vec![],
            comment: None,
            synonyms: vec![],
            tags: Vec::new(),
        }],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: None,

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
            Dimension {
                name: "month".to_string(),
                expr: "date_trunc('month', created_at)".to_string(),
                source_table: None,

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
            Dimension {
                name: "status".to_string(),
                expr: "status".to_string(),
                source_table: None,

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
        ],
        metrics: vec![
            Metric {
                name: "total_revenue".to_string(),
                expr: "sum(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
            Metric {
                name: "order_count".to_string(),
                expr: "count(*)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
            Metric {
                name: "avg_amount".to_string(),
                expr: "avg(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
        ],

        joins: vec![],
        // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
        facts: vec![],
        materializations: vec![],

        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
        comment: None,
        tags: Vec::new(),
    }
}

/// Joined definition: base_table "orders", 4 dimensions (2 with source_table),
/// 3 metrics (2 with source_table), 2 joins, 1 filter.
fn joined_definition() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            semantic_views::model::TableRef {
                alias: "orders".to_string(),
                table: "orders".to_string(),
                pk_columns: vec![],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
                tags: Vec::new(),
            },
            semantic_views::model::TableRef {
                alias: "customers".to_string(),
                table: "customers".to_string(),
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
                tags: Vec::new(),
            },
            semantic_views::model::TableRef {
                alias: "products".to_string(),
                table: "products".to_string(),
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
                tags: Vec::new(),
            },
        ],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: None,

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
            Dimension {
                name: "customer_name".to_string(),
                expr: "customers.name".to_string(),
                source_table: Some("customers".to_string()),

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
            Dimension {
                name: "month".to_string(),
                expr: "date_trunc('month', created_at)".to_string(),
                source_table: None,

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
            Dimension {
                name: "product_category".to_string(),
                expr: "products.category".to_string(),
                source_table: Some("products".to_string()),

                output_type: None,
                comment: None,
                synonyms: vec![],
                is_filter: false,
                tags: Vec::new(),
                sample_values: Vec::new(),
                is_enum: false,
                cortex_search_service: None,
            },
        ],
        metrics: vec![
            Metric {
                name: "total_revenue".to_string(),
                expr: "sum(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
            Metric {
                name: "customer_count".to_string(),
                expr: "count(DISTINCT customers.id)".to_string(),
                source_table: Some("customers".to_string()),
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
            Metric {
                name: "product_count".to_string(),
                expr: "count(DISTINCT products.id)".to_string(),
                source_table: Some("products".to_string()),
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
                tags: Vec::new(),
            },
        ],

        // `customer_count` / `product_count` count their OWN table's key. They
        // counted `orders.customer_id` / `orders.product_id` — a column of the
        // base table, which bound only while every metric was aggregated over
        // the base-anchored join. Each is now computed from its own table
        // (TECH-DEBT #71), where that column does not exist.
        joins: vec![
            // OneToOne so the fan-trap safety check passes for every generated
            // request — this property tests join *exclusion*, not cardinality.
            Join {
                table: "customers".to_string(),
                from_alias: "orders".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec!["id".to_string()],
                cardinality: Cardinality::OneToOne,
                ..Default::default()
            },
            Join {
                table: "products".to_string(),
                from_alias: "orders".to_string(),
                fk_columns: vec!["product_id".to_string()],
                ref_columns: vec!["id".to_string()],
                cardinality: Cardinality::OneToOne,
                ..Default::default()
            },
        ],
        // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
        facts: vec![],
        materializations: vec![],

        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
        comment: None,
        tags: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Bind oracle: a real in-memory DuckDB with physical tables matching the two
// fixtures, so expanded SQL can be bound (LIMIT 0) to prove it is not just
// string-shaped but actually valid — the gap TC-4 calls out ("SQL never
// executed"). One combined schema serves both fixtures: `orders` carries the
// superset of referenced columns; `customers`/`products` back the joins.
// ---------------------------------------------------------------------------

fn oracle_db() -> duckdb::Connection {
    let conn = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    conn.execute_batch(
        "CREATE TABLE orders (
             region      VARCHAR,
             status      VARCHAR,
             amount      DOUBLE,
             created_at  TIMESTAMP,
             customer_id INTEGER,
             product_id  INTEGER
         );
         CREATE TABLE customers (id INTEGER, name VARCHAR);
         CREATE TABLE products  (id INTEGER, category VARCHAR);",
    )
    .expect("create oracle schema");
    conn
}

/// Bind (but do not run) the expanded SQL against the oracle schema by
/// preparing it under a `LIMIT 0`. `prepare` performs full name/type binding
/// in DuckDB, so a forward-referencing join, a dropped table, or a malformed
/// clause surfaces here as an `Err` rather than passing a string-shape check.
fn assert_binds(conn: &duckdb::Connection, sql: &str) -> Result<(), String> {
    let probe = format!("{sql}\nLIMIT 0");
    conn.prepare(&probe)
        .map(|_| ())
        .map_err(|e| format!("expanded SQL failed to bind: {e}\n---\n{probe}"))
}

/// Extract the exact ordinal list from a `GROUP BY` clause. `expand` always
/// emits `GROUP BY` as the final clause with only integer ordinals, so every
/// comma-separated token must parse as a `usize` — a stray token means the
/// generator drifted. Returns `None` when there is no `GROUP BY`.
fn parse_group_by_ordinals(sql: &str) -> Option<Vec<usize>> {
    let tail = sql.split("GROUP BY").nth(1)?;
    Some(
        tail.split(',')
            .map(|t| {
                t.trim()
                    .parse::<usize>()
                    .unwrap_or_else(|_| panic!("non-ordinal token {t:?} in GROUP BY: {sql}"))
            })
            .collect(),
    )
}

// ---------------------------------------------------------------------------
// Strategy: generate arbitrary valid QueryRequest from a definition
// ---------------------------------------------------------------------------

/// Generate a random valid `QueryRequest` from a definition.
///
/// Dimensions: 0..all.
/// Metrics: 0..all.
/// At least one dimension or one metric is always present (both-empty is invalid).
fn arb_query_request(def: &SemanticViewDefinition) -> impl Strategy<Value = QueryRequest> {
    let dim_names: Vec<String> = def.dimensions.iter().map(|d| d.name.clone()).collect();
    let met_names: Vec<String> = def.metrics.iter().map(|m| m.name.clone()).collect();

    let dim_strategy = proptest::sample::subsequence(dim_names, 0..=def.dimensions.len());
    let met_strategy = proptest::sample::subsequence(met_names, 0..=def.metrics.len());

    (dim_strategy, met_strategy)
        .prop_filter("at least one dimension or metric", |(dims, mets)| {
            !dims.is_empty() || !mets.is_empty()
        })
        .prop_map(|(dims, mets)| QueryRequest {
            dimensions: dims.into_iter().map(DimensionName::new).collect(),
            metrics: mets.into_iter().map(MetricName::new).collect(),
            // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
            facts: vec![],
            // No predicate, deliberately: this generator covers the unfiltered
            // shapes and the invariants asserted below are structural claims
            // about dimension/metric expansion (GROUP BY ordinals, DISTINCT,
            // alias quoting), none of which a WHERE clause participates in.
            //
            // TC-12: this used to say randomized `where_clause` coverage was
            // "not yet written". It exists now — PBT-6 (code-review 2026-08-03)
            // added an AST-generated predicate, rendered once in member names
            // for the request and once in raw columns for the oracle, to the
            // numeric differential harnesses: `differential_proptest`,
            // `star_schema_proptest`, `multi_hop_join_proptest`,
            // `semi_additive_proptest` and `window_metric_proptest`. Those are
            // the harnesses that compare numbers, which is where a predicate
            // bug shows up; leaving it `None` here keeps this file's
            // no-predicate shapes covered rather than duplicating them.
            // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
            where_clause: None,
        })
}

// ---------------------------------------------------------------------------
// Property-based tests
// ---------------------------------------------------------------------------

proptest! {
    /// Property 1: Dimensions control aggregation mode.
    /// - Dimensions + metrics: GROUP BY uses ordinals for all dimensions, and
    ///   each dimension expression appears in SELECT.
    /// - Dimensions only (no metrics): SELECT DISTINCT, no GROUP BY.
    /// - Metrics only (no dimensions): no GROUP BY (global aggregate).
    #[test]
    fn dimensions_control_aggregation(req in arb_query_request(&simple_definition())) {
        let def = simple_definition();
        let sql = expand("test", &def, &req).unwrap();

        // Bind oracle: the expanded SQL must be valid against a real schema.
        let conn = oracle_db();
        if let Err(e) = assert_binds(&conn, &sql) {
            prop_assert!(false, "{e}");
        }

        if req.dimensions.is_empty() {
            // Metrics-only: global aggregate, no GROUP BY.
            prop_assert!(
                !sql.contains("GROUP BY"),
                "Empty dimensions should produce no GROUP BY. SQL:\n{sql}"
            );
        } else if req.metrics.is_empty() {
            // Dimensions-only: SELECT DISTINCT, no GROUP BY.
            prop_assert!(
                sql.contains("SELECT DISTINCT"),
                "Dimensions-only should use SELECT DISTINCT. SQL:\n{sql}"
            );
            prop_assert!(
                !sql.contains("GROUP BY"),
                "Dimensions-only should not produce GROUP BY. SQL:\n{sql}"
            );
            // All dimension expressions appear in SELECT
            for dim_name in &req.dimensions {
                let dim_def = def.dimensions.iter()
                    .find(|d| d.name.eq_ignore_ascii_case(dim_name))
                    .unwrap();
                prop_assert!(
                    sql.contains(&dim_def.expr),
                    "SELECT DISTINCT must contain expr '{}' for dimension '{}'. SQL:\n{}",
                    dim_def.expr, dim_name, sql
                );
            }
        } else {
            // Both dimensions and metrics: GROUP BY with ordinal positions for
            // each dimension, and all dimension expressions present in SELECT.
            //
            // Parse the ordinal list EXACTLY (not `.contains("1")`, which the
            // review flagged as matching any digit anywhere): the GROUP BY must
            // be precisely `1, 2, ..., dim_count` — same count, same values, in
            // order, with no extras.
            let dim_count = req.dimensions.len();
            let ordinals = parse_group_by_ordinals(&sql)
                .expect("GROUP BY must exist when both dimensions and metrics present");
            let expected: Vec<usize> = (1..=dim_count).collect();
            prop_assert_eq!(
                &ordinals,
                &expected,
                "GROUP BY ordinals must be exactly {:?}, got {:?}. SQL:\n{}",
                expected,
                ordinals,
                sql
            );

            // Verify dimension expressions appear in the SELECT clause
            // (before the GROUP BY).
            let select_section = sql.split("GROUP BY").next().unwrap();
            for dim_name in &req.dimensions {
                let dim_def = def.dimensions.iter()
                    .find(|d| d.name.eq_ignore_ascii_case(dim_name))
                    .unwrap();
                prop_assert!(
                    select_section.contains(&dim_def.expr),
                    "SELECT must contain expr '{}' for dimension '{}'. SELECT section:\n{}",
                    dim_def.expr, dim_name, select_section
                );
            }
        }
    }

    /// Property 2: All requested dimensions and metrics appear as aliases in SELECT.
    #[test]
    fn all_dimensions_and_metrics_in_select(req in arb_query_request(&simple_definition())) {
        let def = simple_definition();
        let sql = expand("test", &def, &req).unwrap();

        for dim_name in &req.dimensions {
            let dim_def = def.dimensions.iter()
                .find(|d| d.name.eq_ignore_ascii_case(dim_name))
                .unwrap();
            let alias = format!("AS \"{}\"", dim_def.name);
            prop_assert!(
                sql.contains(&alias),
                "SELECT must contain alias '{alias}' for dimension '{dim_name}'. SQL:\n{sql}"
            );
        }
        for met_name in &req.metrics {
            let met_def = def.metrics.iter()
                .find(|m| m.name.eq_ignore_ascii_case(met_name))
                .unwrap();
            let alias = format!("AS \"{}\"", met_def.name);
            prop_assert!(
                sql.contains(&alias),
                "SELECT must contain alias '{alias}' for metric '{met_name}'. SQL:\n{sql}"
            );
        }
    }

    /// Property 3: SQL structure is valid (SELECT, FROM present; GROUP BY iff dims+metrics).
    #[test]
    fn sql_structure_valid(req in arb_query_request(&simple_definition())) {
        let def = simple_definition();
        let sql = expand("test", &def, &req).unwrap();

        prop_assert!(
            sql.starts_with("SELECT"),
            "SQL must start with SELECT. SQL:\n{sql}"
        );
        prop_assert!(
            sql.contains("FROM \"orders\""),
            "SQL must contain FROM base table. SQL:\n{sql}"
        );
        // GROUP BY only when BOTH dimensions and metrics are present.
        if !req.dimensions.is_empty() && !req.metrics.is_empty() {
            prop_assert!(
                sql.contains("GROUP BY"),
                "Both dims + metrics must produce GROUP BY. SQL:\n{sql}"
            );
        }
        // Dimensions-only must use SELECT DISTINCT without GROUP BY.
        if !req.dimensions.is_empty() && req.metrics.is_empty() {
            prop_assert!(
                sql.contains("SELECT DISTINCT"),
                "Dimensions-only must use SELECT DISTINCT. SQL:\n{sql}"
            );
            prop_assert!(
                !sql.contains("GROUP BY"),
                "Dimensions-only must NOT use GROUP BY. SQL:\n{sql}"
            );
        }
    }

    /// Property 4: Joins are only included when a requested dim/metric needs them.
    #[test]
    fn joins_only_when_needed(req in arb_query_request(&joined_definition())) {
        let def = joined_definition();
        let sql = expand("test", &def, &req).unwrap();

        // Bind oracle: exercises join emission against a real schema, so a
        // forward-referencing ON clause or a dropped connecting join (SG-2/SG-10)
        // would fail to bind here, not just fail a substring check.
        let conn = oracle_db();
        if let Err(e) = assert_binds(&conn, &sql) {
            prop_assert!(false, "{e}");
        }

        for join in &def.joins {
            let join_table_needed = req.dimensions.iter().any(|d| {
                def.dimensions.iter()
                    .find(|dd| dd.name.eq_ignore_ascii_case(d))
                    .and_then(|dd| dd.source_table.as_ref())
                    .map_or(false, |st: &String| st.eq_ignore_ascii_case(&join.table))
            }) || req.metrics.iter().any(|m| {
                def.metrics.iter()
                    .find(|mm| mm.name.eq_ignore_ascii_case(m))
                    .and_then(|mm| mm.source_table.as_ref())
                    .map_or(false, |st: &String| st.eq_ignore_ascii_case(&join.table))
            });

            let join_marker = format!("JOIN \"{}\"", join.table);
            if !join_table_needed {
                prop_assert!(
                    !sql.contains(&join_marker),
                    "JOIN '{}' should NOT be included when no requested dim/metric uses it. SQL:\n{}",
                    join.table, sql
                );
            }
        }
    }

    /// Property 5: Global aggregate — any NON-EMPTY subset of metrics with no
    /// dimensions produces no GROUP BY, includes every requested metric's expr,
    /// and binds.
    ///
    /// Previously this property took `Just(<fixed request>)` — a unit test in
    /// proptest costume (TC-4). It now samples a real, varying metrics-only
    /// subset so the invariant is checked across every combination.
    #[test]
    fn global_aggregate_no_group_by(
        metrics in proptest::sample::subsequence(
            simple_definition().metrics.iter().map(|m| m.name.clone()).collect::<Vec<_>>(),
            1..=simple_definition().metrics.len(),
        )
    ) {
        let def = simple_definition();
        let req = QueryRequest {
            // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
            where_clause: None,
            dimensions: vec![],
            metrics: metrics.iter().map(MetricName::new).collect(),
            // PIN: structural harness -- asserts expand() shape/no-panic, not numbers; the numeric coverage for this field lives in the differential harnesses. TECH-DEBT #66
            facts: vec![],
        };
        let sql = expand("test", &def, &req).unwrap();

        let conn = oracle_db();
        if let Err(e) = assert_binds(&conn, &sql) {
            prop_assert!(false, "{e}");
        }

        prop_assert!(
            !sql.contains("GROUP BY"),
            "Global aggregate must not contain GROUP BY. SQL:\n{sql}"
        );
        for met_name in &metrics {
            let met_def = def.metrics.iter()
                .find(|m| &m.name == met_name)
                .unwrap();
            prop_assert!(
                sql.contains(&met_def.expr),
                "Global aggregate SQL must contain metric expr '{}'. SQL:\n{}",
                met_def.expr, sql
            );
        }
    }
}
