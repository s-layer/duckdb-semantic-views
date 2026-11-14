//! Differential proptest for **direct many-to-many relationships** (Phase 8.4).
//!
//! Shape: `o(code) REFERENCES p(code)`, where `code` is no key of `p`, so each
//! `o` row matches every `p` row with its code (Snowflake's direct
//! many-to-many relationship). Each side has a many-to-one parent: `o.fk_c ->
//! c.id` and `p.fk_r -> r.id`. The base table is drawn from all four, and `p`
//! has a PRIMARY KEY (`pid`) in about half the cases, `o` (`id`) in about three
//! quarters.
//!
//! The data holds NULL codes (which match nothing), codes no `p` row has, `p`
//! rows with a code no `o` row has, several `p` rows per code (duplicates
//! included), NULL and dangling foreign keys, and NULL values.
//!
//! Three kinds of query are drawn, each checked for acceptance and, when
//! accepted, against an oracle computed in Rust from the rows, never in SQL.
//! The rules are Snowflake's documented ones (fixture 11, part M):
//!
//! 1. **Metrics.** A metric's table rows are counted once per group: for each
//!    row of its table, the oracle enumerates the rows it reaches across the
//!    relationship (one row with NULLs past a step that reaches none), keeps
//!    those the predicate is TRUE on, and takes the distinct groups. A metric
//!    on `p` grouped or filtered across the relationship needs `p`'s key, and
//!    is refused without it (010933).
//! 2. **Dimensions only, across the relationship.** The distinct combinations
//!    that occur together: every matching `(o, p)` pair, with `c` and `r`
//!    inner-joined when a member needs them.
//! 3. **Facts only, across the relationship.** One row per matching
//!    combination, not deduplicated; but when the far side is only in the
//!    `WHERE` clause, one row per row of the queried table that passes the
//!    filter (which needs that table's key).

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, FactName, MetricName, QueryRequest};
use semantic_views::model::{
    Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};

type Cell = Option<i64>;

#[derive(Debug, Clone)]
struct Instance {
    /// `(code, fk_c, ocat, ov)`; row `i` has id `i`.
    o: Vec<(Cell, Cell, Cell, Cell)>,
    /// `(code, fk_r, pcat, pv)`; row `i` has pid `i`.
    p: Vec<(Cell, Cell, Cell, Cell)>,
    /// `ccat`.
    c: Vec<Cell>,
    /// `rcat`.
    r: Vec<Cell>,
}

const TABLES: [&str; 4] = ["o", "p", "c", "r"];
/// `(dimension name, table, column)`.
const DIMS: [(&str, &str, &str); 4] = [
    ("ocat", "o", "ocat"),
    ("pcat", "p", "pcat"),
    ("ccat", "c", "ccat"),
    ("rcat", "r", "rcat"),
];
/// `(metric name, table, expression)`.
const METS: [(&str, &str, &str); 4] = [
    ("sum_ov", "o", "SUM(o.ov)"),
    ("cnt_o", "o", "COUNT(*)"),
    ("sum_pv", "p", "SUM(p.pv)"),
    ("cnt_pv", "p", "COUNT(p.pv)"),
];
/// `(fact name, table, column)`.
const FACTS: [(&str, &str, &str); 2] = [("ov", "o", "ov"), ("pv", "p", "pv")];

/// The side of the relationship a table is on: `o`'s or `p`'s.
fn near_o(table: &str) -> bool {
    matches!(table, "o" | "c")
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

    fn tables(&self, out: &mut BTreeSet<&'static str>) {
        match self {
            Pred::Eq(d, _) | Pred::IsNull(d) | Pred::In(d, _, _) => {
                out.insert(DIMS[*d].1);
            }
            Pred::Or(a, b) | Pred::And(a, b) => {
                a.tables(out);
                b.tables(out);
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
#[derive(Debug, Clone, Copy)]
struct Wide<'a> {
    inst: &'a Instance,
    o: Option<usize>,
    p: Option<usize>,
    c: Option<usize>,
    r: Option<usize>,
}

impl Wide<'_> {
    fn row(&self, table: &str) -> Option<usize> {
        match table {
            "o" => self.o,
            "p" => self.p,
            "c" => self.c,
            "r" => self.r,
            other => unreachable!("{other}"),
        }
    }

    fn dim(&self, d: usize) -> Cell {
        let inst = self.inst;
        match DIMS[d].0 {
            "ocat" => self.o.and_then(|i| inst.o[i].2),
            "pcat" => self.p.and_then(|i| inst.p[i].2),
            "ccat" => self.c.and_then(|i| inst.c[i]),
            "rcat" => self.r.and_then(|i| inst.r[i]),
            other => unreachable!("{other}"),
        }
    }

    fn fact(&self, f: usize) -> Cell {
        match FACTS[f].0 {
            "ov" => self.o.and_then(|i| self.inst.o[i].3),
            "pv" => self.p.and_then(|i| self.inst.p[i].3),
            other => unreachable!("{other}"),
        }
    }
}

/// The row of a table with `n` rows whose id is `fk`, if any.
fn lookup(fk: Cell, n: usize) -> Option<usize> {
    fk.and_then(|v| usize::try_from(v).ok()).filter(|i| *i < n)
}

/// Whether `o` row `i` and `p` row `j` match: equal codes, neither NULL.
fn matches(inst: &Instance, i: usize, j: usize) -> bool {
    inst.o[i].0.is_some() && inst.o[i].0 == inst.p[j].0
}

fn wide(inst: &Instance, o: Option<usize>, p: Option<usize>) -> Wide<'_> {
    Wide {
        inst,
        o,
        p,
        c: o.and_then(|i| lookup(inst.o[i].1, inst.c.len())),
        r: p.and_then(|j| lookup(inst.p[j].1, inst.r.len())),
    }
}

/// Every joined row each row of `home` (`o` or `p`) reaches across the
/// relationship, grouped by the home row: one with NULLs past it when it
/// matches nothing.
fn reach<'a>(inst: &'a Instance, home: &str) -> Vec<Vec<Wide<'a>>> {
    let (n_home, n_far) = if home == "o" {
        (inst.o.len(), inst.p.len())
    } else {
        (inst.p.len(), inst.o.len())
    };
    (0..n_home)
        .map(|h| {
            let pair = |f: Option<usize>| {
                if home == "o" {
                    wide(inst, Some(h), f)
                } else {
                    wide(inst, f, Some(h))
                }
            };
            let far: Vec<Wide> = (0..n_far)
                .filter(|f| {
                    if home == "o" {
                        matches(inst, h, *f)
                    } else {
                        matches(inst, *f, h)
                    }
                })
                .map(|f| pair(Some(f)))
                .collect();
            if far.is_empty() {
                vec![pair(None)]
            } else {
                far
            }
        })
        .collect()
}

/// Every matching `(o, p)` pair, with `c` and `r` reached, keeping only those
/// that reach a row on each of `needed`: every join inner.
fn combinations<'a>(inst: &'a Instance, needed: &BTreeSet<&str>) -> Vec<Wide<'a>> {
    let mut out = Vec::new();
    for i in 0..inst.o.len() {
        for j in 0..inst.p.len() {
            if !matches(inst, i, j) {
                continue;
            }
            let w = wide(inst, Some(i), Some(j));
            if needed.iter().all(|t| w.row(t).is_some()) {
                out.push(w);
            }
        }
    }
    out
}

#[derive(Debug, Clone)]
enum Kind {
    /// Dimensions (indexes into `DIMS`) and metrics (into `METS`).
    Metrics(Vec<usize>, Vec<usize>),
    /// Dimensions only.
    DimsOnly(Vec<usize>),
    /// A FACTS list: `Ok` a fact (into `FACTS`), `Err` a dimension (into
    /// `DIMS`).
    Facts(Vec<Result<usize, usize>>),
}

#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    first: &'static str,
    /// Whether `o` declares its PRIMARY KEY (`p` holds none either way).
    o_keyed: bool,
    p_keyed: bool,
    kind: Kind,
    pred: Option<Pred>,
}

impl Case {
    fn pred_tables(&self) -> BTreeSet<&'static str> {
        let mut out = BTreeSet::new();
        if let Some(p) = &self.pred {
            p.tables(&mut out);
        }
        out
    }

    /// The tables the selected members are on.
    fn selected_tables(&self) -> BTreeSet<&'static str> {
        match &self.kind {
            Kind::Metrics(dims, _) | Kind::DimsOnly(dims) => {
                dims.iter().map(|d| DIMS[*d].1).collect()
            }
            Kind::Facts(items) => items
                .iter()
                .map(|i| match i {
                    Ok(f) => FACTS[*f].1,
                    Err(d) => DIMS[*d].1,
                })
                .collect(),
        }
    }

    /// Whether the members of a query with no metrics are on both sides.
    fn across(&self) -> bool {
        let all: BTreeSet<&str> = self
            .selected_tables()
            .union(&self.pred_tables())
            .copied()
            .collect();
        all.iter().any(|t| near_o(t)) && all.iter().any(|t| !near_o(t))
    }

    /// For a FACTS query across the relationship whose far side is only in
    /// the predicate: the queried table it returns one row per row of.
    fn semi_join_grain(&self) -> Option<&'static str> {
        let selected = self.selected_tables();
        let pred = self.pred_tables();
        let one_side = selected.iter().all(|t| near_o(t)) || selected.iter().all(|t| !near_o(t));
        if !one_side || pred.is_empty() {
            return None;
        }
        let near = selected.iter().next().is_some_and(|t| near_o(t));
        let (low, high) = if near { ("o", "c") } else { ("p", "r") };
        Some(if selected.contains(low) { low } else { high })
    }

    fn answerable(&self) -> bool {
        match &self.kind {
            Kind::Metrics(dims, mets) => {
                let mut members: BTreeSet<&str> = dims.iter().map(|d| DIMS[*d].1).collect();
                members.extend(self.pred_tables());
                mets.iter().all(|m| {
                    // A metric grouped or filtered across the relationship
                    // counts its table's rows by their key.
                    let home = METS[*m].1;
                    if near_o(home) {
                        self.o_keyed || members.iter().all(|t| near_o(t))
                    } else {
                        self.p_keyed || members.iter().all(|t| !near_o(t))
                    }
                })
            }
            Kind::DimsOnly(_) => true,
            Kind::Facts(_) => match self.semi_join_grain() {
                Some("o") => self.o_keyed,
                Some("p") => self.p_keyed,
                _ => true,
            },
        }
    }
}

fn metric_value(inst: &Instance, m: usize, w: &Wide) -> Option<i128> {
    let v = match METS[m].0 {
        "sum_ov" | "cnt_o" => w.o.and_then(|i| inst.o[i].3),
        _ => w.p.and_then(|j| inst.p[j].3),
    };
    match METS[m].0 {
        "cnt_o" => Some(1),
        "cnt_pv" => Some(i128::from(v.is_some())),
        _ => v.map(i128::from),
    }
}

/// Group key -> aggregate, for metric `m`.
fn oracle_metric(
    inst: &Instance,
    m: usize,
    dims: &[usize],
    pred: Option<&Pred>,
) -> BTreeMap<Vec<Cell>, Option<i128>> {
    let (name, home, _) = METS[m];
    let counts = name.starts_with("cnt");
    let mut pairs: BTreeMap<(usize, Vec<Cell>), Wide> = BTreeMap::new();
    for (h, rows) in reach(inst, home).into_iter().enumerate() {
        for w in rows {
            if pred.is_some_and(|p| p.eval(&w) != Some(true)) {
                continue;
            }
            pairs.insert((h, dims.iter().map(|d| w.dim(*d)).collect()), w);
        }
    }
    let empty = if counts { Some(0) } else { None };
    let mut out: BTreeMap<Vec<Cell>, Option<i128>> = BTreeMap::new();
    if dims.is_empty() {
        out.insert(vec![], empty);
    }
    for ((_, key), w) in pairs {
        let slot = out.entry(key).or_insert(empty);
        if let Some(v) = metric_value(inst, m, &w) {
            *slot = Some(slot.unwrap_or(0) + v);
        }
    }
    out
}

fn to_row(cells: impl Iterator<Item = Cell>) -> Vec<Option<i128>> {
    cells.map(|v| v.map(i128::from)).collect()
}

/// The whole expected result, sorted.
fn oracle(case: &Case) -> Vec<Vec<Option<i128>>> {
    let inst = &case.inst;
    let pred = case.pred.as_ref();
    let passes = |w: &Wide| pred.is_none_or(|p| p.eval(w) == Some(true));
    let needed: BTreeSet<&str> = case
        .selected_tables()
        .union(&case.pred_tables())
        .copied()
        .collect();
    let mut rows: Vec<Vec<Option<i128>>> = match &case.kind {
        Kind::Metrics(dims, mets) => {
            let per: Vec<BTreeMap<Vec<Cell>, Option<i128>>> = mets
                .iter()
                .map(|m| oracle_metric(inst, *m, dims, pred))
                .collect();
            let keys: BTreeSet<Vec<Cell>> = per.iter().flat_map(|p| p.keys().cloned()).collect();
            keys.into_iter()
                .map(|key| {
                    let mut row = to_row(key.iter().copied());
                    for p in &per {
                        row.push(p.get(&key).copied().flatten());
                    }
                    row
                })
                .collect()
        }
        Kind::DimsOnly(dims) => {
            let distinct: BTreeSet<Vec<Option<i128>>> = combinations(inst, &needed)
                .iter()
                .filter(|w| passes(w))
                .map(|w| to_row(dims.iter().map(|d| w.dim(*d))))
                .collect();
            distinct.into_iter().collect()
        }
        Kind::Facts(items) => {
            let values = |w: &Wide| {
                to_row(items.iter().map(|i| match i {
                    Ok(f) => w.fact(*f),
                    Err(d) => w.dim(*d),
                }))
            };
            let combos: Vec<Wide> = combinations(inst, &needed)
                .into_iter()
                .filter(|w| passes(w))
                .collect();
            match case.semi_join_grain() {
                Some(grain) => {
                    let once: BTreeMap<usize, Vec<Option<i128>>> = combos
                        .iter()
                        .filter_map(|w| w.row(grain).map(|g| (g, values(w))))
                        .collect();
                    once.into_values().collect()
                }
                None => combos.iter().map(values).collect(),
            }
        }
    };
    rows.sort();
    rows
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
    (1usize..4, 1usize..4).prop_flat_map(|(nc, nr)| {
        (
            prop::collection::vec((cell(4), fk(nc), cell(3), cell(20)), 1..6),
            prop::collection::vec((cell(4), fk(nr), cell(3), cell(20)), 0..7),
            prop::collection::vec(cell(3), nc),
            prop::collection::vec(cell(3), nr),
        )
            .prop_map(|(o, p, c, r)| Instance { o, p, c, r })
    })
}

fn arb_pred() -> impl Strategy<Value = Pred> {
    let n = DIMS.len();
    let leaf = prop_oneof![
        (0..n, 0i64..3).prop_map(|(d, k)| Pred::Eq(d, k)),
        (0..n).prop_map(Pred::IsNull),
        (0..n, 0i64..3, 0i64..3).prop_map(|(d, a, b)| Pred::In(d, a, b)),
    ];
    leaf.prop_recursive(2, 4, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
        ]
    })
}

fn arb_kind() -> impl Strategy<Value = Kind> {
    let dims = || prop::sample::subsequence((0..DIMS.len()).collect::<Vec<usize>>(), 0..=2);
    let item = prop_oneof![(0..FACTS.len()).prop_map(Ok), (0..DIMS.len()).prop_map(Err)];
    prop_oneof![
        3 => (
            dims(),
            prop::sample::subsequence((0..METS.len()).collect::<Vec<usize>>(), 1..=2),
        )
            .prop_map(|(d, m)| Kind::Metrics(d, m)),
        2 => prop::sample::subsequence((0..DIMS.len()).collect::<Vec<usize>>(), 1..=3)
            .prop_map(Kind::DimsOnly),
        2 => prop::collection::vec(item, 1..=3)
            .prop_filter("distinct items", |v| {
                v.iter().collect::<BTreeSet<_>>().len() == v.len()
            })
            .prop_map(Kind::Facts),
    ]
}

fn arb_case() -> impl Strategy<Value = Case> {
    // A FACTS list on one side, filtered across the relationship, is the
    // semi-join: give the FACTS lists a filter more often.
    let kind_and_pred = arb_kind().prop_flat_map(|kind| {
        let p = if matches!(kind, Kind::Facts(_)) {
            0.9
        } else {
            0.5
        };
        (Just(kind), prop::option::weighted(p, arb_pred()))
    });
    (
        arb_instance(),
        prop::sample::select(TABLES.to_vec()),
        (prop::bool::weighted(0.75), any::<bool>()),
        kind_and_pred,
    )
        .prop_map(|(inst, first, (o_keyed, p_keyed), (kind, pred))| Case {
            inst,
            first,
            o_keyed,
            p_keyed,
            kind,
            pred,
        })
        // A query with no metrics is oracled here only across the
        // relationship; the other harnesses oracle it on one side.
        .prop_filter("across the relationship", |c| {
            matches!(c.kind, Kind::Metrics(..)) || c.across()
        })
}

/// A FACTS list on one side, filtered by a predicate on the other: the
/// semi-join, which [`arb_case`] draws too rarely to rely on.
fn arb_semi_join_case() -> impl Strategy<Value = Case> {
    let near_items: Vec<Result<usize, usize>> = vec![Ok(0), Err(0), Err(2)];
    let far_items: Vec<Result<usize, usize>> = vec![Ok(1), Err(1), Err(3)];
    (
        arb_instance(),
        prop::sample::select(TABLES.to_vec()),
        (prop::bool::weighted(0.75), any::<bool>()),
        any::<bool>(),
        prop::sample::subsequence(vec![0usize, 1, 2], 1..=2),
        0i64..3,
        any::<bool>(),
    )
        .prop_map(
            move |(inst, first, (o_keyed, p_keyed), near, picks, k, is_null)| {
                let (items, far_dims) = if near {
                    (&near_items, [1usize, 3])
                } else {
                    (&far_items, [0usize, 2])
                };
                let d = far_dims[usize::try_from(k).unwrap_or(0) % 2];
                Case {
                    inst,
                    first,
                    o_keyed,
                    p_keyed,
                    kind: Kind::Facts(picks.iter().map(|i| items[*i]).collect()),
                    pred: Some(if is_null {
                        Pred::IsNull(d)
                    } else {
                        Pred::In(d, k, (k + 1) % 3)
                    }),
                }
            },
        )
}

fn table_ref(alias: &str, pk: &[&str]) -> TableRef {
    TableRef {
        alias: alias.to_string(),
        table: format!("{alias}_tbl"),
        pk_columns: pk.iter().map(|c| (*c).to_string()).collect(),
        ..Default::default()
    }
}

fn rel(name: &str, from: &str, to: &str, fk_col: &str, ref_col: &str) -> Join {
    Join {
        table: to.to_string(),
        from_alias: from.to_string(),
        fk_columns: vec![fk_col.to_string()],
        ref_columns: vec![ref_col.to_string()],
        name: Some(name.to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }
}

/// The view as CREATE stores it: `o_p`'s cardinality inferred many-to-many.
fn build_def(case: &Case) -> SemanticViewDefinition {
    let mut tables: Vec<TableRef> = TABLES
        .iter()
        .map(|t| {
            let pk: &[&str] = match *t {
                "p" if case.p_keyed => &["pid"],
                "o" if !case.o_keyed => &[],
                "p" => &[],
                _ => &["id"],
            };
            table_ref(t, pk)
        })
        .collect();
    let at = tables
        .iter()
        .position(|t| t.alias == case.first)
        .expect("base");
    let base = tables.remove(at);
    tables.insert(0, base);
    let mut o_p = rel("o_p", "o", "p", "code", "code");
    o_p.cardinality = Cardinality::ManyToMany;
    let joins = vec![
        o_p,
        rel("o_c", "o", "c", "fk_c", "id"),
        rel("p_r", "p", "r", "fk_r", "id"),
    ];
    SemanticViewDefinition {
        tables,
        dimensions: DIMS
            .iter()
            .map(|(name, t, col)| Dimension {
                name: (*name).to_string(),
                expr: format!("{t}.{col}"),
                source_table: Some((*t).to_string()),
                ..Default::default()
            })
            .collect(),
        metrics: METS
            .iter()
            .map(|(name, t, expr)| Metric {
                name: (*name).to_string(),
                expr: (*expr).to_string(),
                source_table: Some((*t).to_string()),
                ..Default::default()
            })
            .collect(),
        facts: FACTS
            .iter()
            .map(|(name, t, col)| Fact {
                name: (*name).to_string(),
                expr: format!("{t}.{col}"),
                source_table: Some((*t).to_string()),
                ..Default::default()
            })
            .collect(),
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
        "CREATE TABLE o_tbl (id BIGINT, code BIGINT, fk_c BIGINT, ocat BIGINT, ov BIGINT);
         CREATE TABLE p_tbl (pid BIGINT, code BIGINT, fk_r BIGINT, pcat BIGINT, pv BIGINT);
         CREATE TABLE c_tbl (id BIGINT, ccat BIGINT);
         CREATE TABLE r_tbl (id BIGINT, rcat BIGINT);",
    )
    .expect("schema");
    let mut sql = String::new();
    for (i, (code, f, cat, v)) in inst.o.iter().enumerate() {
        let cells = [*code, *f, *cat, *v].map(sql_cell).join(", ");
        sql += &format!("INSERT INTO o_tbl VALUES ({i}, {cells});");
    }
    for (i, (code, f, cat, v)) in inst.p.iter().enumerate() {
        let cells = [*code, *f, *cat, *v].map(sql_cell).join(", ");
        sql += &format!("INSERT INTO p_tbl VALUES ({i}, {cells});");
    }
    for (i, cat) in inst.c.iter().enumerate() {
        sql += &format!("INSERT INTO c_tbl VALUES ({i}, {});", sql_cell(*cat));
    }
    for (i, cat) in inst.r.iter().enumerate() {
        sql += &format!("INSERT INTO r_tbl VALUES ({i}, {});", sql_cell(*cat));
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
    let dims = |d: &[usize]| d.iter().map(|d| DimensionName::new(DIMS[*d].0)).collect();
    let mut req = QueryRequest {
        where_clause: case.pred.as_ref().map(Pred::sql),
        ..QueryRequest::default()
    };
    match &case.kind {
        Kind::Metrics(d, m) => {
            req.dimensions = dims(d);
            req.metrics = m.iter().map(|m| MetricName::new(METS[*m].0)).collect();
        }
        Kind::DimsOnly(d) => req.dimensions = dims(d),
        Kind::Facts(items) => {
            req.facts = items
                .iter()
                .map(|i| match i {
                    Ok(f) => FactName::new(FACTS[*f].0),
                    Err(d) => FactName::new(DIMS[*d].0),
                })
                .collect();
        }
    }
    req
}

/// What a case did: refused, or answered with this many rows.
fn check(case: &Case) -> Result<Option<usize>, TestCaseError> {
    let def = build_def(case);
    let result = expand("v", &def, &request(case));
    if !case.answerable() {
        prop_assert!(
            result.is_err(),
            "expected a refusal, got SQL:\n{}",
            result.unwrap_or_default()
        );
        return Ok(None);
    }
    let sql = result.map_err(|e| TestCaseError::fail(format!("refused: {e}")))?;
    let got = run(&make_db(&case.inst), &sql);
    let want = oracle(case);
    prop_assert_eq!(&got, &want, "SQL:\n{}", sql);
    Ok(Some(got.len()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn many_to_many_matches_snowflakes_rules(case in arb_case()) {
        check(&case)?;
    }
    #[test]
    fn a_filter_across_returns_each_row_once(case in arb_semi_join_case()) {
        prop_assert!(case.semi_join_grain().is_some());
        check(&case)?;
    }
}

/// The generator reaches every kind of query, answered with rows, the
/// semi-join and the refusal, over a fixed-seed sample: so the property above
/// is not vacuous.
#[test]
fn every_kind_is_reached() {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::{Config, TestRunner};
    let mut runner = TestRunner::new_with_rng(
        Config::default(),
        proptest::test_runner::TestRng::deterministic_rng(
            proptest::test_runner::RngAlgorithm::ChaCha,
        ),
    );
    let (strategy, semi) = (arb_case(), arb_semi_join_case());
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for n in 0..800 {
        let case = if n % 4 == 0 {
            semi.new_tree(&mut runner).expect("case").current()
        } else {
            strategy.new_tree(&mut runner).expect("case").current()
        };
        let outcome = check(&case).expect("the property holds");
        let crossing_metric = matches!(&case.kind, Kind::Metrics(d, _)
            if d.iter().any(|d| !near_o(DIMS[*d].1)));
        let label = match (&case.kind, outcome) {
            (_, None) => "refused",
            (_, Some(0)) => "empty",
            (Kind::Metrics(..), Some(_)) if crossing_metric => "metric across",
            (Kind::Metrics(..), Some(_)) => "metric",
            (Kind::DimsOnly(_), Some(_)) => "dims only",
            (Kind::Facts(_), Some(_)) if case.semi_join_grain().is_some() => "facts semi-join",
            (Kind::Facts(_), Some(_)) => "facts",
        };
        *seen.entry(label).or_default() += 1;
    }
    for label in [
        "refused",
        "metric across",
        "dims only",
        "facts semi-join",
        "facts",
    ] {
        assert!(
            seen.get(label).copied().unwrap_or(0) >= 10,
            "{label} reached too rarely: {seen:?}"
        );
    }
}
