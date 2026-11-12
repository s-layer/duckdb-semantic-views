//! Relationship graph validation and topological sort for semantic view definitions.

mod cardinality;
mod derived_metrics;
mod facts;
mod member_exprs;
mod member_refs;
mod names;
mod relationship;
mod same_table_metrics;
mod toposort;
mod using;
mod window_dims;

#[cfg(test)]
mod test_helpers;

// Public API (matches prior graph.rs surface exactly)
pub(crate) use cardinality::infer_cardinality;
pub(crate) use derived_metrics::aggregate_name;
pub use derived_metrics::{contains_aggregate_function, validate_derived_metrics};
pub use facts::{find_fact_references, validate_facts};
pub use member_exprs::validate_member_expressions;
pub use member_refs::validate_member_references;
pub use names::validate_name_uniqueness;
pub(crate) use relationship::{find_relationship, RelNameMatch};
pub use relationship::{validate_graph, RelationshipGraph};
pub use same_table_metrics::{has_same_table_metric_refs, validate_same_table_metric_refs};
pub(crate) use same_table_metrics::{inline_same_table_metrics, same_table_metric_ref_spans};
pub use using::validate_using_relationships;
pub use window_dims::validate_window_dimensions;
