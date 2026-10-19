//! Unified `SHOW SEMANTIC {DIMENSIONS,METRICS,FACTS}` dispatchers (ST-2).
//!
//! Dimensions, metrics, and facts produce an identical 8-column VARCHAR row
//! shape and identical dispatch logic — they differ only in which slice of the
//! definition they iterate. This module collapses the previously
//! near-duplicate `show_dims.rs` / `show_metrics.rs` / `show_facts.rs` into a
//! single [`EntityRow`] + [`collect_entities`] over an [`EntityKind`], with the
//! six `#[no_mangle]` dispatchers reduced to one-line bodies over the shared
//! [`crate::ddl::read_ffi::run_dispatcher`] scaffold.
//!
//! 8 Snowflake-aligned columns: `database_name, schema_name,
//! semantic_view_name, table_name, name, data_type, synonyms, comment` — the
//! same list, in the same order, that Snowflake's `SHOW SEMANTIC …` commands
//! return.
//!
//! `data_type` is the type `semantic_view()` returns for the member, probed at
//! this bind on its connection by `query::member_types` (TECH-DEBT #51): one
//! probe per view for the kind being listed, falling back to one per member
//! when a batch cannot be expanded or bound. A member whose source no longer
//! binds reports an empty `data_type` (or a legacy definition's declared
//! `output_type`); the listing itself never fails on that account.
//!
//! Materializations (7 columns, different tail) and the two-arg
//! `dimensions_for_metric` variant keep their own modules; both route through
//! the same `run_dispatcher` scaffold.

#![cfg(feature = "extension")]

use crate::catalog::CatalogReader;
use crate::ddl::describe::format_json_array;
use crate::ddl::read_ffi::{
    member_types_for_bind, read_str_arg, run_dispatcher, serialize_varchar_rows,
};
use crate::model::SemanticViewDefinition;
use crate::query::member_types::{MemberTypes, Wanted};

/// Which entity slice of a definition a SHOW command reports on.
#[derive(Clone, Copy)]
enum EntityKind {
    Dimensions,
    Metrics,
    Facts,
}

impl EntityKind {
    /// The member kind whose types this listing needs probed.
    fn wanted(self) -> Wanted {
        match self {
            Self::Dimensions => Wanted::DIMENSIONS,
            Self::Metrics => Wanted::METRICS,
            Self::Facts => Wanted::FACTS,
        }
    }
}

/// Probe the reported types of `kind`'s members of `def` on this bind's
/// connection (TECH-DEBT #51). Errs only when the caller was cancelled, which
/// ends the whole listing rather than moving on to the next view.
fn probe_types(
    kind: EntityKind,
    view_name: &str,
    def: &SemanticViewDefinition,
) -> Result<MemberTypes, String> {
    member_types_for_bind(view_name, def, kind.wanted())
}

/// A single 8-column row of `SHOW SEMANTIC {DIMENSIONS,METRICS,FACTS}` output.
struct EntityRow {
    database_name: String,
    schema_name: String,
    semantic_view_name: String,
    table_name: String,
    name: String,
    data_type: String,
    synonyms: String,
    comment: String,
}

impl EntityRow {
    /// Flatten into the ordered VARCHAR cells for the wire format.
    fn into_cells(self) -> Vec<String> {
        vec![
            self.database_name,
            self.schema_name,
            self.semantic_view_name,
            self.table_name,
            self.name,
            self.data_type,
            self.synonyms,
            self.comment,
        ]
    }
}

/// Collect the entity rows of `kind` for a single already-parsed view.
/// `table_name` is resolved from each entity's `source_table` alias via the
/// view's alias map. Parsing (and the FF-9 decision of whether an unparseable
/// definition is a hard error or a skipped row) happens at the call site.
fn collect_entities(
    kind: EntityKind,
    view_name: &str,
    def: &SemanticViewDefinition,
    types: &MemberTypes,
) -> Vec<EntityRow> {
    let db_name = def.database_name.clone().unwrap_or_default();
    let sch_name = def.schema_name.clone().unwrap_or_default();
    let alias_map = def.alias_to_table_map();

    // Dimension / Metric / Fact all expose (name, source_table, synonyms,
    // comment); build one row from those plus the member's reported type.
    let build = |name: &str,
                 source_table: Option<&String>,
                 data_type: String,
                 synonyms: &[String],
                 comment: Option<&String>| {
        let table_name = source_table
            .and_then(|a| alias_map.get(a).cloned())
            .unwrap_or_default();
        EntityRow {
            database_name: db_name.clone(),
            schema_name: sch_name.clone(),
            semantic_view_name: view_name.to_string(),
            table_name,
            name: name.to_string(),
            data_type,
            synonyms: format_json_array(synonyms),
            comment: comment.cloned().unwrap_or_default(),
        }
    };

    match kind {
        EntityKind::Dimensions => def
            .dimensions
            .iter()
            .map(|d| {
                build(
                    &d.name,
                    d.source_table.as_ref(),
                    types.dimension(&d.name, d.output_type.as_ref()),
                    &d.synonyms,
                    d.comment.as_ref(),
                )
            })
            .collect(),
        EntityKind::Metrics => def
            .metrics
            .iter()
            .map(|m| {
                build(
                    &m.name,
                    m.source_table.as_ref(),
                    types.metric(&m.name, m.output_type.as_ref()),
                    &m.synonyms,
                    m.comment.as_ref(),
                )
            })
            .collect(),
        EntityKind::Facts => def
            .facts
            .iter()
            .map(|f| {
                build(
                    &f.name,
                    f.source_table.as_ref(),
                    types.fact(&f.name, f.output_type.as_ref()),
                    &f.synonyms,
                    f.comment.as_ref(),
                )
            })
            .collect(),
    }
}

/// Cross-view `_all` body: collect `kind` rows over every stored view, sorted
/// by `(semantic_view_name, name)`.
fn show_entities_all(kind: EntityKind) -> Result<Vec<u8>, String> {
    let reader = CatalogReader::for_caller()?;
    let entries = reader.list_all()?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    for (name, json) in &entries {
        // FF-9: the cross-view `_all` listing stays tolerant — a single view
        // whose stored JSON won't parse is skipped rather than failing the
        // whole listing. The named single-view path below is the strict one.
        let Ok(def) = SemanticViewDefinition::from_json(name, json) else {
            continue;
        };
        let types = probe_types(kind, name, &def)?;
        for r in collect_entities(kind, name, &def, &types) {
            rows.push(r.into_cells());
        }
    }
    rows.sort_by(|a, b| a[2].cmp(&b[2]).then_with(|| a[4].cmp(&b[4])));
    serialize_varchar_rows(&rows)
}

/// Single-view body: collect `kind` rows for one view, sorted by `name`.
/// A missing view is the canonical "does not exist" error.
fn show_entities_one(
    kind: EntityKind,
    view_name: &str,
    search_path: &[String],
) -> Result<Vec<u8>, String> {
    // FF-4: normalize the requested name so quoted-identifier inputs resolve
    // the same way they do through `semantic_view()` (folds to lowercase
    // whether quoted or not — DuckDB's case-insensitive identifier rule).
    let view = crate::ident::parse_view_ref(view_name)
        .map_err(|e| format!("Invalid view name '{view_name}': {e}"))?;
    let view_name = view.name.clone();
    let reader = CatalogReader::for_caller()?;
    let Some(json) = reader.lookup(&view, search_path)? else {
        return Err(crate::catalog::view_not_found_msg(&view.to_string()));
    };
    // FF-9: named single-view SHOW propagates a parse error — the user asked
    // for this specific view, so a corrupt definition must surface loudly
    // instead of silently returning zero rows.
    let def = SemanticViewDefinition::from_json(&view_name, &json)?;
    let types = probe_types(kind, &view_name, &def)?;
    let mut internal = collect_entities(kind, &view_name, &def, &types);
    internal.sort_by(|a, b| a.name.cmp(&b.name));
    let rows: Vec<Vec<String>> = internal.into_iter().map(EntityRow::into_cells).collect();
    serialize_varchar_rows(&rows)
}

// ---------------------------------------------------------------------------
// The six #[no_mangle] dispatchers. Symbol names + signatures are the ABI the
// C++ shim links against and MUST NOT change; only the bodies are now
// one-liners over the shared scaffold.
// ---------------------------------------------------------------------------

macro_rules! entity_dispatchers {
    ($kind:expr, $all_sym:ident, $one_sym:ident) => {
        /// # Safety
        /// The caller releases the returned buffer via `sv_free_buffer`.
        #[no_mangle]
        pub unsafe extern "C" fn $all_sym(
            out_ptr: *mut *mut u8,
            out_len: *mut usize,
            error_buf: *mut u8,
            error_buf_len: usize,
        ) -> u8 {
            run_dispatcher(
                out_ptr,
                out_len,
                error_buf,
                error_buf_len,
                stringify!($all_sym),
                || show_entities_all($kind),
            )
        }

        /// # Safety
        /// `name_ptr` must point to `name_len` UTF-8 bytes. The caller releases the returned buffer via
        /// `sv_free_buffer`.
        #[no_mangle]
        pub unsafe extern "C" fn $one_sym(
            name_ptr: *const u8,
            name_len: usize,
            sp_ptr: *const u8,
            sp_len: usize,
            out_ptr: *mut *mut u8,
            out_len: *mut usize,
            error_buf: *mut u8,
            error_buf_len: usize,
        ) -> u8 {
            run_dispatcher(
                out_ptr,
                out_len,
                error_buf,
                error_buf_len,
                stringify!($one_sym),
                || {
                    let view_name = unsafe { read_str_arg(name_ptr, name_len, "view name") }?;
                    let search_path =
                        unsafe { crate::ddl::read_ffi::read_search_path(sp_ptr, sp_len) }?;
                    show_entities_one($kind, &view_name, &search_path)
                },
            )
        }
    };
}

entity_dispatchers!(
    EntityKind::Dimensions,
    sv_show_semantic_dimensions_all_bind_rust,
    sv_show_semantic_dimensions_bind_rust
);
entity_dispatchers!(
    EntityKind::Metrics,
    sv_show_semantic_metrics_all_bind_rust,
    sv_show_semantic_metrics_bind_rust
);
entity_dispatchers!(
    EntityKind::Facts,
    sv_show_semantic_facts_all_bind_rust,
    sv_show_semantic_facts_bind_rust
);
