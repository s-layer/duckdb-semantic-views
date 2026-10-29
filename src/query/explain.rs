use crate::catalog::CatalogReader;
use crate::expand::find_routing_materialization_name;
use crate::expand::{expand, QueryRequest};
use crate::model::SemanticViewDefinition;
use crate::util::suggest_closest;

use super::error::QueryError;
use super::expr_items::{lower_expression_items, ItemError};
use super::items::{aliases_for, OutputColumns, RequestItems};
use crate::expand::wildcard::{expand_wildcards, WildcardItemType};

use super::wire::parse_varchar_list;

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 5 (Wave 5) — sv_explain_semantic_view_bind_rust
// ---------------------------------------------------------------------------
//
// FFI dispatcher for the migrated `explain_semantic_view(view_name,
// dimensions := [...], metrics := [...], facts := [...])` table function.
//
// The C++ bind callback (`sv_explain_semantic_view_bind` in
// `cpp/src/shim.cpp`) pulls the positional view-name from `input.inputs[0]` and the optional
// LIST(VARCHAR) named parameters from `input.named_parameters`, serialises
// the three string lists into the standard length-prefixed wire format,
// and invokes this dispatcher in the caller's scope, so the definition is read
// and the plan made in the caller's transaction (TECH-DEBT #19).
//
// Wire format for the three list arguments (`dims_buf`, `metrics_buf`,
// `facts_buf`), each independently encoded as:
//
//   u32 count (little-endian)
//   for each entry:
//     u32 byte_len (little-endian)
//     byte_len bytes (UTF-8, NOT NUL-terminated)
//
// Output wire format (matches Wave 0/1 VARCHAR-rows encoding):
//
//   u32 row_count (little-endian)
//   for each row:
//     u32 byte_len (little-endian)
//     byte_len bytes (UTF-8) — one explain-output line per row, single VARCHAR column
//
// Return codes:
//   0 — success; `(out_ptr, out_len)` populated.
//   1 — user-visible error (catalog miss, validation, expand failure);
//       `error_buf` populated. The C++ side raises `BinderException`.
//   2 — internal error (panic across FFI, allocation failure); `error_buf`
//       populated.

/// # Safety
///
/// The three `*_buf` /
/// `*_len` pairs encode LIST(VARCHAR) arguments using the wire format
/// documented above. `name_ptr` must point to `name_len` UTF-8 bytes.
#[cfg(feature = "extension")]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn sv_explain_semantic_view_bind_rust(
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
    vars_ptr: *const u8,
    vars_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    // R-6/C-3: the catch_unwind guard, buffer publish, error-string write, and
    // panic arm now live in the shared
    // `run_dispatcher` scaffold (ST-2); the body returns `Result<Vec<u8>, String>`.
    crate::ddl::read_ffi::run_dispatcher(
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_explain_semantic_view_bind_rust",
        || unsafe {
            explain_semantic_view_bind_body(
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
                vars_ptr,
                vars_len,
            )
        },
    )
}

/// Body for [`sv_explain_semantic_view_bind_rust`]: decode the request args,
/// resolve + expand the view, capture its `EXPLAIN` plan, and serialize the
/// annotated output as 1-column VARCHAR rows.
///
/// # Safety
///
/// Each `*_ptr` is either null or points to its paired `*_len` readable bytes.
/// A null pointer is only meaningful with `*_len == 0` (an empty list / absent
/// name); the decoders reject `(null, len > 0)` as FFI shape drift rather than
/// dereferencing it.
#[cfg(feature = "extension")]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
unsafe fn explain_semantic_view_bind_body(
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
    vars_ptr: *const u8,
    vars_len: usize,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::{read_str_arg, serialize_varchar_rows};

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
    let variables = parse_varchar_list(vars_ptr, vars_len)
        .map_err(|detail| format!("malformed `variables` payload: {detail}"))?;

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
        // Match the QueryError::EmptyRequest message rendered by the legacy
        // VTab so phase57_introspection assertions stay byte-identical.
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
    // shim puts in their place when it plans the expansion (TECH-DEBT #105).
    if let Some(raw) = &where_clause {
        crate::expand::check_params_supplied(&view_name, raw, param_count)
            .map_err(|e| QueryError::from(e).to_string())?;
    }

    // The variables the query sets are the values of `params` after the
    // predicate's own (Phase 6.5): written into the member expressions and the
    // predicate here, so the expansion never sees a variable.
    let bound = crate::variables::bind(
        &view_name,
        def,
        crate::variables::Values::Query {
            names: &variables,
            first_slot: param_count + 1,
        },
    )?;
    let crate::variables::Bound { def, substitution } = bound;
    let where_clause = where_clause.map(|w| substitution.apply(&w));

    // The items are parsed and checked as semantic_view() checks them: a member
    // requested twice with different aliases, or two output columns with one
    // name, is refused here too. The aliases and the column order rename and
    // reorder only the output, which the explained SQL does not show.
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
    let lowered =
        lower_expression_items(&view_name, &def, items, &substitution).map_err(|e| match e {
            ItemError::Invalid(detail) => invalid(detail),
            ItemError::Expand(e) => QueryError::from(*e).to_string(),
        })?;
    let items = &lowered.items;
    let stored = &def;
    let def = lowered.def.as_ref().unwrap_or(stored);
    // R-3 (code-review 2026-07-11): wildcard failures render through
    // QueryError::WildcardExpansion, matching semantic_view()'s wording.
    let expand_list = |list: &[crate::query::items::Item], kind: &WildcardItemType| {
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

    let mat_name = {
        // Resolve the requested names to their stored items with the SAME
        // quote-aware key `expand()` uses (`ident::ident_matches`), so this
        // display header agrees with the routing in the emitted SQL by
        // construction. A bare `eq_ignore_ascii_case` here treated a quoted
        // reference's quote characters as literal bytes, matching nothing, so
        // a query like `dimensions := ['"region"']` (which `expand()`
        // resolves and routes to a materialization) reported
        // `-- Materialization: none` — the header contradicting the routed SQL
        // two sections below (code-review re-review on #84). The
        // table-qualified variant (`'o.region'`) is still dropped here — a
        // pre-existing gap this header never handled — and is unified with the
        // rest of the materialization matcher in the §6.2 reference engine.
        let dim_refs: Vec<&crate::model::Dimension> = dimensions
            .iter()
            .filter_map(|name| {
                def.dimensions
                    .iter()
                    .find(|d| crate::ident::ident_matches(&d.name, name))
            })
            .collect();
        let met_refs: Vec<&crate::model::Metric> = metrics
            .iter()
            .filter_map(|name| {
                def.metrics
                    .iter()
                    .find(|m| crate::ident::ident_matches(&m.name, name))
            })
            .collect();
        find_routing_materialization_name(def, &dim_refs, &met_refs).map(String::from)
    };

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
    let column_names = crate::expand::output_column_names(&view_name, def, &req)
        .map_err(|e| lowered.relabel(&QueryError::from(lowered.for_request(e)).to_string()))?;
    OutputColumns::new(
        &column_names,
        [&dimension_aliases, &metric_aliases, &fact_aliases],
        items.order,
    )
    .map_err(invalid)?;

    // Build the three-part output, identical to the legacy VTab so
    // phase28_e2e / phase46_* / phase57_introspection / phase64
    // assertions stay byte-identical.
    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("-- Semantic View: {view_name}"));
    // An expression item is shown as written, not as the member it became.
    let shown = |names: &[String]| {
        names
            .iter()
            .map(|n| {
                lowered
                    .labels
                    .iter()
                    .find(|(member, _)| crate::ident::ident_matches(member, n))
                    .map_or(n.as_str(), |(_, text)| text.as_str())
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    lines.push(format!("-- Dimensions: {}", shown(&dimensions)));
    lines.push(format!("-- Metrics: {}", shown(&metrics)));
    if !facts.is_empty() {
        lines.push(format!("-- Facts: {}", shown(&facts)));
    }
    match mat_name {
        Some(ref n) => lines.push(format!("-- Materialization: {n}")),
        None => lines.push("-- Materialization: none".to_string()),
    }
    lines.push(String::new());
    lines.push("-- Expanded SQL:".to_string());
    // The predicate's parameters as written (`$1`, ...), not as the bind
    // finds them (`expand::where_clause::PARAM_FUNCTION`).
    for sql_line in crate::expand::show_params(&expanded_sql).lines() {
        lines.push(sql_line.to_string());
    }
    lines.push(String::new());
    lines.push("-- DuckDB Plan:".to_string());
    let explain_lines = collect_explain_lines(def, &expanded_sql);
    lines.extend(explain_lines);

    // Serialise as 1-column VARCHAR rows.
    let rows: Vec<Vec<String>> = lines.into_iter().map(|l| vec![l]).collect();
    serialize_varchar_rows(&rows)
}

// ---------------------------------------------------------------------------
// EXPLAIN plan extraction
// ---------------------------------------------------------------------------

/// Plan `sql` as `EXPLAIN` would and return the plan as lines of text: the
/// `physical_plan` key, then the rendered tree.
///
/// The plan is made in the caller's context and transaction (TECH-DEBT #19),
/// with the expansion's names resolved as in a view stored where `def` was
/// created, so it shows the plan `semantic_view()` gets: a table created or
/// altered earlier in the transaction, or a temporary one, is planned as it is
/// there.
///
/// If planning fails (e.g., referenced tables do not exist), returns a single
/// fallback line with the error message.
fn collect_explain_lines(def: &SemanticViewDefinition, sql: &str) -> Vec<String> {
    let (database, schema) = crate::expand::body_qualifier(def);
    match crate::ddl::read_ffi::caller_explain(database, schema, sql) {
        Ok(plan) => std::iter::once("physical_plan".to_string())
            .chain(plan.lines().map(ToString::to_string))
            .collect(),
        Err(err) => vec![format!("-- (not available -- {err})")],
    }
}
