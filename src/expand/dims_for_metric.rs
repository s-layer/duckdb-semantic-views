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
    cardinality: Cardinality,
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
/// a plain aggregate on a table with a PRIMARY KEY), back along any
/// relationship that is not ASOF or range. The query refuses a fan-out back
/// across a temporal relationship (010956), and a window or semi-additive
/// metric, or one on a table with no key, refuses them all.
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
    let met_tables: Vec<String> = if let Some(ref st) = met.source_table {
        vec![st.to_ascii_lowercase()]
    } else {
        super::facts::collect_derived_metric_source_tables(met, &def.metrics)
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    };
    let card_map: HashMap<(String, String), Step> = def
        .joins
        .iter()
        .filter(|j| !j.fk_columns.is_empty())
        .map(|j| {
            (
                (
                    j.from_alias.to_ascii_lowercase(),
                    j.table.to_ascii_lowercase(),
                ),
                Step {
                    cardinality: j.cardinality,
                    temporal: j.asof.is_some() || j.range_end.is_some(),
                },
            )
        })
        .collect();
    let fans_out = super::fan_trap::metric_answers_fan_outs(def, met);

    // Steps: forward along any FK; backward along a one-to-one one, or along
    // any plain one when the metric answers a fan-out.
    let mut steps: HashMap<&str, Vec<&str>> = HashMap::new();
    for ((from, to), step) in &card_map {
        steps.entry(from.as_str()).or_default().push(to.as_str());
        if step.cardinality == Cardinality::OneToOne || (fans_out && !step.temporal) {
            steps.entry(to.as_str()).or_default().push(from.as_str());
        }
    }

    def.dimensions
        .iter()
        .filter(|d| reached(def, d, &met_tables, &steps))
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
        if super::refused_as_many_to_many(def, met_table, &dim_table) {
            return false;
        }
        let mut seen: HashSet<&str> = HashSet::from([met_table.as_str()]);
        let mut stack = vec![met_table.as_str()];
        while let Some(current) = stack.pop() {
            if current == dim_table {
                return true;
            }
            for next in steps.get(current).into_iter().flatten() {
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        false
    })
}
