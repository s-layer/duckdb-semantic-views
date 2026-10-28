//! CREATE-time enrichment shared by the CREATE forms.
//!
//! Pre-v0.8.0 this module also hosted `DefineFromJsonVTab` — a table function
//! that the legacy `parse_function` fallback rewrote DDL into. v0.8.0's full
//! unification deleted that path; CREATE now lowers to a native INSERT against
//! `semantic_layer._definitions` directly. Only the enrichment helper remains —
//! called by `crate::parse::native_sql` (the inline body) and
//! `crate::ddl::alter_helpers_ffi` (`FROM YAML FILE`).
//!
//! # Phase 65 (v0.10.0) — read-elimination architecture
//!
//! - **D-05**: removed `resolve_pk_from_catalog`. Auto-fallback to
//!   `duckdb_constraints()` for tables without explicit PRIMARY KEY in the
//!   TABLES clause is gone. Snowflake-aligned: PKs in semantic views are
//!   LOGICAL user assertions, not physical-catalog imports.
//! - **D-06**: when a FK references a table without a PRIMARY KEY (or
//!   UNIQUE) declared in the TABLES clause, step 2 below emits an
//!   actionable hard error pointing the user at the missing declaration.
//! - **D-16 / D-17 / metadata-via-SQL**: removed all `conn`-consuming
//!   work. CREATE-time `now()` / `current_database()` / `current_schema()`
//!   capture moves to the caller's connection via `json_merge_patch`
//!   embedded in the rewritten INSERT (see `parse::emit_native_create_sql`).
//!   CREATE-time `LIMIT 0` column type inference and per-fact
//!   `typeof(expr)` inference also go — they get deferred to read-side
//!   bind callbacks under Plan 05's C++ Catalog API migration. The
//!   function signature is now `(name, def) -> Result<String, String>` —
//!   no `conn` argument, no `infer_types` flag.

/// Run all CREATE-time validation + cardinality inference and return the
/// serialized JSON string ready for storage in `_definitions`.
///
/// Steps performed in order:
/// 1. Re-run cardinality inference (catches FK→PK mismatches once PKs are
///    declared explicitly in the TABLES clause).
/// 2. Catch joins whose FK target has no `pk_columns` (and no
///    `unique_constraints`) declared in the TABLES clause — D-06 hard
///    error path.
/// 3. Run graph / facts / derived-metric / using-relationship validations.
/// 4. Serialize the validated definition to JSON.
///
/// Metadata (`created_on`, `database_name`, `schema_name`) is NOT
/// populated here — the rewritten INSERT in `emit_native_create_sql`
/// wraps the serialized JSON in a `json_merge_patch(..., json_object(
/// 'created_on', strftime(now(), '%Y-%m-%dT%H:%M:%SZ'), 'database_name',
/// current_database(), 'schema_name', current_schema()))` so `DuckDB`
/// resolves the values on the caller's connection at INSERT-time. This
/// makes CREATE SEMANTIC VIEW participate in the caller's transaction
/// without the extension needing a long-lived catalog connection
/// (D-21 + read-elimination architecture).
///
/// Column type inference (`column_type_names`, `column_types_inferred`,
/// dim/metric `output_type`, fact `output_type`) is deferred to
/// read-side bind callbacks per D-16 / D-17 — they stay empty / None
/// in the persisted JSON.
///
/// Called for the inline body (`parse::native_sql`) and for `FROM YAML FILE`
/// (`ddl::alter_helpers_ffi`, from the helper table function's bind).
pub fn enrich_definition_for_create(
    _name: &str,
    mut def: crate::model::SemanticViewDefinition,
) -> Result<String, crate::errors::ParseError> {
    // 1. Re-run cardinality inference. Phase 65: no longer preceded by
    //    `resolve_pk_from_catalog` (D-05). Tables without explicit PRIMARY
    //    KEY in the TABLES clause that are FK-referenced by another table
    //    surface as the D-06 hard error in step 2.
    crate::graph::infer_cardinality(&def.tables, &mut def.joins)?;

    // 2. Catch joins that reference a target without a PRIMARY KEY (or
    //    UNIQUE constraint) declared in the TABLES clause.
    //    Phase 65 (D-06): hard-error path. v0.9.0's resolve_pk_from_catalog
    //    auto-fallback to duckdb_constraints() is gone; the error is
    //    actionable and tells the user exactly what to add.
    //
    //    Two sub-cases:
    //      (a) `REFERENCES target` (no col list) — `infer_cardinality` left
    //          `ref_columns` empty because target has no `pk_columns`.
    //      (b) `REFERENCES target(cols)` — `ref_columns` was set explicitly
    //          but target has no `pk_columns` and no UNIQUE constraint
    //          matching `ref_columns`. (Without the D-06 wrapping this
    //          would surface as the more generic CARD-03 "FK ... does not
    //          match any PRIMARY KEY or UNIQUE constraint" error in
    //          `validate_fk_references`. The D-06 message is more
    //          actionable because it names the fix verbatim.)
    for join in &def.joins {
        if join.fk_columns.is_empty() {
            continue;
        }
        let to_alias_lower = join.table.to_ascii_lowercase();
        let fk_source = join.from_alias.as_str();
        let target = def
            .tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == to_alias_lower);
        let Some(t) = target else {
            // Target alias unresolved — let graph validation surface that
            // with its dedicated message (more specific than D-06).
            continue;
        };

        let target_has_pk = !t.pk_columns.is_empty();
        let target_has_any_unique = !t.unique_constraints.is_empty();
        if target_has_pk || target_has_any_unique {
            // Target has some declared key — either ref_columns matches
            // (handled in step 3 below by graph::validate_graph), or it
            // doesn't (CARD-03 surfaces a column-mismatch error, which is
            // the right shape for that failure mode).
            continue;
        }

        // Target has NEITHER pk_columns NOR any unique_constraints
        // declared in the TABLES clause. This is unambiguously the D-06
        // case regardless of whether ref_columns is empty (implicit
        // REFERENCES) or set (explicit REFERENCES with cols).
        return Err(crate::errors::ParseError::positionless(format!(
            "Table '{target}' has no PRIMARY KEY declared but is \
             referenced by FK in '{fk_source}'. Add PRIMARY KEY \
             (cols) or UNIQUE (cols) to the TABLES clause for \
             {target}. (v0.10.0: physical-catalog PK auto-inference \
             removed -- see CHANGELOG.)",
            target = t.alias,
            fk_source = fk_source,
        )));
    }

    // 3. Graph validations. Name uniqueness runs first (SG-13): dimensions,
    //    metrics, and facts share one request namespace at query time, so
    //    collisions -- within a kind or across kinds, case-insensitive --
    //    are rejected at define time. Read paths keep first-match behavior
    //    for legacy catalog rows that predate this check.
    crate::graph::validate_name_uniqueness(&def)?;
    crate::graph::validate_graph(&def)?;
    crate::graph::validate_facts(&def)?;
    crate::graph::validate_derived_metrics(&def)?;
    crate::graph::validate_using_relationships(&def)?;
    // Phase 6.3: a metric over a same-table metric names a plain one, acyclically.
    crate::graph::validate_same_table_metric_refs(&def)?;
    // TECH-DEBT #52 (PAR-3): last of the graph checks, because it needs the
    // fact and metric name sets the earlier ones validated -- a member
    // expression may legitimately reference those across tables, and only a
    // RAW column of another table is out of scope.
    crate::graph::validate_member_references(&def)?;
    // TECH-DEBT #95: no aggregate in a dimension, no parameter in any member.
    crate::graph::validate_member_expressions(&def)?;

    // 4. Serialize. Metadata (created_on, database_name, schema_name) is
    //    populated by SQL inside the rewritten INSERT — not here. Column
    //    type inference is deferred to read-side bind (Plan 05).
    //    `temp_tables` goes out holding every name written without a schema;
    //    the INSERT keeps only those that are temporary tables in the
    //    creating session (`native_sql::temp_tables_expr`).
    def.temp_tables = crate::expand::temp_table_candidates(&def);
    serde_json::to_string(&def).map_err(|e| crate::errors::ParseError::positionless(e.to_string()))
}
