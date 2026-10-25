//! Persistent catalog for semantic view definitions stored in
//! `semantic_layer._definitions`.
//!
//! Prior to v0.8.0 this module also maintained an in-memory `HashMap` mirror
//! that DDL writes updated alongside the catalog table. v0.8.0 removed the
//! mirror so catalog reads always see the same state `DuckDB` does — which is
//! a prerequisite for transactional DDL: the grammar productions emit
//! `INSERT/DELETE/UPDATE` against `_definitions` directly on the caller's
//! connection, and any cached mirror would diverge across rollback.

use std::collections::HashMap;
use std::path::PathBuf;

use duckdb::Connection;

/// Write-side SQL builders (existence/collision guards) for `_definitions`.
pub(crate) mod writes;

// Extension appended to the DuckDB file path to form the v0.1.0 companion file.
// Used only in the one-time migration below. After the migration runs, the
// companion file is deleted and this constant is never referenced again at runtime.
const V010_COMPANION_EXT: &str = "semantic_views";

/// Path of the v0.1.0 companion file that would sit alongside `db_path`, or
/// `None` for an in-memory database (which never has one).
///
/// The one place this shape is computed. Both readers of it — the writable
/// import pass in [`init_catalog`] and the read-only refusal above it (CAT-8) —
/// must agree byte-for-byte, or the read-only branch would refuse for a file
/// the import pass would not have looked at (or, worse, stay silent for one it
/// would).
fn v010_companion_path(db_path: &str) -> Option<PathBuf> {
    if db_path == ":memory:" {
        return None;
    }
    let mut p = PathBuf::from(db_path);
    let ext = match p.extension() {
        Some(e) => format!("{}.{V010_COMPANION_EXT}", e.to_string_lossy()),
        None => V010_COMPANION_EXT.to_string(),
    };
    p.set_extension(ext);
    Some(p)
}

/// A unique temp-file path for a file-backed unit test (CAT-7).
///
/// File-backed tests used to hardcode a fixed name under `std::env::temp_dir()`.
/// `cargo test` runs several test binaries **concurrently**, and this crate's
/// tests are also run concurrently with other checkouts on a shared machine, so
/// two processes could open the same DuckDB file — one taking a conflicting
/// lock, and each `remove_file`-ing the other's live database on the way in.
/// Observed live during the 2026-08-08 review: four tests failed on a
/// conflicting-lock error and passed in isolation.
///
/// Uniqueness comes from the process id plus a nanosecond timestamp, the
/// pattern `tc6_restart_persistence_survives_reopen` already used. `stem` keeps
/// the failing file identifiable in a `ls /tmp` after a crash, and callers that
/// need companion/WAL siblings derive them from the returned path so all of a
/// test's files share the unique stem.
#[cfg(test)]
pub(crate) fn unique_temp_db_path(stem: &str) -> PathBuf {
    // Three components, because two of them are individually insufficient:
    //
    // - the PID separates concurrent `cargo test` PROCESSES (the CAT-7
    //   collision this helper exists to fix — a sibling run took a conflicting
    //   DuckDB lock, and each test's up-front `remove_file` could delete the
    //   other run's live database);
    // - the counter separates callers WITHIN a process, which the PID cannot:
    //   Rust runs tests on concurrent threads, so every caller here shares a
    //   PID, and `SystemTime` resolution is coarser than nanoseconds on some
    //   platforms — two threads in one tick would otherwise get one path, which
    //   is CAT-7 again inside a single run;
    // - the timestamp keeps names distinct across sequential runs of the same
    //   process image, so a crashed run's leftover file cannot be mistaken for
    //   the current one.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    std::env::temp_dir().join(format!(
        "{stem}_{}_{nanos}_{seq}.duckdb",
        std::process::id()
    ))
}

/// Schema holding the semantic-view catalog table.
pub const DEFINITIONS_SCHEMA: &str = "semantic_layer";
/// Bare (unqualified) name of the semantic-view catalog table.
pub const DEFINITIONS_TABLE_NAME: &str = "_definitions";
/// Fully-qualified catalog table where all semantic-view definitions are stored.
///
/// This is the single source of truth for the table reference; every SQL builder
/// that reads or writes definitions embeds this constant rather than the literal
/// string. The `DEFINITIONS_TABLE == DEFINITIONS_SCHEMA.DEFINITIONS_TABLE_NAME`
/// relationship is asserted by `tests::definitions_table_const_is_consistent`.
pub const DEFINITIONS_TABLE: &str = "semantic_layer._definitions";

/// Canonical "view does not exist" error wording, shared by every read-side DDL
/// command so the message stays identical across the surface. The SQL-side guard
/// selects in the sibling [`writes`] module intentionally inline an escaped copy
/// of this wording.
#[must_use]
pub fn view_not_found_msg(name: &str) -> String {
    format!("semantic view '{name}' does not exist")
}

/// Create the `semantic_layer` schema and `_definitions` table if they do not
/// exist, and run the v0.1.0 companion-file migration once for file-backed
/// databases.
///
/// Idempotent: safe to call on every extension load.
///
/// Phase 63 (v0.9.0): when `is_read_only=true`, skips the entire body —
/// the host DB is read-only so neither the schema/table CREATE nor the
/// companion-file migration (which INSERTs into the table) can run.
/// Reader-path code in `CatalogReader` short-circuits separately when
/// the table is genuinely absent. See 63-RESEARCH.md §3 Q2.
pub fn init_catalog(
    con: &Connection,
    db_path: &str,
    is_read_only: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if is_read_only {
        // A read-only host cannot be migrated in place. Serving it through a
        // legacy read path instead would mean carrying two storage shapes
        // indefinitely, so refuse with something the operator can act on.
        if definitions_is_legacy_shape(con)? {
            return Err(
                "semantic_views: this database's catalog predates schema-scoped \
                 semantic views and the database is open read-only, so it cannot \
                 be migrated. Open it writable once (any LOAD will migrate it), \
                 then reopen read-only."
                    .into(),
            );
        }
        // CAT-8: the v0.1.0 companion-file import below is equally unreachable
        // read-only — it INSERTs. Left unchecked the two un-migrated shapes
        // behaved differently: the legacy *table* refused loudly (above), while
        // a companion file was simply never looked at, so every view in it
        // resolved as "does not exist" with no hint that a whole catalog was
        // sitting unimported next to the database. Refuse with the same
        // actionable wording instead.
        if let Some(companion) = v010_companion_path(db_path) {
            if companion.exists() {
                return Err(format!(
                    "semantic_views: this database has a v0.1.0 companion file \
                     '{}' whose semantic views have not been imported yet, and \
                     the database is open read-only, so it cannot be migrated. \
                     Open it writable once (any LOAD will import it), then \
                     reopen read-only.",
                    companion.display()
                )
                .into());
            }
        }
        return Ok(());
    }
    // FF-10: `definition` is `NOT NULL`. A SQL-NULL definition is an
    // unrecoverable-looking state — readers treat a NULL definition as
    // "view does not exist" while the write-side existence guards see the row
    // as present, so a manually-tampered NULL row can neither be read nor
    // re-created. The constraint makes that state unrepresentable for new
    // catalogs (all writes always supply a definition).
    //
    // The key is `(schema_name, name)`: semantic views are scoped to a schema,
    // so two schemas may each hold a `v`. `schema_name` is a column rather than
    // a field inside `definition` precisely so the database can enforce that —
    // buried in the JSON it was unenforceable, and the single-column key made
    // `CREATE SEMANTIC VIEW other.v` collide with an existing `analytics.v`.
    con.execute_batch(&format!(
        "CREATE SCHEMA IF NOT EXISTS {DEFINITIONS_SCHEMA};
         CREATE TABLE IF NOT EXISTS {DEFINITIONS_TABLE} (
             schema_name VARCHAR NOT NULL,
             name        VARCHAR NOT NULL,
             definition  VARCHAR NOT NULL,
             PRIMARY KEY (schema_name, name)
         );"
    ))?;

    // Rebuild a pre-scoping catalog. Runs before the companion import and the
    // AR-4 version pass so both see the current shape.
    migrate_definitions_to_schema_scoped(con)?;

    // One-time migration: if a v0.1.0 companion file exists alongside the database,
    // import its contents into the table then delete the file.
    if let Some(migration_path) = v010_companion_path(db_path) {
        if migration_path.exists() {
            let contents = std::fs::read_to_string(&migration_path).map_err(|e| {
                format!(
                    "semantic_views: cannot read v0.1.0 companion file '{}': {e}. \
                     The file was left in place — fix its permissions (or move it \
                     away to skip migration) and re-LOAD.",
                    migration_path.display()
                )
            })?;
            let migrated: std::collections::HashMap<String, String> =
                serde_json::from_str(&contents).map_err(|e| {
                    format!(
                        "semantic_views: v0.1.0 companion file '{}' is not valid JSON: {e}. \
                         The file was left in place so its definitions are not lost — \
                         repair it (or move it away to skip migration) and re-LOAD.",
                        migration_path.display()
                    )
                })?;
            for (name, def) in &migrated {
                // v0.1.0 companion files predate schemas entirely — there is
                // no schema to recover from them, so imported rows are filed
                // where they were in fact created.
                //
                // CAT-3: backfilled into the JSON as well, exactly as
                // `migrated_row` does for the legacy-table rebuild. The import
                // is the fourth writer of a definition, and the column and the
                // `schema_name` inside the JSON are documented as being in
                // lockstep — the SHOW / DESCRIBE listings read the JSON, so a
                // row written to the column alone lists with an empty schema
                // and is missed by `SHOW SEMANTIC VIEWS IN SCHEMA main`. A
                // definition this cannot edit is still carried across verbatim
                // rather than dropped.
                let aligned = backfill_schema_name(def, UNRECORDED_SCHEMA_FALLBACK)
                    .unwrap_or_else(|| def.clone());
                con.execute(
                    &format!(
                        "INSERT OR REPLACE INTO {DEFINITIONS_TABLE} \
                         (schema_name, name, definition) VALUES (?, ?, ?)"
                    ),
                    duckdb::params![UNRECORDED_SCHEMA_FALLBACK, name, aligned],
                )?;
            }
            // Delete ONLY after a fully successful import. Pre-fix the file was
            // removed even when unreadable or corrupt, permanently destroying
            // the user's pre-v0.2 definitions. A failed delete must also be an
            // error: if the file survives, every subsequent LOAD re-imports
            // this (now stale) snapshot over newer definitions via
            // INSERT OR REPLACE.
            std::fs::remove_file(&migration_path).map_err(|e| {
                format!(
                    "semantic_views: imported v0.1.0 companion file '{}' but could \
                     not delete it: {e}. Delete it manually before the next LOAD to \
                     avoid re-importing stale definitions.",
                    migration_path.display()
                )
            })?;
        }
    }

    // AR-4: one-time storage-format upgrade pass. Runs after the v0.1.0
    // companion import so freshly-imported rows are considered too. Only on
    // writable DBs (guarded by the is_read_only early-return above).
    upgrade_definitions_schema(con)?;

    Ok(())
}

/// Schema a stored definition is filed under when it records none of its own.
///
/// Rows written before the metadata stamp existed have no `schema_name` at all.
/// They cannot become NULL or empty — the column is half the primary key — so
/// they land in `main`, which is where they were in fact created.
const UNRECORDED_SCHEMA_FALLBACK: &str = "main";

/// True when `_definitions` exists but predates schema scoping.
///
/// "Missing table" and "legacy table" must not be confused: a database that has
/// never held a semantic view has no table, and that is not something to
/// migrate or refuse.
fn definitions_is_legacy_shape(con: &Connection) -> Result<bool, Box<dyn std::error::Error>> {
    let table_exists: i64 = con.query_row(
        "SELECT count(*) FROM information_schema.tables \
         WHERE table_schema = ? AND table_name = ?",
        duckdb::params![DEFINITIONS_SCHEMA, DEFINITIONS_TABLE_NAME],
        |r| r.get(0),
    )?;
    if table_exists == 0 {
        return Ok(false);
    }
    let has_schema_column: i64 = con.query_row(
        "SELECT count(*) FROM information_schema.columns \
         WHERE table_schema = ? AND table_name = ? AND column_name = 'schema_name'",
        duckdb::params![DEFINITIONS_SCHEMA, DEFINITIONS_TABLE_NAME],
        |r| r.get(0),
    )?;
    Ok(has_schema_column == 0)
}

/// The schema a legacy row should migrate into, and the definition to store
/// with it.
///
/// Rows written before the metadata stamp existed record no schema at all.
/// They land in [`UNRECORDED_SCHEMA_FALLBACK`] — and the definition is
/// **backfilled** with the same value, so the new `schema_name` column and the
/// `schema_name` inside the JSON (which the SHOW / DESCRIBE listings read)
/// cannot disagree about where the view lives. Every other write keeps the two
/// in lockstep the same way: CREATE stamps both from one expression, and a
/// schema-moving ALTER RENAME patches both.
///
/// Deliberately a minimal probe rather than a full `SemanticViewDefinition`
/// parse: a row this migration cannot fully understand must still be carried
/// across, not dropped. Losing a definition here is unrecoverable — an
/// unparseable row keeps its bytes verbatim and is filed under the fallback.
fn migrated_row(json: &str, catalog_schemas: &HashMap<String, String>) -> (String, String) {
    #[derive(serde::Deserialize)]
    struct Probe {
        #[serde(default)]
        schema_name: Option<String>,
    }
    let recorded = serde_json::from_str::<Probe>(json)
        .ok()
        .and_then(|p| p.schema_name)
        .filter(|s| !s.is_empty());
    if let Some(schema) = recorded {
        // CAT-1: store the CATALOG's spelling, not the row's. `current_schema()`
        // echoes whatever spelling was last written in `USE`, so a row created
        // after `USE "ANALYTICS"` records `ANALYTICS` for a schema the catalog
        // calls `analytics`. Migrated verbatim it keys under a spelling the
        // catalog never had — and because `INSERT OR REPLACE` conflicts on the
        // byte-equal `(schema_name, name)` key while every read guard folds
        // case, a later `CREATE OR REPLACE analytics.v` inserts a SECOND row
        // rather than replacing it, after which reads resolve to whichever
        // sorts first (the stale one).
        //
        // A schema the catalog does not know keeps its recorded spelling: this
        // migration must carry every row across, and dropping or failing on one
        // is unrecoverable. Such a row is no worse off than before.
        let canonical = catalog_schemas
            .get(&schema.to_ascii_lowercase())
            .cloned()
            .unwrap_or(schema);
        // The column and the `schema_name` inside the JSON are documented as
        // being in lockstep — the SHOW / DESCRIBE listings read the JSON — so
        // rewriting one without the other would split them.
        let aligned = backfill_schema_name(json, &canonical).unwrap_or_else(|| json.to_string());
        return (canonical, aligned);
    }
    let fallback = UNRECORDED_SCHEMA_FALLBACK.to_string();
    let backfilled = backfill_schema_name(json, &fallback).unwrap_or_else(|| json.to_string());
    (fallback, backfilled)
}

/// Lowercased schema name -> the catalog's own spelling, for the current
/// database. `DuckDB` schema names are unique case-insensitively, so this is a
/// well-defined map. Mirrors the `duckdb_schemas()` lookup
/// [`crate::catalog::writes::create_target_schema_expr`] performs at CREATE
/// time, so a migrated row and a freshly created one key identically.
fn catalog_schema_spellings(
    con: &Connection,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let mut stmt = con.prepare(
        "SELECT s.schema_name FROM duckdb_schemas() s \
         WHERE s.database_name = current_database()",
    )?;
    let mapped = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut out = HashMap::new();
    for schema in mapped {
        let schema = schema?;
        out.insert(schema.to_ascii_lowercase(), schema);
    }
    Ok(out)
}

/// Insert `schema_name` into a stored definition object, returning `None` when
/// the row is not a JSON object this can safely edit (it is then carried across
/// byte-for-byte rather than being rewritten or dropped).
fn backfill_schema_name(json: &str, schema: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(json).ok()?;
    value.as_object_mut()?.insert(
        "schema_name".to_string(),
        serde_json::Value::String(schema.to_string()),
    );
    serde_json::to_string(&value).ok()
}

/// Rebuild a pre-scoping `_definitions` into the `(schema_name, name)` shape.
///
/// A rebuild rather than an `ALTER`: `DuckDB` cannot retrofit a primary key onto
/// an existing table. It cannot collide — the old key made `name` globally
/// unique, so every `(schema_name, name)` derived from it is unique too.
///
/// Idempotent, because `init_catalog` runs on every LOAD: a catalog already in
/// the new shape returns immediately.
fn migrate_definitions_to_schema_scoped(
    con: &Connection,
) -> Result<(), Box<dyn std::error::Error>> {
    if !definitions_is_legacy_shape(con)? {
        return Ok(());
    }

    // The schema is derived in Rust, not by a SQL `json_extract`: bootstrap
    // must not depend on the JSON extension being loadable, and this mirrors
    // how `upgrade_definitions_schema` below already reads rows out.
    let rows: Vec<(String, String)> = {
        let mut stmt = con.prepare(&format!("SELECT name, definition FROM {DEFINITIONS_TABLE}"))?;
        let mapped =
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };

    // Read once, outside the swap closure: the map is the same for every row,
    // and the swap runs inside a transaction that drops the old table.
    let catalog_schemas = catalog_schema_spellings(con)?;

    let staging = format!("{DEFINITIONS_SCHEMA}.{DEFINITIONS_TABLE_NAME}__scoped");
    let swap = |con: &Connection| -> Result<(), Box<dyn std::error::Error>> {
        con.execute_batch(&format!(
            "DROP TABLE IF EXISTS {staging};
             CREATE TABLE {staging} (
                 schema_name VARCHAR NOT NULL,
                 name        VARCHAR NOT NULL,
                 definition  VARCHAR NOT NULL,
                 PRIMARY KEY (schema_name, name)
             );"
        ))?;
        for (name, definition) in &rows {
            let (schema, definition) = migrated_row(definition, &catalog_schemas);
            con.execute(
                &format!("INSERT INTO {staging} (schema_name, name, definition) VALUES (?, ?, ?)"),
                duckdb::params![schema, name, definition],
            )?;
        }
        con.execute_batch(&format!(
            "DROP TABLE {DEFINITIONS_TABLE};
             ALTER TABLE {staging} RENAME TO {DEFINITIONS_TABLE_NAME};"
        ))?;
        Ok(())
    };

    // One transaction: a half-swapped catalog would be worse than an
    // unmigrated one, since the old table is dropped partway through.
    con.execute_batch("BEGIN;")?;
    match swap(con) {
        Ok(()) => {
            con.execute_batch("COMMIT;")?;
            Ok(())
        }
        Err(e) => {
            let _ = con.execute_batch("ROLLBACK;");
            Err(format!(
                "semantic_views: could not migrate the catalog to schema-scoped \
                 storage: {e}. The existing catalog was left untouched."
            )
            .into())
        }
    }
}

/// One-time `schema_version` upgrade pass over `_definitions` (AR-4).
///
/// For every stored row still below [`crate::model::CURRENT_SCHEMA_VERSION`]:
/// stamp it to the current version **iff** it can be positively verified as
/// current-format (parses cleanly and every relationship carries `fk_columns`).
/// Rows that fail to parse, or whose relationships lack FK metadata (legacy
/// pre-Phase-24 encodings), are left untouched at version 0 — the fan-trap
/// safety check (`expand::fan_trap`) then hard-errors on read rather than
/// silently trusting them. Non-destructive: no row is deleted or rewritten
/// beyond the version stamp.
///
/// Idempotent: rows already at the current version are skipped, so subsequent
/// loads are no-ops. The `schema_version` integer is inlined from a
/// compile-time constant (no user input), so the `json_object` embed is safe.
fn upgrade_definitions_schema(con: &Connection) -> Result<(), Box<dyn std::error::Error>> {
    // CAT-2: `schema_name` is selected because it is half the primary key. The
    // stamp below must target the row that was actually verified — keying on
    // `name` alone stamped every namesake across schemas, including rows this
    // pass deliberately left at version 0.
    let rows: Vec<(String, String, String)> = {
        let mut stmt = con.prepare(&format!(
            "SELECT schema_name, name, definition FROM {DEFINITIONS_TABLE}"
        ))?;
        let mapped = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };

    for (schema_name, name, json) in rows {
        if crate::model::SemanticViewDefinition::stored_schema_version(&json)
            >= crate::model::CURRENT_SCHEMA_VERSION
        {
            continue; // already current
        }
        // Only stamp rows we can positively verify as current-format. A parse
        // failure or missing FK metadata => un-upgradeable legacy row; leave it
        // at version 0 so reads hard-error rather than silently under-checking.
        let upgradeable = crate::model::SemanticViewDefinition::from_json(&name, &json)
            .is_ok_and(|def| !def.has_incomplete_relationships());
        if !upgradeable {
            continue;
        }
        con.execute(
            &format!(
                "UPDATE {DEFINITIONS_TABLE} \
                 SET definition = json_merge_patch(definition::JSON, \
                     json_object('schema_version', {version}))::VARCHAR \
                 WHERE schema_name = ? AND name = ?",
                version = crate::model::CURRENT_SCHEMA_VERSION
            ),
            duckdb::params![schema_name, name],
        )?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// CatalogReader — the catalog table as the caller's transaction sees it.
// ---------------------------------------------------------------------------

mod reader {
    use crate::ident::ViewRef;

    /// One row of `semantic_layer._definitions`; any cell may be NULL.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct CatalogRow {
        schema_name: Option<String>,
        name: Option<String>,
        definition: Option<String>,
    }

    /// `lower()` as `DuckDB` computes it: one codepoint at a time, each to its
    /// simple lowercase mapping. `str::to_lowercase` differs: it maps a
    /// word-final `Σ` to `ς`, and `İ` to two codepoints.
    fn lower_codepoints(s: &str) -> String {
        // Of the full mappings, only `İ`'s has more than one codepoint, and
        // its first is the simple mapping.
        s.chars()
            .map(|c| c.to_lowercase().next().unwrap_or(c))
            .collect()
    }

    /// `ORDER BY` on a nullable VARCHAR as `DuckDB` sorts it by default:
    /// byte-wise, NULLs last.
    fn nulls_last(value: Option<&String>) -> (bool, Option<&String>) {
        (value.is_none(), value)
    }

    /// Read-side handle for `semantic_layer._definitions`.
    ///
    /// Holds the table's rows as the caller's transaction sees them (TECH-DEBT
    /// #19): [`CatalogReader::for_caller`] scans it in the transaction of the
    /// statement whose bind or scalar is running, so a view created, dropped
    /// or renamed earlier in that transaction, and not yet committed, reads
    /// that way, while another session sees only committed state. The reader
    /// is built per read and dropped with it.
    ///
    /// The queries the reader used to run on its own connection are answered
    /// over the rows with the same rules: `name` matches byte for byte, a
    /// schema case-insensitively, and orders are `DuckDB`'s (byte-wise, NULLs
    /// last).
    #[derive(Debug, Clone)]
    pub struct CatalogReader {
        // Phase 63 (v0.9.0): false only on a read-only host DB whose
        // semantic_layer._definitions table was never created. Reads then
        // answer "empty" / "not found".
        catalog_table_present: bool,
        // The database the table was read from: the managed catalog, which a
        // CAT-4 foreign-database reference is compared against.
        current_database: String,
        rows: Vec<CatalogRow>,
    }

    impl CatalogReader {
        /// The catalog as the transaction of the read now running on this
        /// thread sees it.
        ///
        /// # Errors
        ///
        /// The scan's error (a damaged catalog table, say), never "no views":
        /// a missing table reads as an empty catalog (FF-9).
        #[cfg(feature = "extension")]
        pub fn for_caller() -> Result<Self, String> {
            let defs = crate::ddl::read_ffi::caller_definitions(None)?;
            Ok(Self::from_rows(defs.present, defs.database, defs.rows))
        }

        /// [`CatalogReader::lookup`] over the caller's catalog, reading only
        /// the rows named `view.name` (the lookup matches the name byte for
        /// byte, so it would discard the others). For a scalar that looks
        /// up one view per row, which would otherwise copy the whole
        /// catalog each time.
        ///
        /// # Errors
        ///
        /// As [`CatalogReader::for_caller`] and [`CatalogReader::lookup`].
        #[cfg(feature = "extension")]
        pub fn lookup_in_caller(
            view: &ViewRef,
            search_path: &[String],
        ) -> Result<Option<String>, String> {
            let defs = crate::ddl::read_ffi::caller_definitions(Some(&view.name))?;
            Self::from_rows(defs.present, defs.database, defs.rows).lookup(view, search_path)
        }

        /// A reader over the given `(schema_name, name, definition)` rows.
        #[must_use]
        pub fn from_rows(
            catalog_table_present: bool,
            current_database: impl Into<String>,
            rows: Vec<(Option<String>, Option<String>, Option<String>)>,
        ) -> Self {
            Self {
                catalog_table_present,
                current_database: current_database.into(),
                rows: rows
                    .into_iter()
                    .map(|(schema_name, name, definition)| CatalogRow {
                        schema_name,
                        name,
                        definition,
                    })
                    .collect(),
            }
        }

        /// Fetch the JSON definition for a single view.
        ///
        /// Returns `Ok(None)` when no row exists.
        ///
        /// Semantic views are schema-scoped, so `view.schema` decides which
        /// row is meant. A qualified reference matches that schema only. An
        /// unqualified one matches on name alone, and
        /// [`crate::catalog::resolve_in_search_path`] decides between the
        /// schemas holding a view of that name.
        ///
        /// Phase 63: when the catalog table is absent (read-only host DB
        /// without a bootstrapped `_definitions` table), answers `Ok(None)`.
        /// Callers see the existing "semantic view '<name>' does not exist"
        /// error path.
        pub fn lookup(
            &self,
            view: &ViewRef,
            search_path: &[String],
        ) -> Result<Option<String>, String> {
            // No stored name holds a NUL, and the "does not exist" error would
            // echo it back and be cut short there.
            if view.name.contains('\0') {
                return Err("view name contains null byte".to_string());
            }
            if view.schema.as_deref().is_some_and(|s| s.contains('\0')) {
                return Err("schema name contains null byte".to_string());
            }
            // CAT-4: a reference spelling out a foreign database names an
            // object this catalog cannot hold, so resolving it against the
            // managed catalog would answer a question that was not asked.
            // Checked here rather than at each bind site because every read
            // surface — semantic_view(), explain_semantic_view(), DESCRIBE,
            // SHOW SEMANTIC COLUMNS/ENTITIES, GET_DDL, READ_YAML — reaches the
            // catalog through this one function.
            //
            // Ahead of the `catalog_table_present` short-circuit: a wrong-object
            // reference is wrong whether or not this database has a catalog, and
            // reporting it as "does not exist" would suggest the qualifier was
            // honoured and the view simply absent.
            //
            // The comparison is against the managed catalog, not the caller's
            // current database, so a caller `USE`-d into an attached database
            // still sees `adb.main.v` as foreign: reads are pinned to the
            // primary regardless of `USE`.
            if view.database.is_some() {
                if let Some(err) =
                    crate::catalog::foreign_database_error(view, &self.current_database)
                {
                    return Err(err);
                }
            }
            if !self.catalog_table_present {
                return Ok(None);
            }
            // Folded on both sides: DuckDB matches identifiers
            // case-insensitively and a row may carry a different case than
            // the caller writes.
            let wanted_schema = view.schema.as_deref().map(lower_codepoints);
            let mut matches: Vec<&CatalogRow> = self
                .rows
                .iter()
                .filter(|row| row.name.as_deref() == Some(view.name.as_str()))
                .filter(|row| match &wanted_schema {
                    None => true,
                    Some(wanted) => row
                        .schema_name
                        .as_deref()
                        .is_some_and(|s| lower_codepoints(s) == *wanted),
                })
                .collect();
            matches.sort_by(|a, b| {
                nulls_last(a.schema_name.as_ref()).cmp(&nulls_last(b.schema_name.as_ref()))
            });
            // More than one candidate can only mean an unqualified reference
            // matched views in different schemas — the `(schema_name, name)`
            // primary key rules out duplicates within one.
            let candidates: Vec<(String, String)> = matches
                .into_iter()
                .filter_map(|row| {
                    let definition = row.definition.clone()?;
                    Some((row.schema_name.clone().unwrap_or_default(), definition))
                })
                .collect();
            crate::catalog::resolve_in_search_path(view, &candidates, search_path)
                .map(|opt| opt.map(ToString::to_string))
        }

        /// Whether a view with this reference exists.
        pub fn exists(&self, view: &ViewRef, search_path: &[String]) -> Result<bool, String> {
            Ok(self.lookup(view, search_path)?.is_some())
        }

        /// Return `(name, definition_json)` for every registered view, sorted
        /// by `(schema_name, name)`.
        ///
        /// The schema is not returned separately because it is also inside
        /// `definition_json`, and the two are kept in lockstep by construction:
        /// CREATE stamps both from one expression, the schema-scoping migration
        /// backfills the JSON from the column, and a schema-moving ALTER RENAME
        /// patches both. Callers can therefore read the schema off the parsed
        /// definition without a second source of truth.
        ///
        /// Phase 63: empty when the catalog table is absent.
        pub fn list_all(&self) -> Result<Vec<(String, String)>, String> {
            if !self.catalog_table_present {
                return Ok(Vec::new());
            }
            let mut rows: Vec<&CatalogRow> = self.rows.iter().collect();
            rows.sort_by(|a, b| {
                nulls_last(a.schema_name.as_ref())
                    .cmp(&nulls_last(b.schema_name.as_ref()))
                    .then_with(|| nulls_last(a.name.as_ref()).cmp(&nulls_last(b.name.as_ref())))
            });
            Ok(rows
                .into_iter()
                .map(|row| {
                    (
                        row.name.clone().unwrap_or_default(),
                        row.definition.clone().unwrap_or_default(),
                    )
                })
                .collect())
        }

        /// Return just the distinct view names, sorted. Used by error-path
        /// suggestion helpers ("did you mean ...?").
        ///
        /// Phase 63: empty when the catalog table is absent.
        pub fn list_names(&self) -> Result<Vec<String>, String> {
            if !self.catalog_table_present {
                return Ok(Vec::new());
            }
            let mut names: Vec<Option<&String>> =
                self.rows.iter().map(|row| row.name.as_ref()).collect();
            names.sort_by(|a, b| nulls_last(*a).cmp(&nulls_last(*b)));
            names.dedup();
            Ok(names
                .into_iter()
                .map(|name| name.cloned().unwrap_or_default())
                .collect())
        }
    }
}

pub use reader::CatalogReader;

/// The schemas holding a name, sorted, for the two "which one did you mean"
/// errors below.
///
/// Sorted here rather than relied on from the query. The reader's lookup does
/// carry `ORDER BY schema_name`, so these messages are not unstable today — but
/// [`resolve_in_search_path`] is a pure function whose signature promises
/// nothing about row order, and the write side sorts explicitly
/// (`string_agg(… ORDER BY schema_name)`). Depending on a caller's `ORDER BY`
/// that nothing here enforces is the kind of coupling that breaks quietly when
/// the query is next edited. (Raised by review on PR #187.)
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
fn candidate_schemas(rows: &[(String, String)]) -> String {
    let mut schemas: Vec<&str> = rows.iter().map(|(s, _)| s.as_str()).collect();
    schemas.sort_unstable();
    schemas.join(", ")
}

/// The error a read must raise when a reference spells out a `<database>.`
/// prefix naming a database other than the one holding the catalog, or `None`
/// when the reference is fine to resolve.
///
/// Semantic views are single-catalog (TECH-DEBT #26): `semantic_layer._definitions`
/// lives in one database and every read resolves against it. Write DDL already
/// refuses an explicit foreign-database prefix
/// ([`crate::catalog::writes::current_database_guard_select`]) because dropping
/// the prefix would make `DROP SEMANTIC VIEW otherdb.analytics.v` delete the
/// *current* database's `analytics.v` — a wrong-object write rather than an
/// unsupported one. CAT-4: the same argument applies to reads. Ignoring the
/// prefix makes `semantic_view('otherdb.analytics.v')` return this catalog's
/// data under another database's name, with nothing to tell the caller that the
/// database they asked for was never consulted.
///
/// The comparison folds case per `DuckDB`'s identifier rule (CLAUDE.md: dialect
/// questions follow `DuckDB`), so `MEMORY.main.v` and `memory.main.v` are the
/// same reference. `ViewRef`'s parts are already lowercased by
/// [`crate::ident::parse_view_ref`]; `current_database()` returns the catalog's
/// own spelling, which need not be lowercase.
///
/// Split out of the FFI reader so the rule is unit-testable without a
/// connection — the reader supplies `current_database`, this decides.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn foreign_database_error(
    view: &crate::ident::ViewRef,
    current_database: &str,
) -> Option<String> {
    let written = view.database.as_deref()?;
    if crate::ident::ident_matches(written, current_database) {
        return None;
    }
    Some(format!(
        "semantic_views: '{view}' names database '{written}', but semantic views \
         are single-catalog and the catalog lives in '{current_database}'. Drop \
         the database qualifier, or name '{current_database}'."
    ))
}

/// Pick which of the rows a bare view name matched is the one the reference
/// means, following `DuckDB`'s search-path rule: the first schema on the path
/// that holds a view of that name wins.
///
/// Split out of the FFI lookup so the rule is unit-testable without a
/// connection — the reader does the I/O, this decides.
///
/// `rows` is every `(schema_name, definition)` the name matched, and
/// `search_path` the caller's resolution order, which the shim reads off the
/// caller's context (`sv_caller_search_path`) and the write side evaluates as
/// [`crate::catalog::writes::SEARCH_PATH_SQL`].
///
/// Two cases are not a plain first-match:
///
/// * **The name exists, but in no schema on the path.** `DuckDB` treats that
///   as "does not exist", and so do we — but the error says where it *does*
///   live, because "does not exist" for a view that `SHOW SEMANTIC VIEWS`
///   plainly lists is otherwise baffling.
/// * **The search path is empty.** On `DuckDB` 2.0 every call carries one (the
///   caller's path, or an explicit `search_path` argument), so this is an
///   explicit `search_path := []`, or a v1.5 build, where the shim cannot read
///   the caller's path. Falling back to "the unique match, or an ambiguity
///   error" keeps such calls working and never picks arbitrarily.
///
/// # Errors
///
/// Returns `Err` when the name cannot be resolved to exactly one view and
/// picking one would be a guess.
///
/// Compiled unconditionally so the rule is unit-tested under plain `cargo test`;
/// only the extension-gated reader calls it, so the bundled non-test build sees
/// it as dead.
#[cfg_attr(not(any(feature = "extension", test)), allow(dead_code))]
pub(crate) fn resolve_in_search_path<'a>(
    view: &crate::ident::ViewRef,
    rows: &'a [(String, String)],
    search_path: &[String],
) -> Result<Option<&'a str>, String> {
    if rows.is_empty() {
        return Ok(None);
    }
    // A qualified reference is already filtered to its schema by the query, and
    // `(schema_name, name)` is a primary key, so at most one row can come back.
    if view.schema.is_some() || rows.len() == 1 {
        return Ok(Some(rows[0].1.as_str()));
    }
    if search_path.is_empty() {
        return Err(format!(
            "semantic view '{}' is ambiguous: it exists in schemas {}. \
             Qualify the reference as <schema>.{}",
            view.name,
            candidate_schemas(rows),
            crate::expand::quote_ident_if_needed(&view.name)
        ));
    }
    for candidate in search_path {
        if let Some((_, definition)) = rows
            .iter()
            .find(|(schema, _)| crate::ident::ident_matches(schema, candidate))
        {
            return Ok(Some(definition.as_str()));
        }
    }
    Err(format!(
        "semantic view '{}' does not exist on the search path. It exists in \
         schemas {}, none of which are on the current search path ({}). \
         Qualify the reference as <schema>.{}, or add the schema to search_path.",
        view.name,
        candidate_schemas(rows),
        search_path.join(", "),
        crate::expand::quote_ident_if_needed(&view.name)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(feature = "extension"))]
    use duckdb::Connection;

    /// CAT-7's helper must be unique WITHIN a process, not just across them.
    ///
    /// The first version used PID + `SystemTime` nanos only. Rust runs tests on
    /// concurrent threads, so every caller shares a PID, and on a platform whose
    /// clock is coarser than nanoseconds two callers in one tick get the same
    /// path — CAT-7 again, inside a single run.
    ///
    /// This asserts the **counter**, not "1000 calls happened to differ". A
    /// tight loop on a fine-grained clock (Linux here) produces distinct
    /// timestamps anyway, so that version of the test passes with the counter
    /// removed — vacuous exactly where it matters, since the bug only bites on
    /// the coarse-clock platforms this cannot simulate. Pinning the monotonic
    /// component instead is clock-independent: with the counter gone the
    /// filename has three components rather than four and this fails outright.
    #[test]
    fn unique_temp_db_path_carries_a_clock_independent_counter() {
        // Underscore-free stem so the components are countable: the name is
        // exactly `<stem>_<pid>_<nanos>_<counter>`. Drop the counter and it is
        // three fields, which is what makes this fail pre-fix — checking "the
        // last field increases" would NOT, because the nanos field increases
        // too and parses as an integer just as happily.
        let fields = |p: PathBuf| -> Vec<String> {
            p.file_stem()
                .expect("stem")
                .to_string_lossy()
                .split('_')
                .map(str::to_owned)
                .collect()
        };

        let a = fields(unique_temp_db_path("cat7seq"));
        let b = fields(unique_temp_db_path("cat7seq"));
        assert_eq!(
            a.len(),
            4,
            "expected <stem>_<pid>_<nanos>_<counter>, got {a:?} — without the \
             counter, within-process uniqueness rests entirely on clock \
             resolution, which is the CAT-7 collision"
        );

        let (sa, sb): (u64, u64) = (
            a[3].parse().expect("counter"),
            b[3].parse().expect("counter"),
        );
        assert_eq!(
            sb,
            sa + 1,
            "counter must advance by exactly one per call, got {sa} then {sb}"
        );
        assert_eq!(a[1], b[1], "same process, so the PID field must match");
    }

    // CAT-4 (code-review 2026-08-03): the read side's foreign-database rule.
    // The end-to-end coverage is `test/sql/cat4_read_foreign_database.test`,
    // which pins every read surface; these pin the decision itself, including
    // the cases the .test file cannot reach without a second attached catalog.
    mod foreign_database_tests {
        use super::*;
        use crate::ident::parse_view_ref;

        #[test]
        fn unqualified_reference_is_never_foreign() {
            assert_eq!(
                foreign_database_error(&parse_view_ref("v").unwrap(), "memory"),
                None
            );
        }

        #[test]
        fn schema_qualified_reference_is_never_foreign() {
            assert_eq!(
                foreign_database_error(&parse_view_ref("analytics.v").unwrap(), "memory"),
                None
            );
        }

        #[test]
        fn matching_database_resolves() {
            assert_eq!(
                foreign_database_error(&parse_view_ref("memory.main.v").unwrap(), "memory"),
                None
            );
        }

        // DuckDB matches identifiers case-insensitively (CLAUDE.md: dialect
        // questions follow DuckDB), and `current_database()` returns the
        // catalog's own spelling — which may differ in case from the reference.
        #[test]
        fn database_comparison_folds_case() {
            assert_eq!(
                foreign_database_error(&parse_view_ref("MEMORY.main.v").unwrap(), "memory"),
                None
            );
            assert_eq!(
                foreign_database_error(&parse_view_ref("memory.main.v").unwrap(), "MEMORY"),
                None
            );
            assert_eq!(
                foreign_database_error(&parse_view_ref("\"Memory\".main.v").unwrap(), "memory"),
                None
            );
        }

        #[test]
        fn foreign_database_is_rejected() {
            let err = foreign_database_error(&parse_view_ref("adb.main.v").unwrap(), "memory")
                .expect("a foreign database must be rejected");
            // Both spellings, so the caller can see which of the two is wrong.
            assert!(err.contains("names database 'adb'"), "got: {err}");
            assert!(err.contains("the catalog lives in 'memory'"), "got: {err}");
            assert!(err.contains("single-catalog"), "got: {err}");
            // The whole reference as written, not just the bare name.
            assert!(err.contains("'adb.main.v'"), "got: {err}");
        }

        // The message embeds the reference as written. A database alias may
        // legally hold a dot when quoted (`ATTACH ':memory:' AS "a.b"`), and
        // rendering it raw would print `a.b.main.v` — a four-part reference
        // `parse_view_ref` rejects, so the error would misreport what it
        // parsed. `ViewRef`'s Display quotes exactly the parts that need it.
        #[test]
        fn a_dotted_database_alias_is_named_unambiguously() {
            let err = foreign_database_error(&parse_view_ref("\"a.b\".main.v").unwrap(), "memory")
                .expect("a foreign database must be rejected");
            assert!(err.contains("'\"a.b\".main.v'"), "got: {err}");
            assert!(!err.contains("'a.b.main.v'"), "got: {err}");
        }

        // A two-part reference is `schema.name`, never `database.name` — so a
        // schema that happens to share a name with some other database is not
        // mistaken for a foreign-database reference.
        #[test]
        fn two_part_reference_is_a_schema_not_a_database() {
            assert_eq!(
                foreign_database_error(&parse_view_ref("adb.v").unwrap(), "memory"),
                None
            );
        }
    }

    mod search_path_resolution_tests {
        use super::*;
        use crate::ident::parse_view_ref;

        fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
            pairs
                .iter()
                .map(|(s, d)| ((*s).to_string(), (*d).to_string()))
                .collect()
        }

        #[test]
        fn no_rows_is_not_found() {
            let r =
                resolve_in_search_path(&parse_view_ref("v").unwrap(), &[], &["main".to_string()]);
            assert_eq!(r.unwrap(), None);
        }

        #[test]
        fn first_schema_on_the_path_wins() {
            // The whole point of the rule: `b` is earlier on the path, so
            // `b.v` is what a bare `v` means — even though `a` sorts first.
            let all = rows(&[("a", "DEF_A"), ("b", "DEF_B")]);
            let path = ["b".to_string(), "a".to_string()];
            let got = resolve_in_search_path(&parse_view_ref("v").unwrap(), &all, &path).unwrap();
            assert_eq!(got, Some("DEF_B"));
        }

        #[test]
        fn path_order_decides_not_row_order() {
            // Same rows, reversed path: the answer must flip. This is what
            // separates real search-path resolution from "pick the first row".
            let all = rows(&[("a", "DEF_A"), ("b", "DEF_B")]);
            let got = resolve_in_search_path(
                &parse_view_ref("v").unwrap(),
                &all,
                &["a".to_string(), "b".to_string()],
            )
            .unwrap();
            assert_eq!(got, Some("DEF_A"));
        }

        // Both error paths name the candidate schemas, and both sort them
        // before doing so. The reader's query does carry `ORDER BY
        // schema_name`, so these are not flaky today — but this is a pure
        // function whose signature promises nothing about row order, and the
        // write side sorts explicitly (`string_agg(… ORDER BY schema_name)`).
        // Depending on a caller's ORDER BY that nothing here enforces is the
        // kind of coupling that breaks silently when the query is edited.
        // (Raised by review on PR #187.)

        #[test]
        fn the_ambiguity_error_names_schemas_in_a_stable_order() {
            let all = rows(&[("staging", "DEF_S"), ("analytics", "DEF_A")]);
            let err = resolve_in_search_path(&parse_view_ref("v").unwrap(), &all, &[]).unwrap_err();
            assert!(
                err.contains("schemas analytics, staging"),
                "schemas must be sorted regardless of row order: {err}"
            );
        }

        #[test]
        fn the_off_path_error_names_schemas_in_a_stable_order() {
            let all = rows(&[("staging", "DEF_S"), ("analytics", "DEF_A")]);
            let err = resolve_in_search_path(
                &parse_view_ref("v").unwrap(),
                &all,
                &["elsewhere".to_string()],
            )
            .unwrap_err();
            assert!(
                err.contains("schemas analytics, staging"),
                "schemas must be sorted regardless of row order: {err}"
            );
        }

        #[test]
        fn schema_matching_is_case_insensitive() {
            // The stored spelling is the catalog's; the path carries whatever
            // the caller's USE / SET wrote.
            let all = rows(&[("MySchema", "DEF")]);
            let got = resolve_in_search_path(
                &parse_view_ref("v").unwrap(),
                &all,
                &["MYSCHEMA".to_string()],
            )
            .unwrap();
            assert_eq!(got, Some("DEF"));
        }

        #[test]
        fn a_single_match_resolves_without_consulting_the_path() {
            // The overwhelmingly common case: one view of that name anywhere.
            // It resolves even if its schema is not on the path, which keeps
            // pre-existing single-schema setups working unchanged.
            let all = rows(&[("analytics", "DEF")]);
            let got =
                resolve_in_search_path(&parse_view_ref("v").unwrap(), &all, &["main".to_string()])
                    .unwrap();
            assert_eq!(got, Some("DEF"));
        }

        #[test]
        fn several_matches_none_on_the_path_is_an_actionable_error() {
            let all = rows(&[("a", "DEF_A"), ("b", "DEF_B")]);
            let err =
                resolve_in_search_path(&parse_view_ref("v").unwrap(), &all, &["main".to_string()])
                    .unwrap_err();
            assert!(err.contains("does not exist on the search path"), "{err}");
            // Naming where it DOES live is the difference between a baffling
            // error and one the reader can act on.
            assert!(err.contains("a, b"), "must say where it lives: {err}");
            assert!(err.contains("main"), "must say what the path was: {err}");
        }

        #[test]
        fn a_qualified_reference_takes_the_row_it_was_given() {
            // The query already filtered to the named schema, and
            // (schema_name, name) is a key, so the path is irrelevant here.
            let all = rows(&[("staging", "DEF")]);
            let got = resolve_in_search_path(
                &parse_view_ref("staging.v").unwrap(),
                &all,
                &["main".to_string()],
            )
            .unwrap();
            assert_eq!(got, Some("DEF"));
        }

        #[test]
        fn without_a_search_path_ambiguity_is_still_an_error() {
            // No path means no caller context gave one — SQL emitted before the
            // read side took the path from the caller's context.
            // Guessing would be wrong; the pre-injection rule still applies.
            let all = rows(&[("a", "DEF_A"), ("b", "DEF_B")]);
            let err = resolve_in_search_path(&parse_view_ref("v").unwrap(), &all, &[]).unwrap_err();
            assert!(err.contains("is ambiguous"), "{err}");
            assert!(err.contains("a, b"), "{err}");
        }

        #[test]
        fn the_suggested_name_is_quoted_when_it_needs_to_be() {
            // Same copy-pasteability rule as the write side (PR #186 review):
            // an unquoted `<schema>.a.b` would parse as a three-part name.
            let all = rows(&[("a", "DEF_A"), ("b", "DEF_B")]);
            let err =
                resolve_in_search_path(&parse_view_ref("\"a.b\"").unwrap(), &all, &[]).unwrap_err();
            assert!(err.contains(r#"<schema>."a.b""#), "{err}");
        }
    }

    #[test]
    fn definitions_table_const_is_consistent() {
        assert_eq!(
            DEFINITIONS_TABLE,
            format!("{DEFINITIONS_SCHEMA}.{DEFINITIONS_TABLE_NAME}"),
            "DEFINITIONS_TABLE must equal DEFINITIONS_SCHEMA.DEFINITIONS_TABLE_NAME"
        );
    }

    #[test]
    fn view_not_found_msg_wording() {
        assert_eq!(
            view_not_found_msg("sales"),
            "semantic view 'sales' does not exist"
        );
    }

    // In-memory `Connection` requires the bundled DuckDB API; the `extension`
    // feature swaps in `loadable-extension` stubs that error at runtime with
    // "DuckDB API not initialized or DuckDB feature omitted". Tests that need
    // an actual in-process DB are gated `not(feature = "extension")`; the
    // CatalogReader short-circuit tests (which never touch the DB) below
    // run under the `extension` feature.
    #[cfg(not(feature = "extension"))]
    fn in_memory_con() -> Connection {
        Connection::open_in_memory().expect("in-memory DuckDB")
    }

    /// Schema scoping: `_definitions` gains a `schema_name` column and a
    /// composite `(schema_name, name)` primary key, so two schemas can each
    /// hold a view of the same name.
    ///
    /// The legacy shape keyed on `name` alone, which is why
    /// `CREATE SEMANTIC VIEW other.v` reported "already exists" when
    /// `analytics.v` was present. The schema was only ever recorded *inside*
    /// the definition JSON, where nothing could enforce it.
    ///
    /// Migration is a rebuild (DuckDB cannot retrofit a primary key), and it
    /// cannot collide: the old key made `name` globally unique, so every
    /// `(schema_name, name)` derived from it is unique too.
    #[cfg(not(feature = "extension"))]
    mod schema_scoped_storage_tests {
        use super::*;

        /// Build a catalog in the pre-scoping shape.
        fn legacy_catalog(con: &Connection) {
            con.execute_batch(&format!(
                "CREATE SCHEMA {DEFINITIONS_SCHEMA};
                 CREATE TABLE {DEFINITIONS_TABLE} (
                     name       VARCHAR PRIMARY KEY,
                     definition VARCHAR NOT NULL
                 );"
            ))
            .expect("legacy catalog");
        }

        fn insert_legacy(con: &Connection, name: &str, definition: &str) {
            con.execute(
                &format!("INSERT INTO {DEFINITIONS_TABLE} (name, definition) VALUES (?, ?)"),
                duckdb::params![name, definition],
            )
            .expect("insert legacy row");
        }

        fn scoped_rows(con: &Connection) -> Vec<(String, String)> {
            let mut stmt = con
                .prepare(&format!(
                    "SELECT schema_name, name FROM {DEFINITIONS_TABLE} ORDER BY schema_name, name"
                ))
                .expect("select scoped rows");
            let mapped = stmt
                .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
                .expect("map scoped rows");
            mapped.collect::<Result<Vec<_>, _>>().expect("collect")
        }

        #[test]
        fn a_legacy_catalog_is_migrated_and_the_schema_comes_from_the_definition() {
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"analytics"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![("analytics".to_string(), "v".to_string())]
            );
        }

        /// Read `schema_name` from a migrated row's column AND from inside its
        /// stored JSON — the two the module documents as kept in lockstep.
        fn scoped_row_and_json_schema(con: &Connection, name: &str) -> (String, String) {
            let mut stmt = con
                .prepare(&format!(
                    "SELECT schema_name, definition FROM {DEFINITIONS_TABLE} WHERE name = ?"
                ))
                .expect("prepare");
            let (col, json): (String, String) = stmt
                .query_row(duckdb::params![name], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })
                .expect("row");
            let probe: serde_json::Value = serde_json::from_str(&json).expect("json");
            let in_json = probe
                .get("schema_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();
            (col, in_json)
        }

        // -------------------------------------------------------------------
        // CAT-1 (code-review 2026-08-03): the migration must record the
        // CATALOG's spelling of a schema, not the one the legacy row happens
        // to carry.
        //
        // `current_schema()` echoes the spelling last written in `USE`, so a
        // row created after `USE "ANALYTICS"` recorded `ANALYTICS` for a schema
        // the catalog calls `analytics`. Migrated verbatim, that row keys under
        // a spelling the catalog never had — and since `INSERT OR REPLACE`
        // conflicts on the byte-equal `(schema_name, name)` primary key while
        // every read guard folds case, a later `CREATE OR REPLACE
        // analytics.v` inserts a SECOND row instead of replacing the first.
        // Both then match the folded lookup, and `ORDER BY schema_name` puts
        // the uppercase one first, so reads keep returning the pre-replace
        // definition while the CREATE reported success.
        // -------------------------------------------------------------------

        #[test]
        fn migration_records_the_catalogs_schema_spelling_not_the_rows() {
            let con = in_memory_con();
            con.execute_batch("CREATE SCHEMA analytics;")
                .expect("create schema");
            legacy_catalog(&con);
            // The row records the schema in a spelling `USE "ANALYTICS"` would
            // have produced; the catalog itself calls it `analytics`.
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"ANALYTICS"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![("analytics".to_string(), "v".to_string())],
                "the stored key must be the catalog's own spelling"
            );
        }

        #[test]
        fn migration_keeps_column_and_json_schema_in_lockstep_when_canonicalising() {
            let con = in_memory_con();
            con.execute_batch("CREATE SCHEMA analytics;")
                .expect("create schema");
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"ANALYTICS"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            let (col, in_json) = scoped_row_and_json_schema(&con, "v");
            assert_eq!(col, "analytics");
            assert_eq!(
                in_json, col,
                "the SHOW/DESCRIBE listings read the schema out of the JSON, so \
                 rewriting the column without the JSON would make them disagree"
            );
        }

        /// A recorded schema that no longer exists in the catalog cannot be
        /// canonicalised. The row must still be carried across verbatim rather
        /// than dropped or the migration failing — losing a definition here is
        /// unrecoverable, which is the stance `migrated_row` already documents.
        #[test]
        fn migration_keeps_an_unknown_schema_spelling_rather_than_dropping_the_row() {
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"GhostSchema"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![("GhostSchema".to_string(), "v".to_string())],
                "an unresolvable schema keeps its recorded spelling"
            );
        }

        /// Control: a row already recording the catalog's spelling is untouched.
        #[test]
        fn migration_leaves_an_already_canonical_spelling_alone() {
            let con = in_memory_con();
            con.execute_batch("CREATE SCHEMA analytics;")
                .expect("create schema");
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"analytics"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![("analytics".to_string(), "v".to_string())]
            );
        }

        #[test]
        fn a_row_with_no_recorded_schema_lands_in_main() {
            // Rows written before the metadata stamp existed have no
            // `schema_name` at all. They must not become NULL or empty — the
            // column is part of the key.
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "ancient", r#"{"name":"ancient"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![("main".to_string(), "ancient".to_string())]
            );
        }

        #[test]
        fn a_backfilled_schema_is_written_into_the_definition_too() {
            // The SHOW / DESCRIBE listings read `schema_name` from the stored
            // definition, so a row that gains a column value but keeps a JSON
            // with no schema would report blank while living in `main`. Both
            // must say the same thing.
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "ancient", r#"{"name":"ancient"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            let (schema, definition): (String, String) = con
                .query_row(
                    &format!(
                        "SELECT schema_name, definition FROM {DEFINITIONS_TABLE} \
                          WHERE name = 'ancient'"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("migrated row");
            assert_eq!(schema, "main");
            let parsed: serde_json::Value =
                serde_json::from_str(&definition).expect("definition stays valid JSON");
            assert_eq!(
                parsed
                    .get("schema_name")
                    .and_then(serde_json::Value::as_str),
                Some("main"),
                "definition must record the same schema as the column: {definition}"
            );
            assert_eq!(
                parsed.get("name").and_then(serde_json::Value::as_str),
                Some("ancient"),
                "backfill must not disturb the rest of the definition: {definition}"
            );
        }

        #[test]
        fn a_row_whose_definition_is_not_json_survives_verbatim() {
            // Dropping a row this migration cannot parse would destroy user
            // data irrecoverably. It is filed under the fallback schema with
            // its bytes untouched.
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "corrupt", "not json at all");

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            let (schema, definition): (String, String) = con
                .query_row(
                    &format!(
                        "SELECT schema_name, definition FROM {DEFINITIONS_TABLE} \
                          WHERE name = 'corrupt'"
                    ),
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .expect("migrated row");
            assert_eq!(schema, "main");
            assert_eq!(definition, "not json at all");
        }

        #[test]
        fn migration_preserves_every_row_and_its_definition() {
            // The rebuild drops and recreates the table; losing or corrupting a
            // definition here destroys user data irrecoverably, so assert the
            // payload survives byte-for-byte alongside the count.
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "a", r#"{"name":"a","schema_name":"s1"}"#);
            insert_legacy(&con, "b", r#"{"name":"b","schema_name":"s2"}"#);
            insert_legacy(&con, "c", r#"{"name":"c"}"#);

            init_catalog(&con, ":memory:", false).expect("init_catalog migrates");

            assert_eq!(
                scoped_rows(&con),
                vec![
                    ("main".to_string(), "c".to_string()),
                    ("s1".to_string(), "a".to_string()),
                    ("s2".to_string(), "b".to_string()),
                ]
            );
            let def: String = con
                .query_row(
                    &format!("SELECT definition FROM {DEFINITIONS_TABLE} WHERE name = 'a'"),
                    [],
                    |r| r.get(0),
                )
                .expect("definition survives");
            assert_eq!(def, r#"{"name":"a","schema_name":"s1"}"#);
        }

        #[test]
        fn the_composite_key_lets_two_schemas_hold_the_same_name() {
            // The whole point. Under the old single-column key the second
            // insert violated the primary key.
            let con = in_memory_con();
            init_catalog(&con, ":memory:", false).expect("fresh catalog");

            for schema in ["analytics", "reporting"] {
                con.execute(
                    &format!(
                        "INSERT INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
                         VALUES (?, 'v', '{{}}')"
                    ),
                    duckdb::params![schema],
                )
                .expect("same name in a second schema");
            }
            assert_eq!(scoped_rows(&con).len(), 2);
        }

        #[test]
        fn the_same_name_in_one_schema_is_still_rejected() {
            // Over-widening guard: scoping must not weaken uniqueness WITHIN a
            // schema, which is what the key is for.
            let con = in_memory_con();
            init_catalog(&con, ":memory:", false).expect("fresh catalog");

            let insert = format!(
                "INSERT INTO {DEFINITIONS_TABLE} (schema_name, name, definition) \
                 VALUES ('analytics', 'v', '{{}}')"
            );
            con.execute(&insert, []).expect("first insert");
            assert!(
                con.execute(&insert, []).is_err(),
                "a duplicate (schema, name) must still violate the primary key"
            );
        }

        #[test]
        fn migrating_is_idempotent() {
            // init_catalog runs on every LOAD. A second pass must not rebuild
            // an already-scoped table (and must not lose rows doing so).
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v","schema_name":"analytics"}"#);

            init_catalog(&con, ":memory:", false).expect("first");
            init_catalog(&con, ":memory:", false).expect("second");

            assert_eq!(
                scoped_rows(&con),
                vec![("analytics".to_string(), "v".to_string())]
            );
        }

        #[test]
        fn a_read_only_database_with_a_legacy_catalog_refuses_to_load() {
            // A read-only host cannot be migrated in place, and silently
            // serving it through a legacy path would mean two storage shapes
            // live forever. Refuse with an actionable message instead.
            let con = in_memory_con();
            legacy_catalog(&con);
            insert_legacy(&con, "v", r#"{"name":"v"}"#);

            let err = init_catalog(&con, "/tmp/x.db", true)
                .expect_err("legacy + read-only must not load");
            let msg = err.to_string();
            assert!(
                msg.contains("read-only") && msg.contains("semantic_views"),
                "error should name the extension and the read-only cause, got: {msg}"
            );
        }

        #[test]
        fn a_read_only_database_that_is_already_scoped_loads_fine() {
            // The control: read-only is not itself an error, only read-only
            // WITH an unmigrated catalog.
            let con = in_memory_con();
            init_catalog(&con, ":memory:", false).expect("scope it while writable");
            init_catalog(&con, "/tmp/x.db", true).expect("read-only load of a scoped catalog");
        }

        #[test]
        fn a_read_only_database_with_no_catalog_at_all_still_loads() {
            // Control for the detection logic: "no table" must not be mistaken
            // for "legacy table". A fresh read-only DB has never had one.
            let con = in_memory_con();
            init_catalog(&con, "/tmp/x.db", true).expect("read-only load with no catalog");
        }
    }

    // TEMPORARY: smoke test for v0.8.0 race-guard SQL shape.
    // CTE+DML+RETURNING is NOT supported in DuckDB v1.5.2 ("Parser Error:
    // A CTE needs a SELECT"), so we emit two statements separated by `;`:
    // a guard SELECT that raises via error() if the row is missing, then
    // the DELETE/UPDATE itself. Both statements run on the caller's
    // connection; they share one snapshot only within an explicit caller
    // transaction — under autocommit each auto-commits separately (FF-1 /
    // TECH-DEBT #27). This single-connection smoke test exercises the SQL
    // shape, not the concurrency window.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn two_statement_guard_then_dml_smoke() {
        let con = in_memory_con();
        con.execute_batch(
            "CREATE TABLE t (name VARCHAR PRIMARY KEY, val INTEGER); \
             INSERT INTO t VALUES ('a', 1), ('b', 2), ('c', 3);",
        )
        .unwrap();

        // 1. Guard fires when row missing.
        let err = con
            .execute_batch(
                "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM t WHERE name = 'nonexistent') \
                              THEN error('not found') \
                              ELSE TRUE END; \
                 DELETE FROM t WHERE name = 'nonexistent' RETURNING name;",
            )
            .err()
            .expect("missing-row guard must error");
        assert!(format!("{err}").contains("not found"), "unexpected: {err}");

        // Row 'a' must NOT have been deleted (guard aborted before DELETE).
        let count: i64 = con
            .query_row("SELECT count(*) FROM t WHERE name = 'a'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // 2. Guard passes; DELETE runs and the user-visible result is the
        // RETURNING from the last statement.
        let mut stmt = con
            .prepare(
                "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM t WHERE name = 'a') \
                              THEN error('not found') \
                              ELSE TRUE END; \
                 DELETE FROM t WHERE name = 'a' RETURNING name AS view_name;",
            )
            .expect("multi-statement parse");
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["a".to_string()]);

        // 3. UPDATE variant.
        let mut stmt = con
            .prepare(
                "SELECT CASE WHEN NOT EXISTS (SELECT 1 FROM t WHERE name = 'b') \
                              THEN error('not found') \
                              ELSE TRUE END; \
                 UPDATE t SET val = 99 WHERE name = 'b' RETURNING name AS view_name;",
            )
            .expect("UPDATE guard parse");
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(names, vec!["b".to_string()]);
    }

    // AR-4: the schema_version upgrade pass stamps verifiable current-format
    // rows and leaves un-upgradeable legacy rows at version 0.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn upgrade_stamps_complete_rows_and_skips_incomplete() {
        use crate::model::{SemanticViewDefinition, CURRENT_SCHEMA_VERSION};
        let con = in_memory_con();
        init_catalog(&con, ":memory:", false).unwrap();

        // Complete row: every relationship carries fk_columns, no schema_version.
        let complete = r#"{"tables":[{"alias":"o","table":"orders","pk_columns":["id"]},
            {"alias":"c","table":"customers","pk_columns":["id"]}],
            "dimensions":[],"metrics":[],
            "joins":[{"table":"c","from_alias":"o","fk_columns":["cid"],"ref_columns":["id"]}]}"#;
        // Incomplete row: a relationship in the legacy `on`-only encoding (no fk_columns).
        let incomplete = r#"{"tables":[{"alias":"o","table":"orders"}],
            "dimensions":[],"metrics":[],
            "joins":[{"table":"c","on":"o.cid = c.id"}]}"#;
        // A no-join single-table view: trivially complete, should be stamped.
        let single = r#"{"tables":[{"alias":"o","table":"orders"}],"dimensions":[],"metrics":[]}"#;

        for (name, def) in [
            ("complete_v", complete),
            ("incomplete_v", incomplete),
            ("single_v", single),
        ] {
            con.execute(
                "INSERT INTO semantic_layer._definitions (schema_name, name, definition) \
                 VALUES ('main', ?, ?)",
                duckdb::params![name, def],
            )
            .unwrap();
        }

        // Re-run init_catalog: triggers the one-time upgrade pass.
        init_catalog(&con, ":memory:", false).unwrap();

        let stored = |name: &str| -> String {
            con.query_row(
                "SELECT definition FROM semantic_layer._definitions WHERE name = ?",
                duckdb::params![name],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("complete_v")),
            CURRENT_SCHEMA_VERSION,
            "complete row must be stamped current"
        );
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("single_v")),
            CURRENT_SCHEMA_VERSION,
            "no-join view is trivially complete and must be stamped"
        );
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("incomplete_v")),
            0,
            "un-upgradeable legacy row must be left at version 0"
        );

        // Idempotent: a second pass changes nothing and does not error.
        init_catalog(&con, ":memory:", false).unwrap();
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("complete_v")),
            CURRENT_SCHEMA_VERSION
        );
    }

    // CAT-2 (code-review 2026-08-03): the AR-4 version stamp keys on `name`
    // alone, which was correct when `name` was the whole primary key. Under
    // schema scoping the key is `(schema_name, name)`, so `WHERE name = ?`
    // stamps EVERY same-named row across schemas — including one the pass
    // deliberately left unverified. That contradicts the invariant the pass
    // exists to maintain: an un-upgradeable row must stay at version 0 so
    // reads hard-error rather than silently under-checking it.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn upgrade_stamps_only_the_row_it_verified_not_its_namesakes_in_other_schemas() {
        use crate::model::{SemanticViewDefinition, CURRENT_SCHEMA_VERSION};
        let con = in_memory_con();
        init_catalog(&con, ":memory:", false).unwrap();
        con.execute_batch("CREATE SCHEMA analytics;").unwrap();

        // Same view NAME in two schemas. The `analytics` one is upgradeable;
        // the `main` one carries a legacy `on`-only relationship and must be
        // left alone.
        let upgradeable =
            r#"{"tables":[{"alias":"o","table":"orders"}],"dimensions":[],"metrics":[]}"#;
        let incomplete = r#"{"tables":[{"alias":"o","table":"orders"}],
            "dimensions":[],"metrics":[],
            "joins":[{"table":"c","on":"o.cid = c.id"}]}"#;
        for (schema, def) in [("analytics", upgradeable), ("main", incomplete)] {
            con.execute(
                "INSERT INTO semantic_layer._definitions (schema_name, name, definition) \
                 VALUES (?, 'v', ?)",
                duckdb::params![schema, def],
            )
            .unwrap();
        }

        init_catalog(&con, ":memory:", false).unwrap();

        let stored = |schema: &str| -> String {
            con.query_row(
                "SELECT definition FROM semantic_layer._definitions \
                 WHERE schema_name = ? AND name = 'v'",
                duckdb::params![schema],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
        };
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("analytics")),
            CURRENT_SCHEMA_VERSION,
            "the verified row must be stamped"
        );
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(&stored("main")),
            0,
            "the UNVERIFIED namesake in another schema must NOT be stamped -- \
             stamping it claims a guarantee the pass never checked"
        );
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn init_catalog_creates_schema_and_table() {
        let con = in_memory_con();
        init_catalog(&con, ":memory:", false).unwrap();
        // Idempotent: second call must not error
        init_catalog(&con, ":memory:", false).unwrap();

        let count: i64 = con
            .query_row(
                "SELECT count(*) FROM semantic_layer._definitions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    /// Bootstrap a valid, empty DuckDB file at `db_path` so it can be reopened
    /// read-only (`open_with_flags` will not create a file in read-only mode).
    #[cfg(not(feature = "extension"))]
    fn bootstrap_empty_db_file(db_path: &str) {
        let con = Connection::open(db_path).expect("open writable to create the file");
        con.execute_batch("CREATE TABLE cat8_bootstrap(x INTEGER);")
            .expect("write something so the file has real content");
        con.execute_batch("CHECKPOINT;").expect("flush to disk");
    }

    #[cfg(not(feature = "extension"))]
    fn open_read_only(db_path: &str) -> Connection {
        let cfg = duckdb::Config::default()
            .access_mode(duckdb::AccessMode::ReadOnly)
            .expect("set access_mode");
        Connection::open_with_flags(db_path, cfg).expect("open read-only")
    }

    /// CAT-8 (code-review 2026-08-08): a read-only open of a database that
    /// still has an unimported v0.1.0 companion file used to succeed silently
    /// — the companion detection lives *below* the `is_read_only` early return,
    /// so it was unreachable, and every view in the file then resolved as
    /// "does not exist" with nothing pointing at the un-migrated catalog next
    /// to the database. The sibling un-migrated shape (a legacy `_definitions`
    /// *table*) has always refused loudly; this makes the two agree.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn cat8_read_only_open_refuses_an_unimported_companion_file() {
        let db_path_buf = unique_temp_db_path("test_cat8_ro_companion");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");
        let companion = PathBuf::from(format!("{db_path}.{V010_COMPANION_EXT}"));

        bootstrap_empty_db_file(db_path);
        std::fs::write(
            &companion,
            r#"{"orders": "{\"base_table\":\"orders\",\"dimensions\":[],\"metrics\":[]}"}"#,
        )
        .unwrap();

        let con = open_read_only(db_path);
        let err = init_catalog(&con, db_path, true)
            .expect_err("an unimported companion file must be refused, not ignored");
        let msg = err.to_string();
        for expected in [
            "v0.1.0 companion file",
            "read-only",
            "Open it writable once",
        ] {
            assert!(
                msg.contains(expected),
                "message must stay as actionable as the legacy-table refusal \
                 (missing {expected:?}), got: {msg}"
            );
        }
        assert!(
            msg.contains(&companion.display().to_string()),
            "message must name the file to act on, got: {msg}"
        );
        assert!(
            companion.exists(),
            "refusing must not delete the file — it is the only copy of those views"
        );

        drop(con);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(&companion);
    }

    /// The CAT-8 control: a read-only open with **no** companion file is still
    /// a clean no-op. Without this the fix could refuse every read-only open
    /// and the test above would not notice.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn cat8_read_only_open_without_a_companion_file_is_a_no_op() {
        let db_path_buf = unique_temp_db_path("test_cat8_ro_clean");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");

        bootstrap_empty_db_file(db_path);
        let con = open_read_only(db_path);
        init_catalog(&con, db_path, true)
            .expect("a read-only open with nothing to migrate must succeed");

        drop(con);
        let _ = std::fs::remove_file(db_path);
    }

    /// The companion path both branches compute has to be the same one, or the
    /// read-only refusal fires for a file the writable import would never look
    /// at. Pins the `set_extension` shape (`x.duckdb` -> `x.duckdb.semantic_views`,
    /// extensionless `x` -> `x.semantic_views`) and the in-memory exemption.
    #[test]
    fn cat8_companion_path_shape() {
        assert_eq!(
            v010_companion_path("/tmp/x.duckdb"),
            Some(PathBuf::from("/tmp/x.duckdb.semantic_views"))
        );
        assert_eq!(
            v010_companion_path("/tmp/x"),
            Some(PathBuf::from("/tmp/x.semantic_views"))
        );
        assert_eq!(v010_companion_path(":memory:"), None);
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn migration_corrupt_companion_file_errors_and_survives() {
        // MS-2 (code-review 2026-07-02): pre-fix, an unreadable or corrupt
        // v0.1.0 companion file was silently DELETED without importing —
        // permanent loss of pre-v0.2 definitions. Post-fix init_catalog must
        // error, and the file must be left in place for the user to repair.
        // CAT-7: unique per-invocation paths; see `unique_temp_db_path`.
        let db_path_buf = unique_temp_db_path("test_ms2_corrupt");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");
        let companion = PathBuf::from(format!("{db_path}.{V010_COMPANION_EXT}"));

        std::fs::write(&companion, "{not valid json").unwrap();
        let con = Connection::open(db_path).expect("open file-backed DB");
        let err = init_catalog(&con, db_path, false)
            .expect_err("corrupt companion file must fail the load");
        assert!(
            err.to_string().contains("not valid JSON"),
            "error should name the problem, got: {err}"
        );
        assert!(
            companion.exists(),
            "corrupt companion file must be left in place, not deleted"
        );

        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(&companion);
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn migration_valid_companion_file_imports_then_deletes() {
        // CAT-7: unique per-invocation paths; see `unique_temp_db_path`.
        let db_path_buf = unique_temp_db_path("test_ms2_valid");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");
        let companion = PathBuf::from(format!("{db_path}.{V010_COMPANION_EXT}"));

        std::fs::write(
            &companion,
            r#"{"orders": "{\"base_table\":\"orders\",\"dimensions\":[],\"metrics\":[]}"}"#,
        )
        .unwrap();
        let con = Connection::open(db_path).expect("open file-backed DB");
        init_catalog(&con, db_path, false).expect("valid companion file imports cleanly");

        let count: i64 = con
            .query_row(
                "SELECT count(*) FROM semantic_layer._definitions WHERE name = 'orders'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "companion definition should be imported");
        assert!(
            !companion.exists(),
            "companion file should be deleted after a successful import"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn migration_companion_import_backfills_schema_name_into_json() {
        // CAT-3 (code-review 2026-08-03): the companion import is the fourth
        // writer of a definition row, and the only one that used to skip the
        // `schema_name` backfill `migrated_row` documents as keeping the
        // column and the JSON in lockstep. An imported row filed under column
        // `schema_name = 'main'` whose JSON carried no `schema_name` listed
        // with an empty schema in `SHOW SEMANTIC VIEWS`, and was missed
        // entirely by `SHOW SEMANTIC VIEWS IN SCHEMA main` — the listings read
        // the JSON, not the column.
        // CAT-7: unique per-invocation paths; see `unique_temp_db_path`.
        let db_path_buf = unique_temp_db_path("test_cat3_backfill");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");
        let companion = PathBuf::from(format!("{db_path}.{V010_COMPANION_EXT}"));

        std::fs::write(
            &companion,
            r#"{"orders": "{\"base_table\":\"orders\",\"dimensions\":[],\"metrics\":[]}"}"#,
        )
        .unwrap();
        let con = Connection::open(db_path).expect("open file-backed DB");
        init_catalog(&con, db_path, false).expect("valid companion file imports cleanly");

        let (schema_col, definition): (String, String) = con
            .query_row(
                "SELECT schema_name, definition FROM semantic_layer._definitions \
                 WHERE name = 'orders'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(schema_col, UNRECORDED_SCHEMA_FALLBACK);

        let parsed: serde_json::Value = serde_json::from_str(&definition).unwrap();
        let in_json = parsed
            .get("schema_name")
            .and_then(serde_json::Value::as_str);
        assert_eq!(
            in_json,
            Some(schema_col.as_str()),
            "the imported row's JSON must record the same schema as its column, \
             or the SHOW listings (which read the JSON) disagree with lookup \
             (which reads the column); got definition: {definition}"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn pragma_database_list_returns_file_path() {
        // CAT-7: unique per-invocation paths; see `unique_temp_db_path`.
        let tmpfile_buf = unique_temp_db_path("test_pragma_rust_check");
        let tmpfile = tmpfile_buf.to_str().expect("temp dir is UTF-8");
        let con = Connection::open(tmpfile).expect("open file-backed connection");
        let mut stmt = con.prepare("PRAGMA database_list").expect("prepare PRAGMA");
        let paths: Vec<Option<String>> = stmt
            .query_map([], |row| row.get::<_, Option<String>>(2))
            .expect("query_map")
            .filter_map(|r| r.ok())
            .collect();
        let file_path = paths.into_iter().flatten().find(|s| !s.is_empty());
        assert!(
            file_path.is_some(),
            "PRAGMA database_list should return a non-empty file path for file-backed DB"
        );
        let path = file_path.unwrap();
        assert!(
            path.contains("test_pragma_rust_check"),
            "file path should reference the opened DB file, got: {path}"
        );
        let _ = std::fs::remove_file(tmpfile);
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn pragma_database_list_returns_none_for_in_memory() {
        let con = in_memory_con();
        let mut stmt = con.prepare("PRAGMA database_list").expect("prepare PRAGMA");
        let paths: Vec<Option<String>> = stmt
            .query_map([], |row| row.get::<_, Option<String>>(2))
            .expect("query_map")
            .filter_map(|r| r.ok())
            .collect();
        let file_path = paths.into_iter().flatten().find(|s| !s.is_empty());
        assert!(
            file_path.is_none(),
            "PRAGMA database_list should return no file path for in-memory DB, got: {file_path:?}"
        );
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn persist_02_rollback_leaves_catalog_unchanged() {
        // CAT-7: unique per-invocation paths; see `unique_temp_db_path`.
        let db_path_buf = unique_temp_db_path("test_persist02_rollback");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");

        let con = Connection::open(db_path).expect("open file-backed DB");
        init_catalog(&con, db_path, false).unwrap();

        let json = r#"{"base_table":"orders","dimensions":[],"metrics":[]}"#;
        con.execute(
            "INSERT OR REPLACE INTO semantic_layer._definitions (schema_name, name, definition) \
             VALUES ('main', ?, ?)",
            duckdb::params!["orders", json],
        )
        .unwrap();

        let count_before: i64 = con
            .query_row(
                "SELECT count(*) FROM semantic_layer._definitions WHERE name = 'orders'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count_before, 1);

        // DuckDB table rollback: BEGIN + DELETE + ROLLBACK must leave row present
        con.execute_batch(
            "BEGIN; DELETE FROM semantic_layer._definitions WHERE name = 'orders'; ROLLBACK;",
        )
        .unwrap();

        let count_after: i64 = con
            .query_row(
                "SELECT count(*) FROM semantic_layer._definitions WHERE name = 'orders'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count_after, 1,
            "Row must still exist after ROLLBACK (PERSIST-02)"
        );

        drop(con);
        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(format!("{db_path}.wal"));
    }

    /// TC-6: restart persistence. Open a file-backed DB, persist a definition,
    /// close the connection entirely, reopen from disk, run `init_catalog`
    /// against the now-populated catalog, and confirm the definition survives.
    ///
    /// This is the open→persist→drop→reopen→lookup coverage the Makefile /
    /// `_excluded` test headers claimed "verified via cargo test Rust
    /// integration tests" — before this test, only ROLLBACK (persist_02) was
    /// exercised, not durability across a fresh connection. The key invariant
    /// is that `init_catalog` on an existing catalog is idempotent: it must
    /// `CREATE ... IF NOT EXISTS` and leave stored rows intact, not re-create
    /// or truncate the table.
    #[cfg(not(feature = "extension"))]
    #[test]
    fn tc6_restart_persistence_survives_reopen() {
        // Unique per-invocation filename (pid + nanos) so concurrent `cargo test`
        // runs — in this binary or another process — cannot race on the same
        // DB/WAL paths and flake via cross-deletion/reuse. CAT-7 lifted this
        // out of here into `unique_temp_db_path` and put every other
        // file-backed test on it.
        let db_path_buf = unique_temp_db_path("test_tc6_restart_persistence");
        let db_path = db_path_buf.to_str().expect("temp dir is UTF-8");

        let json = r#"{"schema_version":1,"base_table":"sales","dimensions":[],"metrics":[]}"#;

        // --- Session 1: create the catalog and persist a definition. ---
        {
            let con = Connection::open(db_path).expect("open file-backed DB (session 1)");
            init_catalog(&con, db_path, false).unwrap();
            con.execute(
                "INSERT OR REPLACE INTO semantic_layer._definitions (schema_name, name, definition) \
             VALUES ('main', ?, ?)",
                duckdb::params!["sales", json],
            )
            .unwrap();
            // Force the write to the file so a fresh open sees it.
            con.execute_batch("CHECKPOINT;").unwrap();
            drop(con);
        }

        // --- Session 2: reopen from disk; init_catalog must not clobber. ---
        {
            let con = Connection::open(db_path).expect("reopen file-backed DB (session 2)");
            // Re-running init_catalog against a populated catalog is the exact
            // path taken on every LOAD of an existing database; it must be a
            // no-op for stored rows.
            init_catalog(&con, db_path, false).unwrap();

            let stored: String = con
                .query_row(
                    "SELECT definition FROM semantic_layer._definitions WHERE name = 'sales'",
                    [],
                    |row| row.get(0),
                )
                .expect("definition must survive reopen + init_catalog");
            assert_eq!(
                stored, json,
                "reopened definition must be byte-identical to what was persisted"
            );

            let count: i64 = con
                .query_row(
                    "SELECT count(*) FROM semantic_layer._definitions",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "init_catalog on reopen must not add or drop rows");
            drop(con);
        }

        let _ = std::fs::remove_file(db_path);
        let _ = std::fs::remove_file(format!("{db_path}.wal"));
    }

    // -----------------------------------------------------------------
    // Phase 63 (v0.9.0): read-only LOAD support — init_catalog
    // short-circuit + CatalogReader::{lookup,list_all,list_names}
    // short-circuits when catalog_table_present=false. See
    // 63-RESEARCH.md §3 Q2/Q3/Q4.
    // -----------------------------------------------------------------

    #[cfg(not(feature = "extension"))]
    #[test]
    fn init_catalog_skips_writes_on_readonly() {
        // Phase 63 (RO-01): is_read_only=true must early-return without
        // creating the schema. Verified by checking information_schema.
        let con = in_memory_con();
        init_catalog(&con, ":memory:", true).unwrap();
        let count: i64 = con
            .query_row(
                "SELECT count(*) FROM information_schema.schemata WHERE schema_name = 'semantic_layer'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "init_catalog with is_read_only=true must NOT create the semantic_layer schema"
        );
    }

    #[cfg(not(feature = "extension"))]
    #[test]
    fn init_catalog_writes_when_writable() {
        // Sibling: writable path still creates the schema + table.
        let con = in_memory_con();
        init_catalog(&con, ":memory:", false).unwrap();
        let count: i64 = con
            .query_row(
                "SELECT count(*) FROM semantic_layer._definitions",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "writable init_catalog must create empty _definitions table"
        );
    }

    #[test]
    fn lookup_returns_none_when_table_missing() {
        // Phase 63 (RO-04): an absent catalog table reads as "not found".
        let reader = CatalogReader::from_rows(false, "memory", vec![]);
        let result = reader.lookup(&crate::ident::parse_view_ref("any_view").unwrap(), &[]);
        assert!(
            matches!(result, Ok(None)),
            "expected Ok(None), got: {:?}",
            result
        );
    }

    #[test]
    fn list_all_returns_empty_when_table_missing() {
        // Phase 63 (RO-03): an absent catalog table lists nothing.
        let reader = CatalogReader::from_rows(false, "memory", vec![]);
        let result = reader.list_all();
        assert!(
            matches!(result, Ok(ref v) if v.is_empty()),
            "expected Ok(empty), got: {:?}",
            result
        );
    }

    #[test]
    fn list_names_returns_empty_when_table_missing() {
        // Phase 63 (RO-03): an absent catalog table lists nothing.
        let reader = CatalogReader::from_rows(false, "memory", vec![]);
        let result = reader.list_names();
        assert!(
            matches!(result, Ok(ref v) if v.is_empty()),
            "expected Ok(empty), got: {:?}",
            result
        );
    }

    #[test]
    fn foreign_database_is_checked_against_the_managed_catalog() {
        // CAT-4 runs ahead of the absent-table short-circuit, against the
        // database the rows were read from.
        let reader = CatalogReader::from_rows(false, "memory", vec![]);
        let foreign = crate::ident::parse_view_ref("other.main.v").unwrap();
        assert!(reader.lookup(&foreign, &[]).is_err());
        let own = crate::ident::parse_view_ref("memory.main.v").unwrap();
        assert_eq!(reader.lookup(&own, &[]), Ok(None));
    }

    #[test]
    fn a_nul_in_the_name_is_rejected_not_looked_up() {
        // The SQL lookup this reader replaced refused a NUL at its CString
        // boundary; a plain "does not exist" would echo the NUL back and the
        // message would be cut short there.
        let reader = CatalogReader::from_rows(true, "memory", vec![]);
        let view = |schema: Option<&str>, name: &str| crate::ident::ViewRef {
            database: None,
            schema: schema.map(str::to_string),
            name: name.to_string(),
        };
        let err = reader.lookup(&view(None, "v\0bad"), &[]).unwrap_err();
        assert!(err.contains("view name contains null byte"), "{err}");
        let err = reader.lookup(&view(Some("s\0"), "v"), &[]).unwrap_err();
        assert!(err.contains("schema name contains null byte"), "{err}");
    }

    #[test]
    fn a_schema_folds_one_codepoint_at_a_time() {
        // As DuckDB's lower() does: a word-final Σ is σ, not ς, and İ is i.
        let reader = CatalogReader::from_rows(
            true,
            "memory",
            vec![
                (
                    Some("ΣΑΣ".into()),
                    Some("v".into()),
                    Some("{\"a\":1}".into()),
                ),
                (Some("İ".into()), Some("w".into()), Some("{\"b\":2}".into())),
            ],
        );
        let view = |schema: &str, name: &str| crate::ident::ViewRef {
            database: None,
            schema: Some(schema.to_string()),
            name: name.to_string(),
        };
        let found = reader.lookup(&view("σασ", "v"), &[]).unwrap();
        assert_eq!(found.as_deref(), Some("{\"a\":1}"));
        let found = reader.lookup(&view("i", "w"), &[]).unwrap();
        assert_eq!(found.as_deref(), Some("{\"b\":2}"));
    }

    /// The reader answers over the rows the caller's transaction sees what the
    /// SQL it replaced (TECH-DEBT #19) answered over the table: compared here
    /// against those queries run on a real table holding the same rows,
    /// NULLs, case variants and ordering ties included.
    #[cfg(not(feature = "extension"))]
    mod reader_matches_its_sql {
        use super::*;
        use proptest::prelude::*;

        type Row = (Option<String>, Option<String>, Option<String>);

        fn opt(values: &'static [&'static str]) -> impl Strategy<Value = Option<String>> {
            prop_oneof![
                1 => Just(None),
                6 => proptest::sample::select(values).prop_map(|v| Some(v.to_string())),
            ]
        }

        fn rows() -> impl Strategy<Value = Vec<Row>> {
            proptest::collection::vec(
                (
                    opt(&["main", "Main", "s", "S", "é", "É", "ΣΑΣ", "İ"]),
                    opt(&["v", "V", "w", ""]),
                    opt(&["{\"a\":1}", "{\"b\":2}", ""]),
                ),
                0..10,
            )
            // `(schema_name, name)` is the table's key.
            .prop_map(|rows| {
                let mut seen = std::collections::HashSet::new();
                rows.into_iter()
                    .filter(|(s, n, _)| seen.insert((s.clone(), n.clone())))
                    .collect()
            })
        }

        fn table(rows: &[Row]) -> Connection {
            let con = in_memory_con();
            con.execute_batch(
                "CREATE TABLE d (schema_name VARCHAR, name VARCHAR, definition VARCHAR)",
            )
            .unwrap();
            for (s, n, d) in rows {
                con.execute("INSERT INTO d VALUES (?, ?, ?)", duckdb::params![s, n, d])
                    .unwrap();
            }
            con
        }

        fn sql_lookup(con: &Connection, name: &str, schema: Option<&str>) -> Vec<(String, String)> {
            let mut stmt = con
                .prepare(
                    "SELECT schema_name, definition FROM d \
                     WHERE name = $1 AND ($2 IS NULL OR lower(schema_name) = lower($2)) \
                     ORDER BY schema_name",
                )
                .unwrap();
            stmt.query_map(duckdb::params![name, schema], |r| {
                Ok((
                    r.get::<_, Option<String>>(0)?,
                    r.get::<_, Option<String>>(1)?,
                ))
            })
            .unwrap()
            .filter_map(|r| {
                let (s, d) = r.unwrap();
                Some((s.unwrap_or_default(), d?))
            })
            .collect()
        }

        fn sql_strings(con: &Connection, sql: &str, columns: usize) -> Vec<Vec<String>> {
            let mut stmt = con.prepare(sql).unwrap();
            stmt.query_map([], |r| {
                (0..columns)
                    .map(|i| Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default()))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        }

        proptest! {
            #[test]
            fn lookup_and_listings_match(
                rows in rows(),
                name in proptest::sample::select(&["v", "V", "w", "x", ""][..]),
                schema in opt(&["main", "MAIN", "s", "é", "É", "t", "σασ", "ΣΑΣ", "i", "İ"]),
                search_path in proptest::collection::vec(
                    proptest::sample::select(&["main", "s", "S", "é"][..]).prop_map(String::from),
                    0..3,
                ),
            ) {
                let con = table(&rows);
                let reader = CatalogReader::from_rows(true, "memory", rows.clone());

                let view = crate::ident::ViewRef {
                    database: None,
                    schema: schema.clone(),
                    name: name.to_string(),
                };
                let candidates = sql_lookup(&con, &view.name, view.schema.as_deref());
                let expected = resolve_in_search_path(&view, &candidates, &search_path)
                    .map(|opt| opt.map(ToString::to_string));
                prop_assert_eq!(reader.lookup(&view, &search_path), expected);

                let expected_all: Vec<(String, String)> = sql_strings(
                    &con,
                    "SELECT name, definition FROM d ORDER BY schema_name, name",
                    2,
                )
                .into_iter()
                .map(|r| (r[0].clone(), r[1].clone()))
                .collect();
                prop_assert_eq!(reader.list_all().unwrap(), expected_all);

                let expected_names: Vec<String> =
                    sql_strings(&con, "SELECT DISTINCT name FROM d ORDER BY name", 1)
                        .into_iter()
                        .map(|r| r[0].clone())
                        .collect();
                prop_assert_eq!(reader.list_names().unwrap(), expected_names);
            }
        }
    }
}
