//! Rewrite planning for semantic-view DDL statements (AR-1).
//!
//! Turns a recognised DDL statement into a structured [`RewriteAction`]: the
//! validation/rewrite core that previously lived inline in `parse/mod.rs`.
//! Prefix detection lives in `detect`, SHOW-clause parsing in `show_clauses`,
//! CREATE-body parsing in `create_body`, and native-SQL emission in
//! `native_sql`; [`plan_rewrite`] wires them together. `parse/mod.rs` is now a
//! thin coordinator that declares the submodules and re-exports the public API.

use crate::errors::ParseError;
use crate::ident::{find_identifier_end, parse_view_ref, ViewRef};
use crate::sql_lit::SqlLit;
use crate::util::{byte_offset_within, extract_single_quoted_prefix, SingleQuoteError};

use super::{
    build_filter_suffix, detect_ddl_prefix, match_keyword_prefix, parse_show_filter_clauses,
    skip_leading_whitespace, validate_create_body, DdlKind,
};

use super::show_clauses::ShowClauses;
// Only the emitted-SQL unit tests name this type directly; production code
// passes it through as `clauses.in_schema.as_ref()`.
#[cfg(test)]
use super::show_clauses::ScopeName;

// Used by the `write_error_to_buffer_*` unit tests (which reference it as
// `super::write_error_to_buffer`); the FFI entry points that use it in
// production live in the `ffi` submodule.
#[cfg(test)]
use crate::ffi_util::write_error_to_buffer;

/// The DROP / ALTER / DESCRIBE messages the grammar route (`crate::grammar`)
/// reports too, kept in one place so the two routes cannot drift apart.
pub(crate) mod messages {
    pub(crate) const INVALID_VIEW_NAME: &str = "Invalid view name";
    pub(crate) const INVALID_RENAME_TARGET: &str = "Invalid new view name in RENAME TO";
    pub(crate) const EXPECTED_COMMENT_STRING: &str =
        "Expected single-quoted string after SET COMMENT =";
    pub(crate) const UNSUPPORTED_ALTER: &str =
        "Unsupported ALTER operation. Supported: RENAME TO, SET COMMENT, UNSET COMMENT.";

    pub(crate) fn unexpected_after_view_name(rest: &str) -> String {
        format!("Unexpected tokens after view name: '{rest}'")
    }

    pub(crate) fn unexpected_after_rename_target(rest: &str) -> String {
        format!("Unexpected tokens after new view name in RENAME TO: '{rest}'")
    }
}
use messages::{
    unexpected_after_rename_target, unexpected_after_view_name, EXPECTED_COMMENT_STRING,
    INVALID_RENAME_TARGET, INVALID_VIEW_NAME, UNSUPPORTED_ALTER,
};

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Extract the RAW (un-normalized) view name from a name-only DDL statement.
///
/// The read-side DESCRIBE / SHOW COLUMNS rewrites embed this raw name into a
/// table-function call and defer identifier folding to the TF dispatcher —
/// exactly like `semantic_view()` (FF-4), so every read TF normalizes uniformly
/// at the catalog-read boundary. DROP / ALTER, which emit native catalog SQL
/// with the name baked in (no later normalize opportunity), use the normalizing
/// [`extract_name_only`] wrapper instead.
///
/// The statement ends at the view name: any trailing text is an error (PA-5,
/// code-review 2026-07-02: `DROP SEMANTIC VIEW a b c` used to execute and
/// silently discard `b c`; `DESCRIBE ... a CASCADE` silently ignored the
/// `CASCADE`). ALTER's sub-operation follows the name, so `rewrite_alter`
/// parses the name on its own cursor rather than through this helper.
///
/// `prefix_len` is the byte length of the already-matched prefix; `base` is the
/// absolute byte offset of `trimmed[0]` in the original query, so errors carry a
/// caret position pointing at the offending token (R-2).
fn extract_raw_name_only(
    trimmed: &str,
    prefix_len: usize,
    base: usize,
) -> Result<String, ParseError> {
    let after_prefix = trimmed[prefix_len..].trim();
    if after_prefix.is_empty() {
        return Err(ParseError {
            message: "Missing view name".to_string(),
            position: Some(base + prefix_len),
        });
    }
    // Name is everything up to whitespace (or end), honouring `"..."` regions so
    // a quoted identifier with inner whitespace (`"my view"`) is captured intact.
    // `allow_paren=false` — DROP / DESCRIBE / SHOW COLUMNS / ALTER source-name
    // slots never legally end at `(`.
    let name_end = find_identifier_end(after_prefix, false);
    let raw_name = &after_prefix[..name_end];
    if raw_name.is_empty() {
        return Err(ParseError {
            message: "Missing view name".to_string(),
            position: Some(base + byte_offset_within(trimmed, after_prefix)),
        });
    }
    let rest = after_prefix[name_end..].trim();
    if !rest.is_empty() {
        return Err(ParseError {
            message: unexpected_after_view_name(rest),
            position: Some(base + byte_offset_within(trimmed, rest)),
        });
    }
    Ok(raw_name.to_string())
}

/// Extract and normalize the view reference from a name-only DDL statement
/// (DROP). Folds every part to lowercase and keeps any `schema.` qualifier —
/// semantic views are schema-scoped, so `analytics.v` and `staging.v` are two
/// views and the emitters need to know which one was written. Read-side
/// rewrites use [`extract_raw_name_only`] instead.
fn extract_name_only(trimmed: &str, prefix_len: usize, base: usize) -> Result<ViewRef, ParseError> {
    let raw = extract_raw_name_only(trimmed, prefix_len, base)?;
    parse_view_ref(&raw).map_err(|e| ParseError {
        message: format!("{INVALID_VIEW_NAME}: {e}"),
        // Point the caret at the name token itself, not the end of the prefix:
        // with whitespace after the prefix the two differ. Matches the other
        // helpers in this PR (extract_raw_name_only, rewrite_alter) which all
        // resolve positions from the trimmed name slice.
        position: Some(base + byte_offset_within(trimmed, trimmed[prefix_len..].trim())),
    })
}

// ---------------------------------------------------------------------------
// Read-side DDL lowering (DESCRIBE / SHOW → SELECT * FROM <read TF>(...))
// ---------------------------------------------------------------------------

/// Map a read-side `DdlKind` to the read table function it lowers to.
///
/// Only the read-side kinds are lowered to a function-call `Passthrough`;
/// CREATE routes through `plan_rewrite`, and DROP/ALTER become structured
/// native-DML `RewriteAction`s (their v0.5-era `*_semantic_view` function
/// names were retired in v0.8.0 and are no longer registered). `panic`ing on
/// a write kind is unreachable: every caller is inside a read-side `match`
/// arm of `plan_ddl`.
fn read_function_name(kind: DdlKind) -> &'static str {
    match kind {
        DdlKind::Describe => "describe_semantic_view",
        DdlKind::Show => "list_semantic_views",
        DdlKind::ShowTerse => "list_terse_semantic_views",
        DdlKind::ShowColumns => "show_columns_in_semantic_view",
        DdlKind::ShowDimensions => "show_semantic_dimensions",
        DdlKind::ShowMetrics => "show_semantic_metrics",
        DdlKind::ShowFacts => "show_semantic_facts",
        DdlKind::ShowMaterializations => "show_semantic_materializations",
        DdlKind::Create
        | DdlKind::CreateOrReplace
        | DdlKind::CreateIfNotExists
        | DdlKind::Drop
        | DdlKind::DropIfExists
        | DdlKind::Alter
        | DdlKind::AlterIfExists => {
            unreachable!("read_function_name called on a write-side DdlKind: {kind:?}")
        }
    }
}

// ---------------------------------------------------------------------------
// SHOW SEMANTIC filter clause helpers (Phase 34.1.1)
// ---------------------------------------------------------------------------

/// Extract a single-quoted string from `input`, starting at position 0.
/// Returns `(extracted_content, bytes_consumed)` where `bytes_consumed` includes
/// the opening and closing quotes.
///
/// Handles SQL-style escaping: `''` inside quotes represents a literal `'`.
///
/// Thin adapter over the shared UTF-8-correct extractor (ST-4 consolidation;
/// originally fixed here as Phase 65.1 WR-04) mapping errors to this call
/// site's message wording.
pub(crate) fn extract_quoted_string(input: &str) -> Result<(String, usize), String> {
    extract_single_quoted_prefix(input).map_err(|e| {
        match e {
            SingleQuoteError::NotQuoted => "Expected single-quoted string",
            SingleQuoteError::Unterminated => "Unterminated single-quoted string",
        }
        .to_string()
    })
}

/// Parse an ALTER SEMANTIC VIEW sub-operation into a structured
/// [`RewriteAction`] (RENAME TO → `AlterRename`, SET COMMENT → `AlterSetComment`,
/// UNSET COMMENT → `AlterUnsetComment`). Names/comment are carried raw; the
/// emission stage escapes them.
///
/// `base` is the absolute byte offset of `trimmed[0]` in the original query, so
/// errors carry a caret position pointing at the offending token (R-2). This is
/// the sole ALTER grammar (P-9): the former `validate_alter` pre-pass that
/// `plan_rewrite` ran before this was a second, drift-prone copy of the same
/// grammar; its two unique diagnostics (missing sub-operation, missing view
/// name) are folded in below.
#[allow(clippy::too_many_lines)]
fn rewrite_alter(
    trimmed: &str,
    plen: usize,
    kind: DdlKind,
    base: usize,
) -> Result<RewriteAction, ParseError> {
    // Absolute offset of a subslice of `trimmed`, for caret positions.
    let abs = |sub: &str| base + byte_offset_within(trimmed, sub);
    let after_prefix = trimmed[plen..].trim();
    if after_prefix.is_empty() {
        return Err(ParseError {
            message: "Missing view name after ALTER SEMANTIC VIEW".to_string(),
            position: Some(base + plen),
        });
    }
    // Quote-aware delimiter scan so `"my view"` is captured intact (allow_paren=false).
    let name_end = find_identifier_end(after_prefix, false);
    if name_end == 0 {
        return Err(ParseError {
            message: "Missing view name after ALTER SEMANTIC VIEW".to_string(),
            position: Some(abs(after_prefix)),
        });
    }
    let raw_view_name = &after_prefix[..name_end];
    let view_name = parse_view_ref(raw_view_name).map_err(|e| ParseError {
        message: format!("{INVALID_VIEW_NAME}: {e}"),
        position: Some(abs(after_prefix)),
    })?;
    let rest = after_prefix[name_end..].trim();
    if rest.is_empty() {
        // Name present, no sub-operation (was `validate_alter`'s diagnostic).
        return Err(ParseError {
            message: "Missing ALTER operation after view name. Supported: RENAME TO, SET COMMENT, UNSET COMMENT.".to_string(),
            position: Some(abs(after_prefix) + after_prefix.len()),
        });
    }
    let if_exists = kind == DdlKind::AlterIfExists;

    // Sub-operation keyword matching rides match_keyword_prefix so any
    // amount of whitespace separates the keywords (PA-10: the old
    // starts_with("RENAME TO") required exactly one space) and a trailing
    // word boundary is enforced (PA-4 contract).
    if let Some(consumed) = match_keyword_prefix(rest.as_bytes(), &[b"rename", b"to"]) {
        let after_op = rest[consumed..].trim();
        if after_op.is_empty() {
            return Err(ParseError {
                message: "Missing new name after RENAME TO".to_string(),
                position: Some(abs(rest) + rest.len()),
            });
        }
        // Capture the (possibly quoted) new name, then reject trailing
        // garbage — `RENAME TO x oops` must not rename to `x oops` (PA-5).
        let new_name_end = find_identifier_end(after_op, false);
        let new_name_raw = &after_op[..new_name_end];
        let trailing = after_op[new_name_end..].trim();
        if !trailing.is_empty() {
            return Err(ParseError {
                message: unexpected_after_rename_target(trailing),
                position: Some(abs(trailing)),
            });
        }
        let new_name = parse_view_ref(new_name_raw).map_err(|e| ParseError {
            message: format!("{INVALID_RENAME_TARGET}: {e}"),
            position: Some(abs(after_op)),
        })?;
        Ok(RewriteAction::AlterRename {
            name: view_name,
            new_name,
            if_exists,
        })
    } else if let Some(consumed) = match_keyword_prefix(rest.as_bytes(), &[b"set", b"comment"]) {
        let after_set_comment = rest[consumed..].trim_start();
        if !after_set_comment.starts_with('=') {
            return Err(ParseError {
                message: "Expected '=' after SET COMMENT".to_string(),
                position: Some(abs(after_set_comment)),
            });
        }
        let after_eq = after_set_comment[1..].trim_start();
        if !after_eq.starts_with('\'') {
            return Err(ParseError {
                message: EXPECTED_COMMENT_STRING.to_string(),
                position: Some(abs(after_eq)),
            });
        }
        // Extract the quoted string handling '' escaping
        let (comment_value, consumed_lit) =
            extract_quoted_string(after_eq).map_err(|e| ParseError {
                message: format!("Invalid comment string: {e}"),
                position: Some(abs(after_eq)),
            })?;
        let trailing = after_eq[consumed_lit..].trim();
        if !trailing.is_empty() {
            return Err(ParseError {
                message: format!("Unexpected tokens after SET COMMENT string: '{trailing}'"),
                position: Some(abs(trailing)),
            });
        }
        Ok(RewriteAction::AlterSetComment {
            name: view_name,
            comment: comment_value,
            if_exists,
        })
    } else if let Some(consumed) = match_keyword_prefix(rest.as_bytes(), &[b"unset", b"comment"]) {
        let trailing = rest[consumed..].trim();
        if !trailing.is_empty() {
            return Err(ParseError {
                message: format!("Unexpected tokens after UNSET COMMENT: '{trailing}'"),
                position: Some(abs(trailing)),
            });
        }
        Ok(RewriteAction::AlterUnsetComment {
            name: view_name,
            if_exists,
        })
    } else {
        Err(ParseError {
            message: UNSUPPORTED_ALTER.to_string(),
            position: Some(abs(rest)),
        })
    }
}

/// Parse a non-CREATE semantic view DDL statement into a structured
/// [`RewriteAction`]:
/// - DROP → `Drop`; ALTER → `AlterRename` / `AlterSetComment` / `AlterUnsetComment`.
/// - Read-side DESCRIBE / SHOW / SHOW COLUMNS → `Passthrough` final SQL.
///
/// CREATE forms must go through `plan_rewrite` -> `validate_create_body`.
fn plan_ddl(query: &str) -> Result<RewriteAction, ParseError> {
    // PA-7: comment-blind rewriting (idempotent when the caller already
    // blanked; only allocates when comments are present).
    let blanked = crate::util::blank_sql_comments(query);
    let query = blanked.as_ref();
    let lead = skip_leading_whitespace(query);
    let trimmed = query[lead..].trim_end();
    let trimmed = trimmed.trim_end_matches(';').trim();
    // `trimmed` starts at absolute byte offset `lead` in the (length-preserving
    // blanked) query; helpers add offsets to this for caret positions (R-2).
    let trim_base = lead;

    let Some((kind, plen)) = detect_ddl_prefix(trimmed) else {
        return Err(ParseError {
            message: "Not a semantic view DDL statement".to_string(),
            position: None,
        });
    };

    match kind {
        // CREATE forms are handled by `plan_rewrite` -> `validate_create_body`
        // and never routed here (this is the only caller path): the match is
        // exhaustive, so the branch must exist, but it is unreachable in a
        // well-formed call graph.
        DdlKind::Create | DdlKind::CreateOrReplace | DdlKind::CreateIfNotExists => {
            unreachable!("plan_ddl called with a CREATE DdlKind; CREATE routes via plan_rewrite")
        }
        // DROP: native DELETE (structured).
        DdlKind::Drop | DdlKind::DropIfExists => {
            let name = extract_name_only(trimmed, plen, trim_base)?;
            Ok(RewriteAction::Drop {
                name,
                if_exists: kind == DdlKind::DropIfExists,
            })
        }
        // Read-side name-only forms (DESCRIBE, SHOW COLUMNS IN SEMANTIC VIEW).
        // FF-4: embed the RAW name and let the TF dispatcher fold it, matching
        // the other read TFs (`semantic_view`, `show_semantic_* IN`) which all
        // normalize once at the catalog-read boundary. Normalizing here as well
        // would double-fold a quoted mixed-case name (`"MyView"` → `myview`).
        // The single-quote escape keeps the literal well-formed; the dispatcher
        // reads the view via a prepared statement, so the raw name never reaches
        // SQL text on the read side.
        DdlKind::Describe | DdlKind::ShowColumns => {
            let name = extract_raw_name_only(trimmed, plen, trim_base)?;
            Ok(RewriteAction::Passthrough(lower_name_only_read(
                kind, &name,
            )))
        }
        // SHOW SEMANTIC VIEWS/DIMENSIONS/METRICS/FACTS: optional LIKE/IN/FOR METRIC/STARTS WITH/LIMIT
        DdlKind::Show
        | DdlKind::ShowTerse
        | DdlKind::ShowDimensions
        | DdlKind::ShowMetrics
        | DdlKind::ShowFacts
        | DdlKind::ShowMaterializations => {
            let after_prefix = trimmed[plen..].trim();
            let after_prefix_base = trim_base + byte_offset_within(trimmed, after_prefix);
            let clauses = parse_show_filter_clauses(after_prefix, kind, after_prefix_base)?;
            Ok(RewriteAction::Passthrough(lower_show(
                kind,
                &clauses,
                after_prefix_base,
            )?))
        }
        // ALTER: sub-operation dispatch (RENAME TO, SET COMMENT, UNSET COMMENT)
        DdlKind::Alter | DdlKind::AlterIfExists => rewrite_alter(trimmed, plen, kind, trim_base),
    }
}

/// Lower a name-only read statement (DESCRIBE, SHOW COLUMNS) to its read table
/// function. The name is passed raw: the table function folds and resolves it
/// at the catalog-read boundary (FF-4).
pub(crate) fn lower_name_only_read(kind: DdlKind, raw_name: &str) -> String {
    let safe_name = SqlLit::escape(raw_name);
    let fn_name = read_function_name(kind);
    format!("SELECT * FROM {fn_name}('{safe_name}')")
}

/// Lower a `SHOW SEMANTIC ...` statement, already split into its clauses, to a
/// query over the read table functions. Shared by the parser override and the
/// grammar production, so both emit the same SQL. `position` is where the
/// clauses start, for the one error found here.
pub(crate) fn lower_show(
    kind: DdlKind,
    clauses: &ShowClauses<'_>,
    position: usize,
) -> Result<String, ParseError> {
    // Validate FOR METRIC requires IN
    if clauses.for_metric.is_some() && clauses.in_view.is_none() {
        return Err(ParseError {
            message: "FOR METRIC requires IN view_name".to_string(),
            position: Some(position),
        });
    }

    // Build base SELECT
    let base = if let Some(view_name) = clauses.in_view {
        let safe_name = SqlLit::escape(view_name);
        if let Some(metric_name) = clauses.for_metric {
            let safe_metric = SqlLit::escape(metric_name);
            format!(
                "SELECT * FROM show_semantic_dimensions_for_metric('{safe_name}', '{safe_metric}')"
            )
        } else {
            let fn_name = read_function_name(kind);
            format!("SELECT * FROM {fn_name}('{safe_name}')")
        }
    } else {
        let all_fn = match kind {
            DdlKind::Show => "list_semantic_views",
            DdlKind::ShowTerse => "list_terse_semantic_views",
            DdlKind::ShowDimensions => "show_semantic_dimensions_all",
            DdlKind::ShowMetrics => "show_semantic_metrics_all",
            DdlKind::ShowFacts => "show_semantic_facts_all",
            DdlKind::ShowMaterializations => "show_semantic_materializations_all",
            _ => unreachable!(),
        };
        format!("SELECT * FROM {all_fn}()")
    };

    // Append filter suffix
    let suffix = build_filter_suffix(
        clauses.like_pattern.as_deref(),
        clauses.starts_with.as_deref(),
        clauses.limit,
        clauses.in_schema.as_ref(),
        clauses.in_database.as_ref(),
    );
    Ok(format!("{base}{suffix}"))
}

// ---------------------------------------------------------------------------
// Validation layer: ParseError, plan_rewrite
// ---------------------------------------------------------------------------

/// Structured outcome of parsing/validating a semantic-view DDL statement (AR-2).
///
/// The parser produces one of these directly. Previously it rendered a legacy
/// `SELECT * FROM <fn>('arg', ...)` string that `rewrite_to_native_sql`
/// immediately re-parsed; CREATE additionally round-tripped its definition
/// through JSON (serialize → escape → re-parse → unescape → deserialize) and the
/// `FROM YAML FILE` path smuggled its fields through a `\x01`-delimited sentinel
/// string. Carrying the structured form removes all of that for CREATE and
/// deletes the sentinel entirely.
#[derive(Debug, PartialEq)]
pub enum RewriteAction {
    /// CREATE from an in-memory definition (AS-body, or inline `FROM YAML $$..$$`).
    Create {
        name: ViewRef,
        def: Box<crate::model::SemanticViewDefinition>,
        mode: CreateMode,
    },
    /// CREATE from a YAML file, read + enriched at execution by the
    /// `__sv_compute_create_from_yaml` helper table function.
    CreateFromYamlFile {
        file_path: String,
        name: ViewRef,
        comment: String,
        mode: CreateMode,
    },
    /// DROP — native DELETE against the catalog table.
    Drop { name: ViewRef, if_exists: bool },
    /// ALTER ... RENAME TO — native UPDATE of the `name` column.
    AlterRename {
        name: ViewRef,
        new_name: ViewRef,
        if_exists: bool,
    },
    /// ALTER ... SET COMMENT — native UPDATE via `json_merge_patch`.
    AlterSetComment {
        name: ViewRef,
        comment: String,
        if_exists: bool,
    },
    /// ALTER ... UNSET COMMENT — native UPDATE via `json_merge_patch`.
    AlterUnsetComment { name: ViewRef, if_exists: bool },
    /// Read-side DDL (DESCRIBE / SHOW / SHOW COLUMNS) already lowered to final
    /// `SELECT * FROM <read_side_fn>(...)` SQL that `DuckDB` runs on the caller's
    /// connection unchanged.
    Passthrough(String),
}

impl RewriteAction {
    /// Every view reference a write DDL names — one for CREATE / DROP / ALTER,
    /// two for `ALTER … RENAME TO` (source and target, either of which may
    /// carry its own qualifier). Empty for `Passthrough`, which names nothing
    /// the write-side guards apply to.
    ///
    /// Exists so guards that hold for *all* write DDL — the single-catalog
    /// database check — are written once at the dispatch point instead of
    /// repeated in every emitter.
    // Consumed by `rewrite_to_native_sql` (extension-only); unused under
    // `cargo test`'s bundled build, where the emitters are compiled out.
    #[cfg_attr(not(feature = "extension"), allow(dead_code))]
    pub(crate) fn referenced_views(&self) -> Vec<&ViewRef> {
        match self {
            RewriteAction::Create { name, .. }
            | RewriteAction::CreateFromYamlFile { name, .. }
            | RewriteAction::Drop { name, .. }
            | RewriteAction::AlterSetComment { name, .. }
            | RewriteAction::AlterUnsetComment { name, .. } => vec![name],
            RewriteAction::AlterRename { name, new_name, .. } => vec![name, new_name],
            RewriteAction::Passthrough(_) => Vec::new(),
        }
    }
}

/// CREATE conflict mode, mirroring the three `DdlKind` CREATE variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateMode {
    Create,
    OrReplace,
    IfNotExists,
}

impl CreateMode {
    pub(crate) fn from_kind(kind: DdlKind) -> Self {
        match kind {
            DdlKind::Create => CreateMode::Create,
            DdlKind::CreateOrReplace => CreateMode::OrReplace,
            DdlKind::CreateIfNotExists => CreateMode::IfNotExists,
            _ => unreachable!("CreateMode::from_kind called with non-CREATE kind"),
        }
    }
    // Consumed by `rewrite_to_native_sql` (extension-only) when choosing the
    // INSERT shape; unused under `cargo test`'s bundled build.
    #[cfg_attr(not(feature = "extension"), allow(dead_code))]
    pub(crate) fn or_replace(self) -> bool {
        matches!(self, CreateMode::OrReplace)
    }
    #[cfg_attr(not(feature = "extension"), allow(dead_code))]
    pub(crate) fn if_not_exists(self) -> bool {
        matches!(self, CreateMode::IfNotExists)
    }
}

/// Validate a DDL statement and produce a structured [`RewriteAction`] (AR-2).
///
/// This is the main entry point for the validation layer. CREATE forms carry
/// their definition structurally (`Create` / `CreateFromYamlFile`); DROP and
/// ALTER carry structured `Drop` / `AlterRename` / `AlterSetComment` /
/// `AlterUnsetComment` variants; read-side DESCRIBE / SHOW / SHOW COLUMNS are
/// carried as `Passthrough` final SQL.
pub fn plan_rewrite(query: &str) -> Result<Option<RewriteAction>, ParseError> {
    // PA-7: blank comments once at the entry point (byte-length-preserving,
    // so every error-caret position stays valid for the original query).
    // Downstream scanners and captured expressions never see comment text —
    // a trailing `-- oops` can no longer be absorbed into a stored
    // expression or rename target.
    let blanked = crate::util::blank_sql_comments(query);
    let query = blanked.as_ref();
    let lead = skip_leading_whitespace(query);
    let trimmed = query[lead..].trim_end();
    let trimmed_no_semi = trimmed.trim_end_matches(';').trim();
    let trim_offset = lead;

    let Some((kind, plen)) = detect_ddl_prefix(trimmed_no_semi) else {
        return Ok(None);
    };

    // CREATE-with-body forms validate their clause bodies here; every other
    // recognised form is delegated to `plan_ddl`, which now carries its own
    // structured errors with caret positions resolved at the point each fault
    // is detected (R-2). The old per-arm "missing view name" pre-checks and the
    // five identical `.map_err(|e| ParseError { position: Some(trim_offset +
    // plen), .. })` wrappers — which re-manufactured one coarse position for
    // every inner fault — are gone: `plan_ddl` is the single grammar.
    match kind {
        DdlKind::Create | DdlKind::CreateOrReplace | DdlKind::CreateIfNotExists => {
            validate_create_body(trimmed_no_semi, trim_offset, plen, kind)
        }
        _ => plan_ddl(query).map(Some),
    }
}

// Cardinality inference (Phase 33) now lives in `crate::graph::cardinality`
// (moved out of `parse` in AR-1 to remove the `ddl` -> `parse` layering
// inversion — it is semantic-graph logic, not parsing). Callers use
// `crate::graph::infer_cardinality`.

#[cfg(test)]
mod tests {
    use super::*;
    // AR-1: `super::*` supplies this module's private rewrite fns;
    // `crate::parse::*` supplies the coordinator re-exports (detect_*,
    // escape helpers, PARSE_* consts) that were in scope before the split.
    use crate::parse::*;

    /// Parse a view reference for use in an expected [`RewriteAction`].
    ///
    /// Routing the expectation through the same parser the production path
    /// uses keeps `name: vref("v")` as readable as the `name: "v".to_string()`
    /// it replaced, while also pinning the qualifier slot: `vref("v")` asserts
    /// unqualified, `vref("analytics.v")` asserts the schema was captured.
    fn vref(name: &str) -> ViewRef {
        parse_view_ref(name).expect("test view reference must parse")
    }

    /// Plan a DDL statement, expecting a recognized, valid statement.
    fn plan(query: &str) -> RewriteAction {
        plan_rewrite(query)
            .expect("valid DDL")
            .expect("recognized semantic-view DDL statement")
    }

    /// Plan a read-side DDL statement (DESCRIBE / SHOW / SHOW COLUMNS) and return
    /// its final `SELECT * FROM <read_side_fn>(...)` SQL, asserting the variant
    /// is `Passthrough`.
    fn passthrough_sql(query: &str) -> String {
        match plan(query) {
            RewriteAction::Passthrough(sql) => sql,
            other => panic!("expected RewriteAction::Passthrough, got {other:?}"),
        }
    }

    // ===================================================================
    // B1 / D6: race-guard SQL shape. Pinned so a future refactor cannot
    // silently drop the existence check that protects non-IF-EXISTS DROP /
    // ALTER. The check is snapshot-consistent with the DML within an explicit
    // caller transaction; under autocommit the guard and DML auto-commit
    // separately, so a concurrent commit in that window is accepted debt
    // (FF-1 / TECH-DEBT #27).
    // ===================================================================

    // SQL-string escaping moved to the `SqlLit` newtype (R-1); its escape
    // rules are unit-tested in `src/sql_lit.rs`.

    // ===================================================================
    // AR-7: the empty `OverrideContext` + `sv_make_override_context` /
    // `sv_drop_override_context` lifecycle was retired (dead after Phase 65
    // Plan 06 moved catalog pre-checks into the emitted SQL). The FFI
    // round-trip / null-drop tests that pinned that shape were removed with
    // it; `sv_parser_override_rust` / `sv_parse_function_rust` no longer take
    // an opaque context pointer.
    // ===================================================================

    // ===================================================================
    // Phase 62 Plan 03 — sv_parse_function_rust rc=0/1/2/3 contract.
    // parse_function is reintroduced purely as the error-reporting layer
    // (caret rendering via DISPLAY_EXTENSION_ERROR + error_location).
    // parser_override now defers ALL error cases (rc=2) — the synthesised
    // SELECT error('...') workaround in sql_throwing is gone.
    // ===================================================================

    /// Helper: invoke sv_parse_function_rust with stack buffers and return
    /// (rc, error message, position). Available under default features
    /// because sv_parse_function_rust is a pure-Rust validation layer that
    /// does not touch the DuckDB C API.
    fn call_sv_parse_function(query: &str) -> (u8, String, u32) {
        let mut error_buf = vec![0_u8; 1024];
        let mut position: u32 = u32::MAX;
        let rc = unsafe {
            sv_parse_function_rust(
                query.as_ptr(),
                query.len(),
                error_buf.as_mut_ptr(),
                error_buf.len(),
                &mut position as *mut u32,
            )
        };
        // Truncate error_buf at the first NUL.
        let nul = error_buf.iter().position(|&b| b == 0).unwrap_or(0);
        let msg = String::from_utf8_lossy(&error_buf[..nul]).into_owned();
        (rc, msg, position)
    }

    #[test]
    fn sv_parse_function_rust_returns_2_for_select() {
        // Plain SELECT is not ours — defer to default parser (rc=2).
        let (rc, _msg, _pos) = call_sv_parse_function("SELECT 1;");
        assert_eq!(rc, 2, "SELECT must defer with rc=2");
    }

    #[test]
    fn sv_parse_function_rust_returns_2_for_invalid_utf8() {
        // Invalid UTF-8 bytes — defer rather than panic (rc=2).
        let bad: [u8; 5] = [0xFF, 0xFE, 0xFD, 0x00, 0x00];
        let mut error_buf = vec![0_u8; 1024];
        let mut position: u32 = u32::MAX;
        let rc = unsafe {
            sv_parse_function_rust(
                bad.as_ptr(),
                4, // exclude trailing nul, just 4 invalid bytes
                error_buf.as_mut_ptr(),
                error_buf.len(),
                &mut position as *mut u32,
            )
        };
        assert_eq!(rc, 2, "invalid UTF-8 must defer with rc=2");
    }

    #[test]
    fn sv_parse_function_rust_returns_1_with_position_for_malformed_create() {
        // CREATE prefix recognised but body mis-spelled — validate_and_rewrite
        // returns Err(ParseError) with position set. rc=1; position non-MAX.
        // We use the proven TABLSE typo (transposition) as in the existing
        // proptest at as_body_position_invariant_clause_typo.
        let query = "CREATE SEMANTIC VIEW v AS TABLSE (t);";
        let (rc, msg, pos) = call_sv_parse_function(query);
        assert_eq!(rc, 1, "malformed CREATE must return rc=1; msg={msg}");
        assert_ne!(
            pos,
            u32::MAX,
            "position must be set for malformed CREATE; msg={msg}"
        );
        assert!(!msg.is_empty(), "error message must be populated for rc=1");
    }

    #[test]
    fn sv_parse_function_rust_returns_1_for_near_miss() {
        // CRETAE is a near-miss for CREATE; detect_ddl_kind returns None,
        // detect_near_miss returns Some with position=0. rc=1; suggestion text.
        let query = "CRETAE SEMANTIC VIEW v AS TABLES (t);";
        let (rc, msg, pos) = call_sv_parse_function(query);
        assert_eq!(rc, 1, "near-miss must return rc=1; msg={msg}");
        assert_eq!(pos, 0, "near-miss position must be 0 (start of CRETAE)");
        assert!(
            msg.contains("Did you mean"),
            "near-miss must contain suggestion text; got: {msg}"
        );
    }

    #[cfg(feature = "extension")]
    #[test]
    fn sv_parser_override_rust_returns_2_for_validation_failure() {
        // Phase 62 contract change: the Err(_) branch of rewrite_to_native_sql
        // now returns rc=2 (defer) rather than synthesising a SELECT error('...')
        // statement via the deleted sql_throwing helper. parse_function picks
        // up the error reporting via caret rendering.
        let query = "CREATE SEMANTIC VIEW v AS TABLSE (t);";
        let mut sql_ptr: *mut u8 = std::ptr::null_mut();
        let mut sql_len: usize = 0;
        let mut error_buf = vec![0_u8; 1024];
        let rc = unsafe {
            sv_parser_override_rust(
                query.as_ptr(),
                query.len(),
                &mut sql_ptr as *mut *mut u8,
                &mut sql_len as *mut usize,
                error_buf.as_mut_ptr(),
                error_buf.len(),
            )
        };
        assert_eq!(
            rc, 2,
            "parser_override Err branch must defer (rc=2) so parse_function can render caret"
        );
        assert!(
            sql_ptr.is_null(),
            "no rewritten SQL must be published on rc=2"
        );
        assert_eq!(sql_len, 0, "no SQL length on rc=2");
    }

    // ===================================================================
    // FFI heap-buffer round-trip — guards against the v0.8.0 silent-
    // truncation regression. Pre-fix the SQL output went through a
    // fixed 64 KB buffer; we now hand the C++ caller an owned heap
    // pointer + length, released via sv_free_buffer.
    // ===================================================================

    #[test]
    fn leak_and_reclaim_round_trips_arbitrary_string() {
        use crate::ffi_util::{leak_bytes_to_c_buffer, reclaim_c_buffer};

        let original = "INSERT INTO _definitions VALUES ('x', '...');".repeat(4096);
        assert!(
            original.len() > 64 * 1024,
            "test input should exceed legacy cap"
        );

        let original_clone = original.clone();
        let (ptr, len) = leak_bytes_to_c_buffer(original.into_bytes());
        assert!(!ptr.is_null());
        assert_eq!(len, original_clone.len());

        // Read back exactly `len` bytes (no NUL terminator assumption).
        let recovered = unsafe { std::slice::from_raw_parts(ptr.cast_const(), len) };
        assert_eq!(recovered, original_clone.as_bytes());

        // Free.
        unsafe { reclaim_c_buffer(ptr, len) };
    }

    // ===================================================================
    // detect_semantic_view_ddl tests (multi-prefix detection)
    // ===================================================================

    #[test]
    fn test_detect_create() {
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW x (...)"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_create_or_replace() {
        assert_eq!(
            detect_semantic_view_ddl("CREATE OR REPLACE SEMANTIC VIEW x (...)"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_create_if_not_exists() {
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW IF NOT EXISTS x (...)"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_drop() {
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEW x"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_drop_if_exists() {
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEW IF EXISTS x"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_describe() {
        assert_eq!(
            detect_semantic_view_ddl("DESCRIBE SEMANTIC VIEW x"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_show() {
        assert_eq!(
            detect_semantic_view_ddl("SHOW SEMANTIC VIEWS"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_case_insensitive_all_forms() {
        assert_eq!(
            detect_semantic_view_ddl("create or replace semantic view x (...)"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("drop semantic view if exists x"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("describe semantic view x"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("show semantic views"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_whitespace_and_semicolon() {
        assert_eq!(
            detect_semantic_view_ddl("  DROP SEMANTIC VIEW x  ;"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("\n\tSHOW SEMANTIC VIEWS;\n"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_detect_non_matching() {
        assert_eq!(detect_semantic_view_ddl("SELECT 1"), PARSE_NOT_OURS);
        assert_eq!(
            detect_semantic_view_ddl("CREATE TABLE t (id INT)"),
            PARSE_NOT_OURS
        );
        assert_eq!(detect_semantic_view_ddl(""), PARSE_NOT_OURS);
    }

    #[test]
    fn test_detect_describe_must_have_view() {
        // "DESCRIBE my_table" must NOT be intercepted
        assert_eq!(
            detect_semantic_view_ddl("DESCRIBE my_table"),
            PARSE_NOT_OURS
        );
    }

    #[test]
    fn test_detect_show_must_have_views() {
        // "SHOW TABLES" must NOT be intercepted
        assert_eq!(detect_semantic_view_ddl("SHOW TABLES"), PARSE_NOT_OURS);
    }

    // ===================================================================
    // detect_ddl_kind tests
    // ===================================================================

    #[test]
    fn test_ddl_kind_create() {
        assert_eq!(
            detect_ddl_kind("CREATE SEMANTIC VIEW x (...)"),
            Some(DdlKind::Create)
        );
    }

    #[test]
    fn test_ddl_kind_create_or_replace() {
        // Must be CreateOrReplace, NOT Create
        assert_eq!(
            detect_ddl_kind("CREATE OR REPLACE SEMANTIC VIEW x (...)"),
            Some(DdlKind::CreateOrReplace)
        );
    }

    #[test]
    fn test_ddl_kind_create_if_not_exists() {
        // Must be CreateIfNotExists, NOT Create
        assert_eq!(
            detect_ddl_kind("CREATE SEMANTIC VIEW IF NOT EXISTS x (...)"),
            Some(DdlKind::CreateIfNotExists)
        );
    }

    #[test]
    fn test_ddl_kind_drop() {
        assert_eq!(detect_ddl_kind("DROP SEMANTIC VIEW x"), Some(DdlKind::Drop));
    }

    #[test]
    fn test_ddl_kind_drop_if_exists() {
        // Must be DropIfExists, NOT Drop
        assert_eq!(
            detect_ddl_kind("DROP SEMANTIC VIEW IF EXISTS x"),
            Some(DdlKind::DropIfExists)
        );
    }

    #[test]
    fn test_ddl_kind_describe() {
        assert_eq!(
            detect_ddl_kind("DESCRIBE SEMANTIC VIEW x"),
            Some(DdlKind::Describe)
        );
    }

    #[test]
    fn test_ddl_kind_show() {
        assert_eq!(detect_ddl_kind("SHOW SEMANTIC VIEWS"), Some(DdlKind::Show));
    }

    #[test]
    fn test_ddl_kind_none() {
        assert_eq!(detect_ddl_kind("SELECT 1"), None);
    }

    // ===================================================================
    // rewrite_ddl tests (name-only and no-args forms only; CREATE rejected)
    // ===================================================================

    #[test]
    #[should_panic(expected = "CREATE routes via plan_rewrite")]
    fn test_plan_ddl_create_is_unreachable() {
        // CREATE never reaches `plan_ddl` in a well-formed call graph
        // (`plan_rewrite` routes it to `validate_create_body`); the exhaustive
        // match arm is `unreachable!`. Calling it directly with CREATE trips
        // that invariant — the contract is expressed in the type/panic, not a
        // user-facing error string (R-2).
        let _ = plan_ddl("CREATE SEMANTIC VIEW sales (tables := [...], dimensions := [...])");
    }

    #[test]
    fn test_rewrite_drop() {
        assert_eq!(
            plan("DROP SEMANTIC VIEW sales"),
            RewriteAction::Drop {
                name: vref("sales"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_rewrite_drop_if_exists() {
        assert_eq!(
            plan("DROP SEMANTIC VIEW IF EXISTS sales"),
            RewriteAction::Drop {
                name: vref("sales"),
                if_exists: true,
            }
        );
    }

    #[test]
    fn test_rewrite_describe() {
        let sql = passthrough_sql("DESCRIBE SEMANTIC VIEW sales");
        assert_eq!(sql, "SELECT * FROM describe_semantic_view('sales')");
    }

    #[test]
    fn test_rewrite_show() {
        let sql = passthrough_sql("SHOW SEMANTIC VIEWS");
        assert_eq!(sql, "SELECT * FROM list_semantic_views()");
    }

    #[test]
    fn test_rewrite_name_with_single_quote() {
        // Structured variants carry the RAW name (single quote NOT doubled).
        assert_eq!(
            plan("DROP SEMANTIC VIEW it's_a_view"),
            RewriteAction::Drop {
                name: vref("it's_a_view"),
                if_exists: false,
            }
        );
        // The same raw name is accepted at CREATE (TECH-DEBT #98: DuckDB's
        // tokenizer, and so the grammar, refuses it; corpus row S-014).
        let created = plan(
            "CREATE SEMANTIC VIEW it's_a_view AS TABLES (o AS orders PRIMARY KEY (id)) \
             DIMENSIONS (o.region AS o.region)",
        );
        assert!(
            matches!(&created, RewriteAction::Create { name, .. } if *name == vref("it's_a_view")),
            "got {created:?}"
        );
    }

    // ===================================================================
    // PA-8 (code-review 2026-07-02; case rule revised 2026-07-12): view names
    // fold to lowercase at every DDL capture site, whether quoted or not —
    // DuckDB treats even double-quoted identifiers as case-insensitive.
    // Previously unquoted names were byte-exact case-sensitive
    // (CREATE ... Sales / DROP ... sales -> "does not exist").
    // ===================================================================

    #[test]
    fn test_unquoted_names_fold_to_lowercase_across_ddl_forms() {
        assert_eq!(
            plan("DROP SEMANTIC VIEW Sales"),
            RewriteAction::Drop {
                name: vref("sales"),
                if_exists: false,
            }
        );

        // FF-4: DESCRIBE / SHOW COLUMNS now embed the RAW name and fold at the
        // TF dispatcher (like `semantic_view`), so the rewrite carries 'SALES'
        // verbatim; the dispatcher's normalize_view_name resolves it to 'sales'.
        let sql = passthrough_sql("DESCRIBE SEMANTIC VIEW SALES");
        assert_eq!(sql, "SELECT * FROM describe_semantic_view('SALES')");

        // CREATE folds the (bare, normalized) name carried in RewriteAction.
        let RewriteAction::Create { name, .. } = plan(
            "CREATE SEMANTIC VIEW Sales AS TABLES (o AS orders PRIMARY KEY (id)) \
             DIMENSIONS (o.region AS o.region) METRICS (o.total AS SUM(o.amount))",
        ) else {
            panic!("expected RewriteAction::Create");
        };
        assert_eq!(name, vref("sales"));

        // ALTER folds both the target and the RENAME TO name.
        assert_eq!(
            plan("ALTER SEMANTIC VIEW Sales RENAME TO NewSales"),
            RewriteAction::AlterRename {
                name: vref("sales"),
                new_name: vref("newsales"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_quoted_names_also_fold_to_lowercase_across_ddl_forms() {
        // DuckDB: quoted identifiers are case-insensitive too, so a quoted
        // view name folds to lowercase exactly like an unquoted one (revised
        // 2026-07-12, replacing the Snowflake preserve-quoted rule).
        assert_eq!(
            plan("DROP SEMANTIC VIEW \"Sales\""),
            RewriteAction::Drop {
                name: vref("sales"),
                if_exists: false,
            }
        );

        // A quoted CREATE name folds to lowercase exactly like an unquoted one.
        let RewriteAction::Create { name, .. } = plan(
            "CREATE SEMANTIC VIEW \"Sales\" AS TABLES (o AS orders PRIMARY KEY (id)) \
             DIMENSIONS (o.region AS o.region) METRICS (o.total AS SUM(o.amount))",
        ) else {
            panic!("expected RewriteAction::Create");
        };
        assert_eq!(name, vref("sales"));

        assert_eq!(
            plan("ALTER SEMANTIC VIEW \"Sales\" RENAME TO \"NewSales\""),
            RewriteAction::AlterRename {
                name: vref("sales"),
                new_name: vref("newsales"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_rewrite_drop_missing_name() {
        let err = plan_ddl("DROP SEMANTIC VIEW").unwrap_err();
        assert!(err.message.contains("Missing view name"), "got: {err}");
    }

    // ===================================================================
    // PA-4 (code-review 2026-07-02): prefix keywords require a trailing
    // word boundary. `DROP SEMANTIC VIEWS` (plural typo) used to match the
    // `DROP SEMANTIC VIEW` prefix and drop a view named `s` [verified];
    // `CREATE SEMANTIC VIEWfoo` parsed as CREATE.
    // ===================================================================

    #[test]
    fn test_prefix_requires_trailing_word_boundary() {
        // The plural typo must NOT be detected as ours at all — DuckDB's
        // own parser error is the correct surface.
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEWS x"),
            PARSE_NOT_OURS
        );
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEWS"),
            PARSE_NOT_OURS
        );
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEWfoo (TABLES (o AS orders))"),
            PARSE_NOT_OURS
        );
        assert_eq!(
            detect_semantic_view_ddl("DESCRIBE SEMANTIC VIEWER x"),
            PARSE_NOT_OURS
        );
        // Non-ASCII continuation is an identifier character in DuckDB, not
        // a boundary.
        assert_eq!(
            detect_semantic_view_ddl("SHOW SEMANTIC VIEWSé"),
            PARSE_NOT_OURS
        );
        // Digits and underscore are identifier continuation too.
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEW2 x"),
            PARSE_NOT_OURS
        );
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEW_ x"),
            PARSE_NOT_OURS
        );
    }

    #[test]
    fn test_prefix_accepts_punctuation_boundaries() {
        // `(`, `;`, `"` and end-of-input are legitimate token boundaries.
        assert_eq!(
            detect_semantic_view_ddl("SHOW SEMANTIC VIEWS"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("SHOW SEMANTIC VIEWS;"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("DROP SEMANTIC VIEW\"my view\""),
            PARSE_DETECTED
        );
        assert_eq!(
            plan("DROP SEMANTIC VIEW\"my view\""),
            RewriteAction::Drop {
                name: vref("my view"),
                if_exists: false,
            }
        );
    }

    // ===================================================================
    // PA-7 (code-review 2026-07-02): SQL comments are blanked once at the
    // entry points, so they can no longer corrupt scanning state or be
    // absorbed into stored expressions / rename targets.
    // ===================================================================

    #[test]
    fn test_trailing_comment_after_name_is_ignored() {
        assert_eq!(
            plan("DROP SEMANTIC VIEW a -- trailing comment"),
            RewriteAction::Drop {
                name: vref("a"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_alter_rename_trailing_comment_not_absorbed() {
        // Pre-fix this renamed the view to `x -- oops`.
        assert_eq!(
            plan("ALTER SEMANTIC VIEW a RENAME TO x -- oops"),
            RewriteAction::AlterRename {
                name: vref("a"),
                new_name: vref("x"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_comment_between_prefix_keywords() {
        assert_eq!(
            detect_semantic_view_ddl("DROP /* which? */ SEMANTIC VIEW a"),
            PARSE_DETECTED
        );
        assert_eq!(
            plan("DROP /* which? */ SEMANTIC VIEW a"),
            RewriteAction::Drop {
                name: vref("a"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_comment_markers_inside_string_survive() {
        // `--` inside a COMMENT string literal is content, not a comment.
        assert_eq!(
            plan("ALTER SEMANTIC VIEW a SET COMMENT = 'keep -- this /* too */'"),
            RewriteAction::AlterSetComment {
                name: vref("a"),
                comment: "keep -- this /* too */".to_string(),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_nested_block_comment_fully_skipped() {
        // Block comments nest per the SQL standard (PA-10).
        assert_eq!(
            plan("/* outer /* inner */ still comment */ DROP SEMANTIC VIEW a"),
            RewriteAction::Drop {
                name: vref("a"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_comment_inside_create_body_not_stored() {
        // A line comment inside the AS-body must not leak into the stored
        // expression (pre-fix it commented out generated SQL downstream).
        let RewriteAction::Create { def, .. } = plan(
            "CREATE SEMANTIC VIEW v AS TABLES (o AS orders PRIMARY KEY (id)) \
             DIMENSIONS (o.d AS o.region /* region code */)",
        ) else {
            panic!("expected RewriteAction::Create");
        };
        assert!(
            !def.dimensions
                .iter()
                .any(|d| d.expr.contains("region code") || d.name.contains("region code")),
            "comment text leaked into stored dimension: {def:?}"
        );
    }

    // ===================================================================
    // PA-10 (code-review 2026-07-02): boundary/whitespace nits.
    // ===================================================================

    #[test]
    fn test_alter_subops_tolerate_multiple_spaces() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW a RENAME  \t TO b"),
            RewriteAction::AlterRename {
                name: vref("a"),
                new_name: vref("b"),
                if_exists: false,
            }
        );
        assert_eq!(
            plan("ALTER SEMANTIC VIEW a SET   COMMENT = 'x'"),
            RewriteAction::AlterSetComment {
                name: vref("a"),
                comment: "x".to_string(),
                if_exists: false,
            }
        );
        assert_eq!(
            plan("ALTER SEMANTIC VIEW a UNSET\tCOMMENT"),
            RewriteAction::AlterUnsetComment {
                name: vref("a"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_alter_subops_reject_trailing_garbage() {
        assert!(plan_ddl("ALTER SEMANTIC VIEW a RENAME TO x oops").is_err());
        assert!(plan_ddl("ALTER SEMANTIC VIEW a SET COMMENT = 'x' oops").is_err());
        assert!(plan_ddl("ALTER SEMANTIC VIEW a UNSET COMMENT oops").is_err());
    }

    #[test]
    fn test_validate_alter_agrees_with_rewriter() {
        // PR #50 self-review: validate_alter was a second, drifted grammar
        // implementation — it required single-space sub-ops and split
        // quoted names at their inner space, so validate_and_rewrite (which
        // validates BEFORE rewriting) rejected inputs the rewriter accepts.
        // Flexible inter-keyword whitespace must pass the full pipeline.
        for q in [
            "ALTER SEMANTIC VIEW v RENAME  TO w",
            "ALTER SEMANTIC VIEW v RENAME\tTO w",
            "ALTER SEMANTIC VIEW v SET   COMMENT = 'x'",
            "ALTER SEMANTIC VIEW v UNSET\tCOMMENT",
        ] {
            assert!(
                plan_rewrite(q).is_ok(),
                "plan_rewrite rejected valid ALTER: {q}"
            );
        }
        // A quoted view name with an inner space must survive the validate
        // pass (previously split at the space -> "Unsupported ALTER").
        assert_eq!(
            plan("ALTER SEMANTIC VIEW \"my view\" RENAME TO w"),
            RewriteAction::AlterRename {
                name: vref("my view"),
                new_name: vref("w"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_for_metric_requires_boundaries() {
        // FOREIGN must not match the FOR clause keyword (PR #50 review).
        let err = plan_ddl("SHOW SEMANTIC VIEWS FOREIGN").unwrap_err();
        assert!(err.message.contains("Unexpected tokens"), "got: {err}");
        // METRICS must not match METRIC (the metric would have been 's').
        let err = plan_ddl("SHOW SEMANTIC DIMENSIONS IN v FOR METRICS revenue").unwrap_err();
        assert!(err.message.contains("Expected FOR METRIC"), "got: {err}");
        // The legal form still parses.
        let sql = passthrough_sql("SHOW SEMANTIC DIMENSIONS IN v FOR METRIC revenue");
        assert_eq!(
            sql,
            "SELECT * FROM show_semantic_dimensions_for_metric('v', 'revenue')"
        );
    }

    #[test]
    fn test_starts_with_and_limit_require_boundaries() {
        // STARTSWITH / LIMIT5 used to be accepted without a word boundary.
        assert!(plan_ddl("SHOW SEMANTIC VIEWS STARTSWITH 'a'").is_err());
        assert!(plan_ddl("SHOW SEMANTIC VIEWS LIMIT5").is_err());
        // `_` and non-ASCII bytes are identifier continuation, not
        // boundaries (PR #50 review).
        assert!(plan_ddl("SHOW SEMANTIC VIEWS STARTS WITH_x 'a'").is_err());
        assert!(plan_ddl("SHOW SEMANTIC VIEWS STARTS WITHé 'a'").is_err());
        // The legal forms still parse.
        let sql = passthrough_sql("SHOW SEMANTIC VIEWS STARTS WITH 'a' LIMIT 5");
        assert_eq!(
            sql,
            "SELECT * FROM list_semantic_views() WHERE starts_with(name, 'a') LIMIT 5"
        );
    }

    // ===================================================================
    // PA-5 (code-review 2026-07-02): name-only forms must reject trailing
    // garbage instead of executing and silently discarding it.
    // ===================================================================

    #[test]
    fn test_name_only_forms_reject_trailing_garbage() {
        for q in [
            "DROP SEMANTIC VIEW a b c",
            "DROP SEMANTIC VIEW IF EXISTS a b",
            "DESCRIBE SEMANTIC VIEW a CASCADE",
            "SHOW COLUMNS IN SEMANTIC VIEW a b",
        ] {
            let err = plan_ddl(q).unwrap_err();
            assert!(
                err.message.contains("Unexpected tokens after view name"),
                "expected trailing-garbage error for {q}, got: {err}"
            );
        }
    }

    #[test]
    fn test_rewrite_not_semantic() {
        let err = plan_ddl("SELECT 1").unwrap_err();
        assert!(
            err.message.contains("Not a semantic view DDL"),
            "got: {err}"
        );
    }

    // ===================================================================
    // Additional detect_semantic_view_ddl coverage (legacy test cases)
    // ===================================================================

    #[test]
    fn test_basic_detection() {
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW test (...)"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_case_insensitive() {
        assert_eq!(
            detect_semantic_view_ddl("create semantic view test"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("Create Semantic View test"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("CREATE semantic VIEW test"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_leading_whitespace() {
        assert_eq!(
            detect_semantic_view_ddl("  CREATE SEMANTIC VIEW test"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("\n\tCREATE SEMANTIC VIEW test"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_trailing_semicolon() {
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW test;"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW test ;"),
            PARSE_DETECTED
        );
        assert_eq!(
            detect_semantic_view_ddl("CREATE SEMANTIC VIEW test ;\n"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn test_non_matching() {
        assert_eq!(detect_semantic_view_ddl("SELECT 1"), PARSE_NOT_OURS);
        assert_eq!(
            detect_semantic_view_ddl("CREATE TABLE test"),
            PARSE_NOT_OURS
        );
        assert_eq!(detect_semantic_view_ddl("CREATE VIEW test"), PARSE_NOT_OURS);
        assert_eq!(detect_semantic_view_ddl(""), PARSE_NOT_OURS);
        assert_eq!(detect_semantic_view_ddl(";"), PARSE_NOT_OURS);
        assert_eq!(detect_semantic_view_ddl("CREATE"), PARSE_NOT_OURS);
    }

    #[test]
    fn test_too_short() {
        assert_eq!(
            detect_semantic_view_ddl("create semantic vie"),
            PARSE_NOT_OURS
        );
    }

    #[test]
    fn test_exact_prefix_only() {
        assert_eq!(
            detect_semantic_view_ddl("create semantic view"),
            PARSE_DETECTED
        );
    }

    // ===================================================================
    // plan_rewrite tests
    // ===================================================================

    #[test]
    fn test_validate_and_rewrite_rejects_paren_body() {
        // CLN-01: non-AS-body syntax rejected with clear error
        let result =
            plan_rewrite("CREATE SEMANTIC VIEW sales (tables := [...], dimensions := [...])");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("Expected 'AS' or 'FROM YAML'"),
            "Expected 'Expected AS or FROM YAML' error, got: {}",
            err.message
        );
    }

    #[test]
    fn test_validate_and_rewrite_not_ours() {
        let result = plan_rewrite("SELECT 1");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_validate_and_rewrite_drop() {
        // Non-CREATE forms should pass through without clause validation
        let result = plan_rewrite("DROP SEMANTIC VIEW x");
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_validate_and_rewrite_show() {
        let result = plan_rewrite("SHOW SEMANTIC VIEWS");
        assert!(result.is_ok());
        assert!(result.unwrap().is_some());
    }

    #[test]
    fn test_validate_and_rewrite_describe() {
        let result = plan_rewrite("DESCRIBE SEMANTIC VIEW sv1");
        assert!(result.is_ok());
        let sql = result.unwrap();
        assert!(
            sql.is_some(),
            "Expected Some(rewritten action) for DESCRIBE"
        );
    }

    #[test]
    fn test_validate_and_rewrite_drop_if_exists() {
        let result = plan_rewrite("DROP SEMANTIC VIEW IF EXISTS sv1");
        assert!(result.is_ok());
        let sql = result.unwrap();
        assert!(
            sql.is_some(),
            "Expected Some(rewritten action) for DROP IF EXISTS"
        );
    }

    // ===================================================================
    // detect_near_miss tests
    // ===================================================================

    #[test]
    fn test_near_miss_creat() {
        let result = detect_near_miss("CREAT SEMANTIC VIEW x (tables := [])");
        assert!(result.is_some());
        let err = result.unwrap();
        assert!(
            err.message.contains("Did you mean")
                && err.message.to_lowercase().contains("create semantic view"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_near_miss_drop_semantc() {
        let result = detect_near_miss("DROP SEMANTC VIEW x");
        assert!(result.is_some());
        let err = result.unwrap();
        assert!(
            err.message.contains("Did you mean")
                && err.message.to_lowercase().contains("drop semantic view"),
            "got: {}",
            err.message
        );
    }

    #[test]
    fn test_near_miss_show_semantic_view() {
        // "SHOW SEMANTIC VIEW" (missing 'S') should suggest "SHOW SEMANTIC VIEWS"
        let result = detect_near_miss("SHOW SEMANTIC VIEW");
        assert!(result.is_some());
        let err = result.unwrap();
        assert!(err.message.contains("Did you mean"), "got: {}", err.message);
    }

    #[test]
    fn test_near_miss_select() {
        // Regular SQL should NOT trigger near-miss
        let result = detect_near_miss("SELECT 1");
        assert!(result.is_none());
    }

    #[test]
    fn test_near_miss_show_tables() {
        // "SHOW TABLES" has too large edit distance from any DDL prefix
        let result = detect_near_miss("SHOW TABLES");
        assert!(result.is_none());
    }

    #[test]
    fn test_near_miss_position_zero() {
        let result = detect_near_miss("CREAT SEMANTIC VIEW x ()");
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.position, Some(0));
    }

    // ===================================================================
    // ParseError position tests
    // ===================================================================

    #[test]
    fn test_parse_error_position_paren_body_rejected() {
        // Non-AS-body syntax returns "Expected 'AS' or 'FROM YAML'" error with position
        let query = "CREATE SEMANTIC VIEW x (tables := [])";
        let result = plan_rewrite(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.message.contains("Expected 'AS' or 'FROM YAML'"),
            "got: {}",
            err.message
        );
        assert!(err.position.is_some());
    }

    #[test]
    fn test_parse_error_position_structural() {
        // For missing name, position should point at end of prefix
        let query = "CREATE SEMANTIC VIEW";
        let result = plan_rewrite(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.position.is_some());
    }

    // R-2: with errors carried from their origin (rather than one coarse
    // `Some(trim_offset + plen)` manufactured for every inner fault), the caret
    // now points at the offending token itself. Each case asserts the byte the
    // position lands on so a future coarsening regresses loudly.
    #[test]
    fn parse_error_position_points_at_offending_token() {
        // Trailing garbage after a DROP name → caret at the garbage, not the prefix.
        let q = "DROP SEMANTIC VIEW foo bar";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(
            &q[pos..pos + 3],
            "bar",
            "caret should sit on the trailing token"
        );

        // Unsupported ALTER sub-operation → caret at the bad keyword.
        let q = "ALTER SEMANTIC VIEW v TRUNCATE";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(
            &q[pos..pos + 8],
            "TRUNCATE",
            "caret should sit on the sub-op"
        );

        // Unexpected SHOW clause token → caret at the token.
        let q = "SHOW SEMANTIC VIEWS FOREIGN";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(
            &q[pos..pos + 7],
            "FOREIGN",
            "caret should sit on the clause token"
        );

        // Non-numeric LIMIT → caret at the offending value.
        let q = "SHOW SEMANTIC VIEWS LIMIT abc";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(
            &q[pos..pos + 3],
            "abc",
            "caret should sit on the LIMIT value"
        );

        // Trailing garbage after a RENAME target → caret at the garbage.
        let q = "ALTER SEMANTIC VIEW v RENAME TO w oops";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(
            &q[pos..pos + 4],
            "oops",
            "caret should sit on the trailing token"
        );

        // Invalid view name → caret at the name token, not the end of the
        // prefix. Extra whitespace after the prefix makes the two differ, so
        // this pins that `extract_name_only` resolves from the trimmed slice.
        let q = "DROP SEMANTIC VIEW   \"unterminated";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(&q[pos..=pos], "\"", "caret should sit on the name token");
    }

    #[test]
    fn parse_error_position_survives_leading_comment() {
        // The offending-token caret is an offset into the ORIGINAL query, so a
        // length-preserving comment blank must not shift it.
        let q = "/* hi */ SHOW SEMANTIC VIEWS FOREIGN";
        let pos = plan_rewrite(q).unwrap_err().position.expect("position set");
        assert_eq!(&q[pos..pos + 7], "FOREIGN");
    }

    // ===================================================================
    // Phase 25 Plan 03: AS-body dispatch tests
    // ===================================================================

    mod phase25_parse_tests {
        use super::*;

        #[test]
        fn as_body_create_rewrites_to_from_json() {
            let query = "CREATE SEMANTIC VIEW v AS TABLES (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))";
            let RewriteAction::Create { name, mode, .. } = plan(query) else {
                panic!("expected RewriteAction::Create");
            };
            assert_eq!(mode, CreateMode::Create);
            assert_eq!(name, vref("v"), "Must carry view name");
        }

        #[test]
        fn as_body_create_or_replace_rewrites_to_from_json() {
            let query = "CREATE OR REPLACE SEMANTIC VIEW v AS TABLES (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))";
            assert!(matches!(
                plan(query),
                RewriteAction::Create {
                    mode: CreateMode::OrReplace,
                    ..
                }
            ));
        }

        #[test]
        fn as_body_create_if_not_exists_rewrites_to_from_json() {
            let query = "CREATE SEMANTIC VIEW IF NOT EXISTS v AS TABLES (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))";
            assert!(matches!(
                plan(query),
                RewriteAction::Create {
                    mode: CreateMode::IfNotExists,
                    ..
                }
            ));
        }

        #[test]
        fn old_paren_body_is_rejected() {
            // CLN-01: non-AS-body syntax rejected with clear error
            let query = "CREATE SEMANTIC VIEW v (tables := [], dimensions := [])";
            let result = plan_rewrite(query);
            assert!(result.is_err(), "Paren-body must be rejected: {result:?}");
            let err = result.unwrap_err();
            assert!(
                err.message.contains("Expected 'AS' or 'FROM YAML'"),
                "Expected 'Expected AS or FROM YAML' error, got: {}",
                err.message
            );
        }

        #[test]
        fn drop_still_rewrites_unchanged() {
            let query = "DROP SEMANTIC VIEW v";
            assert_eq!(
                plan(query),
                RewriteAction::Drop {
                    name: vref("v"),
                    if_exists: false,
                }
            );
        }
    }

    // ===================================================================
    // Phase 33: Cardinality inference tests
    // ===================================================================

    mod phase33_inference_tests {
        use super::*;
        use crate::graph::infer_cardinality;
        use crate::model::{Cardinality, Join, TableRef};

        fn make_table(alias: &str, pk: &[&str], unique: &[&[&str]]) -> TableRef {
            TableRef {
                alias: alias.to_string(),
                table: alias.to_string(),
                pk_columns: pk.iter().map(|s| (*s).to_string()).collect(),
                unique_constraints: unique
                    .iter()
                    .map(|cols| cols.iter().map(|s| (*s).to_string()).collect())
                    .collect(),
                comment: None,
                synonyms: vec![],
            }
        }

        fn make_join(name: &str, from: &str, to: &str, fk: &[&str], ref_cols: &[&str]) -> Join {
            Join {
                name: Some(name.to_string()),
                from_alias: from.to_string(),
                table: to.to_string(),
                fk_columns: fk.iter().map(|s| (*s).to_string()).collect(),
                ref_columns: ref_cols.iter().map(|s| (*s).to_string()).collect(),
                ..Default::default()
            }
        }

        #[test]
        fn resolves_ref_columns_to_target_pk() {
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("customers", &["cust_id"], &[]),
            ];
            let mut rels = vec![make_join("r", "orders", "customers", &["customer_id"], &[])];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].ref_columns, vec!["cust_id"]);
        }

        #[test]
        fn keeps_explicit_ref_columns() {
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("customers", &["cust_id"], &[&["email"]]),
            ];
            let mut rels = vec![make_join(
                "r",
                "orders",
                "customers",
                &["customer_email"],
                &["email"],
            )];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].ref_columns, vec!["email"]);
        }

        #[cfg(feature = "extension")]
        #[test]
        fn errors_when_target_has_no_pk_and_no_explicit_ref() {
            // Phase 65 (D-05/D-06): `infer_cardinality` still silently
            // skips the join (leaves ref_columns empty); the hard error
            // fires inside `enrich_definition_for_create` step 2 with the
            // D-06 actionable message. v0.9.0's resolve_pk_from_catalog
            // catalog fallback is gone (D-05).
            //
            // Feature-gated on `extension` because `crate::ddl` lives
            // under `#[cfg(feature = "extension")]` (src/lib.rs:283-284).
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("events", &[], &[]), // no PK declared
            ];
            let mut rels = vec![make_join("r", "orders", "events", &["event_id"], &[])];
            // infer_cardinality itself remains tolerant: skips the join.
            infer_cardinality(&tables, &mut rels).unwrap();
            assert!(
                rels[0].ref_columns.is_empty(),
                "ref_columns should remain empty when target has no PK"
            );

            // enrich_definition_for_create step 2 fires the D-06 hard
            // error. Phase 65 (Plan 03): the function no longer takes a
            // `conn` arg or `infer_types` flag — all CREATE-time catalog
            // access has been removed.
            let def = crate::model::SemanticViewDefinition {
                tables,
                joins: rels,
                ..Default::default()
            };
            let err = crate::ddl::define::enrich_definition_for_create("v_bad", def)
                .expect_err("D-06 hard error must fire for FK→no-PK")
                .message;
            assert!(
                err.contains("has no PRIMARY KEY declared but is referenced by FK in"),
                "D-06 substring missing in error: {err}"
            );
            assert!(
                err.contains(
                    "(v0.10.0: physical-catalog PK auto-inference removed -- see CHANGELOG.)"
                ),
                "D-06 CHANGELOG parenthetical missing in error: {err}"
            );
        }

        #[cfg(feature = "extension")]
        #[test]
        fn enrich_rejects_cross_kind_name_collision() {
            // SG-13 (code review 2026-07-02): dimension/metric/fact names
            // share one request namespace, so define-time validation
            // (enrich_definition_for_create -> validate_name_uniqueness)
            // must reject a dimension and metric sharing a name, even when
            // no derived metrics exist (the old check was gated on them).
            let def = crate::model::SemanticViewDefinition {
                tables: vec![make_table("orders", &["id"], &[])],
                dimensions: vec![crate::model::Dimension {
                    name: "region".to_string(),
                    expr: "orders.region".to_string(),
                    source_table: Some("orders".to_string()),
                    ..Default::default()
                }],
                metrics: vec![crate::model::Metric {
                    name: "REGION".to_string(),
                    expr: "count(orders.region)".to_string(),
                    source_table: Some("orders".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            };
            let err = crate::ddl::define::enrich_definition_for_create("v_dup", def)
                .expect_err("cross-kind name collision must be rejected at define time")
                .message;
            assert!(
                err.contains("duplicate name 'REGION'")
                    && err.contains("metric 'REGION' collides with dimension 'region'"),
                "SG-13 error shape mismatch: {err}"
            );
        }

        #[test]
        fn infers_one_to_one_from_pk_match() {
            // orders PK is (id), FK is (id) -> OneToOne
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("customers", &["cust_id"], &[]),
            ];
            let mut rels = vec![make_join("r", "orders", "customers", &["id"], &[])];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].cardinality, Cardinality::OneToOne);
        }

        #[test]
        fn infers_one_to_one_from_unique_match() {
            // orders has UNIQUE(email), FK is (email) -> OneToOne
            let tables = vec![
                make_table("orders", &["id"], &[&["email"]]),
                make_table("customers", &["cust_id"], &[]),
            ];
            let mut rels = vec![make_join("r", "orders", "customers", &["email"], &[])];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].cardinality, Cardinality::OneToOne);
        }

        #[test]
        fn infers_many_to_one_when_fk_is_bare() {
            // orders PK is (id), FK is (customer_id) -- doesn't match PK or UNIQUE
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("customers", &["cust_id"], &[]),
            ];
            let mut rels = vec![make_join("r", "orders", "customers", &["customer_id"], &[])];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].cardinality, Cardinality::ManyToOne);
        }

        #[test]
        fn case_insensitive_column_matching() {
            // PK is (ID) uppercase, FK is (id) lowercase -> should still match OneToOne
            let tables = vec![
                make_table("orders", &["ID"], &[]),
                make_table("customers", &["cust_id"], &[]),
            ];
            let mut rels = vec![make_join("r", "orders", "customers", &["id"], &[])];
            infer_cardinality(&tables, &mut rels).unwrap();
            assert_eq!(rels[0].cardinality, Cardinality::OneToOne);
        }

        #[test]
        fn fk_ref_column_count_mismatch_error() {
            let tables = vec![
                make_table("orders", &["id"], &[]),
                make_table("customers", &["a", "b"], &[]),
            ];
            // FK has 1 col, target PK has 2 cols
            let mut rels = vec![make_join("r", "orders", "customers", &["customer_id"], &[])];
            let err = infer_cardinality(&tables, &mut rels).unwrap_err();
            assert!(
                err.message.contains("FK column count"),
                "Expected FK column count error, got: {}",
                err.message
            );
        }

        #[test]
        fn rewrite_produces_json_with_ref_columns_and_cardinality() {
            let query = "CREATE SEMANTIC VIEW v AS \
                         TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (cust_id)) \
                         RELATIONSHIPS (r AS o(customer_id) REFERENCES c) \
                         DIMENSIONS (o.region AS region) \
                         METRICS (o.revenue AS SUM(amount))";
            let RewriteAction::Create { def, .. } = plan(query) else {
                panic!("expected RewriteAction::Create");
            };
            // The join's ref_columns should be resolved from the target PK.
            assert_eq!(def.joins.len(), 1, "Expected one join, got: {def:?}");
            assert_eq!(
                def.joins[0].ref_columns,
                vec!["cust_id".to_string()],
                "Expected target PK 'cust_id' in ref_columns, got: {:?}",
                def.joins[0].ref_columns
            );
        }
    }

    // ===================================================================
    // Phase 34.1.1: SHOW SEMANTIC filter clause tests
    // ===================================================================

    mod phase34_1_1_show_filter_tests {
        use super::*;

        // --- extract_quoted_string tests ---

        #[test]
        fn test_extract_quoted_string_normal() {
            let (s, n) = extract_quoted_string("'hello'").unwrap();
            assert_eq!(s, "hello");
            assert_eq!(n, 7);
        }

        #[test]
        fn test_extract_quoted_string_escaped_quotes() {
            let (s, n) = extract_quoted_string("'O''Brien'").unwrap();
            assert_eq!(s, "O'Brien");
            assert_eq!(n, 10);
        }

        #[test]
        fn test_extract_quoted_string_empty() {
            let (s, n) = extract_quoted_string("''").unwrap();
            assert_eq!(s, "");
            assert_eq!(n, 2);
        }

        #[test]
        fn test_extract_quoted_string_unterminated() {
            let result = extract_quoted_string("'unterminated");
            assert!(result.is_err());
        }

        #[test]
        fn test_extract_quoted_string_no_opening_quote() {
            let result = extract_quoted_string("no_quote");
            assert!(result.is_err());
        }

        // Phase 65.1 WR-04: round-trip non-ASCII payloads through the
        // quoted-string extractor. The previous `bytes[pos] as char`
        // implementation silently corrupted multi-byte UTF-8 sequences
        // into the Latin-1 supplement region (U+0080..U+00FF).
        #[test]
        fn test_extract_quoted_string_utf8_cyrillic() {
            let input = "'Привет'"; // Russian "Hello"
            let (s, n) = extract_quoted_string(input).unwrap();
            assert_eq!(s, "Привет");
            assert_eq!(n, input.len());
        }

        #[test]
        fn test_extract_quoted_string_utf8_cjk_and_emoji() {
            let input = "'你好 🦆'";
            let (s, n) = extract_quoted_string(input).unwrap();
            assert_eq!(s, "你好 🦆");
            assert_eq!(n, input.len());
        }

        #[test]
        fn test_extract_quoted_string_utf8_em_dash_and_smart_quotes() {
            let input = "'a — b “c” d'";
            let (s, n) = extract_quoted_string(input).unwrap();
            assert_eq!(s, "a — b “c” d");
            assert_eq!(n, input.len());
        }

        #[test]
        fn test_extract_quoted_string_utf8_escaped_quotes_around_nonascii() {
            // SQL '' escaping preserved when the surrounding payload is
            // non-ASCII.
            let input = "'café ''noir'''";
            let (s, n) = extract_quoted_string(input).unwrap();
            assert_eq!(s, "café 'noir'");
            assert_eq!(n, input.len());
        }

        // --- build_filter_suffix tests ---

        #[test]
        fn test_build_filter_suffix_like_only() {
            assert_eq!(
                build_filter_suffix(Some("%rev%"), None, None, None, None),
                " WHERE name ILIKE '%rev%'"
            );
        }

        #[test]
        fn test_build_filter_suffix_starts_with_only() {
            assert_eq!(
                build_filter_suffix(None, Some("total"), None, None, None),
                " WHERE starts_with(name, 'total')"
            );
        }

        #[test]
        fn test_build_filter_suffix_limit_only() {
            assert_eq!(
                build_filter_suffix(None, None, Some(5), None, None),
                " LIMIT 5"
            );
        }

        #[test]
        fn test_build_filter_suffix_all_three() {
            assert_eq!(
                build_filter_suffix(Some("%x%"), Some("a"), Some(10), None, None),
                " WHERE name ILIKE '%x%' AND starts_with(name, 'a') LIMIT 10"
            );
        }

        #[test]
        fn test_build_filter_suffix_none() {
            assert_eq!(build_filter_suffix(None, None, None, None, None), "");
        }

        #[test]
        fn test_build_filter_suffix_reescapes_quotes() {
            assert_eq!(
                build_filter_suffix(Some("O'Brien"), None, None, None, None),
                " WHERE name ILIKE 'O''Brien'"
            );
        }

        #[test]
        fn test_build_filter_suffix_in_schema() {
            assert_eq!(
                build_filter_suffix(
                    None,
                    None,
                    None,
                    Some(&ScopeName::Named("main".into())),
                    None
                ),
                " WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn test_build_filter_suffix_in_database() {
            assert_eq!(
                build_filter_suffix(
                    None,
                    None,
                    None,
                    None,
                    Some(&ScopeName::Named("memory".into()))
                ),
                " WHERE lower(database_name) = lower('memory')"
            );
        }

        #[test]
        fn test_build_filter_suffix_like_and_schema() {
            assert_eq!(
                build_filter_suffix(
                    Some("%x%"),
                    None,
                    None,
                    Some(&ScopeName::Named("main".into())),
                    None
                ),
                " WHERE name ILIKE '%x%' AND lower(schema_name) = lower('main')"
            );
        }

        // --- rewrite_ddl SHOW with filter clauses ---

        #[test]
        fn test_rewrite_show_dims_like_cross_view() {
            let sql = passthrough_sql("SHOW SEMANTIC DIMENSIONS LIKE '%rev%'");
            assert_eq!(
                sql,
                "SELECT * FROM show_semantic_dimensions_all() WHERE name ILIKE '%rev%'"
            );
        }

        #[test]
        fn test_rewrite_show_dims_like_in_starts_with_limit() {
            let sql = passthrough_sql(
                "SHOW SEMANTIC DIMENSIONS LIKE '%c%' IN v STARTS WITH 'cust' LIMIT 2",
            );
            assert_eq!(
                sql,
                "SELECT * FROM show_semantic_dimensions('v') WHERE name ILIKE '%c%' AND starts_with(name, 'cust') LIMIT 2"
            );
        }

        #[test]
        fn test_rewrite_show_metrics_starts_with_limit() {
            let sql = passthrough_sql("SHOW SEMANTIC METRICS STARTS WITH 'total' LIMIT 1");
            assert_eq!(
                sql,
                "SELECT * FROM show_semantic_metrics_all() WHERE starts_with(name, 'total') LIMIT 1"
            );
        }

        #[test]
        fn test_rewrite_show_facts_limit() {
            let sql = passthrough_sql("SHOW SEMANTIC FACTS LIMIT 10");
            assert_eq!(sql, "SELECT * FROM show_semantic_facts_all() LIMIT 10");
        }

        #[test]
        fn test_rewrite_show_dims_for_metric_with_all_clauses() {
            let sql = passthrough_sql(
                "SHOW SEMANTIC DIMENSIONS LIKE '%x%' IN v FOR METRIC m STARTS WITH 'a' LIMIT 3",
            );
            assert_eq!(
                sql,
                "SELECT * FROM show_semantic_dimensions_for_metric('v', 'm') WHERE name ILIKE '%x%' AND starts_with(name, 'a') LIMIT 3"
            );
        }

        #[test]
        fn test_rewrite_show_dims_like_after_in_error() {
            let result = plan_ddl("SHOW SEMANTIC DIMENSIONS IN v LIKE '%x%'");
            assert!(result.is_err(), "LIKE after IN should error");
        }

        #[test]
        fn test_rewrite_show_metrics_limit_non_numeric() {
            let result = plan_ddl("SHOW SEMANTIC METRICS LIMIT abc");
            assert!(result.is_err(), "Non-numeric LIMIT should error");
        }

        #[test]
        fn test_rewrite_show_for_metric_on_metrics_error() {
            let result = plan_ddl("SHOW SEMANTIC METRICS IN v FOR METRIC m");
            assert!(result.is_err(), "FOR METRIC on SHOW METRICS should error");
        }

        // --- Case insensitivity ---

        #[test]
        fn test_rewrite_show_case_insensitive() {
            let sql = passthrough_sql("show semantic dimensions like '%x%' in v");
            assert_eq!(
                sql,
                "SELECT * FROM show_semantic_dimensions('v') WHERE name ILIKE '%x%'"
            );
        }

        // --- SHOW SEMANTIC VIEWS with filter clauses ---

        #[test]
        fn test_rewrite_show_views_like() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS LIKE '%prod%'");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE name ILIKE '%prod%'"
            );
        }

        #[test]
        fn test_rewrite_show_views_starts_with_limit() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS STARTS WITH 'sales' LIMIT 5");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE starts_with(name, 'sales') LIMIT 5"
            );
        }

        #[test]
        fn test_rewrite_show_views_all_clauses() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS LIKE '%x%' STARTS WITH 'a' LIMIT 3");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE name ILIKE '%x%' AND starts_with(name, 'a') LIMIT 3"
            );
        }

        // --- PA-1 regression: non-ASCII trailing tokens must error cleanly ---

        #[test]
        fn test_show_views_non_ascii_trailing_tokens_error_not_panic() {
            // Pre-fix: `rest[..4]` sliced "aΩΩ" mid-codepoint and panicked
            // ("byte index 4 is not a char boundary"), surfacing as
            // "internal error (panic)" at the FFI boundary.
            let err = plan_ddl("SHOW SEMANTIC VIEWS aΩΩ").unwrap_err();
            assert!(err.message.contains("Unexpected tokens"), "got: {err}");

            // Every clause scanner position: 2, 3, 4, 5, 6-byte prefixes.
            for q in [
                "SHOW SEMANTIC VIEWS Ω",
                "SHOW SEMANTIC VIEWS ΩΩΩ",
                "SHOW SEMANTIC DIMENSIONS éé",
                "SHOW SEMANTIC METRICS 東京",
                "SHOW SEMANTIC FACTS ☕☕☕",
            ] {
                let result = plan_ddl(q);
                assert!(result.is_err(), "expected clean error for {q}");
            }
        }

        #[test]
        fn test_show_views_like_non_ascii_pattern_roundtrips() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS LIKE '%café%'");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE name ILIKE '%café%'"
            );
        }

        #[test]
        fn test_rewrite_show_views_in_requires_schema_or_database() {
            let result = plan_ddl("SHOW SEMANTIC VIEWS IN some_view");
            assert!(
                result.is_err(),
                "IN without SCHEMA/DATABASE should be rejected for SHOW SEMANTIC VIEWS"
            );
            let err = result.unwrap_err();
            // Wording widened when IN ACCOUNT and the bare IN SCHEMA / IN
            // DATABASE forms landed; the assertion still names every accepted
            // spelling rather than being loosened to "is an error".
            assert!(
                err.message.contains(
                    "SHOW SEMANTIC VIEWS requires a scope: \
                     IN {ACCOUNT | DATABASE [db] | SCHEMA [[db.]schema]}"
                ),
                "got: {err}"
            );
        }

        #[test]
        fn test_rewrite_show_views_in_schema() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA main");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn test_rewrite_show_views_in_database() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS IN DATABASE memory");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE lower(database_name) = lower('memory')"
            );
        }

        #[test]
        fn test_rewrite_show_terse() {
            let sql = passthrough_sql("SHOW TERSE SEMANTIC VIEWS");
            assert_eq!(sql, "SELECT * FROM list_terse_semantic_views()");
        }

        #[test]
        fn test_rewrite_show_terse_like() {
            let sql = passthrough_sql("SHOW TERSE SEMANTIC VIEWS LIKE '%prod%'");
            assert_eq!(
                sql,
                "SELECT * FROM list_terse_semantic_views() WHERE name ILIKE '%prod%'"
            );
        }

        #[test]
        fn test_rewrite_show_terse_in_schema() {
            let sql = passthrough_sql("SHOW TERSE SEMANTIC VIEWS IN SCHEMA main");
            assert_eq!(
                sql,
                "SELECT * FROM list_terse_semantic_views() WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn test_rewrite_show_views_in_schema_like() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS LIKE '%x%' IN SCHEMA main");
            assert_eq!(
                sql,
                "SELECT * FROM list_semantic_views() WHERE name ILIKE '%x%' AND lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn test_rewrite_show_columns_in_semantic_view() {
            let sql = passthrough_sql("SHOW COLUMNS IN SEMANTIC VIEW sales");
            assert_eq!(sql, "SELECT * FROM show_columns_in_semantic_view('sales')");
        }

        #[test]
        fn test_rewrite_show_views_for_metric_error() {
            let result = plan_ddl("SHOW SEMANTIC VIEWS FOR METRIC m");
            assert!(
                result.is_err(),
                "FOR METRIC should be rejected for SHOW SEMANTIC VIEWS"
            );
            let err = result.unwrap_err();
            assert!(
                err.message.contains("FOR METRIC is only valid"),
                "got: {err}"
            );
        }

        #[test]
        fn test_rewrite_show_views_no_clauses_regression() {
            let sql = passthrough_sql("SHOW SEMANTIC VIEWS");
            assert_eq!(sql, "SELECT * FROM list_semantic_views()");
        }
    }

    // -----------------------------------------------------------------------
    // Phase 57: SHOW SEMANTIC MATERIALIZATIONS tests (INTR-03)
    // -----------------------------------------------------------------------

    #[test]
    fn detect_show_materializations() {
        assert_eq!(
            detect_ddl_kind("SHOW SEMANTIC MATERIALIZATIONS"),
            Some(DdlKind::ShowMaterializations)
        );
    }

    #[test]
    fn detect_show_materializations_in_view() {
        assert_eq!(
            detect_ddl_kind("SHOW SEMANTIC MATERIALIZATIONS IN my_view"),
            Some(DdlKind::ShowMaterializations)
        );
    }

    #[test]
    fn rewrite_show_materializations_all() {
        let sql = passthrough_sql("SHOW SEMANTIC MATERIALIZATIONS");
        assert_eq!(sql, "SELECT * FROM show_semantic_materializations_all()");
    }

    #[test]
    fn rewrite_show_materializations_in_view() {
        let sql = passthrough_sql("SHOW SEMANTIC MATERIALIZATIONS IN my_view");
        assert_eq!(
            sql,
            "SELECT * FROM show_semantic_materializations('my_view')"
        );
    }

    #[test]
    fn near_miss_show_materialization() {
        // "SHOW SEMANTIC MATERIALIZATION" (missing 'S') should suggest the correct prefix
        let result = detect_near_miss("SHOW SEMANTIC MATERIALIZATION");
        assert!(result.is_some());
        let err = result.unwrap();
        assert!(err.message.contains("Did you mean"), "got: {}", err.message);
    }

    // -----------------------------------------------------------------------
    // Phase 43: View-level COMMENT tests
    // -----------------------------------------------------------------------

    mod phase43_view_comment_tests {
        use super::*;

        #[test]
        fn test_view_comment_parsed() {
            let RewriteAction::Create { def, .. } = plan(
                "CREATE SEMANTIC VIEW my_view COMMENT = 'My view' AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))"
            ) else {
                panic!("expected RewriteAction::Create");
            };
            // The comment (RAW) should be carried on the definition.
            assert_eq!(def.comment.as_deref(), Some("My view"));
        }

        #[test]
        fn test_view_without_comment() {
            let RewriteAction::Create { def, mode, .. } = plan(
                "CREATE SEMANTIC VIEW my_view AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))"
            ) else {
                panic!("expected RewriteAction::Create");
            };
            assert_eq!(mode, CreateMode::Create, "Should use plain CREATE mode");
            assert_eq!(def.comment, None, "No comment should be carried");
        }

        #[test]
        fn test_view_comment_escaped_quotes() {
            let RewriteAction::Create { def, .. } = plan(
                "CREATE SEMANTIC VIEW my_view COMMENT = 'It''s great' AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))"
            ) else {
                panic!("expected RewriteAction::Create");
            };
            // Structured variants carry the RAW (un-escaped) comment.
            assert_eq!(def.comment.as_deref(), Some("It's great"));
        }

        #[test]
        fn test_view_comment_with_create_or_replace() {
            let RewriteAction::Create { def, mode, .. } = plan(
                "CREATE OR REPLACE SEMANTIC VIEW my_view COMMENT = 'Updated' AS TABLES (o AS orders PRIMARY KEY (id)) DIMENSIONS (o.region AS o.region) METRICS (o.rev AS SUM(o.amount))"
            ) else {
                panic!("expected RewriteAction::Create");
            };
            assert_eq!(def.comment.as_deref(), Some("Updated"));
            assert_eq!(mode, CreateMode::OrReplace, "Should use OR REPLACE mode");
        }
    }

    // ===================================================================
    // ALTER SET/UNSET COMMENT tests (Phase 45)
    // ===================================================================

    #[test]
    fn test_detect_ddl_kind_alter_set_comment() {
        assert_eq!(
            detect_ddl_kind("ALTER SEMANTIC VIEW v SET COMMENT = 'test'"),
            Some(DdlKind::Alter)
        );
    }

    #[test]
    fn test_detect_ddl_kind_alter_if_exists_set_comment() {
        assert_eq!(
            detect_ddl_kind("ALTER SEMANTIC VIEW IF EXISTS v SET COMMENT = 'test'"),
            Some(DdlKind::AlterIfExists)
        );
    }

    #[test]
    fn test_detect_ddl_kind_alter_unset_comment() {
        assert_eq!(
            detect_ddl_kind("ALTER SEMANTIC VIEW v UNSET COMMENT"),
            Some(DdlKind::Alter)
        );
    }

    #[test]
    fn test_detect_ddl_kind_alter_if_exists_unset_comment() {
        assert_eq!(
            detect_ddl_kind("ALTER SEMANTIC VIEW IF EXISTS v UNSET COMMENT"),
            Some(DdlKind::AlterIfExists)
        );
    }

    #[test]
    fn test_detect_ddl_kind_alter_rename_backwards_compat() {
        assert_eq!(
            detect_ddl_kind("ALTER SEMANTIC VIEW v RENAME TO w"),
            Some(DdlKind::Alter)
        );
    }

    #[test]
    fn test_validate_rewrite_alter_set_comment() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW v SET COMMENT = 'hello'"),
            RewriteAction::AlterSetComment {
                name: vref("v"),
                comment: "hello".to_string(),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_unset_comment() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW v UNSET COMMENT"),
            RewriteAction::AlterUnsetComment {
                name: vref("v"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_if_exists_set_comment() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW IF EXISTS v SET COMMENT = 'hello'"),
            RewriteAction::AlterSetComment {
                name: vref("v"),
                comment: "hello".to_string(),
                if_exists: true,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_if_exists_unset_comment() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW IF EXISTS v UNSET COMMENT"),
            RewriteAction::AlterUnsetComment {
                name: vref("v"),
                if_exists: true,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_rename_unchanged() {
        assert_eq!(
            plan("ALTER SEMANTIC VIEW v RENAME TO w"),
            RewriteAction::AlterRename {
                name: vref("v"),
                new_name: vref("w"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_unsupported_operation() {
        let err = plan_rewrite("ALTER SEMANTIC VIEW v TRUNCATE").unwrap_err();
        assert!(
            err.message
                .contains("RENAME TO, SET COMMENT, UNSET COMMENT"),
            "Error should list supported ops, got: {}",
            err.message
        );
    }

    #[test]
    fn test_validate_rewrite_alter_set_comment_escaped_quotes() {
        // Structured variants carry the RAW (un-escaped) comment.
        assert_eq!(
            plan("ALTER SEMANTIC VIEW v SET COMMENT = 'it''s a test'"),
            RewriteAction::AlterSetComment {
                name: vref("v"),
                comment: "it's a test".to_string(),
                if_exists: false,
            }
        );
    }

    #[test]
    fn test_validate_rewrite_alter_missing_operation() {
        let err = plan_rewrite("ALTER SEMANTIC VIEW v").unwrap_err();
        assert!(
            err.message
                .contains("RENAME TO, SET COMMENT, UNSET COMMENT"),
            "Error should list supported ops, got: {}",
            err.message
        );
    }

    // ===================================================================
    // Phase 52: Dollar-quote extraction tests
    // ===================================================================

    #[test]
    fn test_extract_dollar_quoted_untagged() {
        let (content, consumed) = extract_dollar_quoted("$$hello world$$").unwrap();
        assert_eq!(content, "hello world");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn test_extract_dollar_quoted_tagged() {
        let (content, consumed) = extract_dollar_quoted("$yaml$my content$yaml$").unwrap();
        assert_eq!(content, "my content");
        assert_eq!(consumed, 22);
    }

    #[test]
    fn test_extract_dollar_quoted_empty_content() {
        let (content, consumed) = extract_dollar_quoted("$$$$").unwrap();
        assert_eq!(content, "");
        assert_eq!(consumed, 4);
    }

    #[test]
    fn test_extract_dollar_quoted_no_leading_dollar() {
        let err = extract_dollar_quoted("not a dollar").unwrap_err();
        assert!(err.message.contains("Expected '$'"));
    }

    #[test]
    fn test_extract_dollar_quoted_unterminated_opening() {
        let err = extract_dollar_quoted("$no_close").unwrap_err();
        assert!(err.message.contains("Unterminated dollar-quote opening"));
    }

    #[test]
    fn test_extract_dollar_quoted_unterminated_body() {
        let err = extract_dollar_quoted("$$no closing").unwrap_err();
        assert!(err.message.contains("Unterminated dollar-quoted string"));
    }

    #[test]
    fn test_extract_dollar_quoted_rejects_invalid_tag() {
        // P-6: `$1$` is not a valid dollar-quote tag ($1 is a parameter), and
        // `blank_sql_comments` treats it as SQL. The extractor must agree and
        // reject it rather than accept a delimiter the blanker already corrupted
        // the payload for. A tag with interior whitespace is likewise rejected.
        // The message says "Invalid" (a closing `$` exists, but the tag body is
        // illegal) — distinct from the "Unterminated" case with no second `$`.
        let err = extract_dollar_quoted("$1$body$1$").unwrap_err();
        assert!(
            err.message.contains("Invalid dollar-quote tag"),
            "{}",
            err.message
        );
        assert!(extract_dollar_quoted("$ta g$body$ta g$")
            .unwrap_err()
            .message
            .contains("Invalid dollar-quote tag"));
    }

    #[test]
    fn test_extract_dollar_quoted_inner_dollar() {
        // First closing $$ wins — content is "has inner "
        let (content, consumed) = extract_dollar_quoted("$$has inner $$ text$$").unwrap();
        assert_eq!(content, "has inner ");
        assert_eq!(consumed, 14);
    }

    #[test]
    fn test_extract_dollar_quoted_multiline() {
        let input = "$$\ntables:\n  - alias: o\n    table: orders\n$$";
        let (content, _) = extract_dollar_quoted(input).unwrap();
        assert!(content.contains("tables:"));
        assert!(content.contains("alias: o"));
    }

    // ===================================================================
    // Phase 52: YAML DDL rewrite tests
    // ===================================================================

    #[test]
    fn test_yaml_rewrite_basic_create() {
        let yaml_text = r#"$$
base_table: orders
tables:
  - alias: o
    table: orders
    pk_columns:
      - id
dimensions:
  - name: region
    expr: o.region
    source_table: o
metrics:
  - name: total_amount
    expr: SUM(o.amount)
    source_table: o
$$"#;
        let action =
            rewrite_ddl_yaml_body(DdlKind::Create, &vref("test_view"), yaml_text, None).unwrap();
        let RewriteAction::Create { name, mode, .. } = action else {
            panic!("expected RewriteAction::Create, got {action:?}");
        };
        assert_eq!(name, vref("test_view"));
        assert_eq!(mode, CreateMode::Create);
    }

    #[test]
    fn test_yaml_rewrite_create_or_replace() {
        let yaml_text = "$$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        let action =
            rewrite_ddl_yaml_body(DdlKind::CreateOrReplace, &vref("v"), yaml_text, None).unwrap();
        assert!(matches!(
            action,
            RewriteAction::Create {
                mode: CreateMode::OrReplace,
                ..
            }
        ));
    }

    #[test]
    fn test_yaml_rewrite_create_if_not_exists() {
        let yaml_text = "$$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        let result =
            rewrite_ddl_yaml_body(DdlKind::CreateIfNotExists, &vref("v"), yaml_text, None).unwrap();
        assert!(matches!(
            result,
            RewriteAction::Create {
                mode: CreateMode::IfNotExists,
                ..
            }
        ));
    }

    #[test]
    fn test_yaml_rewrite_trailing_content_rejected() {
        let yaml_text =
            "$$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$ extra stuff";
        let err = rewrite_ddl_yaml_body(DdlKind::Create, &vref("v"), yaml_text, None).unwrap_err();
        assert!(err
            .message
            .contains("Unexpected content after closing dollar-quote"));
    }

    #[test]
    fn test_yaml_rewrite_invalid_yaml() {
        let yaml_text = "$$\n: : : not valid yaml [[[$$";
        let err =
            rewrite_ddl_yaml_body(DdlKind::Create, &vref("bad_view"), yaml_text, None).unwrap_err();
        assert!(err.message.contains("bad_view"));
    }

    #[test]
    fn test_yaml_rewrite_comment_override() {
        let yaml_text =
            "$$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\ncomment: yaml comment\n$$";
        let result = rewrite_ddl_yaml_body(
            DdlKind::Create,
            &vref("v"),
            yaml_text,
            Some("ddl comment".to_string()),
        )
        .unwrap();
        let RewriteAction::Create { def, .. } = result else {
            panic!("expected RewriteAction::Create, got {result:?}");
        };
        // DDL comment overrides YAML comment
        assert_eq!(def.comment.as_deref(), Some("ddl comment"));
    }

    #[test]
    fn test_yaml_rewrite_base_table_populated() {
        let yaml_text = r#"$$
base_table: ""
tables:
  - alias: o
    table: orders
    pk_columns: []
dimensions: []
metrics:
  - name: total
    expr: sum(o.amount)
    source_table: o
$$"#;
        let result = rewrite_ddl_yaml_body(DdlKind::Create, &vref("v"), yaml_text, None).unwrap();
        let RewriteAction::Create { def, .. } = result else {
            panic!("expected RewriteAction::Create, got {result:?}");
        };
        // base_table should be populated from first table entry
        assert!(def.tables.iter().any(|t| t.table == "orders"));
    }

    #[test]
    fn test_yaml_rewrite_tagged_dollar_quote() {
        let yaml_text = "$yaml$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$yaml$";
        let result = rewrite_ddl_yaml_body(DdlKind::Create, &vref("v"), yaml_text, None).unwrap();
        assert!(matches!(result, RewriteAction::Create { .. }));
    }

    // ===================================================================
    // Phase 52: FROM YAML detection in validate_create_body
    // ===================================================================

    #[test]
    fn test_from_yaml_detection_via_rewrite_ddl() {
        let query = r#"CREATE SEMANTIC VIEW yaml_test FROM YAML $$
base_table: t
tables:
  - alias: t
    table: t
    pk_columns: [id]
dimensions:
  - name: d
    expr: t.d
    source_table: t
metrics: []
$$"#;
        let RewriteAction::Create { name, mode, .. } = plan(query) else {
            panic!("expected RewriteAction::Create");
        };
        assert_eq!(name, vref("yaml_test"));
        assert_eq!(mode, CreateMode::Create);
    }

    #[test]
    fn test_from_yaml_case_insensitive() {
        let query = "CREATE SEMANTIC VIEW v from yaml $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(query), RewriteAction::Create { .. }));
    }

    #[test]
    fn test_from_yaml_mixed_case() {
        let query = "CREATE SEMANTIC VIEW v From Yaml $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(query), RewriteAction::Create { .. }));
    }

    #[test]
    fn test_from_yaml_flexible_whitespace() {
        // P-7 (code-review 2026-07-11): the FROM YAML detection was a 9-byte
        // literal compare requiring exactly one space — `FROM  YAML`,
        // `FROM\tYAML`, and `FROM /* fmt */ YAML` (comments blank to a run
        // of spaces) all fell through to the generic error. Fixed PA-10
        // class; this site was missed by the Phase 25.1 sweep.
        let two_spaces = "CREATE SEMANTIC VIEW v FROM  YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(two_spaces), RewriteAction::Create { .. }));

        let tab = "CREATE SEMANTIC VIEW v FROM\tYAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(tab), RewriteAction::Create { .. }));

        let comment = "CREATE SEMANTIC VIEW v FROM /* fmt */ YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(comment), RewriteAction::Create { .. }));

        let newline = "CREATE SEMANTIC VIEW v FROM\nYAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(newline), RewriteAction::Create { .. }));
    }

    #[test]
    fn test_error_message_mentions_from_yaml() {
        let query = "CREATE SEMANTIC VIEW v SOMETHING_ELSE";
        let err = plan_rewrite(query).unwrap_err();
        assert!(
            err.message.contains("FROM YAML"),
            "Error should mention FROM YAML: {}",
            err.message
        );
    }

    #[test]
    fn test_create_or_replace_from_yaml() {
        let query = "CREATE OR REPLACE SEMANTIC VIEW v FROM YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(
            plan(query),
            RewriteAction::Create {
                mode: CreateMode::OrReplace,
                ..
            }
        ));
    }

    #[test]
    fn test_create_if_not_exists_from_yaml() {
        let query = "CREATE SEMANTIC VIEW IF NOT EXISTS v FROM YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(
            plan(query),
            RewriteAction::Create {
                mode: CreateMode::IfNotExists,
                ..
            }
        ));
    }

    #[test]
    fn test_comment_with_from_yaml() {
        let query = "CREATE SEMANTIC VIEW v COMMENT = 'my comment' FROM YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        let RewriteAction::Create { def, .. } = plan(query) else {
            panic!("expected RewriteAction::Create");
        };
        assert_eq!(def.comment.as_deref(), Some("my comment"));
    }

    // ===================================================================
    // Phase 53: FROM YAML FILE tests
    // ===================================================================

    #[test]
    fn test_extract_single_quoted_basic() {
        let (content, consumed) = extract_single_quoted("'/path/to/file.yaml'").unwrap();
        assert_eq!(content, "/path/to/file.yaml");
        assert_eq!(consumed, 20);
    }

    #[test]
    fn test_extract_single_quoted_escaped() {
        // '/file''s.yaml' = ' f i l e ' ' s . y a m l ' = 15 chars
        let (content, consumed) = extract_single_quoted("'/file''s.yaml'").unwrap();
        assert_eq!(content, "/file's.yaml");
        assert_eq!(consumed, 15);
    }

    #[test]
    fn test_extract_single_quoted_empty() {
        let (content, consumed) = extract_single_quoted("''").unwrap();
        assert_eq!(content, "");
        assert_eq!(consumed, 2);
    }

    #[test]
    fn test_extract_single_quoted_no_quote() {
        let err = extract_single_quoted("no quote").unwrap_err();
        assert!(
            err.message.contains("Expected single-quoted file path"),
            "Error: {}",
            err.message
        );
    }

    #[test]
    fn test_extract_single_quoted_unterminated() {
        let err = extract_single_quoted("'unterminated").unwrap_err();
        assert!(
            err.message.contains("Unterminated file path string"),
            "Error: {}",
            err.message
        );
    }

    // Phase 65.1 WR-04: round-trip non-ASCII payloads through the
    // FROM YAML FILE quoted-path extractor. The Plan 07 FileSystem-direct
    // YAML read path makes this matter: the path string is passed
    // verbatim to LocalFileSystem::OpenFile, so corrupted bytes would
    // surface as "file not found" rather than the user's expected open.
    #[test]
    fn test_extract_single_quoted_utf8_cjk_path() {
        let input = "'/tmp/数据/视图.yaml'";
        let (content, consumed) = extract_single_quoted(input).unwrap();
        assert_eq!(content, "/tmp/数据/视图.yaml");
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_extract_single_quoted_utf8_accented_path() {
        let input = "'/Users/café/définition.yaml'";
        let (content, consumed) = extract_single_quoted(input).unwrap();
        assert_eq!(content, "/Users/café/définition.yaml");
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn test_extract_single_quoted_utf8_emoji_path() {
        let input = "'/tmp/🦆-data.yaml'";
        let (content, consumed) = extract_single_quoted(input).unwrap();
        assert_eq!(content, "/tmp/🦆-data.yaml");
        assert_eq!(consumed, input.len());
    }

    // Phase 65.1 WR-04: extract_view_comment lives further up the file
    // and is tested implicitly through the validate_create_body
    // round-trip. Add a direct non-ASCII test here.
    #[test]
    fn test_extract_view_comment_utf8_cyrillic() {
        let (comment, remaining) = extract_view_comment("COMMENT = 'Привет, мир'").unwrap();
        assert_eq!(comment.as_deref(), Some("Привет, мир"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_extract_view_comment_utf8_emoji_and_em_dash() {
        let (comment, remaining) = extract_view_comment("COMMENT = '🦆 — quack' AS (...)").unwrap();
        assert_eq!(comment.as_deref(), Some("🦆 — quack"));
        assert_eq!(remaining, " AS (...)");
    }

    #[test]
    fn test_extract_view_comment_utf8_escaped_quotes_around_nonascii() {
        let (comment, remaining) = extract_view_comment("COMMENT = 'café ''noir'''").unwrap();
        assert_eq!(comment.as_deref(), Some("café 'noir'"));
        assert_eq!(remaining, "");
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_create() {
        let result = rewrite_ddl_yaml_file_body(
            DdlKind::Create,
            &vref("myview"),
            "'/path/to/def.yaml'",
            None,
        )
        .unwrap();
        assert_eq!(
            result,
            RewriteAction::CreateFromYamlFile {
                file_path: "/path/to/def.yaml".to_string(),
                name: vref("myview"),
                comment: String::new(),
                mode: CreateMode::Create,
            }
        );
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_replace() {
        let result = rewrite_ddl_yaml_file_body(
            DdlKind::CreateOrReplace,
            &vref("v"),
            "'/f.yaml'",
            Some("a comment".into()),
        )
        .unwrap();
        assert_eq!(
            result,
            RewriteAction::CreateFromYamlFile {
                file_path: "/f.yaml".to_string(),
                name: vref("v"),
                comment: "a comment".to_string(),
                mode: CreateMode::OrReplace,
            }
        );
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_if_not_exists() {
        let result =
            rewrite_ddl_yaml_file_body(DdlKind::CreateIfNotExists, &vref("v"), "'/f.yaml'", None)
                .unwrap();
        assert_eq!(
            result,
            RewriteAction::CreateFromYamlFile {
                file_path: "/f.yaml".to_string(),
                name: vref("v"),
                comment: String::new(),
                mode: CreateMode::IfNotExists,
            }
        );
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_with_comment() {
        let result = rewrite_ddl_yaml_file_body(
            DdlKind::Create,
            &vref("v"),
            "'/f.yaml'",
            Some("my comment".into()),
        )
        .unwrap();
        let RewriteAction::CreateFromYamlFile { comment, .. } = result else {
            panic!("expected RewriteAction::CreateFromYamlFile, got {result:?}");
        };
        assert_eq!(comment, "my comment");
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_empty_path() {
        let err = rewrite_ddl_yaml_file_body(DdlKind::Create, &vref("v"), "''", None).unwrap_err();
        assert!(
            err.message.contains("File path cannot be empty"),
            "Error: {}",
            err.message
        );
    }

    #[test]
    fn test_rewrite_ddl_yaml_file_body_trailing_content() {
        let err =
            rewrite_ddl_yaml_file_body(DdlKind::Create, &vref("v"), "'/f.yaml' extra stuff", None)
                .unwrap_err();
        assert!(
            err.message.contains("Unexpected content after file path"),
            "Error: {}",
            err.message
        );
    }

    #[test]
    fn test_validate_and_rewrite_yaml_file() {
        let query = "CREATE SEMANTIC VIEW v FROM YAML FILE '/test.yaml'";
        assert!(
            matches!(plan(query), RewriteAction::CreateFromYamlFile { .. }),
            "Expected CreateFromYamlFile action"
        );
    }

    #[test]
    fn test_validate_and_rewrite_yaml_file_case_insensitive() {
        let query = "CREATE SEMANTIC VIEW v from yaml file '/test.yaml'";
        assert!(matches!(
            plan(query),
            RewriteAction::CreateFromYamlFile { .. }
        ));
    }

    #[test]
    fn test_validate_and_rewrite_yaml_inline_still_works() {
        // Regression: FROM YAML $$...$$ still works after FILE branch is added
        let query = "CREATE SEMANTIC VIEW v FROM YAML $$\nbase_table: t\ntables:\n  - alias: t\n    table: t\n    pk_columns: [id]\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n$$";
        assert!(matches!(plan(query), RewriteAction::Create { .. }));
    }

    #[test]
    fn test_error_message_mentions_from_yaml_file() {
        let query = "CREATE SEMANTIC VIEW v SOMETHING_ELSE";
        let err = plan_rewrite(query).unwrap_err();
        assert!(
            err.message.contains("FROM YAML FILE"),
            "Error should mention FROM YAML FILE: {}",
            err.message
        );
    }

    // ===================================================================
    // `skip_leading_whitespace` — leading-whitespace offset.
    //
    // Comment handling is upstream in `blank_sql_comments` (P-5, code-review
    // 2026-07-11): callers blank first, so by the time input reaches this
    // helper comments are already spaces. The former inline comment-scanning
    // branches were dead and are gone; end-to-end leading-comment skipping is
    // covered by `detect_create_with_leading_block_comment` and the
    // `detect_semantic_view_ddl` comment cases below (which go through the
    // blank-then-skip pipeline).
    // ===================================================================

    #[test]
    fn skip_lws_empty() {
        assert_eq!(skip_leading_whitespace(""), 0);
    }

    #[test]
    fn skip_lws_only_whitespace() {
        assert_eq!(skip_leading_whitespace("   \n\t"), 5);
    }

    #[test]
    fn skip_lws_no_leading_match() {
        // No leading whitespace -> offset 0.
        assert_eq!(skip_leading_whitespace("CREATE"), 0);
    }

    #[test]
    fn skip_lws_blanked_comment_is_whitespace() {
        // Mirrors production: a leading comment is blanked to spaces upstream,
        // then this helper skips it as whitespace. (A raw `-- hi\n` would NOT
        // be skipped — comment handling deliberately no longer lives here.)
        let raw = "-- hi\nCREATE";
        let blanked = crate::util::blank_sql_comments(raw);
        assert_eq!(&blanked[skip_leading_whitespace(&blanked)..], "CREATE");
    }

    #[test]
    fn detect_create_with_leading_block_comment() {
        assert_eq!(
            detect_semantic_view_ddl("/* hi */ CREATE SEMANTIC VIEW x AS TABLES (t AS t PRIMARY KEY (x)) DIMENSIONS (t.xx AS t.x) METRICS (t.sy AS SUM(t.y))"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn detect_create_with_leading_line_comment() {
        assert_eq!(
            detect_semantic_view_ddl("-- hi\nCREATE SEMANTIC VIEW x AS TABLES (t AS t PRIMARY KEY (x)) DIMENSIONS (t.xx AS t.x) METRICS (t.sy AS SUM(t.y))"),
            PARSE_DETECTED
        );
    }

    #[test]
    fn detect_create_or_replace_with_dbt_style_annotation() {
        let q = "/* {\"app\": \"dbt\", \"node_id\": \"model.x\"} */ CREATE OR REPLACE SEMANTIC VIEW x AS TABLES (t AS t PRIMARY KEY (x)) DIMENSIONS (t.xx AS t.x) METRICS (t.sy AS SUM(t.y))";
        assert_eq!(detect_semantic_view_ddl(q), PARSE_DETECTED);
        let kind = detect_ddl_kind(q);
        assert_eq!(kind, Some(DdlKind::CreateOrReplace));
    }

    #[test]
    fn detect_other_ddl_forms_with_leading_comment() {
        for q in [
            "/* x */ DROP SEMANTIC VIEW v",
            "/* x */ ALTER SEMANTIC VIEW v RENAME TO w",
            "/* x */ DESCRIBE SEMANTIC VIEW v",
            "/* x */ SHOW SEMANTIC VIEWS",
            "/* x */ SHOW SEMANTIC METRICS IN v",
            "-- annotation\nDROP SEMANTIC VIEW v",
        ] {
            assert_eq!(detect_semantic_view_ddl(q), PARSE_DETECTED, "failed: {q}");
        }
    }

    #[test]
    fn comment_only_is_not_semantic_view_ddl() {
        assert_eq!(
            detect_semantic_view_ddl("/* just a comment */"),
            PARSE_NOT_OURS
        );
        assert_eq!(
            detect_semantic_view_ddl("-- just a comment\n"),
            PARSE_NOT_OURS
        );
    }

    #[test]
    fn validate_and_rewrite_with_leading_comment_succeeds() {
        let q = "/* annotation */ DROP SEMANTIC VIEW v";
        assert_eq!(
            plan(q),
            RewriteAction::Drop {
                name: vref("v"),
                if_exists: false,
            }
        );
    }

    #[test]
    fn error_position_accounts_for_leading_comment() {
        // Missing view name -- error position should point at the offset AFTER
        // both the comment AND the prefix, in the ORIGINAL query string.
        let q = "/* hi */ DROP SEMANTIC VIEW";
        let err = plan_rewrite(q).expect_err("should error: missing name");
        let pos = err.position.expect("position should be set");
        // Position should be inside the original string (not into the stripped slice).
        // The prefix "DROP SEMANTIC VIEW" starts at byte 9 (after "/* hi */ ").
        // After consuming the prefix (18 bytes), we're at byte 27 == query.len().
        assert_eq!(pos, q.len(), "position should reference original query");
    }

    // -------------------------------------------------------------------
    // write_error_to_buffer: UTF-8 char-boundary truncation
    //
    // The C buffer is fixed-size (1024 bytes in the C++ shim). If a long
    // error message has a multi-byte codepoint straddling the truncation
    // point, naive byte-truncation would emit invalid UTF-8 in the
    // NUL-terminated tail. The helper must walk back to a char boundary.
    // -------------------------------------------------------------------

    #[test]
    fn write_error_to_buffer_truncates_at_char_boundary() {
        // 'é' is two bytes (0xC3 0xA9). Build a string whose byte-length
        // forces truncation to land mid-codepoint, then verify the C-string
        // tail is valid UTF-8.
        let mut s = String::new();
        for _ in 0..511 {
            s.push('é'); // 511 * 2 = 1022 bytes
        }
        s.push('é'); // now 1024 bytes; max_copy=1023 lands between the two
                     // bytes of the final 'é'
        assert_eq!(s.len(), 1024);

        let mut buf = vec![0u8; 1024];
        unsafe {
            super::write_error_to_buffer(buf.as_mut_ptr(), buf.len(), &s);
        }
        // Find the NUL and slice up to it.
        let nul = buf
            .iter()
            .position(|&b| b == 0)
            .expect("NUL terminator written");
        // Bytes before NUL must be valid UTF-8 (no orphaned lead byte).
        std::str::from_utf8(&buf[..nul]).expect("truncated tail must be valid UTF-8");
    }

    #[test]
    fn write_error_to_buffer_handles_short_string() {
        let s = "ok";
        let mut buf = vec![0xFFu8; 16];
        unsafe {
            super::write_error_to_buffer(buf.as_mut_ptr(), buf.len(), s);
        }
        assert_eq!(&buf[..2], b"ok");
        assert_eq!(buf[2], 0);
    }

    // ===================================================================
    // Phase 64: Quoted identifier handling (QID-01..QID-06).
    //
    // Wires `crate::ident::{normalize_view_name, find_identifier_end}` into
    // the five DDL capture sites in this file. Each capture site is
    // exercised here via its public-facing entry point (plan_ddl for
    // DROP/DESCRIBE/SHOW COLUMNS, plan_rewrite for CREATE/ALTER) with quoted
    // and FQN forms — the bare unquoted last part is what reaches the catalog.
    // ===================================================================

    mod phase64_quoted_ident_tests {
        use super::*;

        // ----- extract_name_only via rewrite_ddl (DROP / DESCRIBE / SHOW COLUMNS / ALTER source) -----

        #[test]
        fn drop_with_quoted_fqn() {
            assert_eq!(
                plan("DROP SEMANTIC VIEW \"db\".\"sch\".\"v\""),
                RewriteAction::Drop {
                    name: vref("db.sch.v"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn drop_with_quoted_bare() {
            assert_eq!(
                plan("DROP SEMANTIC VIEW \"orders_sv\""),
                RewriteAction::Drop {
                    name: vref("orders_sv"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn drop_with_unquoted_fqn() {
            assert_eq!(
                plan("DROP SEMANTIC VIEW db.sch.v"),
                RewriteAction::Drop {
                    // The qualifier is kept, not discarded: it pins WHICH `v`
                    // the DROP means now that two schemas may each hold one.
                    name: vref("db.sch.v"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn drop_with_partial_quoting() {
            assert_eq!(
                plan("DROP SEMANTIC VIEW main.\"orders_sv\""),
                RewriteAction::Drop {
                    name: vref("main.orders_sv"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn drop_with_quoted_whitespace_name() {
            // `"my view"` has an inner space — the quote-aware delimiter
            // scan must NOT truncate mid-quote.
            assert_eq!(
                plan("DROP SEMANTIC VIEW \"my view\""),
                RewriteAction::Drop {
                    name: vref("my view"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn drop_if_exists_with_quoted_fqn() {
            assert_eq!(
                plan("DROP SEMANTIC VIEW IF EXISTS \"db\".\"sch\".\"v\""),
                RewriteAction::Drop {
                    name: vref("db.sch.v"),
                    if_exists: true,
                }
            );
        }

        #[test]
        fn describe_with_quoted_fqn() {
            // FF-4: the raw qualified name is embedded verbatim; the dispatcher's
            // normalize_view_name reduces it to the bare 'v' at lookup time.
            let sql = passthrough_sql("DESCRIBE SEMANTIC VIEW \"memory\".\"main\".\"v\"");
            assert_eq!(
                sql,
                "SELECT * FROM describe_semantic_view('\"memory\".\"main\".\"v\"')"
            );
        }

        #[test]
        fn show_columns_with_quoted_fqn() {
            // FF-4: raw qualified name embedded verbatim; dispatcher reduces to 'v'.
            let sql = passthrough_sql("SHOW COLUMNS IN SEMANTIC VIEW \"memory\".\"main\".\"v\"");
            assert_eq!(
                sql,
                "SELECT * FROM show_columns_in_semantic_view('\"memory\".\"main\".\"v\"')"
            );
        }

        #[test]
        fn drop_with_unterminated_quote_errors() {
            let err = plan_ddl("DROP SEMANTIC VIEW \"foo").unwrap_err();
            assert!(
                err.message.contains("Invalid view name") && err.message.contains("unterminated"),
                "expected invalid-view-name/unterminated error, got: {err}"
            );
        }

        // ----- validate_create_body via validate_and_rewrite (CREATE / OR REPLACE / IF NOT EXISTS) -----
        //
        // We use the minimal AS-keyword body that produces a parsable
        // semantic view definition: `TABLES (...)`, `DIMENSIONS (...)`,
        // `METRICS (...)`. `plan(...)` returns `RewriteAction::Create { name,
        // .. }` carrying the bare, normalized view name — asserting it proves
        // the CREATE capture-site normalisation without needing the
        // extension-gated INSERT emission.

        const MINIMAL_BODY: &str = "AS TABLES (o AS orders PRIMARY KEY (id)) \
                                    DIMENSIONS (o.region AS o.region) \
                                    METRICS (o.total AS SUM(o.amount))";

        /// Plan a CREATE query and return the reference it captured in
        /// `RewriteAction::Create`.
        fn create_ref(query: &str) -> ViewRef {
            match plan(query) {
                RewriteAction::Create { name, .. } => name,
                other => panic!("expected RewriteAction::Create, got {other:?}"),
            }
        }

        // Semantic views are schema-scoped, so a qualifier is no longer
        // discarded on the way to the emitter: these pin BOTH halves — the
        // bare name in the name slot, and the qualifier the CREATE now
        // honours (it decides which schema the row lands in).

        #[test]
        fn create_with_quoted_fqn_captures_schema_and_bare_name() {
            let q = format!("CREATE SEMANTIC VIEW \"db\".\"sch\".\"orders_sv\" {MINIMAL_BODY}");
            assert_eq!(create_ref(&q), vref("db.sch.orders_sv"));
        }

        #[test]
        fn create_or_replace_with_quoted_fqn_captures_schema_and_bare_name() {
            let q = format!(
                "CREATE OR REPLACE SEMANTIC VIEW \"db\".\"sch\".\"orders_sv\" {MINIMAL_BODY}"
            );
            assert_eq!(create_ref(&q), vref("db.sch.orders_sv"));
        }

        #[test]
        fn create_if_not_exists_with_quoted_fqn_captures_schema_and_bare_name() {
            let q = format!(
                "CREATE SEMANTIC VIEW IF NOT EXISTS \"db\".\"sch\".\"orders_sv\" {MINIMAL_BODY}"
            );
            assert_eq!(create_ref(&q), vref("db.sch.orders_sv"));
        }

        #[test]
        fn create_with_partial_quoting_captures_schema_and_bare_name() {
            let q = format!("CREATE SEMANTIC VIEW main.\"orders_sv\" {MINIMAL_BODY}");
            let r = create_ref(&q);
            assert_eq!(r.name, "orders_sv");
            assert_eq!(r.schema.as_deref(), Some("main"));
        }

        #[test]
        fn create_with_quoted_whitespace_name_extracts_intact() {
            let q = format!("CREATE SEMANTIC VIEW \"my view\" {MINIMAL_BODY}");
            let r = create_ref(&q);
            assert_eq!(r.name, "my view");
            assert_eq!(r.schema, None, "one quoted part is a name, not a qualifier");
        }

        #[test]
        fn create_with_unterminated_quote_errors() {
            let q = format!("CREATE SEMANTIC VIEW \"foo {MINIMAL_BODY}");
            // Since the delimiter scan saturates at input.len() inside an
            // unterminated quote, normalize_view_name surfaces the error.
            let err = plan_rewrite(&q).unwrap_err();
            assert!(
                err.message.contains("Invalid view name") && err.message.contains("unterminated"),
                "expected invalid-view-name error, got: {}",
                err.message
            );
        }

        // ----- rewrite_alter — source slot AND RENAME TO target slot -----

        #[test]
        fn alter_rename_source_quoted() {
            assert_eq!(
                plan("ALTER SEMANTIC VIEW \"v\" RENAME TO new_name"),
                RewriteAction::AlterRename {
                    name: vref("v"),
                    new_name: vref("new_name"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn alter_rename_target_quoted() {
            assert_eq!(
                plan("ALTER SEMANTIC VIEW v RENAME TO \"memory\".\"main\".\"new_v\""),
                RewriteAction::AlterRename {
                    name: vref("v"),
                    // The qualifier survives into the target slot: a qualified
                    // RENAME TO moves the view into that schema.
                    new_name: vref("memory.main.new_v"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn alter_rename_both_quoted() {
            assert_eq!(
                plan(
                    "ALTER SEMANTIC VIEW \"memory\".\"main\".\"v\" RENAME TO \"memory\".\"main\".\"new_v\""
                ),
                RewriteAction::AlterRename {
                    name: vref("memory.main.v"),
                    new_name: vref("memory.main.new_v"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn alter_set_comment_with_quoted_source() {
            assert_eq!(
                plan("ALTER SEMANTIC VIEW \"v\" SET COMMENT = 'x'"),
                RewriteAction::AlterSetComment {
                    name: vref("v"),
                    comment: "x".to_string(),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn alter_unset_comment_with_quoted_source() {
            assert_eq!(
                plan("ALTER SEMANTIC VIEW \"v\" UNSET COMMENT"),
                RewriteAction::AlterUnsetComment {
                    name: vref("v"),
                    if_exists: false,
                }
            );
        }

        #[test]
        fn alter_rename_target_unterminated_quote_errors() {
            let err = plan_rewrite("ALTER SEMANTIC VIEW v RENAME TO \"foo").unwrap_err();
            assert!(
                err.message.contains("Invalid new view name in RENAME TO"),
                "expected invalid-new-view-name error, got: {}",
                err.message
            );
        }

        // ----- CREATE-branch name capture, quoted/mixed FQN forms (Site C explicit) -----

        #[test]
        fn create_quoted_fqn_captures_bare_name() {
            let q = format!("CREATE SEMANTIC VIEW \"a\".\"b\".\"c\" {MINIMAL_BODY}");
            assert_eq!(create_ref(&q), vref("a.b.c"));
        }

        #[test]
        fn create_mixed_quoting_captures_bare_name() {
            let q = format!("CREATE SEMANTIC VIEW a.\"b\".c {MINIMAL_BODY}");
            assert_eq!(create_ref(&q), vref("a.b.c"));
        }

        #[test]
        fn create_with_four_part_name_errors() {
            // A fourth part cannot be silently truncated to the last one:
            // `a.b.c.d` names nothing this catalog can hold.
            let q = format!("CREATE SEMANTIC VIEW a.b.c.d {MINIMAL_BODY}");
            let err = plan_rewrite(&q).unwrap_err();
            assert!(
                err.message.contains("Invalid view name") && err.message.contains("too many"),
                "expected a too-many-parts error, got: {}",
                err.message
            );
        }
    }

    /// TECH-DEBT #25 residual — the SHOW name slots.
    ///
    /// `IN SCHEMA` / `IN DATABASE`, `IN <view>`, and `FOR METRIC` each peeled
    /// their identifier at the first whitespace, the last members of the
    /// whitespace-tokeniser family #24 and #25 fixed elsewhere. Two distinct
    /// failures came out of that, and the tests below separate them because a
    /// fix for one does not imply the other:
    ///
    /// 1. a quoted name containing whitespace was truncated mid-quote, so the
    ///    tail became "Unexpected tokens" — a loud but wrong error;
    /// 2. a quoted name *without* whitespace was captured with its quote
    ///    characters intact. For the schema/database slots that is the worse
    ///    half: the value goes straight into `schema_name = '<literal>'`, so
    ///    `IN SCHEMA "main"` silently matched nothing.
    ///
    /// The view and metric slots keep the FF-4 convention — emit the RAW name
    /// and let the read table function fold it once at the catalog boundary —
    /// so only their capture is asserted here.
    mod show_name_slot_quoted_ident_tests {
        use super::*;

        // ----- 1. quoted names containing whitespace survive the capture -----

        #[test]
        fn in_schema_accepts_a_quoted_name_containing_whitespace() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"my schema\""),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('my schema')"
            );
        }

        #[test]
        fn in_database_accepts_a_quoted_name_containing_whitespace() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN DATABASE \"my db\""),
                "SELECT * FROM list_semantic_views() WHERE lower(database_name) = lower('my db')"
            );
        }

        #[test]
        fn in_view_accepts_a_quoted_name_containing_whitespace() {
            // FF-4: the raw (still-quoted) name is embedded; the TF normalizes.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN \"my view\""),
                "SELECT * FROM show_semantic_dimensions('\"my view\"')"
            );
        }

        #[test]
        fn for_metric_accepts_a_quoted_name_containing_whitespace() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN v FOR METRIC \"my metric\""),
                "SELECT * FROM show_semantic_dimensions_for_metric('v', '\"my metric\"')"
            );
        }

        // ----- 1b. ...and the view slot must NOT strip them -----

        #[test]
        fn a_quoted_view_name_containing_a_dot_keeps_its_quotes() {
            // The counterpart to the stripping tests below: this slot defers to
            // the read TF's `normalize_view_name`, so the quotes have to still
            // be there when it arrives.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN \"a.b\""),
                "SELECT * FROM show_semantic_dimensions('\"a.b\"')"
            );
        }

        #[test]
        fn an_unquoted_dot_is_a_qualifier_but_a_quoted_one_is_not() {
            // Why the deferral above is load-bearing rather than merely tidy.
            // `normalize_view_name` reads an UNQUOTED dot as a qualifier
            // separator and keeps only the last part, so a parser-side strip
            // would hand the TF `a.b` and resolve the view named `b` — a
            // different view that may well exist. That failure mode is a silent
            // WRONG HIT, not a miss, which is why it gets its own pin.
            assert_eq!(crate::ident::normalize_view_name("\"a.b\"").unwrap(), "a.b");
            assert_eq!(crate::ident::normalize_view_name("a.b").unwrap(), "b");
        }

        // ----- 2. the schema/database slots must also STRIP the quotes -----

        #[test]
        fn in_schema_strips_quotes_before_emitting_the_literal() {
            // Nothing downstream unquotes this one — it is compared as a plain
            // string literal — so a surviving quote character is a silent
            // no-match rather than an error.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"main\""),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn in_database_strips_quotes_before_emitting_the_literal() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN DATABASE \"memory\""),
                "SELECT * FROM list_semantic_views() WHERE lower(database_name) = lower('memory')"
            );
        }

        #[test]
        fn a_doubled_quote_inside_a_quoted_schema_name_unescapes() {
            // `""` is the SQL escape for a literal `"`, so the stored name is
            // `a"b`. Asserted separately from plain stripping: a naive
            // trim_matches('"') would produce `a""b` and pass the test above.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"a\"\"b\""),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('a\"b')"
            );
        }

        #[test]
        fn a_quoted_schema_name_may_contain_a_semicolon() {
            // `;` terminates a bare identifier but is inert inside quotes.
            // Pinned because the scan's delimiter set is wider than whitespace.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"a;b\""),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('a;b')"
            );
        }

        #[test]
        fn a_quoted_schema_name_survives_a_single_quote_escape() {
            // Quote-stripping happens BEFORE `SqlLit::escape`, so the literal
            // is still well-formed. A fix that stripped after escaping would
            // emit `'"O''Brien"'` and fail here.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"O'Brien\""),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('O''Brien')"
            );
        }

        #[test]
        fn a_quoted_schema_name_cannot_break_out_of_its_literal() {
            // The capture is deliberately WIDER than it was — bytes that used
            // to terminate the name are now accepted inside quotes — so pin
            // the escape boundary explicitly rather than trusting that the
            // stripping step left `SqlLit` in charge. The `'` doubles, which
            // keeps the payload inside the literal.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"a'; DROP TABLE t; --\""),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('a''; DROP TABLE t; --')"
            );
        }

        // ----- controls: over-rejection / over-stripping would fail here -----

        #[test]
        fn an_unquoted_schema_name_is_unchanged() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA main"),
                "SELECT * FROM list_semantic_views() WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn an_unquoted_view_and_metric_are_unchanged() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN v FOR METRIC revenue"),
                "SELECT * FROM show_semantic_dimensions_for_metric('v', 'revenue')"
            );
        }

        #[test]
        fn a_quoted_name_still_composes_with_the_other_clauses() {
            // The capture returns the right remainder, so LIKE before and
            // LIMIT after still parse. A scan that over-consumed would eat
            // them and surface as "Unexpected tokens".
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS LIKE '%x%' IN SCHEMA \"my schema\" LIMIT 3"),
                "SELECT * FROM list_semantic_views() \
                 WHERE name ILIKE '%x%' AND lower(schema_name) = lower('my schema') LIMIT 3"
            );
        }

        #[test]
        fn an_unterminated_quote_in_a_schema_name_still_errors() {
            // `find_identifier_end` saturates at end-of-input for an
            // unterminated quote, so the whole tail is captured. It must not
            // be silently accepted as a name.
            let err = plan_ddl("SHOW SEMANTIC VIEWS IN SCHEMA \"oops").unwrap_err();
            assert!(
                err.message.contains("Invalid schema name"),
                "expected an invalid-schema-name error, got: {}",
                err.message
            );
        }
    }

    /// TECH-DEBT #25 — case folding for the `IN SCHEMA` / `IN DATABASE` slots.
    ///
    /// These two are the last name sites comparing with raw `=` rather than
    /// `DuckDB`'s case-insensitive identifier rule (the rule `ident_matches`
    /// states and every other site follows). Left open by the quoting fix on
    /// the reasoning that an unquoted name is passed through as written, so
    /// folding only the quoted spelling would make quoting mean something
    /// extra.
    ///
    /// That reasoning assumed the stored side is canonical. It is not: it is
    /// stamped from `current_schema()` at CREATE time, which `DuckDB` returns
    /// as the spelling the caller typed in their last `USE` — not the
    /// catalog's. So two views in ONE schema can be stamped `MySchema` and
    /// `myschema`, and no single spelling of the filter returns both. Folding
    /// BOTH sides is what makes the filter answerable at all; it also keeps
    /// quoting meaning nothing extra, which is the invariant the original
    /// reasoning wanted.
    ///
    /// The row-level end-to-end proof is
    /// `test/integration/test_show_scope_case_insensitive.py` — it needs to
    /// compare returned rows, and `list_semantic_views` leads with a live
    /// `created_on` that cannot be written into a `.test` file.
    mod show_scope_case_folding_tests {
        use super::*;

        #[test]
        fn in_schema_folds_case_on_both_sides() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA MySchema"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('MySchema')"
            );
        }

        #[test]
        fn in_database_folds_case_on_both_sides() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN DATABASE MyDb"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(database_name) = lower('MyDb')"
            );
        }

        #[test]
        fn a_quoted_scope_name_folds_exactly_like_an_unquoted_one() {
            // A CONTROL, green before this fix as well as after — the quoting
            // fix had already made the two spellings identical. It is not
            // evidence the fold works; it guards the invariant the original
            // deferral was protecting, and would fail on the specific wrong fix
            // that deferral feared: folding the quoted spelling only.
            //
            // Asserted as an equality between the two spellings rather than
            // against a literal, so it keeps holding if the emitted form is
            // ever rewritten.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"MySchema\""),
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA MySchema")
            );
        }

        #[test]
        fn the_fold_is_left_to_sql_rather_than_applied_in_rust() {
            // Why `lower(x) = lower(y)` and not a Rust-side fold of the
            // requested name: the stored side is folded by DuckDB's `lower()`,
            // which is Unicode-aware, while Rust's `to_ascii_lowercase` (what
            // `ident::normalize_ident_part` uses) is not. Folding here would
            // emit `'myschÉma'` against a stored `'myschéma'` and STOP matching
            // a non-ASCII name that matches exactly today — a regression, not
            // merely a gap. Pin that the literal reaches SQL verbatim.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"MYSCHÉMA\""),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('MYSCHÉMA')"
            );
        }
    }

    /// `IN SCHEMA <db_name>.<schema_name>` — Snowflake's qualified scope form.
    ///
    /// Snowflake's grammar for the SHOW scope clause is
    /// `IN { <name> | ACCOUNT | DATABASE [<db>] | SCHEMA [<db>.<schema>] }`, so
    /// the two-part spelling is the documented way to name a schema in another
    /// database. It parsed here — `parse_qualified_identifier_with_quoting`
    /// accepts it — and then the parts were rejoined with `.` and compared
    /// against `schema_name`, which stores a BARE schema name. The result was
    /// `schema_name = 'memory.main'`: no match, no error, empty result.
    ///
    /// The same silent-no-match shape as TECH-DEBT #25's quoting half, and the
    /// case-fold fix could not help — folding does not make two different
    /// strings equal.
    ///
    /// A qualified schema now populates BOTH slots, which is what makes the
    /// answer correct rather than merely non-empty: matching only the schema
    /// half would return same-named schemas in OTHER databases.
    mod show_scope_qualified_name_tests {
        use super::*;

        #[test]
        fn a_qualified_schema_name_filters_on_database_and_schema() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA memory.main"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('main') \
                 AND lower(database_name) = lower('memory')"
            );
        }

        #[test]
        fn a_quoted_qualified_schema_name_splits_on_the_unquoted_dot() {
            // The split has to happen on the dot BETWEEN the quoted parts, not
            // inside either of them, so this fails both a naive `split('.')`
            // and any fix that gives up on quoting once a dot is present.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"my db\".\"my schema\""),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('my schema') \
                 AND lower(database_name) = lower('my db')"
            );
        }

        #[test]
        fn a_three_part_schema_name_is_rejected() {
            // Snowflake allows at most `<db>.<schema>` here. Three parts had no
            // sensible reading and previously became the silent no-match
            // `schema_name = 'a.b.c'`.
            let err = plan_ddl("SHOW SEMANTIC VIEWS IN SCHEMA a.b.c").unwrap_err();
            assert!(
                err.message.contains("Invalid schema name"),
                "expected an invalid-schema-name error, got: {}",
                err.message
            );
        }

        #[test]
        fn a_qualified_database_name_is_rejected() {
            // `IN DATABASE` takes ONE part in Snowflake — a database has nothing
            // to qualify it with. Silently no-matched before.
            let err = plan_ddl("SHOW SEMANTIC VIEWS IN DATABASE a.b").unwrap_err();
            assert!(
                err.message.contains("Invalid database name"),
                "expected an invalid-database-name error, got: {}",
                err.message
            );
        }

        // ----- controls: green before this change as well as after -----

        #[test]
        fn an_unqualified_schema_name_emits_no_database_predicate() {
            // Over-reach guard: the qualified path must not start inventing a
            // database predicate for the common single-part spelling.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA main"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn a_quoted_dot_is_not_a_qualifier() {
            // A CONTROL, and the reason the fix cannot be `split('.')`: a schema
            // literally named `a.b` is ONE part, and must stay one part with no
            // database predicate. Green before this change, and the single most
            // likely thing a careless fix breaks.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA \"a.b\""),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('a.b')"
            );
        }
    }

    /// TECH-DEBT #25 Group A — the scope clause on the other SHOW commands,
    /// and the rest of Snowflake's `IN` grammar.
    ///
    /// Snowflake accepts
    /// `IN { view_name | ACCOUNT | DATABASE [db] | SCHEMA [db.schema] }` on
    /// SHOW SEMANTIC DIMENSIONS / METRICS / FACTS as well as VIEWS. Here the
    /// clause was gated to VIEWS only, and the other three fell through to a
    /// branch that takes the view-name slot unconditionally — so `SCHEMA` was
    /// read as a view name and the rest surfaced as "Unexpected tokens".
    ///
    /// No rewrite plumbing is involved: those commands already emit
    /// `show_semantic_*_all()` when no view is named, and those functions lead
    /// with `database_name, schema_name`, so `build_filter_suffix`'s existing
    /// predicates apply as they stand.
    mod show_scope_other_commands_tests {
        use super::*;

        // ----- boundary: a NAME that looks like a following clause -----

        #[test]
        fn a_schema_named_startswith_is_not_the_starts_with_clause() {
            // `starts_following_clause` tells "no name was given" from "the next
            // clause begins here". LIMIT and FOR matched whole words; STARTS did
            // not, so an unquoted schema named `startswith` was read as the
            // start of a STARTS WITH clause — the bare-scope form plus a broken
            // trailing clause, instead of a schema name. Same class as the
            // `IN schemas` case on the view-name side.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA startswith"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower('startswith')"
            );
        }

        #[test]
        fn a_real_starts_with_clause_after_a_bare_scope_still_parses() {
            // The control for the row above: `STARTS` followed by whitespace IS
            // the clause, so tightening the match must not stop the bare scope
            // form from composing with it.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA STARTS WITH 'a'"),
                "SELECT * FROM list_semantic_views() \
                 WHERE starts_with(name, 'a') AND lower(schema_name) = lower(current_schema())"
            );
        }

        // ----- (a) the scope clause reaches the other three commands -----

        #[test]
        fn in_schema_scopes_show_dimensions() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN SCHEMA main"),
                "SELECT * FROM show_semantic_dimensions_all() \
                 WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn in_schema_scopes_show_metrics() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC METRICS IN SCHEMA main"),
                "SELECT * FROM show_semantic_metrics_all() \
                 WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn in_schema_scopes_show_facts() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC FACTS IN SCHEMA main"),
                "SELECT * FROM show_semantic_facts_all() \
                 WHERE lower(schema_name) = lower('main')"
            );
        }

        #[test]
        fn in_database_scopes_show_dimensions() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN DATABASE memory"),
                "SELECT * FROM show_semantic_dimensions_all() \
                 WHERE lower(database_name) = lower('memory')"
            );
        }

        #[test]
        fn a_qualified_schema_scopes_show_dimensions() {
            // The qualified form from #184 has to reach the new commands too,
            // rather than only the VIEWS path it was fixed on.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN SCHEMA memory.main"),
                "SELECT * FROM show_semantic_dimensions_all() \
                 WHERE lower(schema_name) = lower('main') \
                 AND lower(database_name) = lower('memory')"
            );
        }

        // ----- (b) the rest of Snowflake's IN grammar -----

        #[test]
        fn in_account_emits_no_scope_predicate() {
            // DuckDB has no account. Snowflake's ACCOUNT scope means "everything
            // I can see", which here is the unfiltered listing — so the clause
            // parses and contributes nothing rather than erroring.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN ACCOUNT"),
                "SELECT * FROM list_semantic_views()"
            );
        }

        #[test]
        fn in_account_scopes_show_dimensions_too() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN ACCOUNT"),
                "SELECT * FROM show_semantic_dimensions_all()"
            );
        }

        #[test]
        fn a_bare_in_schema_scopes_to_the_current_schema() {
            // Emitted as a call, not a literal, so it resolves on the caller's
            // connection. That also makes it exact: `schema_name` is stamped
            // from `current_schema()` at CREATE, so both sides are the same
            // function.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN SCHEMA"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(schema_name) = lower(current_schema())"
            );
        }

        #[test]
        fn a_bare_in_database_scopes_to_the_current_database() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC VIEWS IN DATABASE"),
                "SELECT * FROM list_semantic_views() \
                 WHERE lower(database_name) = lower(current_database())"
            );
        }

        #[test]
        fn a_bare_in_schema_on_dimensions_is_the_scope_not_a_view_named_schema() {
            // THE BACK-COMPAT CHANGE, pinned deliberately. Before this, on the
            // non-VIEWS commands `IN SCHEMA` meant "the view named SCHEMA" and
            // rewrote to show_semantic_dimensions('SCHEMA'). It now means the
            // current schema. A view actually named `schema` is still reachable
            // by quoting it — see the control below.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN SCHEMA"),
                "SELECT * FROM show_semantic_dimensions_all() \
                 WHERE lower(schema_name) = lower(current_schema())"
            );
        }

        #[test]
        fn a_quoted_view_named_schema_is_still_reachable() {
            // The escape hatch that makes the back-compat change tolerable:
            // quoting forces the view-name reading.
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN \"schema\""),
                "SELECT * FROM show_semantic_dimensions('\"schema\"')"
            );
        }

        // ----- controls: the view-name slot must be untouched -----

        #[test]
        fn an_ordinary_view_name_still_selects_the_single_view_function() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN orders_sv"),
                "SELECT * FROM show_semantic_dimensions('orders_sv')"
            );
        }

        #[test]
        fn for_metric_still_composes_with_a_view_name() {
            assert_eq!(
                passthrough_sql("SHOW SEMANTIC DIMENSIONS IN v FOR METRIC revenue"),
                "SELECT * FROM show_semantic_dimensions_for_metric('v', 'revenue')"
            );
        }
    }
}
