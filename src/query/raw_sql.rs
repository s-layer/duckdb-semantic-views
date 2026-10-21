//! Raw `DuckDB` C-API query helpers for the member-type probe.
//!
//! These run SQL on a `duckdb_connection` the caller holds and read the result
//! through the C API directly. The extension types members in the caller's own
//! context instead (`read_ffi::caller_describe`, TECH-DEBT #19), which reports
//! what `DESCRIBE` reports; these give `cargo test` the same probe against the
//! bundled `DuckDB`, which drives the member-type tests and proptests.

use std::ffi::{CStr, CString};

use libduckdb_sys as ffi;

/// A failed statement: `DuckDB`'s error text, and whether `DuckDB` classified
/// the failure as an interrupt (`DUCKDB_ERROR_INTERRUPT`), so a caller that
/// otherwise tolerates failures can still tell a cancellation apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RawSqlError {
    pub message: String,
    pub interrupted: bool,
}

impl From<RawSqlError> for String {
    fn from(e: RawSqlError) -> Self {
        e.message
    }
}

/// Execute a SQL string via the `DuckDB` C API and return the result, keeping
/// the failure's error class. The caller is responsible for calling
/// `duckdb_destroy_result` on the returned result when done.
///
/// # Safety
///
/// `conn` must be a valid, non-null `duckdb_connection` handle.
pub(crate) unsafe fn execute_sql_checked(
    conn: ffi::duckdb_connection,
    sql: &str,
) -> Result<ffi::duckdb_result, RawSqlError> {
    let sql_cstr = CString::new(sql).map_err(|e| RawSqlError {
        message: e.to_string(),
        interrupted: false,
    })?;
    let mut result: ffi::duckdb_result = std::mem::zeroed();
    let rc = ffi::duckdb_query(conn, sql_cstr.as_ptr(), &raw mut result);
    if rc != ffi::DuckDBSuccess {
        let err_ptr = ffi::duckdb_result_error(&raw mut result);
        let message = if err_ptr.is_null() {
            "unknown error".to_string()
        } else {
            CStr::from_ptr(err_ptr).to_string_lossy().into_owned()
        };
        let interrupted = ffi::duckdb_result_error_type(&raw mut result)
            == ffi::duckdb_error_type_DUCKDB_ERROR_INTERRUPT;
        ffi::duckdb_destroy_result(&raw mut result);
        return Err(RawSqlError {
            message,
            interrupted,
        });
    }
    Ok(result)
}

/// The name and full logical type of every output column of `sql`, the type in
/// `DuckDB`'s own spelling (`HUGEINT`, `DECIMAL(18,2)`, `VARCHAR[]`,
/// `STRUCT(a INTEGER)`), which is what `typeof()` reports for the column. Via
/// `DESCRIBE`, which binds the statement without running it.
///
/// # Safety
///
/// `conn` must be a valid, non-null `duckdb_connection` handle.
#[allow(clippy::cast_possible_truncation)]
pub(crate) unsafe fn describe_columns(
    conn: ffi::duckdb_connection,
    sql: &str,
) -> Result<Vec<(String, String)>, RawSqlError> {
    // `DESCRIBE <query>` output: column_name, column_type, null, key, default,
    // extra. Wrapping in a subquery keeps a leading `WITH` legal.
    let mut result = execute_sql_checked(
        conn,
        &format!("DESCRIBE SELECT * FROM ({sql}) __sv_describe"),
    )?;
    let mut out = Vec::new();
    let chunk_count = ffi::duckdb_result_chunk_count(result) as usize;
    for chunk_idx in 0..chunk_count {
        let chunk = ffi::duckdb_result_get_chunk(result, chunk_idx as ffi::idx_t);
        if chunk.is_null() {
            continue;
        }
        let row_count = ffi::duckdb_data_chunk_get_size(chunk) as usize;
        for row_idx in 0..row_count {
            out.push((
                read_varchar_from_vector(chunk, 0, row_idx),
                read_varchar_from_vector(chunk, 1, row_idx),
            ));
        }
        ffi::duckdb_destroy_data_chunk(&mut { chunk });
    }
    ffi::duckdb_destroy_result(&raw mut result);
    Ok(out)
}

/// Read a VARCHAR value from a data chunk vector at the given column and row.
///
/// The vector must contain VARCHAR (`duckdb_string_t`) data. Returns an empty
/// string for NULL values.
///
/// Decodes the `duckdb_string_t` layout directly from vector memory to avoid
/// reliance on C API helper functions that may not be available in loadable
/// extension mode.
///
/// # Safety
///
/// `chunk` must be a valid, non-null `duckdb_data_chunk` handle.
/// `col_idx` and `row_idx` must be within bounds.
#[allow(clippy::cast_possible_truncation)]
pub(crate) unsafe fn read_varchar_from_vector(
    chunk: ffi::duckdb_data_chunk,
    col_idx: usize,
    row_idx: usize,
) -> String {
    let vector = ffi::duckdb_data_chunk_get_vector(chunk, col_idx as ffi::idx_t);

    // Check for NULL using the validity mask.
    let validity = ffi::duckdb_vector_get_validity(vector);
    if !validity.is_null() {
        let entry_idx = row_idx / 64;
        let bit_idx = row_idx % 64;
        let entry = *validity.add(entry_idx);
        if entry & (1u64 << bit_idx) == 0 {
            return String::new(); // NULL
        }
    }

    // Read the duckdb_string_t from the vector data.
    // Layout is a 16-byte union:
    //   Inline  (len <= 12): { length: u32, inlined: [c_char; 12] }
    //   Pointer (len > 12):  { length: u32, prefix: [c_char; 4], ptr: *mut c_char }
    let data_ptr = ffi::duckdb_vector_get_data(vector);
    let string_t_ptr = data_ptr.cast::<ffi::duckdb_string_t>().add(row_idx);
    let string_t = &*string_t_ptr;

    // The length field is at the same offset for both union variants.
    let len = string_t.value.inlined.length as usize;
    if len == 0 {
        return String::new();
    }

    let bytes = if len <= 12 {
        // Inline: string data follows the length field directly.
        let inline_ptr = string_t.value.inlined.inlined.as_ptr().cast::<u8>();
        std::slice::from_raw_parts(inline_ptr, len)
    } else {
        // Pointer: data is at the heap-allocated ptr.
        let heap_ptr = string_t.value.pointer.ptr.cast::<u8>();
        if heap_ptr.is_null() {
            return String::new();
        }
        std::slice::from_raw_parts(heap_ptr, len)
    };

    String::from_utf8_lossy(bytes).into_owned()
}
