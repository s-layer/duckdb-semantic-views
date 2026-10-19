//! Structural guard: `init_extension` must NOT call `duckdb_connect`.
//!
//! Phase 65 (v0.10.0) — ROADMAP success criterion 4.
//!
//! This test parses `src/lib.rs` and walks the syntax tree to confirm that
//! the `init_extension` function body does not contain a call to
//! `duckdb_connect`. Plan 06 retired the H1 `catalog_conn` allocation; Plan 05
//! retired the H2 `query_conn` allocation. Together they eliminated the
//! long-lived extension-owned `duckdb_connection` handles whose
//! `shared_ptr<DatabaseInstance>` references kept the underlying DuckDB
//! `Database` alive past the caller's `close()`, causing the in-process
//! RW→RO reopen busy-spin in `DBInstanceCache::GetInstanceInternal`
//! (LIFE-01 root cause; see
//! `.planning/phases/65-overridecontext-connection-teardown/65-01-SPIKES.md`
//! for the lldb evidence).
//!
//! Reads now open no connection at all: they run in the caller's own
//! `ClientContext` and transaction (the `sv_caller_*` entry points,
//! TECH-DEBT #19). Re-introducing a long-lived
//! `duckdb_connection` in `init_extension` would silently re-create the
//! LIFE-01 hang. This test fails CI if anyone does so.
//!
//! ## Known limitation (documented per D-22 bounded scope)
//!
//! The visitor only matches the **last** segment of a call expression's
//! path against the literal identifier `duckdb_connect`. That catches the
//! common shapes:
//!   - `duckdb_connect(...)`
//!   - `ffi::duckdb_connect(...)`
//!   - `libduckdb_sys::duckdb_connect(...)`
//!
//! It does NOT catch aliasing via `use … as`:
//!
//!   ```rust,ignore
//!   use ffi::duckdb_connect as my_connect;
//!   my_connect(db, &mut conn);            // NOT detected
//!   ```
//!
//! Resolving the use-graph would require name resolution beyond what a
//! pure syntactic scan can do. Per D-22 (bounded scope with signal
//! surfacing) we accept this trade-off: anyone deliberately aliasing
//! `duckdb_connect` to evade the guard is consciously circumventing it,
//! which would show up at code review. The honest-mistake re-introduction
//! path is the call shape we DO catch.
//!
//! ## What this test does NOT cover (by design)
//!
//! - Helper modules outside `init_extension`. The crate's `RawDb` test
//!   helper at `src/lib.rs:226-277` legitimately calls `duckdb_connect`
//!   from inside `RawDb::open_in_memory()`; that's NOT a long-lived
//!   handle in `init_extension` and is scoped to test fixtures.
//! - C++ code in `cpp/src/shim.cpp`. It opens no connection today; a
//!   `Connection` constructed there would be caught in review, not here.
//! - The other long-lived-handle candidates (`duckdb_open`,
//!   `duckdb_open_ext`). Those would create a whole new DB, not just
//!   a connection — much more obvious in code review than a stray
//!   `duckdb_connect`. Worth extending if a regression appears.

use syn::visit::Visit;
use syn::{ExprCall, ItemFn};

/// Walks `init_extension`'s body looking for any call expression whose
/// last path segment is the identifier `duckdb_connect`.
struct Finder {
    in_init_extension: bool,
    found_call_sites: Vec<String>,
}

impl<'ast> Visit<'ast> for Finder {
    fn visit_item_fn(&mut self, f: &'ast ItemFn) {
        let was = self.in_init_extension;
        if f.sig.ident == "init_extension" {
            self.in_init_extension = true;
        }
        syn::visit::visit_item_fn(self, f);
        self.in_init_extension = was;
    }

    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if self.in_init_extension {
            if let syn::Expr::Path(p) = &*c.func {
                if let Some(last) = p.path.segments.last() {
                    if last.ident == "duckdb_connect" {
                        // Capture the full path for the assert message.
                        let segs: Vec<String> = p
                            .path
                            .segments
                            .iter()
                            .map(|s| s.ident.to_string())
                            .collect();
                        self.found_call_sites.push(segs.join("::"));
                    }
                }
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
}

#[test]
fn init_extension_has_no_duckdb_connect_call() {
    let src = std::fs::read_to_string("src/lib.rs").expect("read src/lib.rs");
    let file: syn::File = syn::parse_str(&src).expect("parse src/lib.rs");

    let mut f = Finder {
        in_init_extension: false,
        found_call_sites: Vec::new(),
    };
    f.visit_file(&file);

    assert!(
        f.found_call_sites.is_empty(),
        "init_extension contains a duckdb_connect call site (found: {:?}). \
         Phase 65 (v0.10.0) retired long-lived extension-owned \
         duckdb_connection handles (H1 catalog_conn + H2 query_conn) to \
         resolve the in-process RW→RO reopen hang (LIFE-01). If a new \
         read needs the catalog, use the caller's context through the \
         sv_caller_* entry points (TECH-DEBT #19) instead. See \
         .planning/phases/65-overridecontext-connection-teardown/ for \
         the full record.",
        f.found_call_sites
    );
}

// ---------------------------------------------------------------------------
// Phase 65.1 Plan 03b (D-11 / WR-05) — defence-in-depth AST walker for
// `duckdb_disconnect` calls anywhere in `src/`.
// ---------------------------------------------------------------------------
//
// No connection crosses the FFI boundary any more (TECH-DEBT #19 removed the
// `BorrowedConnection` newtype that once guarded borrowed handles at the type
// level), so Rust code in this crate has no handle it could disconnect other
// than one it opened itself. This walker scans every `.rs` file under `src/`
// for any call expression whose terminal path segment is
// `duckdb_disconnect`, and fails the test if any are found outside the
// allow-list.
//
// Same known limitation as the `Finder` above: this walker matches the
// terminal path segment literally and does NOT resolve `use … as` aliases.
// Documented per Phase 65 D-22 (bounded scope with signal surfacing).

/// Allow-list of `(file_path, full_call_path)` pairs that legitimately call
/// `duckdb_disconnect`. The single legitimate case in this crate today is
/// the `RawDb` test fixture in `src/lib.rs::test_helpers`, which OWNS its
/// `duckdb_database` + `duckdb_connection` pair (via `duckdb_open` +
/// `duckdb_connect` in `open_in_memory`) and releases them in `Drop`. That
/// is the canonical OWN-and-release pattern — NOT a BORROW.
///
/// Every entry here is a deliberate exception. Adding to this list during
/// code review should require explicit justification ("this site owns the
/// handle and releases it; no borrow is involved").
const ALLOWED_DISCONNECT_SITES: &[(&str, &str)] = &[
    // src/lib.rs::test_helpers::RawDb::Drop — owns the connection allocated
    // by RawDb::open_in_memory (paired with duckdb_open + duckdb_connect).
    // Test-only module (`#[cfg(not(feature = "extension"))]`).
    ("src/lib.rs", "ffi::duckdb_disconnect"),
];

/// Walks all of `src/` looking for any call expression whose last path
/// segment is the identifier `duckdb_disconnect`, with a clear diagnostic
/// on regression. Allowed sites are
/// filtered via `ALLOWED_DISCONNECT_SITES` above.
struct DisconnectFinder {
    /// (source_file_path, full_call_path) for every match found.
    found_call_sites: Vec<(String, String)>,
    current_file: String,
}

impl<'ast> Visit<'ast> for DisconnectFinder {
    fn visit_expr_call(&mut self, c: &'ast ExprCall) {
        if let syn::Expr::Path(p) = &*c.func {
            if let Some(last) = p.path.segments.last() {
                if last.ident == "duckdb_disconnect" {
                    let segs: Vec<String> = p
                        .path
                        .segments
                        .iter()
                        .map(|s| s.ident.to_string())
                        .collect();
                    self.found_call_sites
                        .push((self.current_file.clone(), segs.join("::")));
                }
            }
        }
        syn::visit::visit_expr_call(self, c);
    }
}

#[test]
fn no_duckdb_disconnect_anywhere_in_src() {
    let mut finder = DisconnectFinder {
        found_call_sites: Vec::new(),
        current_file: String::new(),
    };

    for entry in walkdir::WalkDir::new("src/") {
        let entry = entry.expect("walkdir entry");
        if entry.path().extension().and_then(|s| s.to_str()) != Some("rs") {
            continue;
        }
        let src = match std::fs::read_to_string(entry.path()) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let file = match syn::parse_str::<syn::File>(&src) {
            Ok(f) => f,
            // Skip unparseable files — should not happen in src/, but a
            // hard panic here would mask the real signal.
            Err(_) => continue,
        };
        finder.current_file = entry.path().display().to_string();
        finder.visit_file(&file);
    }

    // Filter out documented OWN-and-release sites (paired duckdb_open +
    // duckdb_connect + duckdb_disconnect + duckdb_close on a self-owned
    // handle — typically Drop impls). See ALLOWED_DISCONNECT_SITES.
    //
    // Phase 65.1 IN-03: track which allow-list entries actually matched a
    // call site so a stale entry (e.g. allowed call site moved or deleted)
    // surfaces as a test failure instead of silently weakening the guard
    // for any future code legitimately added at the recorded path.
    let mut matched = vec![false; ALLOWED_DISCONNECT_SITES.len()];
    finder.found_call_sites.retain(|(file, path)| {
        for (idx, (allowed_file, allowed_path)) in ALLOWED_DISCONNECT_SITES.iter().enumerate() {
            if file == allowed_file && path == allowed_path {
                matched[idx] = true;
                return false;
            }
        }
        true
    });

    assert!(
        finder.found_call_sites.is_empty(),
        "duckdb_disconnect call sites found in src/: {:#?}. \
         A raw `ffi::duckdb_connection` should never be disconnected by \
         Rust code in this crate: reads run in the caller's context and \
         no connection crosses the FFI boundary (TECH-DEBT #19). See \
         .planning/phases/65.1-phase-65-code-review-remediation/65.1-CONTEXT.md \
         (D-10 + D-11) for the full record.",
        finder.found_call_sites
    );

    // Phase 65.1 IN-03: every allow-list entry must have matched at least
    // one call site. If a permitted call site is moved or deleted and the
    // exception is left behind, the entry quietly becomes a free pass for
    // any future code added at that (file, path) coordinate. Failing the
    // test forces the allow-list to stay tightly aligned with the actual
    // call graph.
    for (idx, (file, path)) in ALLOWED_DISCONNECT_SITES.iter().enumerate() {
        assert!(
            matched[idx],
            "ALLOWED_DISCONNECT_SITES entry ({file:?}, {path:?}) matched \
             zero call sites — the documented call site appears to have \
             moved or been deleted. Remove the stale exception from \
             ALLOWED_DISCONNECT_SITES in tests/no_long_lived_conn.rs (or \
             update it to point at the new location) so the guard does \
             not silently grant a free pass for future code added at \
             this coordinate."
        );
    }
}
