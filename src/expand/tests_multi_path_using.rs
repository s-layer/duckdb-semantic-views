//! Multi-path `USING` (Phase 6.3): metrics that reach one role-played table
//! through different relationships, co-queried by a dimension on that table.
//!
//! Snowflake computes each metric through its own relationship and outer-merges
//! the per-metric results on the queried dimensions, so a city with departures
//! but no arrivals shows a NULL arrival count. These tests pin NUMBERS: each
//! executes the generated SQL against an in-memory DuckDB.

use super::*;
use crate::model::{Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef};

/// Flights between three airports, so every role-scoped number differs from its
/// sibling and some city is missing from each role:
///
/// | flight | from  | to    | carrier |
/// |--------|-------|-------|---------|
/// | 1      | Alpha | Beta  | c1      |
/// | 2      | Alpha | Beta  | c1      |
/// | 3      | Beta  | Alpha | c2      |
/// | 4      | Alpha | Gamma | c2      |
///
/// Departures by city: Alpha 3, Beta 1 (no Gamma row). Arrivals: Alpha 1,
/// Beta 2, Gamma 1.
fn flights_db() -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE flights (flight_id INTEGER, departure_code VARCHAR,
                               arrival_code VARCHAR, carrier VARCHAR, seats INTEGER);
         CREATE TABLE airports (airport_code VARCHAR, city VARCHAR, region_id INTEGER);
         CREATE TABLE regions (region_id INTEGER, region_name VARCHAR);
         INSERT INTO regions VALUES (1, 'North'), (2, 'South');
         INSERT INTO airports VALUES ('A', 'Alpha', 1), ('B', 'Beta', 1), ('C', 'Gamma', 2);
         INSERT INTO flights VALUES (1, 'A', 'B', 'c1', 10), (2, 'A', 'B', 'c1', 20),
                                    (3, 'B', 'A', 'c2', 30), (4, 'A', 'C', 'c2', 40);",
    )
    .expect("fixture data");
    con
}

fn table(alias: &str, name: &str, pk: &str) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: name.to_string(),
        pk_columns: vec![pk.to_string()],
        ..Default::default()
    }
}

fn dim(name: &str, expr: &str, table: &str) -> Dimension {
    Dimension {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: Some(table.to_string()),
        ..Default::default()
    }
}

fn metric(name: &str, expr: &str, table: Option<&str>, using: &[&str]) -> Metric {
    Metric {
        name: name.to_string(),
        expr: expr.to_string(),
        source_table: table.map(str::to_string),
        using_relationships: using.iter().map(|r| (*r).to_string()).collect(),
        ..Default::default()
    }
}

fn rel(name: &str, from: &str, to: &str, fk: &str, pk: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk.to_string()],
        ref_columns: vec![pk.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }
}

/// Snowflake's documented shape: one metric per role, plus a derived metric over
/// both and a metric with no `USING` at all.
fn flights_def() -> SemanticViewDefinition {
    SemanticViewDefinition {
        tables: vec![
            table("f", "flights", "flight_id"),
            table("a", "airports", "airport_code"),
        ],
        dimensions: vec![dim("city", "a.city", "a"), dim("carrier", "f.carrier", "f")],
        metrics: vec![
            metric("departures", "COUNT(*)", Some("f"), &["dep_airport"]),
            metric("arrivals", "COUNT(*)", Some("f"), &["arr_airport"]),
            metric("seats_out", "SUM(f.seats)", Some("f"), &["dep_airport"]),
            metric("movements", "departures + arrivals", None, &[]),
            metric("flight_count", "COUNT(*)", Some("f"), &[]),
        ],
        joins: vec![
            rel("dep_airport", "f", "a", "departure_code", "airport_code"),
            rel("arr_airport", "f", "a", "arrival_code", "airport_code"),
        ],
        ..Default::default()
    }
}

fn req(dims: &[&str], mets: &[&str]) -> QueryRequest {
    QueryRequest {
        where_clause: None,
        facts: vec![],
        dimensions: dims.iter().map(|d| DimensionName::new(*d)).collect(),
        metrics: mets.iter().map(|m| MetricName::new(*m)).collect(),
    }
}

/// Every row of `sql`, each column rendered as text (`NULL` for a NULL), sorted.
fn rows(con: &duckdb::Connection, sql: &str) -> Vec<Vec<String>> {
    let mut stmt = con
        .prepare(sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind, got: {e}\nSQL:\n{sql}"));
    let mut out: Vec<Vec<String>> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            (0..n)
                .map(|i| {
                    let v: duckdb::types::Value = r.get(i)?;
                    Ok(match v {
                        duckdb::types::Value::Null => "NULL".to_string(),
                        duckdb::types::Value::Text(s) => s,
                        other => format!("{other:?}")
                            .trim_start_matches(|c: char| c.is_alphabetic())
                            .trim_matches(|c| c == '(' || c == ')')
                            .to_string(),
                    })
                })
                .collect()
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    out.sort();
    out
}

fn table_of(text: &[&[&str]]) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = text
        .iter()
        .map(|row| row.iter().map(|c| (*c).to_string()).collect())
        .collect();
    out.sort();
    out
}

fn run(def: &SemanticViewDefinition, dims: &[&str], mets: &[&str]) -> Vec<Vec<String>> {
    let sql = expand("v", def, &req(dims, mets)).unwrap_or_else(|e| panic!("{e}"));
    rows(&flights_db(), &sql)
}

/// The documented example: departures and arrivals by city, each through its own
/// relationship, outer-merged. Gamma has no departures and so a NULL there.
#[test]
fn metrics_with_different_using_paths_are_outer_merged_by_city() {
    assert_eq!(
        run(&flights_def(), &["city"], &["departures", "arrivals"]),
        table_of(&[
            &["Alpha", "3", "1"],
            &["Beta", "1", "2"],
            &["Gamma", "NULL", "1"],
        ])
    );
}

/// Two metrics on the SAME path share one group; a third on the other path is
/// merged in beside them.
#[test]
fn metrics_sharing_a_path_are_computed_together() {
    assert_eq!(
        run(
            &flights_def(),
            &["city"],
            &["departures", "seats_out", "arrivals"]
        ),
        table_of(&[
            &["Alpha", "3", "70", "1"],
            &["Beta", "1", "30", "2"],
            &["Gamma", "NULL", "NULL", "1"],
        ])
    );
}

/// A dimension on the metrics' own table groups every path alike.
#[test]
fn a_dimension_on_the_metric_table_groups_each_path() {
    assert_eq!(
        run(
            &flights_def(),
            &["carrier", "city"],
            &["departures", "arrivals"]
        ),
        table_of(&[
            &["c1", "Alpha", "2", "NULL"],
            &["c1", "Beta", "NULL", "2"],
            &["c2", "Alpha", "1", "1"],
            &["c2", "Beta", "1", "NULL"],
            &["c2", "Gamma", "NULL", "1"],
        ])
    );
}

/// A derived metric over both paths is assembled from its components after the
/// merge, like any derived metric whose components sit in different groups: a
/// city missing from one path has a NULL component, so a NULL sum.
#[test]
fn a_derived_metric_over_both_paths_combines_its_components_per_city() {
    assert_eq!(
        run(&flights_def(), &["city"], &["movements"]),
        table_of(&[&["Alpha", "4"], &["Beta", "3"], &["Gamma", "NULL"]])
    );
}

/// Without a dimension on the role-played table there is nothing to merge on
/// per role: the totals are unchanged from before this feature.
#[test]
fn without_a_role_played_dimension_the_totals_are_unchanged() {
    assert_eq!(
        run(&flights_def(), &["carrier"], &["departures", "arrivals"]),
        table_of(&[&["c1", "2", "2"], &["c2", "2", "2"]])
    );
}

/// A metric with no `USING` cannot say which airport a city belongs to, so the
/// query is still refused, as Snowflake refuses it.
#[test]
fn a_co_queried_metric_without_using_keeps_the_query_ambiguous() {
    let err = expand(
        "v",
        &flights_def(),
        &req(&["city"], &["departures", "flight_count"]),
    )
    .unwrap_err();
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected AmbiguousPath, got: {err}"
    );
}

/// A dimension alone names no path at all: still ambiguous.
#[test]
fn a_role_played_dimension_alone_is_still_ambiguous() {
    let err = expand("v", &flights_def(), &req(&["city"], &[])).unwrap_err();
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected AmbiguousPath, got: {err}"
    );
}

// ---------------------------------------------------------------------------
// A metric over another metric on its own table (Snowflake's documented
// `f.deps USING (fdep) AS f.n` pattern): the referenced metric's aggregate is
// computed through the referencing metric's own path.
// ---------------------------------------------------------------------------

/// `flights_def` plus metrics written over the plain `flight_count` and a
/// `total_seats` sum, the way Snowflake's documentation writes them.
fn metric_over_metric_def() -> SemanticViewDefinition {
    let mut def = flights_def();
    def.metrics.extend([
        metric("total_seats", "SUM(f.seats)", Some("f"), &[]),
        metric("deps", "f.flight_count", Some("f"), &["dep_airport"]),
        metric("arrs", "flight_count", Some("f"), &["arr_airport"]),
        metric(
            "avg_seats_out",
            "f.total_seats / f.flight_count",
            Some("f"),
            &["dep_airport"],
        ),
        metric("flights_again", "f.flight_count", Some("f"), &[]),
        metric("deps_again", "f.flights_again", Some("f"), &["dep_airport"]),
    ]);
    def
}

/// The documented pattern: each metric aggregates its referenced metric
/// through its own relationship. A bare reference works as a qualified one.
#[test]
fn a_metric_over_a_same_table_metric_aggregates_through_its_own_path() {
    assert_eq!(
        run(&metric_over_metric_def(), &["city"], &["deps", "arrs"]),
        table_of(&[
            &["Alpha", "3", "1"],
            &["Beta", "1", "2"],
            &["Gamma", "NULL", "1"],
        ])
    );
}

/// An expression over several same-table metrics is computed per group, from
/// the aggregates through the metric's own path: Alpha departs 10+20+40 seats
/// over 3 flights, Beta 30 over 1.
#[test]
fn an_expression_over_same_table_metrics_is_computed_through_the_path() {
    let got = run(&metric_over_metric_def(), &["city"], &["avg_seats_out"]);
    assert_eq!(
        got.len(),
        2,
        "Gamma has no departures, so no group: {got:?}"
    );
    assert_eq!(got[0][0], "Alpha");
    assert!(got[0][1].starts_with("23.33"), "{got:?}");
    assert_eq!(got[1], vec!["Beta".to_string(), "30.0".to_string()]);
}

/// A reference inside an aggregate call is the column, as before: `seats` in
/// `SUM(f.seats)` is not a metric even when one of that name exists.
#[test]
fn a_reference_inside_an_aggregate_is_still_a_column() {
    let mut def = metric_over_metric_def();
    def.metrics
        .push(metric("seats", "SUM(f.seats) * 0", Some("f"), &[]));
    assert_eq!(
        run(&def, &["carrier"], &["total_seats"]),
        table_of(&[&["c1", "30"], &["c2", "70"]])
    );
}

/// A chain of same-table references resolves all the way down, through the
/// path of the metric at the top of the chain.
#[test]
fn a_chain_of_same_table_references_resolves_through_the_outer_path() {
    assert_eq!(
        run(&metric_over_metric_def(), &["city"], &["deps_again"]),
        table_of(&[&["Alpha", "3"], &["Beta", "1"]])
    );
}

/// A semi-additive metric written over a plain same-table metric snapshots
/// like one written with the aggregate itself: the latest flight (4, 40
/// seats) when its snapshot dimension is not queried.
#[test]
fn a_semi_additive_metric_over_a_same_table_metric_snapshots() {
    let mut def = metric_over_metric_def();
    def.dimensions.push(dim("fid", "f.flight_id", "f"));
    let mut latest = metric("latest_seats", "f.total_seats", Some("f"), &[]);
    latest.non_additive_by = vec![crate::model::NonAdditiveDim {
        dimension: "fid".to_string(),
        ..Default::default()
    }];
    def.metrics.push(latest);
    assert_eq!(run(&def, &[], &["latest_seats"]), table_of(&[&["40"]]));
}

/// Metrics that each name a different path cannot be split per path when the
/// query also holds an active semi-additive metric (its snapshot group cannot
/// honour a role, TECH-DEBT #65). The refusal stands, and its advice must not
/// tell the user to add the `USING` every metric already has.
#[test]
fn metrics_naming_different_paths_beside_a_snapshot_are_refused_with_advice_that_fits() {
    let mut def = flights_def();
    def.dimensions.push(dim("fid", "f.flight_id", "f"));
    let mut latest = metric("latest_seats", "SUM(f.seats)", Some("f"), &["dep_airport"]);
    latest.non_additive_by = vec![crate::model::NonAdditiveDim {
        dimension: "fid".to_string(),
        ..Default::default()
    }];
    def.metrics.push(latest);
    let err = expand(
        "v",
        &def,
        &req(&["city"], &["departures", "arrivals", "latest_seats"]),
    )
    .unwrap_err();
    assert!(
        matches!(err, ExpandError::AmbiguousPath { .. }),
        "expected AmbiguousPath, got: {err}"
    );
    assert!(
        err.to_string().contains("cannot be split by path"),
        "the advice must cover metrics that already name a path: {err}"
    );
}

/// `flights_def` plus `wd`, a window over `departures` (`USING (dep_airport)`),
/// and `wa`, the same over `arrivals` (`USING (arr_airport)`).
fn window_def() -> SemanticViewDefinition {
    let mut def = flights_def();
    for (name, inner) in [
        ("wd", "departures"),
        ("wa", "arrivals"),
        ("wf", "flight_count"),
    ] {
        def.metrics.push(Metric {
            name: name.to_string(),
            expr: format!("SUM({inner})"),
            source_table: Some("f".to_string()),
            window_spec: Some(crate::model::WindowSpec {
                window_function: "SUM".to_string(),
                inner_metric: inner.to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec![],
                order_by: vec![],
                frame_clause: None,
            }),
            ..Default::default()
        });
    }
    def
}

/// A window metric reads its inner metric through that metric's `USING`, so
/// grouping it by a role-played dimension takes that path: departures per
/// departure city.
#[test]
fn a_window_over_a_using_metric_groups_through_its_path() {
    assert_eq!(
        run(&window_def(), &["city"], &["wd"]),
        table_of(&[&["Alpha", "3"], &["Beta", "1"]])
    );
    assert_eq!(
        run(&window_def(), &["city"], &["wa"]),
        table_of(&[&["Alpha", "1"], &["Beta", "2"], &["Gamma", "1"]])
    );
}

/// A window over a metric with no `USING` names no path, and two windows over
/// different paths cannot be split per path: both stay refused.
#[test]
fn a_window_without_a_path_or_beside_another_path_is_refused() {
    for mets in [&["wf"][..], &["wd", "wa"][..]] {
        let err = expand("v", &window_def(), &req(&["city"], mets)).unwrap_err();
        assert!(
            matches!(err, ExpandError::AmbiguousPath { .. }),
            "{mets:?}: expected AmbiguousPath, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// Phase 6.3b: a dimension one relationship past the role-played table.
// Airports A and B are in North, C in South.
// ---------------------------------------------------------------------------

/// `flights_def` plus `r` (regions), reached only through the role-played
/// airports: `a(region_id) REFERENCES r`.
fn regions_def() -> SemanticViewDefinition {
    let mut def = window_def();
    def.tables.push(table("r", "regions", "region_id"));
    def.dimensions.push(dim("region", "r.region_name", "r"));
    def.joins
        .push(rel("airport_region", "a", "r", "region_id", "region_id"));
    def
}

/// Snowflake's transitive hop: the metric's `USING` names the path to
/// airports, and the single relationship onward to regions follows it. All
/// four flights depart from North; three arrive there and one in South.
#[test]
fn a_dimension_past_the_role_played_table_follows_the_metrics_path() {
    assert_eq!(
        run(&regions_def(), &["region"], &["departures"]),
        table_of(&[&["North", "4"]])
    );
    assert_eq!(
        run(&regions_def(), &["region"], &["arrivals"]),
        table_of(&[&["North", "3"], &["South", "1"]])
    );
}

/// Metrics through different paths are merged per region, as they are per city.
#[test]
fn metrics_with_different_paths_are_merged_by_a_region() {
    assert_eq!(
        run(&regions_def(), &["region"], &["departures", "arrivals"]),
        table_of(&[&["North", "4", "3"], &["South", "NULL", "1"]])
    );
    assert_eq!(
        run(&regions_def(), &["region"], &["movements"]),
        table_of(&[&["North", "7"], &["South", "NULL"]])
    );
}

/// The region and the city of one role group together.
#[test]
fn a_region_and_a_city_group_through_the_same_path() {
    assert_eq!(
        run(&regions_def(), &["region", "city"], &["departures"]),
        table_of(&[&["North", "Alpha", "3"], &["North", "Beta", "1"]])
    );
}

/// A window metric reaches the region through its inner metric's path.
#[test]
fn a_window_reaches_a_region_through_its_inner_metric() {
    assert_eq!(
        run(&regions_def(), &["region"], &["wa"]),
        table_of(&[&["North", "3"], &["South", "1"]])
    );
}

/// A metric with no path to airports still cannot say which region it means.
#[test]
fn a_region_with_a_metric_that_names_no_path_is_refused() {
    for mets in [&["flight_count"][..], &["departures", "flight_count"][..]] {
        let err = expand("v", &regions_def(), &req(&["region"], mets)).unwrap_err();
        assert!(
            matches!(err, ExpandError::AmbiguousDescendantPath { .. }),
            "{mets:?}: expected AmbiguousDescendantPath, got: {err}"
        );
    }
}

/// A semi-additive metric whose snapshot dimension is a region snapshots
/// through its own `USING` path, also when the region is not queried (the
/// table is then joined only for the snapshot ordering). Every flight departs
/// from North, so each carrier's flights all rank first.
#[test]
fn a_snapshot_over_a_region_joins_it_through_the_metrics_path() {
    let mut def = regions_def();
    let mut latest = metric(
        "latest_dep_seats",
        "SUM(f.seats)",
        Some("f"),
        &["dep_airport"],
    );
    latest.non_additive_by = vec![crate::model::NonAdditiveDim {
        dimension: "region".to_string(),
        ..Default::default()
    }];
    def.metrics.push(latest);
    assert_eq!(
        run(&def, &["carrier"], &["latest_dep_seats"]),
        table_of(&[&["c1", "30"], &["c2", "70"]])
    );
    assert_eq!(
        run(&def, &["region"], &["latest_dep_seats"]),
        table_of(&[&["North", "100"]])
    );
}

/// TECH-DEBT #65: the hop reaches dimensions only. A metric over a FACT on the
/// descendant table is still refused, loudly, rather than joined off the
/// first-declared airport.
#[test]
fn a_metric_over_a_fact_past_the_role_played_table_is_refused() {
    let mut def = regions_def();
    def.facts.push(crate::model::Fact {
        name: "rname".to_string(),
        expr: "r.region_name".to_string(),
        source_table: Some("r".to_string()),
        ..Default::default()
    });
    def.metrics.push(metric(
        "dep_regions",
        "COUNT(DISTINCT r.rname)",
        Some("f"),
        &["dep_airport"],
    ));
    for dims in [&[][..], &["carrier"][..]] {
        let err = expand("v", &def, &req(dims, &["dep_regions"])).unwrap_err();
        assert!(
            matches!(err, ExpandError::AmbiguousMemberFactPath { .. }),
            "{dims:?}: {err}"
        );
    }
}
