//! Role-playing (USING) scoped-alias resolution.
//!
//! Extracted from `sql_gen.rs`'s `mod tests::phase32_role_playing_tests` (§6.2 move 6,
//! code-review 2026-07-11) — behaviour-named files replace the phase-named
//! archaeology. `use super::*` resolves against `crate::expand`'s re-exports.

use super::*;
use crate::expand::test_helpers::TestFixtureExt;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

fn flights_airports_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        variables: Vec::new(),
        tables: vec![
            TableRef {
                alias: "f".to_string(),
                table: "flights".to_string(),
                pk_columns: vec!["flight_id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "a".to_string(),
                table: "airports".to_string(),
                pk_columns: vec!["airport_code".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![
            Dimension {
                name: "city".to_string(),
                expr: "a.city".to_string(),
                source_table: Some("a".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "country".to_string(),
                expr: "a.country".to_string(),
                source_table: Some("a".to_string()),
                ..Default::default()
            },
            Dimension {
                name: "carrier".to_string(),
                expr: "f.carrier".to_string(),
                source_table: Some("f".to_string()),
                ..Default::default()
            },
        ],
        metrics: vec![
            Metric {
                name: "departure_count".to_string(),
                expr: "COUNT(*)".to_string(),
                source_table: Some("f".to_string()),
                using_relationships: vec!["dep_airport".to_string()],
                ..Default::default()
            },
            Metric {
                name: "arrival_count".to_string(),
                expr: "COUNT(*)".to_string(),
                source_table: Some("f".to_string()),
                using_relationships: vec!["arr_airport".to_string()],
                ..Default::default()
            },
            Metric {
                name: "total_flights".to_string(),
                expr: "departure_count + arrival_count".to_string(),
                ..Default::default()
            },
        ],
        joins: vec![
            Join {
                table: "a".to_string(),
                from_alias: "f".to_string(),
                fk_columns: vec!["departure_code".to_string()],
                ref_columns: vec!["airport_code".to_string()],
                name: Some("dep_airport".to_string()),
                cardinality: Cardinality::ManyToOne,
                ..Default::default()
            },
            Join {
                table: "a".to_string(),
                from_alias: "f".to_string(),
                fk_columns: vec!["arrival_code".to_string()],
                ref_columns: vec!["airport_code".to_string()],
                name: Some("arr_airport".to_string()),
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

/// `flights_airports_def` extended with a `regions` table that `airports`
/// references — a DESCENDANT of the role-playing target `a` (EXP-4).
fn flights_airports_regions_def() -> SemanticViewDefinition {
    let mut def = flights_airports_def();
    def.tables.push(TableRef {
        alias: "r".to_string(),
        table: "regions".to_string(),
        pk_columns: vec!["region_id".to_string()],
        ..Default::default()
    });
    def.dimensions.push(Dimension {
        name: "region_name".to_string(),
        expr: "r.region_name".to_string(),
        source_table: Some("r".to_string()),
        ..Default::default()
    });
    def.joins.push(Join {
        table: "r".to_string(),
        from_alias: "a".to_string(),
        fk_columns: vec!["region_id".to_string()],
        ref_columns: vec!["region_id".to_string()],
        name: Some("airport_region".to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    });
    def
}

#[test]
fn descendant_of_role_playing_table_follows_the_metrics_path() {
    // EXP-4 (code-review 2026-07-18) made a dimension on `r`, a descendant of
    // the role-playing table `a`, an error: a bare walk hung `r` off whichever
    // airport instance was declared first. Phase 6.3b follows Snowflake's
    // transitive hop instead: `arrival_count USING (arr_airport)` names the
    // airport instance, and `r` is joined off THAT instance under its own
    // scoped alias, never bare.
    let def = flights_airports_regions_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_name")],
        metrics: vec![MetricName::new("arrival_count")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains(
            r#"LEFT JOIN "regions" AS "r__arr_airport" ON "a__arr_airport"."region_id" = "r__arr_airport"."region_id""#
        ),
        "regions must be joined off the arrival airport: {sql}"
    );
    assert!(
        sql.contains("r__arr_airport.region_name"),
        "the dimension must read the scoped instance: {sql}"
    );
    assert!(
        !sql.contains(r#"AS "r" "#) && !sql.contains(r#""a"."region_id""#),
        "no bare regions or airports join may appear: {sql}"
    );
}

#[test]
fn descendant_of_role_playing_table_without_a_path_errors_ambiguous() {
    // A metric with no USING names no airport instance, so the region it would
    // be grouped by is still ambiguous.
    let mut def = flights_airports_regions_def();
    def.metrics.push(Metric {
        name: "flight_rows".to_string(),
        expr: "COUNT(*)".to_string(),
        source_table: Some("f".to_string()),
        ..Default::default()
    });
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_name")],
        metrics: vec![MetricName::new("flight_rows")],
    };
    let err = expand("test_flights", &def, &req).unwrap_err();
    match err {
        ExpandError::AmbiguousDescendantPath {
            view_name,
            dimension_name,
            dimension_table,
            role_playing_table,
            available_relationships,
        } => {
            assert_eq!(view_name, "test_flights");
            assert_eq!(dimension_name, "region_name");
            assert_eq!(dimension_table, "r");
            assert_eq!(role_playing_table, "a");
            assert!(
                available_relationships.contains(&"dep_airport".to_string())
                    && available_relationships.contains(&"arr_airport".to_string()),
                "both airport relationships must be listed: {available_relationships:?}"
            );
        }
        other => panic!("Expected AmbiguousDescendantPath, got: {other}"),
    }
}

#[test]
fn descendant_through_single_relationship_still_resolves() {
    // Guard against over-rejection: when the intermediate table is reached by a
    // SINGLE relationship (not role-playing), a dimension on its descendant is
    // unambiguous and must still expand. Here only `dep_airport` connects
    // flights to airports, so `region_name` on `r` has one join path.
    let mut def = flights_airports_regions_def();
    // Drop the second (arr) relationship so `a` is no longer role-playing.
    def.joins
        .retain(|j| j.name.as_deref() != Some("arr_airport"));
    def.metrics
        .retain(|m| m.name != "arrival_count" && m.name != "total_flights");
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("region_name")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test_flights", &def, &req).expect("single-path descendant must resolve");
    assert!(sql.contains("region_name"), "SQL: {sql}");
}

#[test]
fn using_metric_generates_scoped_join_alias() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("a__dep_airport"),
        "Scoped alias a__dep_airport must appear: {sql}"
    );
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__dep_airport\""),
        "LEFT JOIN with scoped alias must appear: {sql}"
    );
}

#[test]
fn two_using_metrics_generate_two_scoped_joins() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("carrier")],
        metrics: vec![
            MetricName::new("departure_count"),
            MetricName::new("arrival_count"),
        ],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__dep_airport\""),
        "dep_airport scoped JOIN must appear: {sql}"
    );
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__arr_airport\""),
        "arr_airport scoped JOIN must appear: {sql}"
    );
}

#[test]
fn dimension_rewritten_to_scoped_alias() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("a__dep_airport.city"),
        "Dimension must be rewritten to scoped alias: {sql}"
    );
}

#[test]
fn ambiguous_dimension_without_using_produces_error() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![],
    };
    let result = expand("test_flights", &def, &req);
    assert!(result.is_err(), "Ambiguous dimension must produce error");
    match result.unwrap_err() {
        ExpandError::AmbiguousPath {
            view_name,
            dimension_name,
            dimension_table,
            available_relationships,
        } => {
            assert_eq!(view_name, "test_flights");
            assert_eq!(dimension_name, "city");
            assert_eq!(dimension_table, "a");
            assert!(available_relationships.contains(&"dep_airport".to_string()));
            assert!(available_relationships.contains(&"arr_airport".to_string()));
        }
        other => panic!("Expected AmbiguousPath, got: {other}"),
    }
}

#[test]
fn ambiguous_path_error_lists_relationships() {
    let err = ExpandError::AmbiguousPath {
        view_name: "test_flights".to_string(),
        dimension_name: "city".to_string(),
        dimension_table: "a".to_string(),
        available_relationships: vec!["dep_airport".to_string(), "arr_airport".to_string()],
    };
    let msg = format!("{err}");
    assert!(msg.contains("test_flights"));
    assert!(msg.contains("city"));
    assert!(msg.contains("ambiguous"));
    assert!(msg.contains("dep_airport"));
    assert!(msg.contains("arr_airport"));
}

#[test]
fn non_ambiguous_single_relationship_works_without_using() {
    let mut def = SemanticViewDefinition::default()
        .with_table("orders", "orders", &[])
        .with_table("o", "orders", &["id"])
        .with_table("c", "customers", &["id"])
        .with_dimension("customer_name", "c.name", Some("c"))
        .with_metric("revenue", "SUM(o.amount)", Some("o"));
    def.joins.push(Join {
        table: "c".to_string(),
        from_alias: "o".to_string(),
        fk_columns: vec!["customer_id".to_string()],
        name: Some("order_to_customer".to_string()),
        ..Default::default()
    });
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("revenue")],
    };
    let result = expand("test", &def, &req);
    assert!(
        result.is_ok(),
        "Single relationship must work without USING: {:?}",
        result.err()
    );
}

#[test]
fn base_table_dimension_works_unchanged() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("carrier")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("f.carrier AS \"carrier\""),
        "Base table dimension must appear unchanged: {sql}"
    );
}

#[test]
fn fan_trap_detection_works_with_using_paths() {
    let def = SemanticViewDefinition {
        variables: Vec::new(),
        tables: vec![
            TableRef {
                alias: "f".to_string(),
                table: "flights".to_string(),
                pk_columns: vec!["flight_id".to_string()],
                ..Default::default()
            },
            TableRef {
                alias: "a".to_string(),
                table: "airports".to_string(),
                pk_columns: vec!["airport_code".to_string()],
                ..Default::default()
            },
        ],
        dimensions: vec![Dimension {
            name: "carrier".to_string(),
            expr: "f.carrier".to_string(),
            source_table: Some("f".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "airport_count".to_string(),
            expr: "COUNT(*)".to_string(),
            source_table: Some("a".to_string()),
            ..Default::default()
        }],
        joins: vec![Join {
            table: "a".to_string(),
            from_alias: "f".to_string(),
            fk_columns: vec!["dep_airport_code".to_string()],
            ref_columns: vec!["airport_code".to_string()],
            name: Some("dep_flights".to_string()),
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
    };
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("carrier")],
        metrics: vec![MetricName::new("airport_count")],
    };
    let result = expand("test", &def, &req);
    assert!(result.is_err(), "Fan trap must still be detected");
    match result.unwrap_err() {
        ExpandError::FanTrap { .. } => {}
        other => panic!("Expected FanTrap, got: {other}"),
    }
}

#[test]
fn derived_metric_with_two_using_resolves_both_joins() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("carrier")],
        metrics: vec![MetricName::new("total_flights")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__dep_airport\""),
        "Derived metric must resolve dep_airport join: {sql}"
    );
    assert!(
        sql.contains("LEFT JOIN \"airports\" AS \"a__arr_airport\""),
        "Derived metric must resolve arr_airport join: {sql}"
    );
}

#[test]
fn metric_using_from_base_table_no_unnecessary_join() {
    let def = SemanticViewDefinition {
        variables: Vec::new(),
        tables: vec![TableRef {
            alias: "o".to_string(),
            table: "orders".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }],
        dimensions: vec![Dimension {
            name: "region".to_string(),
            expr: "o.region".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "cnt".to_string(),
            expr: "COUNT(*)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![],
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
        dimensions: vec![DimensionName::new("region")],
        metrics: vec![MetricName::new("cnt")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        !sql.contains("JOIN"),
        "No JOIN needed when everything is on base table: {sql}"
    );
}

#[test]
fn backward_compat_no_using_expands_as_before() {
    let def = SemanticViewDefinition {
        variables: Vec::new(),
        tables: vec![
            TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
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
        dimensions: vec![Dimension {
            name: "customer_name".to_string(),
            expr: "c.name".to_string(),
            source_table: Some("c".to_string()),
            ..Default::default()
        }],
        metrics: vec![Metric {
            name: "revenue".to_string(),
            expr: "SUM(o.amount)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }],
        joins: vec![Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            name: Some("order_to_customer".to_string()),
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
        dimensions: vec![DimensionName::new("customer_name")],
        metrics: vec![MetricName::new("revenue")],
    };
    let sql = expand("test", &def, &req).unwrap();
    assert!(
        sql.contains("LEFT JOIN \"customers\" AS \"c\""),
        "Non-USING definition must use bare alias: {sql}"
    );
    assert!(
        sql.contains("c.name AS"),
        "Dimension expr must use bare alias: {sql}"
    );
}

#[test]
fn derived_metric_using_both_paths_groups_each_component_through_its_own() {
    // Phase 6.3: this used to be refused as ambiguous. Each component now names
    // its own role, so each is grouped through it and the derived metric is
    // assembled after the outer merge (numbers: `tests_multi_path_using`).
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![MetricName::new("total_flights")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("a__dep_airport.city") && sql.contains("a__arr_airport.city"),
        "each component groups by its own role's city: {sql}"
    );
    assert!(sql.contains("FULL OUTER JOIN"), "{sql}");
}

#[test]
fn scoped_join_on_clause_uses_correct_fk_pk() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: vec![DimensionName::new("city")],
        metrics: vec![MetricName::new("departure_count")],
    };
    let sql = expand("test_flights", &def, &req).unwrap();
    assert!(
        sql.contains("\"f\".\"departure_code\" = \"a__dep_airport\".\"airport_code\""),
        "Scoped JOIN ON clause must use correct FK/PK: {sql}"
    );
}

// ---------------------------------------------------------------------------
// EXP-10 (code-review 2026-08-03): a `where_clause` member on a role-playing
// table must not bind silently to the first-declared relationship.
//
// Role-playing ambiguity is checked for queried DIMENSIONS (`find_using_context`,
// Phase 32) and, on the facts path, for queried dims and facts (EXP-5) — but a
// `where_clause` member's table is in neither set. It rides
// `resolve_joins_pkfk`'s `fact_source_tables` parameter, whose loop (unlike the
// dimension loop) never consults `role_playing_bare_aliases`, so the bare alias
// joins via `tree_parent` — the first-declared edge.
//
// Only a queried dimension's *expression* is rewritten to a scoped alias, so a
// where-member has no way to name its role; per EXP-4/EXP-5 the correct posture
// is to fail loudly rather than pick an edge by declaration order.
// ---------------------------------------------------------------------------

#[test]
fn where_clause_on_role_playing_table_is_rejected() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: Some("city = 'NYC'".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("departure_count")],
    };
    let result = expand("flights_sv", &def, &req);
    // The specific variant, not merely "an error": a bare `is_err()` would go
    // green on any unrelated failure and stop guarding EXP-10.
    match result {
        Err(ExpandError::AmbiguousWhereClausePath {
            view_name,
            member_name,
            member_table,
            role_playing_table,
            available_relationships,
        }) => {
            assert_eq!(view_name, "flights_sv");
            assert_eq!(member_name, "city");
            assert_eq!(member_table, "a");
            assert_eq!(role_playing_table, "a");
            // The error must name BOTH roles -- that list is what tells the
            // user how to resolve the ambiguity.
            assert!(
                available_relationships.contains(&"dep_airport".to_string())
                    && available_relationships.contains(&"arr_airport".to_string()),
                "error must list both roles, got: {available_relationships:?}"
            );
        }
        Err(other) => panic!("expected AmbiguousWhereClausePath, got: {other}"),
        Ok(sql) => panic!(
            "a where_clause member on a role-playing table must not bind to the \
             first-declared relationship silently. Emitted SQL:\n{sql}"
        ),
    }
}

/// The wrong-number shape the rejection exists to prevent: the metric's
/// `USING (arr_airport)` scopes its join to `a__arr_airport`, while the
/// predicate's `a` is joined via the first-declared `dep_airport`. Filtering
/// on departure city while counting arrivals is a silently wrong answer, not a
/// different-but-defensible one.
#[test]
fn where_clause_role_playing_does_not_filter_through_the_other_role() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: Some("city = 'NYC'".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("arrival_count")],
    };
    let result = expand("flights_sv", &def, &req);
    match result {
        Err(ExpandError::AmbiguousWhereClausePath {
            member_name,
            role_playing_table,
            ..
        }) => {
            assert_eq!(member_name, "city");
            assert_eq!(role_playing_table, "a");
        }
        Err(other) => panic!(
            "expected AmbiguousWhereClausePath -- a different error means this \
             case is no longer guarding the wrong-role filter: {other}"
        ),
        Ok(sql) => panic!(
            "expected a role-playing ambiguity error; emitted SQL instead (the \
             predicate binds to the dep_airport instance while the metric uses \
             arr_airport):\n{sql}"
        ),
    }
}

/// Control: the same predicate on a NON-role-playing table's member stays
/// legal. Without this the fix above could be "reject every where_clause on a
/// joined table" and still look green.
#[test]
fn where_clause_on_non_role_playing_table_still_allowed() {
    let def = flights_airports_def();
    let req = QueryRequest {
        where_clause: Some("carrier = 'AA'".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("departure_count")],
    };
    let result = expand("flights_sv", &def, &req);
    assert!(
        result.is_ok(),
        "a where_clause on the base table's own dimension is unambiguous: {:?}",
        result.err()
    );
}

// ---------------------------------------------------------------------------
// EXP-31 (code-review 2026-08-08): the role-playing twin of EXP-27. A
// `where_clause` member whose FACT CHAIN reaches a ROLE-PLAYING table had that
// table joined with no ambiguity check at all.
//
// EXP-10 (above) closed the case where the member is *declared* on the
// role-playing table. #207 (EXP-23) then taught `resolve_where_clause` to
// contribute the tables a member reaches THROUGH its fact references to
// `source_tables`, so the join resolver joins those too — and
// `check_where_clause_role_playing_path` still read only `member.table`. The
// reached table therefore rode `fact_source_tables` straight to `tree_parent`,
// the first-declared relationship: exactly the silent, declaration-order-
// dependent binding EXP-10 exists to prevent, reached one hop further along.
//
// Same blind spot #207 opened for fan traps (EXP-27) at a different fence,
// which is why both now walk `WhereMember::fact_tables` alongside
// `WhereMember::table`.
// ---------------------------------------------------------------------------

/// `flights_airports_def` plus a fact on the ROLE-PLAYING table `a` and a fact
/// on the base whose expression reaches it, so a predicate naming the base
/// fact pulls `a` in without ever naming it.
///
/// A fact on `a` is not itself illegal: `check_fact_role_playing_path` rejects
/// one when it is *queried*, and a `where_clause` member is not queried.
fn flights_airports_fact_chain_def() -> SemanticViewDefinition {
    flights_airports_def()
        // A plain metric with no USING, so nothing else in the query supplies a
        // role and the ambiguity is the member's alone.
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_fact("ap_elev", "a.elevation", "a")
        .with_fact("f_elev", "ap_elev + 1", "f")
}

/// The FACT branch: the predicate names a fact on the base whose expression
/// reaches a fact on the role-playing table.
#[test]
fn exp31_where_member_fact_chain_to_a_role_playing_table_is_rejected() {
    let def = flights_airports_fact_chain_def();
    let req = QueryRequest {
        where_clause: Some("f_elev > 0".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("flight_count")],
    };
    match expand("flights_sv", &def, &req) {
        Err(ExpandError::AmbiguousWhereClausePath {
            member_name,
            member_table,
            role_playing_table,
            available_relationships,
            ..
        }) => {
            assert_eq!(member_name, "f_elev");
            assert_eq!(
                member_table, "a",
                "the error must name the table joined on the member's behalf; \
                 the table the member is declared on is unambiguous"
            );
            assert_eq!(role_playing_table, "a");
            assert_eq!(
                available_relationships,
                vec!["dep_airport".to_string(), "arr_airport".to_string()],
                "the message must list the roles the user has to choose between"
            );
        }
        Err(other) => panic!(
            "expected AmbiguousWhereClausePath — a different error means this \
             case stopped guarding EXP-31: {other}"
        ),
        Ok(sql) => panic!(
            "EXP-31: the role-playing table was joined for a where_clause fact \
             chain with no ambiguity check, so the predicate binds to the \
             first-declared relationship (dep_airport) silently.\n\
             emitted SQL:\n{sql}"
        ),
    }
}

/// The DIMENSION branch: TECH-DEBT #54 made predicate dimensions' expressions
/// fact-inlined too, so a dimension on the base reaching the same fact takes
/// the identical unchecked path.
#[test]
fn exp31_where_member_dimension_reaching_a_role_playing_fact_is_rejected() {
    let def = flights_airports_fact_chain_def().with_dimension(
        "high_altitude",
        "CASE WHEN ap_elev > 5000 THEN 'hi' ELSE 'lo' END",
        Some("f"),
    );
    let req = QueryRequest {
        where_clause: Some("high_altitude = 'hi'".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("flight_count")],
    };
    match expand("flights_sv", &def, &req) {
        Err(ExpandError::AmbiguousWhereClausePath {
            member_name,
            member_table,
            ..
        }) => {
            assert_eq!(member_name, "high_altitude");
            assert_eq!(member_table, "a");
        }
        Err(other) => panic!("expected AmbiguousWhereClausePath, got: {other}"),
        Ok(sql) => panic!(
            "EXP-31 (dimension branch): a where_clause DIMENSION whose \
             expression reaches a fact on the role-playing table joined it \
             unchecked.\nemitted SQL:\n{sql}"
        ),
    }
}

/// Transitivity: the reached set walks the whole chain, so a two-hop chain
/// that only arrives at the role-playing table at its far end is rejected too.
/// A check that looked at the directly-named fact alone would miss this.
#[test]
fn exp31_transitive_fact_chain_to_a_role_playing_table_is_rejected() {
    let def = flights_airports_fact_chain_def().with_fact("f_elev2", "f_elev * 2", "f");
    let req = QueryRequest {
        where_clause: Some("f_elev2 > 0".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("flight_count")],
    };
    match expand("flights_sv", &def, &req) {
        Err(ExpandError::AmbiguousWhereClausePath { member_name, .. }) => {
            assert_eq!(member_name, "f_elev2");
        }
        Err(other) => panic!("expected AmbiguousWhereClausePath, got: {other}"),
        Ok(sql) => panic!(
            "EXP-31 (transitive): `f_elev2 -> f_elev -> ap_elev@a` reached the \
             role-playing table through two hops unchecked.\n\
             emitted SQL:\n{sql}"
        ),
    }
}

/// CONTROL — a fact chain reaching a NON-role-playing table must still expand.
/// Without this the fix could be "reject every where_clause fact chain" and
/// still look green. The same shape as the tests above with one relationship
/// instead of two, so nothing but the role-playing multi-edge differs.
#[test]
fn exp31_control_fact_chain_to_a_non_role_playing_table_still_expands() {
    let def = SemanticViewDefinition::default()
        .with_table("f", "exp31_flights", &["flight_id"])
        .with_table("a", "exp31_airports", &["airport_code"])
        .with_metric("flight_count", "COUNT(*)", Some("f"))
        .with_fact("ap_elev", "a.elevation", "a")
        .with_fact("f_elev", "ap_elev + 1", "f")
        .with_pkfk_join(
            "dep_airport",
            "f",
            "a",
            &["departure_code"],
            &["airport_code"],
        );
    let req = QueryRequest {
        where_clause: Some("f_elev > 0".to_string()),
        facts: vec![],
        dimensions: vec![],
        metrics: vec![MetricName::new("flight_count")],
    };
    let sql = expand("flights_sv", &def, &req)
        .expect("one relationship to the target means no role to disambiguate");
    assert!(
        sql.contains("exp31_airports"),
        "the table reached through the fact chain must still be joined: {sql}"
    );
}

// ---------------------------------------------------------------------------
// TECH-DEBT #69 (MM-1) / #70 (MM-2): role-played facts and quoted role names,
// asserted on the NUMBERS the emitted SQL returns, not on its text.
// ---------------------------------------------------------------------------

// The executing tests below need the bundled DuckDB, so they are compiled out
// of the `extension`-feature unit run (as `tests_phantom_row_guard` is); the
// default-features run, which can execute SQL, covers them.

/// Flights over two airports. Every flight departs `A` (elevation 100, `Dep
/// City`) and arrives at `B` (elevation 7, `Arr City`), so each role-scoped
/// number differs from its sibling: binding the wrong role changes the answer.
#[cfg(not(feature = "extension"))]
fn role_numbers_db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE flights (flight_id INTEGER, departure_code VARCHAR,
                               arrival_code VARCHAR, carrier VARCHAR);
         CREATE TABLE airports (airport_code VARCHAR, city VARCHAR,
                                country VARCHAR, elevation INTEGER);
         INSERT INTO airports VALUES ('A', 'Dep City', 'X', 100), ('B', 'Arr City', 'X', 7);
         INSERT INTO flights VALUES (1, 'A', 'B', 'c1'), (2, 'A', 'B', 'c1'), (3, 'A', 'B', 'c2');",
    )
    .expect("fixture data");
    con
}

/// Run `sql` and return `(first column if there are two, last column as f64)`,
/// sorted. Panics with the SQL when it does not bind.
#[cfg(not(feature = "extension"))]
fn run_rows(con: &duckdb::Connection, sql: &str) -> Vec<(Option<String>, f64)> {
    let mut stmt = con
        .prepare(sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind, got: {e}\nSQL:\n{sql}"));
    let mut rows: Vec<(Option<String>, f64)> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            if n == 1 {
                Ok((None, r.get::<_, f64>(0)?))
            } else {
                Ok((Some(r.get::<_, String>(0)?), r.get::<_, f64>(1)?))
            }
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    rows
}

/// `flights_airports_def` plus a fact on the ROLE-PLAYED table and one metric
/// per role aggregating it — the MM-1 shape.
fn role_played_fact_def() -> SemanticViewDefinition {
    let mut def = flights_airports_def();
    def.facts.push(crate::model::Fact {
        name: "elev".to_string(),
        expr: "a.elevation".to_string(),
        source_table: Some("a".to_string()),
        ..Default::default()
    });
    for (name, rel) in [("dep_elev", "dep_airport"), ("arr_elev", "arr_airport")] {
        def.metrics.push(Metric {
            name: name.to_string(),
            expr: "SUM(elev)".to_string(),
            source_table: Some("f".to_string()),
            using_relationships: vec![rel.to_string()],
            ..Default::default()
        });
    }
    def.metrics.push(Metric {
        name: "elev_climb".to_string(),
        expr: "dep_elev - arr_elev".to_string(),
        ..Default::default()
    });
    def
}

fn metrics_req(dims: &[&str], mets: &[&str]) -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// TECH-DEBT #69 (MM-1): a `USING (dep_airport)` metric over a fact on the
/// role-played table. The fact inlined as raw `a.elevation` while the table was
/// joined only as `"airports" AS "a__dep_airport"`, so the query failed to bind
/// (`Referenced table "a" not found`). Three flights departing elevation 100.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_using_metric_over_a_role_played_fact_binds_to_its_role() {
    let sql = expand(
        "v",
        &role_played_fact_def(),
        &metrics_req(&[], &["dep_elev"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 300.0)],
        "{sql}"
    );
}

/// The other role over the SAME fact must read the other instance: three
/// flights arriving at elevation 7. Discriminates a rewrite that picks the
/// first-declared relationship instead of the metric's own `USING`.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_using_metric_over_a_role_played_fact_reads_the_other_role() {
    let sql = expand(
        "v",
        &role_played_fact_def(),
        &metrics_req(&[], &["arr_elev"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 21.0)],
        "{sql}"
    );
}

/// Grouped by a dimension on the role-played table: the dimension and the
/// inlined fact are rewritten to the SAME scoped alias, so all 300 lands on the
/// departure city.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_role_played_fact_and_dimension_share_the_scoped_alias() {
    let sql = expand(
        "v",
        &role_played_fact_def(),
        &metrics_req(&["city"], &["dep_elev"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Dep City".to_string()), 300.0)],
        "{sql}"
    );
}

/// A derived metric over BOTH roles: each base metric's inlined fact is scoped
/// to its own relationship before the derived expression composes them, so the
/// two instances stay distinct: 300 - 21.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_derived_metric_over_both_roles_keeps_them_distinct() {
    let sql = expand(
        "v",
        &role_played_fact_def(),
        &metrics_req(&[], &["elev_climb"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 279.0)],
        "{sql}"
    );
}

/// `flights_airports_def` with the departure relationship renamed to a stored,
/// QUOTED identifier (quotes retained, as the body parser stores it).
#[cfg(not(feature = "extension"))]
fn quoted_role_def(stored_rel: &str) -> SemanticViewDefinition {
    let mut def = flights_airports_def();
    def.joins[0].name = Some(stored_rel.to_string());
    def.metrics[0].using_relationships = vec![stored_rel.to_string()];
    def
}

/// TECH-DEBT #70 (MM-2): a quoted relationship name with a space. The scoped
/// alias was concatenated from the raw stored text, so the JOIN emitted
/// `"a__""Dep Airport"""` and the dimension `a__"Dep Airport".city`, which does
/// not parse. All three flights depart `Dep City`.
#[test]
#[cfg(not(feature = "extension"))]
fn td70_quoted_relationship_name_with_a_space_emits_one_alias() {
    let def = quoted_role_def("\"Dep Airport\"");
    let sql = expand("v", &def, &metrics_req(&["city"], &["departure_count"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Dep City".to_string()), 3.0)],
        "{sql}"
    );
}

/// TECH-DEBT #70: an embedded (doubled) quote in the relationship name — the
/// case where raw concatenation and `quote_ident` disagree the most.
#[test]
#[cfg(not(feature = "extension"))]
fn td70_quoted_relationship_name_with_an_embedded_quote_emits_one_alias() {
    let def = quoted_role_def("\"de\"\"p\"");
    let sql = expand("v", &def, &metrics_req(&["city"], &["departure_count"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Dep City".to_string()), 3.0)],
        "{sql}"
    );
}

/// CONTROL for #70: a bare-safe relationship name keeps its pre-fix spelling —
/// the reference site stays unquoted `a__dep_airport.city`, byte-identical to
/// every existing golden.
#[test]
fn td70_bare_relationship_name_keeps_the_unquoted_reference() {
    let sql = expand(
        "v",
        &flights_airports_def(),
        &metrics_req(&["city"], &["departure_count"]),
    )
    .unwrap();
    assert!(sql.contains("a__dep_airport.city"), "{sql}");
    assert!(sql.contains("AS \"a__dep_airport\""), "{sql}");
}

// The role-playing fence for facts a MEMBER reaches (the pairing #69 walks
// past). Making a role-played fact reachable through `USING` is only half of
// it: a member that reaches the same fact WITHOUT a single role to name was
// joined on the first-declared relationship, a silent wrong-role number.

fn expect_member_fact_ambiguity(def: &SemanticViewDefinition, req: &QueryRequest, member: &str) {
    match expand("v", def, req) {
        Err(ExpandError::AmbiguousMemberFactPath { detail }) => {
            assert_eq!(detail.member_name, member);
            assert_eq!(detail.role_playing_table, "a");
        }
        Err(other) => panic!("expected AmbiguousMemberFactPath, got: {other}"),
        Ok(sql) => panic!("accepted, binding to an arbitrary role:\n{sql}"),
    }
}

/// A metric with NO `USING` over the role-played fact: before the fence it
/// returned 300 — the departure role, purely because `dep_airport` is
/// declared first.
#[test]
fn td69_fence_metric_without_using_over_a_role_played_fact_is_rejected() {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "any_elev".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        ..Default::default()
    });
    expect_member_fact_ambiguity(&def, &metrics_req(&[], &["any_elev"]), "any_elev");
}

/// The derived-metric spelling of the same thing: the fence must walk into the
/// base metrics a derived metric inlines, where the fact reference lives.
#[test]
fn td69_fence_derived_metric_over_an_unscoped_role_played_fact_is_rejected() {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "any_elev".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        ..Default::default()
    });
    def.metrics.push(Metric {
        name: "twice_any".to_string(),
        expr: "any_elev * 2".to_string(),
        ..Default::default()
    });
    expect_member_fact_ambiguity(&def, &metrics_req(&[], &["twice_any"]), "any_elev");
}

/// A DIMENSION on the base table whose expression reaches the role-played fact
/// (#54 made dimension expressions fact-inlined): a dimension carries no
/// `USING`, so it has no role to pick.
#[test]
fn td69_fence_dimension_reaching_a_role_played_fact_is_rejected() {
    let mut def = role_played_fact_def();
    def.dimensions.push(Dimension {
        name: "elev_band".to_string(),
        expr: "elev > 50".to_string(),
        source_table: Some("f".to_string()),
        ..Default::default()
    });
    expect_member_fact_ambiguity(
        &def,
        &metrics_req(&["elev_band"], &["departure_count"]),
        "elev_band",
    );
}

/// A `USING` metric over a fact on a DESCENDANT of the role-played table: the
/// scoped-alias rewrite renames only the role-played table itself, so the
/// descendant would hang off the bare, first-declared instance.
#[test]
fn td69_fence_using_metric_over_a_fact_below_the_role_played_table_is_rejected() {
    let mut def = flights_airports_regions_def();
    def.facts.push(crate::model::Fact {
        name: "region_code".to_string(),
        expr: "r.region_id".to_string(),
        source_table: Some("r".to_string()),
        ..Default::default()
    });
    def.metrics.push(Metric {
        name: "dep_regions".to_string(),
        expr: "COUNT(DISTINCT region_code)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["dep_airport".to_string()],
        ..Default::default()
    });
    match expand("v", &def, &metrics_req(&[], &["dep_regions"])) {
        Err(ExpandError::AmbiguousMemberFactPath { detail }) => {
            assert_eq!(detail.member_name, "dep_regions");
            assert_eq!(detail.fact_table, "r");
            assert_eq!(detail.role_playing_table, "a");
        }
        other => panic!("expected AmbiguousMemberFactPath, got: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// PR #247 review follow-ups: the #69 rewrite and its fence, on the paths the
// first cut did not walk — a WINDOW metric over a USING inner metric, a
// semi-additive USING metric, mixed-case duplicate USING spellings, and a
// metric declared ON the role-played table (which can never carry a USING of
// its own: a USING relationship must originate from the metric's table).
// ---------------------------------------------------------------------------

/// `role_played_fact_def` plus a window metric (qualified, as DDL stores it)
/// over `inner`, partitioned by the base-table `carrier` dimension.
fn role_played_window_def(inner: &str) -> SemanticViewDefinition {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "win".to_string(),
        expr: format!("SUM({inner}) OVER (PARTITION BY carrier)"),
        source_table: Some("f".to_string()),
        window_spec: Some(crate::model::WindowSpec {
            window_function: "SUM".to_string(),
            inner_metric: inner.to_string(),
            partition_dims: vec!["carrier".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });
    def
}

/// Review finding (window): the window metric is qualified, so the join
/// resolver never followed its inner metric's `USING` — the inlined fact named
/// `a__dep_airport` but no scoped join was emitted and `__sv_agg` did not bind.
/// c1 departs twice (2 x 100), c2 once.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_window_over_a_using_metric_over_a_role_played_fact_binds_to_its_role() {
    let sql = expand(
        "v",
        &role_played_window_def("dep_elev"),
        &metrics_req(&["carrier"], &["win"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![
            (Some("c1".to_string()), 200.0),
            (Some("c2".to_string()), 100.0)
        ],
        "{sql}"
    );
}

/// The window over the OTHER role reads the other instance (elevation 7).
#[test]
#[cfg(not(feature = "extension"))]
fn td69_window_over_the_other_role_reads_the_other_instance() {
    let sql = expand(
        "v",
        &role_played_window_def("arr_elev"),
        &metrics_req(&["carrier"], &["win"]),
    )
    .unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![
            (Some("c1".to_string()), 14.0),
            (Some("c2".to_string()), 7.0)
        ],
        "{sql}"
    );
}

/// Review finding (window, fence half): the fence's `source_table.is_some()`
/// branch treated the window metric itself as the base and never looked at its
/// inner metric, so an inner metric with NO `USING` over the role-played fact
/// was never checked.
#[test]
fn td69_fence_window_over_an_unscoped_role_played_fact_is_rejected() {
    let mut def = role_played_window_def("any_elev");
    def.metrics.push(Metric {
        name: "any_elev".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        ..Default::default()
    });
    expect_member_fact_ambiguity(&def, &metrics_req(&["carrier"], &["win"]), "any_elev");
}

/// A semi-additive `USING` metric over the role-played fact. `NON ADDITIVE BY
/// (carrier)` keeps the latest carrier (`c2`, one flight): 100 departing.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_semi_additive_using_metric_over_a_role_played_fact_binds_to_its_role() {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "latest_arr_elev".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["arr_airport".to_string()],
        non_additive_by: vec![crate::model::NonAdditiveDim {
            dimension: "carrier".to_string(),
            ..Default::default()
        }],
        ..Default::default()
    });
    let sql = expand("v", &def, &metrics_req(&[], &["latest_arr_elev"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 7.0)],
        "{sql}"
    );
}

/// Review finding (dedup): `USING (dep_airport, DEP_AIRPORT)` names ONE
/// relationship — validation, the fence and the join resolver all fold case —
/// but the fact rewrite counted the two spellings as two roles, skipped the
/// rewrite, and left the fact on the never-joined bare alias.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_duplicate_using_spellings_name_one_role() {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "dep_elev_twice_named".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["dep_airport".to_string(), "DEP_AIRPORT".to_string()],
        ..Default::default()
    });
    let sql = expand("v", &def, &metrics_req(&[], &["dep_elev_twice_named"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 300.0)],
        "{sql}"
    );
}

/// `role_played_fact_def` with both role-playing edges declared ONE-TO-ONE, so
/// the fan-trap fence (which rejects a many-to-one parent-table metric outright)
/// does not stand in front of the role-playing one.
fn one_to_one_role_played_fact_def() -> SemanticViewDefinition {
    let mut def = role_played_fact_def();
    for j in &mut def.joins {
        j.cardinality = Cardinality::OneToOne;
    }
    def
}

fn metric_on(table: &str, name: &str, expr: &str) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

fn expect_metric_path_ambiguity(def: &SemanticViewDefinition, req: &QueryRequest, member: &str) {
    match expand("v", def, req) {
        Err(ExpandError::AmbiguousMetricPath { detail }) => {
            assert_eq!(detail.metric_name, member);
            assert_eq!(detail.role_playing_table, "a");
        }
        Err(other) => panic!("expected AmbiguousMetricPath, got: {other}"),
        Ok(sql) => panic!("accepted, binding to an arbitrary role:\n{sql}"),
    }
}

/// Review finding (same-table metric): a metric declared ON the role-played
/// table over its own fact skipped the fence (`own == fact_table`), and the
/// base-anchored join bound bare `a` through the first-declared edge: 300 (the
/// departure role) for no reason but declaration order.
#[test]
fn td69_fence_metric_on_the_role_played_table_over_its_own_fact_is_rejected() {
    let mut def = one_to_one_role_played_fact_def();
    def.metrics.push(metric_on("a", "any_elev", "SUM(elev)"));
    expect_metric_path_ambiguity(&def, &metrics_req(&[], &["any_elev"]), "any_elev");
}

/// The same hole without a fact: a raw column on the role-played table. Not a
/// fact reference at all, so no member-fact fence can see it — the metric's own
/// grain table is what has no role.
#[test]
fn td69_fence_metric_on_the_role_played_table_over_a_raw_column_is_rejected() {
    let mut def = one_to_one_role_played_fact_def();
    def.metrics
        .push(metric_on("a", "raw_elev", "SUM(a.elevation)"));
    expect_metric_path_ambiguity(&def, &metrics_req(&["carrier"], &["raw_elev"]), "raw_elev");
}

/// A derived metric over such a metric is rejected through the base it inlines.
#[test]
fn td69_fence_derived_metric_over_a_role_played_table_metric_is_rejected() {
    let mut def = one_to_one_role_played_fact_def();
    def.metrics
        .push(metric_on("a", "raw_elev", "SUM(a.elevation)"));
    def.metrics.push(Metric {
        name: "twice_raw".to_string(),
        expr: "raw_elev * 2".to_string(),
        ..Default::default()
    });
    expect_metric_path_ambiguity(&def, &metrics_req(&[], &["twice_raw"]), "raw_elev");
}

/// A metric on a DESCENDANT of the role-played table hangs off the bare,
/// first-declared instance just the same.
#[test]
fn td69_fence_metric_below_the_role_played_table_is_rejected() {
    let mut def = flights_airports_regions_def();
    for j in &mut def.joins {
        j.cardinality = Cardinality::OneToOne;
    }
    def.metrics
        .push(metric_on("r", "region_rows", "COUNT(r.region_id)"));
    expect_metric_path_ambiguity(&def, &metrics_req(&[], &["region_rows"]), "region_rows");
}

/// CONTROL: with only ONE relationship to `a` there is no role to pick, so the
/// same metric keeps expanding.
///
/// The number is the metric's HOME-table answer (TECH-DEBT #71): `raw_elev` is
/// declared on `a`, so it sums `a`'s rows — 100 + 7 = 107, airport B included
/// though no flight departs from it. It was 300 (three departures at elevation
/// 100) while a metric on a one-to-one neighbour of the base table was summed
/// over the base-anchored join, which counted airport A once per flight — the
/// declared one-to-one the data does not honour.
#[test]
#[cfg(not(feature = "extension"))]
fn td69_metric_on_a_single_relationship_parent_still_expands() {
    let mut def = one_to_one_role_played_fact_def();
    def.joins
        .retain(|j| j.name.as_deref() == Some("dep_airport"));
    def.metrics
        .retain(|m| m.using_relationships.is_empty() && m.source_table.is_some());
    def.metrics
        .push(metric_on("a", "raw_elev", "SUM(a.elevation)"));
    let sql = expand("v", &def, &metrics_req(&[], &["raw_elev"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 107.0)],
        "{sql}"
    );
}

/// `role_played_fact_def` with the role-played table `a` declared FIRST, so it
/// is the base table and both relationships point at the root.
#[cfg(not(feature = "extension"))]
fn role_played_base_def() -> SemanticViewDefinition {
    let mut def = role_played_fact_def();
    def.tables.reverse();
    assert_eq!(def.tables[0].alias, "a");
    def
}

/// v0.14.0 release review F1: a metric declared on the role-played table when
/// that table is the BASE table. It is the anchor of the base-anchored `FROM`
/// and of its own grain, so nothing joins it through a relationship and there
/// is no role to pick. v0.13.0 answered 2 (two airports); the own-table fence
/// rejected it as `AmbiguousMetricPath`.
#[test]
#[cfg(not(feature = "extension"))]
fn metric_on_a_role_played_base_table_over_a_raw_column_expands() {
    let mut def = role_played_base_def();
    def.metrics.push(metric_on("a", "n_airports", "COUNT(*)"));
    let sql = expand("v", &def, &metrics_req(&[], &["n_airports"]))
        .unwrap_or_else(|e| panic!("a base-table metric has no role to pick: {e}"));
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 2.0)],
        "{sql}"
    );
}

/// The same base-table metric over the fact on its own table: 100 + 7.
#[test]
#[cfg(not(feature = "extension"))]
fn metric_on_a_role_played_base_table_over_its_own_fact_expands() {
    let mut def = role_played_base_def();
    def.metrics.push(metric_on("a", "any_elev", "SUM(elev)"));
    let sql = expand("v", &def, &metrics_req(&[], &["any_elev"]))
        .unwrap_or_else(|e| panic!("a base-table metric has no role to pick: {e}"));
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 107.0)],
        "{sql}"
    );
}

/// CONTROL for the two above: the referencing table's metrics keep binding to
/// the role their `USING` names when the role-played table is the base table.
#[test]
#[cfg(not(feature = "extension"))]
fn using_metric_into_a_role_played_base_table_keeps_its_role() {
    let def = role_played_base_def();
    let sql = expand("v", &def, &metrics_req(&[], &["arr_elev"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 21.0)],
        "{sql}"
    );
}

/// A DIMENSION on the role-played base table, co-queried with a `USING`
/// metric from the referencing table, binds to that metric's role: all three
/// flights depart `Dep City` at elevation 100. (Test-coverage review TC1: the
/// dimension branch was only ever exercised with the dimension on a joined
/// table.)
#[test]
#[cfg(not(feature = "extension"))]
fn dimension_on_a_role_played_base_table_binds_to_the_using_role() {
    let def = role_played_base_def();
    let sql = expand("v", &def, &metrics_req(&["city"], &["dep_elev"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Dep City".to_string()), 300.0)],
        "{sql}"
    );
}

/// `one_to_one_role_played_fact_def` with the role-played table declared
/// first, plus a metric on it and one on the referencing table. One-to-one, so
/// the fan-trap fence does not stand in front of the role-playing one.
fn one_to_one_role_played_base_def() -> SemanticViewDefinition {
    let mut def = one_to_one_role_played_fact_def();
    def.tables.reverse();
    def.metrics.push(metric_on("a", "n_airports", "COUNT(*)"));
    def.metrics.push(metric_on("f", "n_flights", "COUNT(*)"));
    def
}

/// Assert the request is rejected as ambiguous in both declaration orders.
fn expect_base_metric_rejected_both_orders(req: &QueryRequest) {
    for swapped in [false, true] {
        let mut def = one_to_one_role_played_base_def();
        if swapped {
            def.joins.swap(0, 1);
        }
        expect_metric_path_ambiguity(&def, req, "n_airports");
    }
}

/// The base-table exemption holds only while the query stays on the base
/// table. A filter on the referencing table joins it across the role-playing
/// relationships, through the first-declared one, so swapping their
/// declaration order changed the answer (found by metamorphic (e) with a
/// role-played root). Rejected, as it was before the exemption.
#[test]
fn base_table_metric_filtered_on_the_referencing_table_is_rejected() {
    let mut req = metrics_req(&[], &["n_airports"]);
    req.where_clause = Some("carrier = 'c1'".to_string());
    expect_base_metric_rejected_both_orders(&req);
}

/// The same with a dimension on the referencing table.
#[test]
fn base_table_metric_grouped_by_a_referencing_table_dimension_is_rejected() {
    expect_base_metric_rejected_both_orders(&metrics_req(&["carrier"], &["n_airports"]));
}

/// The same with a metric on the referencing table co-queried.
#[test]
fn base_table_metric_co_queried_with_a_referencing_table_metric_is_rejected() {
    expect_base_metric_rejected_both_orders(&metrics_req(&[], &["n_airports", "n_flights"]));
}

/// The same through a semi-additive metric on the base table whose
/// `NON ADDITIVE BY` dimension lives on the referencing table: that dimension
/// is not a queried one, but the snapshot must still join its table.
#[test]
fn base_table_semi_additive_metric_by_a_referencing_table_dimension_is_rejected() {
    for swapped in [false, true] {
        let mut def = one_to_one_role_played_base_def();
        if swapped {
            def.joins.swap(0, 1);
        }
        def.metrics.push(Metric {
            name: "last_airports".to_string(),
            expr: "SUM(a.elevation)".to_string(),
            source_table: Some("a".to_string()),
            non_additive_by: vec![crate::model::NonAdditiveDim {
                dimension: "carrier".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        });
        match expand("v", &def, &metrics_req(&[], &["last_airports"])) {
            Err(ExpandError::AmbiguousMetricPath { .. }) => {}
            other => panic!("expected AmbiguousMetricPath (swapped: {swapped}), got: {other:?}"),
        }
    }
}

/// A semi-additive metric on the base table snapshotting by `spelling`.
fn base_semi_additive(spelling: &str) -> Metric {
    Metric {
        name: "last_elev".to_string(),
        expr: "SUM(a.elevation)".to_string(),
        source_table: Some("a".to_string()),
        non_additive_by: vec![crate::model::NonAdditiveDim {
            dimension: spelling.to_string(),
            ..Default::default()
        }],
        ..Default::default()
    }
}

/// The base-table exemption's walk over the dimensions a metric names
/// resolves them the way CREATE does, so a dotted `NON ADDITIVE BY (a.city)`
/// is on the base table just like `city`. It matched the bare name only, so
/// the dotted spelling resolved to nothing and lost the exemption (PR #256
/// review). Checked on the helper itself: end to end, the dimension fence
/// rejects the snapshot dimension first (TECH-DEBT #86, pinned below).
#[test]
fn base_table_exemption_resolves_a_metrics_dimension_in_any_spelling() {
    let def = one_to_one_role_played_base_def();
    for spelling in ["city", "a.city", "A.\"City\""] {
        let m = base_semi_additive(spelling);
        assert_eq!(
            super::role_playing::base_table_if_query_stays_on_it(&def, &[], &[&m], &[]),
            Some("a".to_string()),
            "NON ADDITIVE BY ({spelling}) is on the base table"
        );
    }
    // CONTROL: a snapshot dimension on the referencing table, in either
    // spelling, takes the query off the base table.
    for spelling in ["carrier", "f.carrier"] {
        let m = base_semi_additive(spelling);
        assert_eq!(
            super::role_playing::base_table_if_query_stays_on_it(&def, &[], &[&m], &[]),
            None,
            "NON ADDITIVE BY ({spelling}) is on the referencing table"
        );
    }
    // The window dimensions take the same walk: partitioning, exclusion and
    // ordering each, under both spellings.
    let window = |spelling: &str, on: usize| {
        let names = vec![spelling.to_string()];
        let mut ws = crate::model::WindowSpec {
            window_function: "SUM".to_string(),
            inner_metric: "n_airports".to_string(),
            ..Default::default()
        };
        match on {
            0 => ws.partition_dims = names,
            1 => ws.excluding_dims = names,
            _ => {
                ws.order_by = vec![crate::model::WindowOrderBy {
                    expr: spelling.to_string(),
                    ..Default::default()
                }];
            }
        }
        Metric {
            name: "w".to_string(),
            expr: "SUM(n_airports) OVER ()".to_string(),
            source_table: Some("a".to_string()),
            window_spec: Some(ws),
            ..Default::default()
        }
    };
    for on in 0..3 {
        for (spelling, stays) in [("city", true), ("a.city", true), ("f.carrier", false)] {
            let m = window(spelling, on);
            assert_eq!(
                super::role_playing::base_table_if_query_stays_on_it(&def, &[], &[&m], &[]),
                stays.then(|| "a".to_string()),
                "window dimension {spelling} (slot {on})"
            );
        }
    }
}

/// KNOWN GAP (TECH-DEBT #86): the same base-table dimension with only a
/// metric on the base table, or with no metric, needs no join and so has no
/// role to pick (the answer is each airport's own city), but the dimension
/// check still reports it as ambiguous. Pinned so the fix flips it on purpose:
/// it should then return one row per city with `n_airports` = 1.
#[test]
fn known_gap_base_table_dimension_without_a_joined_metric_is_rejected() {
    let mut def = role_played_fact_def();
    def.tables.reverse();
    def.metrics.push(metric_on("a", "n_airports", "COUNT(*)"));
    for mets in [&["n_airports"][..], &[][..]] {
        match expand("v", &def, &metrics_req(&["city"], mets)) {
            Err(ExpandError::AmbiguousPath {
                dimension_table, ..
            }) => assert_eq!(dimension_table, "a"),
            other => panic!("TECH-DEBT #86 changed ({mets:?}); update the entry: {other:?}"),
        }
    }
    // The same fence rejects a semi-additive base-table metric whose snapshot
    // dimension is on the base table, though nothing is joined: when #86 is
    // fixed, this should expand to 100 (`Dep City`, the last city) in every
    // spelling.
    for spelling in ["city", "a.city"] {
        def.metrics.push(base_semi_additive(spelling));
        match expand("v", &def, &metrics_req(&[], &["last_elev"])) {
            Err(ExpandError::AmbiguousPath {
                dimension_table, ..
            }) => assert_eq!(dimension_table, "a"),
            other => panic!("TECH-DEBT #86 changed ({spelling}); update the entry: {other:?}"),
        }
        def.metrics.pop();
    }
}

// ---------------------------------------------------------------------------
// Relationship names follow DuckDB identifier rules (TECH-DEBT #28 sites).
//
// The body parser stores a relationship name and a `USING` entry as written,
// quotes and all. Every site that matched one against the other compared
// `to_ascii_lowercase()` spellings, so `"Dep_Airport"` and `dep_airport` — one
// identifier to DuckDB — were two different relationships to validation, the
// join resolver, the role-playing fence, the fact rewrite and the per-grain
// path. They now all match with `ident::ident_matches` / normalize with
// `ident::normalize_ident_part`, so the five agree.
// ---------------------------------------------------------------------------

/// Departure relationship declared QUOTED; the metric's `USING` names it bare.
#[cfg(not(feature = "extension"))]
fn mixed_quoting_role_def() -> SemanticViewDefinition {
    let mut def = flights_airports_def();
    def.joins[0].name = Some("\"Dep_Airport\"".to_string());
    def.metrics[0].using_relationships = vec!["dep_airport".to_string()];
    def
}

/// The dimension must bind to the departure role the bare `USING` names: all
/// three flights depart `Dep City`. With ASCII-lowercase matching the join
/// resolver found no relationship for the spelling and emitted no scoped join.
#[test]
#[cfg(not(feature = "extension"))]
fn rel_quoting_bare_using_binds_a_quoted_relationship_role() {
    let def = mixed_quoting_role_def();
    let sql = expand("v", &def, &metrics_req(&["city"], &["departure_count"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Dep City".to_string()), 3.0)],
        "{sql}"
    );
}

/// The converse spelling — bare declaration, quoted upper-case `USING`.
#[test]
#[cfg(not(feature = "extension"))]
fn rel_quoting_quoted_using_binds_a_bare_relationship_role() {
    let mut def = flights_airports_def();
    def.metrics[1].using_relationships = vec!["\"ARR_AIRPORT\"".to_string()];
    let sql = expand("v", &def, &metrics_req(&["city"], &["arrival_count"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Arr City".to_string()), 3.0)],
        "{sql}"
    );
}

/// A `USING` metric over a fact on the role-played table (#69), with the
/// relationship quoted at its declaration: the fence must find exactly one
/// role and the fact rewrite must move `a.elevation` onto that role's alias.
/// Three departures at elevation 100.
#[test]
#[cfg(not(feature = "extension"))]
fn rel_quoting_role_played_fact_binds_a_quoted_relationship_role() {
    let mut def = role_played_fact_def();
    def.joins[0].name = Some("\"Dep_Airport\"".to_string());
    let sql = expand("v", &def, &metrics_req(&[], &["dep_elev"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 300.0)],
        "{sql}"
    );
}

/// `USING (dep_airport, "DEP_AIRPORT")` names one relationship twice. Guard
/// against a half-migration: once every site resolves both spellings to the
/// same edge, the fence and the fact rewrite must still count ONE role
/// (deduplicated by edge), not two.
#[test]
#[cfg(not(feature = "extension"))]
fn rel_quoting_duplicate_spellings_differing_in_quoting_name_one_role() {
    let mut def = role_played_fact_def();
    def.metrics.push(Metric {
        name: "dep_elev_quoted_twice".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["dep_airport".to_string(), "\"DEP_AIRPORT\"".to_string()],
        ..Default::default()
    });
    let sql = expand("v", &def, &metrics_req(&[], &["dep_elev_quoted_twice"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(None, 300.0)],
        "{sql}"
    );
}

// ---------------------------------------------------------------------------
// Legacy stored pairs whose names differ only by quoting (PR #249 review).
//
// Before #249 the role-playing diamond check compared relationship names by
// ASCII-lowercase spelling, so `dep` and `"dep"` were accepted as two roles of
// one table, and every lookup by name — also by spelling — sent `USING (dep)`
// to the bare one and `USING ("dep")` to the quoted one. CREATE now rejects
// such a pair (TECH-DEBT #79), but a row stored earlier is never re-validated,
// so it must keep answering exactly as it did: every lookup prefers the old
// spelling match and only falls back to the identifier rule when no spelling
// matches (`graph::find_relationship` / `RelNameMatch`).
// ---------------------------------------------------------------------------

/// `flights_airports_def` as a pre-#249 stored row: the departure edge is
/// named `dep`, the arrival edge `"dep"` (stored with its quotes). Built
/// directly, bypassing CREATE validation, as a catalog row would be loaded.
#[cfg(not(feature = "extension"))]
fn legacy_quoted_pair_def() -> SemanticViewDefinition {
    let mut def = flights_airports_def();
    def.joins[0].name = Some("dep".to_string());
    def.joins[1].name = Some("\"dep\"".to_string());
    def.metrics[0].using_relationships = vec!["dep".to_string()];
    def.metrics[1].using_relationships = vec!["\"dep\"".to_string()];
    def
}

/// Join resolver: `USING ("dep")` named the quoted (arrival) edge, so all three
/// flights group under `Arr City`. Taking the first identifier match instead
/// joins the departure edge and reports `Dep City`.
#[test]
#[cfg(not(feature = "extension"))]
fn legacy_quoted_pair_using_keeps_binding_the_quoted_relationship() {
    let def = legacy_quoted_pair_def();
    let sql = expand("v", &def, &metrics_req(&["city"], &["arrival_count"])).unwrap();
    assert_eq!(
        run_rows(&role_numbers_db(), &sql),
        vec![(Some("Arr City".to_string()), 3.0)],
        "{sql}"
    );
}

/// `find_using_context`: the two co-queried metrics name two different stored
/// relationships to `a`, so the dimension has no single role — the query was
/// rejected as ambiguous. Deduplicating `USING` entries by normalized name
/// merges them into one role and silently answers with one of the two.
#[test]
#[cfg(not(feature = "extension"))]
fn legacy_quoted_pair_two_roles_in_one_query_stay_ambiguous() {
    let def = legacy_quoted_pair_def();
    let err = expand(
        "v",
        &def,
        &metrics_req(&["city"], &["departure_count", "arrival_count"]),
    )
    .expect_err("two stored relationships, one dimension: no single role");
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected AmbiguousPath, got: {err}"
    );
}

/// The #69 fence: a metric over a role-played fact whose `USING` names BOTH
/// stored relationships has no single role to put the fact on, and was
/// rejected. Counting roles by normalized name counts one and lets it through.
#[test]
#[cfg(not(feature = "extension"))]
fn legacy_quoted_pair_fact_metric_naming_both_roles_stays_rejected() {
    let mut def = role_played_fact_def();
    def.joins[0].name = Some("dep".to_string());
    def.joins[1].name = Some("\"dep\"".to_string());
    def.metrics.push(Metric {
        name: "both_elev".to_string(),
        expr: "SUM(elev)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["dep".to_string(), "\"dep\"".to_string()],
        ..Default::default()
    });
    let err = expand("v", &def, &metrics_req(&[], &["both_elev"]))
        .expect_err("USING names two stored roles of the fact's table");
    assert!(
        matches!(err, ExpandError::AmbiguousMemberFactPath { .. }),
        "expected AmbiguousMemberFactPath, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// Dot-qualified relationship names keep their part boundaries (PR #249 review).
//
// `x.y` (two identifier parts) and `"x.y"` (one part containing a dot) are
// different identifiers to DuckDB, but both normalize to the string `x.y`.
// ---------------------------------------------------------------------------

/// `"x.y"` (to `d`) is declared before `x.y` (to `r`); `USING ("x"."y")` names
/// the two-part `x.y`, so the scoped join must target `r`. Matching on the
/// joined normalized string bound the first-declared `"x.y"` instead.
#[test]
fn using_a_two_part_relationship_name_does_not_bind_a_dotted_one_part_name() {
    let mut def = flights_airports_def();
    def.tables[1].alias = "d".to_string();
    def.tables.push(TableRef {
        alias: "r".to_string(),
        table: "airports".to_string(),
        pk_columns: vec!["airport_code".to_string()],
        ..Default::default()
    });
    def.joins[0].table = "d".to_string();
    def.joins[0].name = Some("\"x.y\"".to_string());
    def.joins[1].table = "r".to_string();
    def.joins[1].name = Some("x.y".to_string());
    def.dimensions = vec![Dimension {
        name: "arr_city".to_string(),
        expr: "r.city".to_string(),
        source_table: Some("r".to_string()),
        ..Default::default()
    }];
    def.metrics = vec![Metric {
        name: "arrivals".to_string(),
        expr: "COUNT(*)".to_string(),
        source_table: Some("f".to_string()),
        using_relationships: vec!["\"x\".\"y\"".to_string()],
        ..Default::default()
    }];
    let sql = expand("v", &def, &metrics_req(&["arr_city"], &["arrivals"])).unwrap();
    assert!(sql.contains(r#"AS "r__x.y""#), "{sql}");
    assert!(!sql.contains(r#""d__x.y""#), "{sql}");
}
