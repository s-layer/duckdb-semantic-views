//! CREATE planning and read-side lowering for semantic-view DDL (AR-1).
//!
//! [`plan_create`] turns a `CREATE … SEMANTIC VIEW` statement's text into a
//! structured [`RewriteAction`]: CREATE-body parsing lives in `create_body`
//! and native-SQL emission in `native_sql`. The grammar productions
//! (`crate::grammar`) build the DROP / ALTER actions themselves, and lower
//! DESCRIBE / SHOW through [`lower_name_only_read`] and [`lower_show`].

use crate::errors::ParseError;
use crate::ident::ViewRef;

use super::{detect_create_prefix, skip_leading_whitespace, validate_create_body, DdlKind};

// The read-side lowering serves the grammar productions only, which compile
// with the extension or for tests.
#[cfg(any(feature = "extension", test))]
use super::{build_filter_suffix, show_clauses::ShowClauses};
#[cfg(any(feature = "extension", test))]
use crate::sql_lit::SqlLit;

/// The DROP / ALTER / DESCRIBE messages the grammar productions
/// (`crate::grammar`) report.
#[cfg(any(feature = "extension", test))]
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

// ---------------------------------------------------------------------------
// Read-side DDL lowering (DESCRIBE / SHOW → SELECT * FROM <read TF>(...))
// ---------------------------------------------------------------------------

/// Map a read-side `DdlKind` to the read table function it lowers to.
///
/// Only the read-side kinds are lowered to a function-call `Passthrough`;
/// CREATE routes through [`plan_create`], and DROP/ALTER become structured
/// native-DML `RewriteAction`s (their v0.5-era `*_semantic_view` function
/// names were retired in v0.8.0 and are no longer registered). `panic`ing on
/// a write kind is unreachable: every caller is a read-side grammar
/// production.
#[cfg(any(feature = "extension", test))]
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

/// Lower a name-only read statement (DESCRIBE, SHOW COLUMNS) to its read table
/// function. The name is passed raw: the table function folds and resolves it
/// at the catalog-read boundary (FF-4).
#[cfg(any(feature = "extension", test))]
pub(crate) fn lower_name_only_read(kind: DdlKind, raw_name: &str) -> String {
    let safe_name = SqlLit::escape(raw_name);
    let fn_name = read_function_name(kind);
    format!("SELECT * FROM {fn_name}('{safe_name}')")
}

/// Lower a `SHOW SEMANTIC ...` statement, already split into its clauses, to a
/// query over the read table functions, for the grammar production. `position` is where the
/// clauses start, for the one error found here.
#[cfg(any(feature = "extension", test))]
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
// Validation layer: ParseError, plan_create
// ---------------------------------------------------------------------------

/// Structured outcome of parsing/validating a semantic-view DDL statement (AR-2).
///
/// The planners produce one of these directly. Previously they rendered a legacy
/// `SELECT * FROM <fn>('arg', ...)` string that the emitter immediately
/// re-parsed; CREATE additionally round-tripped its definition
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
    // Consumed by `lower_action` (extension-only); unused under
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
    // Consumed by `lower_action` (extension-only) when choosing the
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

/// Plan a `CREATE … SEMANTIC VIEW` statement as a structured [`RewriteAction`]
/// (AR-2): `Create` for an `AS` body or inline `FROM YAML`, `CreateFromYamlFile`
/// for `FROM YAML FILE`. Returns `Ok(None)` when the text is not a CREATE.
///
/// The grammar's CREATE production calls this with the statement's text,
/// rebuilt from its tokens; the pure-Rust tests and fuzz targets call it
/// directly.
pub fn plan_create(query: &str) -> Result<Option<RewriteAction>, ParseError> {
    // PA-7: blank comments once at the entry point (byte-length-preserving,
    // so every error-caret position stays valid for the original text).
    // Downstream scanners and captured expressions never see comment text.
    let (blanked, unterminated) = crate::util::blank_sql_comments_tracking(query);
    let query = blanked.as_ref();
    let lead = skip_leading_whitespace(query);
    let trimmed = query[lead..].trim_end();
    let trimmed_no_semi = trimmed.trim_end_matches(';').trim();

    let Some((kind, plen)) = detect_create_prefix(trimmed_no_semi) else {
        return Ok(None);
    };
    // Past an unterminated quote the pass blanked nothing, so a comment there
    // would be planned as code; `DuckDB`'s tokenizer refuses such text before
    // the grammar sees it. A quote the body parser reads (in the body, or a
    // `"` in the name, which `parse_view_ref` owns) gets its own message from
    // it. A `'` or `$tag$` in a bare name is read by nothing, so it is refused
    // first, before the body parser trips on comment text left in place. That
    // holds wherever the quote closes: one that closes in the body hides the
    // body's comments from the pass just the same. So the check is on the
    // name's own text, scanned on its own: a quote still open at its end.
    // Measured from the slice itself: `trim` also strips leading non-ASCII
    // whitespace, which `lead` does not count.
    let name_start = crate::util::byte_offset_within(query, trimmed_no_semi[plen..].trim_start());
    let name_end = name_start + crate::ident::find_identifier_end(&query[name_start..], true);
    let (_, open_in_name) = crate::util::blank_sql_comments_tracking(&query[..name_end]);
    if let Some(pos) = open_in_name.filter(|&p| query.as_bytes()[p] != b'"') {
        return Err(if unterminated == Some(pos) {
            unterminated_quote(query, pos)
        } else {
            quote_in_view_name(query, pos)
        });
    }
    let planned = validate_create_body(trimmed_no_semi, lead, plen, kind)?;
    if let Some(pos) = unterminated {
        return Err(unterminated_quote(query, pos));
    }
    Ok(planned)
}

/// The error for a quoted region opened at `pos` and never closed.
fn unterminated_quote(query: &str, pos: usize) -> ParseError {
    let message = match query.as_bytes()[pos] {
        b'"' => "Unterminated quoted identifier.",
        b'$' => "Unterminated dollar-quoted string.",
        _ => "Unterminated string literal.",
    };
    ParseError {
        message: message.to_string(),
        position: Some(pos),
    }
}

/// A `'` or `$tag$` opened in a bare view name that closes later: the name is
/// not a string, so it is refused as such rather than as unterminated.
fn quote_in_view_name(query: &str, pos: usize) -> ParseError {
    let what = if query.as_bytes()[pos] == b'$' {
        "a dollar-quoted string"
    } else {
        "a string literal"
    };
    ParseError {
        message: format!(
            "A view name cannot be {what}; quote the name with double quotes, as in \"my view\"."
        ),
        position: Some(pos),
    }
}

// Cardinality inference (Phase 33) now lives in `crate::graph::cardinality`
// (moved out of `parse` in AR-1 to remove the `ddl` -> `parse` layering
// inversion — it is semantic-graph logic, not parsing). Callers use
// `crate::graph::infer_cardinality`.

#[cfg(test)]
mod tests {
    use super::*;
    // `crate::parse::*` supplies the coordinator re-exports the tests use.
    use crate::ident::parse_view_ref;
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
        plan_create(query)
            .expect("valid DDL")
            .expect("recognized semantic-view DDL statement")
    }

    // ===================================================================
    // PA-7 (code-review 2026-07-02): SQL comments are blanked once at the
    // entry point, so they can no longer corrupt scanning state or be
    // absorbed into stored expressions.
    // ===================================================================

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

    #[test]
    fn test_unterminated_quote_is_refused_not_planned() {
        // Past a stray quote the comment pass blanks nothing, so `-- a` was
        // planned into the key column, and re-planning the rendered DDL
        // (where the quote is gone) dropped it: a render that was not a
        // fixpoint (fuzz_create_text, CI run 40187299120).
        let text = "CREATE SEMANTIC VIEW v' AS TABLES (o AS t PRIMARY KEY (x -- a\n y)) \
                    METRICS (o.n AS COUNT(*))";
        let err = plan_create(text).expect_err("a stray quote in the name is refused");
        assert_eq!(err.message, "Unterminated string literal.");
        assert_eq!(err.position, Some(text.find('\'').unwrap()));
        // The stray quote is reported even when the body after it would trip
        // the body parser on comment text the pass left in place.
        let text = "CREATE SEMANTIC VIEW v' AS TABLES (o AS t PRIMARY KEY (id)) -- note";
        let err = plan_create(text).expect_err("refused");
        assert_eq!(err.message, "Unterminated string literal.");
        assert_eq!(err.position, Some(text.find('\'').unwrap()));
        // The same for a dollar quote opening at the name.
        let text = "CREATE SEMANTIC VIEW $a$ AS TABLES (o AS t PRIMARY KEY (x -- a\n y)) \
                    METRICS (o.n AS COUNT(*))";
        let err = plan_create(text).expect_err("refused");
        assert_eq!(err.message, "Unterminated dollar-quoted string.");
        assert_eq!(err.position, Some(text.find('$').unwrap()));
        // Text that is not a CREATE is still not ours, quote or no quote.
        assert!(matches!(plan_create("SELECT 'open"), Ok(None)));
    }

    #[test]
    fn test_unterminated_quote_in_body_keeps_body_parser_message() {
        // The body parser already refuses a stray quote where it looks, with
        // a message naming the clause; the entry-point check must not
        // replace it.
        for (text, message) in [
            (
                "CREATE SEMANTIC VIEW v AS TABLES (o AS t PRIMARY KEY (x)) COMMENT = 'open",
                "Unclosed single-quoted string.",
            ),
            (
                "CREATE SEMANTIC VIEW v AS TABLES (o AS \"t PRIMARY KEY (x))",
                "Unterminated quoted identifier in clause 'TABLES'.",
            ),
        ] {
            let err = plan_create(text).expect_err(text);
            assert_eq!(err.message, message, "{text}");
        }
    }

    // ===================================================================
    // plan_create tests
    // ===================================================================

    #[test]
    fn test_validate_and_rewrite_rejects_paren_body() {
        // CLN-01: non-AS-body syntax rejected with clear error
        let result =
            plan_create("CREATE SEMANTIC VIEW sales (tables := [...], dimensions := [...])");
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
        let result = plan_create("SELECT 1");
        assert!(result.is_ok());
        assert!(result.unwrap().is_none());
    }

    // ===================================================================
    // ParseError position tests
    // ===================================================================

    #[test]
    fn test_parse_error_position_paren_body_rejected() {
        // Non-AS-body syntax returns "Expected 'AS' or 'FROM YAML'" error with position
        let query = "CREATE SEMANTIC VIEW x (tables := [])";
        let result = plan_create(query);
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
        let result = plan_create(query);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.position.is_some());
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
            let result = plan_create(query);
            assert!(result.is_err(), "Paren-body must be rejected: {result:?}");
            let err = result.unwrap_err();
            assert!(
                err.message.contains("Expected 'AS' or 'FROM YAML'"),
                "Expected 'Expected AS or FROM YAML' error, got: {}",
                err.message
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
    // Phase 34.1.1: the SHOW SEMANTIC filter suffix
    // ===================================================================

    mod phase34_1_1_show_filter_tests {
        use super::*;

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
        let err = plan_create(query).unwrap_err();
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
        let err = plan_create(query).unwrap_err();
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
    // helper comments are already spaces.
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

    // ===================================================================
    // Phase 64: Quoted identifier handling (QID-01..QID-06).
    //
    // Wires `crate::ident::{normalize_view_name, find_identifier_end}` into
    // CREATE's name capture, exercised here through `plan_create` with quoted
    // and FQN forms — the bare unquoted last part is what reaches the catalog.
    // (The grammar productions capture the DROP / ALTER / DESCRIBE / SHOW
    // names; their quoting rows are in the `ddl_corpus_*.test` files.)
    // ===================================================================

    mod phase64_quoted_ident_tests {
        use super::*;

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
            let err = plan_create(&q).unwrap_err();
            assert!(
                err.message.contains("Invalid view name") && err.message.contains("unterminated"),
                "expected invalid-view-name error, got: {}",
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
            let err = plan_create(&q).unwrap_err();
            assert!(
                err.message.contains("Invalid view name") && err.message.contains("too many"),
                "expected a too-many-parts error, got: {}",
                err.message
            );
        }
    }
}
