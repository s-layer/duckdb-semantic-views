// Parse detection and rewriting for semantic view DDL statements.
//
// This module provides two layers:
// 1. Pure detection/rewrite functions (`detect_semantic_view_ddl`,
//    `plan_rewrite`) testable under `cargo test`
//    without the extension feature.
// 2. FFI entry points (`sv_parser_override_rust`, `sv_free_buffer`)
//    feature-gated on `extension`, with `catch_unwind` for panic safety.
//
// Prefix detection lives in the `detect` submodule; SHOW-clause filter
// parsing in `show_clauses`; CREATE-body parsing in `create_body` (AR-1).
// These are re-exported below so existing `crate::parse::*` call sites and
// tests resolve unchanged. The rewrite-planning core (RewriteAction,
// plan_rewrite, plan_ddl, ALTER handling) lives in the `rewrite` submodule
// (AR-1); this module is a thin coordinator.

mod create_body;
pub(crate) use create_body::validate_create_body;
#[cfg(test)]
pub(crate) use create_body::{
    extract_dollar_quoted, extract_single_quoted, extract_view_comment, rewrite_ddl_yaml_body,
    rewrite_ddl_yaml_file_body,
};

mod detect;
pub use detect::{detect_ddl_kind, detect_near_miss, detect_semantic_view_ddl};
pub(crate) use detect::{detect_ddl_prefix, match_keyword_prefix, skip_leading_whitespace};

mod ffi;
#[cfg(feature = "extension")]
pub use ffi::sv_free_buffer;
#[cfg(test)]
pub(crate) use ffi::sv_parse_function_rust;
#[cfg(all(feature = "extension", test))]
pub(crate) use ffi::sv_parser_override_rust;

mod native_sql;
#[cfg(feature = "extension")]
pub(crate) use native_sql::rewrite_to_native_sql;

mod show_clauses;
pub(crate) use show_clauses::{build_filter_suffix, parse_show_filter_clauses};

mod rewrite;
pub(crate) use rewrite::extract_quoted_string;
pub use rewrite::{plan_rewrite, CreateMode, RewriteAction};

/// Not our statement -- return `DISPLAY_ORIGINAL_ERROR`.
pub const PARSE_NOT_OURS: u8 = 0;
/// Detected a semantic view DDL statement -- return `PARSE_SUCCESSFUL`.
pub const PARSE_DETECTED: u8 = 1;

// ---------------------------------------------------------------------------
// DdlKind enum
// ---------------------------------------------------------------------------

/// The supported DDL statement forms for semantic views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdlKind {
    Create,
    CreateOrReplace,
    CreateIfNotExists,
    Drop,
    DropIfExists,
    Describe,
    Show,
    ShowTerse,
    ShowColumns,
    Alter,
    AlterIfExists,
    ShowDimensions,
    ShowMetrics,
    ShowFacts,
    ShowMaterializations,
}
