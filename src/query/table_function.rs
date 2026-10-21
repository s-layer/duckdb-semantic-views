use libduckdb_sys as ffi;

use crate::catalog::CatalogReader;
use crate::expand::wildcard::{expand_wildcards, WildcardItemType};
use crate::expand::{expand, QueryRequest};
use crate::model::SemanticViewDefinition;
use crate::util::suggest_closest;

use super::error::QueryError;
use super::items::{aliases_for, Item, OutputColumns, RequestItems};
use super::wire::{build_projected_execution_sql, parse_varchar_list, serialize_register_payload};

// The raw C-API helpers moved to the always-compiled `raw_sql` module so the
// read-side member-type probe (TECH-DEBT #51) can share them; re-exported here
// for their existing callers (`explain.rs`).
pub(crate) use super::raw_sql::{execute_sql_raw, read_varchar_from_vector};

// ---------------------------------------------------------------------------
// Phase 65 Plan 05 Task 6 (Wave 6) — sv_semantic_view_bind_rust
// ---------------------------------------------------------------------------
//
// FFI dispatcher for the migrated
// `semantic_view(view_name, dimensions := [...], metrics := [...], facts := [...])`
// table function. The C++ bind callback (`sv_semantic_view_bind` in
// `cpp/src/shim.cpp`) opens a per-call `Connection probe(*context.db)`,
// flattens the three optional LIST(VARCHAR) named parameters into the
// length-prefixed wire format (same encoding as the Wave 5 explain
// migration), and invokes this dispatcher. Same `reinterpret_cast` bridge
// + BORROW contract as the 15 prior migrations.
//
// Responsibilities of the Rust side:
//   - Catalog lookup (name normalisation, view-not-found suggestion).
//   - Wildcard expansion + `QueryRequest` construction.
//   - `expand::expand()` → expanded SQL.
//   - Column-type inference at read-side bind time per D-16/D-17: a single
//     unconditional LIMIT 0 probe on the per-call conn, for every query shape.
//     AR-4 (PR-2) removed the DDL-time persisted-types fast path this comment
//     used to describe — `column_type_names` / `column_types_inferred` are gone
//     from the model (src/model.rs) and legacy rows carrying them fall through
//     to the same probe. A failed probe is an error, not a VARCHAR fallback
//     (WR-08 / D-15).
//   - `build_execution_sql()` wrapping for HUGEINT→BIGINT casts etc.
//
// The dispatcher returns a flat binary buffer to the C++ side encoding
// the schema + execution_sql for bind to declare output columns and the
// init_global callback to run the query. Wire format:
//
//   u32 n_cols (little-endian)
//   for each col:
//     u32 byte_len + bytes (column name, UTF-8)
//     u32 type_id (little-endian; already passed through normalize_type_id)
//   u32 byte_len + bytes (execution_sql, UTF-8)
//
// Return codes mirror the Wave 5 dispatcher:
//   0 — success; (out_ptr, out_len) populated.
//   1 — user-visible error; error_buf populated (raised as BinderException).
//   2 — internal error (panic across FFI); error_buf populated.

/// # Safety
///
/// `conn` is a borrowed handle (do NOT disconnect). Wire-format payloads
/// described above; `name_ptr` must point to `name_len` UTF-8 bytes.
#[cfg(feature = "extension")]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn sv_semantic_view_bind_rust(
    conn: ffi::duckdb_connection,
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
    order_ptr: *const u8,
    order_len: usize,
    out_ptr: *mut *mut u8,
    out_len: *mut usize,
    error_buf: *mut u8,
    error_buf_len: usize,
) -> u8 {
    // R-6/C-3: the catch_unwind guard, borrowed-connection null check, buffer
    // publish, error-string write, and panic arm now live in the shared
    // `run_dispatcher` scaffold (ST-2); the body does only the interesting work
    // and returns `Result<Vec<u8>, String>`.
    crate::ddl::read_ffi::run_dispatcher(
        conn,
        out_ptr,
        out_len,
        error_buf,
        error_buf_len,
        "sv_semantic_view_bind_rust",
        |borrowed| unsafe {
            semantic_view_bind_body(
                borrowed,
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
                order_ptr,
                order_len,
            )
        },
    )
}

/// Body for [`sv_semantic_view_bind_rust`]: decode the request args, resolve
/// the view, expand + type-infer, and serialize the register payload. Returns
/// the wire buffer on success or the user-visible error message.
///
/// # Safety
///
/// Each `*_ptr` is either null or points to its paired `*_len` readable bytes.
/// A null pointer is only meaningful with `*_len == 0` (an empty list / absent
/// name); the decoders reject `(null, len > 0)` as FFI shape drift rather than
/// dereferencing it. The borrowed connection must outlive the call (see the
/// module borrow contract).
#[cfg(feature = "extension")]
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
unsafe fn semantic_view_bind_body(
    borrowed: &crate::ddl::read_ffi::BorrowedConnection,
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
    order_ptr: *const u8,
    order_len: usize,
) -> Result<Vec<u8>, String> {
    use crate::ddl::read_ffi::{probe_catalog_table_present, read_str_arg};

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
    let present = probe_catalog_table_present(borrowed)?;
    let reader = CatalogReader::new(borrowed, present);
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

    let invalid = |detail: String| {
        QueryError::InvalidRequest {
            view_name: view_name.clone(),
            detail,
        }
        .to_string()
    };
    let items =
        RequestItems::parse(&dimensions, &metrics, &facts, &column_order).map_err(invalid)?;
    let expand_list = |list: &[Item], kind: &WildcardItemType| {
        expand_wildcards(&RequestItems::members(list), &def, kind).map_err(|e| {
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
    let expanded_sql =
        expand(&view_name, &def, &req).map_err(|e| QueryError::from(e).to_string())?;

    // Type inference: a LIMIT-0 probe on the per-call connection yields
    // the output column names + types. The probe runs on `conn`, not a
    // long-lived handle (H2). AR-4 (PR-2) removed the DDL-time
    // persisted-types fast path (`column_type_names` / `column_types_inferred`
    // were dead for post-v0.10 rows) — every row now infers at read time,
    // matching Plan 03 D-16, so this is a single unconditional probe.
    let (column_names, column_type_ids): (Vec<String>, Vec<u32>) = {
        let limit0_sql = format!("{expanded_sql} LIMIT 0");
        // Phase 65.1 Plan 11 / WR-08 / D-15: surface probe failures via
        // the error message. No silent vec![0u32; names.len()] fallback to
        // DUCKDB_TYPE_INVALID — that masked broken FACTS expressions behind a
        // VARCHAR placeholder at query time.
        let (names, types) = try_infer_schema(borrowed, &limit0_sql).map_err(|msg| {
            format!(
                "semantic_view: type inference failed for query \
                 `{limit0_sql}`: {msg}"
            )
        })?;
        let type_ids: Vec<u32> = types.iter().map(|t| normalize_type_id(*t)).collect();
        (names, type_ids)
    };

    // Lay the columns out as the items ask (aliases, column_order), and build
    // the execution SQL with casts where needed (HUGEINT→BIGINT etc).
    let output = OutputColumns::new(
        &column_names,
        [&dimension_aliases, &metric_aliases, &fact_aliases],
        items.order,
    )
    .map_err(invalid)?;
    let (column_names, column_type_ids, execution_sql) =
        build_projected_execution_sql(&expanded_sql, &column_names, &column_type_ids, &output);

    // Serialise schema + execution_sql into a flat binary buffer.
    serialize_register_payload(&column_names, &column_type_ids, &execution_sql)
}

// ---------------------------------------------------------------------------
// FFI helpers
// ---------------------------------------------------------------------------

/// Normalize HUGEINT/UHUGEINT type IDs to BIGINT/UBIGINT.
///
/// `DuckDB`'s query planner infers `sum()` as HUGEINT at LIMIT-0 time, but the
/// runtime optimizer may substitute `sum_no_overflow` → BIGINT (8 bytes/value).
/// Normalizing stored type IDs prevents stale HUGEINT values from propagating
/// into output declarations.
pub(crate) fn normalize_type_id(t: u32) -> u32 {
    const HUGEINT: u32 = ffi::DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT;
    const UHUGEINT: u32 = ffi::DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT;
    const BIGINT: u32 = ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT;
    const UBIGINT: u32 = ffi::DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT;
    match t {
        HUGEINT => BIGINT,
        UHUGEINT => UBIGINT,
        _ => t,
    }
}

// ---------------------------------------------------------------------------
// Schema inference
// ---------------------------------------------------------------------------

/// Attempt to infer column names and types by executing a LIMIT 0 query.
///
/// Returns `Err(msg)` with the underlying `execute_sql_raw` error text when
/// the LIMIT 0 probe fails. Phase 65.1 Plan 11 / WR-08 / D-15: callers
/// surface the error via `write_err` + return rc=1, which the C++ binder
/// re-raises as `BinderException`. The previous `Option` return type
/// swallowed the diagnostic via `.ok()?`, masking broken DDL behind a
/// silent VARCHAR placeholder for `DUCKDB_TYPE_INVALID`.
///
/// # Safety
///
/// The underlying `duckdb_connection` accessed via `borrowed.as_raw()` must
/// be valid for the lifetime of the borrow.
pub(crate) unsafe fn try_infer_schema(
    borrowed: &crate::ddl::read_ffi::BorrowedConnection,
    sql: &str,
) -> Result<(Vec<String>, Vec<ffi::duckdb_type>), String> {
    super::raw_sql::infer_schema_raw(borrowed.as_raw(), sql).map_err(String::from)
}
