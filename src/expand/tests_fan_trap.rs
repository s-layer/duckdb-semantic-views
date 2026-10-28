//! Fan-trap / chasm-trap detection during expansion.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase31_fan_trap_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::{minimal_def, TestFixtureExt};
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

fn fan_trap_three_table_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "li".to_string(),
                table: "line_items".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "c".to_string(),
                table: "customers".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "status".to_string(),
                expr: "li.status".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "segment".to_string(),
                expr: "c.segment".to_string(),
                source_table: Some("c".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![
            Metric {
                name: "revenue".to_string(),
                expr: "SUM(li.extended_price)".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
            Metric {
                name: "order_count".to_string(),
                expr: "COUNT(*)".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
        ],
        joins: vec![
            Join {
                table: "o".to_string(),
                from_alias: "li".to_string(),
                fk_columns: vec!["order_id".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("li_to_order".to_string()),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            },
            Join {
                table: "c".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("order_to_customer".to_string()),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            },
        ],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

#[test]
fn fan_trap_one_to_many_blocked() {
    let def = fan_trap_three_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("order_count")],
    };
    let result = expand("sales", &def, &req);
    assert!(result.is_err(), "Fan trap must block the query");
    match result.unwrap_err() {
        ExpandError::FanTrap { detail } => {
            assert_eq!(detail.view_name, "sales");
            assert_eq!(detail.metric_name, "order_count");
            assert_eq!(detail.dimension_name, "status");
        }
        other => panic!("Expected FanTrap, got: {other}"),
    }
}

#[test]
fn fan_trap_many_to_one_safe() {
    let def = fan_trap_three_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("revenue")],
    };
    let result = expand("sales", &def, &req);
    assert!(
        result.is_ok(),
        "MANY TO ONE direction must be safe: {:?}",
        result.err()
    );
}

#[test]
fn fan_trap_one_to_one_safe() {
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "d".to_string(),
                table: "details".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "detail".to_string(),
            expr: "d.detail".to_string(),
            source_table: Some("d".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "cnt".to_string(),
            expr: "COUNT(*)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![Join {
            table: "d".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["detail_id".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("order_to_detail".to_string()),
            cardinality: Cardinality::OneToOne,
            ..Default::default()
        }],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("detail")],
        metrics: vec![MetricName::new("cnt")],
    };
    let result = expand("test", &def, &req);
    assert!(
        result.is_ok(),
        "ONE TO ONE must be safe: {:?}",
        result.err()
    );
}

#[test]
fn fan_trap_same_table_safe() {
    let def = fan_trap_three_table_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("revenue")],
    };
    let result = expand("sales", &def, &req);
    assert!(
        result.is_ok(),
        "Same table must be safe: {:?}",
        result.err()
    );
}

#[test]
fn fan_trap_no_joins_safe() {
    let def = minimal_def("orders", "region", "region", "cnt", "COUNT(*)");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("cnt")],
    };
    let result = expand("test", &def, &req);
    assert!(result.is_ok(), "No joins must be safe: {:?}", result.err());
}

#[test]
fn fan_trap_transitive_chain() {
    let mut def = fan_trap_three_table_def();
    def.metrics.push(Metric {
        name: "customer_count".to_string(),
        expr: "COUNT(DISTINCT c.id)".to_string(),
        source_table: Some("c".to_string()),
        ..Default::default()
    });
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("customer_count")],
    };
    let result = expand("sales", &def, &req);
    assert!(
        result.is_err(),
        "Transitive chain fan trap must be detected"
    );
    match result.unwrap_err() {
        ExpandError::FanTrap { detail } => {
            assert_eq!(detail.metric_name, "customer_count");
            assert_eq!(detail.dimension_name, "status");
        }
        other => panic!("Expected FanTrap, got: {other}"),
    }
}

#[test]
fn fan_trap_derived_metric_blocked() {
    let mut def = fan_trap_three_table_def();
    def.metrics.push(Metric {
        name: "avg_order".to_string(),
        expr: "order_count / 1".to_string(),
        ..Default::default()
    });
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("status")],
        metrics: vec![MetricName::new("avg_order")],
    };
    let result = expand("sales", &def, &req);
    assert!(result.is_err(), "Derived metric fan trap must be detected");
    match result.unwrap_err() {
        ExpandError::FanTrap { detail } => {
            assert_eq!(detail.metric_name, "avg_order");
            assert_eq!(detail.dimension_name, "status");
        }
        other => panic!("Expected FanTrap, got: {other}"),
    }
}

#[test]
fn fan_trap_error_message_format() {
    let err = ExpandError::FanTrap {
        detail: Box::new(FanTrapError {
            view_name: "sales".to_string(),
            metric_name: "order_count".to_string(),
            metric_table: "o".to_string(),
            dimension_name: "status".to_string(),
            dimension_table: "li".to_string(),
            relationship_name: "li_to_order".to_string(),
        }),
    };
    let msg = format!("{err}");
    assert!(msg.contains("sales"), "Must contain view name");
    assert!(msg.contains("order_count"), "Must contain metric name");
    assert!(msg.contains("status"), "Must contain dimension name");
    assert!(
        msg.contains("li_to_order"),
        "Must contain relationship name"
    );
    assert!(
        msg.contains("fan trap detected"),
        "Must contain 'fan trap detected'"
    );
    assert!(
        msg.contains("many-to-one cardinality"),
        "Must describe the cardinality direction"
    );
}

#[test]
fn cyclic_relationships_are_rejected_by_expand() {
    // #141 (fuzz_sql_expand OOM): relationships forming a cycle (a -> b via r1,
    // b -> a via r2) are parser-reachable and previously drove
    // `check_fan_traps`' JoinTree parent-walk into an infinite loop, allocating
    // until OOM. d48abee made the walks terminate and left the Ok/Err outcome
    // deliberately unspecified — so `expand` returned Ok, and the fence CERTIFIED
    // the cyclic definition (EXP-15): `fanning_edge_on_path` sees the forward
    // edge `(a, b)` and calls the hop safe without considering the reverse
    // `ManyToOne` edge `(b, a)` that fans.
    //
    // EXP-15 specifies the outcome: `build_relationship_graph` re-runs the
    // CREATE-time cycle check, so this is now `UncheckableDefinition`.
    //
    // Note what this test no longer proves: the fence short-circuits BEFORE the
    // JoinTree parent-walk, so it is not the #141 termination guard any more.
    // That guard's direct coverage is
    // `graph::join_tree::tests::walks_terminate_on_cyclic_parent_map`, which
    // exercises the walk on a cyclic parent map without going through the fence.
    let def = SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "a".to_string(),
                table: "ta".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "b".to_string(),
                table: "tb".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "d".to_string(),
            expr: "a.c".to_string(),
            source_table: Some("a".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "m".to_string(),
            expr: "sum(b.v)".to_string(),
            source_table: Some("b".to_string()),
            ..Default::default()
        }],
        joins: vec![
            Join {
                from_alias: "a".to_string(),
                table: "b".to_string(),
                fk_columns: vec!["bid".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("r1".to_string()),
                cardinality: Cardinality::ManyToOne,
            },
            Join {
                from_alias: "b".to_string(),
                table: "a".to_string(),
                fk_columns: vec!["aid".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("r2".to_string()),
                cardinality: Cardinality::ManyToOne,
            },
        ],
        ..Default::default()
    };
    let req = QueryRequest {
        where_clause: None,
        dimensions: vec!["d".into()],
        metrics: vec!["m".into()],
        facts: vec![],
    };
    match expand("v", &def, &req) {
        Err(ExpandError::UncheckableDefinition { view_name, reason }) => {
            assert_eq!(view_name, "v");
            assert!(
                reason.contains("cycle"),
                "reason should name the cycle: {reason}"
            );
        }
        other => panic!("Expected UncheckableDefinition, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// EXP-9 (code-review 2026-08-03): the fence must check the NON ADDITIVE BY
// dimension's OWN table, not just the queried dimensions.
//
// EXP-3 made active semi-additive metrics subject to the metric x dimension and
// metric x metric checks, but the table joined *because of the un-queried NA
// dimension itself* is exempt from every check. `collect_na_dim_source_tables`
// joins it into the snapshot CTE exactly as a queried dimension's table is
// joined, so when that join fans, the RANK runs over duplicated source rows —
// and ties across the fanned copies of one row are indistinguishable from ties
// across distinct rows, so the CTE cannot dedupe them (silent double-count).
//
// `snapshot_cte_anchor` does not rescue this shape: it returns None when the
// metric is already at the root grain, leaving the base-anchored fanned join.
// ---------------------------------------------------------------------------

/// Root `o` (orders) with child `li` (line_items) on the many side, a metric at
/// the ROOT grain, and its `NON ADDITIVE BY` dimension on the child.
fn semi_additive_na_dim_fans_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "li".to_string(),
                table: "line_items".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "ship_ts".to_string(),
                expr: "li.ship_ts".to_string(),
                source_table: Some("li".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![Metric {
            name: "balance_at".to_string(),
            expr: "SUM(o.balance)".to_string(),
            source_table: Some("o".to_string()),
            non_additive_by: vec![crate::model::NonAdditiveDim {
                dimension: "ship_ts".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }],
        joins: vec![Join {
            table: "o".to_string(),
            from_alias: "li".to_string(),
            fk_columns: vec!["order_id".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("li_to_order".to_string()),
            cardinality: Cardinality::ManyToOne,
            ..Default::default()
        }],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

#[test]
fn semi_additive_na_dim_on_fanning_table_blocked() {
    let def = semi_additive_na_dim_fans_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("balance_at")],
    };
    let result = expand("sales", &def, &req);
    // Assert the specific variant AND its payload: a bare `is_err()` would go
    // green on any unrelated failure (a mistyped metric name resolving to
    // `UnknownMetric`, say) and quietly stop guarding EXP-9.
    match result {
        Err(ExpandError::FanTrap { detail }) => {
            assert_eq!(detail.view_name, "sales");
            assert_eq!(detail.metric_name, "balance_at");
            // The NA dimension is named as the fanning participant even though
            // it was never queried -- that is the whole point of EXP-9.
            assert_eq!(detail.dimension_name, "ship_ts");
            assert_eq!(detail.dimension_table, "li");
            assert_eq!(detail.relationship_name, "li_to_order");
        }
        Err(other) => panic!("expected FanTrap naming the NA dimension, got: {other}"),
        Ok(sql) => panic!(
            "the NA dimension's table fans the metric's grain inside the snapshot \
             CTE -- RANK ties across the duplicated rows double-count the metric. \
             Emitted SQL instead:\n{sql}"
        ),
    }
}

/// The asymmetry that makes EXP-9 unmistakable: *querying* the same dimension
/// makes the metric regular and the fence already rejects the identical join.
/// The un-queried case must not be the permissive one.
#[test]
fn semi_additive_na_dim_queried_is_already_blocked() {
    let def = semi_additive_na_dim_fans_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("ship_ts")],
        metrics: vec![MetricName::new("balance_at")],
    };
    let result = expand("sales", &def, &req);
    match result {
        Err(ExpandError::FanTrap { detail }) => {
            assert_eq!(detail.metric_name, "balance_at");
            assert_eq!(detail.dimension_name, "ship_ts");
        }
        Err(other) => panic!(
            "control: the queried dimension on the fanning child must still be a \
             FanTrap, got: {other}"
        ),
        Ok(sql) => panic!("control: this has always been a FanTrap; got SQL:\n{sql}"),
    }
}

/// Control: an NA dimension on a table that does NOT fan the metric's grain
/// stays legal, so the fix cannot degenerate into "reject every NA dimension
/// on another table".
#[test]
fn semi_additive_na_dim_on_non_fanning_table_allowed() {
    let mut def = semi_additive_na_dim_fans_def();
    // Flip the edge: `o` now references `li`, so reaching `li` from `o` crosses
    // the many-to-one edge forwards -- one `li` row per `o` row, no fan-out.
    def.joins = vec![Join {
        table: "li".to_string(),
        from_alias: "o".to_string(),
        fk_columns: vec!["ship_id".to_string()],
        ref_columns: vec!["id".to_string()],
        name: Some("order_to_shipment".to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }];
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("balance_at")],
    };
    let result = expand("sales", &def, &req);
    assert!(
        result.is_ok(),
        "a many-to-one NA-dim join does not fan the metric: {:?}",
        result.err()
    );
}

/// The error payload reports the source table **as declared**, not folded.
///
/// Copilot review on PR #189: the EXP-9 branch initially put the lowercased
/// lookup key in `FanTrapError.dimension_table`, while the queried-dimension
/// branch reports `dim.source_table` verbatim. Path and cardinality lookups
/// still use the folded key — only the message changed — so this pins the two
/// apart with a mixed-case alias, which the lowercase-aliased tests above
/// cannot distinguish.
#[test]
fn semi_additive_na_dim_fan_trap_reports_declared_table_spelling() {
    let mut def = semi_additive_na_dim_fans_def();
    // Re-declare the child table under a mixed-case alias, wiring every
    // reference to it (the dimension's source, its expression, and the
    // relationship's from side) to the new spelling.
    def.tables[1].alias = "LiNe".to_string();
    def.dimensions[1].source_table = Some("LiNe".to_string());
    def.dimensions[1].expr = "LiNe.ship_ts".to_string();
    def.joins[0].from_alias = "LiNe".to_string();

    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("balance_at")],
    };
    match expand("sales", &def, &req) {
        Err(ExpandError::FanTrap { detail }) => assert_eq!(
            detail.dimension_table, "LiNe",
            "the error must echo the declared spelling, not the folded lookup key"
        ),
        Err(other) => panic!("expected FanTrap, got: {other}"),
        Ok(sql) => panic!("expected FanTrap; got SQL:\n{sql}"),
    }
}

// ---------------------------------------------------------------------------
// EXP-12 (code-review 2026-08-03): a QUOTED window inner-metric reference must
// behave exactly like its unquoted spelling.
//
// Four sites resolve `WindowSpec::inner_metric`: the CREATE-time validator
// (`body_parser`), the fan-trap fence's `metric_grain`, the per-grain
// planner's `window_cte_anchor`, and the emitter (`expand::window`). Only the
// emitter used the canonical identifier key; the other three compared raw
// spellings, so `"total"` did not match a metric stored as `total`.
//
// The CREATE-side strictness masked the rest: a definition with a quoted
// reference could not be stored, so the fence never saw one. Migrating CREATE
// alone would have opened a silent inflation path -- the fence would lose the
// inner aggregate's grain, `RootGrainFanTrap` would not fire, and the
// base-anchored `__sv_agg` CTE would compute the inner aggregate over a fanned
// join while the emitter happily resolved the same reference. So all four move
// together, and the invariant tested here is the one that ties them: the
// quoted and unquoted spellings must produce byte-identical outcomes.
// ---------------------------------------------------------------------------

/// Base `b` with parent `p` (b references p, so p is the "one" side and sits
/// ABOVE the root grain). A window metric on `b` whose inner aggregate lives on
/// `p` — the shape whose grain the fence must see through the reference.
fn window_inner_ref_def(inner_ref: &str) -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            TableRef {
                alias: "b".to_string(),
                table: "base".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "p".to_string(),
                table: "parent".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "seg".to_string(),
            expr: "p.seg".to_string(),
            source_table: Some("p".to_string()),
            ..Default::default()
        }],
        metrics: vec![
            Metric {
                name: "total".to_string(),
                expr: "SUM(p.amount)".to_string(),
                source_table: Some("p".to_string()),
                ..Default::default()
            },
            Metric {
                name: "w".to_string(),
                expr: "AVG(total)".to_string(),
                source_table: Some("b".to_string()),
                window_spec: Some(crate::model::WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: inner_ref.to_string(),
                    extra_args: vec![],
                    excluding_dims: vec![],
                    partition_dims: vec!["seg".to_string()],
                    order_by: vec![],
                    frame_clause: None,
                }),
                ..Default::default()
            },
        ],
        joins: vec![Join {
            table: "p".to_string(),
            from_alias: "b".to_string(),
            fk_columns: vec!["parent_id".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("b_to_p".to_string()),
            cardinality: Cardinality::ManyToOne,
            ..Default::default()
        }],
        facts: vec![],
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
    }
}

/// The EXP-12 invariant: quoting the inner-metric reference changes nothing.
///
/// Asserting equality of the two OUTCOMES (rather than pinning one specific
/// answer) is deliberate — it holds whether the shape is answered by anchoring
/// the CTE at the inner aggregate's grain or rejected by the fence, and it
/// keeps holding if that decision is ever revisited. What it forbids is the
/// asymmetry: the unquoted spelling seeing the inner grain while the quoted one
/// silently does not.
#[test]
fn window_quoted_inner_metric_reference_behaves_like_unquoted() {
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("seg")],
        metrics: vec![MetricName::new("w")],
    };
    let plain = expand("wq", &window_inner_ref_def("total"), &req);
    let quoted = expand("wq", &window_inner_ref_def("\"total\""), &req);

    match (&plain, &quoted) {
        (Ok(a), Ok(b)) => assert_eq!(
            a, b,
            "quoted and unquoted inner-metric references must emit identical SQL"
        ),
        (Err(a), Err(b)) => assert_eq!(
            a.to_string(),
            b.to_string(),
            "quoted and unquoted inner-metric references must fail identically"
        ),
        (Ok(sql), Err(e)) => panic!(
            "unquoted resolved but quoted did not -- the quoted reference lost \
             the inner aggregate's grain.\nunquoted SQL:\n{sql}\nquoted error: {e}"
        ),
        (Err(e), Ok(sql)) => {
            panic!("quoted resolved but unquoted did not.\nunquoted error: {e}\nquoted SQL:\n{sql}")
        }
    }
}

/// The same invariant with the reference's CASE varied inside the quotes.
/// DuckDB folds case whether or not a name is quoted, so `"TOTAL"` is `total`.
#[test]
fn window_quoted_case_varied_inner_metric_reference_behaves_like_unquoted() {
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("seg")],
        metrics: vec![MetricName::new("w")],
    };
    let plain = expand("wq", &window_inner_ref_def("total"), &req);
    let quoted = expand("wq", &window_inner_ref_def("\"TOTAL\""), &req);
    assert_eq!(
        plain.as_ref().map_err(std::string::ToString::to_string),
        quoted.as_ref().map_err(std::string::ToString::to_string),
        "case inside quotes is folded too, so the outcomes must match"
    );
}

/// The fence-side half stated directly: the inner aggregate's table is part of
/// the window metric's grain no matter how the reference is spelled. This is
/// what would have been lost had the CREATE-side check migrated alone.
#[test]
fn window_quoted_inner_metric_contributes_its_grain_to_the_fence() {
    for reference in ["total", "\"total\"", "\"Total\""] {
        let def = window_inner_ref_def(reference);
        let met = def
            .metrics
            .iter()
            .find(|m| m.name == "w")
            .expect("window metric");
        let grains = crate::expand::fan_trap::metric_grain(met, &def).anchored("b");
        assert!(
            grains.iter().any(|t| t == "p"),
            "inner aggregate's table `p` must be in the grain set for reference \
             {reference}, got {grains:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// EXP-27 (code-review 2026-08-08): a `where_clause` member whose FACT CHAIN
// reaches a fanning table had that table joined with no fan fence at all.
//
// #207 (EXP-23) taught `resolve_where_clause` to contribute the tables a
// predicate member reaches THROUGH its fact references to `source_tables`, so
// the join resolver joins them and the inlined expression binds. That is right
// for the parent direction (`s -> c`), which is the case EXP-23 was about. It
// is wrong without a fence for the CHILD direction: neither fan check sees the
// pair. `check_where_clause_fan_traps` walks only the member's OWN table
// (`fan_trap.rs`, `member_table`), and `check_referenced_fact_fan_traps` is
// handed the QUERIED dimensions and metrics -- a `where_clause` member is
// neither. So the fanning join is emitted and the metric silently doubles.
//
// Pre-#207 both shapes below were a loud binder error (`li` was never joined),
// so this is a loud -> silent-wrong regression, not a pre-existing gap.
// ---------------------------------------------------------------------------

/// base `o`, child `li` (`li.order_id REFERENCES o.id`, so `o -> li` fans).
/// Fact `liq` lives on the CHILD; fact `of` lives on the base and reaches it.
fn exp27_def() -> SemanticViewDefinition {
    SemanticViewDefinition::default()
        .with_table("o", "exp27_o", &["id"])
        .with_table("li", "exp27_li", &["id"])
        .with_dimension("region", "o.region", Some("o"))
        .with_fact("liq", "li.qty", "li")
        .with_fact("of", "li.liq * 2", "o")
        .with_metric("rev", "SUM(o.amount)", Some("o"))
        .with_pkfk_join("li_to_o", "li", "o", &["order_id"], &["id"])
}

fn exp27_req(where_clause: &str) -> QueryRequest {
    QueryRequest {
        where_clause: Some(where_clause.to_string()),
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("rev")],
    }
}

/// The FACT branch: the predicate names a fact on the base whose expression
/// reaches a fact on the child table.
#[test]
fn exp27_where_member_fact_chain_to_a_fanning_table_is_fenced() {
    let def = exp27_def();
    match expand("v", &def, &exp27_req("of > 0")) {
        Err(ExpandError::WhereClauseFanTrap {
            metric_name,
            member_name,
            member_table,
            relationship_name,
            ..
        }) => {
            assert_eq!(metric_name, "rev");
            assert_eq!(member_name, "of");
            assert_eq!(
                member_table, "li",
                "the table joined on the member's behalf"
            );
            assert_eq!(relationship_name, "li_to_o");
        }
        Err(other) => panic!("expected WhereClauseFanTrap, got: {other}"),
        Ok(sql) => panic!(
            "EXP-27: the fanning child table was joined for a where_clause fact \
             chain with no fence -- SUM(o.amount) is aggregated over multiplied \
             rows.\nemitted SQL:\n{sql}"
        ),
    }
}

/// The DIMENSION branch: the predicate names a *dimension* on the base whose
/// expression reaches the same child-table fact. TECH-DEBT #54 made dimension
/// expressions fact-inlined, so this reaches the identical join with an
/// identical absence of fencing.
#[test]
fn exp27_where_member_dimension_reaching_a_fanning_fact_is_fenced() {
    let def = exp27_def().with_dimension(
        "band",
        "CASE WHEN li.liq > 0 THEN 'hi' ELSE 'lo' END",
        Some("o"),
    );
    match expand("v", &def, &exp27_req("band = 'hi'")) {
        Err(ExpandError::WhereClauseFanTrap {
            metric_name,
            member_name,
            member_table,
            ..
        }) => {
            assert_eq!(metric_name, "rev");
            assert_eq!(member_name, "band");
            assert_eq!(member_table, "li");
        }
        Err(other) => panic!("expected WhereClauseFanTrap, got: {other}"),
        Ok(sql) => panic!(
            "EXP-27 (dimension branch): a where_clause DIMENSION whose expression \
             reaches a child-table fact joined the fanning table \
             unfenced.\nemitted SQL:\n{sql}"
        ),
    }
}

/// Transitivity: `member_fact_tables` walks the chain, so a two-hop chain that
/// only reaches the fanning table at its far end must be fenced too. A fence
/// that looked at the directly-named fact alone would miss this.
#[test]
fn exp27_transitive_fact_chain_to_a_fanning_table_is_fenced() {
    let def = exp27_def().with_fact("of2", "of + 1", "o");
    match expand("v", &def, &exp27_req("of2 > 0")) {
        Err(ExpandError::WhereClauseFanTrap { member_name, .. }) => {
            assert_eq!(member_name, "of2");
        }
        Err(other) => panic!("expected WhereClauseFanTrap, got: {other}"),
        Ok(sql) => panic!(
            "EXP-27 (transitive): `of2 -> of -> liq@li` reached the fanning table \
             through two hops unfenced.\nemitted SQL:\n{sql}"
        ),
    }
}

/// CONTROL 1 -- the legitimate EXP-23 shape must keep working: a fact chain
/// that reaches a PARENT table crosses the many-to-one edge forwards, which
/// does not fan, so the join is joined and the query expands. This is the
/// `cr0806i_cross` fixture from `cr20260806_inlining_gaps.test` in miniature.
#[test]
fn exp27_control_non_fanning_parent_fact_chain_still_expands() {
    let def = SemanticViewDefinition::default()
        .with_table("s", "exp27_sales", &["id"])
        .with_table("c", "exp27_cust", &["id"])
        .with_dimension("sid", "s.id", Some("s"))
        .with_fact("cust_rate", "c.rate", "c")
        .with_fact("weighted", "s.amount * cust_rate", "s")
        .with_metric("total", "SUM(s.amount)", Some("s"))
        .with_pkfk_join("s_to_c", "s", "c", &["cust_id"], &["id"]);
    let req = QueryRequest {
        where_clause: Some("weighted > 25".to_string()),
        facts: vec![],
        dimensions: vec![DimensionName::new("sid")],
        metrics: vec![MetricName::new("total")],
    };
    let sql = expand("v", &def, &req).expect("a parent-direction fact chain must still expand");
    assert!(
        sql.contains("exp27_cust"),
        "the table reached through the fact chain must still be joined: {sql}"
    );
}

/// CONTROL 2 -- the fence is anchored at the METRIC's grain, not at the
/// member's own table, exactly like the existing where-clause fan check. A
/// metric already at the child grain is not multiplied by joining the child, so
/// the same predicate must still expand.
#[test]
fn exp27_control_child_grain_metric_is_not_fenced() {
    let def = exp27_def().with_metric("li_qty", "SUM(li.qty)", Some("li"));
    let req = QueryRequest {
        where_clause: Some("of > 0".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("li_qty")],
    };
    let sql = expand("v", &def, &req)
        .expect("a metric at the child grain is not inflated by joining the child");
    assert!(sql.contains("exp27_li"), "{sql}");
}

/// The data-level statement of what the fence prevents: with one order and two
/// line items, joining `li` doubles `SUM(o.amount)`. Kept as an executable
/// oracle so the fixture cannot drift into one where the join is harmless and
/// the fence tests above become vacuous.
#[cfg(not(feature = "extension"))]
#[test]
fn exp27_the_fanning_join_really_does_double_the_metric() {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE exp27_o (id INTEGER, region VARCHAR, amount INTEGER);
         INSERT INTO exp27_o VALUES (1, 'E', 100);
         CREATE TABLE exp27_li (id INTEGER, order_id INTEGER, qty INTEGER);
         INSERT INTO exp27_li VALUES (1, 1, 5), (2, 1, 6);",
    )
    .expect("setup");
    let one = |sql: &str| {
        con.query_row(sql, [], |r| r.get::<_, i64>(0))
            .expect("query")
    };
    assert_eq!(
        one("SELECT SUM(o.amount) FROM exp27_o o"),
        100,
        "the un-joined answer is the correct one"
    );
    assert_eq!(
        one("SELECT SUM(o.amount) FROM exp27_o o \
             LEFT JOIN exp27_li li ON li.order_id = o.id \
             WHERE ((li.qty) * 2) > 0"),
        200,
        "joining the child table for the predicate doubles the metric -- this \
         is the 2x EXP-27 emitted silently"
    );
}

// ---------------------------------------------------------------------------
// PR #247 review follow-up: the join resolver now joins the tables a WINDOW
// metric's inner aggregate reaches through its fact references (so a role-
// played fact under a USING inner metric binds). The referenced-fact fan fence
// must see the same set, or a child-table fact reached through a window's
// inner metric is joined unfenced and the inner aggregate silently inflates.
// ---------------------------------------------------------------------------

/// `exp27_def` plus a base-table metric over the CHILD fact `liq`, a window
/// metric over it, and a derived metric over it.
fn window_over_fanning_fact_def() -> SemanticViewDefinition {
    exp27_def()
        .with_metric("li_qty", "SUM(liq)", Some("o"))
        .with_metric("li_qty_w", "SUM(li_qty) OVER ()", Some("o"))
        .with_window_spec(
            "li_qty_w",
            crate::model::WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: "li_qty".to_string(),
                ..Default::default()
            },
        )
        .with_metric("li_qty_twice", "li_qty * 2", None)
}

fn expect_referenced_fact_fan_trap(def: &SemanticViewDefinition, metric: &str, member: &str) {
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new(metric)],
    };
    match expand("v", def, &req) {
        Err(ExpandError::ReferencedFactFanTrap { detail }) => {
            assert_eq!(detail.member_name, member);
            assert_eq!(detail.fact_table, "li");
        }
        Err(other) => panic!("expected ReferencedFactFanTrap, got: {other}"),
        Ok(sql) => panic!("the fanning child table was joined unfenced:\n{sql}"),
    }
}

/// CONTROL: queried directly, the base metric is fenced already.
#[test]
fn referenced_fact_fan_trap_direct_metric_is_fenced() {
    expect_referenced_fact_fan_trap(&window_over_fanning_fact_def(), "li_qty", "li_qty");
}

/// The window metric's inner aggregate reaches the child fact.
#[test]
fn referenced_fact_fan_trap_window_inner_metric_is_fenced() {
    expect_referenced_fact_fan_trap(&window_over_fanning_fact_def(), "li_qty_w", "li_qty");
}

/// A derived metric over the same base metric.
#[test]
fn referenced_fact_fan_trap_derived_metric_is_fenced() {
    expect_referenced_fact_fan_trap(&window_over_fanning_fact_def(), "li_qty_twice", "li_qty");
}
