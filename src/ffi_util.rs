//! Canonical FFI seam helpers: error-buffer writing and heap-buffer handoff.
//!
//! Every Rust↔C++ boundary in this extension uses the same two conventions:
//!
//! * **Fixed-size error buffers** — the C++ caller passes `(buf, buf_len)`
//!   and expects a NUL-terminated UTF-8 message truncated to fit. Truncation
//!   must land on a `char` boundary: cutting a multi-byte codepoint in half
//!   produces invalid UTF-8 that `DuckDB` then embeds in a `BinderException`.
//! * **Heap-owned result buffers** — Rust allocates via `Box<[u8]>::into_raw`
//!   and publishes the exact `(ptr, len)` pair through out-parameters; the
//!   C++ side MUST release it with `sv_free_buffer(ptr, len)`. `Box<[u8]>`
//!   (not a leaked `Vec`) guarantees `len == capacity`, which the matching
//!   `Vec::from_raw_parts(ptr, len, len)` in [`reclaim_c_buffer`] relies on.
//!
//! Publish contract ("both-or-drop"): if *either* out-pointer is null the
//! buffer is dropped and *neither* slot is written. Writing only one slot
//! would desync `(ptr, len)` and either leak or corrupt the later
//! `sv_free_buffer` call; dropping is always safe.
//!
//! History: these helpers used to exist as three copies each (`parse.rs`,
//! `ddl/read_ffi.rs`, `ddl/alter_helpers_ffi.rs`) with *diverging* semantics
//! — one error writer truncated mid-codepoint (FF-5) and one publisher
//! leaked on null out-pointers. Consolidated per ST-4 (code-review
//! 2026-07-02); do not re-inline these at call sites.

/// Encode a byte length as a little-endian wire `u32`, erroring rather than
/// clamping when it exceeds `u32::MAX` (FF-6). A silent `as u32` truncation
/// would write a length prefix that disagrees with the bytes actually
/// appended, desyncing every subsequent field on the C++ read side. Overflow
/// is unreachable for real payloads (a single row/cell, or the execution SQL,
/// would each need to exceed 4 GiB), so the error is a hard corruption signal,
/// not a routine path.
///
/// The read-path serializers (`ddl::read_ffi`) use it. C-6 (code-review
/// 2026-07-11) collapsed two divergent copies onto it; the query path's copy
/// left with the result wire format when `semantic_view()` moved to
/// `bind_replace` (TECH-DEBT #42).
#[cfg(feature = "extension")]
pub(crate) fn wire_len(n: usize, what: &str) -> Result<u32, String> {
    u32::try_from(n).map_err(|_| format!("{what} ({n} bytes) exceeds the wire-format u32 limit"))
}

/// Write a NUL-terminated error message into a fixed-size C buffer.
///
/// Truncates to at most `buf_len - 1` payload bytes, walking back to a UTF-8
/// `char` boundary so a multi-byte codepoint straddling the truncation point
/// is dropped whole rather than producing an invalid UTF-8 tail in the C
/// string. No-op when `buf` is null or `buf_len == 0`.
///
/// # Safety
///
/// `buf` must be either null OR point to writable storage of at least
/// `buf_len` bytes.
pub unsafe fn write_error_to_buffer(buf: *mut u8, buf_len: usize, msg: &str) {
    if buf.is_null() || buf_len == 0 {
        return;
    }
    let max_copy = buf_len - 1; // reserve space for the NUL terminator
    let mut copy_len = msg.len().min(max_copy);
    // is_char_boundary(0) is always true, so this terminates.
    while !msg.is_char_boundary(copy_len) {
        copy_len -= 1;
    }
    if copy_len > 0 {
        std::ptr::copy_nonoverlapping(msg.as_ptr(), buf, copy_len);
    }
    *buf.add(copy_len) = 0;
}

/// Move an owned byte buffer onto the heap via `Box<[u8]>::into_raw` and
/// return the `(ptr, len)` pair the C++ caller must pass back to
/// `sv_free_buffer`. The buffer is NOT NUL-terminated — the C++ side reads
/// exactly `len` bytes.
///
/// Uses `Box<[u8]>` rather than a leaked `Vec` because `Vec::shrink_to_fit`
/// is only a hint — the allocator may keep excess capacity, which would make
/// the matching `Vec::from_raw_parts(ptr, len, len)` in [`reclaim_c_buffer`]
/// undefined behaviour in release builds. `into_boxed_slice` guarantees
/// `len == capacity`.
#[must_use]
pub fn leak_bytes_to_c_buffer(bytes: Vec<u8>) -> (*mut u8, usize) {
    let boxed: Box<[u8]> = bytes.into_boxed_slice();
    let len = boxed.len();
    let ptr = Box::into_raw(boxed).cast::<u8>();
    (ptr, len)
}

/// Reclaim a buffer produced by [`leak_bytes_to_c_buffer`].
///
/// Safe to call with a null pointer (no-op).
///
/// # Safety
///
/// `ptr`/`len` must be the exact pair returned by an earlier call to
/// [`leak_bytes_to_c_buffer`] (or its FFI exports), and may only be released
/// once.
pub unsafe fn reclaim_c_buffer(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    let slice = std::ptr::slice_from_raw_parts_mut(ptr, len);
    drop(Box::from_raw(slice));
}

/// Publish an owned byte buffer to the FFI `(ptr, len)` out-parameters.
///
/// Both-or-drop contract: if *either* out-pointer is null the buffer is
/// dropped and *neither* slot is written — a misbehaving C++ caller cannot
/// induce a leak or a desynced `(ptr, len)` pair through us.
///
/// # Safety
///
/// Either both `out_ptr` and `out_len` point to writable slots, or the call
/// is treated as "drop and skip writing". On success the C++ side takes
/// ownership and MUST release via `sv_free_buffer(ptr, len)` with the exact
/// pair written here.
pub unsafe fn publish_owned_bytes(bytes: Vec<u8>, out_ptr: *mut *mut u8, out_len: *mut usize) {
    if out_ptr.is_null() || out_len.is_null() {
        return; // dropping `bytes` here releases the heap allocation
    }
    let (ptr, len) = leak_bytes_to_c_buffer(bytes);
    *out_ptr = ptr;
    *out_len = len;
}

/// [`publish_owned_bytes`] for an owned UTF-8 `String` payload.
///
/// # Safety
///
/// Same contract as [`publish_owned_bytes`].
pub unsafe fn publish_owned_string(s: String, out_ptr: *mut *mut u8, out_len: *mut usize) {
    publish_owned_bytes(s.into_bytes(), out_ptr, out_len);
}

/// FFI export: free a heap buffer the Rust side handed to C++ (SQL, rule
/// text, read results).
///
/// Safe to call with a null pointer (no-op).
///
/// # Safety
///
/// `ptr`/`len` must be the exact pair a Rust FFI function published through
/// its out-parameters. Calling with any other pair (or twice on the same
/// pair) is undefined behaviour.
#[cfg(feature = "extension")]
#[no_mangle]
pub unsafe extern "C" fn sv_free_buffer(ptr: *mut u8, len: usize) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        reclaim_c_buffer(ptr, len);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_c_str(buf: &[u8]) -> &str {
        let nul = buf.iter().position(|&b| b == 0).expect("NUL terminator");
        std::str::from_utf8(&buf[..nul]).expect("valid UTF-8 up to NUL")
    }

    #[test]
    fn write_error_fits_short_message() {
        let mut buf = [0xFFu8; 32];
        unsafe { write_error_to_buffer(buf.as_mut_ptr(), buf.len(), "boom") };
        assert_eq!(read_c_str(&buf), "boom");
    }

    #[test]
    fn write_error_truncates_at_char_boundary() {
        // "héllo" is 6 bytes: h(1) é(2) l l o. A 4-byte buffer holds 3
        // payload bytes + NUL; byte 3 is mid-'é'... no — é occupies bytes
        // 1-2, so 3 payload bytes = "hé". Use a case that actually straddles:
        // 2-byte buffer → 1 payload byte; "é..." would straddle at byte 1.
        let mut buf = [0xFFu8; 2];
        unsafe { write_error_to_buffer(buf.as_mut_ptr(), buf.len(), "éx") };
        // 1 payload byte available, but that's mid-'é' → walk back to 0.
        assert_eq!(read_c_str(&buf), "");

        let mut buf = [0xFFu8; 4];
        unsafe { write_error_to_buffer(buf.as_mut_ptr(), buf.len(), "héllo") };
        // 3 payload bytes: "hé" is 3 bytes and ends on a boundary.
        assert_eq!(read_c_str(&buf), "hé");
    }

    #[test]
    fn write_error_multibyte_never_produces_invalid_utf8() {
        // Sweep every truncation length against a mixed-width message; the
        // payload up to NUL must always parse as UTF-8 (the FF-5 regression).
        let msg = "café ☕ 東京 z";
        for buf_len in 1..=msg.len() + 2 {
            let mut buf = vec![0xFFu8; buf_len];
            unsafe { write_error_to_buffer(buf.as_mut_ptr(), buf_len, msg) };
            let s = read_c_str(&buf);
            assert!(msg.starts_with(s), "truncation produced non-prefix: {s:?}");
        }
    }

    #[test]
    fn write_error_null_or_zero_is_noop() {
        unsafe { write_error_to_buffer(std::ptr::null_mut(), 16, "x") };
        let mut buf = [0xAAu8; 1];
        unsafe { write_error_to_buffer(buf.as_mut_ptr(), 0, "x") };
        assert_eq!(buf[0], 0xAA);
    }

    #[test]
    fn leak_and_reclaim_roundtrip() {
        let (ptr, len) = leak_bytes_to_c_buffer(b"payload".to_vec());
        assert!(!ptr.is_null());
        assert_eq!(len, 7);
        let copy = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        assert_eq!(copy, b"payload");
        unsafe { reclaim_c_buffer(ptr, len) };
    }

    #[test]
    fn reclaim_null_is_noop() {
        unsafe { reclaim_c_buffer(std::ptr::null_mut(), 0) };
        unsafe { reclaim_c_buffer(std::ptr::null_mut(), 99) };
    }

    #[test]
    fn leak_empty_buffer_roundtrip() {
        let (ptr, len) = leak_bytes_to_c_buffer(Vec::new());
        assert_eq!(len, 0);
        unsafe { reclaim_c_buffer(ptr, len) };
    }

    #[test]
    fn publish_writes_both_slots() {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: usize = usize::MAX;
        unsafe { publish_owned_bytes(b"abc".to_vec(), &raw mut ptr, &raw mut len) };
        assert!(!ptr.is_null());
        assert_eq!(len, 3);
        unsafe { reclaim_c_buffer(ptr, len) };
    }

    #[test]
    fn publish_with_null_out_pointer_drops_and_writes_nothing() {
        // Null ptr slot: len slot must stay untouched (both-or-drop).
        let mut len: usize = usize::MAX;
        unsafe { publish_owned_bytes(b"abc".to_vec(), std::ptr::null_mut(), &raw mut len) };
        assert_eq!(len, usize::MAX, "len slot written despite null ptr slot");

        // Null len slot: ptr slot must stay untouched.
        let mut ptr: *mut u8 = std::ptr::null_mut();
        unsafe { publish_owned_bytes(b"abc".to_vec(), &raw mut ptr, std::ptr::null_mut()) };
        assert!(ptr.is_null(), "ptr slot written despite null len slot");
    }

    #[test]
    fn publish_string_delegates() {
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut len: usize = 0;
        unsafe { publish_owned_string("héllo".to_string(), &raw mut ptr, &raw mut len) };
        assert_eq!(len, 6);
        let copy = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
        assert_eq!(copy, "héllo".as_bytes());
        unsafe { reclaim_c_buffer(ptr, len) };
    }
}
