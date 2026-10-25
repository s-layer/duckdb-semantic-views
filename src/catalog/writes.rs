//! Write-side SQL builders for the `semantic_layer._definitions` catalog table
//! (AR-1).
//!
//! These functions build the pure-SQL existence/collision guards that the
//! DROP/ALTER emitters (`crate::parse::native_sql`) prepend
//! to their DML. They live here, next to the table identity
//! ([`super::DEFINITIONS_TABLE`]) and the canonical "does not exist" wording
//! ([`super::view_not_found_msg`]) they mirror, rather than in the parse layer
//! that consumes them. Callers pass a [`crate::sql_lit::SqlLit`] (a name
//! already `''`-escaped exactly once); each builder embeds it into a
//! single-quoted literal.
//!
//! All three are compiled unconditionally (they have no FFI dependency) so the
//! guard-wording unit tests below run under `cargo test`; the `allow(dead_code)`
//! covers the bundled-non-test build where only the extension emitters call
//! them.

use super::{DEFINITIONS_SCHEMA, DEFINITIONS_TABLE, DEFINITIONS_TABLE_NAME};
use crate::sql_lit::SqlLit;

/// How a statement names the schema a semantic view lives in.
///
/// Semantic views are scoped to a schema: `analytics.v` and `staging.v` are two
/// different views. A statement either writes the schema out (`Named`) or
/// leaves it to be resolved (`Unqualified`) — and what "resolved" means differs
/// between creating a view and finding an existing one, which is why the two
/// have separate expression builders below.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) enum SchemaTarget {
    /// No `schema.` qualifier was written.
    Unqualified,
    /// An explicit qualifier, already `''`-escaped exactly once.
    Named(SqlLit),
}

/// SQL scalar yielding the schema a CREATE should write into: the catalog's
/// canonical spelling of the named schema, or of `current_schema()` when the
/// statement is unqualified.
///
/// Three things make this a lookup rather than a literal:
///
/// * **The catalog's spelling is authoritative.** `current_schema()` echoes
///   whatever spelling the last `USE` used — `USE "MYSCHEMA"` reports
///   `MYSCHEMA` for a schema actually named `MySchema`. Storing that verbatim
///   would key rows by a spelling the catalog never had.
/// * **`(schema_name, name)` is a primary key**, so the stored schema must be
///   one deterministic string per schema. `DuckDB` schema names are unique
///   case-insensitively, so the catalog spelling is exactly that.
/// * **A missing schema must fail.** `CREATE SEMANTIC VIEW nosuch.v` errors
///   like `CREATE TABLE nosuch.t` does, instead of silently landing the view in
///   the current schema.
///
/// Scoped to `current_database()` — semantic views are single-catalog
/// (TECH-DEBT #26), and the FF-3 guard in [`managed_catalog_guard_select`]
/// already rejects the USE-d-into-another-database case.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn create_target_schema_expr(target: &SchemaTarget) -> String {
    let wanted = match target {
        SchemaTarget::Unqualified => "current_schema()".to_string(),
        SchemaTarget::Named(schema) => format!("'{schema}'"),
    };
    format!(
        "COALESCE( \
           (SELECT s.schema_name FROM duckdb_schemas() s \
             WHERE s.database_name = current_database() \
               AND lower(s.schema_name) = lower({wanted}) \
             LIMIT 1), \
           error('semantic_views: schema ''' || {wanted} || \
                 ''' does not exist') \
         )"
    )
}

/// SQL yielding the caller's schema resolution order, most significant first.
///
/// `current_schemas(false)` is what `SET search_path` / `USE` set, and is
/// **empty** on a fresh connection — so `current_schema()` is appended to cover
/// the default. Duplicates (`USE a` already puts `a` on the path) are harmless:
/// resolution takes the first match. `current_schemas(true)` is deliberately not
/// used: it prepends the *temp* schema, itself called `main`, which would shadow
/// a real schema of that name. The read side builds the same order in C++
/// (`sv_caller_search_path`), so the two resolve a bare name alike.
pub(crate) const SEARCH_PATH_SQL: &str = "list_concat(current_schemas(false), [current_schema()])";

/// SQL scalar yielding the schema an existing semantic view lives in.
///
/// A qualifier answers the question outright. Unqualified, the answer comes
/// from the catalog, resolved through the caller's **search path** — `DuckDB`'s
/// rule for every other unqualified object name, and the one the read side now
/// follows too. The branches, in the order the emitted `CASE` takes them:
///
/// * **At most one candidate** — that schema, or `NULL` when the name matched
///   nothing (so the caller's existence guard reports the canonical "does not
///   exist"). A lone view is reachable by its bare name even from a schema
///   that is off the path, which is what makes an unqualified `DROP` keep
///   working in the ordinary single-schema case.
/// * **No candidate is on the path** — an error naming the schemas the view
///   *does* live in. `DuckDB` would call this "does not exist"; bare, that is
///   baffling for a view `SHOW SEMANTIC VIEWS` plainly lists, so the message
///   says where it is and how to reach it.
/// * **Otherwise** — the candidate whose schema appears earliest on the path.
///
/// This mirrors [`crate::catalog::resolve_in_search_path`] branch for branch,
/// deliberately: writes and reads resolving a bare name differently would mean
/// `DROP SEMANTIC VIEW v` deleting a view other than the one `semantic_view('v')`
/// reads. The read side takes the path from the caller's context in its bind
/// (`sv_caller_search_path` in the shim); here the emitted SQL runs on the
/// caller's own connection, so it just evaluates [`SEARCH_PATH_SQL`] there.
///
/// Both sides of the position lookup are folded — `DuckDB` matches identifiers
/// case-insensitively, and a row may carry a different case than the path entry.
///
/// Referencing `_definitions`, this expression must not be bound on a
/// never-bootstrapped database — every caller already runs behind
/// [`definitions_table_guard_select`].
///
/// `suggested` is the name **identifier-quoted if it needs to be**, used only in
/// the off-path message: a view called `my view` or `a.b` must be suggested as
/// `<schema>."my view"`, since an unquoted suggestion is a syntax error or —
/// worse, for a dotted name — parses as a different qualified reference. It is
/// a separate argument rather than derived here because `name` has already been
/// SQL-escaped, and identifier quoting has to happen before that (R-1: names
/// are escaped exactly once, at the `lower_action` boundary).
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn resolved_schema_expr(
    target: &SchemaTarget,
    name: &SqlLit,
    suggested: &SqlLit,
) -> String {
    match target {
        SchemaTarget::Named(schema) => format!("'{schema}'"),
        SchemaTarget::Unqualified => {
            // The path twice over, and the difference matters. `folded` is for
            // matching — identifiers compare case-insensitively, and a row may
            // carry a different case than the path entry. `path` is what the
            // caller configured, and is what the message shows: reporting
            // `(analytics)` to someone who wrote `SET search_path = 'Analytics'`
            // names a path they did not set, and disagrees with the read side,
            // which prints its input verbatim.
            let path = SEARCH_PATH_SQL;
            let folded = format!("list_transform({path}, lambda s: lower(s))");
            // 1-based position of a row's schema on the path, or NULL when the
            // schema is not on it. `count(rank)` therefore counts the reachable
            // candidates, and `arg_min` over it picks the earliest.
            let rank = format!("list_position({folded}, lower(schema_name))");
            format!(
                "(SELECT CASE \
                     WHEN count(*) <= 1 THEN min(schema_name) \
                     WHEN count({rank}) = 0 \
                       THEN error('semantic view ''{name}'' does not exist on the search path. \
                                   It exists in schemas ' \
                                  || string_agg(schema_name, ', ' ORDER BY schema_name) \
                                  || ', none of which are on the current search path (' \
                                  || array_to_string({path}, ', ') \
                                  || '). Qualify the reference as <schema>.{suggested}, or add \
                                      the schema to search_path.') \
                     ELSE arg_min(schema_name, {rank}) \
                            FILTER (WHERE {rank} IS NOT NULL) END \
               FROM {DEFINITIONS_TABLE} WHERE name = '{name}')"
            )
        }
    }
}

/// The `WHERE` predicate identifying one semantic view row, given a schema
/// expression from [`resolved_schema_expr`] or [`create_target_schema_expr`].
///
/// Schema names are compared folded on both sides: `DuckDB` matches
/// identifiers case-insensitively, and a row written before the catalog
/// spelling was canonicalised may carry a different case than the qualifier
/// the caller writes. The view name needs no fold — it is stored already
/// folded by `normalize_view_name`.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn row_predicate(name: &SqlLit, schema_expr: &str) -> String {
    format!("name = '{name}' AND lower(schema_name) = lower({schema_expr})")
}

/// Build the existence-guard SELECT for non-IF-EXISTS DROP/ALTER.
///
/// `name` is the view name already `''`-escaped as a [`SqlLit`] (produced
/// via `SqlLit::escape` at the `lower_action` boundary).
///
/// The emitted statement errors with `semantic view '<name>' does not
/// exist` when the row is missing from the catalog table (`DEFINITIONS_TABLE`).
/// Caller appends `;` and the actual DELETE/UPDATE.
///
/// # Transactional scope of the guard (FF-1)
///
/// The guard and the DML are emitted as consecutive statements of one
/// multi-statement rewrite that `DuckDB` re-parses and runs on the caller's
/// connection. Their atomicity — and therefore whether the guard's check is
/// snapshot-consistent with the DML — depends entirely on the caller's
/// transaction state:
///
/// * **Inside an explicit transaction** (`BEGIN … COMMIT`, or an ADBC/PG
///   connection with `autocommit = false`): every emitted statement shares
///   the one open transaction and its MVCC snapshot, so the guard's decision
///   is consistent with the DML that follows. This is the atomic path.
/// * **Under autocommit** (the default): `DuckDB` commits after *each* statement
///   of a multi-statement string, so the guard and the DML execute in
///   **separate implicit transactions**. A different connection that commits
///   in the window between them can invalidate the guard's decision:
///   - concurrent DROP — both droppers' existence guards pass, both DELETEs
///     run; the loser's DELETE matches 0 rows and reports success having
///     deleted nothing (a silent no-op, not an error);
///   - concurrent RENAME — the loser's collision guard passes, then the
///     UPDATE hits `DuckDB`'s primary-key constraint and surfaces a raw
///     `Constraint Error: Duplicate key` instead of the friendly
///     `already exists` wording.
///
/// This guard window is accepted debt (TECH-DEBT #27), the DROP/ALTER sibling
/// of the CREATE race in #23. It is **not** closed by wrapping the rewrite in
/// an emitted `BEGIN … COMMIT`: `DuckDB` rejects a nested `BEGIN` (`cannot start
/// a transaction within a transaction`), so that wrapper would fail outright
/// whenever the caller is already in a transaction, and an emitted `COMMIT`
/// would prematurely commit an `autocommit = false` caller's in-flight work —
/// breaking the very transaction-participation contract the native-DML rewrite
/// exists to provide. Callers needing atomic check-and-write should wrap their
/// own DDL in `BEGIN … COMMIT` (the atomic path above).
///
/// Phase 65 Plan 06: this guard subsumes both (a) the legacy "view never
/// existed" catalog pre-check (retired with H1 `catalog_conn`) AND (b)
/// the Phase 60 race-guard for "row dropped between pre-check and DML".
/// A single "does not exist" message covers both cases — matches the
/// wording the v0.6.0 sqllogictests pin (`phase20_extended_ddl`,
/// `phase34_1_alter_rename`, `phase45_alter_comment`, `65_alter_*`).
///
/// The CTE form `WITH op AS (DELETE ... RETURNING)` is rejected by `DuckDB`
/// 1.10.502 with `Parser Error: A CTE needs a SELECT`, so we use a
/// two-statement string instead. See the smoke test
/// `catalog::tests::two_statement_guard_then_dml_smoke` for the working shape.
/// Phase 65.1 Plan 04 (WR-03): outer `information_schema` guard.
///
/// Emits a SELECT that errors with the canonical
/// `semantic view '<name>' does not exist` wording when
/// `semantic_layer._definitions` is missing (e.g. a fresh RO DB that was
/// never RW-LOADed, so `init_catalog` never ran). Designed to run as the
/// FIRST statement in a multi-statement string so the subsequent
/// statements (which reference `_definitions` directly) never bind on a
/// never-bootstrapped DB — `DuckDB` binds and executes multi-statement
/// strings one statement at a time, so a failure here short-circuits the
/// rest (empirically verified — see Plan 04 SUMMARY for probe notes).
///
/// We deliberately do NOT collapse this into a single CASE expression
/// with `existence_guard_select`: `DuckDB` binds CASE branches eagerly, so
/// the inner `SELECT 1 FROM semantic_layer._definitions ...` would still
/// fail to bind on missing-table even if the outer WHEN guarantees it
/// would never evaluate at runtime.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn definitions_table_guard_select(name: &SqlLit) -> String {
    format!(
        "SELECT CASE \
              WHEN NOT EXISTS (SELECT 1 FROM information_schema.tables \
                                WHERE table_schema = '{DEFINITIONS_SCHEMA}' \
                                  AND table_name = '{DEFINITIONS_TABLE_NAME}') \
                THEN error('semantic view ''{name}'' does not exist') \
              ELSE TRUE \
            END"
    )
}

///
/// `row` is the schema-scoped predicate from [`row_predicate`]. When its schema
/// expression evaluates to `NULL` — an unqualified reference no schema holds —
/// the predicate is never true, so the guard reports "does not exist", which is
/// exactly the case.
///
/// `display` is the reference **as the caller wrote it**, used only in the
/// message. It is separate from the name inside `row` so a qualified miss reads
/// `semantic view 'staging.v' does not exist` rather than naming a bare `v`
/// that does exist — in another schema.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn existence_guard_select(row: &str, display: &SqlLit) -> String {
    format!(
        "SELECT CASE WHEN NOT EXISTS \
                   (SELECT 1 FROM {DEFINITIONS_TABLE} WHERE {row}) \
                THEN error('semantic view ''{display}'' does not exist') \
                ELSE TRUE END"
    )
}

/// Build the "target name must NOT already exist" guard for ALTER RENAME.
/// Errors with `semantic view '<new_name>' already exists` if a row with
/// the new name is found in the target schema (`schema_expr` — the schema the
/// rename lands in, which is the source's unless the new name carries its own
/// qualifier). Runs as a statement of the rewrite preceding the UPDATE; its
/// EXISTS check is
/// snapshot-consistent with the UPDATE only within an explicit caller
/// transaction — see the transactional-scope note on
/// [`existence_guard_select`] (FF-1 / TECH-DEBT #27) for the autocommit
/// guard window (a concurrent committer can take the target name between the
/// guard and the UPDATE, surfacing a raw PK constraint error).
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn rename_collision_guard_select(row: &str, display: &SqlLit) -> String {
    format!(
        "SELECT CASE WHEN EXISTS \
                   (SELECT 1 FROM {DEFINITIONS_TABLE} WHERE {row}) \
                THEN error('semantic view ''{display}'' already exists') \
                ELSE TRUE END"
    )
}

/// Build the guard rejecting a `<database>.` prefix that names some database
/// other than the caller's.
///
/// Semantic views are single-catalog (TECH-DEBT #26): the catalog table lives
/// in one database and every read resolves against it, so a statement spelling
/// out a *different* database cannot be honoured. It must not be quietly
/// applied to the current one either, which is what dropping the prefix would
/// do — `DROP SEMANTIC VIEW otherdb.analytics.v` would then delete the current
/// database's `analytics.v`. That is a wrong-object write rather than an
/// unsupported one, which is why this errors instead of ignoring the prefix.
///
/// Distinct from [`managed_catalog_guard_select`], which catches the *implicit*
/// case (the caller `USE`-d into another database); this one catches the
/// explicit spelling. `db` is the written database name and `display` the whole
/// reference as the caller wrote it, both `''`-escaped exactly once. The
/// comparison calls `current_database()` rather than baking in a literal, so it
/// resolves on the caller's connection at execution.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn current_database_guard_select(db: &SqlLit, display: &SqlLit) -> String {
    format!(
        "SELECT CASE WHEN lower('{db}') <> lower(current_database()) \
                THEN error('semantic_views: ''{display}'' names database ''{db}'', but \
                            semantic views are single-catalog and this session''s database is ''' \
                           || current_database() || \
                           '''. Manage them from the database the extension was loaded into.') \
                ELSE TRUE END"
    )
}

/// Build the single-catalog guard prepended to every write DDL (FF-3).
///
/// Semantic views are single-catalog: `semantic_layer._definitions` is created
/// only in the database the extension was loaded into (the primary), and every
/// read looks it up in that primary catalog, whatever the caller has `USE`-d. A write issued while the caller is `USE`-d into a different (e.g.
/// attached) database resolves `semantic_layer._definitions` against that other
/// catalog. In the common case that catalog has no `semantic_layer` schema, so
/// the write would otherwise fail with a cryptic
/// `schema semantic_layer does not exist` (CREATE) or a misleading
/// "does not exist" (DROP/ALTER).
///
/// This guard turns that into an actionable single-catalog error. It fires when
/// a semantic-view catalog exists in SOME OTHER database but NOT the current one
/// — exactly the "USE-d into the wrong database, and this database has no
/// catalog" case. It is a no-op on the normal single-catalog path (the current
/// database holds the catalog) and on a fresh / never-bootstrapped DB (no
/// catalog in any database — the existing table/row guards handle that). It uses
/// `duckdb_tables()`, which spans every attached catalog, rather than
/// `information_schema.tables`, which only sees the current one.
///
/// Residual (documented single-catalog limitation — TECH-DEBT #26): if the
/// attached database the caller is `USE`-d into ALSO has its own
/// `semantic_layer._definitions` (e.g. it was itself bootstrapped as a primary
/// at some point), the guard does NOT fire — the write lands in that catalog
/// while the primary-pinned reads never see it. Detecting this requires knowing
/// which catalog the read binds use, which is not exposed on the caller's
/// connection; fully closing it is the reader-context-threading work tracked as
/// AR-6 (see TECH-DEBT #26). Managing two independent semantic-view catalogs
/// from one session is unsupported until then.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn managed_catalog_guard_select() -> String {
    format!(
        "SELECT CASE \
              WHEN EXISTS (SELECT 1 FROM duckdb_tables() \
                            WHERE schema_name = '{DEFINITIONS_SCHEMA}' \
                              AND table_name = '{DEFINITIONS_TABLE_NAME}' \
                              AND database_name <> current_database()) \
               AND NOT EXISTS (SELECT 1 FROM duckdb_tables() \
                            WHERE schema_name = '{DEFINITIONS_SCHEMA}' \
                              AND table_name = '{DEFINITIONS_TABLE_NAME}' \
                              AND database_name = current_database()) \
                THEN error('semantic_views: semantic-view DDL was issued against database ''' \
                           || current_database() || \
                           ''', but the semantic view catalog lives in a different database. \
                           Semantic views are single-catalog: manage them from the database the \
                           extension was loaded into, without USE-ing into an attached database.') \
              ELSE TRUE \
            END"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The schema expression a plain unqualified statement builds its guards
    /// from. Spelled out once here so each guard test reads as "this guard,
    /// that schema" rather than repeating the resolution subquery.
    fn unqualified(name: &str) -> String {
        resolved_schema_expr(
            &SchemaTarget::Unqualified,
            &SqlLit::escape(name),
            &SqlLit::escape(&crate::expand::quote_ident_if_needed(name)),
        )
    }

    #[test]
    fn existence_guard_select_emits_not_exists_and_error() {
        let sales = SqlLit::escape("sales");
        let g = existence_guard_select(&row_predicate(&sales, &unqualified("sales")), &sales);
        assert!(g.contains("NOT EXISTS"), "missing NOT EXISTS: {g}");
        assert!(
            g.contains("FROM semantic_layer._definitions WHERE name = 'sales'"),
            "guard targets wrong table/predicate: {g}"
        );
        assert!(
            g.contains("error('semantic view ''sales'' does not exist')"),
            "missing error() with 'does not exist' wording: {g}"
        );
        // Must be a SELECT (so it can run as the first of two statements
        // without affecting catalog state when the row is present).
        assert!(g.trim_start().starts_with("SELECT "), "not a SELECT: {g}");
        // Must not contain a trailing ';' — the caller appends ';' + DML.
        assert!(!g.contains(';'), "guard must not include ';' itself: {g}");
    }

    #[test]
    fn definitions_table_guard_emits_information_schema_check() {
        // Phase 65.1 Plan 04 (WR-03): the table-guard SELECT runs as the
        // FIRST statement of the DROP/ALTER rewrite. It checks
        // information_schema for `_definitions` and errors with the
        // canonical "does not exist" wording when the table is missing.
        // It does NOT touch `_definitions` itself — bind-time-safe on a
        // never-bootstrapped RO DB.
        let g = definitions_table_guard_select(&SqlLit::escape("sales"));
        assert!(
            g.contains("information_schema.tables"),
            "missing information_schema guard: {g}"
        );
        assert!(
            g.contains("table_schema = 'semantic_layer'"),
            "guard missing schema predicate: {g}"
        );
        assert!(
            g.contains("table_name = '_definitions'"),
            "guard missing table predicate: {g}"
        );
        assert!(
            g.contains("error('semantic view ''sales'' does not exist')"),
            "missing canonical wording: {g}"
        );
        // Must NOT touch `semantic_layer._definitions` directly — that's
        // the whole point of running this BEFORE the row guard / DML.
        assert!(
            !g.contains("FROM semantic_layer._definitions"),
            "table guard must not bind against _definitions (defeats the purpose): {g}"
        );
        assert!(g.trim_start().starts_with("SELECT "), "not a SELECT: {g}");
        assert!(!g.contains(';'), "guard must not include ';' itself: {g}");
    }

    #[test]
    fn definitions_table_guard_escapes_quotes_in_name() {
        // Quote-doubling for embedded `'` inside the canonical error
        // wording — same convention as `existence_guard_select`.
        let g = definitions_table_guard_select(&SqlLit::escape("O'Brien"));
        assert!(
            g.contains("error('semantic view ''O''Brien'' does not exist')"),
            "error message wrong: {g}"
        );
    }

    #[test]
    fn existence_guard_select_doubles_quotes_in_name() {
        // SqlLit::escape doubles the single quote; embedding it inside
        // an outer SQL string literal preserves correct decoding (DuckDB
        // sees ''X'' as 'X' in the literal). The user-facing error message
        // must read: semantic view 'O'Brien' does not exist.
        let obrien = SqlLit::escape("O'Brien");
        let g = existence_guard_select(&row_predicate(&obrien, &unqualified("O'Brien")), &obrien);
        assert!(
            g.contains("WHERE name = 'O''Brien'"),
            "WHERE clause wrong: {g}"
        );
        assert!(
            g.contains("error('semantic view ''O''Brien'' does not exist')"),
            "error message wrong: {g}"
        );
    }

    #[test]
    fn rename_collision_guard_select_emits_exists_and_error() {
        let taken = SqlLit::escape("taken");
        let schema = resolved_schema_expr(
            &SchemaTarget::Named(SqlLit::escape("analytics")),
            &taken,
            &taken,
        );
        let g = rename_collision_guard_select(&row_predicate(&taken, &schema), &taken);
        assert!(g.contains("EXISTS"), "missing EXISTS: {g}");
        assert!(
            !g.contains("NOT EXISTS"),
            "must be EXISTS, not NOT EXISTS: {g}"
        );
        assert!(
            g.contains("FROM semantic_layer._definitions WHERE name = 'taken'"),
            "guard targets wrong table/predicate: {g}"
        );
        assert!(
            g.contains("lower(schema_name) = lower('analytics')"),
            "collision guard must be scoped to the target schema: {g}"
        );
        assert!(
            g.contains("error('semantic view ''taken'' already exists')"),
            "missing error() with 'already exists' wording: {g}"
        );
        assert!(g.trim_start().starts_with("SELECT "), "not a SELECT: {g}");
        assert!(!g.contains(';'), "guard must not include ';' itself: {g}");
    }

    #[test]
    fn managed_catalog_guard_detects_cross_catalog_via_duckdb_tables() {
        // FF-3: the single-catalog guard must span catalogs (duckdb_tables, not
        // information_schema), fire only when the catalog lives in ANOTHER
        // database than the current one, and carry an actionable single-catalog
        // message that names the current database.
        let g = managed_catalog_guard_select();
        assert!(
            g.contains("FROM duckdb_tables()"),
            "must use duckdb_tables() (spans catalogs), not information_schema: {g}"
        );
        assert!(
            g.contains("database_name <> current_database()")
                && g.contains("database_name = current_database()"),
            "must compare the catalog's database against the current one: {g}"
        );
        // `duckdb_tables()` exposes `schema_name`, not `table_schema`.
        assert!(
            g.contains("schema_name = 'semantic_layer'")
                && g.contains("table_name = '_definitions'"),
            "must match the semantic_layer._definitions catalog table: {g}"
        );
        assert!(
            g.contains("single-catalog") && g.contains("|| current_database() ||"),
            "message must name the current database and state the single-catalog rule: {g}"
        );
        assert!(g.trim_start().starts_with("SELECT "), "not a SELECT: {g}");
        assert!(!g.contains(';'), "guard must not include ';' itself: {g}");
    }

    // --- schema scoping (TECH-DEBT #25) ---------------------------------

    #[test]
    fn create_target_schema_resolves_current_schema_when_unqualified() {
        let e = create_target_schema_expr(&SchemaTarget::Unqualified);
        assert!(
            e.contains("current_schema()"),
            "unqualified CREATE targets the current schema: {e}"
        );
        // The catalog's spelling, not current_schema()'s echo of the last USE
        // — `USE \"MYSCHEMA\"` reports MYSCHEMA for a schema named MySchema,
        // and (schema_name, name) is a primary key.
        assert!(
            e.contains("FROM duckdb_schemas()") && e.contains("s.schema_name"),
            "must resolve to the catalog's canonical spelling: {e}"
        );
        assert!(
            e.contains("s.database_name = current_database()"),
            "must stay within the single semantic-view catalog: {e}"
        );
    }

    #[test]
    fn create_target_schema_errors_on_a_schema_that_does_not_exist() {
        let e = create_target_schema_expr(&SchemaTarget::Named(SqlLit::escape("nosuch")));
        assert!(
            e.contains("lower('nosuch')"),
            "must look up the named schema: {e}"
        );
        assert!(
            e.contains("does not exist") && e.contains("error("),
            "a missing schema must fail, not silently fall back: {e}"
        );
    }

    #[test]
    fn create_target_schema_escapes_quotes_in_the_schema_name() {
        let e = create_target_schema_expr(&SchemaTarget::Named(SqlLit::escape("O'Brien")));
        assert!(
            e.contains("lower('O''Brien')"),
            "embedded quote must stay doubled: {e}"
        );
    }

    #[test]
    fn resolved_schema_of_a_qualified_reference_is_the_qualifier() {
        let e = resolved_schema_expr(
            &SchemaTarget::Named(SqlLit::escape("analytics")),
            &SqlLit::escape("v"),
            &SqlLit::escape("v"),
        );
        assert_eq!(e, "'analytics'");
        // A qualifier answers the question outright — no catalog probe, so a
        // qualified DROP of a name that also exists elsewhere cannot be
        // reported ambiguous.
        assert!(
            !e.contains("ambiguous"),
            "a qualified reference is never ambiguous: {e}"
        );
    }

    // The three tests below replace a single earlier one that pinned the
    // interim "two schemas hold this name -> error" rule. That rule is gone,
    // so its assertions could not be carried forward verbatim; each branch it
    // covered is now pinned by one of these, and the off-path and
    // unique-match branches it did not distinguish are pinned separately.

    #[test]
    fn resolved_schema_of_an_unqualified_reference_follows_the_search_path() {
        let e = unqualified("v");
        // The same path the read side reads off the caller's context, so
        // `DROP SEMANTIC VIEW v` and `semantic_view('v')` cannot disagree
        // about which `v` they mean.
        assert!(
            e.contains(SEARCH_PATH_SQL),
            "must resolve through the caller's search path: {e}"
        );
        // Earliest schema on the path wins. `min(schema_name)` would pick
        // alphabetically, which is a different view whenever the path order
        // and the alphabet disagree.
        assert!(
            e.contains("list_position") && e.contains("arg_min(schema_name"),
            "must pick the earliest path position, not the alphabetical first: {e}"
        );
        assert!(
            !e.contains("is ambiguous"),
            "the path decides; a second candidate is no longer an error: {e}"
        );
        // The wording carries no `;` on purpose: guards are joined into a
        // multi-statement string, and every guard test asserts it contributes
        // exactly one statement.
        assert!(!e.contains(';'), "resolution must not embed a ';': {e}");
    }

    #[test]
    fn resolved_schema_reports_a_name_that_is_off_the_path() {
        let e = unqualified("v");
        assert!(
            e.contains("does not exist on the search path"),
            "an off-path name must say so rather than resolving: {e}"
        );
        assert!(
            e.contains("string_agg(schema_name, ', ' ORDER BY schema_name)"),
            "the error must name the schemas it DOES live in, in a stable order: {e}"
        );
        assert!(
            e.contains("Qualify the reference as <schema>.v"),
            "the error must say how to reach it: {e}"
        );
        // Same wording as the read side's `resolve_in_search_path`, so the two
        // paths cannot drift into describing the same situation differently.
        assert!(
            e.contains("or add the schema to search_path"),
            "the error must offer the other remedy too: {e}"
        );
        // The path is shown as the caller spelled it. Folding is a matching
        // concern: reporting `(analytics)` to someone who wrote
        // `SET search_path = 'Analytics'` describes a path they did not
        // configure, and disagrees with the read side, which prints its input
        // verbatim. (Raised by review on PR #187.)
        assert!(
            e.contains(&format!("array_to_string({}, ', ')", SEARCH_PATH_SQL)),
            "the message must render the unfolded path: {e}"
        );
    }

    #[test]
    fn a_single_candidate_resolves_without_consulting_the_path() {
        // Mirrors `resolve_in_search_path`, where one row short-circuits ahead
        // of the path check: a view that is the only one of its name stays
        // reachable by that bare name even from a schema that is off-path.
        // Zero rows fall into the same branch and yield NULL, so the caller's
        // existence guard reports the canonical "does not exist".
        let e = unqualified("v");
        assert!(
            e.contains("WHEN count(*) <= 1 THEN min(schema_name)"),
            "the unique match must resolve on its own: {e}"
        );
    }

    #[test]
    fn row_predicate_matches_the_schema_case_insensitively() {
        let p = row_predicate(&SqlLit::escape("v"), "'Analytics'");
        assert_eq!(p, "name = 'v' AND lower(schema_name) = lower('Analytics')");
    }

    #[test]
    fn off_path_suggestion_quotes_a_name_that_needs_it() {
        // The suggestion is meant to be copy-pasted. For a view named `my view`
        // an unquoted `<schema>.my view` is a syntax error; for `a.b` it is
        // worse — it parses as schema `a`, view `b`, a different reference
        // entirely. (Raised by review on PR #186.)
        let spaced = unqualified("my view");
        assert!(
            spaced.contains(r#"Qualify the reference as <schema>."my view""#),
            "a name needing quotes must be suggested quoted: {spaced}"
        );
        let dotted = unqualified("a.b");
        assert!(
            dotted.contains(r#"Qualify the reference as <schema>."a.b""#),
            "a dotted name must be suggested quoted, or it reads as a qualifier: {dotted}"
        );
        // ...and a bare-safe name stays unquoted, so the common message is not
        // made noisier for everyone.
        let plain = unqualified("sales");
        assert!(
            plain.contains("Qualify the reference as <schema>.sales"),
            "a bare-safe name must not gain quotes: {plain}"
        );
        // Quoting belongs to the MESSAGE only — the lookup still matches on the
        // raw stored name.
        assert!(
            spaced.contains("WHERE name = 'my view'"),
            "lookup must use the unquoted stored name: {spaced}"
        );
    }

    #[test]
    fn current_database_guard_compares_against_the_live_current_database() {
        let g = current_database_guard_select(
            &SqlLit::escape("otherdb"),
            &SqlLit::escape("otherdb.analytics.v"),
        );
        // A call, not a literal: the caller's database is only known at
        // execution, on the caller's connection.
        assert!(
            g.contains("lower('otherdb') <> lower(current_database())"),
            "must compare the written database against the live one: {g}"
        );
        assert!(
            g.contains("|| current_database() ||"),
            "the message must name the database the caller is actually in: {g}"
        );
        assert!(
            g.contains("''otherdb.analytics.v'' names database ''otherdb''"),
            "the message must quote the reference as written: {g}"
        );
        assert!(
            g.contains("single-catalog"),
            "the message must state the rule being enforced: {g}"
        );
        assert!(g.trim_start().starts_with("SELECT "), "not a SELECT: {g}");
        assert!(!g.contains(';'), "guard must not include ';' itself: {g}");
    }

    #[test]
    fn current_database_guard_escapes_quotes_in_both_slots() {
        // The database name and the display form are embedded in one SQL
        // string literal each; an embedded `'` must stay doubled or the
        // emitted guard is a syntax error rather than a check.
        let g = current_database_guard_select(&SqlLit::escape("O'Db"), &SqlLit::escape("O'Db.s.v"));
        assert!(g.contains("lower('O''Db')"), "database slot wrong: {g}");
        assert!(g.contains("''O''Db.s.v''"), "display slot wrong: {g}");
    }
}
