//! Which dimensions `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` lists.
//!
//! Snowflake lists every dimension the metric can be grouped by, fanned ones
//! included, and leaves out one the query refuses: a range relationship
//! walked backwards (Phase 7 fixture 8's S1-S3 and F4). The rule is restated
//! here from the relationship graph rather than by expanding a query per
//! dimension, because a window metric's listing also carries the dimensions
//! it requires, which a query of it grouped by one other dimension refuses.
//! `tests/dag_proptest.rs` checks the two agree: every dimension a query of
//! the metric alone answers is listed, and a listed one it refuses is reached
//! two ways.

use std::collections::{HashMap, HashSet};

use crate::model::{Cardinality, Dimension, Metric, SemanticViewDefinition};

/// A relationship as [`dimensions_for_metric`] walks it.
#[derive(Clone, Copy)]
struct Step {
    one_to_one: bool,
    /// An ASOF or range relationship.
    temporal: bool,
}

/// The dimensions a query of `met` grouped by each one alone is answered for,
/// in declaration order, so that `SHOW SEMANTIC DIMENSIONS ... FOR METRIC`
/// lists them.
///
/// A dimension is listed when, for at least one of the metric's source tables,
/// some path from it to the dimension's table steps only forward along
/// foreign keys or back along one-to-one relationships, or, when the metric
/// answers a fan-out once per group ([`super::fan_trap::metric_answers_fan_outs`]:
/// a plain aggregate on a table with a PRIMARY KEY, or a semi-additive metric
/// grouped by its `NON ADDITIVE BY` dimension), back along any
/// relationship that is not ASOF or range. The query refuses a fan-out back
/// across a temporal relationship (010956), and a semi-additive metric
/// grouped by another dimension, or a metric on a table with no key, refuses
/// them all. A window metric lists what its inner metric lists, and the
/// dimensions its window clause names.
///
/// Every path counts, not only the one through a spanning tree: in a diamond
/// (orders reach regions through customers and through stores) the tree walked
/// from the first table could route orders to stores through regions, which
/// fans out, and hide a dimension the metric reaches directly. A table reached
/// by several paths is still listed; a query that needs one path picks it
/// with `USING`, and one that needs none is refused by the query's own checks.
///
/// A table reached by no chain and two ways is left out, as the query refuses
/// it ([`super::refused_as_many_to_many`]): a step back along a one-to-one
/// relationship does not fan out, but it is not a chain either.
#[must_use]
pub fn dimensions_for_metric<'a>(
    def: &'a SemanticViewDefinition,
    met: &Metric,
) -> Vec<&'a Dimension> {
    if def.joins.is_empty() {
        return def.dimensions.iter().collect();
    }
    // A window metric runs over the rows its inner metric gives (Phase 7 PR
    // 5), so it can be grouped by whatever its inner metric can, and by the
    // dimensions its window clause names.
    if let Some(inner) = window_inner(def, met) {
        let listed: HashSet<*const Dimension> = dimensions_for_metric(def, inner)
            .into_iter()
            .map(std::ptr::from_ref)
            .collect();
        let required: HashSet<String> = super::window::window_required_dimensions(def, met)
            .iter()
            .map(|d| crate::ident::normalize_ident_part(d))
            .collect();
        return def
            .dimensions
            .iter()
            .filter(|d| {
                listed.contains(&std::ptr::from_ref(*d))
                    || required.contains(&crate::ident::normalize_ident_part(&d.name))
            })
            .collect();
    }
    let met_tables: Vec<String> = if let Some(ref st) = met.source_table {
        vec![st.to_ascii_lowercase()]
    } else {
        super::facts::collect_derived_metric_source_tables(met, &def.metrics)
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    };
    // Two relationships between the same pair (a second role) are one step:
    // temporal when either is, as the query's `fan_trap::temporal_edges`
    // reads it, and one-to-one only when both are.
    let mut card_map: HashMap<(String, String), Step> = HashMap::new();
    for j in def.joins.iter().filter(|j| !j.fk_columns.is_empty()) {
        let step = Step {
            one_to_one: j.cardinality == Cardinality::OneToOne,
            temporal: j.asof.is_some() || j.range_end.is_some(),
        };
        card_map
            .entry((
                j.from_alias.to_ascii_lowercase(),
                j.table.to_ascii_lowercase(),
            ))
            .and_modify(|s| {
                s.one_to_one &= step.one_to_one;
                s.temporal |= step.temporal;
            })
            .or_insert(step);
    }

    // Steps: forward along any FK; backward along a one-to-one one, or along
    // any plain one when the metric answers a fan-out.
    let steps = |fans_out: bool| {
        let mut steps: HashMap<&str, Vec<&str>> = HashMap::new();
        for ((from, to), step) in &card_map {
            steps.entry(from.as_str()).or_default().push(to.as_str());
            if step.one_to_one || (fans_out && !step.temporal) {
                steps.entry(to.as_str()).or_default().push(from.as_str());
            }
        }
        steps
    };
    let any_dim = super::fan_trap::metric_answers_fan_outs(def, met, None);
    let (plain, fanning) = (steps(false), steps(true));
    let snapshots = fanning_snapshots(def, met, &plain);
    def.dimensions
        .iter()
        .filter(|d| {
            let key = crate::ident::normalize_ident_part(&d.name);
            let snapshot_fans = snapshots
                .iter()
                .any(|na_keys| met.source_table.is_none() || na_keys.iter().any(|k| *k != key));
            let fans = any_dim || super::fan_trap::metric_answers_fan_outs(def, met, Some(d));
            !snapshot_fans && reached(def, d, &met_tables, if fans { &fanning } else { &plain })
        })
        .collect()
}

/// The `NON ADDITIVE BY` keys of each semi-additive metric `met` is built
/// from whose snapshot joins a `NON ADDITIVE BY` dimension across a fan-out.
/// A query of `met` grouped by any dimension but those (the metric is then
/// active and takes the snapshot path) is refused as a fan trap, its own
/// table's dimensions included (EXP-9,
/// `tests_fan_trap::semi_additive_na_dim_fans_def`).
fn fanning_snapshots(
    def: &SemanticViewDefinition,
    met: &Metric,
    plain: &HashMap<&str, Vec<&str>>,
) -> Vec<Vec<String>> {
    let names: HashSet<String> = if met.source_table.is_some() {
        HashSet::from([crate::ident::normalize_ident_part(&met.name)])
    } else {
        super::facts::collect_transitive_metric_names(met, &def.metrics)
    };
    def.metrics
        .iter()
        .filter(|m| names.contains(&crate::ident::normalize_ident_part(&m.name)))
        .filter_map(|m| {
            let home = vec![m.source_table.as_deref()?.to_ascii_lowercase()];
            let fans = m.non_additive_by.iter().any(|na| {
                super::resolution::find_dimension(def, &na.dimension)
                    .and_then(|nd| nd.source_table.as_deref())
                    .is_some_and(|t| !walk(&home, &t.to_ascii_lowercase(), plain))
            });
            fans.then(|| {
                m.non_additive_by
                    .iter()
                    .map(|na| super::semi_additive::na_dim_match_key(def, &na.dimension))
                    .collect()
            })
        })
        .collect()
}

/// Whether `dim`'s table is reached from one of `met_tables` along `steps`.
fn reached(
    def: &SemanticViewDefinition,
    dim: &Dimension,
    met_tables: &[String],
    steps: &HashMap<&str, Vec<&str>>,
) -> bool {
    // Base table dimension (no source_table): always reachable
    let Some(ref dim_table_raw) = dim.source_table else {
        return true;
    };
    let dim_table = dim_table_raw.to_ascii_lowercase();
    met_tables.iter().any(|met_table| {
        !super::refused_as_many_to_many(def, met_table, &dim_table)
            && walk(std::slice::from_ref(met_table), &dim_table, steps)
    })
}

/// Whether `to` is reached from one of `from` along `steps`.
fn walk(from: &[String], to: &str, steps: &HashMap<&str, Vec<&str>>) -> bool {
    let mut seen: HashSet<&str> = from.iter().map(String::as_str).collect();
    let mut stack: Vec<&str> = seen.iter().copied().collect();
    while let Some(current) = stack.pop() {
        if current == to {
            return true;
        }
        for next in steps.get(current).into_iter().flatten() {
            if seen.insert(next) {
                stack.push(next);
            }
        }
    }
    false
}

/// The metric a window metric `met` is computed over, unless that is itself a
/// window (which the query refuses, `WindowOverWindow`).
fn window_inner<'a>(def: &'a SemanticViewDefinition, met: &Metric) -> Option<&'a Metric> {
    let inner = &met.window_spec.as_ref()?.inner_metric;
    def.metrics
        .iter()
        .find(|m| crate::ident::ident_matches(&m.name, inner))
        .filter(|m| !m.is_window())
}
