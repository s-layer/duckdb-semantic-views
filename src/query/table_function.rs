use crate::catalog::CatalogReader;
use crate::expand::wildcard::{expand_wildcards, WildcardItemType};
use crate::expand::{expand, QueryRequest};
use crate::model::SemanticViewDefinition;
use crate::util::suggest_closest;

use super::error::QueryError;
use super::expr_items::{lower_expression_items, ItemError};
use super::items::{aliases_for, Item, OutputColumns, RequestItems};
use super::wire::{build_projected_sql, frame_bind_payload, parse_varchar_list};

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 6 (Wave 6) — sv_semantic_view_bind_rust
// ---------------------------------------------------------------------------
//
// FFI dispatcher for the migrated
// `semantic_view(view_name, dimensions := [...], metrics := [...], facts := [...])`
// table function. The C++ bind callback (`sv_semantic_view_bind` in
// `cpp/src/shim.cpp`) flattens the three optional LIST(VARCHAR) named parameters into the
// length-prefixed wire format (same encoding as the Wave 5 explain
// migration), and invokes this dispatcher in the caller's scope.
//
// Responsibilities of the Rust side:
//   - Catalog lookup (name normalisation, view-not-found suggestion).
//   - Wildcard expansion + `QueryRequest` construction.
//   - `expand::expand()` → expanded SQL, laid out as the items ask (aliases,
//     `column_order`).
//
// The dispatcher returns that SQL, UTF-8, and nothing else: the C++ side
// (`sv_semantic_view_bind_replace`) parses it into a subquery that DuckDB binds
// and plans inside the caller's query, so its column names and types are
// DuckDB's own and it runs in the caller's transaction (TECH-DEBT #42). No
// probe runs here; the catalog read is in the caller's transaction too
// (TECH-DEBT #19), so a view created earlier in it is found.
//
// Return codes mirror the Wave 5 dispatcher:
//   0 — success; (out_ptr, out_len) populated.
//   1 — user-visible error; error_buf populated (raised as BinderException).
//   2 — internal error (panic across FFI); error_buf populated.

/// # Safety
///
/// Wire-format payloads
/// described above; `name_ptr` must point to `name_len` UTF-8 bytes.
#[cfg(feature = "extension")]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn sv_semantic_view_bind_rust(
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    dims_ptr: *const u8,
    dims_len: usize,
    metrics_ptr: *const u8,
    metrics_len: usize,
    facts_ptr: *const u8,
    facts_len: usize,
    where_ptr: *const u8,
    where_len: usize,
    param_count: usize,
    order_ptr: *const u8,
    order_len: usize,
    fence: u8,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    // R-6/C-3: the catch_unwind guard, buffer publish, error-string write, and
    // panic arm now live in the shared
    // `run_dispatcher` scaffold (ST-2); the body does only the interesting work
    // and returns `Result<Vec<u8>, String>`.
    crate::ddl::read_ffi::run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_semantic_view_bind_rust",
        || unsafe {
            semantic_view_bind_body(
                name_ptr,
                name_len,
                sp_ptr,
                sp_len,
                dims_ptr,
                dims_len,
                metrics_ptr,
                metrics_len,
                facts_ptr,
                facts_len,
                where_ptr,
                where_len,
                param_count,
                order_ptr,
                order_len,
                fence != 0,
            )
        },
    )
}

/// Body for [`sv_semantic_view_bind_rust`]: decode the request args, resolve
/// the view and expand it. Returns the SQL to plan, UTF-8, or the user-visible
/// error message.
///
/// # Safety
///
/// Each `*_ptr` is either null or points to its paired `*_len` readable bytes.
/// A null pointer is only meaningful with `*_len == 0` (an empty list / absent
/// name); the decoders reject `(null, len > 0)` as FFI shape drift rather than
/// dereferencing it.
#[cfg(feature = "extension")]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
unsafe fn semantic_view_bind_body(
    name_ptr: *const u8,
    name_len: usize,
    sp_ptr: *const u8,
    sp_len: usize,
    dims_ptr: *const u8,
    dims_len: usize,
    metrics_ptr: *const u8,
    metrics_len: usize,
    facts_ptr: *const u8,
    facts_len: usize,
    where_ptr: *const u8,
    where_len: usize,
    param_count: usize,
    order_ptr: *const u8,
    order_len: usize,
    fence: bool,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::read_str_arg;

    let view_name_raw = read_str_arg(name_ptr, name_len, "view name")?;
    let view = crate::ident::parse_view_ref(&view_name_raw)
        .map_err(|e| format!("Invalid view name '{view_name_raw}': {e}"))?;
    let view_name = view.name.clone();
    let search_path = unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;

    let dimensions = parse_varchar_list(dims_ptr, dims_len)
        .map_err(|detail| format!("malformed `dimensions` payload: {detail}"))?;
    let metrics = parse_varchar_list(metrics_ptr, metrics_len)
        .map_err(|detail| format!("malformed `metrics` payload: {detail}"))?;
    let facts = parse_varchar_list(facts_ptr, facts_len)
        .map_err(|detail| format!("malformed `facts` payload: {detail}"))?;
    let column_order = parse_varchar_list(order_ptr, order_len)
        .map_err(|detail| format!("malformed `column_order` payload: {detail}"))?;

    // The `where_clause` named parameter is optional: the shim passes
    // (nullptr, 0) when the caller omitted it. An explicitly empty string is
    // also treated as absent — `where_clause := ''` would otherwise splice an
    // empty predicate and produce `WHERE ` followed by nothing.
    let where_clause = if where_ptr.is_null() || where_len == 0 {
        None
    } else {
        let raw = read_str_arg(where_ptr, where_len, "where_clause")?;
        if raw.trim().is_empty() {
            None
        } else {
            Some(raw)
        }
    };

    if dimensions.is_empty() && metrics.is_empty() && facts.is_empty() {
        return Err(QueryError::EmptyRequest { view_name }.to_string());
    }

    // FF-9: surface a probe-query failure as an error distinct from "no
    // views" instead of silently folding it into absence.
    let reader = CatalogReader::for_caller()?;
    let json_str = match reader.lookup(&view, &search_path) {
        Ok(Some(j)) => j,
        Ok(None) => {
            let available = reader.list_names().unwrap_or_default();
            let suggestion = suggest_closest(&view_name, &available);
            return Err(QueryError::ViewNotFound {
                name: view_name,
                suggestion,
                available,
            }
            .to_string());
        }
        Err(e) => return Err(e),
    };

    let def = SemanticViewDefinition::from_json(&view_name, &json_str)?;

    // `$1`, `$2`, ... in the predicate are the values of `params`, which the
    // shim puts in their place once the expansion is parsed (TECH-DEBT #105).
    if let Some(raw) = &where_clause {
        crate::expand::check_params_supplied(&view_name, raw, param_count)
            .map_err(|e| QueryError::from(e).to_string())?;
    }

    // For the shim's isolated bind (`fence`), the predicate becomes one that
    // names only its members (`expand::where_fence`), so the expansion checks
    // their expressions and joins on the query's own route without the
    // caller's outer columns.
    let where_clause = match where_clause {
        Some(raw) if fence => Some(
            crate::expand::where_fence(&view_name, &def, &raw)
                .map_err(|e| QueryError::from(e).to_string())?,
        ),
        other => other,
    };

    let invalid = |detail: String| {
        QueryError::InvalidRequest {
            view_name: view_name.clone(),
            detail,
        }
        .to_string()
    };
    let items =
        RequestItems::parse(&dimensions, &metrics, &facts, &column_order).map_err(invalid)?;
    // An expression item becomes a member of the query's own copy of the
    // definition (TECH-DEBT #76). Wildcards expand against the stored one, so
    // that `o.*` takes in no expression item.
    let lowered = lower_expression_items(&view_name, &def, items).map_err(|e| match e {
        ItemError::Invalid(detail) => invalid(detail),
        ItemError::Expand(e) => QueryError::from(*e).to_string(),
    })?;
    let items = &lowered.items;
    let stored = &def;
    let def = lowered.def.as_ref().unwrap_or(stored);
    let expand_list = |list: &[Item], kind: &WildcardItemType| {
        expand_wildcards(&RequestItems::members(list), stored, kind).map_err(|e| {
            QueryError::WildcardExpansion {
                view_name: view_name.clone(),
                detail: e,
            }
            .to_string()
        })
    };
    let dimensions = expand_list(&items.dimensions, &WildcardItemType::Dimension)?;
    let metrics = expand_list(&items.metrics, &WildcardItemType::Metric)?;
    let facts = expand_list(&items.facts, &WildcardItemType::Fact)?;
    let dimension_aliases = aliases_for(&items.dimensions, &dimensions).map_err(invalid)?;
    let metric_aliases = aliases_for(&items.metrics, &metrics).map_err(invalid)?;
    let fact_aliases = aliases_for(&items.facts, &facts).map_err(invalid)?;

    let req = QueryRequest {
        where_clause,
        dimensions: dimensions
            .iter()
            .map(|s| crate::expand::DimensionName::new(s.clone()))
            .collect(),
        metrics: metrics
            .iter()
            .map(|s| crate::expand::MetricName::new(s.clone()))
            .collect(),
        facts: facts
            .iter()
            .map(|s| crate::expand::FactName::new(s.clone()))
            .collect(),
    };
    let expanded_sql = expand(&view_name, def, &req)
        .map_err(|e| lowered.relabel(&QueryError::from(lowered.for_request(e)).to_string()))?;

    // Lay the columns out as the items ask (aliases, column_order).
    let column_names = crate::expand::output_column_names(&view_name, def, &req)
        .map_err(|e| lowered.relabel(&QueryError::from(lowered.for_request(e)).to_string()))?;
    let output = OutputColumns::new(
        &column_names,
        [&dimension_aliases, &metric_aliases, &fact_aliases],
        items.order,
    )
    .map_err(invalid)?;
    let (database, schema) = crate::expand::body_qualifier(def);
    Ok(frame_bind_payload(
        database,
        schema,
        &build_projected_sql(&expanded_sql, &column_names, &output),
    ))
}
