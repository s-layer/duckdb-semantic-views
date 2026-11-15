//! Differential proptest for **LOD expressions**: `FIXED DIMENSIONS` (Phase
//! 8.5a) and `INCLUDE` / `EXCLUDE DIMENSIONS` (Phase 8.5b).
//!
//! Shape: `o(fk_c) REFERENCES c(id)`, many-to-one. `o` holds the value `ov`
//! and the dimension `ocat`, `c` the dimensions `ccat` and `cid` (its id).
//! The data holds NULL and dangling foreign keys, customers with no orders,
//! and NULL values.
//!
//! Each case draws two LOD expressions on `o`: an aggregate of `o.ov` (SUM,
//! COUNT, MIN or MAX) fixed on any subset of the three dimensions. One is the
//! LOD fact `lf`, read by the metrics `m_lod` (an outer aggregate of `lf`),
//! `m_inl` (the same outer aggregate of the same expression written inline)
//! and `m_mix` (`SUM(o.ov) - MAX(lf)`). The other is the LOD dimension `ld`,
//! which queries group and filter by. `m_two` writes both inline in one
//! metric, `MAX(<lf>) - MIN(<ld>)`.
//!
//! The oracle is computed in Rust from the rows, never in SQL. Snowflake's
//! documented rules (fixture 11):
//!
//! - An LOD expression's value for an `o` row is its aggregate over every `o`
//!   row with the same fixed-dimension values, the parents left-joined. It is
//!   computed before the query's filter.
//! - A metric over an LOD fact counts each fixed-grain value once per group:
//!   the value of a fixed-dimension tuple reaches the groups of the `o` rows
//!   that have the tuple and pass the filter.
//! - A NULL fixed-dimension value is one value, as `GROUP BY` makes it: the
//!   orders with no customer share one value. Snowflake gives those NULL; the
//!   divergence is documented in `docs/how-to/lod-expressions.rst`.
//!
//! A third LOD expression, the fact `lie`, is an aggregate of `o.ov` with
//! `INCLUDE` or `EXCLUDE DIMENSIONS` on any subset of the three dimensions,
//! read by `m_ie` (an outer aggregate of `lie`), `m_ie_inl` (the same written
//! inline) and `m_ie_mix` (`SUM(o.ov) - MAX(lie)`). Its grain is the query's
//! dimensions with its own (`INCLUDE`) or without them (`EXCLUDE`), and it is
//! computed over the orders the filter keeps (fixture 11 L5-L11, 11b E1-E6):
//!
//! - an `INCLUDE` tuple's value counts once per group of the orders it
//!   covers, as a `FIXED` one does;
//! - an `EXCLUDE` value is that of the group's coarser tuple, for every group
//!   the query answers: the groups of its other metrics and of the orders,
//!   and those a query of its dimensions and filter with no metrics lists
//!   whose coarser tuple has orders (11b E5: a customer with no orders shows
//!   the region's total, and the orders with no customer row their NULL
//!   region's);
//! - a query grouped or filtered by `ld` is refused beside one.
//!
//! Queries are metrics (grouped by any of the dimensions and `ld`),
//! dimensions only, and facts (`lf` alone, one row per fixed-grain value; or
//! `lf` beside other items, one row per order).

use std::collections::{BTreeMap, BTreeSet};

use proptest::prelude::*;
use semantic_views::expand::{expand, DimensionName, FactName, MetricName, QueryRequest};
use semantic_views::model::{
    Cardinality, Dimension, Fact, Join, Metric, SemanticViewDefinition, TableRef,
};

type Cell = Option<i64>;

#[derive(Debug, Clone)]
struct Instance {
    /// `(fk_c, ocat, ov)`; row `i` has id `i`.
    o: Vec<(Cell, Cell, Cell)>,
    /// `ccat`; row `i` has id `i`.
    c: Vec<Cell>,
}

/// The plain dimensions: `(name, table, column)`. `ld` is index 3.
const DIMS: [(&str, &str, &str); 3] = [
    ("ocat", "o", "ocat"),
    ("ccat", "c", "ccat"),
    ("cid", "c", "id"),
];
const LD: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agg {
    Sum,
    Count,
    Min,
    Max,
}

impl Agg {
    fn sql(self) -> &'static str {
        match self {
            Agg::Sum => "SUM",
            Agg::Count => "COUNT",
            Agg::Min => "MIN",
            Agg::Max => "MAX",
        }
    }

    /// The aggregate of `values`, as SQL computes it (NULLs skipped).
    fn apply(self, values: impl Iterator<Item = Option<i128>>) -> Option<i128> {
        let present: Vec<i128> = values.flatten().collect();
        match self {
            Agg::Count => Some(present.len() as i128),
            _ if present.is_empty() => None,
            Agg::Sum => Some(present.iter().sum()),
            Agg::Min => present.iter().min().copied(),
            Agg::Max => present.iter().max().copied(),
        }
    }
}

/// An LOD expression: `<agg>(o.ov) <kind> DIMENSIONS (<dims>)`.
#[derive(Debug, Clone)]
struct Lod {
    agg: Agg,
    fixed: Vec<usize>,
    kind: LodKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LodKind {
    Fixed,
    Include,
    Exclude,
}

impl Lod {
    fn sql(&self) -> String {
        let dims: Vec<String> = self
            .fixed
            .iter()
            .map(|d| format!("{}.{}", DIMS[*d].1, DIMS[*d].0))
            .collect();
        let kind = match self.kind {
            LodKind::Fixed => "FIXED",
            LodKind::Include => "INCLUDE",
            LodKind::Exclude => "EXCLUDE",
        };
        format!(
            "{}(o.ov) {kind} DIMENSIONS ({})",
            self.agg.sql(),
            dims.join(", ")
        )
    }
}

/// A generated predicate over the dimensions (index 3 is `ld`).
#[derive(Debug, Clone)]
enum Pred {
    Eq(usize, i64),
    IsNull(usize),
    Gt(usize, i64),
    Or(Box<Pred>, Box<Pred>),
    And(Box<Pred>, Box<Pred>),
}

fn dim_name(d: usize) -> &'static str {
    if d == LD {
        "ld"
    } else {
        DIMS[d].0
    }
}

impl Pred {
    fn sql(&self) -> String {
        match self {
            Pred::Eq(d, k) => format!("{} = {k}", dim_name(*d)),
            Pred::IsNull(d) => format!("{} IS NULL", dim_name(*d)),
            Pred::Gt(d, k) => format!("{} > {k}", dim_name(*d)),
            Pred::Or(a, b) => format!("({}) OR ({})", a.sql(), b.sql()),
            Pred::And(a, b) => format!("({}) AND ({})", a.sql(), b.sql()),
        }
    }

    fn dims(&self, out: &mut BTreeSet<usize>) {
        match self {
            Pred::Eq(d, _) | Pred::IsNull(d) | Pred::Gt(d, _) => {
                out.insert(*d);
            }
            Pred::Or(a, b) | Pred::And(a, b) => {
                a.dims(out);
                b.dims(out);
            }
        }
    }

    /// SQL three-valued logic: `None` is UNKNOWN.
    fn eval(&self, row: &[Option<i128>]) -> Option<bool> {
        match self {
            Pred::Eq(d, k) => row[*d].map(|v| v == i128::from(*k)),
            Pred::IsNull(d) => Some(row[*d].is_none()),
            Pred::Gt(d, k) => row[*d].map(|v| v > i128::from(*k)),
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

/// The metrics, by index.
/// `m_two` reads two inline LOD expressions, `lf`'s and `ld`'s:
/// `MAX(<lf>) - MIN(<ld>)`.
const METS: [&str; 8] = [
    "sum_ov", "m_lod", "m_inl", "m_mix", "m_two", "m_ie", "m_ie_inl", "m_ie_mix",
];

/// Whether metric `m` reads the INCLUDE or EXCLUDE expression `lie`.
fn reads_ie(m: usize) -> bool {
    METS[m].starts_with("m_ie")
}

/// A fact-list item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Item {
    Lf,
    Ov,
    Dim(usize),
}

#[derive(Debug, Clone)]
enum Kind {
    /// Dimensions (0..=3, 3 is `ld`) and metrics.
    Metrics(Vec<usize>, Vec<usize>),
    DimsOnly(Vec<usize>),
    /// Fact-list items, `Lf` among them.
    Facts(Vec<Item>),
}

#[derive(Debug, Clone)]
struct Case {
    inst: Instance,
    lf: Lod,
    ld: Lod,
    /// `lie`, INCLUDE or EXCLUDE.
    lie: Lod,
    /// The outer aggregate of `m_lod` and `m_inl`, and of `m_ie` and
    /// `m_ie_inl`.
    outer: Agg,
    kind: Kind,
    pred: Option<Pred>,
    /// The table declared first.
    o_first: bool,
    o_keyed: bool,
}

/// The row of a table with `n` rows whose id is `fk`, if any.
fn lookup(fk: Cell, n: usize) -> Option<usize> {
    fk.and_then(|v| usize::try_from(v).ok()).filter(|i| *i < n)
}

fn plain_dims(inst: &Instance, i: usize) -> [Option<i128>; 3] {
    let (fk, ocat, _) = inst.o[i];
    let c = lookup(fk, inst.c.len());
    [
        ocat.map(i128::from),
        c.and_then(|c| inst.c[c]).map(i128::from),
        c.map(|c| c as i128),
    ]
}

/// The fixed-dimension tuple of each order.
fn keys(inst: &Instance, lod: &Lod) -> Vec<Vec<Option<i128>>> {
    (0..inst.o.len())
        .map(|i| {
            let d = plain_dims(inst, i);
            lod.fixed.iter().map(|f| d[*f]).collect()
        })
        .collect()
}

/// Each fixed-grain tuple's value: the aggregate over every order with it.
fn lod_values(inst: &Instance, lod: &Lod) -> BTreeMap<Vec<Option<i128>>, Option<i128>> {
    let ks = keys(inst, lod);
    let mut members: BTreeMap<Vec<Option<i128>>, Vec<Option<i128>>> = BTreeMap::new();
    for (i, k) in ks.into_iter().enumerate() {
        members
            .entry(k)
            .or_default()
            .push(inst.o[i].2.map(i128::from));
    }
    members
        .into_iter()
        .map(|(k, vs)| (k, lod.agg.apply(vs.into_iter())))
        .collect()
}

impl Case {
    /// Each order's dimension row: `ocat, ccat, cid, ld`.
    fn rows(&self) -> Vec<Vec<Option<i128>>> {
        let ld = lod_values(&self.inst, &self.ld);
        let ld_keys = keys(&self.inst, &self.ld);
        (0..self.inst.o.len())
            .map(|i| {
                let mut row = plain_dims(&self.inst, i).to_vec();
                row.push(ld[&ld_keys[i]]);
                row
            })
            .collect()
    }

    fn passes(&self, row: &[Option<i128>]) -> bool {
        self.pred.as_ref().is_none_or(|p| p.eval(row) == Some(true))
    }

    /// The dimensions the query names, its filter's among them.
    fn all_dims(&self) -> BTreeSet<usize> {
        let mut out = BTreeSet::new();
        if let Some(p) = &self.pred {
            p.dims(&mut out);
        }
        match &self.kind {
            Kind::Metrics(d, _) | Kind::DimsOnly(d) => out.extend(d.iter().copied()),
            Kind::Facts(items) => out.extend(items.iter().filter_map(|i| match i {
                Item::Dim(d) => Some(*d),
                _ => None,
            })),
        }
        out
    }

    /// Whether the query is answered. A facts list is answered when one of
    /// its tables reaches the others many-to-one: `lf`'s fixed-grain values
    /// alone, or anything with the orders. `lf` beside only customer
    /// dimensions or `ld` (each LOD expression its own fixed-grain table) is
    /// refused, as rows of two tables neither of which reaches the other.
    fn answerable(&self) -> bool {
        if let Kind::Metrics(_, mets) = &self.kind {
            let by_ld = self.all_dims().contains(&LD);
            return !(by_ld && mets.iter().any(|m| reads_ie(*m)));
        }
        let Kind::Facts(items) = &self.kind else {
            return true;
        };
        let on_o = |d: usize| d < LD && DIMS[d].1 == "o";
        items.len() == 1 && self.pred.is_none()
            || items.contains(&Item::Ov)
            || self.all_dims().into_iter().any(on_o)
    }
}

/// Per group, the aggregate over entities, each counted once per group.
/// `entities` maps an entity to its value and the orders it covers.
fn once_per_group(
    rows: &[Vec<Option<i128>>],
    case: &Case,
    dims: &[usize],
    agg: Agg,
    entities: &[(Option<i128>, Vec<usize>)],
) -> BTreeMap<Vec<Option<i128>>, Option<i128>> {
    let mut per: BTreeMap<Vec<Option<i128>>, Vec<Option<i128>>> = BTreeMap::new();
    for (value, orders) in entities {
        let groups: BTreeSet<Vec<Option<i128>>> = orders
            .iter()
            .filter(|i| case.passes(&rows[**i]))
            .map(|i| dims.iter().map(|d| rows[*i][*d]).collect())
            .collect();
        for g in groups {
            per.entry(g).or_default().push(*value);
        }
    }
    let mut out: BTreeMap<Vec<Option<i128>>, Option<i128>> = per
        .into_iter()
        .map(|(g, vs)| (g, agg.apply(vs.into_iter())))
        .collect();
    if dims.is_empty() && out.is_empty() {
        out.insert(vec![], agg.apply(std::iter::empty()));
    }
    out
}

fn oracle_metric(
    case: &Case,
    rows: &[Vec<Option<i128>>],
    dims: &[usize],
    m: usize,
) -> BTreeMap<Vec<Option<i128>>, Option<i128>> {
    let inst = &case.inst;
    let orders: Vec<(Option<i128>, Vec<usize>)> = (0..inst.o.len())
        .map(|i| (inst.o[i].2.map(i128::from), vec![i]))
        .collect();
    let lf_keys = keys(inst, &case.lf);
    let lf: Vec<(Option<i128>, Vec<usize>)> = lod_values(inst, &case.lf)
        .into_iter()
        .map(|(k, v)| {
            let covers = (0..inst.o.len()).filter(|i| lf_keys[*i] == k).collect();
            (v, covers)
        })
        .collect();
    let ld_keys = keys(inst, &case.ld);
    let ld: Vec<(Option<i128>, Vec<usize>)> = lod_values(inst, &case.ld)
        .into_iter()
        .map(|(k, v)| {
            let covers = (0..inst.o.len()).filter(|i| ld_keys[*i] == k).collect();
            (v, covers)
        })
        .collect();
    let minus = |a: BTreeMap<Vec<Option<i128>>, Option<i128>>,
                 b: BTreeMap<Vec<Option<i128>>, Option<i128>>| {
        a.into_iter()
            .map(|(g, x)| {
                let y = b.get(&g).copied().flatten();
                (g, x.zip(y).map(|(x, y)| x - y))
            })
            .collect()
    };
    match METS[m] {
        "sum_ov" => once_per_group(rows, case, dims, Agg::Sum, &orders),
        "m_two" => minus(
            once_per_group(rows, case, dims, Agg::Max, &lf),
            once_per_group(rows, case, dims, Agg::Min, &ld),
        ),
        "m_lod" | "m_inl" => once_per_group(rows, case, dims, case.outer, &lf),
        _ => minus(
            once_per_group(rows, case, dims, Agg::Sum, &orders),
            once_per_group(rows, case, dims, Agg::Max, &lf),
        ),
    }
}

/// The groups a query of `dims` and the case's filter with no metrics lists:
/// the customers, with orders or not, when every member is a customer's;
/// otherwise the orders.
fn listed(case: &Case, rows: &[Vec<Option<i128>>], dims: &[usize]) -> BTreeSet<Vec<Option<i128>>> {
    let inst = &case.inst;
    let mut members = case
        .pred
        .as_ref()
        .map(|p| {
            let mut out = BTreeSet::new();
            p.dims(&mut out);
            out
        })
        .unwrap_or_default();
    members.extend(dims.iter().copied());
    let on_c = members.iter().all(|d| *d < LD && DIMS[*d].1 == "c");
    if on_c {
        (0..inst.c.len())
            .map(|c| vec![None, inst.c[c].map(i128::from), Some(c as i128), None])
            .filter(|r| case.passes(r))
            .map(|r| dims.iter().map(|d| r[*d]).collect())
            .collect()
    } else {
        rows.iter()
            .filter(|r| case.passes(r))
            .map(|r| dims.iter().map(|d| r[*d]).collect())
            .collect()
    }
}

/// `lie`'s grain in a query grouped by `dims`: the dimensions its tuples
/// hold.
fn ie_grain(case: &Case, dims: &[usize]) -> Vec<usize> {
    let mut grain: BTreeSet<usize> = dims.iter().copied().collect();
    match case.lie.kind {
        LodKind::Include => grain.extend(case.lie.fixed.iter().copied()),
        _ => {
            for d in &case.lie.fixed {
                grain.remove(d);
            }
        }
    }
    grain.into_iter().collect()
}

/// `lie`'s value per tuple of its grain, over the orders the filter keeps,
/// with the orders each covers.
fn ie_values(
    case: &Case,
    rows: &[Vec<Option<i128>>],
    grain: &[usize],
) -> BTreeMap<Vec<Option<i128>>, (Option<i128>, Vec<usize>)> {
    let mut members: BTreeMap<Vec<Option<i128>>, Vec<usize>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        if case.passes(r) {
            members
                .entry(grain.iter().map(|d| r[*d]).collect())
                .or_default()
                .push(i);
        }
    }
    members
        .into_iter()
        .map(|(k, orders)| {
            let v = case
                .lie
                .agg
                .apply(orders.iter().map(|i| case.inst.o[*i].2.map(i128::from)));
            (k, (v, orders))
        })
        .collect()
}

/// An outer aggregate of `lie` (`agg`) per group: an INCLUDE tuple counted
/// once per group of the orders it covers; for EXCLUDE, the value of each
/// group's coarser tuple, over `groups`.
fn oracle_ie(
    case: &Case,
    rows: &[Vec<Option<i128>>],
    dims: &[usize],
    agg: Agg,
    groups: &BTreeSet<Vec<Option<i128>>>,
) -> BTreeMap<Vec<Option<i128>>, Option<i128>> {
    let grain = ie_grain(case, dims);
    let values = ie_values(case, rows, &grain);
    if case.lie.kind == LodKind::Include {
        let entities: Vec<(Option<i128>, Vec<usize>)> = values.into_values().collect();
        return once_per_group(rows, case, dims, agg, &entities);
    }
    groups
        .iter()
        .map(|g| {
            let k: Vec<Option<i128>> = grain
                .iter()
                .map(|d| g[dims.iter().position(|x| x == d).expect("in the grain")])
                .collect();
            let v = values.get(&k).and_then(|(v, _)| *v);
            (g.clone(), agg.apply(std::iter::once(v)))
        })
        .collect()
}

/// The whole expected result, sorted.
fn oracle(case: &Case) -> Vec<Vec<Option<i128>>> {
    let inst = &case.inst;
    let rows = case.rows();
    let mut out: Vec<Vec<Option<i128>>> = match &case.kind {
        Kind::Metrics(dims, mets) => {
            let exclude = case.lie.kind == LodKind::Exclude;
            // Every metric's groups but the EXCLUDE parts', which take each
            // group's value; those add the listed groups.
            let mut per: Vec<Option<BTreeMap<Vec<Option<i128>>, Option<i128>>>> = Vec::new();
            let mut groups: BTreeSet<Vec<Option<i128>>> = BTreeSet::new();
            for m in mets {
                let p = match METS[*m] {
                    "m_ie" | "m_ie_inl" if exclude => None,
                    "m_ie" | "m_ie_inl" => Some(oracle_ie(case, &rows, dims, case.outer, &groups)),
                    "m_ie_mix" => Some(once_per_group(
                        &rows,
                        case,
                        dims,
                        Agg::Sum,
                        &(0..inst.o.len())
                            .map(|i| (inst.o[i].2.map(i128::from), vec![i]))
                            .collect::<Vec<_>>(),
                    )),
                    _ => Some(oracle_metric(case, &rows, dims, *m)),
                };
                if let Some(p) = &p {
                    groups.extend(p.keys().cloned());
                }
                if reads_ie(*m) && exclude {
                    // The listed groups whose coarser tuple has orders.
                    let grain = ie_grain(case, dims);
                    let values = ie_values(case, &rows, &grain);
                    groups.extend(listed(case, &rows, dims).into_iter().filter(|g| {
                        let k: Vec<Option<i128>> = grain
                            .iter()
                            .map(|d| g[dims.iter().position(|x| x == d).expect("in the grain")])
                            .collect();
                        values.contains_key(&k)
                    }));
                    groups.extend(
                        rows.iter()
                            .filter(|r| case.passes(r))
                            .map(|r| dims.iter().map(|d| r[*d]).collect()),
                    );
                }
                per.push(p);
            }
            if dims.is_empty() {
                groups.insert(vec![]);
            }
            let ex = |agg: Agg| oracle_ie(case, &rows, dims, agg, &groups);
            let per: Vec<BTreeMap<Vec<Option<i128>>, Option<i128>>> = mets
                .iter()
                .zip(per)
                .map(|(m, p)| match (METS[*m], p) {
                    ("m_ie_mix", Some(sum)) => {
                        let max = if exclude {
                            ex(Agg::Max)
                        } else {
                            oracle_ie(case, &rows, dims, Agg::Max, &groups)
                        };
                        sum.into_iter()
                            .map(|(g, x)| {
                                let y = max.get(&g).copied().flatten();
                                (g, x.zip(y).map(|(x, y)| x - y))
                            })
                            .collect()
                    }
                    (_, Some(p)) => p,
                    (_, None) => ex(case.outer),
                })
                .collect();
            groups
                .into_iter()
                .map(|g| {
                    let mut row = g.clone();
                    for p in &per {
                        row.push(p.get(&g).copied().flatten());
                    }
                    row
                })
                .collect()
        }
        Kind::DimsOnly(dims) => {
            let on_c = case.all_dims().iter().all(|d| *d < LD && DIMS[*d].1 == "c");
            let distinct: BTreeSet<Vec<Option<i128>>> = if on_c {
                // The customers, each one whether it has orders or not.
                (0..inst.c.len())
                    .map(|c| vec![None, inst.c[c].map(i128::from), Some(c as i128), None])
                    .filter(|r| case.passes(r))
                    .map(|r| dims.iter().map(|d| r[*d]).collect())
                    .collect()
            } else {
                rows.iter()
                    .filter(|r| case.passes(r))
                    .map(|r| dims.iter().map(|d| r[*d]).collect())
                    .collect()
            };
            distinct.into_iter().collect()
        }
        Kind::Facts(items) => {
            if items == &[Item::Lf] {
                lod_values(inst, &case.lf)
                    .into_values()
                    .map(|v| vec![v])
                    .collect()
            } else {
                let lf = lod_values(inst, &case.lf);
                let lf_keys = keys(inst, &case.lf);
                (0..inst.o.len())
                    .filter(|i| case.passes(&rows[*i]))
                    .map(|i| {
                        items
                            .iter()
                            .map(|it| match it {
                                Item::Lf => lf[&lf_keys[i]],
                                Item::Ov => inst.o[i].2.map(i128::from),
                                Item::Dim(d) => rows[i][*d],
                            })
                            .collect()
                    })
                    .collect()
            }
        }
    };
    out.sort();
    out
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
    (1usize..5).prop_flat_map(|nc| {
        (
            prop::collection::vec((fk(nc), cell(3), cell(20)), 1..9),
            prop::collection::vec(cell(3), nc),
        )
            .prop_map(|(o, c)| Instance { o, c })
    })
}

fn arb_agg() -> impl Strategy<Value = Agg> {
    prop_oneof![
        Just(Agg::Sum),
        Just(Agg::Count),
        Just(Agg::Min),
        Just(Agg::Max)
    ]
}

fn arb_lod() -> impl Strategy<Value = Lod> {
    (
        arb_agg(),
        prop::sample::subsequence((0..DIMS.len()).collect::<Vec<usize>>(), 0..=3),
    )
        .prop_map(|(agg, fixed)| Lod {
            agg,
            fixed,
            kind: LodKind::Fixed,
        })
}

fn arb_ie() -> impl Strategy<Value = Lod> {
    (
        arb_agg(),
        prop::sample::subsequence((0..DIMS.len()).collect::<Vec<usize>>(), 0..=2),
        any::<bool>(),
    )
        .prop_map(|(agg, fixed, include)| Lod {
            agg,
            fixed,
            kind: if include {
                LodKind::Include
            } else {
                LodKind::Exclude
            },
        })
}

fn arb_pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        (0..=LD, 0i64..3).prop_map(|(d, k)| Pred::Eq(d, k)),
        (0..=LD).prop_map(Pred::IsNull),
        (0..=LD, 0i64..25).prop_map(|(d, k)| Pred::Gt(d, k)),
    ];
    leaf.prop_recursive(2, 4, 2, |inner| {
        prop_oneof![
            (inner.clone(), inner.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            (inner.clone(), inner).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
        ]
    })
}

fn arb_kind() -> impl Strategy<Value = Kind> {
    let dims = |range| prop::sample::subsequence((0..=LD).collect::<Vec<usize>>(), range);
    let item = prop_oneof![
        Just(Item::Ov),
        Just(Item::Dim(0)),
        Just(Item::Dim(1)),
        Just(Item::Dim(LD)),
    ];
    prop_oneof![
        4 => (
            dims(0..=2),
            prop::sample::subsequence((0..METS.len()).collect::<Vec<usize>>(), 1..=3),
        )
            .prop_map(|(d, m)| Kind::Metrics(d, m)),
        1 => dims(1..=3).prop_map(Kind::DimsOnly),
        1 => prop::collection::btree_set(item, 0..=2).prop_map(|rest| {
            let mut items = vec![Item::Lf];
            items.extend(rest);
            Kind::Facts(items)
        }),
    ]
}

fn arb_case() -> impl Strategy<Value = Case> {
    (
        arb_instance(),
        arb_lod(),
        arb_lod(),
        arb_ie(),
        arb_agg(),
        arb_kind(),
        prop::option::of(arb_pred()),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(inst, lf, ld, lie, outer, kind, pred, o_first, o_keyed)| {
            // `lf` alone in FACTS is one row per fixed-grain value: no filter.
            let pred = if matches!(&kind, Kind::Facts(i) if i.len() == 1) {
                None
            } else {
                pred
            };
            Case {
                inst,
                lf,
                ld,
                lie,
                outer,
                kind,
                pred,
                o_first,
                o_keyed,
            }
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

fn build_def(case: &Case) -> SemanticViewDefinition {
    let o = table_ref("o", if case.o_keyed { &["id"] } else { &[] });
    let c = table_ref("c", &["id"]);
    let tables = if case.o_first { vec![o, c] } else { vec![c, o] };
    let joins = vec![Join {
        table: "c".to_string(),
        from_alias: "o".to_string(),
        fk_columns: vec!["fk_c".to_string()],
        ref_columns: vec!["id".to_string()],
        name: Some("o_c".to_string()),
        cardinality: Cardinality::ManyToOne,
        ..Default::default()
    }];
    let mut dimensions: Vec<Dimension> = DIMS
        .iter()
        .map(|(name, t, col)| Dimension {
            name: (*name).to_string(),
            expr: format!("{t}.{col}"),
            source_table: Some((*t).to_string()),
            ..Default::default()
        })
        .collect();
    dimensions.push(Dimension {
        name: "ld".to_string(),
        expr: case.ld.sql(),
        source_table: None,
        ..Default::default()
    });
    let outer = case.outer.sql();
    let metric = |name: &str, expr: String, table: Option<&str>| Metric {
        name: name.to_string(),
        expr,
        source_table: table.map(str::to_string),
        ..Default::default()
    };
    SemanticViewDefinition {
        tables,
        dimensions,
        metrics: vec![
            metric("sum_ov", "SUM(o.ov)".to_string(), Some("o")),
            metric("m_lod", format!("{outer}(lf)"), None),
            metric("m_inl", format!("{outer}({})", case.lf.sql()), None),
            metric("m_mix", "SUM(o.ov) - MAX(lf)".to_string(), None),
            metric(
                "m_two",
                format!("MAX({}) - MIN({})", case.lf.sql(), case.ld.sql()),
                None,
            ),
            metric("m_ie", format!("{outer}(lie)"), None),
            metric("m_ie_inl", format!("{outer}({})", case.lie.sql()), None),
            metric("m_ie_mix", "SUM(o.ov) - MAX(lie)".to_string(), None),
        ],
        facts: vec![
            Fact {
                name: "ov".to_string(),
                expr: "o.ov".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            },
            Fact {
                name: "lf".to_string(),
                expr: case.lf.sql(),
                source_table: None,
                ..Default::default()
            },
            Fact {
                name: "lie".to_string(),
                expr: case.lie.sql(),
                source_table: None,
                ..Default::default()
            },
        ],
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
        "CREATE TABLE o_tbl (id BIGINT, fk_c BIGINT, ocat BIGINT, ov BIGINT);
         CREATE TABLE c_tbl (id BIGINT, ccat BIGINT);",
    )
    .expect("schema");
    let mut sql = String::new();
    for (i, (f, cat, v)) in inst.o.iter().enumerate() {
        let cells = [*f, *cat, *v].map(sql_cell).join(", ");
        sql += &format!("INSERT INTO o_tbl VALUES ({i}, {cells});");
    }
    for (i, cat) in inst.c.iter().enumerate() {
        sql += &format!("INSERT INTO c_tbl VALUES ({i}, {});", sql_cell(*cat));
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
    let dims = |d: &[usize]| d.iter().map(|d| DimensionName::new(dim_name(*d))).collect();
    let mut req = QueryRequest {
        where_clause: case.pred.as_ref().map(Pred::sql),
        ..QueryRequest::default()
    };
    match &case.kind {
        Kind::Metrics(d, m) => {
            req.dimensions = dims(d);
            req.metrics = m.iter().map(|m| MetricName::new(METS[*m])).collect();
        }
        Kind::DimsOnly(d) => req.dimensions = dims(d),
        Kind::Facts(items) => {
            req.facts = items
                .iter()
                .map(|i| match i {
                    Item::Lf => FactName::new("lf"),
                    Item::Ov => FactName::new("ov"),
                    Item::Dim(d) => FactName::new(dim_name(*d)),
                })
                .collect();
        }
    }
    req
}

/// Answers the case and compares with the oracle: refused, or answered with
/// this many rows.
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
    fn lod_matches_snowflakes_rules(case in arb_case()) {
        check(&case)?;
    }
}

/// The generator reaches every kind of query with rows over a fixed-seed
/// sample, so the property above is not vacuous.
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
    let strategy = arb_case();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for _ in 0..1500 {
        let case = strategy.new_tree(&mut runner).expect("case").current();
        let outcome = check(&case).expect("the property holds");
        let label = match &case.kind {
            Kind::Metrics(..) if outcome.is_none() => "ie refused",
            _ if outcome.is_none() => "refused",
            _ if outcome == Some(0) => "empty",
            Kind::Metrics(d, m)
                if !d.is_empty()
                    && case.pred.is_some()
                    && m.iter().any(|m| reads_ie(*m))
                    && case.lie.kind == LodKind::Include =>
            {
                "include grouped, filtered"
            }
            Kind::Metrics(d, m)
                if !d.is_empty()
                    && case.pred.is_some()
                    && m.iter().any(|m| reads_ie(*m))
                    && case.lie.kind == LodKind::Exclude =>
            {
                "exclude grouped, filtered"
            }
            Kind::Metrics(d, m) if !d.is_empty() && m.iter().any(|m| reads_ie(*m)) => {
                "include or exclude grouped"
            }
            Kind::Metrics(d, m) if d.contains(&LD) && m.iter().any(|m| *m != 0) => {
                "lod metric by ld"
            }
            Kind::Metrics(d, m) if !d.is_empty() && m.iter().any(|m| *m != 0) => {
                "lod metric grouped"
            }
            Kind::Metrics(..) if case.pred.is_some() => "metric filtered",
            Kind::Metrics(..) => "metric",
            Kind::DimsOnly(d) if d.contains(&LD) => "dims only with ld",
            Kind::DimsOnly(_) => "dims only",
            Kind::Facts(i) if i.len() == 1 => "lf alone",
            Kind::Facts(_) => "facts",
        };
        *seen.entry(label).or_default() += 1;
    }
    for label in [
        "include grouped, filtered",
        "exclude grouped, filtered",
        "include or exclude grouped",
        "ie refused",
        "lod metric by ld",
        "lod metric grouped",
        "metric filtered",
        "dims only with ld",
        "lf alone",
        "facts",
        "refused",
    ] {
        assert!(
            seen.get(label).copied().unwrap_or(0) >= 10,
            "{label} reached too rarely: {seen:?}"
        );
    }
}
