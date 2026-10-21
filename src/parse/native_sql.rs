//! Native-SQL emission for the `parser_override` DDL path (AR-1).
//!
//! Every recognised CREATE / DROP / ALTER semantic-view statement is rewritten
//! here into native `INSERT` / `DELETE` / `UPDATE` against
//! `semantic_layer._definitions` (plus pure-SQL existence/collision guards), so
//! the writes run on the caller's connection inside the caller's transaction.
//! Read-side DDL (DESCRIBE / SHOW) is passed through unchanged by
//! [`rewrite_to_native_sql`], the dispatch entry point.
//!
//! SQL-string escaping is handled by the [`crate::sql_lit::SqlLit`] newtype
//! (R-1): names are escaped exactly once at this dispatch boundary via
//! `SqlLit::escape`, and the emission helpers take `&SqlLit` so a raw `&str`
//! cannot be embedded by mistake. The emission/rewrite functions are
//! `extension`-gated; `rewrite_to_native_sql` is re-exported from the parent
//! module for the FFI entry points. The guard-SELECT builders in
//! `crate::catalog::writes` are compiled unconditionally so their wording
//! unit tests run under `cargo test`.

#[cfg(feature = "extension")]
use super::{plan_rewrite, RewriteAction};
#[cfg(feature = "extension")]
use crate::catalog::writes::{
    create_target_schema_expr, current_database_guard_select, definitions_table_guard_select,
    existence_guard_select, rename_collision_guard_select, resolved_schema_expr, row_predicate,
    SchemaTarget,
};
#[cfg(feature = "extension")]
use crate::catalog::DEFINITIONS_TABLE;
#[cfg(feature = "extension")]
use crate::errors::ParseError;
#[cfg(feature = "extension")]
use crate::ident::{parse_view_ref, ViewRef};
#[cfg(feature = "extension")]
use crate::sql_lit::SqlLit;

/// Re-check an already-normalised bare view name, returning it unchanged.
///
/// Re-quoted before re-parsing so the pass is a no-op on normalised input: a
/// bare `a.b` would otherwise re-split into schema `a` / name `b`.
#[cfg(feature = "extension")]
fn revalidate_name(name: &str) -> Result<String, ParseError> {
    parse_view_ref(&crate::expand::quote_ident(name))
        .map(|r| r.name)
        .map_err(|e| ParseError {
            message: format!("Invalid view name: {e}"),
            position: None,
        })
}

/// Split a parsed view reference into the escaped name literal and the schema
/// target its qualifier (if any) denotes.
///
/// The `database` part is not represented here — it names no schema. It is
/// enforced separately by [`current_database_guard`], because ignoring it would
/// silently retarget `otherdb.analytics.v` at the current database's
/// `analytics.v`.
#[cfg(feature = "extension")]
fn escaped_parts(view: &ViewRef) -> (SqlLit, SchemaTarget) {
    let target = view.schema.as_ref().map_or(SchemaTarget::Unqualified, |s| {
        SchemaTarget::Named(SqlLit::escape(s))
    });
    (SqlLit::escape(&view.name), target)
}

/// The name as it should be *written back* in an error suggesting a qualified
/// spelling — identifier-quoted when it needs to be, then SQL-escaped for
/// embedding. Quoting has to happen before the SQL escape, which is why this
/// cannot be derived from the `SqlLit` that `escaped_parts` returns.
#[cfg(feature = "extension")]
fn suggested_name(view: &ViewRef) -> SqlLit {
    SqlLit::escape(&crate::expand::quote_ident_if_needed(&view.name))
}

/// [`current_database_guard_select`] for a parsed reference: the guard
/// statement plus its trailing `; ` when a `<database>.` prefix was written,
/// and the empty string when it was not, so callers can splice it in
/// unconditionally.
#[cfg(feature = "extension")]
fn current_database_guard(view: &ViewRef) -> String {
    view.database.as_ref().map_or_else(String::new, |database| {
        format!(
            "{}; ",
            current_database_guard_select(
                &SqlLit::escape(database),
                &SqlLit::escape(&view.to_string())
            )
        )
    })
}

// ---------------------------------------------------------------------------
// v0.8.x: native-SQL rewrite for parser_override (transactional DDL)
// ---------------------------------------------------------------------------
//
// `parser_override` is the sole semantic-view DDL entry point. Every recognised
// statement is rewritten here and re-executed on the caller's connection by
// DuckDB — the legacy parse_function / sv_ddl_internal fallback was retired
// in v0.8.0.
//
// Rewriting is dispatched by shape:
//
//   * CREATE / CREATE OR REPLACE / CREATE IF NOT EXISTS
//     CREATE ... FROM YAML FILE '/path/...'
//     DROP / DROP IF EXISTS
//     ALTER ... RENAME TO / SET COMMENT / UNSET COMMENT
//       → emitted as native INSERT / DELETE / UPDATE against
//         `semantic_layer._definitions`, so writes participate in the
//         caller's transaction (the v0.8.0 ADBC autocommit=false fix).
//
//   * DESCRIBE / SHOW SEMANTIC * / GET_DDL / READ_YAML_FROM_SEMANTIC_VIEW
//       → passed through as `SELECT * FROM <existing_read_side_table_function>(...)`
//         (or the same SQL with WHERE/LIMIT clauses appended). DuckDB re-parses
//         and executes on the caller's connection. The read-side table functions
//         themselves query via a fresh per-call `Connection(*context.db)` opened
//         in each C++ bind callback (committed state; the `catalog_conn` static
//         was retired in Phase 65). Making DESCRIBE/SHOW transactional w.r.t. the
//         caller's snapshot is blocked by a DuckDB liveness constraint — see
//         TECH-DEBT #19 for the full analysis.
//
//   * Anything else (`validate_and_rewrite` returns None)
//       → `Ok(None)`; the C++ shim returns DISPLAY_ORIGINAL_ERROR and DuckDB's
//         default parser handles it.
//
// AR-7: this used to take `&OverrideContext`, but that struct was empty after
// Phase 65 Plan 06 moved CREATE/DROP/ALTER existence checks into the emitted
// SQL (pure-SQL race guards on the caller's connection) — so the parameter was
// dead and has been removed. Under `cargo test` (no extension feature) this
// path is excluded entirely (this entry point is feature-gated; its sole
// caller — `sv_parser_override_rust` — is `extension`-only).
//
// INVARIANT (AR-5) — purity / idempotence. This function MUST be a pure
// function of `query` (and committed catalog state): for a given input it
// must produce the same `Ok(Some)` / `Ok(None)` / `Err(message, position)`
// result on every call, with no dependence on call order, wall-clock time,
// `HashMap` iteration order, or any mutable process state. The error-reporting
// layer depends on this: after the override path runs, DuckDB's failed default
// parser drives `sv_parse_function_rust`, which calls this function a SECOND
// time (via `run_validation_for_parse_function`) purely to recover the same
// `Err` message and caret position the override produced. If the two runs can
// diverge, the caret error shown to the user no longer matches the rewrite
// that actually ran. Any future change that reads mutable state or introduces
// nondeterminism here breaks that contract and must instead cache the first
// run's `(query -> result)` rather than re-deriving it.
#[cfg(feature = "extension")]
pub(crate) fn rewrite_to_native_sql(query: &str) -> Result<Option<String>, ParseError> {
    // Not semantic-view DDL: DuckDB's own parser handles it. (Statements that
    // only *query* a semantic view used to be rewritten here to carry the
    // caller's search path; the read functions now take it from the caller's
    // context themselves.)
    let Some(action) = plan_rewrite(query)? else {
        return Ok(None);
    };

    // Every `<database>.` prefix the statement writes must name the current
    // database — collected before the match consumes `action`. Read-side DDL
    // contributes none.
    let database_guards: String = action
        .referenced_views()
        .into_iter()
        .map(current_database_guard)
        .collect();

    // Read-side DDL is passed through unchanged; write DDL gets the FF-3
    // single-catalog guard prepended below.
    let emitted: Option<String> = match action {
        // Read-side DDL (DESCRIBE / SHOW / SHOW COLUMNS): DuckDB runs the
        // read-side table function on the caller's connection unchanged.
        RewriteAction::Passthrough(sql) => return Ok(Some(sql)),
        // CREATE from an in-memory definition — hand the definition straight to
        // the shared emission path. AR-2: no JSON serialize → re-parse →
        // deserialize round-trip; the `SemanticViewDefinition` flows structurally.
        RewriteAction::Create { name, def, mode } => {
            emit_native_create_sql(&name, *def, mode.or_replace(), mode.if_not_exists())?
        }
        // CREATE FROM YAML FILE — emit the INSERT that selects from the
        // `__sv_compute_create_from_yaml` helper TF (which reads the file at
        // execution). AR-2: no `\x01`-delimited sentinel string.
        RewriteAction::CreateFromYamlFile {
            file_path,
            name,
            comment,
            mode,
        } => emit_native_create_from_yaml_file(
            &file_path,
            &name,
            &comment,
            mode.or_replace(),
            mode.if_not_exists(),
        )?,
        // DROP / ALTER: pure-SQL race-guard + native DML on the caller's
        // connection. Names carried raw; `SqlLit::escape` at the boundary
        // produces the escaped literal the emission helpers embed (R-1: the
        // escaped-vs-raw distinction is type-enforced, not by convention).
        // The comment is passed RAW — `rewrite_alter_comment` needs it
        // un-escaped to build the JSON patch and escapes the patch itself.
        RewriteAction::Drop { name, if_exists } => rewrite_drop(&name, if_exists)?,
        RewriteAction::AlterRename {
            name,
            new_name,
            if_exists,
        } => rewrite_alter_rename(&name, &new_name, if_exists)?,
        RewriteAction::AlterSetComment {
            name,
            comment,
            if_exists,
        } => rewrite_alter_comment(&name, Some(&comment), if_exists)?,
        RewriteAction::AlterUnsetComment { name, if_exists } => {
            rewrite_alter_comment(&name, None, if_exists)?
        }
    };

    // FF-3: prepend the single-catalog guard to every write DDL. Run as the
    // FIRST statement so multi-statement execution short-circuits before the
    // DML when the caller is USE-d into a database that isn't the one holding
    // the semantic-view catalog (typically after `USE <attached_db>`). Without
    // it, such a write either fails with a cryptic "schema semantic_layer does
    // not exist" (CREATE) or writes a row the primary-pinned reads never see.
    // The guard is a no-op on the normal single-catalog path.
    Ok(emitted.map(|dml| {
        format!(
            "{}; {database_guards}{dml}",
            crate::catalog::writes::managed_catalog_guard_select()
        )
    }))
}

/// Shared CREATE-emission helper for the in-memory-definition path
/// (`RewriteAction::Create`). The FROM YAML FILE path uses the sibling
/// `emit_native_create_from_yaml_file`.
///
/// Steps (Phase 65 Plan 06 — pure-SQL):
/// 1. Run `enrich_definition_for_create` (validation + graph + serialize
///    to JSON; no catalog connection needed).
/// 2. Emit `INSERT [OR REPLACE / OR IGNORE] INTO semantic_layer._definitions
///    ... RETURNING name AS view_name` so `DuckDB` executes the write on the
///    caller's connection inside the caller's transaction. The plain CREATE
///    form (no OR REPLACE, no IF NOT EXISTS) wraps the INSERT in a
///    CASE+error subquery that emits "already exists" wording — replaces
///    the pre-Plan-06 `catalog.exists()` Rust-side pre-check.
///
/// For IF NOT EXISTS on an already-existing view, `INSERT OR IGNORE`
/// absorbs the duplicate silently — equivalent shape to the legacy
/// `SELECT ... WHERE 1 = 0` fast path (zero rows returned).
#[cfg(feature = "extension")]
fn emit_native_create_sql(
    view: &ViewRef,
    def: crate::model::SemanticViewDefinition,
    or_replace: bool,
    if_not_exists: bool,
) -> Result<Option<String>, ParseError> {
    // Defensive validation — `view` arrives already normalised (every part
    // case-folded) from validate_create_body via the `RewriteAction::Create` it
    // produced. Only the NAME slot is re-checked, and re-quoted first so the
    // pass is a true no-op on normalised input: re-parsing the bare name would
    // split a dotted one (`"a.b"` → schema `a`, name `b`). The qualifier is
    // carried through untouched — re-deriving it would lose it.
    let view = ViewRef {
        name: revalidate_name(&view.name)?,
        ..view.clone()
    };
    let (name_escaped, schema_target) = escaped_parts(&view);
    let name = &view.name;

    // Phase 65 (D-16, metadata-via-SQL): enrichment no longer takes a
    // catalog connection. CREATE-time `now()` / `current_database()` /
    // `current_schema()` capture is embedded as SQL inside the emitted
    // INSERT via `json_merge_patch` so it resolves on the CALLER's
    // connection at INSERT-time, preserving D-21 transactional contract
    // without parser_override holding a long-lived handle. CREATE-time
    // column type inference (`column_type_names`, fact `output_type`)
    // is deferred to read-side bind under Plan 05's C++ Catalog API
    // migration (D-17).
    let enriched_json = crate::ddl::define::enrich_definition_for_create(name, def)?;
    let enriched_escaped = SqlLit::escape(&enriched_json);

    // The schema this CREATE lands in — the qualifier if one was written,
    // otherwise `current_schema()`, resolved either way to the catalog's own
    // spelling and erroring if the schema does not exist. It appears twice in
    // the emitted INSERT (the `schema_name` column and the JSON metadata), so
    // the row and its definition can never disagree about where the view lives.
    let schema_expr = create_target_schema_expr(&schema_target);

    // Metadata-via-SQL sub-expression: produces a VARCHAR by patching
    // the enriched JSON (no created_on / database_name / schema_name
    // fields populated by the Rust side) with the now()/current_database()
    // /schema values resolved on the caller's connection.
    //
    // RFC-7396 semantics: json_merge_patch overrides any keys present in
    // the patch. Phase 39 metadata behaviour is preserved because the
    // enriched JSON omits the three metadata keys (Vec::is_empty /
    // Option::is_none skip_serializing) so the patch is the sole source.
    // AR-4: stamp the storage-format version alongside the metadata so every
    // freshly written row records `schema_version`. It is injected here (not
    // carried on the struct) so it never leaks into YAML export.
    let schema_version = crate::model::CURRENT_SCHEMA_VERSION;
    let metadata_patched_definition = format!(
        "json_merge_patch( \
            '{enriched_escaped}'::JSON, \
            json_object( \
              'created_on', strftime(now(), '%Y-%m-%dT%H:%M:%SZ'), \
              'database_name', current_database(), \
              'schema_name', {schema_expr}, \
              'resolution_schema_name', current_schema(), \
              'schema_version', {schema_version} \
            ) \
         )::VARCHAR"
    );

    // The generated SQL runs on the caller's connection, so its EXISTS
    // subqueries see in-flight INSERTs from the same transaction. Three
    // shapes:
    //   - OR REPLACE: straight INSERT OR REPLACE, no guard needed.
    //   - IF NOT EXISTS: INSERT OR IGNORE absorbs same-snapshot duplicates
    //     (the same-txn duplicate path, mirroring the SELECT WHERE 1=0
    //     fast path on committed-state hits). It does *not* paper over
    //     a cross-connection committer race: two transactions that each
    //     see no row will both INSERT, and DuckDB's PK constraint raises
    //     a write-write conflict on the second commit. That matches plain
    //     CREATE concurrency semantics — see TECH-DEBT item 23.
    //   - Plain CREATE: CASE+error() raises the friendly "already exists"
    //     message before the INSERT can fire, replacing what would
    //     otherwise be a generic PK constraint violation. Phase 65: the
    //     parser-side `ctx.catalog.exists` pre-check above is the
    //     committed-state fast path; the CASE inside the INSERT is the
    //     same-transaction guard.
    //
    // All three write `(schema_name, name, definition)`; the conflict target
    // is the `(schema_name, name)` primary key, so OR REPLACE / OR IGNORE act
    // within the target schema only and a same-named view in another schema is
    // left alone.
    let occupied = row_predicate(&name_escaped, &schema_expr);
    let sql = if or_replace {
        format!(
            "INSERT OR REPLACE INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT {schema_expr}, '{name_escaped}', {metadata_patched_definition} \
             RETURNING name AS view_name"
        )
    } else if if_not_exists {
        format!(
            "INSERT OR IGNORE INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT {schema_expr}, '{name_escaped}', {metadata_patched_definition} \
             RETURNING name AS view_name"
        )
    } else {
        format!(
            "INSERT INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT \
               {schema_expr}, \
               CASE WHEN EXISTS (SELECT 1 FROM {DEFINITIONS_TABLE} \
                                 WHERE {occupied}) \
                    THEN error('semantic view ''{name_escaped}'' already exists; \
                                use CREATE OR REPLACE SEMANTIC VIEW to overwrite') \
                    ELSE '{name_escaped}' \
               END, \
               {metadata_patched_definition} \
             RETURNING name AS view_name"
        )
    };
    Ok(Some(sql))
}

/// Read the FROM YAML FILE sentinel produced by `rewrite_ddl_yaml_file_body`
/// and emit a transactional INSERT that selects from the
/// `__sv_compute_create_from_yaml(path, name, kind, comment)` helper TF
/// (registered via the C++ Catalog API in `cpp/src/shim.cpp`). The helper's
/// bind callback opens a per-call `Connection(*context.db)`, runs
/// `read_text()` against the user-supplied path, calls into Rust to parse
/// and enrich the YAML, and returns a metadata-less JSON in a single row.
/// The outer INSERT wraps that row with `json_merge_patch` to add the
/// metadata fields (`created_on`, `database_name`, `schema_name`) on the
/// caller's connection -- matching `emit_native_create_sql`'s non-YAML
/// behaviour byte-for-byte.
///
/// Phase 65 Plan 06: pure-SQL, no extension-owned catalog connection. The YAML
/// read happens inside the `__sv_compute_create_from_yaml` helper TF's
/// bind callback (per-call `Connection(*context.db)`), not on any
/// long-lived extension-owned connection.
#[cfg(feature = "extension")]
fn emit_native_create_from_yaml_file(
    file_path: &str,
    view: &ViewRef,
    comment: &str,
    or_replace: bool,
    if_not_exists: bool,
) -> Result<Option<String>, ParseError> {
    // Phase 65.1 Plan 07 (IN-04 D-24): `kind` is not threaded into the helper
    // TF — the outer INSERT shape (OR IGNORE / OR REPLACE / plain) already
    // encodes the ON CONFLICT behaviour, chosen from `or_replace`/`if_not_exists`.

    // Defensive validation of the name (matches emit_native_create_sql):
    // only the NAME slot is re-checked, and re-quoted first so the pass is a
    // no-op on already-normalised input; the qualifier rides through untouched.
    let view = ViewRef {
        name: revalidate_name(&view.name)?,
        ..view.clone()
    };
    let (name_escaped, schema_target) = escaped_parts(&view);
    let schema_expr = create_target_schema_expr(&schema_target);
    let occupied = row_predicate(&name_escaped, &schema_expr);
    let path_escaped = SqlLit::escape(file_path);
    let comment_escaped = SqlLit::escape(comment);

    // Helper-TF subquery + metadata-via-SQL wrapper. The helper TF returns
    // exactly one row whose `new_def` column contains the metadata-less
    // enriched JSON. We patch in the metadata fields on the caller's
    // connection so they reflect the user's session (matches Plan 03's
    // non-YAML CREATE behaviour byte-for-byte).
    //
    // RFC-7396 semantics (verified by Plan 04 Wave 0 spike): json_merge_patch
    // overrides keys present in the patch. The helper TF's new_def omits the
    // three metadata keys (skip_serializing_if on the struct), so the patch
    // is the sole source -- no risk of overwriting a user-supplied value.
    // AR-4: stamp schema_version alongside the metadata (see the inline-CREATE
    // sibling above). Injected here rather than carried on the struct so it
    // stays out of YAML export.
    let metadata_patched = format!(
        "json_merge_patch( \
            new_def::JSON, \
            json_object( \
              'created_on', strftime(now(), '%Y-%m-%dT%H:%M:%SZ'), \
              'database_name', current_database(), \
              'schema_name', {schema_expr}, \
              'resolution_schema_name', current_schema(), \
              'schema_version', {schema_version} \
            ) \
         )::VARCHAR",
        schema_version = crate::model::CURRENT_SCHEMA_VERSION
    );
    let helper_from = format!(
        "FROM __sv_compute_create_from_yaml('{path_escaped}', \
            '{name_escaped}', '{comment_escaped}')"
    );

    // Three INSERT shapes mirror the inline CREATE path
    // (emit_native_create_sql):
    //   OR REPLACE     : INSERT OR REPLACE -- no friendly-error guard needed.
    //   IF NOT EXISTS  : INSERT OR IGNORE absorbs same-snapshot duplicates.
    //   Plain          : CASE+error guard inside SELECT raises the friendly
    //                    "already exists" message before the INSERT can fire
    //                    (Phase 60 race-guard pattern carried forward).
    let sql = if or_replace {
        format!(
            "INSERT OR REPLACE INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT {schema_expr}, '{name_escaped}', {metadata_patched} \
             {helper_from} \
             RETURNING name AS view_name"
        )
    } else if if_not_exists {
        format!(
            "INSERT OR IGNORE INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT {schema_expr}, '{name_escaped}', {metadata_patched} \
             {helper_from} \
             RETURNING name AS view_name"
        )
    } else {
        format!(
            "INSERT INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
             SELECT \
               {schema_expr}, \
               CASE WHEN EXISTS (SELECT 1 FROM {DEFINITIONS_TABLE} \
                                 WHERE {occupied}) \
                    THEN error('semantic view ''{name_escaped}'' already exists; \
                                use CREATE OR REPLACE SEMANTIC VIEW to overwrite') \
                    ELSE '{name_escaped}' \
               END, \
               {metadata_patched} \
             {helper_from} \
             RETURNING name AS view_name"
        )
    };
    Ok(Some(sql))
}

// SQL-string escaping is handled by the `SqlLit` newtype (`crate::sql_lit`),
// which makes the escaped-vs-raw distinction a compile-time contract instead
// of a naming convention (R-1). The old free `escape_sql_arg` /
// `unescape_sql_arg` pair was removed: names are escaped exactly once at the
// `rewrite_to_native_sql` boundary via `SqlLit::escape`, and the comment now
// flows RAW to `rewrite_alter_comment` (no escape→unescape round-trip).

#[cfg(feature = "extension")]
// Infallible today, but kept `Result`-returning for symmetry with the fallible
// `rewrite_*` siblings (e.g. `rewrite_alter_comment`) dispatched through the
// same `?`-chained match in `rewrite_to_native_sql`; diverging one signature
// would fragment that dispatch.
#[allow(clippy::unnecessary_wraps)]
fn rewrite_drop(view: &ViewRef, if_exists: bool) -> Result<Option<String>, ParseError> {
    // Which schema's `v` this DROP means: the qualifier if one was written,
    // otherwise the one schema holding a view of that name (erroring when
    // several do, rather than dropping an arbitrary one).
    let (name_escaped, target) = escaped_parts(view);
    let name_escaped = &name_escaped;
    let display = SqlLit::escape(&view.to_string());
    let schema_expr = resolved_schema_expr(&target, name_escaped, &suggested_name(view));
    let row = row_predicate(name_escaped, &schema_expr);
    if if_exists {
        // IF EXISTS: pure DELETE on the caller's connection — affects 0
        // rows when the view is missing (silent no-op contract).
        //
        // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard`
        // so the DELETE never binds against a missing
        // `semantic_layer._definitions` on a never-bootstrapped RO DB
        // (which would otherwise leak `Catalog Error: Table
        // _definitions does not exist`). When the table is missing the
        // guard errors with the canonical "does not exist" wording and
        // the DELETE is never bound (per-statement lazy bind — see
        // `definitions_table_guard_select` docs). The silent-no-op
        // contract for missing-row-but-table-present is preserved by
        // the DELETE's 0-row effect.
        let table_guard = definitions_table_guard_select(&display);
        return Ok(Some(format!(
            "{table_guard}; \
             DELETE FROM {DEFINITIONS_TABLE} WHERE {row} \
             RETURNING name AS view_name"
        )));
    }

    // Plain DROP: pure-SQL existence guard + DELETE on the caller's
    // connection. The guard's NOT EXISTS check is snapshot-consistent with
    // the DELETE only within an explicit caller transaction; under autocommit
    // the two statements auto-commit separately, so a concurrent DROP can
    // leave the loser's DELETE matching 0 rows and reporting success having
    // deleted nothing (FF-1 / TECH-DEBT #27 — see the transactional-scope note
    // on `existence_guard_select`). Phase 65 Plan 06: the legacy
    // `catalog.exists()` Rust-side pre-check is gone — H1 catalog_conn retired;
    // the guard subsumes both the never-existed case and the (in-transaction)
    // concurrent-drop case under a single "does not exist" wording.
    //
    // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard` so
    // neither the row-existence guard NOR the DELETE bind against a
    // missing `semantic_layer._definitions` on a never-bootstrapped RO
    // DB. Three-statement form: <table_guard>; <row_guard>; <DELETE>.
    // First statement errors → second and third never bind.
    let table_guard = definitions_table_guard_select(&display);
    let guard = existence_guard_select(&row, &display);
    Ok(Some(format!(
        "{table_guard}; \
         {guard}; \
         DELETE FROM {DEFINITIONS_TABLE} WHERE {row} \
         RETURNING name AS view_name"
    )))
}

///
/// A qualifier on the NEW name moves the view: `ALTER SEMANTIC VIEW a.v RENAME
/// TO b.v` lands it in schema `b`. An unqualified new name keeps the view where
/// it is, so the common `RENAME TO other` never moves anything.
#[cfg(feature = "extension")]
// Infallible today; kept `Result`-returning for symmetry with the fallible
// `rewrite_*` siblings dispatched through the same match (see `rewrite_drop`).
#[allow(clippy::unnecessary_wraps)]
fn rewrite_alter_rename(
    old: &ViewRef,
    new: &ViewRef,
    if_exists: bool,
) -> Result<Option<String>, ParseError> {
    let (old_escaped, old_target) = escaped_parts(old);
    let (new_escaped, new_target) = escaped_parts(new);
    let (old_escaped, new_escaped) = (&old_escaped, &new_escaped);
    let old_display = SqlLit::escape(&old.to_string());
    let new_display = SqlLit::escape(&new.to_string());
    let source_schema = resolved_schema_expr(&old_target, old_escaped, &suggested_name(old));
    // Where the row ends up: the new name's own qualifier when it has one,
    // otherwise wherever the source already lives.
    let dest_schema = match &new_target {
        SchemaTarget::Unqualified => source_schema.clone(),
        named @ SchemaTarget::Named(_) => create_target_schema_expr(named),
    };
    let source_row = row_predicate(old_escaped, &source_schema);
    let dest_row = row_predicate(new_escaped, &dest_schema);
    // The stored definition carries a `schema_name` metadata field that the
    // SHOW / DESCRIBE listings read. Patch it alongside the column so a
    // schema-moving rename cannot leave the two disagreeing about where the
    // view lives (a no-op when the rename stays put — the patch writes the
    // same value back).
    let moved_definition = format!(
        "json_merge_patch(definition::JSON, \
            json_object('schema_name', {dest_schema}))::VARCHAR"
    );
    if if_exists {
        // IF EXISTS: pure UPDATE on the caller's connection. We still need
        // the rename-collision guard (target name must not be taken),
        // because PK violations from DuckDB's UPDATE produce a less
        // actionable error message. The guard's EXISTS check is
        // snapshot-consistent with the UPDATE only within an explicit caller
        // transaction; under autocommit a concurrent writer can take the
        // target name in the window between the guard and the UPDATE, and the
        // UPDATE then surfaces the raw PK error the guard meant to pre-empt
        // (FF-1 / TECH-DEBT #27). The UPDATE itself silently affects 0 rows on
        // a missing source row — matches the IF EXISTS contract.
        //
        // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard`
        // so neither the collision guard NOR the UPDATE bind against a
        // missing `semantic_layer._definitions` on a never-bootstrapped
        // RO DB.
        let table_guard = definitions_table_guard_select(&old_display);
        let collision_guard = rename_collision_guard_select(&dest_row, &new_display);
        return Ok(Some(format!(
            "{table_guard}; \
             {collision_guard}; \
             UPDATE {DEFINITIONS_TABLE} \
                SET name = '{new_escaped}', \
                    schema_name = {dest_schema}, \
                    definition = {moved_definition} \
              WHERE {source_row} \
             RETURNING '{old_escaped}'::VARCHAR AS old_name, name AS new_name"
        )));
    }

    // Plain ALTER RENAME: pure-SQL existence guard (source must exist) +
    // collision guard (target must not exist) + UPDATE. The EXISTS checks are
    // snapshot-consistent with the DML only within an explicit caller
    // transaction; under autocommit each statement auto-commits separately, so
    // a concurrent committer in the guard window can make the source vanish or
    // the target appear, surfacing a raw PK error or a silent no-op (FF-1 /
    // TECH-DEBT #27). Phase 65 Plan 06: the legacy `catalog.exists()`
    // Rust-side pre-checks are gone.
    //
    // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard` so
    // none of the row guards / UPDATE bind against a missing
    // `semantic_layer._definitions` on a never-bootstrapped RO DB.
    let table_guard = definitions_table_guard_select(&old_display);
    let exist_guard = existence_guard_select(&source_row, &old_display);
    let collision_guard = rename_collision_guard_select(&dest_row, &new_display);
    Ok(Some(format!(
        "{table_guard}; \
         {exist_guard}; \
         {collision_guard}; \
         UPDATE {DEFINITIONS_TABLE} \
            SET name = '{new_escaped}', \
                schema_name = {dest_schema}, \
                definition = {moved_definition} \
          WHERE {source_row} \
         RETURNING '{old_escaped}'::VARCHAR AS old_name, name AS new_name"
    )))
}

#[cfg(feature = "extension")]
fn rewrite_alter_comment(
    view: &ViewRef,
    new_comment_raw: Option<&str>,
    if_exists: bool,
) -> Result<Option<String>, ParseError> {
    let (name_escaped, target) = escaped_parts(view);
    let display = SqlLit::escape(&view.to_string());
    let schema_expr = resolved_schema_expr(&target, &name_escaped, &suggested_name(view));
    let row = row_predicate(&name_escaped, &schema_expr);
    // Phase 65 Plan 06 — all pure-SQL on the caller's connection:
    //   - ALTER SET/UNSET COMMENT uses json_merge_patch (Plan 04 Wave 0
    //     spike confirmed DuckDB v1.5.2 honors RFC-7396 null-as-delete).
    //   - Existence is enforced by the existence_guard_select preceding
    //     the UPDATE (plain ALTER) — replaces the legacy `catalog.exists()`
    //     Rust-side pre-check. IF EXISTS uses a plain UPDATE that affects
    //     0 rows on a missing source.
    //
    // The legacy "does not exist" wording is preserved by
    // existence_guard_select — matches phase45's expectations
    // byte-for-byte.

    // Build the json_merge_patch patch literal.
    //   SET COMMENT 'new text' -> `'{"comment":"new text"}'::JSON`
    //   UNSET COMMENT          -> `'{"comment":null}'::JSON`  (RFC-7396 null-as-delete)
    //
    // For SET, we use serde_json::to_string on a one-key object so internal
    // `"` and `\` characters in the user's comment are JSON-escaped
    // correctly; then `SqlLit::escape` doubles any embedded single quotes for
    // the outer single-quoted SQL literal. Belt-and-braces escape: JSON
    // first (handles `"`/`\`/control chars), SQL second (handles `'`).
    let (patch_json_for_sql, status_label) =
        match new_comment_raw {
            Some(comment) => {
                // The comment arrives RAW; serde_json handles `"`/`\`/control
                // chars, then `SqlLit::escape` doubles any `'` for the outer
                // single-quoted SQL literal (belt-and-braces: JSON then SQL).
                let patch = serde_json::to_string(&serde_json::json!({"comment": comment}))
                    .map_err(|e| ParseError {
                        message: format!("failed to build comment patch: {e}"),
                        position: None,
                    })?;
                (SqlLit::escape(&patch), "comment set")
            }
            None => {
                // UNSET COMMENT: constant patch (no single quotes, so the
                // escape is a no-op — wrapped in SqlLit for type parity with
                // the SET arm). The Wave 0 spike empirically confirms DuckDB
                // v1.5.2 implements RFC-7396 null-as-delete.
                (SqlLit::escape(r#"{"comment":null}"#), "comment unset")
            }
        };

    if if_exists {
        // IF EXISTS preserves its silent contract on race: pre-check saw the
        // row; if a concurrent DROP commits before our UPDATE, the UPDATE
        // simply affects 0 rows.
        //
        // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard`
        // so the UPDATE never binds against a missing
        // `semantic_layer._definitions` on a never-bootstrapped RO DB
        // (which would leak `Catalog Error: Table _definitions does
        // not exist`). On missing-table the guard errors with the
        // canonical wording; on missing-row-but-table-present the
        // UPDATE's 0-row effect preserves the silent IF EXISTS contract.
        let table_guard = definitions_table_guard_select(&display);
        return Ok(Some(format!(
            "{table_guard}; \
             UPDATE {DEFINITIONS_TABLE} \
                SET definition = json_merge_patch(definition::JSON, '{patch_json_for_sql}'::JSON)::VARCHAR \
              WHERE {row} \
             RETURNING name, '{status_label}'::VARCHAR AS status"
        )));
    }

    // Plain ALTER: pure-SQL existence guard + UPDATE on the caller's
    // connection. The guard's NOT EXISTS check is snapshot-consistent with the
    // UPDATE only within an explicit caller transaction; under autocommit the
    // guard and UPDATE auto-commit separately, so a concurrent DROP in the
    // guard window leaves the UPDATE affecting 0 rows (FF-1 / TECH-DEBT #27).
    // Concurrent ALTER-against-the-same-row carries no lost-update risk
    // regardless, because we apply the mutation via json_merge_patch ON THE
    // CURRENT ROW — not a Rust-side snapshot.
    //
    // Phase 65.1 Plan 04 (WR-03): prepend a `definitions_table_guard` so
    // neither the row guard NOR the UPDATE bind against a missing
    // `semantic_layer._definitions` on a never-bootstrapped RO DB.
    let table_guard = definitions_table_guard_select(&display);
    let guard = existence_guard_select(&row, &display);
    Ok(Some(format!(
        "{table_guard}; \
         {guard}; \
         UPDATE {DEFINITIONS_TABLE} \
            SET definition = json_merge_patch(definition::JSON, '{patch_json_for_sql}'::JSON)::VARCHAR \
          WHERE {row} \
         RETURNING name, '{status_label}'::VARCHAR AS status"
    )))
}
