//! Differential proptest for **many-to-many bridges** (Phase 7.3).
//!
//! Shape: a bridge table `bt` references both `h` (`bt.hid -> h.id`) and `x`
//! (`bt.xid -> x.id`). Each side has its own many-to-one parent: `h.fk_z ->
//! z.id` and `x.fk_y -> y.id`. When the base table is `h`, a plain child
//! `c.hid -> h.id` hangs under `h` too. CREATE has accepted that child with
//! any base table since 0.19.0; the harness declares it only where its oracle
//! models it. The base table is drawn from `h`, `x`,
//! `bt` and `z`, because a bridge must not depend on which table comes first.
//!
//! Every relationship is many-to-one, and the data holds NULL and dangling
//! foreign keys, NULL dimension values and values, duplicate bridge rows,
//! `h` and `x` rows with no bridge row, and bridge rows that reach nothing.
//!
//! Two invariants are checked per case:
//!
//! 1. **Accepted exactly when Snowflake would answer.** A metric on `h` may be
//!    grouped or filtered by a member on `z` (its parent), on `bt`, `x` or `y`
//!    (across the bridge), but not on `c`, a plain child below its grain. A
//!    metric on `x` likewise reaches `y` directly and `bt`, `h` and `z` across
//!    the bridge, and a metric on `bt` reaches every table but `c` many-to-one.
//!    Anything with two fan-outs or a fan-out after a hop up is refused. The
//!    rule is restated here from the topology, not read from the expander.
//! 2. **Accepted query ⇒ numerically correct.** The oracle is computed in
//!    Rust, never in SQL: for each row of a metric's home table it enumerates
//!    the rows it reaches through the bridge (one row with NULLs if none),
//!    keeps those the predicate is TRUE on, and takes the distinct groups. Each
//!    (home row, group) pair then contributes once. The metrics are merged on
//!    the groups, a group one metric lacks reading NULL for it, as Snowflake
//!    merges per-metric results.

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, MetricName, QueryRequest};
use semantic_views::model::{
    Cardinality, Dimension, Join, Metric, SemanticViewDefinition, TableRef,
};

type Cell = Option<i64>;

/// A generated instance. Row `i` of `h`, `x`, `z`, `y` and `c` has id `i`.
#[derive(Debug, Clone)]
struct Instance {
    /// `(fk_z, hcat, hv)`.
    h: Vec<(Cell, Cell, Cell)>,
    /// `(fk_y, xcat, xv)`.
    x: Vec<(Cell, Cell, Cell)>,
    /// `zcat`.
    z: Vec<Cell>,
    /// `ycat`.
    y: Vec<Cell>,
    /// `(hid, xid, rcat, bv)`.
    bt: Vec<(Cell, Cell, Cell, Cell)>,
    /// `(hid, ccat)`.
    c: Vec<(Cell, Cell)>,
}

/// The tables, and which one each dimension and metric lives on.
const TABLES: [&str; 6] = ["h", "x", "bt", "z", "y", "c"];
/// `(dimension name, table, column)`.
const DIMS: [(&str, &str, &str); 6] = [
    ("hcat", "h", "hcat"),
    ("xcat", "x", "xcat"),
    ("rcat", "bt", "rcat"),
    ("zcat", "z", "zcat"),
    ("ycat", "y", "ycat"),
    ("ccat", "c", "ccat"),
];
/// `(metric name, table, aggregate, value column)`; `None` is `COUNT(*)`.
const METS: [(&str, &str, Option<&str>); 5] = [
    ("sum_hv", "h", Some("hv")),
    ("cnt_h", "h", None),
    ("sum_xv", "x", Some("xv")),
    ("cnt_bt", "bt", None),
    ("sum_bv", "bt", Some("bv")),
];

/// How a metric's home table reaches a member's table, restated from the
/// topology: `true` when the query is answerable (the same table, a
/// many-to-one path, or one fan-out straight into the bridge followed by
/// many-to-one hops), `false` when it fans otherwise.
fn reachable(home: &str, table: &str) -> bool {
    match home {
        "h" => table != "c",
        "x" => table != "c",
        "bt" => table != "c",
        _ => unreachable!("no metric on {home}"),
    }
}

/// A generated predicate over the dimensions (by index into `DIMS`).
#[derive(Debug, Clone)]
enum Pred {
    Eq(usize, i64),
    IsNull(usize),
    In(usize, i64, i64),
    Or(Box<Pred>, Box<Pred>),
    And(Box<Pred>, Box<Pred>),
}

impl Pred {
    fn sql(&self) -> String {
        match self {
            Pred::Eq(d, k) => format!("{} = {k}", DIMS[*d].0),
            Pred::IsNull(d) => format!("{} IS NULL", DIMS[*d].0),
            Pred::In(d, a, b) => format!("{} IN ({a}, {b})", DIMS[*d].0),
            Pred::Or(a, b) => format!("({}) OR ({})", a.sql(), b.sql()),
            Pred::And(a, b) => format!("({}) AND ({})", a.sql(), b.sql()),
        }
    }

    fn dims(&self, out: &mut Vec<usize>) {
        match self {
            Pred::Eq(d, _) | Pred::IsNull(d) | Pred::In(d, _, _) => out.push(*d),
            Pred::Or(a, b) | Pred::And(a, b) => {
                a.dims(out);
                b.dims(out);
            }
        }
    }

    /// SQL three-valued logic: `None` is UNKNOWN.
    fn eval(&self, row: &Wide) -> Option<bool> {
        match self {
            Pred::Eq(d, k) => row.dim(*d).map(|v| v == *k),
            Pred::IsNull(d) => Some(row.dim(*d).is_none()),
            Pred::In(d, a, b) => row.dim(*d).map(|v| v == *a || v == *b),
            Pred::Or(a, b) => match (a.eval(row), b.eval(row)) {
                (Some(true), _) | (_, Some(true)) => Some(true),
                (Some(false), Some(false)) => Some(false),
                _ => None,
            },
            Pred::And(a, b) => match (a.eval(row), b.eval(row)) {
                (Some(false), _) | (_, Some(false)) => Some(false),
                (Some(true), Some(true)) => Some(true),
                _ => None,
            },
        }
    }
}

/// One joined row: the index of the row reached on each table, if any.
#[derive(Debug, Clone, Default)]
struct Wide<'a> {
    inst: Option<&'a Instance>,
    h: Option<usize>,
    x: Option<usize>,
    z: Option<usize>,
    y: Option<usize>,
    bt: Option<usize>,
    c: Option<usize>,
}

impl Wide<'_> {
    fn dim(&self, d: usize) -> Cell {
        let inst = self.inst.expect("instance");
        match DIMS[d].0 {
            "hcat" => self.h.and_then(|i| inst.h[i].1),
            "xcat" => self.x.and_then(|i| inst.x[i].1),
            "rcat" => self.bt.and_then(|i| inst.bt[i].2),
            "zcat" => self.z.and_then(|i| inst.z[i]),
            "ycat" => self.y.and_then(|i| inst.y[i]),
            "ccat" => self.c.and_then(|i| inst.c[i].1),
            other => unreachable!("{other} is never oracled"),
        }
    }
}

/// The row of a table with `n` rows whose id is `fk`, if any.
fn lookup(fk: Cell, n: usize) -> Option<usize> {
    fk.and_then(|v| usize::try_from(v).ok()).filter(|i| *i < n)
}

/// Every joined row each row of `home` reaches, grouped by the home row.
fn reach<'a>(inst: &'a Instance, home: &str) -> Vec<Vec<Wide<'a>>> {
    let base = Wide {
        inst: Some(inst),
        ..Wide::default()
    };
    let with_h = |w: &mut Wide, i: Option<usize>| {
        w.h = i;
        w.z = i.and_then(|i| lookup(inst.h[i].0, inst.z.len()));
    };
    let with_x = |w: &mut Wide, i: Option<usize>| {
        w.x = i;
        w.y = i.and_then(|i| lookup(inst.x[i].0, inst.y.len()));
    };
    match home {
        "h" => (0..inst.h.len())
            .map(|i| {
                let mut w = base.clone();
                with_h(&mut w, Some(i));
                let links: Vec<usize> = (0..inst.bt.len())
                    .filter(|b| lookup(inst.bt[*b].0, inst.h.len()) == Some(i))
                    .collect();
                if links.is_empty() {
                    return vec![w];
                }
                links
                    .into_iter()
                    .map(|b| {
                        let mut w = w.clone();
                        w.bt = Some(b);
                        with_x(&mut w, lookup(inst.bt[b].1, inst.x.len()));
                        w
                    })
                    .collect()
            })
            .collect(),
        "x" => (0..inst.x.len())
            .map(|i| {
                let mut w = base.clone();
                with_x(&mut w, Some(i));
                let links: Vec<usize> = (0..inst.bt.len())
                    .filter(|b| lookup(inst.bt[*b].1, inst.x.len()) == Some(i))
                    .collect();
                if links.is_empty() {
                    return vec![w];
                }
                links
                    .into_iter()
                    .map(|b| {
                        let mut w = w.clone();
                        w.bt = Some(b);
                        with_h(&mut w, lookup(inst.bt[b].0, inst.h.len()));
                        w
                    })
                    .collect()
            })
            .collect(),
        "bt" => (0..inst.bt.len())
            .map(|b| {
                let mut w = base.clone();
                w.bt = Some(b);
                with_h(&mut w, lookup(inst.bt[b].0, inst.h.len()));
                with_x(&mut w, lookup(inst.bt[b].1, inst.x.len()));
                vec![w]
            })
            .collect(),
        _ => unreachable!(),
    }
}

/// The value metric `m` aggregates for its `i`th home row (`None` for COUNT).
fn metric_value(inst: &Instance, m: usize, i: usize) -> Cell {
    match METS[m].0 {
        "sum_hv" => inst.h[i].2,
        "sum_xv" => inst.x[i].2,
        "sum_bv" => inst.bt[i].3,
        _ => None,
    }
}

/// Group key -> aggregate, for metric `m`.
fn oracle_metric(
    inst: &Instance,
    m: usize,
    dims: &[usize],
    pred: Option<&Pred>,
) -> BTreeMap<Vec<Cell>, Option<i128>> {
    let (_, home, agg) = METS[m];
    let mut pairs: BTreeSet<(usize, Vec<Cell>)> = BTreeSet::new();
    for (i, rows) in reach(inst, home).into_iter().enumerate() {
        for w in rows {
            if pred.is_some_and(|p| p.eval(&w) != Some(true)) {
                continue;
            }
            pairs.insert((i, dims.iter().map(|d| w.dim(*d)).collect()));
        }
    }
    let mut out: BTreeMap<Vec<Cell>, Option<i128>> = BTreeMap::new();
    if dims.is_empty() {
        out.insert(vec![], if agg.is_some() { None } else { Some(0) });
    }
    for (i, key) in pairs {
        let slot = out
            .entry(key)
            .or_insert(if agg.is_some() { None } else { Some(0) });
        match agg {
            None => *slot = Some(slot.unwrap_or(0) + 1),
            Some(_) => {
                if let Some(v) = metric_value(inst, m, i) {
                    *slot = Some(slot.unwrap_or(0) + i128::from(v));
                }
            }
        }
    }
    out
}

/// The whole expected result, sorted.
fn oracle(inst: &Instance, case: &Case) -> Vec<Vec<Option<i128>>> {
    let per: Vec<BTreeMap<Vec<Cell>, Option<i128>>> = case
        .mets
        .iter()
        .map(|m| oracle_metric(inst, *m, &case.dims, case.pred.as_ref()))
        .collect();
    let keys: BTreeSet<Vec<Cell>> = per.iter().flat_map(|p| p.keys().cloned()).collect();
    let mut rows: Vec<Vec<Option<i128>>> = keys
        .into_iter()
        .map(|key| {
            let mut row: Vec<Option<i128>> = key.iter().map(|v| v.map(i128::from)).collect();
            for p in &per {
                row.push(p.get(&key).copied().flatten());
            }
            row
        })
        .collect();
    rows.sort();
    rows
}

#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    first: &'static str,
    dims: Vec<usize>,
    mets: Vec<usize>,
    pred: Option<Pred>,
}

impl Case {
    fn has_child(&self) -> bool {
        self.first == "h"
    }

    /// Whether the oracle says the query is answerable.
    fn answerable(&self) -> bool {
        let mut members = self.dims.clone();
        if let Some(p) = &self.pred {
            p.dims(&mut members);
        }
        self.mets
            .iter()
            .all(|m| members.iter().all(|d| reachable(METS[*m].1, DIMS[*d].1)))
    }
}

fn cell(n: i64) -> impl Strategy<Value = Cell> {
    prop_oneof![1 => Just(None), 4 => (0..n).prop_map(Some)]
}

/// A foreign key into a table of `n` rows: valid, NULL, or dangling.
fn fk(n: usize) -> impl Strategy<Value = Cell> {
    let n = n as i64;
    prop_oneof![1 => Just(None), 1 => Just(Some(n + 7)), 6 => (0..n.max(1)).prop_map(Some)]
}

fn arb_instance() -> impl Strategy<Value = Instance> {
    (1usize..5, 1usize..5, 1usize..4, 1usize..4).prop_flat_map(|(nh, nx, nz, ny)| {
        (
            prop::collection::vec((fk(nz), cell(3), cell(20)), nh),
            prop::collection::vec((fk(ny), cell(3), cell(20)), nx),
            prop::collection::vec(cell(3), nz),
            prop::collection::vec(cell(3), ny),
            prop::collection::vec((fk(nh), fk(nx), cell(3), cell(20)), 0..9),
            prop::collection::vec((fk(nh), cell(3)), 0..4),
        )
            .prop_map(|(h, x, z, y, bt, c)| Instance { h, x, z, y, bt, c })
    })
}

fn arb_pred(n_dims: usize) -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        (0..n_dims, 0i64..3).prop_map(|(d, k)| Pred::Eq(d, k)),
        (0..n_dims).prop_map(Pred::IsNull),
        (0..n_dims, 0i64..3, 0i64..3).prop_map(|(d, a, b)| Pred::In(d, a, b)),
    ];
    leaf.prop_recursive(2, 4, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
        ]
    })
}

fn arb_case() -> impl Strategy<Value = Case> {
    (
        arb_instance(),
        prop::sample::select(vec!["h", "x", "bt", "z"]),
        prop::sample::subsequence((0..5).collect::<Vec<usize>>(), 1..=3),
        any::<u8>(),
    )
        .prop_flat_map(|(inst, first, mets, salt)| {
            let n_dims = if first == "h" { 6 } else { 5 };
            (
                Just(inst),
                Just(first),
                prop::sample::subsequence((0..n_dims).collect::<Vec<usize>>(), 0..=2),
                Just(mets),
                prop::option::weighted(0.4, arb_pred(n_dims)),
                Just(salt),
            )
        })
        .prop_map(|(inst, first, dims, mets, pred, _)| Case {
            inst,
            first,
            dims,
            mets,
            pred,
        })
}

fn table_ref(alias: &str, pk: &[&str]) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: format!("{alias}_tbl"),
        pk_columns: pk.iter().map(|c| (*c).to_string()).collect(),
        ..Default::default()
    }
}

fn rel(name: &str, from: &str, to: &str, fk_col: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk_col.to_string()],
        ref_columns: vec!["id".to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }
}

fn build_def(case: &Case) -> SemanticViewDefinition {
    let mut tables: Vec<TableRef> = TABLES
        .iter()
        .filter(|t| **t != "c" || case.has_child())
        .map(|t| table_ref(t, if *t == "bt" { &[] } else { &["id"] }))
        .collect();
    let at = tables
        .iter()
        .position(|t| t.alias == case.first)
        .expect("base");
    let base = tables.remove(at);
    tables.insert(0, base);
    let mut joins = vec![
        rel("bt_h", "bt", "h", "hid"),
        rel("bt_x", "bt", "x", "xid"),
        rel("h_z", "h", "z", "fk_z"),
        rel("x_y", "x", "y", "fk_y"),
    ];
    if case.has_child() {
        joins.push(rel("c_h", "c", "h", "hid"));
    }
    let dimensions = DIMS
        .iter()
        .filter(|(_, t, _)| *t != "c" || case.has_child())
        .map(|(name, t, col)| Dimension {
            name: (*name).to_string(),
            expr: format!("{t}.{col}"),
            source_table: Some((*t).to_string()),
            ..Default::default()
        })
        .collect();
    let metrics = METS
        .iter()
        .map(|(name, t, col)| Metric {
            name: (*name).to_string(),
            expr: col.map_or_else(|| "COUNT(*)".to_string(), |c| format!("SUM({t}.{c})")),
            source_table: Some((*t).to_string()),
            ..Default::default()
        })
        .collect();
    SemanticViewDefinition {
        tables,
        dimensions,
        metrics,
        joins,
        ..Default::default()
    }
}

fn sql_cell(c: Cell) -> String {
    c.map_or_else(|| "NULL".to_string(), |v| v.to_string())
}

fn make_db(inst: &Instance) -> duckdb::Connection {
    let con = duckdb::Connection::open_in_memory().expect("in-memory DuckDB");
    con.execute_batch(
        "CREATE TABLE h_tbl (id BIGINT, fk_z BIGINT, hcat BIGINT, hv BIGINT);
         CREATE TABLE x_tbl (id BIGINT, fk_y BIGINT, xcat BIGINT, xv BIGINT);
         CREATE TABLE z_tbl (id BIGINT, zcat BIGINT);
         CREATE TABLE y_tbl (id BIGINT, ycat BIGINT);
         CREATE TABLE bt_tbl (hid BIGINT, xid BIGINT, rcat BIGINT, bv BIGINT);
         CREATE TABLE c_tbl (id BIGINT, hid BIGINT, ccat BIGINT);",
    )
    .expect("schema");
    let mut sql = String::new();
    for (i, (fz, cat, v)) in inst.h.iter().enumerate() {
        sql += &format!(
            "INSERT INTO h_tbl VALUES ({i}, {}, {}, {});",
            sql_cell(*fz),
            sql_cell(*cat),
            sql_cell(*v)
        );
    }
    for (i, (fy, cat, v)) in inst.x.iter().enumerate() {
        sql += &format!(
            "INSERT INTO x_tbl VALUES ({i}, {}, {}, {});",
            sql_cell(*fy),
            sql_cell(*cat),
            sql_cell(*v)
        );
    }
    for (i, cat) in inst.z.iter().enumerate() {
        sql += &format!("INSERT INTO z_tbl VALUES ({i}, {});", sql_cell(*cat));
    }
    for (i, cat) in inst.y.iter().enumerate() {
        sql += &format!("INSERT INTO y_tbl VALUES ({i}, {});", sql_cell(*cat));
    }
    for (hid, xid, cat, v) in &inst.bt {
        sql += &format!(
            "INSERT INTO bt_tbl VALUES ({}, {}, {}, {});",
            sql_cell(*hid),
            sql_cell(*xid),
            sql_cell(*cat),
            sql_cell(*v)
        );
    }
    for (i, (hid, cat)) in inst.c.iter().enumerate() {
        sql += &format!(
            "INSERT INTO c_tbl VALUES ({i}, {}, {});",
            sql_cell(*hid),
            sql_cell(*cat)
        );
    }
    con.execute_batch(&sql).expect("data");
    con
}

fn run(con: &duckdb::Connection, sql: &str) -> Vec<Vec<Option<i128>>> {
    let mut stmt = con
        .prepare(sql)
        .unwrap_or_else(|e| panic!("emitted SQL must bind: {e}\n{sql}"));
    let mut rows: Vec<Vec<Option<i128>>> = stmt
        .query_map([], |r| {
            let n = r.as_ref().column_count();
            (0..n)
                .map(|i| {
                    let v: duckdb::types::Value = r.get(i)?;
                    Ok(match v {
                        duckdb::types::Value::Null => None,
                        duckdb::types::Value::BigInt(x) => Some(i128::from(x)),
                        duckdb::types::Value::HugeInt(x) => Some(x),
                        duckdb::types::Value::Int(x) => Some(i128::from(x)),
                        other => panic!("unexpected value {other:?}"),
                    })
                })
                .collect()
        })
        .expect("query runs")
        .map(Result::unwrap)
        .collect();
    rows.sort();
    rows
}

fn request(case: &Case) -> QueryRequest {
    QueryRequest {
        where_clause: case.pred.as_ref().map(Pred::sql),
        // PIN: FACTS queries are not answered across a bridge (validate_fact_table_path refuses them); this harness oracles metrics and dimensions. TECH-DEBT #66
        facts: vec![],
        dimensions: case
            .dims
            .iter()
            .map(|d| DimensionName::new(DIMS[*d].0))
            .collect(),
        metrics: case
            .mets
            .iter()
            .map(|m| MetricName::new(METS[*m].0))
            .collect(),
    }
}

fn check(case: &Case) -> Result<(), TestCaseError> {
    let def = build_def(case);
    let result = expand("v", &def, &request(case));
    if !case.answerable() {
        prop_assert!(
            result.is_err(),
            "expected a refusal, got SQL:\n{}",
            result.unwrap_or_default()
        );
        return Ok(());
    }
    let sql = result.map_err(|e| TestCaseError::fail(format!("refused: {e}")))?;
    let got = run(&make_db(&case.inst), &sql);
    let want = oracle(&case.inst, case);
    prop_assert_eq!(got, want, "SQL:\n{}", sql);
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn bridge_counts_each_row_once_per_group(case in arb_case()) {
        check(&case)?;
    }
}

/// The tables each table reaches by following foreign keys (itself included),
/// restated from the topology.
fn up(table: &str) -> &'static [&'static str] {
    match table {
        "h" => &["h", "z"],
        "x" => &["x", "y"],
        "bt" => &["bt", "h", "x", "z", "y"],
        "z" => &["z"],
        "y" => &["y"],
        "c" => &["c", "h", "z"],
        other => unreachable!("unexpected table {other}"),
    }
}

/// The table a dimensions-only query reads from: of the tables that reach every
/// member's table, the one no other such table sits above (Snowflake's answer
/// in the 2026-11-09 dims-only fixture). `None` when no single table does, as
/// for a member on `c` beside one across the bridge.
fn dims_only_anchor(members: &[usize]) -> Option<&'static str> {
    let candidates: Vec<&'static str> = TABLES
        .iter()
        .copied()
        .filter(|t| members.iter().all(|d| up(t).contains(&DIMS[*d].1)))
        .collect();
    let closest: Vec<&'static str> = candidates
        .iter()
        .copied()
        .filter(|t| !candidates.iter().any(|o| o != t && up(t).contains(o)))
        .collect();
    match closest.as_slice() {
        [one] => Some(*one),
        _ => None,
    }
}

/// The rows of `anchor`, each with the row it reaches on every table above it.
fn anchor_rows<'a>(inst: &'a Instance, anchor: &str) -> Vec<Wide<'a>> {
    let base = Wide {
        inst: Some(inst),
        ..Wide::default()
    };
    let h_up = |w: &mut Wide, i: Option<usize>| {
        w.h = i;
        w.z = i.and_then(|i| lookup(inst.h[i].0, inst.z.len()));
    };
    let x_up = |w: &mut Wide, i: Option<usize>| {
        w.x = i;
        w.y = i.and_then(|i| lookup(inst.x[i].0, inst.y.len()));
    };
    let one = |f: &dyn Fn(&mut Wide, usize), n: usize| -> Vec<Wide<'a>> {
        (0..n)
            .map(|i| {
                let mut w = base.clone();
                f(&mut w, i);
                w
            })
            .collect()
    };
    match anchor {
        "h" => one(&|w, i| h_up(w, Some(i)), inst.h.len()),
        "x" => one(&|w, i| x_up(w, Some(i)), inst.x.len()),
        "z" => one(&|w, i| w.z = Some(i), inst.z.len()),
        "y" => one(&|w, i| w.y = Some(i), inst.y.len()),
        "bt" => one(
            &|w, b| {
                w.bt = Some(b);
                h_up(w, lookup(inst.bt[b].0, inst.h.len()));
                x_up(w, lookup(inst.bt[b].1, inst.x.len()));
            },
            inst.bt.len(),
        ),
        "c" => one(
            &|w, i| {
                w.c = Some(i);
                h_up(w, lookup(inst.c[i].0, inst.h.len()));
            },
            inst.c.len(),
        ),
        other => unreachable!("unexpected anchor {other}"),
    }
}

/// The distinct dimension tuples of the anchor's rows the predicate is TRUE on.
fn dims_only_oracle(inst: &Instance, case: &Case, anchor: &str) -> Vec<Vec<Option<i128>>> {
    let rows: BTreeSet<Vec<Option<i128>>> = anchor_rows(inst, anchor)
        .into_iter()
        .filter(|w| case.pred.as_ref().is_none_or(|p| p.eval(w) == Some(true)))
        .map(|w| {
            case.dims
                .iter()
                .map(|d| w.dim(*d).map(i128::from))
                .collect()
        })
        .collect();
    rows.into_iter().collect()
}

fn arb_dims_only_case() -> impl Strategy<Value = Case> {
    (
        arb_instance(),
        prop::sample::select(vec!["h", "x", "bt", "z"]),
    )
        .prop_flat_map(|(inst, first)| {
            let n_dims = if first == "h" { 6 } else { 5 };
            (
                Just(inst),
                Just(first),
                prop::sample::subsequence((0..n_dims).collect::<Vec<usize>>(), 1..=3),
                prop::option::weighted(0.4, arb_pred(n_dims)),
            )
        })
        .prop_map(|(inst, first, dims, pred)| Case {
            inst,
            first,
            dims,
            mets: vec![],
            pred,
        })
}

/// The members a dims-only case names: its dimensions and its filter's.
fn dims_only_members(case: &Case) -> Vec<usize> {
    let mut members = case.dims.clone();
    if let Some(p) = &case.pred {
        p.dims(&mut members);
    }
    members
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// A dimensions-only query lists the rows of the table under all its
    /// members, joined up from there, whichever table is declared first: one
    /// table's own values, a child's rows beside its parent, the bridge's rows
    /// across it. Dangling and NULL foreign keys give NULL parent columns.
    /// Members no single table is under are refused.
    #[test]
    fn dims_only_lists_the_rows_of_the_table_under_its_members(case in arb_dims_only_case()) {
        let members = dims_only_members(&case);
        let def = build_def(&case);
        // A member on `c` beside one across the bridge has no table under both:
        // Snowflake refuses it, the two "are not related".
        let Some(anchor) = dims_only_anchor(&members) else {
            let err = expand("v", &def, &request(&case))
                .err()
                .ok_or_else(|| TestCaseError::fail("no table under every member, yet answered"))?;
            prop_assert!(err.to_string().contains("are not related"), "{}", err);
            return Ok(());
        };
        let sql = expand("v", &def, &request(&case))
            .map_err(|e| TestCaseError::fail(format!("refused: {e}")))?;
        let got = run(&make_db(&case.inst), &sql);
        let want = dims_only_oracle(&case.inst, &case, anchor);
        prop_assert_eq!(got, want, "anchor {}, SQL:\n{}", anchor, sql);
    }
}

/// The dims-only generator reaches every anchor, a filter, most declared
/// orders for an anchor that is not the base table, and members no table is
/// under.
#[test]
fn dims_only_generator_reaches_every_anchor() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let mut anchors: BTreeSet<&str> = BTreeSet::new();
    let mut off_base = 0;
    let mut filtered = 0;
    let mut unrelated = 0;
    for _ in 0..2000 {
        let case = arb_dims_only_case()
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let Some(anchor) = dims_only_anchor(&dims_only_members(&case)) else {
            unrelated += 1;
            continue;
        };
        anchors.insert(anchor);
        if anchor != case.first {
            off_base += 1;
        }
        if case.pred.is_some() {
            filtered += 1;
        }
    }
    assert_eq!(anchors.len(), TABLES.len(), "{anchors:?}");
    assert!(
        off_base > 500 && filtered > 300 && unrelated > 50,
        "{off_base} {filtered} {unrelated}"
    );
}

/// The fixture's B1 in this harness's terms, pinned so a regression names the
/// shape: `h` rows 0 and 1 share the `x` category 1 through two bridge rows
/// each, and `h` row 2 has no bridge row.
#[test]
fn pinned_once_per_group() {
    let inst = Instance {
        h: vec![
            (None, Some(0), Some(100)),
            (None, Some(0), Some(200)),
            (None, Some(1), Some(50)),
        ],
        x: vec![
            (None, Some(1), None),
            (None, Some(1), None),
            (None, Some(2), None),
        ],
        z: vec![None],
        y: vec![None],
        bt: vec![
            (Some(0), Some(0), Some(0), None),
            (Some(0), Some(1), Some(1), None),
            (Some(1), Some(0), Some(0), None),
            (Some(1), Some(2), Some(1), None),
        ],
        c: vec![],
    };
    let case = Case {
        inst,
        first: "x",
        dims: vec![1],
        mets: vec![0, 1],
        pred: None,
    };
    assert_eq!(
        oracle(&case.inst, &case),
        vec![
            vec![None, Some(50), Some(1)],
            vec![Some(1), Some(300), Some(2)],
            vec![Some(2), Some(200), Some(1)],
        ]
    );
    check(&case).unwrap();
}

/// The generator reaches what the properties need: every base table, bridged
/// and refused queries, and a filter across the bridge.
#[test]
fn generator_reaches_bridged_refused_and_filtered_cases() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let (mut bridged, mut refused, mut filtered) = (false, false, false);
    let mut firsts: BTreeSet<&str> = BTreeSet::new();
    for _ in 0..2000 {
        let case = arb_case().new_tree(&mut runner).unwrap().current();
        firsts.insert(case.first);
        let crosses = |m: usize, d: usize| {
            matches!(
                (METS[m].1, DIMS[d].1),
                ("h", "x" | "bt" | "y") | ("x", "h" | "bt" | "z")
            )
        };
        if case.answerable()
            && case
                .mets
                .iter()
                .any(|m| case.dims.iter().any(|d| crosses(*m, *d)))
        {
            bridged = true;
        }
        if !case.answerable() {
            refused = true;
        }
        if let Some(p) = &case.pred {
            let mut ds = Vec::new();
            p.dims(&mut ds);
            if case.answerable() && case.mets.iter().any(|m| ds.iter().any(|d| crosses(*m, *d))) {
                filtered = true;
            }
        }
    }
    assert!(
        bridged && refused && filtered,
        "{bridged} {refused} {filtered}"
    );
    assert_eq!(firsts.len(), 4, "{firsts:?}");
}
