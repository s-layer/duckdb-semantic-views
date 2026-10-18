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

/// The SQL `semantic_view()` plans: the expansion, with its columns renamed
/// and reordered as `output` lays them out
/// ([`OutputColumns`](super::items::OutputColumns)), or the expansion itself
/// when the layout is its own. `column_names` are the expansion's column names.
///
/// A column keeps the expansion's own name unless it is renamed, so the names
/// and types are whatever the expansion gives them. Names are quoted via
/// [`quote_ident`] (FF-8).
#[must_use]
pub fn build_projected_sql(
    expanded_sql: &str,
    column_names: &[String],
    output: &super::items::OutputColumns,
) -> String {
    if output.is_identity(column_names) {
        return expanded_sql.to_string();
    }
    let clauses: Vec<String> = output
        .columns
        .iter()
        .map(|(src, name)| {
            let source = quote_ident(&column_names[*src]);
            if *name == column_names[*src] {
                source
            } else {
                format!("{source} AS {}", quote_ident(name))
            }
        })
        .collect();
    format!(
        "SELECT {} FROM ({expanded_sql}) __sv_inner",
        clauses.join(", ")
    )
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

    // -- build_projected_sql ---------------------------------------------

    #[test]
    fn the_default_layout_is_the_expansion_itself() {
        use super::super::items::{Group, OutputColumns};
        let names = vec!["region".to_string(), "amt".to_string()];
        let out =
            OutputColumns::new(&names, [&[None], &[None], &[]], Group::DEFAULT_ORDER).unwrap();
        assert_eq!(build_projected_sql("SELECT 1", &names, &out), "SELECT 1");
    }

    #[test]
    fn a_projection_renames_and_reorders_the_columns() {
        use super::super::items::{Group, OutputColumns};
        let names = vec!["region".to_string(), "amt".to_string()];
        let order = [Group::Metrics, Group::Dimensions, Group::Facts];
        let out =
            OutputColumns::new(&names, [&[Some("R".to_string())], &[None], &[]], order).unwrap();
        assert_eq!(
            build_projected_sql("SELECT 1", &names, &out),
            "SELECT \"amt\", \"region\" AS \"R\" FROM (SELECT 1) __sv_inner"
        );
    }

    #[test]
    fn a_projection_quotes_an_embedded_double_quote() {
        // FF-8: a column name containing `"` must be escaped, not raw-formatted.
        use super::super::items::{Group, OutputColumns};
        let names = vec!["we\"ird".to_string()];
        let out = OutputColumns::new(
            &names,
            [&[Some("x\"y".to_string())], &[], &[]],
            Group::DEFAULT_ORDER,
        )
        .unwrap();
        assert_eq!(
            build_projected_sql("SELECT 1", &names, &out),
            "SELECT \"we\"\"ird\" AS \"x\"\"y\" FROM (SELECT 1) __sv_inner"
        );
    }
}
