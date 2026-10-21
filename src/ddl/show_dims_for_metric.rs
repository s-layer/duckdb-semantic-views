//! `SHOW SEMANTIC DIMENSIONS ... FOR METRIC` dispatcher (ST-2).
//!
//! A two-argument TF `(view_name, metric_name)` returning 3 VARCHAR + 1 BOOL
//! per row `(table_name, name, data_type, required)`. It doesn't fit the
//! shared 8-column `EntityRow` (extra metric arg, fan-trap reachability
//! filter, trailing BOOL column), so it keeps its own body — but routes the
//! scaffold through [`crate::ddl::read_ffi::run_dispatcher`] like every other
//! read-side function.

#![cfg(feature = "extension")]

use std::collections::{HashMap, HashSet};

use crate::catalog::CatalogReader;
use crate::ddl::read_ffi::{read_str_arg, run_dispatcher, serialize_varchar_bool_rows};
use crate::expand::collect_derived_metric_source_tables;
use crate::graph::{JoinTree, RelationshipGraph};
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
                    ],
                    required_dim_names.contains(&crate::ident::normalize_ident_part(&d.name)),
                )
            })
            .collect()
    } else {
        let graph =
            RelationshipGraph::from_definition(&def).map_err(|e| format!("graph error: {e}"))?;
        // The directed parent tree — same derivation the fan-trap check uses (E-7).
        let tree = JoinTree::from_graph(&graph);
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
            .filter(|d| is_dimension_reachable_for_metric(d, &met_tables, &tree, &card_map))
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
/// A dimension is reachable if, for at least one metric source table, the path
/// from the metric table to the dimension table does not traverse any edge in
/// the fan-out direction (i.e., from the "one" side to the "many" side).
///
/// Fan-out direction: for edge `(from_alias, to_alias)` with `ManyToOne`,
/// from->to is safe (forward), to->from is fan-out (reverse).
fn is_dimension_reachable_for_metric(
    dim: &Dimension,
    met_tables: &[String],
    tree: &JoinTree,
    card_map: &HashMap<(String, String), Cardinality>,
) -> bool {
    // Base table dimension (no source_table): always reachable
    let Some(ref dim_table_raw) = dim.source_table else {
        return true;
    };
    let dim_table = dim_table_raw.to_ascii_lowercase();

    for met_table in met_tables {
        if *met_table == dim_table {
            return true; // Same table, no fan-out possible
        }

        let met_ancestors = tree.ancestors_to_root(met_table);
        let dim_ancestors = tree.ancestors_to_root(&dim_table);

        // Find the lowest common ancestor (LCA)
        let dim_ancestor_set: HashSet<&String> = dim_ancestors.iter().collect();
        let lca = met_ancestors
            .iter()
            .find(|a| dim_ancestor_set.contains(a))
            .cloned();

        let Some(lca) = lca else {
            continue; // No common ancestor (disconnected -- shouldn't happen in valid tree)
        };

        let mut fan_out = false;

        // Check path UP from met_table to LCA.
        // Walking current -> parent at each step.
        let mut current = met_table.clone();
        while current != lca {
            let Some(parent) = tree.parent_of(&current) else {
                break;
            };

            // The card_map stores edges as (from_alias, to_alias) where
            // from_alias has the FK pointing to to_alias.
            //
            // Walking current -> parent:
            // - If edge is (current, parent): forward direction. ManyToOne forward = safe.
            // - If edge is (parent, current): reverse direction. ManyToOne reverse = fan-out.
            if card_map.get(&(current.clone(), parent.clone())).is_some() {
                // Edge is current->parent (current has FK to parent).
                // Walking current->parent = forward direction = always safe.
            } else if let Some(&card) = card_map.get(&(parent.clone(), current.clone())) {
                // Edge is parent->current (parent has FK to current).
                // Walking current->parent = reverse direction.
                // ManyToOne reverse = fan-out (going from "one" side to "many" side).
                if card == Cardinality::ManyToOne {
                    fan_out = true;
                    break;
                }
            }
            current = parent.clone();
        }

        if fan_out {
            // This metric source table path has fan-out for this dimension.
            // Try next metric source table (for derived metrics with multiple sources).
            continue;
        }

        // Check path DOWN from LCA to dim_table.
        // Build path from LCA to dim_table using dim_ancestors.
        // dim_ancestors is [dim_table, parent, ..., root].
        // Find LCA position and reverse the sub-path.
        if let Some(lca_pos) = dim_ancestors.iter().position(|a| *a == lca) {
            let path_down: Vec<String> = dim_ancestors[..=lca_pos].iter().rev().cloned().collect();
            for window in path_down.windows(2) {
                let a = &window[0]; // closer to LCA
                let b = &window[1]; // closer to dim_table
                                    // Walking a -> b (downward in the tree, away from root)
                if card_map.get(&(a.clone(), b.clone())).is_some() {
                    // Edge is a->b (a has FK to b). Walking forward = safe.
                } else if let Some(&card) = card_map.get(&(b.clone(), a.clone())) {
                    // Edge is b->a (b has FK to a). Walking a->b = reverse direction.
                    // ManyToOne reverse = fan-out.
                    if card == Cardinality::ManyToOne {
                        fan_out = true;
                        break;
                    }
                }
            }
        }

        if !fan_out {
            // Found a valid (no fan-out) path from this metric source to the dimension.
            return true;
        }
        // else: try next metric source table
    }

    // No metric source table found a valid path -- dimension not reachable.
    // Edge case: if met_tables is empty (derived metric with no source tables),
    // the dimension is not reachable.
    false
}
