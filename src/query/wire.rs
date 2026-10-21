//! Pure helpers for the read-path FFI wire format.
//!
//! These functions carry no `unsafe` FFI entrypoints and touch no
//! `duckdb_connection` — they are the byte-level encode/decode and SQL-shape
//! logic that the `#[cfg(feature = "extension")]` bind callbacks in
//! `table_function.rs` / `explain.rs` delegate to. They live in this
//! always-compiled module (not behind the `extension` gate) so they are
//! covered by the default `cargo test` / clippy / coverage runs — the FFI
//! entrypoints that call them cannot be, since the `extension` feature swaps in
//! loadable-extension stubs that abort at runtime (TC-8).
//!
//! Consolidating them here also collapses the two byte-identical copies of the
//! LIST(VARCHAR) argument decoder that previously lived independently in
//! `table_function.rs` (`sv_parse_string_list`) and `explain.rs`
//! (`parse_string_list`) — the "fix landed in one copy" hazard §5.1 calls out.

use crate::expand::quote_ident;
use crate::ffi_util::wire_len;
use libduckdb_sys as ffi;

/// Decode a length-prefixed LIST(VARCHAR) argument buffer into a `Vec<String>`.
///
/// Wire format (little-endian, produced by the C++ `sv_serialize_varchar_list`
/// side of the bridge):
///
/// ```text
/// u32 count
/// for each element: u32 byte_len | byte_len UTF-8 bytes
/// ```
///
/// Returns `Err(diagnostic)` on truncation, overflow, or trailing bytes. The
/// C++ dispatcher surfaces the message as an rc=1 `BinderException`, so the
/// detail (`"expected u32 at offset N of M"`, `"element i declares length n but
/// only k bytes remain"`, `"trailing k bytes after count c"`) has to be
/// actionable for an FFI-shape regression — a flat "malformed payload" is not
/// (WR-05).
///
/// # Safety
///
/// `buf` must either be null (in which case `len` must be 0) or point to `len`
/// readable bytes. A `(null, len > 0)` call is rejected explicitly rather than
/// forming a `from_raw_parts(null, len)` slice, which is UB.
pub unsafe fn parse_varchar_list(buf: *const u8, len: usize) -> Result<Vec<String>, String> {
    if buf.is_null() {
        return if len == 0 {
            Ok(Vec::new())
        } else {
            Err(format!("null buffer but len={len} (FFI shape drift)"))
        };
    }
    if len < 4 {
        return Err(format!(
            "buffer too short for count prefix: len={len} (expected >= 4)"
        ));
    }
    let slice = std::slice::from_raw_parts(buf, len);
    decode_varchar_list(slice)
}

/// Safe core of [`parse_varchar_list`], operating on an already-formed byte
/// slice. Split out so it can be exercised directly by unit tests without
/// constructing raw pointers.
fn decode_varchar_list(slice: &[u8]) -> Result<Vec<String>, String> {
    let len = slice.len();
    if len < 4 {
        return Err(format!(
            "buffer too short for count prefix: len={len} (expected >= 4)"
        ));
    }
    let mut off = 0usize;
    let read_u32 = |slice: &[u8], off: &mut usize| -> Result<u32, String> {
        if *off + 4 > slice.len() {
            return Err(format!(
                "expected u32 at offset {} of {} (truncated)",
                *off,
                slice.len()
            ));
        }
        let v = u32::from_le_bytes(slice[*off..*off + 4].try_into().map_err(
            |e: std::array::TryFromSliceError| format!("u32 decode failed at offset {}: {e}", *off),
        )?);
        *off += 4;
        Ok(v)
    };
    let count = read_u32(slice, &mut off)? as usize;
    // FF-6: cap the pre-allocation at the largest element count the buffer
    // could actually hold. The 4-byte count prefix has already been consumed,
    // and each remaining element carries at least a 4-byte length prefix, so
    // the ceiling is `(len - 4) / 4`. A corrupt `count` near u32::MAX would
    // otherwise request a ~100 GB allocation up front; the per-element bounds
    // check below still rejects a genuinely truncated payload.
    let mut out = Vec::with_capacity(count.min(len.saturating_sub(4) / 4));
    for i in 0..count {
        let n = read_u32(slice, &mut off)
            .map_err(|e| format!("reading length for element {i} of {count}: {e}"))?
            as usize;
        if off + n > slice.len() {
            return Err(format!(
                "element {i} of {count} declares length {n} but only {} bytes remain at offset {off}",
                slice.len().saturating_sub(off)
            ));
        }
        out.push(String::from_utf8_lossy(&slice[off..off + n]).into_owned());
        off += n;
    }
    if off != len {
        return Err(format!(
            "trailing {} bytes after count {count} (consumed {off} of {len})",
            len - off
        ));
    }
    Ok(out)
}

/// Map a `DuckDB` `type_id` to the SQL type name used to wrap an output column
/// in an explicit cast, or `None` when the column must pass through uncast.
///
/// This is the cast map that guards the vector-reference type contract: the
/// execution SQL declares each column's type so the runtime vector always
/// matches the bind-time schema. HUGEINT/UHUGEINT are down-cast to
/// BIGINT/UBIGINT (the optimizer substitution `DuckDB` performs anyway); complex
/// types declared as VARCHAR at bind (STRUCT/MAP/INVALID) cast to VARCHAR;
/// DECIMAL and LIST pass through (`None`) because a bare type name would drop
/// precision/scale or the child type.
///
/// The arms are kept one-per-type (rather than collapsing the several that all
/// yield `Some("VARCHAR")` / `None`) so the map reads as an explicit,
/// self-documenting type contract — hence the `match_same_arms` allow.
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn type_id_to_cast_sql(type_id: u32) -> Option<&'static str> {
    use ffi::{
        DUCKDB_TYPE_DUCKDB_TYPE_BIGINT as BIGINT, DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN as BOOLEAN,
        DUCKDB_TYPE_DUCKDB_TYPE_DATE as DATE, DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL as DECIMAL,
        DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE as DOUBLE, DUCKDB_TYPE_DUCKDB_TYPE_FLOAT as FLOAT,
        DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT as HUGEINT, DUCKDB_TYPE_DUCKDB_TYPE_INTEGER as INTEGER,
        DUCKDB_TYPE_DUCKDB_TYPE_INVALID as INVALID, DUCKDB_TYPE_DUCKDB_TYPE_MAP as MAP,
        DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT as SMALLINT, DUCKDB_TYPE_DUCKDB_TYPE_STRUCT as STRUCT,
        DUCKDB_TYPE_DUCKDB_TYPE_TIME as TIME, DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP as TIMESTAMP,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_MS as TIMESTAMP_MS,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS as TIMESTAMP_NS,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_S as TIMESTAMP_S,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ as TIMESTAMP_TZ,
        DUCKDB_TYPE_DUCKDB_TYPE_TINYINT as TINYINT, DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT as UBIGINT,
        DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT as UHUGEINT, DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER as UINTEGER,
        DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT as USMALLINT,
        DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT as UTINYINT, DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR as VARCHAR,
    };

    match type_id {
        BOOLEAN => Some("BOOLEAN"),
        TINYINT => Some("TINYINT"),
        SMALLINT => Some("SMALLINT"),
        INTEGER => Some("INTEGER"),
        BIGINT | HUGEINT => Some("BIGINT"),
        UTINYINT => Some("UTINYINT"),
        USMALLINT => Some("USMALLINT"),
        UINTEGER => Some("UINTEGER"),
        UBIGINT | UHUGEINT => Some("UBIGINT"),
        FLOAT => Some("FLOAT"),
        DOUBLE => Some("DOUBLE"),
        DATE => Some("DATE"),
        TIME => Some("TIME"),
        TIMESTAMP => Some("TIMESTAMP"),
        TIMESTAMP_S => Some("TIMESTAMP_S"),
        TIMESTAMP_MS => Some("TIMESTAMP_MS"),
        TIMESTAMP_NS => Some("TIMESTAMP_NS"),
        TIMESTAMP_TZ => Some("TIMESTAMPTZ"),
        VARCHAR => Some("VARCHAR"),
        STRUCT | MAP | INVALID => Some("VARCHAR"),
        // DECIMAL and LIST columns cannot be cast via bare SQL type name --
        // DECIMAL requires precision/scale (bare "DECIMAL" defaults to (18,3)
        // which changes the value), LIST requires child type. These types are
        // handled via logical type metadata at bind time, so pass through
        // unmodified in the execution SQL wrapper.
        DECIMAL => None,
        // Unknown types: pass through rather than risk a lossy VARCHAR cast.
        // The runtime type check in func() will catch any real mismatch.
        _ => None,
    }
}

/// The type name `semantic_view()` declares for an output column whose probed
/// C-API type id is `type_id`, spelled as `DuckDB` prints it (what `typeof()`
/// returns for that column), or `None` for DECIMAL and LIST.
///
/// This mirrors `sv_logical_type_from_c_type_id` in `cpp/src/shim.cpp`, the map
/// the C++ bind uses to declare `semantic_view()`'s output schema: HUGEINT /
/// UHUGEINT are declared BIGINT / UBIGINT, ENUM / STRUCT / MAP and any id the
/// map does not know are declared VARCHAR. DECIMAL and LIST are declared from a
/// logical-type probe instead (width/scale, child type), so their name cannot
/// come from the id alone — the caller reads it from the probed type.
///
/// It exists for the read-side member-type probe (TECH-DEBT #51): SHOW /
/// DESCRIBE report the type `semantic_view()` returns, not the raw type of the
/// expanded SQL, so a member's `data_type` always equals `typeof()` of its
/// column. A change to the C++ map must be mirrored here;
/// `member_types::tests::declared_type_names_match_typeof` and
/// `test/sql/member_data_type.test` pin the agreement.
#[must_use]
#[allow(clippy::match_same_arms)]
pub fn declared_type_name(type_id: u32) -> Option<&'static str> {
    use ffi::{
        DUCKDB_TYPE_DUCKDB_TYPE_BIGINT as BIGINT, DUCKDB_TYPE_DUCKDB_TYPE_BIT as BIT,
        DUCKDB_TYPE_DUCKDB_TYPE_BLOB as BLOB, DUCKDB_TYPE_DUCKDB_TYPE_BOOLEAN as BOOLEAN,
        DUCKDB_TYPE_DUCKDB_TYPE_DATE as DATE, DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL as DECIMAL,
        DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE as DOUBLE, DUCKDB_TYPE_DUCKDB_TYPE_FLOAT as FLOAT,
        DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT as HUGEINT, DUCKDB_TYPE_DUCKDB_TYPE_INTEGER as INTEGER,
        DUCKDB_TYPE_DUCKDB_TYPE_INTERVAL as INTERVAL, DUCKDB_TYPE_DUCKDB_TYPE_LIST as LIST,
        DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT as SMALLINT, DUCKDB_TYPE_DUCKDB_TYPE_TIME as TIME,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP as TIMESTAMP,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_MS as TIMESTAMP_MS,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_NS as TIMESTAMP_NS,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_S as TIMESTAMP_S,
        DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ as TIMESTAMP_TZ,
        DUCKDB_TYPE_DUCKDB_TYPE_TIME_TZ as TIME_TZ, DUCKDB_TYPE_DUCKDB_TYPE_TINYINT as TINYINT,
        DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT as UBIGINT, DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT as UHUGEINT,
        DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER as UINTEGER,
        DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT as USMALLINT,
        DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT as UTINYINT, DUCKDB_TYPE_DUCKDB_TYPE_UUID as UUID,
        DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR as VARCHAR,
    };

    match type_id {
        BOOLEAN => Some("BOOLEAN"),
        TINYINT => Some("TINYINT"),
        SMALLINT => Some("SMALLINT"),
        INTEGER => Some("INTEGER"),
        BIGINT | HUGEINT => Some("BIGINT"),
        UTINYINT => Some("UTINYINT"),
        USMALLINT => Some("USMALLINT"),
        UINTEGER => Some("UINTEGER"),
        UBIGINT | UHUGEINT => Some("UBIGINT"),
        FLOAT => Some("FLOAT"),
        DOUBLE => Some("DOUBLE"),
        TIMESTAMP => Some("TIMESTAMP"),
        DATE => Some("DATE"),
        TIME => Some("TIME"),
        INTERVAL => Some("INTERVAL"),
        VARCHAR => Some("VARCHAR"),
        BLOB => Some("BLOB"),
        TIMESTAMP_S => Some("TIMESTAMP_S"),
        TIMESTAMP_MS => Some("TIMESTAMP_MS"),
        TIMESTAMP_NS => Some("TIMESTAMP_NS"),
        TIMESTAMP_TZ => Some("TIMESTAMP WITH TIME ZONE"),
        TIME_TZ => Some("TIME WITH TIME ZONE"),
        UUID => Some("UUID"),
        BIT => Some("BIT"),
        // Declared from the probed logical type (width/scale, child type).
        DECIMAL | LIST => None,
        // ENUM, STRUCT, MAP, INVALID and every id the C++ map does not list
        // (ARRAY, UNION, ...) are declared VARCHAR.
        _ => Some("VARCHAR"),
    }
}

/// Build the SQL used at execution time, wrapping the expanded SQL with explicit
/// type casts for EVERY output column.
///
/// This ensures runtime column types always match the bind-time schema
/// declaration, preventing type mismatches in `duckdb_vector_reference_vector`.
/// `DuckDB` optimizes away no-op casts (e.g., `col::BIGINT` when `col` is
/// already BIGINT), so the wrapper has negligible performance overhead.
///
/// Column names are quoted via [`quote_ident`] (FF-8): an inferred name
/// containing `"` would otherwise break the cast wrapper and mis-alias the
/// column.
#[must_use]
pub fn build_execution_sql(
    expanded_sql: &str,
    column_names: &[String],
    column_type_ids: &[u32],
) -> String {
    // If there are no columns, return the original SQL (edge case).
    if column_names.is_empty() {
        return expanded_sql.to_string();
    }

    let clauses: Vec<String> = column_names
        .iter()
        .zip(column_type_ids.iter())
        .map(|(name, &tid)| {
            let quoted = quote_ident(name);
            match type_id_to_cast_sql(tid) {
                Some(cast_type) => format!("{quoted}::{cast_type} AS {quoted}"),
                None => quoted,
            }
        })
        .collect();

    format!(
        "SELECT {} FROM ({expanded_sql}) __sv_inner",
        clauses.join(", ")
    )
}

/// [`build_execution_sql`] for a request whose columns are renamed or
/// reordered ([`OutputColumns`](super::items::OutputColumns)): the execution
/// SQL, and the output's column names and type ids in output order.
///
/// When `output` is the expansion's own layout the SQL is
/// [`build_execution_sql`]'s, byte for byte.
#[must_use]
pub fn build_projected_execution_sql(
    expanded_sql: &str,
    column_names: &[String],
    column_type_ids: &[u32],
    output: &super::items::OutputColumns,
) -> (Vec<String>, Vec<u32>, String) {
    if output.is_identity(column_names) {
        return (
            column_names.to_vec(),
            column_type_ids.to_vec(),
            build_execution_sql(expanded_sql, column_names, column_type_ids),
        );
    }
    let mut names = Vec::with_capacity(output.columns.len());
    let mut type_ids = Vec::with_capacity(output.columns.len());
    let clauses: Vec<String> = output
        .columns
        .iter()
        .map(|(src, name)| {
            let tid = column_type_ids[*src];
            names.push(name.clone());
            type_ids.push(tid);
            let source = quote_ident(&column_names[*src]);
            let target = quote_ident(name);
            match type_id_to_cast_sql(tid) {
                Some(cast_type) => format!("{source}::{cast_type} AS {target}"),
                None => format!("{source} AS {target}"),
            }
        })
        .collect();
    let sql = format!(
        "SELECT {} FROM ({expanded_sql}) __sv_inner",
        clauses.join(", ")
    );
    (names, type_ids, sql)
}

/// Serialize the inferred schema + execution SQL into the flat register wire
/// format consumed by the C++ `semantic_view` bind:
///
/// ```text
/// u32 n_cols
/// for each col: u32 name_len | name bytes | u32 type_id
/// u32 sql_len | sql bytes
/// ```
///
/// FF-6: every length goes through a checked `u32::try_from` and the function
/// returns an error rather than a bare `as u32` truncation, which would write a
/// length prefix that disagrees with the bytes appended and desync the header
/// from the payload on the C++ read side. Overflow is unreachable for real
/// queries (a column name or the execution SQL would each need to exceed 4 GiB).
pub fn serialize_register_payload(
    column_names: &[String],
    column_type_ids: &[u32],
    execution_sql: &str,
) -> Result<Vec<u8>, String> {
    // Guard against slice desync: the header writes `n_cols` from
    // `column_names.len()`, but the body serializes via `zip`, which would
    // silently truncate to the shorter slice and emit a header that disagrees
    // with the payload. Today both vectors come from the same
    // `duckdb_column_count` loop, but reject mismatch explicitly so a future
    // caller cannot desync the wire format.
    if column_names.len() != column_type_ids.len() {
        return Err(format!(
            "column name count ({}) disagrees with type id count ({})",
            column_names.len(),
            column_type_ids.len()
        ));
    }
    let n_cols = wire_len(column_names.len(), "column count")?;
    let cap = 4
        + column_names.iter().map(|n| 4 + n.len()).sum::<usize>()
        + column_type_ids.len() * 4
        + 4
        + execution_sql.len();
    let mut buf: Vec<u8> = Vec::with_capacity(cap);
    buf.extend_from_slice(&n_cols.to_le_bytes());
    for (name, tid) in column_names.iter().zip(column_type_ids.iter()) {
        let nl = wire_len(name.len(), "column name")?;
        buf.extend_from_slice(&nl.to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(&tid.to_le_bytes());
    }
    let sql_len = wire_len(execution_sql.len(), "execution SQL")?;
    buf.extend_from_slice(&sql_len.to_le_bytes());
    buf.extend_from_slice(execution_sql.as_bytes());
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- helpers ----------------------------------------------------------

    /// Encode a LIST(VARCHAR) buffer in the wire format `parse_varchar_list`
    /// decodes, so round-trip properties can be asserted directly.
    fn encode_varchar_list(items: &[&str]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&(u32::try_from(items.len()).unwrap()).to_le_bytes());
        for s in items {
            buf.extend_from_slice(&(u32::try_from(s.len()).unwrap()).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        buf
    }

    /// Decode the register payload produced by `serialize_register_payload`
    /// back into its parts, mirroring the C++ bind read side, so the encoder
    /// can be checked by a symmetric decoder rather than magic byte offsets.
    fn decode_register_payload(buf: &[u8]) -> (Vec<String>, Vec<u32>, String) {
        let mut off = 0usize;
        let rd_u32 = |buf: &[u8], off: &mut usize| {
            let v = u32::from_le_bytes(buf[*off..*off + 4].try_into().unwrap());
            *off += 4;
            v
        };
        let n = rd_u32(buf, &mut off) as usize;
        let mut names = Vec::new();
        let mut tids = Vec::new();
        for _ in 0..n {
            let nl = rd_u32(buf, &mut off) as usize;
            names.push(String::from_utf8(buf[off..off + nl].to_vec()).unwrap());
            off += nl;
            tids.push(rd_u32(buf, &mut off));
        }
        let sl = rd_u32(buf, &mut off) as usize;
        let sql = String::from_utf8(buf[off..off + sl].to_vec()).unwrap();
        off += sl;
        assert_eq!(off, buf.len(), "decoder must consume the whole payload");
        (names, tids, sql)
    }

    // -- parse_varchar_list ----------------------------------------------

    #[test]
    fn parse_varchar_list_roundtrips_via_decode() {
        for items in [
            vec![],
            vec!["region"],
            vec!["region", "month", "status"],
            vec!["", "non-empty", ""],      // empty elements
            vec!["café", "São Paulo", "Ω"], // multi-byte UTF-8
            vec!["a,b", "c\"d", "e__f"],    // delimiter-bearing names
        ] {
            let buf = encode_varchar_list(&items);
            let decoded = decode_varchar_list(&buf).expect("well-formed buffer decodes");
            assert_eq!(decoded, items, "round-trip mismatch for {items:?}");
        }
    }

    #[test]
    fn parse_varchar_list_empty_buffer_is_empty_vec() {
        // count=0 is the smallest legal buffer.
        assert_eq!(
            decode_varchar_list(&0u32.to_le_bytes()).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn parse_varchar_list_rejects_short_count_prefix() {
        let err = decode_varchar_list(&[0u8, 0, 0]).unwrap_err();
        assert!(err.contains("too short for count prefix"), "got: {err}");
    }

    #[test]
    fn parse_varchar_list_rejects_truncated_element_length() {
        // count=1 but no room for the element's 4-byte length prefix.
        let mut buf = 1u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0, 0]); // only 2 of 4 length bytes
        let err = decode_varchar_list(&buf).unwrap_err();
        assert!(err.contains("reading length for element 0"), "got: {err}");
        assert!(err.contains("truncated"), "got: {err}");
    }

    #[test]
    fn parse_varchar_list_rejects_element_overrunning_buffer() {
        // count=1, element declares length 100 but only a few bytes remain.
        let mut buf = 1u32.to_le_bytes().to_vec();
        buf.extend_from_slice(&100u32.to_le_bytes());
        buf.extend_from_slice(b"abc");
        let err = decode_varchar_list(&buf).unwrap_err();
        assert!(err.contains("declares length 100"), "got: {err}");
        assert!(err.contains("bytes remain"), "got: {err}");
    }

    #[test]
    fn parse_varchar_list_rejects_trailing_bytes() {
        let mut buf = encode_varchar_list(&["a"]);
        buf.extend_from_slice(b"garbage");
        let err = decode_varchar_list(&buf).unwrap_err();
        assert!(err.contains("trailing"), "got: {err}");
    }

    #[test]
    fn parse_varchar_list_does_not_overallocate_on_corrupt_count() {
        // count = u32::MAX but a tiny buffer — the with_capacity ceiling must
        // clamp to (len-4)/4 so this returns an error instead of OOM-ing.
        let mut buf = u32::MAX.to_le_bytes().to_vec();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let err = decode_varchar_list(&buf).unwrap_err();
        assert!(err.contains("reading length for element"), "got: {err}");
    }

    #[test]
    fn parse_varchar_list_null_buffer() {
        // (null, 0) is legal empty; (null, >0) is FFI shape drift.
        unsafe {
            assert_eq!(
                parse_varchar_list(std::ptr::null(), 0).unwrap(),
                Vec::<String>::new()
            );
            assert!(parse_varchar_list(std::ptr::null(), 8).is_err());
        }
    }

    // -- type_id_to_cast_sql ---------------------------------------------

    #[test]
    fn cast_sql_hugeint_downcasts_to_bigint() {
        assert_eq!(
            type_id_to_cast_sql(ffi::DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT),
            Some("BIGINT")
        );
        assert_eq!(
            type_id_to_cast_sql(ffi::DUCKDB_TYPE_DUCKDB_TYPE_UHUGEINT),
            Some("UBIGINT")
        );
    }

    #[test]
    fn cast_sql_complex_types_declared_varchar() {
        for t in [
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_STRUCT,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_MAP,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_INVALID,
        ] {
            assert_eq!(type_id_to_cast_sql(t), Some("VARCHAR"));
        }
    }

    #[test]
    fn cast_sql_decimal_and_list_pass_through() {
        // Bare DECIMAL/LIST casts are lossy, so these must be None.
        assert_eq!(
            type_id_to_cast_sql(ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL),
            None
        );
        assert_eq!(type_id_to_cast_sql(ffi::DUCKDB_TYPE_DUCKDB_TYPE_LIST), None);
    }

    #[test]
    fn cast_sql_timestamp_tz_uses_short_name() {
        assert_eq!(
            type_id_to_cast_sql(ffi::DUCKDB_TYPE_DUCKDB_TYPE_TIMESTAMP_TZ),
            Some("TIMESTAMPTZ")
        );
    }

    #[test]
    fn cast_sql_unknown_type_passes_through() {
        // An out-of-range type id must pass through (None), not fabricate a cast.
        assert_eq!(type_id_to_cast_sql(9999), None);
    }

    // -- build_execution_sql ---------------------------------------------

    #[test]
    fn build_execution_sql_no_columns_returns_input() {
        assert_eq!(build_execution_sql("SELECT 1", &[], &[]), "SELECT 1");
    }

    #[test]
    fn build_execution_sql_wraps_with_casts_and_passthrough() {
        let names = vec!["region".to_string(), "amt".to_string()];
        let tids = vec![
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL, // pass-through, no cast
        ];
        let out = build_execution_sql("SELECT region, amt FROM t", &names, &tids);
        assert_eq!(
            out,
            "SELECT \"region\"::VARCHAR AS \"region\", \"amt\" FROM (SELECT region, amt FROM t) __sv_inner"
        );
    }

    #[test]
    fn a_projected_identity_is_the_plain_execution_sql() {
        use super::super::items::{Group, OutputColumns};
        let names = vec!["region".to_string(), "amt".to_string()];
        let tids = vec![
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL,
        ];
        let out =
            OutputColumns::new(&names, [&[None], &[None], &[]], Group::DEFAULT_ORDER).unwrap();
        let (n, t, sql) = build_projected_execution_sql("SELECT 1", &names, &tids, &out);
        assert_eq!(sql, build_execution_sql("SELECT 1", &names, &tids));
        assert_eq!((n, t), (names, tids));
    }

    #[test]
    fn a_projection_renames_and_reorders_the_columns() {
        use super::super::items::{Group, OutputColumns};
        let names = vec!["region".to_string(), "amt".to_string()];
        let tids = vec![
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_DECIMAL,
        ];
        let order = [Group::Metrics, Group::Dimensions, Group::Facts];
        let out =
            OutputColumns::new(&names, [&[Some("R".to_string())], &[None], &[]], order).unwrap();
        let (n, t, sql) = build_projected_execution_sql("SELECT 1", &names, &tids, &out);
        assert_eq!(n, vec!["amt".to_string(), "R".to_string()]);
        assert_eq!(t, vec![tids[1], tids[0]]);
        assert_eq!(
            sql,
            "SELECT \"amt\" AS \"amt\", \"region\"::VARCHAR AS \"R\" FROM (SELECT 1) __sv_inner"
        );
    }

    #[test]
    fn build_execution_sql_quotes_embedded_double_quote() {
        // FF-8: a column name containing `"` must be escaped, not raw-formatted.
        let names = vec!["we\"ird".to_string()];
        let tids = vec![ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER];
        let out = build_execution_sql("SELECT 1", &names, &tids);
        assert!(
            out.contains("\"we\"\"ird\"::INTEGER AS \"we\"\"ird\""),
            "embedded quote must be doubled: {out}"
        );
    }

    // -- serialize_register_payload --------------------------------------

    #[test]
    fn serialize_register_payload_roundtrips() {
        let names = vec!["region".to_string(), "café".to_string()];
        let tids = vec![
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR,
            ffi::DUCKDB_TYPE_DUCKDB_TYPE_BIGINT,
        ];
        let sql = "SELECT * FROM t";
        let buf = serialize_register_payload(&names, &tids, sql).unwrap();
        let (dn, dt, ds) = decode_register_payload(&buf);
        assert_eq!(dn, names);
        assert_eq!(dt, tids);
        assert_eq!(ds, sql);
    }

    #[test]
    fn serialize_register_payload_empty_columns() {
        let buf = serialize_register_payload(&[], &[], "SELECT 1").unwrap();
        let (dn, dt, ds) = decode_register_payload(&buf);
        assert!(dn.is_empty());
        assert!(dt.is_empty());
        assert_eq!(ds, "SELECT 1");
    }

    #[test]
    fn serialize_register_payload_rejects_slice_desync() {
        // Header would claim 2 columns but only 1 type id is present.
        let err = serialize_register_payload(
            &["a".to_string(), "b".to_string()],
            &[ffi::DUCKDB_TYPE_DUCKDB_TYPE_INTEGER],
            "SELECT 1",
        )
        .unwrap_err();
        assert!(err.contains("disagrees with type id count"), "got: {err}");
    }
}
