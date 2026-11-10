//! `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` dispatcher (ST-2).
//!
//! A two-argument TF `(view_name, metric_name)` returning Snowflake's columns
//! `(table_name, name, data_type, required, synonyms, comment)`: five VARCHAR
//! and the BOOL `required`, which goes last on the wire and fourth in the
//! output. It doesn't fit the shared `EntityRow` (extra metric arg, fan-trap
//! reachability filter, BOOL column), so it keeps its own body — but routes the
//! scaffold through [`crate::ddl::read_ffi::run_dispatcher`] like every other
//! read-side function.

#![cfg(feature = "extension")]

use std::collections::{HashMap, HashSet};

use crate::catalog::CatalogReader;
use crate::ddl::describe::format_json_array;
use crate::ddl::read_ffi::{read_str_arg, run_dispatcher, serialize_varchar_bool_rows};
use crate::expand::collect_derived_metric_source_tables;
use crate::graph::RelationshipGraph;
use crate::model::{Cardinality, Dimension, SemanticViewDefinition};
use crate::util::suggest_closest;

/// # Safety
///
/// Both name pointers must point to valid UTF-8
/// bytes of the matching length. The caller releases the returned buffer via
/// `sv_free_buffer`.
#[no_mangle]
pub unsafe extern "C" fn sv_show_semantic_dimensions_for_metric_bind_rust(
    view_name_ptr: *const u8,
    view_name_len: usize,
    metric_name_ptr: *const u8,
    metric_name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_show_semantic_dimensions_for_metric_bind_rust",
        || unsafe {
            show_dims_for_metric(
                view_name_ptr,
                view_name_len,
                metric_name_ptr,
                metric_name_len,
                sp_ptr,
                sp_len,
            )
        },
    )
}

/// Body for [`sv_show_semantic_dimensions_for_metric_bind_rust`]: resolve the
/// view + metric, then emit each dimension with a `required` flag, filtered to
/// those reachable from the metric's source table(s) without a fan trap.
///
/// # Safety
///
/// `view_name_ptr` / `metric_name_ptr` must each be null or point to the
/// matching number of readable bytes.
// Linear resolve-and-filter path (decode names, look up view, find metric,
// walk reachable dimensions, serialize); kept together so each not-found case
// returns its own tailored diagnostic.
#[allow(clippy::too_many_lines)]
unsafe fn show_dims_for_metric(
    view_name_ptr: *const u8,
    view_name_len: usize,
    metric_name_ptr: *const u8,
    metric_name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
) -> Result<Vec<u8>, String> {
    let view_name = read_str_arg(view_name_ptr, view_name_len, "view name")?;
    let metric_name = read_str_arg(metric_name_ptr, metric_name_len, "metric name")?;
    // FF-4: normalize the view name so quoted-identifier inputs resolve the
    // same way they do through `semantic_view()`.
    let view = crate::ident::parse_view_ref(&view_name)
        .map_err(|e| format!("Invalid view name '{view_name}': {e}"))?;
    let view_name = view.name.clone();
    let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;

    let reader = CatalogReader::for_caller()?;
    let Some(json) = reader.lookup(&view, &search_path)? else {
        let available = reader.list_names().unwrap_or_default();
        let not_found = crate::catalog::view_not_found_msg(&view.to_string());
        return Err(match suggest_closest(&view_name, &available) {
            Some(suggestion) => format!("{not_found}. Did you mean '{suggestion}'?"),
            None => not_found,
        });
    };
    let def = SemanticViewDefinition::from_json(&view_name, &json)?;

    // `ident_matches`, not a bare case fold. The parser embeds the RAW metric
    // name (FF-4) and a metric declared `t."total amount"` is STORED with its
    // quotes, so the same-spelling case did match under the old fold — what it
    // could not do is match ACROSS spellings: a metric declared `plain_total`
    // referenced as `FOR METRIC "plain_total"` compared `plain_total` against
    // `"plain_total"` and reported the metric as not found. This is the #28
    // name-field-matcher class — fold case AND strip quotes.
    let Some(met) = def
        .metrics
        .iter()
        .find(|m| crate::ident::ident_matches(&m.name, &metric_name))
    else {
        let available: Vec<String> = def.metrics.iter().map(|m| m.name.clone()).collect();
        return Err(match suggest_closest(&metric_name, &available) {
            Some(suggestion) => format!(
                "metric '{metric_name}' not found in semantic view '{view_name}'. \
                 Did you mean '{suggestion}'?"
            ),
            None => format!("metric '{metric_name}' not found in semantic view '{view_name}'"),
        });
    };

    // A window metric's PARTITION BY [EXCLUDING] / ORDER BY references,
    // resolved (bare, dotted or quoted) to their declared dimensions by the
    // same resolver the query-time `WindowMetricRequiredDimension` check uses,
    // keyed by the canonical identifier. A raw `to_ascii_lowercase()` of the
    // reference used to miss `ORDER BY s.placed` and `ORDER BY "PLACED"` (the
    // #28 name-matcher class), reporting `required = false` for a dimension
    // the query then demanded.
    let required_dim_names: HashSet<String> = crate::expand::window_required_dimensions(&def, met)
        .iter()
        .map(|n| crate::ident::normalize_ident_part(n))
        .collect();

    let met_tables: Vec<String> = if let Some(ref st) = met.source_table {
        vec![st.to_ascii_lowercase()]
    } else {
        collect_derived_metric_source_tables(met, &def.metrics)
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect()
    };

    let alias_map = def.alias_to_table_map();
    // TECH-DEBT #51: probe the dimensions' types in the caller's context.
    let types = crate::ddl::read_ffi::member_types_for_bind(
        &view_name,
        &def,
        crate::query::member_types::Wanted::DIMENSIONS,
    )?;
    let mut rows: Vec<(Vec<String>, bool)> = if def.joins.is_empty() {
        def.dimensions
            .iter()
            .map(|d| {
                let table_name = d
                    .source_table
                    .as_ref()
                    .and_then(|a| alias_map.get(a).cloned())
                    .unwrap_or_default();
                (
                    vec![
                        table_name,
                        d.name.clone(),
                        types.dimension(&d.name, d.output_type.as_ref()),
                        format_json_array(&d.synonyms),
                        d.comment.clone().unwrap_or_default(),
                    ],
                    required_dim_names.contains(&crate::ident::normalize_ident_part(&d.name)),
                )
            })
            .collect()
    } else {
        // Built only to surface a malformed graph as an error, as before.
        RelationshipGraph::from_definition(&def).map_err(|e| format!("graph error: {e}"))?;
        let card_map: HashMap<(String, String), Cardinality> = def
            .joins
            .iter()
            .filter(|j| !j.fk_columns.is_empty())
            .map(|j| {
                (
                    (
                        j.from_alias.to_ascii_lowercase(),
                        j.table.to_ascii_lowercase(),
                    ),
                    j.cardinality,
                )
            })
            .collect();
        def.dimensions
            .iter()
            .filter(|d| is_dimension_reachable_for_metric(d, &met_tables, &card_map))
            .map(|d| {
                let table_name = d
                    .source_table
                    .as_ref()
                    .and_then(|a| alias_map.get(a).cloned())
                    .unwrap_or_default();
                (
                    vec![
                        table_name,
                        d.name.clone(),
                        types.dimension(&d.name, d.output_type.as_ref()),
                        format_json_array(&d.synonyms),
                        d.comment.clone().unwrap_or_default(),
                    ],
                    required_dim_names.contains(&crate::ident::normalize_ident_part(&d.name)),
                )
            })
            .collect()
    };
    rows.sort_by(|a, b| a.0[1].cmp(&b.0[1]));
    serialize_varchar_bool_rows(&rows)
}

/// Check if a dimension is reachable from a given set of metric source tables
/// without causing a fan trap.
///
/// A dimension is reachable if, for at least one metric source table, some
/// path from the metric table to the dimension table never steps in the
/// fan-out direction (from the "one" side to the "many" side).
///
/// Fan-out direction: for edge `(from_alias, to_alias)` with `ManyToOne`,
/// from->to is safe (forward), to->from is fan-out (reverse). A `OneToOne`
/// edge is safe both ways.
///
/// Every path counts, not only the one through a spanning tree: in a diamond
/// (orders reach regions through customers and through stores) the tree walked
/// from the first table could route orders to stores through regions, which
/// fans out, and hide a dimension the metric reaches directly. A table reached
/// by several safe paths is still listed; a query that needs one path picks it
/// with `USING`, and one that needs none is refused by the query's own checks.
fn is_dimension_reachable_for_metric(
    dim: &Dimension,
    met_tables: &[String],
    card_map: &HashMap<(String, String), Cardinality>,
) -> bool {
    // Base table dimension (no source_table): always reachable
    let Some(ref dim_table_raw) = dim.source_table else {
        return true;
    };
    let dim_table = dim_table_raw.to_ascii_lowercase();

    // Safe steps: forward along any FK, and backward along a one-to-one one.
    let mut safe_steps: HashMap<&str, Vec<&str>> = HashMap::new();
    for ((from, to), card) in card_map {
        safe_steps
            .entry(from.as_str())
            .or_default()
            .push(to.as_str());
        if *card == Cardinality::OneToOne {
            safe_steps
                .entry(to.as_str())
                .or_default()
                .push(from.as_str());
        }
    }

    met_tables.iter().any(|met_table| {
        let mut seen: HashSet<&str> = HashSet::from([met_table.as_str()]);
        let mut stack = vec![met_table.as_str()];
        while let Some(current) = stack.pop() {
            if current == dim_table {
                return true;
            }
            for next in safe_steps.get(current).into_iter().flatten() {
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
        false
    })
}
