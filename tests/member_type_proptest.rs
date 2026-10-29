//! Randomized check of the read-side member types SHOW / DESCRIBE report
//! (TECH-DEBT #51, `query::member_types`).
//!
//! A two-table star (`t` many-to-one `u`) is generated with columns of random
//! types, and over it random dimensions, facts, aggregate metrics and derived
//! metrics. For every member, two independent formulations must agree with the
//! type `infer_member_types` reports:
//!
//! 1. **An oracle that never goes through expansion.** A dimension's or fact's
//!    type is `typeof(<expr>)` evaluated directly against its table; an
//!    aggregate metric's is `typeof(<agg>(<col>))` over its table; a derived
//!    metric's is `typeof` of its operator applied to columns typed as each
//!    operand's type. `semantic_view()` returns the expanded SQL's own types,
//!    so no declaration rule maps the result (`SUM` over an integer stays a
//!    HUGEINT).
//! 2. **The type the member has when queried with others.** A random request
//!    mixing the member with other members is expanded and probed the way
//!    `semantic_view()` binds it; every member it returns must carry the same
//!    type the batched / per-member SHOW probe reported. This is what makes the
//!    probe's batching (one request per member kind, falling back to one per
//!    member) safe: a member's type does not depend on its co-queried members.
//!
//! A **drop axis** removes one source column after the definition is built, so
//! the members reading it can no longer bind: those must report no type, and
//! every other member must still report its oracle type — the batch falls back
//! to per-member probes rather than losing the whole view.
//!
//! A **role-playing axis** declares a second relationship from `t` to `u`, with
//! a quoted name, so every member on `u` is role-played: it cannot be queried
//! without `USING`, and the probe types it through a one-relationship copy of
//! the definition (`single_role_copy`). Its oracle type does not change — a
//! type never depends on which relationship reached the row.
//!
//! Not generated here: window and semi-additive metrics, and PRIVATE members.
//! They are covered by `src/query/member_types.rs` unit tests and
//! `test/sql/member_data_type.test`; neither changes how a type is computed,
//! only which request shape the probe has to use.

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, FactName, MetricName, QueryRequest};
use semantic_views::model::{
    Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};
use semantic_views::query::member_types::{
    infer_member_types, probe_output_types, MemberTypes, Wanted,
};
use semantic_views::test_helpers::{execute_sql_raw, RawDb};

use libduckdb_sys as ffi;

/// Column type pool: (SQL type, literal for the one data row, category).
const TYPES: [(&str, &str, Cat); 12] = [
    ("INTEGER", "7", Cat::Int),
    ("BIGINT", "7", Cat::Int),
    ("SMALLINT", "7", Cat::Int),
    ("UTINYINT", "7", Cat::Int),
    ("HUGEINT", "7", Cat::Int),
    ("DOUBLE", "1.5", Cat::Float),
    ("FLOAT", "1.5", Cat::Float),
    ("DECIMAL(10,2)", "1.5", Cat::Float),
    ("DECIMAL(18,4)", "1.5", Cat::Float),
    ("VARCHAR", "'abc'", Cat::Text),
    ("DATE", "DATE '2024-01-02'", Cat::Date),
    ("BOOLEAN", "true", Cat::Bool),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cat {
    Int,
    Float,
    Text,
    Date,
    Bool,
}

const ROW_TEMPLATES: [&str; 6] = [
    "{c}",
    "{c} + 1",
    "{c} * 2",
    "upper({c})",
    "year({c})",
    "{c} IS NULL",
];

fn row_template_fits(i: usize, cat: Cat) -> bool {
    match i {
        0 | 5 => true,
        1 | 2 => matches!(cat, Cat::Int | Cat::Float),
        3 => cat == Cat::Text,
        4 => cat == Cat::Date,
        _ => false,
    }
}

const AGGS: [&str; 6] = ["SUM", "AVG", "COUNT", "MIN", "MAX", "COUNT_DISTINCT"];

fn agg_fits(i: usize, cat: Cat) -> bool {
    match i {
        0 | 1 => matches!(cat, Cat::Int | Cat::Float),
        _ => true,
    }
}

fn agg_expr(i: usize, col: &str) -> String {
    if AGGS[i] == "COUNT_DISTINCT" {
        format!("COUNT(DISTINCT {col})")
    } else {
        format!("{}({col})", AGGS[i])
    }
}

const DERIVED_OPS: [&str; 3] = ["{a} + {b}", "{a} / {b}", "{a} * 2"];

#[derive(Debug, Clone)]
struct Case {
    /// Column types of `t` and `u` (indexes into [`TYPES`]).
    t_cols: Vec<usize>,
    u_cols: Vec<usize>,
    /// (on_u, column, template) — dimensions; facts reuse the same shape.
    dims: Vec<(bool, usize, usize)>,
    facts: Vec<(bool, usize, usize)>,
    /// (on_u, column, aggregate)
    metrics: Vec<(bool, usize, usize)>,
    /// (op, a, b) over metric indexes
    derived: Vec<(usize, usize, usize)>,
    /// Column of `t` to drop after CREATE, if any.
    drop_t_col: Option<usize>,
    /// Seed choosing the co-queried request members.
    pick: Vec<bool>,
    /// Declare a second, quoted relationship `t` → `u`, making `u` role-played.
    role_played: bool,
}

/// A member over one column: (on `u`?, column index, template / aggregate).
/// Fitness to the column's type is repaired after generation.
fn arb_member(n_t: usize, n_u: usize, n_tpl: usize) -> impl Strategy<Value = (bool, usize, usize)> {
    (any::<bool>(), 0..n_t.max(n_u), 0..n_tpl).prop_map(move |(on_u, c, tpl)| {
        let c = if on_u { c % n_u } else { c % n_t };
        (on_u, c, tpl)
    })
}

fn arb_case() -> impl Strategy<Value = Case> {
    (
        prop::collection::vec(0..TYPES.len(), 1..5),
        prop::collection::vec(0..TYPES.len(), 1..4),
    )
        .prop_flat_map(|(t_cols, u_cols)| {
            let (nt, nu) = (t_cols.len(), u_cols.len());
            (
                Just(t_cols),
                Just(u_cols),
                prop::collection::vec(arb_member(nt, nu, ROW_TEMPLATES.len()), 1..5),
                prop::collection::vec(arb_member(nt, nu, ROW_TEMPLATES.len()), 0..3),
                prop::collection::vec(arb_member(nt, nu, AGGS.len()), 1..5),
                prop::collection::vec((0..DERIVED_OPS.len(), 0..8usize, 0..8usize), 0..3),
                prop::option::weighted(0.3, 0..nt),
                prop::collection::vec(any::<bool>(), 16),
                prop::bool::weighted(0.3),
            )
        })
        .prop_map(
            |(t_cols, u_cols, dims, facts, metrics, derived, drop_t_col, pick, role_played)| {
                let cat = |on_u: bool, c: usize| TYPES[if on_u { u_cols[c] } else { t_cols[c] }].2;
                // Re-target any template / aggregate that does not fit its
                // column's category to the always-valid first one, so every
                // generated definition is well-typed before the drop axis.
                let fix_row = |v: Vec<(bool, usize, usize)>| -> Vec<(bool, usize, usize)> {
                    v.into_iter()
                        .map(|(u, c, tpl)| {
                            (
                                u,
                                c,
                                if row_template_fits(tpl, cat(u, c)) {
                                    tpl
                                } else {
                                    0
                                },
                            )
                        })
                        .collect()
                };
                let metrics: Vec<_> = metrics
                    .into_iter()
                    .map(|(u, c, a)| (u, c, if agg_fits(a, cat(u, c)) { a } else { 2 }))
                    .collect();
                let nm = metrics.len();
                let derived = derived
                    .into_iter()
                    .map(|(op, a, b)| (op, a % nm, b % nm))
                    .collect();
                Case {
                    dims: fix_row(dims),
                    facts: fix_row(facts),
                    metrics,
                    derived,
                    t_cols,
                    u_cols,
                    drop_t_col,
                    pick,
                    role_played,
                }
            },
        )
}

fn col_ref(on_u: bool, c: usize) -> String {
    format!("{}.c{c}", if on_u { "u" } else { "t" })
}

fn row_expr(on_u: bool, c: usize, tpl: usize) -> String {
    ROW_TEMPLATES[tpl].replace("{c}", &col_ref(on_u, c))
}

fn table_name(on_u: bool) -> &'static str {
    if on_u {
        "u"
    } else {
        "t"
    }
}

fn build_def(case: &Case) -> SemanticViewDefinition {
    let tables = vec![
        TableRef {
            alias: "t".to_string(),
            table: "t".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        },
        TableRef {
            alias: "u".to_string(),
            table: "u".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        },
    ];
    let dimensions = case
        .dims
        .iter()
        .enumerate()
        .map(|(i, &(u, c, tpl))| Dimension {
            name: format!("d{i}"),
            expr: row_expr(u, c, tpl),
            source_table: Some(table_name(u).to_string()),
            ..Default::default()
        })
        .collect();
    let facts = case
        .facts
        .iter()
        .enumerate()
        .map(|(i, &(u, c, tpl))| Fact {
            name: format!("f{i}"),
            expr: row_expr(u, c, tpl),
            source_table: Some(table_name(u).to_string()),
            ..Default::default()
        })
        .collect();
    let mut metrics: Vec<Metric> = case
        .metrics
        .iter()
        .enumerate()
        .map(|(i, &(u, c, a))| Metric {
            name: format!("m{i}"),
            expr: agg_expr(a, &col_ref(u, c)),
            source_table: Some(table_name(u).to_string()),
            ..Default::default()
        })
        .collect();
    for (i, &(op, a, b)) in case.derived.iter().enumerate() {
        metrics.push(Metric {
            name: format!("x{i}"),
            expr: DERIVED_OPS[op]
                .replace("{a}", &format!("m{a}"))
                .replace("{b}", &format!("m{b}")),
            source_table: None,
            ..Default::default()
        });
    }
    SemanticViewDefinition {
        variables: Vec::new(),
        max_staleness: None,
        module_custom_instructions: Default::default(),
        verified_queries: Vec::new(),
        tables,
        dimensions,
        metrics,
        joins: std::iter::once(("fk", "t_u"))
            .chain(case.role_played.then_some(("fk2", "\"T u2\"")))
            .map(|(fk, name)| Join {
                from_alias: "t".to_string(),
                table: "u".to_string(),
                fk_columns: vec![fk.to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some(name.to_string()),
                cardinality: Cardinality::ManyToOne,
            })
            .collect(),
        facts,
        materializations: vec![],
        created_on: None,
        database_name: None,
        schema_name: None,
        resolution_schema_name: None,
        temp_tables: vec![],
        comment: None,
        tags: Vec::new(),
    }
}

fn make_db(case: &Case) -> RawDb {
    let db = RawDb::open_in_memory();
    let cols = |v: &[usize]| -> (String, String) {
        let decl: Vec<String> = v
            .iter()
            .enumerate()
            .map(|(i, &ty)| format!("c{i} {}", TYPES[ty].0))
            .collect();
        let vals: Vec<String> = v
            .iter()
            .map(|&ty| format!("CAST({} AS {})", TYPES[ty].1, TYPES[ty].0))
            .collect();
        (decl.join(", "), vals.join(", "))
    };
    let (ud, uv) = cols(&case.u_cols);
    let (td, tv) = cols(&case.t_cols);
    unsafe {
        db.exec(&format!("CREATE TABLE u (id INTEGER PRIMARY KEY, {ud})"));
        db.exec(&format!(
            "CREATE TABLE t (id INTEGER PRIMARY KEY, fk INTEGER, fk2 INTEGER, {td})"
        ));
        db.exec(&format!("INSERT INTO u VALUES (1, {uv})"));
        db.exec(&format!("INSERT INTO t VALUES (1, 1, 1, {tv})"));
    }
    db
}

/// Read the single VARCHAR cell of `sql`'s first row.
fn scalar(db: &RawDb, sql: &str) -> String {
    try_scalar(db, sql).unwrap_or_else(|e| panic!("oracle `{sql}`: {e}"))
}

/// [`scalar`], returning the binder error instead of panicking.
fn try_scalar(db: &RawDb, sql: &str) -> Result<String, String> {
    unsafe {
        let mut result = execute_sql_raw(db.conn, sql)?;
        let p = ffi::duckdb_value_varchar(&raw mut result, 0, 0);
        let s = std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned();
        ffi::duckdb_free(p.cast());
        ffi::duckdb_destroy_result(&raw mut result);
        Ok(s)
    }
}

/// Oracle: (dimension types, fact types, metric types incl. derived), raw.
/// A derived metric whose operator does not apply to its operands' types
/// (`DATE + DATE`) is ill-typed: its oracle type is empty, and so must be the
/// reported one — it cannot be queried either.
fn oracle(case: &Case, db: &RawDb) -> (Vec<String>, Vec<String>, Vec<String>) {
    let row = |v: &[(bool, usize, usize)]| -> Vec<String> {
        v.iter()
            .map(|&(u, c, tpl)| {
                scalar(
                    db,
                    &format!(
                        "SELECT typeof({}) FROM {} LIMIT 1",
                        row_expr(u, c, tpl),
                        table_name(u)
                    ),
                )
            })
            .collect()
    };
    let mut metrics: Vec<String> = case
        .metrics
        .iter()
        .map(|&(u, c, a)| {
            scalar(
                db,
                &format!(
                    "SELECT typeof({}) FROM {}",
                    agg_expr(a, &col_ref(u, c)),
                    table_name(u)
                ),
            )
        })
        .collect();
    for (i, &(op, a, b)) in case.derived.iter().enumerate() {
        // An operand that is itself untypable makes the derived one untypable.
        let t = if metrics[a].is_empty() || metrics[b].is_empty() {
            String::new()
        } else {
            // Column references, not `CAST(NULL AS T)` literals: DuckDB folds
            // an all-NULL constant expression to type "NULL".
            let tbl = format!("oracle_x{i}");
            unsafe {
                db.exec(&format!(
                    "CREATE TEMP TABLE {tbl} AS SELECT CAST(NULL AS {}) AS a, CAST(NULL AS {}) AS b",
                    metrics[a], metrics[b]
                ));
            }
            let expr = DERIVED_OPS[op].replace("{a}", "a").replace("{b}", "b");
            let t =
                try_scalar(db, &format!("SELECT typeof({expr}) FROM {tbl}")).unwrap_or_default();
            unsafe { db.exec(&format!("DROP TABLE {tbl}")) };
            t
        };
        metrics.push(t);
    }
    (row(&case.dims), row(&case.facts), metrics)
}

/// Members (by name) whose expression reads the dropped column of `t`.
fn broken_by_drop(case: &Case) -> Vec<String> {
    let Some(dc) = case.drop_t_col else {
        return vec![];
    };
    let hit = |&(u, c, _): &(bool, usize, usize)| !u && c == dc;
    let mut out = Vec::new();
    for (i, m) in case.dims.iter().enumerate() {
        if hit(m) {
            out.push(format!("d{i}"));
        }
    }
    for (i, m) in case.facts.iter().enumerate() {
        if hit(m) {
            out.push(format!("f{i}"));
        }
    }
    for (i, m) in case.metrics.iter().enumerate() {
        if hit(m) {
            out.push(format!("m{i}"));
        }
    }
    for (i, &(op, a, b)) in case.derived.iter().enumerate() {
        let uses_b = DERIVED_OPS[op].contains("{b}");
        if hit(&case.metrics[a]) || (uses_b && hit(&case.metrics[b])) {
            out.push(format!("x{i}"));
        }
    }
    out
}

fn reported(types: &MemberTypes, name: &str) -> String {
    match name.as_bytes()[0] {
        b'd' => types.dimension(name, None),
        b'f' => types.fact(name, None),
        _ => types.metric(name, None),
    }
}

/// Run both properties; return how many co-queried columns property 2 compared.
fn check(case: &Case) -> Result<usize, TestCaseError> {
    let def = build_def(case);
    let db = make_db(case);
    let (dims, facts, metrics) = oracle(case, &db);
    if let Some(dc) = case.drop_t_col {
        unsafe { db.exec(&format!("ALTER TABLE t DROP COLUMN c{dc}")) };
    }
    let broken = broken_by_drop(case);

    let types = infer_member_types("v", &def, Wanted::ALL, &|| false, &mut |sql| unsafe {
        probe_output_types(db.conn, sql)
    })
    .expect("nothing cancels this call");

    // Property 1: every member reports its independently computed type, or
    // nothing when its source column was dropped.
    let expected = dims
        .iter()
        .enumerate()
        .map(|(i, t)| (format!("d{i}"), t))
        .chain(facts.iter().enumerate().map(|(i, t)| (format!("f{i}"), t)))
        .chain(metrics.iter().enumerate().map(|(i, t)| {
            let n = if i < case.metrics.len() {
                format!("m{i}")
            } else {
                format!("x{}", i - case.metrics.len())
            };
            (n, t)
        }));
    for (name, raw) in expected {
        let want = if broken.contains(&name) || raw.is_empty() {
            String::new()
        } else {
            raw.clone()
        };
        prop_assert_eq!(
            reported(&types, &name),
            want,
            "member {} (raw oracle type {})\ncase: {:?}",
            name,
            raw,
            case
        );
    }

    // Property 2: a random co-queried request returns each member with the
    // type the probe reported for it on its own kind's batch.
    let pick = |names: Vec<String>, off: usize| -> Vec<String> {
        names
            .into_iter()
            .enumerate()
            .filter(|(i, _)| case.pick[(i + off) % case.pick.len()])
            .map(|(_, n)| n)
            .collect()
    };
    let dim_names = pick((0..case.dims.len()).map(|i| format!("d{i}")).collect(), 0);
    let met_names = pick(def.metrics.iter().map(|m| m.name.clone()).collect(), 5);
    let fact_names = pick((0..case.facts.len()).map(|i| format!("f{i}")).collect(), 11);
    let mut compared = 0usize;
    // A facts request lists the dimensions in FACTS too (Snowflake's `2026_07`
    // rule refuses FACTS with DIMENSIONS), where they are read row by row.
    let dims_and_facts: Vec<String> = dim_names.iter().cloned().chain(fact_names).collect();
    for (dims, mets, facts) in [
        (dim_names.clone(), met_names, vec![]),
        (vec![], vec![], dims_and_facts),
    ] {
        let req = QueryRequest {
            dimensions: dims.iter().map(|n| DimensionName::new(n.clone())).collect(),
            metrics: mets.iter().map(|n| MetricName::new(n.clone())).collect(),
            facts: facts.iter().map(|n| FactName::new(n.clone())).collect(),
            where_clause: None, // PIN: types do not depend on row filtering; the request shape is what varies here
        };
        if req.dimensions.is_empty() && req.metrics.is_empty() && req.facts.is_empty() {
            continue;
        }
        let Ok(sql) = expand("v", &def, &req) else {
            continue;
        };
        let Ok(cols) = (unsafe { probe_output_types(db.conn, &format!("{sql} LIMIT 0")) }) else {
            continue;
        };
        for c in cols {
            compared += 1;
            prop_assert_eq!(
                &reported(&types, &c.name),
                &c.data_type,
                "member {} co-queried with {:?}/{:?}/{:?}\ncase: {:?}",
                c.name,
                dim_names,
                mets,
                facts,
                case
            );
        }
    }
    Ok(compared)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn reported_member_types_match_the_oracle_and_the_query(case in arb_case()) {
        check(&case)?;
    }
}

/// Anti-vacuity: the generator reaches the cells the properties are about —
/// type-changing aggregates, a SUM over an integer (HUGEINT), derived metrics, facts on
/// both tables, and the drop axis breaking some members but not all.
#[test]
fn generator_reaches_the_interesting_cells() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = arb_case();
    let (mut avg, mut sum_int, mut derived, mut u_fact, mut partial_drop) =
        (false, false, false, false, false);
    let mut role_played_u_members = 0usize;
    for _ in 0..400 {
        let c = strat.new_tree(&mut runner).unwrap().current();
        let cat = |u: bool, col: usize| TYPES[if u { c.u_cols[col] } else { c.t_cols[col] }].2;
        avg |= c.metrics.iter().any(|&(_, _, a)| a == 1);
        sum_int |= c
            .metrics
            .iter()
            .any(|&(u, col, a)| a == 0 && cat(u, col) == Cat::Int);
        derived |= !c.derived.is_empty();
        u_fact |= c.facts.iter().any(|&(u, _, _)| u);
        let broken = broken_by_drop(&c).len();
        partial_drop |=
            broken > 0 && broken < c.dims.len() + c.facts.len() + c.metrics.len() + c.derived.len();
        let on_u = |v: &[(bool, usize, usize)]| v.iter().any(|&(u, _, _)| u);
        role_played_u_members +=
            usize::from(c.role_played && on_u(&c.dims) && on_u(&c.facts) && on_u(&c.metrics));
    }
    assert!(
        avg && sum_int && derived && u_fact && partial_drop,
        "avg={avg} sum_int={sum_int} derived={derived} u_fact={u_fact} partial_drop={partial_drop}"
    );
    // Every member kind on the role-played table, typed through the
    // one-relationship copy, in a fair share of cases.
    assert!(
        role_played_u_members >= 20,
        "only {role_played_u_members}/400 cases put a dimension, fact and metric on a role-played `u`"
    );
}

/// Anti-vacuity for property 2: generated co-queried requests are accepted and
/// compared often enough to mean something (a request the expander rejects, or
/// an all-`false` pick, compares nothing).
#[test]
fn co_queried_requests_are_compared() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = arb_case();
    let (mut cases_with, mut total) = (0usize, 0usize);
    for _ in 0..60 {
        let c = strat.new_tree(&mut runner).unwrap().current();
        let n = check(&c).unwrap_or_else(|e| panic!("property failed: {e}"));
        total += n;
        cases_with += usize::from(n > 0);
    }
    assert!(
        cases_with >= 20 && total >= 60,
        "only {cases_with}/60 cases compared a co-queried column ({total} columns)"
    );
}
