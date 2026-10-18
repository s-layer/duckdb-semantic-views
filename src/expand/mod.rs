mod facts;
mod fan_trap;
mod join_resolver;
mod materialization;
mod per_grain;
mod resolution;
mod role_playing;
mod select_spec;
mod semi_additive;
mod sql_gen;
mod types;
mod where_clause;
// Live under the `extension` feature (wildcard expansion in the query/explain
// FFI paths); dead only in the default build, so scope the allow accordingly
// (ST-8) rather than blanket-suppressing dead_code in the extension build too.
#[cfg_attr(not(feature = "extension"), allow(dead_code))]
pub(crate) mod wildcard;
mod window;

#[cfg(test)]
mod test_helpers;

// Behaviour-named expansion test modules, extracted from sql_gen.rs's monolithic
// phase-named `mod tests` (§6.2 move 6, code-review 2026-07-11).
#[cfg(test)]
mod tests_cast;
#[cfg(test)]
mod tests_count_star_rewrite;
#[cfg(test)]
mod tests_derived_metric;
#[cfg(test)]
mod tests_expand;
#[cfg(test)]
mod tests_expand_basic;
#[cfg(test)]
mod tests_fact_inlining;
#[cfg(test)]
mod tests_fact_query;
#[cfg(test)]
mod tests_facts_awareness;
#[cfg(test)]
mod tests_facts_path_role_playing;
#[cfg(test)]
mod tests_fan_trap;
// Pins NUMBERS (executes the SQL in-memory), so gated like
// `tests_phantom_row_guard` below.
#[cfg(all(test, not(feature = "extension")))]
mod tests_bare_column_scoping;
#[cfg(all(test, not(feature = "extension")))]
mod tests_home_anchoring;
#[cfg(test)]
mod tests_join_emission_regression;
#[cfg(test)]
mod tests_per_grain;
// These tests pin NUMBERS, not SQL text: every case executes the generated SQL
// against an in-memory `DuckDB`. The `extension` feature swaps the bundled
// DuckDB API for loadable-extension stubs, where `Connection::open_in_memory`
// panics with "DuckDB API not initialized" — so the module is gated the same way
// `semi_additive.rs`'s `execution` submodule and `catalog.rs`'s tests are. The
// default-features run (the one that can execute SQL) is what covers this file.
#[cfg(all(test, not(feature = "extension")))]
mod tests_phantom_row_guard;
#[cfg(test)]
mod tests_pkfk_expand;
#[cfg(test)]
mod tests_private_access;
#[cfg(test)]
mod tests_qualified_name_resolution;
#[cfg(test)]
mod tests_qualified_refs;
#[cfg(test)]
mod tests_role_playing;
#[cfg(test)]
mod tests_semi_additive_dependency;

// Public API (the pre-split expand.rs surface, plus the boxed fan-trap detail
// structs re-exported for R-9).
pub use resolution::{quote_ident, quote_ident_if_needed, quote_stored_ident, quote_table_ref};
pub use sql_gen::{expand, output_column_names};
pub use types::{
    DimensionName, ExpandError, FactName, FanTrapError, MetricFanTrapError, MetricName,
    QueryRequest,
};

// Crate-internal API (used by ddl/show_dims_for_metric.rs under extension feature)
#[cfg(feature = "extension")]
pub(crate) use facts::collect_derived_metric_source_tables;
#[cfg(feature = "extension")]
pub(crate) use materialization::find_routing_materialization_name;
// The shim's isolated bind of `semantic_view()` (`query::table_function`).
#[cfg(feature = "extension")]
pub(crate) use where_clause::where_fence;
// Read-side member-type probe (TECH-DEBT #51, `query::member_types`).
pub(crate) use window::window_required_dimensions;
