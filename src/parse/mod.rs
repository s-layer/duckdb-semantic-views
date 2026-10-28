// Text-level parsing behind the grammar productions for semantic-view DDL.
//
// The grammar (`crate::grammar`) recognises every statement. What it hands
// back here: CREATE's text, planned by [`plan_create`] (prefix detection in
// `detect`, the body in `create_body`); the read-side lowering and shared
// messages (`rewrite`, `show_clauses`); and the guarded native SQL for every
// write (`native_sql`).

mod create_body;
pub(crate) use create_body::validate_create_body;
#[cfg(test)]
pub(crate) use create_body::{
    extract_dollar_quoted, extract_single_quoted, extract_view_comment, rewrite_ddl_yaml_body,
    rewrite_ddl_yaml_file_body,
};

mod detect;
pub(crate) use detect::{detect_create_prefix, match_keyword_prefix, skip_leading_whitespace};

mod native_sql;
#[cfg(feature = "extension")]
pub(crate) use native_sql::lower_action;

// The grammar productions use these; they compile with the extension or for tests.
#[cfg(any(feature = "extension", test))]
mod show_clauses;
#[cfg(any(feature = "extension", test))]
pub(crate) use show_clauses::{
    build_filter_suffix, show_unexpected_tokens, ScopeName, ShowClauses,
};

mod rewrite;
#[cfg(any(feature = "extension", test))]
pub(crate) use rewrite::messages;
#[cfg(any(feature = "extension", test))]
pub(crate) use rewrite::{lower_name_only_read, lower_show};
pub use rewrite::{plan_create, CreateMode, RewriteAction, ViewProperty};

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
