//! Serialized model of a stored semantic-view definition.
//!
//! # Wire-format freeze (C-11, code-review 2026-07-11)
//!
//! These structs ARE the on-disk format: every definition is persisted as JSON
//! in the catalog and re-emitted verbatim by YAML export, so the serde surface
//! is a compatibility contract, not an implementation detail. Two conventions
//! look inconsistent but are deliberate and frozen — do not "tidy" them, and
//! match them when adding fields:
//!
//! - **`source_table` and `output_type` serialize explicit `null`s** (they carry
//!   `#[serde(default)]` but *not* `skip_serializing_if`), whereas every sibling
//!   optional/collection field is omitted when empty. These two predate the
//!   `skip_serializing_if` convention; stored JSON in the wild already contains
//!   their explicit `null`s, so adding the skip now would change bytes for
//!   round-tripped definitions. New fields should use `skip_serializing_if`.
//! - **Enum variants persist in their Rust casing** (e.g. `AccessModifier`,
//!   `Cardinality`, `SortOrder`, `NullsOrder`) — no `#[serde(rename_all)]`. That
//!   casing is now baked into stored JSON and the YAML export, so renaming a
//!   variant or adding a rename attribute is a breaking format change.

use serde::{Deserialize, Serialize};

/// A table alias entry for the `tables` DDL parameter.
/// Maps a short alias (e.g., `"o"`) to a physical table name (e.g., `"orders"`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct TableRef {
    pub alias: String,
    pub table: String,
    /// Primary key columns for this table (Phase 24: PK/FK model).
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pk_columns: Vec<String>,
    /// UNIQUE constraints on this table. Each inner Vec is one constraint's column list.
    /// A table can have zero or more UNIQUE constraints (composite allowed).
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unique_constraints: Vec<Vec<String>>,
    /// Optional human-readable comment for this table entry.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Informational synonyms (aliases) for this table entry.
    /// Old stored JSON without this field deserializes to empty Vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
}

/// A named SQL column expression used as a dimension.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Dimension {
    pub name: String,
    pub expr: String,
    /// Optional source table — declares which join table this dimension comes from.
    /// If `None`, the dimension is assumed to come from the base table.
    #[serde(default)]
    pub source_table: Option<String>,
    /// Optional user-declared output type for this dimension column.
    /// When set, the generated SQL wraps the expression in `CAST(expr AS <type>)`
    /// AND declares the output column as this type in `bind()`.
    /// If None, the inferred or fallback type is used.
    #[serde(default)]
    pub output_type: Option<String>,
    /// Optional human-readable comment for this dimension.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Informational synonyms (aliases) for this dimension.
    /// Old stored JSON without this field deserializes to empty Vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    /// Marks this member as a **named filter** — Snowflake's `LABELS = (FILTER)`.
    ///
    /// A filter is an ordinary boolean-valued member that is intended to be
    /// referenced bare in a query's pre-aggregation predicate
    /// (`where_clause := 'is_domestic'`) rather than selected as output. The
    /// flag is metadata: resolution already substitutes any dimension/fact name
    /// appearing in the predicate, so this records *intent* and drives
    /// introspection (`DESCRIBE`, `SHOW`, `GET_DDL`).
    ///
    /// The BOOLEAN requirement is not checked here. We cannot evaluate the
    /// expression's type without a binder, so a non-boolean filter surfaces as
    /// `DuckDB`'s own type error at query time rather than a guess at define
    /// time.
    ///
    /// Old stored JSON without this field deserializes to `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_filter: bool,
}

/// Sort order for NON ADDITIVE BY dimension ordering.
/// Default: Asc (matches Snowflake default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum SortOrder {
    #[default]
    Asc,
    Desc,
}

impl SortOrder {
    /// Returns `true` when the variant is the default (`Asc`).
    /// Used by `serde(skip_serializing_if)` to omit the field from JSON
    /// when it matches the default, preserving backward-compatible output.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Asc)
    }
}

/// NULLS placement for NON ADDITIVE BY dimension ordering.
/// Default: Last (matches `DuckDB` ASC default and Snowflake ASC default).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum NullsOrder {
    #[default]
    Last,
    First,
}

impl NullsOrder {
    /// Returns `true` when the variant is the default (`Last`).
    /// Used by `serde(skip_serializing_if)` to omit the field from JSON
    /// when it matches the default, preserving backward-compatible output.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Last)
    }
}

/// A dimension reference in a NON ADDITIVE BY clause.
/// Specifies which dimension(s) a metric is non-additive by,
/// with sort order and nulls placement for snapshot selection.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct NonAdditiveDim {
    pub dimension: String,
    #[serde(default, skip_serializing_if = "SortOrder::is_default")]
    pub order: SortOrder,
    #[serde(default, skip_serializing_if = "NullsOrder::is_default")]
    pub nulls: NullsOrder,
}

/// Parsed window function specification for window metrics.
/// Stored alongside the raw expression for expansion-time rewriting.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct WindowSpec {
    /// The window function name (e.g., "AVG", "LAG", "SUM")
    pub window_function: String,
    /// The metric name referenced inside the window function
    pub inner_metric: String,
    /// Additional arguments after the inner metric (e.g., "30" in LAG(metric, 30))
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    /// Dimensions to EXCLUDE from partitioning (PARTITION BY EXCLUDING semantics)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluding_dims: Vec<String>,
    /// Explicit partition dimensions (PARTITION BY semantics, mutually exclusive with `excluding_dims`)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub partition_dims: Vec<String>,
    /// ORDER BY clause entries (dimension/expression + direction + nulls)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order_by: Vec<WindowOrderBy>,
    /// Raw frame clause (e.g., "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_clause: Option<String>,
}

/// An ORDER BY entry in a window function specification.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct WindowOrderBy {
    pub expr: String,
    #[serde(default, skip_serializing_if = "SortOrder::is_default")]
    pub order: SortOrder,
    #[serde(default, skip_serializing_if = "NullsOrder::is_default")]
    pub nulls: NullsOrder,
}

/// A named aggregation expression used as a metric.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Metric {
    pub name: String,
    pub expr: String,
    /// Optional source table — declares which join table this metric comes from.
    /// If `None`, the metric is assumed to come from the base table.
    #[serde(default)]
    pub source_table: Option<String>,
    /// Optional user-declared output type for this metric column.
    /// When set, the generated SQL wraps the expression in `CAST(expr AS <type>)`
    /// AND declares the output column as this type in `bind()`.
    /// If None, the inferred or fallback type is used.
    #[serde(default)]
    pub output_type: Option<String>,
    /// Phase 32: Named relationships that this metric traverses.
    /// When non-empty, the expansion engine uses these relationship names
    /// to resolve which join path to follow (role-playing dimensions).
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub using_relationships: Vec<String>,
    /// Optional human-readable comment for this metric.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Informational synonyms (aliases) for this metric.
    /// Old stored JSON without this field deserializes to empty Vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    /// Access modifier: PUBLIC (default, queryable) or PRIVATE (hidden from queries,
    /// usable only in derived metric expressions).
    /// Old stored JSON without this field deserializes as Public.
    #[serde(default, skip_serializing_if = "AccessModifier::is_default")]
    pub access: AccessModifier,
    /// Dimensions this metric is non-additive by (snapshot aggregation).
    /// When non-empty, expansion uses a `RANK`-based CTE for snapshot
    /// selection (rows tied at the snapshot value all aggregate).
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub non_additive_by: Vec<NonAdditiveDim>,
    /// Window function specification for window metrics.
    /// When Some, this metric uses a window function wrapping another metric.
    /// Old stored JSON without this field deserializes to None.
    /// Not serialized when None to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_spec: Option<WindowSpec>,
}

impl Metric {
    /// Returns true if this metric is a window function metric.
    #[must_use]
    pub fn is_window(&self) -> bool {
        self.window_spec.is_some()
    }
}

/// A named materialization declaration mapping a pre-aggregated table
/// to the dimensions and metrics it covers.
///
/// At define time, only the dimension/metric name references are validated
/// (must match declared names). The TABLE is not validated for existence
/// (it may be created later by external tools like dbt).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Materialization {
    /// User-assigned name for this materialization (e.g., `daily_revenue_by_region`).
    pub name: String,
    /// Fully qualified table name of the pre-aggregated table
    /// (e.g., `catalog.schema.daily_revenue_agg`).
    pub table: String,
    /// Dimension names covered by this materialization.
    /// Must be a subset of the semantic view's declared dimensions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dimensions: Vec<String>,
    /// Metric names covered by this materialization.
    /// Must be a subset of the semantic view's declared metrics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<String>,
}

/// A named raw SQL column expression — a pre-aggregation fact, scoped to a table alias.
/// Added in Phase 11 for the FACTS clause of CREATE SEMANTIC VIEW.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Fact {
    pub name: String,
    pub expr: String,
    /// Which table alias this fact is scoped to.
    #[serde(default)]
    pub source_table: Option<String>,
    /// Optional output type for this fact, used by SHOW FACTS `data_type` column.
    /// Populated at define time via type inference when possible.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default)]
    pub output_type: Option<String>,
    /// Optional human-readable comment for this fact.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// Informational synonyms (aliases) for this fact.
    /// Old stored JSON without this field deserializes to empty Vec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synonyms: Vec<String>,
    /// Marks this member as a **named filter** — Snowflake's `LABELS = (FILTER)`.
    ///
    /// A filter is an ordinary boolean-valued member that is intended to be
    /// referenced bare in a query's pre-aggregation predicate
    /// (`where_clause := 'is_domestic'`) rather than selected as output. The
    /// flag is metadata: resolution already substitutes any dimension/fact name
    /// appearing in the predicate, so this records *intent* and drives
    /// introspection (`DESCRIBE`, `SHOW`, `GET_DDL`).
    ///
    /// The BOOLEAN requirement is not checked here. We cannot evaluate the
    /// expression's type without a binder, so a non-boolean filter surfaces as
    /// `DuckDB`'s own type error at query time rather than a guess at define
    /// time.
    ///
    /// Old stored JSON without this field deserializes to `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_filter: bool,
    /// Access modifier: PUBLIC (default, queryable) or PRIVATE (hidden from queries,
    /// usable only in derived metric expressions).
    /// Old stored JSON without this field deserializes as Public.
    #[serde(default, skip_serializing_if = "AccessModifier::is_default")]
    pub access: AccessModifier,
}

/// Cardinality of a relationship between two tables.
///
/// Inferred from PK/UNIQUE constraints at define time (Phase 33).
/// `ManyToOne`: FK columns on the from-side table are bare (no PK/UNIQUE match).
/// `OneToOne`: FK columns on the from-side table match a PK or UNIQUE constraint.
/// Defaults to `ManyToOne` when deserialized from JSON without this field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum Cardinality {
    #[default]
    ManyToOne,
    OneToOne,
}

impl Cardinality {
    /// Returns `true` when the variant is the default (`ManyToOne`).
    /// Used by `serde(skip_serializing_if)` to omit the field from JSON
    /// when it matches the default, preserving backward-compatible output.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::ManyToOne)
    }
}

/// Access modifier for facts and metrics.
/// Default is Public -- private items cannot be queried directly
/// but can be referenced by derived metric expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub enum AccessModifier {
    #[default]
    Public,
    Private,
}

impl AccessModifier {
    /// Returns `true` when the variant is the default (`Public`).
    /// Used by `serde(skip_serializing_if)` to omit the field from JSON
    /// when it matches the default, preserving backward-compatible output.
    #[must_use]
    pub fn is_default(&self) -> bool {
        matches!(self, Self::Public)
    }
}

/// A JOIN relationship between the base table and another source table.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct Join {
    pub table: String,
    // AR-4 (PR-2): the pre-Phase-24 FK encodings `on` (raw SQL ON clause),
    // `from_cols`, and `join_columns` were removed here. They were
    // deserialize-only backward-compat shims with no runtime reads; current
    // DDL emits `from_alias` + `fk_columns`. Old stored JSON that still
    // carries those keys deserializes fine (serde ignores unknown fields),
    // and the AR-4 upgrade pass / fan-trap guard handle rows that lack the
    // current FK metadata.
    /// Phase 24: The source table alias from which FK columns are defined.
    /// In `order_to_customer AS o(customer_id) REFERENCES c`, this is `"o"`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from_alias: String,
    /// Phase 24: FK column names from the source alias (`from_alias`) side.
    /// In `order_to_customer AS o(customer_id) REFERENCES c`, this is `["customer_id"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fk_columns: Vec<String>,
    /// Phase 33: Resolved referenced columns on the target table.
    /// Populated during inference: either the target's PK or the explicit UNIQUE columns.
    /// Used by `synthesize_on_clause` to generate ON clause.
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ref_columns: Vec<String>,
    /// Phase 24: Optional relationship name for multi-table FK declarations.
    /// In `order_to_customer AS o(customer_id) REFERENCES c`, this is `Some("order_to_customer")`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Phase 31: Cardinality of this relationship.
    /// Defaults to `ManyToOne` when omitted in DDL (most common FK pattern).
    /// Old stored JSON without this field deserializes as `ManyToOne`.
    /// Not serialized when `ManyToOne` to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Cardinality::is_default")]
    pub cardinality: Cardinality,
}

/// Current storage-format version stamped into freshly written definitions
/// (AR-4). A `schema_version` key is injected into the stored JSON at write
/// time via the same `json_merge_patch` that records `created_on` etc.
/// (see `parse::native_sql::emit_native_create_sql`), so it is not a field
/// on [`SemanticViewDefinition`] — it never appears in YAML export and adds
/// no construction-site burden. Absent / `0` in stored JSON denotes a
/// pre-versioning ("legacy") row; the `init_catalog` upgrade pass stamps
/// completed rows up to this value. Bump when the stored JSON shape changes
/// in a way the upgrade pass must normalise.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Top-level definition of a semantic view.
///
/// Stored as JSON in `semantic_layer._definitions`.
/// Required fields: `tables`, `dimensions`, `metrics`.
/// Optional fields: `joins` (defaults to []), `facts` (defaults to []).
/// Note: `deny_unknown_fields` is intentionally NOT set — old stored JSON with extra
/// fields (e.g., from future schema changes) must still load without error.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "arbitrary", derive(arbitrary::Arbitrary))]
pub struct SemanticViewDefinition {
    /// Table alias registry for multi-table views.
    #[serde(default)]
    pub tables: Vec<TableRef>,
    pub dimensions: Vec<Dimension>,
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub joins: Vec<Join>,
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Named materializations mapping pre-aggregated tables to covered dims/metrics.
    /// Old stored JSON without this field deserializes with empty Vec.
    /// Not serialized when empty to preserve backward-compatible JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub materializations: Vec<Materialization>,
    // AR-4 (PR-2): the parallel DDL-time type-inference vectors
    // `column_type_names` / `column_types_inferred` were removed here. They
    // were never populated for post-v0.10.0 rows (D-16/D-17 deferred type
    // inference to read-side bind), so they were dead for new definitions;
    // legacy v0.7.1-era rows that still carry the keys now fall through to
    // the same read-side bind inference as new rows. Old JSON with these
    // keys deserializes fine (serde ignores unknown fields).
    /// ISO 8601 timestamp of when this semantic view was created.
    /// Captured at define time via `DuckDB` `now()`.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_on: Option<String>,
    /// Database name from the connection context at define time.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_name: Option<String>,
    /// Schema this semantic view lives in — the qualifier written on the
    /// CREATE name, or the creating session's schema when it was unqualified.
    /// Mirrors the `schema_name` **column** of the catalog table, which is what
    /// the `(schema_name, name)` primary key scopes the view by; the two are
    /// written together and kept in lockstep.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_name: Option<String>,
    /// Schema that unqualified table references in the body resolve against —
    /// the creating session's `current_schema()`, following `DuckDB`'s rule
    /// that a view body is resolved in the creating session's context rather
    /// than in the schema the view happens to live in.
    ///
    /// Separate from [`Self::schema_name`] only because the two can now differ:
    /// `CREATE SEMANTIC VIEW analytics.sales AS TABLES (o AS orders …)` issued
    /// from `main` puts the view in `analytics` while `orders` still means
    /// `main.orders`. Before views were schema-scoped there was one schema in
    /// play and `schema_name` served both roles, so rows written then have no
    /// value here and fall back to `schema_name` — exactly their old behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_schema_name: Option<String>,
    /// The `TABLES` entries and materialization tables, by unquoted name, that
    /// resolved to a temporary table in the creating session (written without a
    /// schema, or as `main.<name>`: `DuckDB` looks in `temp.main` first for
    /// both), so the expansion qualifies these with `temp.main` rather than with
    /// [`Self::database_name`] / [`Self::resolution_schema_name`]. Such a view
    /// can be queried in the session that has the table, and fails with
    /// "does not exist" in any other. Stamped at CREATE, like the fields
    /// above, and stripped from a YAML export with them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temp_tables: Vec<String>,
    /// View-level comment describing the purpose of this semantic view.
    /// Old stored JSON without this field deserializes to None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

impl SemanticViewDefinition {
    /// Physical table name of the primary (first) table in the view.
    ///
    /// All semantic views have at least one table entry. This is the table
    /// that appears in the FROM clause of expanded SQL.
    #[must_use]
    pub fn base_table(&self) -> &str {
        self.tables.first().map_or("", |t| t.table.as_str())
    }

    /// Build a mapping from table alias to actual table name.
    ///
    /// Used by SHOW/DESCRIBE `VTabs` to resolve the stored alias (e.g. `"o"`)
    /// to the real table name (e.g. `"orders"`).
    #[must_use]
    pub fn alias_to_table_map(&self) -> std::collections::HashMap<String, String> {
        self.tables
            .iter()
            .map(|t| (t.alias.clone(), t.table.clone()))
            .collect()
    }
}

impl SemanticViewDefinition {
    /// Parse and validate a JSON string, returning a typed definition.
    ///
    /// Returns an error if the JSON is invalid or missing required fields.
    ///
    /// The `name` parameter is used only in the error message for context.
    pub fn from_json(name: &str, json: &str) -> Result<Self, String> {
        let def: Self = serde_json::from_str(json)
            .map_err(|e| format!("invalid definition for semantic view '{name}': {e}"))?;
        Ok(def)
    }

    /// Read the `schema_version` recorded in a stored definition's JSON
    /// without fully deserializing it (AR-4).
    ///
    /// The version lives only in the stored JSON (injected at write time via
    /// `json_merge_patch`), not on this struct, so it is read with a minimal
    /// probe. Absent, non-integer, or unparseable JSON all map to `0` — the
    /// "legacy / pre-versioning" sentinel — so callers treat anything they
    /// cannot positively identify as current-format as legacy.
    #[must_use]
    pub fn stored_schema_version(json: &str) -> u32 {
        #[derive(Deserialize)]
        struct Probe {
            #[serde(default)]
            schema_version: u32,
        }
        serde_json::from_str::<Probe>(json).map_or(0, |p| p.schema_version)
    }

    /// True when any relationship lacks foreign-key column metadata
    /// (`fk_columns`) — a legacy (pre-Phase-24) encoding the graph/fan-trap
    /// machinery silently skips.
    ///
    /// Such a row cannot be verified for fan-trap safety (SG-7) and cannot be
    /// completed by the `init_catalog` upgrade pass, so it is treated as an
    /// un-upgradeable legacy definition that hard-errors on read.
    #[must_use]
    pub fn has_incomplete_relationships(&self) -> bool {
        self.joins.iter().any(|j| j.fk_columns.is_empty())
    }
}

impl SemanticViewDefinition {
    /// Maximum YAML input size (1 MiB). Sanity guard against oversized input.
    /// This is NOT a security boundary -- creating semantic views is a
    /// privileged operation guarded by warehouse auth. See trust assumption docs.
    pub const YAML_SIZE_CAP: usize = 1_048_576;

    /// Parse a YAML string into a typed semantic view definition.
    ///
    /// Returns an error if the YAML is syntactically invalid or missing
    /// required fields. The `name` parameter appears in the error message.
    pub fn from_yaml(name: &str, yaml: &str) -> Result<Self, String> {
        let def: Self = yaml_serde::from_str(yaml)
            .map_err(|e| format!("invalid YAML definition for semantic view '{name}': {e}"))?;
        Ok(def)
    }

    /// Parse YAML with a size cap check.
    ///
    /// Rejects input exceeding [`YAML_SIZE_CAP`] (1 MiB) before parsing.
    /// Returns an error including the actual size and the cap.
    pub fn from_yaml_with_size_cap(name: &str, yaml: &str) -> Result<Self, String> {
        if yaml.len() > Self::YAML_SIZE_CAP {
            return Err(format!(
                "YAML definition for semantic view '{name}' exceeds size limit \
                 ({} bytes > {} byte cap)",
                yaml.len(),
                Self::YAML_SIZE_CAP,
            ));
        }
        let def = Self::from_yaml(name, yaml)?;
        // RT-5 / RT-6: YAML is the only path into a definition that bypasses
        // the clause parsers, so the identifier rules they enforce are applied
        // here instead. Without this, GET_DDL can render a definition its own
        // parser rejects — or, worse, one that re-parses to a different model.
        validate_ddl_representable(&def)
            .map_err(|e| format!("invalid YAML definition for semantic view '{name}': {e}"))?;
        Ok(def)
    }
}

/// What EVERY identifier slot must satisfy: `identifier_slot_error`, plus the
/// two things that helper deliberately does not cover.
///
/// A slot may carry a SQL comment marker and survive every identifier check —
/// the front door blanks comments BEFORE parsing, so `PRIMARY KEY (a--b)` comes
/// back truncated to end-of-line. `identifier_slot_error` has no comment
/// awareness, so that is a separate check rather than part of it.
///
/// And a slot may be EMPTY: `identifier_slot_error` returns `None` for an
/// all-whitespace value on purpose, because its DDL call sites report emptiness
/// with a clause-specific "missing name/alias" message built from context it
/// does not have. Here there is no such call site, so the check belongs here.
fn slot_common(kind: &str, what: &str, value: &str) -> Result<(), String> {
    // Load-bearing, not defensive: a relationship named `"  "` renders as
    // `      AS o(cid) REFERENCES c`, which the RELATIONSHIPS parser rejects
    // with "Relationship name is required" (PR #209 review).
    if value.trim().is_empty() {
        return Err(format!("{kind} {what} is empty"));
    }
    if let Some(e) = crate::body_parser::identifier_slot_error(value) {
        return Err(format!("{kind} {what} '{value}' is invalid: {e}"));
    }
    if crate::util::blank_sql_comments(value) != value {
        return Err(format!(
            "{kind} {what} '{value}' contains a SQL comment marker"
        ));
    }
    Ok(())
}

/// A slot the grammar fills with exactly ONE identifier: an alias, a table, a
/// member name, a column.
///
/// A dot here re-parses as a qualifier and silently yields a DIFFERENT model —
/// `source_table: "a.b"` with `name: "region"` renders `a.b.region`, which
/// comes back as alias `a`, name `b.region`, with no error anywhere.
/// `identifier_slot_error` accepts it, because a qualified identifier is a
/// perfectly good identifier; it is the SLOT that admits only one part.
fn slot_single(kind: &str, what: &str, value: &str) -> Result<(), String> {
    slot_common(kind, what, value)?;
    match crate::ident::parse_qualified_identifier(value) {
        Ok(parts) if parts.len() == 1 => Ok(()),
        Ok(_) => Err(format!(
            "{kind} {what} '{value}' must be a single identifier, not a qualified one"
        )),
        Err(e) => Err(format!("{kind} {what} '{value}' is invalid: {e}")),
    }
}

/// A slot the grammar fills with ONE identifier that may legitimately be
/// dot-qualified — a relationship name, a materialization's physical table.
///
/// Both are emitted verbatim by the renderer and both are read back as a single
/// value, so `a.b` renders as `a.b` and re-parses as `a.b`: the same model, no
/// drift. The DDL parsers for these slots agree — RELATIONSHIPS validates its
/// name with `identifier_slot_error`, which accepts a qualified identifier, and
/// a materialization's TABLE is a *physical* table, where `schema.table` is the
/// ordinary spelling. Sending these through [`slot_single`] would have made
/// YAML refuse definitions the DDL path accepts and `GET_DDL` round-trips
/// unchanged (PR #209 review, both verified against the parser).
///
/// What [`slot_common`] still refuses is what actually breaks: an empty or
/// blank slot, a multi-token run (`my table`, `a DIMENSIONS`), and a comment
/// marker.
fn slot_qualified(kind: &str, what: &str, value: &str) -> Result<(), String> {
    slot_common(kind, what, value)
}

/// A slot that legitimately accepts `alias.name` as well as a bare name — a
/// member REFERENCE, per the D-08 dotted form.
fn slot_reference(kind: &str, what: &str, value: &str) -> Result<(), String> {
    slot_common(kind, what, value)?;
    match crate::ident::parse_qualified_identifier(value) {
        Ok(parts) if parts.len() <= 2 => Ok(()),
        Ok(_) => Err(format!(
            "{kind} {what} '{value}' has too many qualifier parts"
        )),
        Err(e) => Err(format!("{kind} {what} '{value}' is invalid: {e}")),
    }
}

/// One element of a `(a, b, …)` column list.
fn slot_column(kind: &str, what: &str, value: &str) -> Result<(), String> {
    slot_single(kind, what, value)?;
    if crate::body_parser::column_roundtrips_verbatim(value) {
        Ok(())
    } else {
        Err(format!(
            "{kind} {what} '{value}' does not round-trip as one column"
        ))
    }
}

fn validate_yaml_tables(def: &SemanticViewDefinition) -> Result<(), String> {
    for t in &def.tables {
        slot_single("table", "alias", &t.alias)?;
        // The physical table name goes out through `emit_table`, which quotes
        // anything that would not re-parse verbatim — a space, a depth-0 comma
        // and a `--` run all survive that way (each probed). The one value it
        // cannot protect is the empty string: `quote_ident("")` is `""`, which
        // the TABLES parser rejects as an empty quoted identifier. So this is
        // deliberately an emptiness check and not a `slot_*` call — the wider
        // check would reject names `GET_DDL` renders correctly (PR #209 review).
        if t.table.is_empty() {
            return Err(format!(
                "table '{}' has an empty table name; a TABLES entry is \
                 'alias AS table_name'",
                t.alias
            ));
        }
        for c in &t.pk_columns {
            slot_column("table", "PRIMARY KEY column", c)?;
        }
        for uc in &t.unique_constraints {
            for c in uc {
                slot_column("table", "UNIQUE column", c)?;
            }
        }
    }
    Ok(())
}

fn validate_yaml_joins(def: &SemanticViewDefinition) -> Result<(), String> {
    for j in &def.joins {
        let Some(name) = j.name.as_deref() else {
            return Err(format!(
                "relationship from '{}' to '{}' has no name; a relationship name is \
                 required (the DDL form is 'rel_name AS from_alias(fk_cols) REFERENCES to_alias')",
                j.from_alias, j.table
            ));
        };
        slot_qualified("relationship", "name", name)?;
        slot_single("relationship", "from_alias", &j.from_alias)?;
        slot_single("relationship", "table", &j.table)?;
        for c in &j.fk_columns {
            slot_column("relationship", "FK column", c)?;
        }
        for c in &j.ref_columns {
            slot_column("relationship", "REFERENCES column", c)?;
        }
    }
    Ok(())
}

/// Dimensions and facts must be qualified; metrics need not be.
///
/// Probed against the grammar rather than assumed: a metric with no
/// `source_table` is a DERIVED metric, and `profit AS revenue - cost` is
/// accepted unqualified. Sweeping metrics into the same rule would reject every
/// YAML-imported derived metric.
/// Every member is `name AS expr`, and the clause parsers reject an entry whose
/// expression is missing ("Missing expression after 'AS'"). A blank one renders
/// as exactly that — `e.p AS ` — so `GET_DDL` emits DDL it cannot read back.
///
/// Whitespace-only counts as blank: the parser trims before deciding, so
/// `e.p AS    ` fails the same way. Found by `fuzz_render_roundtrip`; bisecting
/// its crash cleared every other field in the definition (a NUL-bearing alias,
/// a table name full of spaces, a trailing-space member name — the renderer
/// quotes all of those and they round-trip).
///
/// Deliberately narrow: it is the *expression* that the grammar requires, not
/// the rest of a member's payload. Comments, synonyms and `output_type` were
/// probed with quotes, newlines, parens and empty values, and all round-trip,
/// so none of them is checked here.
/// What EVERY free-form SQL slot must satisfy — a member expression, a window
/// function's extra argument, a window frame clause.
///
/// Two rules, both of them about what the RENDERER can carry. Unlike an
/// identifier a free expression cannot be quote-protected on the way out: it is
/// emitted verbatim, so whatever structure it carries is structure the clause
/// parser will read.
///
/// 1. `column_roundtrips_verbatim` — balanced quotes and brackets that never
///    dip negative (an unbalanced `)` closes the enclosing clause early), each
///    closer of the same kind as its opener (the clause scan refuses `(]`), no
///    depth-0 comma (which would split one member or argument into two), and no
///    surrounding whitespace (which the parser trims, silently storing a
///    different string than the one given).
/// 2. No SQL comment marker (RT-8, code-review 2026-08-08). This is the rule
///    `slot_common` has always applied to identifier slots and that
///    `member_expr` was missing: `column_roundtrips_verbatim` has no comment
///    awareness (`crate::body_parser::scan` documents that itself), so
///    `o.a -- x` passed. The front door blanks comments BEFORE parsing, so on
///    replay everything from the `--` to end-of-line disappeared — including
///    the comma separating the member from the next one. Executed at 57e38fd:
///    two dimensions came back as ONE, the second's definition absorbed into
///    the first's expression, with no error anywhere.
///
/// Block comments are refused on the same rule rather than a narrower one.
/// `/* x */` inside a single member does not move a member boundary, but
/// blanking still replaces it with spaces, so the replayed expression is a
/// different string — drift, just quieter. `blank_sql_comments` is string- and
/// dollar-quote-aware, so a `--` inside a literal is not a comment and is not
/// refused (pinned by a control test).
fn free_expr_common(kind: &str, what: &str, expr: &str) -> Result<(), String> {
    if !crate::body_parser::column_roundtrips_verbatim(expr) {
        return Err(format!(
            "{kind} {what} the DDL grammar cannot carry ('{expr}'); an expression \
             must have balanced quotes and brackets, no top-level comma, and no \
             surrounding whitespace"
        ));
    }
    if crate::util::blank_sql_comments(expr) != expr {
        return Err(format!(
            "{kind} {what} containing a SQL comment marker ('{expr}'); comments are \
             blanked before the DDL body is parsed, so a comment inside an \
             expression silently deletes the rest of the line — including the \
             comma that ends the entry"
        ));
    }
    Ok(())
}

fn member_expr(kind: &str, name: &str, expr: &str) -> Result<(), String> {
    if expr.trim().is_empty() {
        return Err(format!(
            "{kind} '{name}' has an empty expression; a {kind} is '{name} AS <expr>'"
        ));
    }
    // An expression is free-form SQL, so unlike an identifier it CANNOT be
    // quote-protected on the way out — the renderer must emit it verbatim, and
    // whatever structure it carries is structure the clause parser will read.
    // The fuzz target's own header has always said as much ("a free-form `expr`
    // cannot be quote-protected"); what was missing was the matching
    // precondition, so an expression the grammar cannot carry looked like
    // render/parse drift instead of an unstorable definition.
    //
    // `column_roundtrips_verbatim` is exactly the rule that matters here, for
    // the same reason it matters for a column: balanced quotes and brackets
    // that never dip negative (an unbalanced `)` closes the CLAUSE early — the
    // shape the fuzzer found, `AS )   P ; rip`), no depth-0 comma (which would
    // split one member into two), and no surrounding whitespace (which the
    // parser trims, silently storing a different expression than the one given).
    free_expr_common(kind, &format!("'{name}' has an expression"), expr)?;
    // After `AS`, the keyword grammar ends the expression at the first
    // depth-0 `COMMENT`, `LABELS` or `WITH SYNONYMS` and reads the rest as
    // the entry's annotations, so an expression holding one renders to DDL
    // that no longer plans (fuzz_create_text, CI run 40184213171). Window
    // arguments are not checked: they sit inside `OVER (…)`, below depth 0.
    if let Some(at) = crate::body_parser::find_annotation_start(expr) {
        let keyword = expr[at..]
            .split(|c: char| !c.is_ascii_alphabetic())
            .next()
            .unwrap_or_default();
        return Err(format!(
            "{kind} '{name}' has an expression the DDL grammar cannot carry ('{expr}'); \
             '{keyword}' at depth 0 starts the entry's annotations. Quote it as an \
             identifier if it names a column"
        ));
    }
    Ok(())
}

/// `output_type` has NO DDL surface: no clause declares it and
/// [`crate::render_ddl`] has no emit site for it, so `GET_DDL` drops it
/// silently and the replayed view loses the `CAST(expr AS <type>)` the field
/// asks for (`expand/materialization.rs`, `expand/per_grain.rs`,
/// `expand/sql_gen.rs`, `expand/window.rs`, `expand/semi_additive.rs`) along
/// with the `data_type` every SHOW / DESCRIBE surface reports from it.
///
/// RT-7 (code-review 2026-08-08). Rejecting is the honest answer, and the
/// alternative was MEASURED rather than assumed: rendering the field as
/// `CAST(expr AS T)` inside the expression would ALSO change what referencing
/// members compute, because `expand::facts::inline_facts` and
/// `inline_derived_metrics` splice a member's RAW `expr` into its referrers and
/// never apply the cast (pinned by
/// `tests/yaml_proptest.rs::output_type_is_not_applied_when_a_member_is_inlined`).
/// So `CAST`-rendering would silently change numbers for every referrer — and
/// it would still not restore `output_type`, leaving this contract false.
///
/// Existing stored definitions are unaffected: they load through `from_json`,
/// which does not run this validator. Only new YAML ingest is refused, and the
/// message names the workaround that is exactly equivalent for the member
/// itself.
fn reject_output_type(kind: &str, name: &str, output_type: Option<&str>) -> Result<(), String> {
    if let Some(t) = output_type {
        return Err(format!(
            "{kind} '{name}' declares output_type '{t}', which no DDL clause can \
             express — GET_DDL would drop it and the restored view would lose the \
             cast. Write the cast into the expression instead \
             (expr: CAST(<expr> AS {t}))"
        ));
    }
    Ok(())
}

/// A `WindowSpec` is emitted RAW, field by field, by
/// `render_ddl::emit_window_expr`, and re-read by the OVER-clause parser. That
/// makes every one of its fields an RT-6 slot, and MODEL-1's model-changing
/// repro lived in the least identifier-like of them: a `frame_clause` of
/// `'ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) , junk AS ('` rendered DDL that
/// replayed to THREE metrics — one injected out of a frame-clause string.
///
/// Two layers, deliberately:
///
/// 1. Per-slot rules the round-trip cannot see. Comment markers survive a
///    render → parse round trip untouched (the parser does not blank comments;
///    the front door does, upstream), so they must be refused by name. The
///    identifier slots additionally get their usual emptiness / single-token
///    checks, which produce a message naming the offending value.
/// 2. The round trip itself: render the spec and re-parse it with the REAL
///    OVER-clause parser, then require the result to equal what was stored.
///    This is what makes the check complete rather than a growing list of
///    hazards — a partition dimension named `rows`, an extra argument carrying
///    a depth-0 comma and the frame-clause injection above are all caught by
///    the same comparison, and anything that genuinely round-trips is accepted.
fn validate_window_spec(metric_name: &str, ws: &WindowSpec) -> Result<(), String> {
    let kind = "metric";
    slot_qualified(
        kind,
        &format!("'{metric_name}' window function"),
        &ws.window_function,
    )?;
    slot_reference(
        kind,
        &format!("'{metric_name}' window inner metric"),
        &ws.inner_metric,
    )?;
    for arg in &ws.extra_args {
        free_expr_common(kind, &format!("'{metric_name}' has a window argument"), arg)?;
    }
    for dim in ws.excluding_dims.iter().chain(&ws.partition_dims) {
        slot_reference(
            kind,
            &format!("'{metric_name}' window PARTITION BY dimension"),
            dim,
        )?;
    }
    for ob in &ws.order_by {
        slot_reference(
            kind,
            &format!("'{metric_name}' window ORDER BY dimension"),
            &ob.expr,
        )?;
    }
    if let Some(ref frame) = ws.frame_clause {
        free_expr_common(
            kind,
            &format!("'{metric_name}' has a window frame clause"),
            frame,
        )?;
    }

    let rendered = crate::render_ddl::render_window_spec(ws);
    let replayed = crate::body_parser::reparse_window_expr(&rendered).map_err(|e| {
        format!(
            "metric '{metric_name}' has a window specification the DDL grammar \
             cannot carry: rendered as '{rendered}', which does not parse back ({})",
            e.message
        )
    })?;
    if replayed.as_ref() == Some(ws) {
        Ok(())
    } else {
        // Both specs go in the message. The rejections this fires on are
        // subtle by nature — a `frame_clause` that escapes its parentheses, a
        // partition dimension named `rows` that reads back as a frame — and
        // "reads back as something different" without saying *what* leaves the
        // author diffing two invisible values. `{:?}` on both is verbose but it
        // is the only thing that makes a whitespace or keyword-boundary case
        // diagnosable from the error alone.
        Err(format!(
            "metric '{metric_name}' has a window specification that does not \
             round-trip: it renders as '{rendered}', which reads back as a \
             DIFFERENT window specification.\n  stored:   {ws:?}\n  re-parsed: {replayed:?}"
        ))
    }
}

fn validate_yaml_members(def: &SemanticViewDefinition) -> Result<(), String> {
    for d in &def.dimensions {
        slot_single("dimension", "name", &d.name)?;
        member_expr("dimension", &d.name, &d.expr)?;
        reject_output_type("dimension", &d.name, d.output_type.as_deref())?;
        let Some(src) = d.source_table.as_deref() else {
            return Err(format!(
                "dimension '{}' has no source_table; a dimension must name its table \
                 (the DDL form is 'alias.name AS expr')",
                d.name
            ));
        };
        slot_single("dimension", "source_table", src)?;
    }
    for f in &def.facts {
        slot_single("fact", "name", &f.name)?;
        member_expr("fact", &f.name, &f.expr)?;
        reject_output_type("fact", &f.name, f.output_type.as_deref())?;
        let Some(src) = f.source_table.as_deref() else {
            return Err(format!(
                "fact '{}' has no source_table; a fact must name its table \
                 (the DDL form is 'alias.name AS expr')",
                f.name
            ));
        };
        slot_single("fact", "source_table", src)?;
    }
    for m in &def.metrics {
        slot_single("metric", "name", &m.name)?;
        member_expr("metric", &m.name, &m.expr)?;
        reject_output_type("metric", &m.name, m.output_type.as_deref())?;
        if let Some(ref ws) = m.window_spec {
            validate_window_spec(&m.name, ws)?;
        }
        if let Some(src) = m.source_table.as_deref() {
            slot_single("metric", "source_table", src)?;
        } else {
            // A metric with no source table is a DERIVED metric, and
            // `body_parser/metrics.rs` refuses three clauses on that form —
            // each with its own error. YAML could set all three anyway, and
            // `GET_DDL` then rendered e.g. `ZZZZ NON ADDITIVE BY (v) AS ZZZZ`,
            // which this project's own parser rejects. Found by
            // `fuzz_render_roundtrip`; the three are listed together because
            // they are one rule in the grammar ("only qualified metrics may
            // use these"), and fixing them one at a time would just mean three
            // more red CI runs.
            for (present, clause) in [
                (!m.using_relationships.is_empty(), "USING"),
                (!m.non_additive_by.is_empty(), "NON ADDITIVE BY"),
                (m.window_spec.is_some(), "OVER"),
            ] {
                if present {
                    return Err(format!(
                        "metric '{}' has no source_table, making it a derived metric, \
                         but carries a {clause} clause; only a qualified metric \
                         ('alias.name AS expr') may use {clause}",
                        m.name
                    ));
                }
            }
        }
        for r in &m.using_relationships {
            slot_reference("metric", "USING relationship", r)?;
        }
        for na in &m.non_additive_by {
            slot_reference("metric", "NON ADDITIVE BY dimension", &na.dimension)?;
        }
    }
    Ok(())
}

fn validate_yaml_materializations(def: &SemanticViewDefinition) -> Result<(), String> {
    for mat in &def.materializations {
        slot_single("materialization", "name", &mat.name)?;
        slot_qualified("materialization", "table", &mat.table)?;
        for d in &mat.dimensions {
            slot_reference("materialization", "dimension reference", d)?;
        }
        for m in &mat.metrics {
            slot_reference("materialization", "metric reference", m)?;
        }
        // MODEL-1: `parse_materializations_clause` requires at least one of the
        // two sub-clauses, and renders nothing for an empty list — so a
        // materialization with neither renders `MATERIALIZATIONS ( m AS TABLE t
        // )`, which its own parser refuses. Kept here rather than in
        // `cross_refs` because it is a clause-STRUCTURE rule (the parser
        // enforces it while reading one entry, not by resolving names).
        if mat.dimensions.is_empty() && mat.metrics.is_empty() {
            return Err(format!(
                "Materialization '{}': must specify at least one of DIMENSIONS or METRICS.",
                mat.name
            ));
        }
    }
    Ok(())
}

/// Reject a definition whose CLAUSE STRUCTURE the DDL grammar could not have
/// produced, independently of what its identifier slots contain.
///
/// `find_clause_bounds` requires TABLES, and requires at least one of
/// DIMENSIONS / METRICS. A definition with neither renders a lone TABLES clause
/// — a body its own parser refuses with "At least one of 'DIMENSIONS' or
/// 'METRICS' is required." — so `GET_DDL` emits DDL that cannot be replayed.
///
/// Found by `fuzz_render_roundtrip` within minutes of RT-5 removing that
/// target's parse-fail escape, which is the point: the escape had been
/// classifying exactly this finding as uninteresting input.
fn validate_yaml_structure(def: &SemanticViewDefinition) -> Result<(), String> {
    // TECH-DEBT #94 (Snowflake parity): a view needs at least one table, as
    // `TABLES ()` is refused in the DDL. This also refuses the legacy stored
    // format (an empty `tables`), which `render_create_ddl` declines to render.
    if def.tables.is_empty() {
        return Err(
            "definition has no tables; a semantic view needs at least one table".to_string(),
        );
    }
    if def.dimensions.is_empty() && def.metrics.is_empty() {
        return Err(
            "definition has neither dimensions nor metrics; at least one of the two is \
             required (the DDL grammar requires a DIMENSIONS or a METRICS clause)"
                .to_string(),
        );
    }
    Ok(())
}

/// Reject a definition the DDL grammar could not have produced — in its
/// identifier slots or in its clause structure (RT-5 / RT-6, code-review
/// 2026-08-06).
///
/// This is the reachability rule for a stored definition: the DDL path enforces
/// it through the clause grammar, and YAML — the one surface that reaches
/// `SemanticViewDefinition` without going through the clause parsers — enforces
/// it by calling this. `fuzz_render_roundtrip` uses it as its PRECONDITION, so
/// that a parse failure after it is a genuine render-contract break rather than
/// something to shrug at.
///
/// YAML is the one surface that reaches `SemanticViewDefinition` without going
/// through the clause parsers, so it was the one surface with no
/// identifier-syntax validation at all. `GET_DDL` then rendered those slots
/// verbatim, and the results ranged from bad to silent:
///
/// - a join with no `name` rendered `     AS o(customer_id) REFERENCES c`, DDL
///   this project's own parser rejects;
/// - a dimension with no `source_table` rendered `region AS c.region`, likewise
///   rejected;
/// - `source_table: "a.b"` rendered `a.b.region AS …`, which re-parses cleanly
///   as source table `a` and name `b.region` — a DIFFERENT model, no error;
/// - `pk_columns: ["a--b"]` rendered `PRIMARY KEY (a--b)`, which the front
///   door's comment-blanking pre-pass truncates to end-of-line.
pub fn validate_ddl_representable(def: &SemanticViewDefinition) -> Result<(), String> {
    validate_yaml_structure(def)?;
    validate_yaml_tables(def)?;
    validate_yaml_joins(def)?;
    validate_yaml_members(def)?;
    validate_yaml_materializations(def)?;
    // MODEL-1 (code-review 2026-08-08): the semantic cross-reference checks —
    // NON ADDITIVE BY, a window metric's dimensions and inner metric,
    // materialization names and references — lived only in
    // `parse_keyword_body`, so YAML skipped every one of them. They are the
    // same function now, so a definition that imports is one whose GET_DDL its
    // own parser accepts.
    crate::body_parser::cross_refs::validate_cross_references(
        &def.dimensions,
        &def.metrics,
        &def.materializations,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // RT-5 / RT-6 (code-review 2026-08-06): YAML import performed NO
    // identifier-syntax validation, while the DDL path validates every slot.
    // So YAML could produce definitions the DDL grammar cannot express — and
    // `GET_DDL` then rendered them back as DDL its own parser rejects, or,
    // worse, as DDL that re-parses to a DIFFERENT model.
    //
    // Verified against the renderer before the fix:
    //   - a join with no `name`      -> `     AS o(customer_id) REFERENCES c`
    //   - a dimension with no `source_table` -> `region AS c.region`
    //   - `source_table: "a.b"`      -> `a.b.region AS c.region`, which
    //     re-parses as source_table `a`, name `b.region` — silently a
    //     different model, no error anywhere.
    //
    // The grammar's actual requirements, probed rather than assumed:
    // dimensions and facts REQUIRE a source table; metrics do NOT (a derived
    // metric is legitimately unqualified); relationships REQUIRE a name.

    fn yaml_err(yaml: &str) -> String {
        SemanticViewDefinition::from_yaml_with_size_cap("v", yaml)
            .expect_err("YAML the DDL grammar could not express must be rejected")
    }

    const VALID_TABLES: &str = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n";

    #[test]
    fn yaml_join_without_a_name_is_rejected() {
        let yaml = format!(
            "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n  \
             - alias: c\n    table: customers\n    pk_columns: [id]\n\
             joins:\n  - from_alias: o\n    table: c\n    fk_columns: [customer_id]\n\
             dimensions:\n  - name: region\n    expr: c.region\n    source_table: c\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("relationship") && e.contains("name"),
            "the error must name the missing relationship name: {e}"
        );
    }

    #[test]
    fn yaml_dimension_without_a_source_table_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: region\n    expr: o.region\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("dimension"), "{e}");
        assert!(
            e.contains("source_table") || e.contains("source table"),
            "{e}"
        );
    }

    #[test]
    fn yaml_fact_without_a_source_table_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}facts:\n  - name: net\n    expr: o.a * 2\n\
             dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("fact"), "{e}");
    }

    #[test]
    fn yaml_dotted_source_table_is_rejected() {
        // The silent one: `a.b` + `region` renders `a.b.region`, which
        // re-parses as alias `a`, name `b.region`.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: region\n    expr: o.region\n    source_table: a.b\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("a.b"),
            "the error must quote the offending slot: {e}"
        );
    }

    #[test]
    fn yaml_member_name_with_a_space_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: my dim\n    expr: o.region\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("my dim"), "{e}");
    }

    #[test]
    fn yaml_pk_column_containing_a_comment_marker_is_rejected() {
        // `a--b` passes the render-side column predicate (QuoteState has no
        // comment awareness) and renders `PRIMARY KEY (a--b)`, which the front
        // door's comment-blanking pre-pass then truncates to end-of-line.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [a--b]\n\
                    dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        let e = yaml_err(yaml);
        assert!(e.contains("a--b"), "{e}");
    }

    // --- PR #209 review + fuzz_render_roundtrip, 2026-08-07 ---
    //
    // Removing the fuzz oracle's parse-fail escape immediately paid: the target
    // shrank to a definition with TABLES and nothing else, which
    // `validate_ddl_representable` waved through and `GET_DDL` then rendered as
    // a body its own parser rejects. Three more shapes came out of probing the
    // same boundary by hand, two of them in the opposite direction — slots this
    // validator refused that the DDL grammar accepts and that round-trip
    // unchanged.

    /// TECH-DEBT #94 (Snowflake parity): a view needs at least one table,
    /// so an empty `tables` is rejected at CREATE like `TABLES ()`, even with
    /// members (corpus row S-358).
    #[test]
    fn yaml_with_no_tables_is_rejected() {
        let e = yaml_err("base_table: t\ntables: []\ndimensions: []\nmetrics: []\n");
        assert!(e.contains("at least one table"), "got: {e}");
    }

    #[test]
    fn yaml_with_no_tables_is_rejected_even_with_members() {
        let e = yaml_err(
            "base_table: t\ntables: []\n\
             dimensions:\n  - name: region\n    expr: t.region\n    source_table: t\nmetrics: []\n",
        );
        assert!(e.contains("at least one table"), "got: {e}");
    }

    #[test]
    fn yaml_with_neither_dimensions_nor_metrics_is_rejected() {
        // `clause_bounds.rs`: TABLES is required and so is at least one of
        // DIMENSIONS / METRICS. Rendered, this definition is a lone TABLES
        // clause -- "At least one of 'DIMENSIONS' or 'METRICS' is required."
        let yaml = format!("{VALID_TABLES}dimensions: []\nmetrics: []\n");
        let e = yaml_err(&yaml);
        assert!(
            e.contains("dimension") && e.contains("metric"),
            "the error must name the required clauses: {e}"
        );
    }

    #[test]
    fn the_shape_ci_fuzzing_found_is_covered_by_the_oracles_precondition() {
        // `fuzz_render_roundtrip` skips only what `validate_ddl_representable`
        // rejects, so the target stays red until this definition is one of
        // them. Reconstructed field-for-field from the crash artifact
        // (`fuzz/seeds/fuzz_render_roundtrip/regression_tables_only_no_members.txt`,
        // byte-identical to the one CI uploaded): TABLES and nothing else.
        //
        // Asserted on a directly-constructed definition rather than through
        // YAML because that is how the fuzz target builds one — the YAML route
        // is covered separately by
        // `yaml_with_neither_dimensions_nor_metrics_is_rejected`.
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "emantic".to_string(),
                table: " view".to_string(),
                comment: Some("nder roundtrip seed: ".to_string()),
                synonyms: vec!["e".to_string(), String::new(), String::new()],
                ..Default::default()
            }],
            ..Default::default()
        };
        let e = validate_ddl_representable(&def)
            .expect_err("the definition CI's fuzzer found must not pass the precondition");
        assert!(
            e.contains("dimension") && e.contains("metric"),
            "it fails for the reason the parser fails on it -- a missing \
             DIMENSIONS/METRICS clause, not its table name: {e}"
        );
    }

    // Round two of the same fuzz oracle (2026-08-07). The shrunk definition
    // rendered `e.p  AS ` -- a metric with an EMPTY expression. Bisecting the
    // crash field-by-field cleared every other suspect in it: a NUL-bearing
    // alias, a table name full of spaces, and a trailing-space member name all
    // round-trip (the renderer quotes them). Only the blank expression breaks,
    // and it breaks identically for all three member kinds, so all three are
    // pinned here rather than just the one the fuzzer happened to reach.
    //
    // Every other payload field that reaches the renderer was probed in the
    // same batch and is fine: comments containing quotes / newlines / nothing,
    // synonyms containing quotes / parens / nothing, and `output_type` empty,
    // spacey or malformed.

    #[test]
    fn yaml_derived_metric_carrying_a_qualified_only_clause_is_rejected() {
        // `body_parser/metrics.rs` refuses USING / NON ADDITIVE BY / OVER on an
        // unqualified metric. All three are checked here because they are one
        // rule in the grammar; the fuzzer only ever reached NON ADDITIVE BY.
        for (field, clause) in [
            ("using_relationships: [r]", "USING"),
            ("non_additive_by:\n      - dimension: d", "NON ADDITIVE BY"),
        ] {
            let yaml = format!(
                "{VALID_TABLES}dimensions: []\n\
                 metrics:\n  - name: derived\n    expr: base * 2\n    {field}\n"
            );
            let e = yaml_err(&yaml);
            assert!(
                e.contains("derived") && e.contains(clause),
                "the error must name the metric and the offending clause ({clause}): {e}"
            );
        }
    }

    #[test]
    fn yaml_member_expression_the_grammar_cannot_carry_is_rejected() {
        // An expression is free-form SQL and CANNOT be quote-protected on the
        // way out, so the precondition has to refuse the shapes that would
        // re-parse as something else. The unbalanced `)` is the one the fuzzer
        // found: it closes the DIMENSIONS clause early.
        for expr in [
            "')'",        // fine -- inside a string literal (control, below)
            ")  P ; rip", // unbalanced close paren: ends the clause
            "a, b",       // depth-0 comma: splits one member into two
            "sum(o.a) ",  // trailing space: the parser trims, storing a different expr
            "sum((o.a)",  // unbalanced open paren: swallows the rest of the clause
        ]
        .iter()
        .skip(1)
        {
            let yaml = format!(
                "{VALID_TABLES}dimensions: []\n\
                 metrics:\n  - name: total\n    expr: '{}'\n    source_table: o\n",
                expr.replace('\'', "''")
            );
            let e = yaml_err(&yaml);
            assert!(
                e.contains("total"),
                "the error must name the offending metric (expr {expr:?}): {e}"
            );
        }
    }

    #[test]
    fn yaml_expression_with_punctuation_inside_a_literal_still_imports() {
        // Control: parens and commas inside a string literal are inert, and a
        // nested call is ordinary. Neither may be swept up by the rule above.
        let yaml = format!(
            "{VALID_TABLES}dimensions: []\n\
             metrics:\n  - name: total\n    expr: \"coalesce(sum(o.amount), 0) || ')'\"\n    source_table: o\n"
        );
        SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
            .expect("balanced nesting and quoted punctuation are ordinary SQL");
    }

    #[test]
    fn yaml_metric_with_an_empty_expression_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions: []\n\
             metrics:\n  - name: total\n    expr: ''\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("metric") && e.contains("total"), "{e}");
    }

    #[test]
    fn yaml_dimension_with_an_empty_expression_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: region\n    expr: ''\n    source_table: o\n\
             metrics: []\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("dimension") && e.contains("region"), "{e}");
    }

    #[test]
    fn yaml_fact_with_a_blank_expression_is_rejected() {
        // Whitespace-only, not empty: the parser trims before deciding, so
        // `f AS    ` is the same "Missing expression after 'AS'" as `f AS `.
        let yaml = format!(
            "{VALID_TABLES}facts:\n  - name: net\n    expr: '   '\n    source_table: o\n\
             dimensions: []\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("fact") && e.contains("net"), "{e}");
    }

    #[test]
    fn yaml_member_payloads_that_do_round_trip_still_import() {
        // Control for the three above: the renderer handles quotes, newlines
        // and parens in comments and synonyms, so neither may be swept up by an
        // over-broad "payload must be tame" rule.
        //
        // `output_type: 'DECIMAL(10, 2)'` used to be asserted here too, under a
        // comment claiming types "round-trip". They never did — RT-7
        // (code-review 2026-08-08) found that `render_ddl` has no emit site for
        // the field at all, so `GET_DDL` DROPS it and the replayed view loses
        // its casts. The probe behind the old assertion had checked that the
        // value survives YAML import, not that it survives the round trip, so a
        // passing test read as coverage of something it never touched. The
        // field is now refused at import (`reject_output_type`); the three
        // `yaml_output_type_on_a_*_is_rejected` tests own that behaviour.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
                    \x20   comment: \"it's a metric\"\n\
                    \x20   synonyms: [\"a)\", \"it's\"]\n";
        SemanticViewDefinition::from_yaml_with_size_cap("v", yaml)
            .expect("comments and synonyms with punctuation all round-trip");
    }

    #[test]
    fn yaml_empty_physical_table_name_is_rejected() {
        // The one table-name value `emit_table` cannot protect: everything
        // else that would not re-parse verbatim gets quoted (a space, a comma,
        // even `--`), but `quote_ident("")` is `""`, which the TABLES parser
        // rejects as an empty quoted identifier.
        let yaml = "tables:\n  - alias: o\n    table: ''\ndimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        let e = yaml_err(yaml);
        assert!(e.contains("table"), "{e}");
    }

    #[test]
    fn yaml_blank_relationship_name_is_rejected() {
        // `identifier_slot_error` returns `None` for an all-whitespace slot --
        // its DDL call sites report emptiness themselves, from context this
        // validator does not have. Rendered, a blank name yields
        // `      AS o(cid) REFERENCES c`.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n  \
                    - alias: c\n    table: customers\n    pk_columns: [id]\n\
                    joins:\n  - name: '  '\n    from_alias: o\n    table: c\n    fk_columns: [cid]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        let e = yaml_err(yaml);
        assert!(e.contains("relationship"), "{e}");
    }

    #[test]
    fn yaml_dotted_relationship_name_still_imports() {
        // The RELATIONSHIPS parser validates the name with
        // `identifier_slot_error`, which accepts a qualified identifier, and
        // the renderer emits it verbatim -- `a.b` re-parses to `a.b`. Refusing
        // it here would reject YAML the DDL path accepts.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n  \
                    - alias: c\n    table: customers\n    pk_columns: [id]\n\
                    joins:\n  - name: a.b\n    from_alias: o\n    table: c\n    fk_columns: [cid]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        SemanticViewDefinition::from_yaml_with_size_cap("v", yaml)
            .expect("a dot-qualified relationship name is what the DDL parser accepts");
    }

    #[test]
    fn yaml_qualified_materialization_table_still_imports() {
        // A materialization's TABLE is a PHYSICAL table, so `schema.table` is
        // the ordinary spelling, not a hostile one -- and it round-trips.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
                    materializations:\n  - name: m\n    table: sch.mt\n    metrics: [total]\n";
        SemanticViewDefinition::from_yaml_with_size_cap("v", yaml)
            .expect("a schema-qualified materialization table is a legal physical table name");
    }

    #[test]
    fn yaml_multi_token_materialization_table_is_still_rejected() {
        // Control for the two relaxations above: `mat.table` is emitted RAW,
        // so a multi-token value really does break -- `TABLE my table,` comes
        // back as "duplicate TABLE sub-clause".
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
                    materializations:\n  - name: m\n    table: my table\n    metrics: [total]\n";
        let e = yaml_err(yaml);
        assert!(e.contains("my table"), "{e}");
    }

    #[test]
    fn yaml_multi_token_relationship_name_is_still_rejected() {
        // Control: relaxing to "may be qualified" must not relax to "anything".
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n  \
                    - alias: c\n    table: customers\n    pk_columns: [id]\n\
                    joins:\n  - name: x y\n    from_alias: o\n    table: c\n    fk_columns: [cid]\n\
                    dimensions: []\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        let e = yaml_err(yaml);
        assert!(e.contains("x y"), "{e}");
    }

    /// Controls: the legitimate shapes must still import. Without these,
    /// "validate YAML" could degenerate into "reject YAML".
    #[test]
    fn yaml_valid_definition_still_imports() {
        let yaml = format!(
            "tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n  \
             - alias: c\n    table: customers\n    pk_columns: [id]\n\
             joins:\n  - name: o_to_c\n    from_alias: o\n    table: c\n    fk_columns: [customer_id]\n\
             facts:\n  - name: net\n    expr: o.a * 2\n    source_table: o\n\
             dimensions:\n  - name: region\n    expr: c.region\n    source_table: c\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
            .expect("a well-formed YAML definition must import");
    }

    #[test]
    fn yaml_derived_metric_without_a_source_table_still_imports() {
        // A metric with no source_table is a DERIVED metric — the grammar
        // accepts `profit AS revenue - cost` unqualified, so this must not be
        // swept up by the dimension/fact rule.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: derived\n    expr: base * 2\n"
        );
        SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
            .expect("a derived metric is legitimately unqualified");
    }

    #[test]
    fn yaml_quoted_identifier_slots_still_import() {
        // Quoting is a legal spelling, not a hostile one.
        let yaml = "tables:\n  - alias: o\n    table: orders\n    pk_columns: ['\"my id\"']\n\
                    dimensions:\n  - name: '\"my dim\"'\n    expr: o.d\n    source_table: o\n\
                    metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n";
        SemanticViewDefinition::from_yaml_with_size_cap("v", yaml)
            .expect("a well-formed quoted identifier is a legal slot value");
    }

    // -----------------------------------------------------------------------
    // Code review 2026-08-08 §6 — the YAML-ingress class RT-5/RT-6 did not
    // reach. RT-5/RT-6 enumerated identifier SLOTS; every shape below sits in
    // a field that is not a slot. Each was executed at 57e38fd: it passed the
    // full YAML gate, and replaying its own `GET_DDL` through the front door
    // then produced a DIFFERENT model, or none at all.
    // -----------------------------------------------------------------------

    // --- RT-8: comment markers in a free-form member expression ---
    //
    // `member_expr` checked `column_roundtrips_verbatim`, which has no comment
    // awareness (`model.rs` documents that itself), while `slot_common` — the
    // rule for every identifier slot — rejects comment markers outright. So a
    // `--` in a YAML expression passed, and the front door, which blanks
    // comments BEFORE parsing, then blanked from the `--` through the comma
    // that separates one member from the next: two dimensions came back as
    // ONE, with the second's definition absorbed into the first's expression.
    // Silent model corruption through the documented dump/restore path.

    // --- Brackets that don't nest by kind (#286) ---
    //
    // The DDL parser refuses a clause whose brackets don't pair up by kind, so
    // an expression like `SUM(o.a[1)]`, whose `(`/`)` and all-kinds counts both
    // balance, would import and then render a GET_DDL that CREATE refuses.

    #[test]
    fn yaml_metric_expression_with_brackets_of_the_wrong_kind_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: 'SUM(o.a[1)]'\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("total") && e.contains("balanced quotes and brackets"),
            "the error must name the member and the bracket rule: {e}"
        );
    }

    #[test]
    fn yaml_dimension_expression_with_a_line_comment_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d1\n    expr: 'o.a -- x'\n    source_table: o\n  \
             - name: d2\n    expr: o.b\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("d1") && e.contains("comment"),
            "the error must name the member and say a comment marker is the problem: {e}"
        );
    }

    #[test]
    fn yaml_metric_expression_with_a_line_comment_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: 'sum(o.amount) -- oops'\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("total") && e.contains("comment"),
            "the error must name the metric and the comment marker: {e}"
        );
    }

    #[test]
    fn yaml_fact_expression_with_a_block_comment_is_rejected() {
        // Block comments are refused on the same rule rather than a narrower
        // one: `blank_sql_comments` replaces them with spaces, so the replayed
        // expression is a DIFFERENT string even when no member boundary moves.
        let yaml = format!(
            "{VALID_TABLES}facts:\n  - name: net\n    expr: 'o.a /* x */ * 2'\n    source_table: o\n\
             dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("net") && e.contains("comment"),
            "the error must name the fact and the comment marker: {e}"
        );
    }

    #[test]
    fn yaml_expression_with_a_double_dash_inside_a_string_literal_still_imports() {
        // Control for the three above. `blank_sql_comments` is string-aware, so
        // a `--` inside a literal is not a comment and the expression survives
        // the front door unchanged. A rule that rejected it would refuse valid
        // SQL for no round-trip gain.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: \"o.a || '--'\"\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
            .expect("a double dash inside a string literal is not a comment");
    }

    // --- An annotation keyword at depth 0 in a member expression ---
    //
    // After `AS`, the keyword grammar ends the expression at the first depth-0
    // `COMMENT`, `LABELS` or `WITH SYNONYMS`, so a YAML expression holding one
    // rendered a GET_DDL that CREATE refuses (fuzz_create_text).

    #[test]
    fn yaml_expression_with_a_depth0_annotation_keyword_is_rejected() {
        for (expr, keyword) in [
            ("o.a COMMENT 'x'", "COMMENT"),
            ("o.a labels", "labels"),
            ("o.a WITH SYNONYMS", "WITH"),
        ] {
            let yaml = format!(
                "{VALID_TABLES}dimensions:\n  - name: d\n    expr: \"{expr}\"\n    source_table: o\n\
                 metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
            );
            let e = yaml_err(&yaml);
            assert!(
                e.contains("'d'") && e.contains(&format!("'{keyword}' at depth 0")),
                "the error must name the member and the keyword: {e}"
            );
        }
    }

    #[test]
    fn yaml_expression_with_an_annotation_word_below_depth0_still_imports() {
        // Controls: the keyword grammar splits none of these, so neither may
        // the YAML rule.
        for expr in [
            "o.comment",
            "o.\\\"comment\\\"",
            "coalesce(o.a, comment)",
            "o.a || ' COMMENT '",
            "o.comment_count",
            "o.a WITH_X",
        ] {
            let yaml = format!(
                "{VALID_TABLES}dimensions:\n  - name: d\n    expr: \"{expr}\"\n    source_table: o\n\
                 metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
            );
            SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
                .unwrap_or_else(|e| panic!("{expr} must import: {e}"));
        }
    }

    // --- MODEL-1: semantic cross-references ---
    //
    // These checks lived ONLY in `parse_keyword_body`, so the YAML path never
    // ran them. Each definition below is YAML-valid and renders `GET_DDL` that
    // this project's own parser rejects.

    #[test]
    fn yaml_non_additive_by_naming_an_undeclared_dimension_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             \x20   non_additive_by:\n      - dimension: ghost\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_window_inner_metric_naming_an_undeclared_metric_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(ghost) OVER ()\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: ghost\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_window_partition_dim_naming_an_undeclared_dimension_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER (PARTITION BY ghost)\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: base\n\
             \x20     partition_dims: [ghost]\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_window_order_by_naming_an_undeclared_dimension_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER (ORDER BY ghost)\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: base\n\
             \x20     order_by:\n        - expr: ghost\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_materialization_naming_an_undeclared_dimension_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             materializations:\n  - name: m\n    table: mt\n    dimensions: [ghost]\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_materialization_naming_an_undeclared_metric_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             materializations:\n  - name: m\n    table: mt\n    metrics: [ghost]\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("ghost"), "{e}");
    }

    #[test]
    fn yaml_duplicate_materialization_names_are_rejected() {
        // Case-folded: `m1` and `M1` are one name to the parser, which refuses
        // the pair. `graph/names.rs` folds MEMBER names the same way; the
        // materialization-name gap is this one.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             materializations:\n  - name: m1\n    table: mt\n    metrics: [total]\n  \
             - name: M1\n    table: mt2\n    metrics: [total]\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.to_ascii_lowercase().contains("duplicate"),
            "the error must say the names collide: {e}"
        );
    }

    #[test]
    fn yaml_materialization_with_no_members_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             materializations:\n  - name: m\n    table: mt\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("DIMENSIONS") && e.contains("METRICS"),
            "the error must name the required sub-clauses: {e}"
        );
    }

    // --- MODEL-1, syntactic half: the whole `WindowSpec` payload is emitted
    // RAW by `emit_window_expr`, so it is the RT-6 slot class, not the
    // cross-reference class TECH-DEBT #60 consciously deferred.

    #[test]
    fn yaml_window_frame_clause_that_escapes_its_parens_is_rejected() {
        // The model-CHANGING one. Rendered, this frame clause closes the OVER
        // parenthesis and opens a new METRICS entry: replay parsed THREE
        // metrics where the definition declared two — a metric injected out of
        // a frame-clause string.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER ()\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: base\n\
             \x20     frame_clause: 'ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) , junk AS ('\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("'w'") && e.contains("junk"),
            "the error must name the metric and quote the offending frame clause: {e}"
        );
    }

    #[test]
    fn yaml_window_frame_clause_with_a_comment_is_rejected() {
        // Renders inside the OVER parens, where the front door's comment
        // blanking erases it and everything after it on the line — including
        // the `)` that closes OVER.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER ()\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: base\n\
             \x20     frame_clause: 'ROWS UNBOUNDED PRECEDING -- x'\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("'w'") && e.contains("comment"),
            "the error must name the metric and the comment marker: {e}"
        );
    }

    #[test]
    fn yaml_window_partition_dim_named_like_a_frame_keyword_is_rejected() {
        // Silent: `PARTITION BY rows` re-parses with an EMPTY dim list and
        // `frame_clause: Some("rows")` — the partition key disappears and a
        // frame appears, with no error anywhere.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: rows\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER (PARTITION BY rows)\n    source_table: o\n\
             \x20   window_spec:\n      window_function: AVG\n      inner_metric: base\n\
             \x20     partition_dims: [rows]\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("'w'") && e.contains("round-trip"),
            "the error must name the metric and say the spec does not round-trip: {e}"
        );
    }

    #[test]
    fn yaml_window_extra_arg_with_a_top_level_comma_is_rejected() {
        // `extra_args` are joined with `, ` inside the function-call parens, so
        // one argument carrying a depth-0 comma comes back as two.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: LAG(base, 1) OVER ()\n    source_table: o\n\
             \x20   window_spec:\n      window_function: LAG\n      inner_metric: base\n\
             \x20     extra_args: ['1, 2']\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("'w'") && e.contains("1, 2"),
            "the error must name the metric and the offending argument: {e}"
        );
    }

    #[test]
    fn yaml_window_function_that_is_not_an_identifier_is_rejected() {
        // `window_function` is emitted verbatim immediately before the argument
        // parens; anything that is not a plain identifier chain there re-reads
        // as something else.
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: AVG(base) OVER ()\n    source_table: o\n\
             \x20   window_spec:\n      window_function: '1 + AVG'\n      inner_metric: base\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("1 + AVG"), "{e}");
    }

    #[test]
    fn yaml_valid_window_metric_still_imports() {
        // Control for the six window rejections: an ordinary window metric —
        // partition, order, frame, extra argument — must still import, or
        // "validate the window spec" has degenerated into "refuse it".
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: base\n    expr: sum(o.amount)\n    source_table: o\n  \
             - name: w\n    expr: x\n    source_table: o\n\
             \x20   window_spec:\n      window_function: LAG\n      inner_metric: base\n\
             \x20     extra_args: ['1']\n      partition_dims: [d]\n\
             \x20     order_by:\n        - expr: d\n\
             \x20     frame_clause: 'ROWS BETWEEN 1 PRECEDING AND CURRENT ROW'\n"
        );
        SemanticViewDefinition::from_yaml_with_size_cap("v", &yaml)
            .expect("an ordinary window metric must import");
    }

    // --- RT-7: `output_type` has no DDL surface at all ---

    #[test]
    fn yaml_output_type_on_a_dimension_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             \x20   output_type: 'VARCHAR(10)'\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(
            e.contains("output_type") && e.contains("CAST"),
            "the error must name the field and point at the workaround: {e}"
        );
    }

    #[test]
    fn yaml_output_type_on_a_metric_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n\
             \x20   output_type: DOUBLE\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("output_type") && e.contains("total"), "{e}");
    }

    #[test]
    fn yaml_output_type_on_a_fact_is_rejected() {
        let yaml = format!(
            "{VALID_TABLES}facts:\n  - name: net\n    expr: o.a\n    source_table: o\n\
             \x20   output_type: 'DECIMAL(10, 2)'\n\
             dimensions:\n  - name: d\n    expr: o.d\n    source_table: o\n\
             metrics:\n  - name: total\n    expr: sum(o.amount)\n    source_table: o\n"
        );
        let e = yaml_err(&yaml);
        assert!(e.contains("output_type") && e.contains("net"), "{e}");
    }

    // --- AR-4: schema_version probe + incomplete-relationship detection ---

    #[test]
    fn stored_schema_version_reads_injected_value() {
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(r#"{"schema_version":1}"#),
            1
        );
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(r#"{"schema_version":7,"tables":[]}"#),
            7
        );
    }

    #[test]
    fn stored_schema_version_absent_or_bad_is_zero() {
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(r#"{"tables":[]}"#),
            0
        );
        assert_eq!(SemanticViewDefinition::stored_schema_version("not json"), 0);
        assert_eq!(
            SemanticViewDefinition::stored_schema_version(r#"{"schema_version":"x"}"#),
            0
        );
    }

    #[test]
    fn has_incomplete_relationships_detects_empty_fk() {
        let mut def = SemanticViewDefinition::default();
        assert!(!def.has_incomplete_relationships(), "no joins is complete");
        def.joins.push(Join {
            table: "c".into(),
            from_alias: "o".into(),
            fk_columns: vec!["cid".into()],
            ..Default::default()
        });
        assert!(
            !def.has_incomplete_relationships(),
            "join with fk_columns is complete"
        );
        def.joins.push(Join {
            table: "x".into(),
            ..Default::default()
        });
        assert!(
            def.has_incomplete_relationships(),
            "join without fk_columns is incomplete"
        );
    }

    #[test]
    fn valid_definition_roundtrips() {
        let json = r#"{
            "tables": [{"alias": "o", "table": "orders"}],
            "dimensions": [{"name": "region", "expr": "region"}],
            "metrics": [{"name": "revenue", "expr": "sum(amount)"}]
        }"#;
        let def = SemanticViewDefinition::from_json("orders", json).unwrap();
        assert_eq!(def.base_table(), "orders");
        assert_eq!(def.dimensions.len(), 1);
        assert_eq!(def.metrics.len(), 1);
        assert!(def.joins.is_empty());
    }

    #[test]
    fn invalid_json_is_error() {
        assert!(SemanticViewDefinition::from_json("test", "{not json}").is_err());
    }

    #[test]
    fn optional_fields_default_to_empty() {
        let json = r#"{"base_table": "t", "dimensions": [], "metrics": []}"#;
        let def = SemanticViewDefinition::from_json("test", json).unwrap();
        assert!(def.joins.is_empty());
    }

    #[test]
    fn old_json_without_source_table_deserializes() {
        // Backward compat: Phase 2 definitions don't have source_table.
        let json = r#"{
            "base_table": "orders",
            "dimensions": [{"name": "region", "expr": "region"}],
            "metrics": [{"name": "revenue", "expr": "sum(amount)"}]
        }"#;
        let def = SemanticViewDefinition::from_json("orders", json).unwrap();
        assert!(def.dimensions[0].source_table.is_none());
        assert!(def.metrics[0].source_table.is_none());
    }

    #[test]
    fn json_with_source_table_deserializes() {
        let json = r#"{
            "base_table": "orders",
            "dimensions": [{"name": "customer_name", "expr": "customers.name", "source_table": "customers"}],
            "metrics": [{"name": "revenue", "expr": "sum(amount)", "source_table": "line_items"}]
        }"#;
        let def = SemanticViewDefinition::from_json("orders", json).unwrap();
        assert_eq!(def.dimensions[0].source_table.as_deref(), Some("customers"));
        assert_eq!(def.metrics[0].source_table.as_deref(), Some("line_items"));
    }

    mod phase11_model_tests {
        use super::*;

        #[test]
        fn fact_roundtrip() {
            // Fact with source_table
            let json = r#"{"name":"rev","expr":"amount","source_table":"orders"}"#;
            let fact: Fact = serde_json::from_str(json).unwrap();
            assert_eq!(fact.name, "rev");
            assert_eq!(fact.expr, "amount");
            assert_eq!(fact.source_table.as_deref(), Some("orders"));

            // Fact without source_table — defaults to None
            let json2 = r#"{"name":"total","expr":"price * qty"}"#;
            let fact2: Fact = serde_json::from_str(json2).unwrap();
            assert_eq!(fact2.name, "total");
            assert!(fact2.source_table.is_none());
        }

        #[test]
        fn legacy_join_fk_encodings_are_ignored_on_load() {
            // AR-4 (PR-2): the removed `on` / `from_cols` / `join_columns`
            // keys must still deserialize — serde ignores unknown fields, so
            // an old stored Join loads carrying only the current fields.
            let json = r#"{"table":"customers","on":"a.id=b.id","from_cols":["cid"],
                "join_columns":[{"from":"cid","to":"id"}],"fk_columns":["cid"],"from_alias":"o"}"#;
            let join: Join = serde_json::from_str(json).unwrap();
            assert_eq!(join.table, "customers");
            assert_eq!(join.from_alias, "o");
            assert_eq!(join.fk_columns, vec!["cid"]);
        }

        #[test]
        fn definition_with_facts() {
            let json = r#"{
                "base_table": "orders",
                "dimensions": [],
                "metrics": [],
                "facts": [{"name":"unit_price","expr":"amount / qty","source_table":"orders"}]
            }"#;
            let def = SemanticViewDefinition::from_json("orders", json).unwrap();
            assert_eq!(def.facts.len(), 1);
            assert_eq!(def.facts[0].name, "unit_price");
            assert_eq!(def.facts[0].expr, "amount / qty");
            assert_eq!(def.facts[0].source_table.as_deref(), Some("orders"));
        }

        #[test]
        fn definition_without_facts_defaults_empty() {
            let json = r#"{"base_table":"orders","dimensions":[],"metrics":[]}"#;
            let def = SemanticViewDefinition::from_json("orders", json).unwrap();
            assert!(def.facts.is_empty(), "facts should default to []");
        }

        #[test]
        fn unknown_fields_are_allowed() {
            // deny_unknown_fields removed — old stored JSON with extra fields must load
            let json = r#"{"base_table": "t", "dimensions": [], "metrics": [], "extra": 1}"#;
            assert!(
                SemanticViewDefinition::from_json("test", json).is_ok(),
                "unknown fields must not cause rejection after deny_unknown_fields removal"
            );
        }
    }

    mod phase11_1_model_tests {
        use super::*;

        #[test]
        fn table_ref_roundtrip() {
            let tr = TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                ..Default::default()
            };
            let json = serde_json::to_string(&tr).unwrap();
            assert_eq!(json, r#"{"alias":"o","table":"orders"}"#);
            let rt: TableRef = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.alias, "o");
            assert_eq!(rt.table, "orders");
        }

        #[test]
        fn semantic_view_definition_with_tables_roundtrip() {
            let def = SemanticViewDefinition {
                tables: vec![TableRef {
                    alias: "o".to_string(),
                    table: "orders".to_string(),
                    ..Default::default()
                }],
                dimensions: vec![],
                metrics: vec![],

                joins: vec![],
                facts: vec![],
                materializations: vec![],

                created_on: None,
                database_name: None,
                schema_name: None,
                resolution_schema_name: None,
                temp_tables: vec![],
                comment: None,
            };
            let json = serde_json::to_string(&def).unwrap();
            assert!(
                json.contains(r#""tables":[{"alias":"o","table":"orders"}]"#),
                "tables field must appear in serialized JSON: {json}"
            );
            let rt: SemanticViewDefinition = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.tables.len(), 1);
            assert_eq!(rt.tables[0].alias, "o");
            assert_eq!(rt.tables[0].table, "orders");
        }

        #[test]
        fn old_definition_without_tables_deserializes_with_empty_vec() {
            // Old stored JSON without `tables` field — must load with tables: []
            let json = r#"{"base_table":"orders","dimensions":[],"metrics":[]}"#;
            let def: SemanticViewDefinition = serde_json::from_str(json).unwrap();
            assert!(
                def.tables.is_empty(),
                "tables should default to [] for old JSON without tables field"
            );
        }
    }

    mod phase31_cardinality_tests {
        use super::*;

        #[test]
        fn cardinality_serde_roundtrip() {
            // Both variants serialize and deserialize correctly
            for (variant, expected_json) in [
                (Cardinality::ManyToOne, r#""ManyToOne""#),
                (Cardinality::OneToOne, r#""OneToOne""#),
            ] {
                let json = serde_json::to_string(&variant).unwrap();
                assert_eq!(json, expected_json);
                let rt: Cardinality = serde_json::from_str(&json).unwrap();
                assert_eq!(rt, variant);
            }
        }

        #[test]
        fn join_with_cardinality_roundtrip() {
            let join = Join {
                table: "customers".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                name: Some("order_to_customer".to_string()),
                cardinality: Cardinality::OneToOne,
                ..Default::default()
            };
            let json = serde_json::to_string(&join).unwrap();
            assert!(json.contains(r#""cardinality":"OneToOne""#));
            let rt: Join = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.cardinality, Cardinality::OneToOne);
        }

        #[test]
        fn old_json_without_cardinality_defaults_to_many_to_one() {
            // Backward compat: old JSON without cardinality field
            let json = r#"{"table":"customers","on":"a.id=b.id"}"#;
            let join: Join = serde_json::from_str(json).unwrap();
            assert_eq!(
                join.cardinality,
                Cardinality::ManyToOne,
                "Missing cardinality must default to ManyToOne"
            );
        }

        #[test]
        fn old_json_with_one_to_many_is_rejected() {
            // Phase 33: OneToMany variant removed -- old JSON with it must fail
            let result = serde_json::from_str::<Cardinality>(r#""OneToMany""#);
            assert!(
                result.is_err(),
                "OneToMany should be an unknown variant after Phase 33"
            );
        }

        #[test]
        fn definition_with_cardinality_joins_roundtrips() {
            let def = SemanticViewDefinition {
                dimensions: vec![],
                metrics: vec![],
                joins: vec![Join {
                    table: "c".to_string(),
                    from_alias: "o".to_string(),
                    fk_columns: vec!["customer_id".to_string()],
                    name: Some("order_to_customer".to_string()),
                    cardinality: Cardinality::OneToOne,
                    ..Default::default()
                }],
                ..Default::default()
            };
            let json = serde_json::to_string(&def).unwrap();
            let rt = SemanticViewDefinition::from_json("orders", &json).unwrap();
            assert_eq!(rt.joins.len(), 1);
            assert_eq!(rt.joins[0].cardinality, Cardinality::OneToOne);
        }
    }

    mod phase32_using_relationships_tests {
        use super::*;

        #[test]
        fn metric_with_using_relationships_roundtrips() {
            let met = Metric {
                name: "departure_count".to_string(),
                expr: "COUNT(*)".to_string(),
                source_table: Some("f".to_string()),
                output_type: None,
                using_relationships: vec!["dep_airport".to_string()],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(json.contains("using_relationships"));
            let rt: Metric = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.using_relationships, vec!["dep_airport"]);
        }

        #[test]
        fn old_json_without_using_relationships_deserializes_with_empty_vec() {
            // Backward compat: Phase 30 definitions don't have using_relationships
            let json = r#"{"name":"revenue","expr":"SUM(amount)","source_table":"o"}"#;
            let met: Metric = serde_json::from_str(json).unwrap();
            assert!(
                met.using_relationships.is_empty(),
                "using_relationships should default to [] for old JSON"
            );
        }

        #[test]
        fn metric_with_empty_using_relationships_does_not_emit_field() {
            // skip_serializing_if = "Vec::is_empty" means no using_relationships key in output
            let met = Metric {
                name: "revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                source_table: Some("o".to_string()),
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                !json.contains("using_relationships"),
                "Empty using_relationships should be omitted from JSON: {json}"
            );
        }
    }

    mod phase12_model_tests {
        use super::*;

        #[test]
        fn output_type_on_dimension_roundtrips() {
            let dim = Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: None,
                output_type: Some("BIGINT".to_string()),
                comment: None,
                synonyms: vec![],
                is_filter: false,
            };
            let json = serde_json::to_string(&dim).unwrap();
            let rt: Dimension = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.output_type.as_deref(), Some("BIGINT"));
        }

        #[test]
        fn output_type_on_metric_roundtrips() {
            let met = Metric {
                name: "revenue".to_string(),
                expr: "sum(amount)".to_string(),
                source_table: None,
                output_type: Some("DOUBLE".to_string()),
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
            };
            let json = serde_json::to_string(&met).unwrap();
            let rt: Metric = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.output_type.as_deref(), Some("DOUBLE"));
        }

        #[test]
        fn old_json_without_output_type_deserializes() {
            // Old JSON without output_type field — must deserialize to None
            let json = r#"{
                "base_table": "orders",
                "dimensions": [{"name": "region", "expr": "region"}],
                "metrics": [{"name": "revenue", "expr": "sum(amount)"}]
            }"#;
            let def = SemanticViewDefinition::from_json("orders", json).unwrap();
            assert!(
                def.dimensions[0].output_type.is_none(),
                "output_type should default to None"
            );
            assert!(
                def.metrics[0].output_type.is_none(),
                "output_type should default to None"
            );
        }

        #[test]
        fn old_json_with_removed_type_inference_vecs_still_deserializes() {
            // AR-4 (PR-2): the removed column_type_names / column_types_inferred
            // keys in a legacy row must be ignored, not rejected.
            let json = r#"{"base_table": "orders", "dimensions": [], "metrics": [],
                "column_type_names": ["region"], "column_types_inferred": [17]}"#;
            assert!(
                SemanticViewDefinition::from_json("orders", json).is_ok(),
                "legacy type-inference keys must be ignored on load"
            );
        }
    }

    mod phase33_model_tests {
        use super::*;

        #[test]
        fn table_ref_with_unique_constraints_roundtrip() {
            let tr = TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![
                    vec!["email".to_string()],
                    vec!["first_name".to_string(), "last_name".to_string()],
                ],
                comment: None,
                synonyms: vec![],
            };
            let json = serde_json::to_string(&tr).unwrap();
            assert!(json.contains("unique_constraints"));
            let rt: TableRef = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.unique_constraints.len(), 2);
            assert_eq!(rt.unique_constraints[0], vec!["email"]);
            assert_eq!(rt.unique_constraints[1], vec!["first_name", "last_name"]);
        }

        #[test]
        fn old_json_without_unique_constraints_deserializes() {
            // Backward compat: old JSON without unique_constraints field
            let json = r#"{"alias":"o","table":"orders","pk_columns":["id"]}"#;
            let tr: TableRef = serde_json::from_str(json).unwrap();
            assert!(
                tr.unique_constraints.is_empty(),
                "unique_constraints should default to [] for old JSON"
            );
        }

        #[test]
        fn join_with_ref_columns_roundtrip() {
            let join = Join {
                table: "c".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec!["id".to_string()],
                name: Some("o_to_c".to_string()),
                ..Default::default()
            };
            let json = serde_json::to_string(&join).unwrap();
            assert!(json.contains("ref_columns"));
            let rt: Join = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.ref_columns, vec!["id"]);
        }

        #[test]
        fn old_json_without_ref_columns_deserializes() {
            // Backward compat: old JSON without ref_columns field
            let json = r#"{"table":"customers","on":"a.id=b.id"}"#;
            let join: Join = serde_json::from_str(json).unwrap();
            assert!(
                join.ref_columns.is_empty(),
                "ref_columns should default to [] for old JSON"
            );
        }

        #[test]
        fn empty_unique_constraints_not_serialized() {
            let tr = TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
            };
            let json = serde_json::to_string(&tr).unwrap();
            assert!(
                !json.contains("unique_constraints"),
                "Empty unique_constraints should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn empty_ref_columns_not_serialized() {
            let join = Join {
                table: "c".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec![],
                ..Default::default()
            };
            let json = serde_json::to_string(&join).unwrap();
            assert!(
                !json.contains("ref_columns"),
                "Empty ref_columns should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn table_ref_without_pk_is_valid() {
            let tr = TableRef {
                alias: "f".to_string(),
                table: "fact_table".to_string(),
                pk_columns: vec![],
                unique_constraints: vec![],
                comment: None,
                synonyms: vec![],
            };
            assert_eq!(tr.alias, "f");
            assert_eq!(tr.table, "fact_table");
            assert!(tr.pk_columns.is_empty());
            // Roundtrip through JSON
            let json = serde_json::to_string(&tr).unwrap();
            let rt: TableRef = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.alias, "f");
            assert!(rt.pk_columns.is_empty());
        }
    }

    mod phase39_metadata_tests {
        use super::*;

        #[test]
        fn created_on_roundtrip() {
            let def = SemanticViewDefinition {
                dimensions: vec![],
                metrics: vec![],
                created_on: Some("2026-04-01T12:00:00Z".to_string()),
                database_name: Some("mydb".to_string()),
                schema_name: Some("main".to_string()),
                resolution_schema_name: Some("main".to_string()),
                temp_tables: vec![],
                ..Default::default()
            };
            let json = serde_json::to_string(&def).unwrap();
            let rt = SemanticViewDefinition::from_json("orders", &json).unwrap();
            assert_eq!(rt.created_on.as_deref(), Some("2026-04-01T12:00:00Z"));
            assert_eq!(rt.database_name.as_deref(), Some("mydb"));
            assert_eq!(rt.schema_name.as_deref(), Some("main"));
        }

        #[test]
        fn old_json_without_metadata_fields_deserializes() {
            let json = r#"{"base_table":"orders","dimensions":[],"metrics":[]}"#;
            let def = SemanticViewDefinition::from_json("orders", json).unwrap();
            assert!(
                def.created_on.is_none(),
                "created_on should default to None"
            );
            assert!(
                def.database_name.is_none(),
                "database_name should default to None"
            );
            assert!(
                def.schema_name.is_none(),
                "schema_name should default to None"
            );
        }

        #[test]
        fn fact_output_type_roundtrip() {
            let fact = Fact {
                name: "rev".to_string(),
                expr: "amount".to_string(),
                source_table: Some("orders".to_string()),
                output_type: Some("DECIMAL(10,2)".to_string()),
                comment: None,
                synonyms: vec![],
                is_filter: false,
                access: AccessModifier::Public,
            };
            let json = serde_json::to_string(&fact).unwrap();
            let rt: Fact = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.output_type.as_deref(), Some("DECIMAL(10,2)"));
        }

        #[test]
        fn old_fact_json_without_output_type_deserializes() {
            let json = r#"{"name":"rev","expr":"amount","source_table":"orders"}"#;
            let fact: Fact = serde_json::from_str(json).unwrap();
            assert!(
                fact.output_type.is_none(),
                "output_type should default to None"
            );
        }
    }

    mod phase43_metadata_tests {
        use super::*;

        #[test]
        fn access_modifier_default_is_public() {
            assert_eq!(AccessModifier::default(), AccessModifier::Public);
        }

        #[test]
        fn access_modifier_is_default() {
            assert!(AccessModifier::Public.is_default());
            assert!(!AccessModifier::Private.is_default());
        }

        #[test]
        fn pre_v060_json_deserializes_with_defaults() {
            // Full v0.5.5 JSON blob with NO comment/synonyms/access fields
            let json = r#"{
                "base_table": "orders",
                "tables": [{"alias": "o", "table": "orders", "pk_columns": ["id"]}],
                "dimensions": [{"name": "region", "expr": "region", "source_table": "o"}],
                "metrics": [{"name": "revenue", "expr": "SUM(amount)", "source_table": "o", "using_relationships": ["rel1"]}],
                "facts": [{"name": "unit_price", "expr": "price / qty", "source_table": "o", "output_type": "DOUBLE"}],
                "joins": [{"table": "c", "from_alias": "o", "fk_columns": ["customer_id"]}],
                "column_type_names": ["region", "revenue"],
                "column_types_inferred": [17, 20],
                "created_on": "2026-04-01T12:00:00Z",
                "database_name": "mydb",
                "schema_name": "main"
            }"#;
            let def = SemanticViewDefinition::from_json("orders", json).unwrap();

            // View-level comment
            assert!(def.comment.is_none(), "view comment should default to None");

            // Table metadata
            assert!(
                def.tables[0].comment.is_none(),
                "table comment should default to None"
            );
            assert!(
                def.tables[0].synonyms.is_empty(),
                "table synonyms should default to []"
            );

            // Dimension metadata
            assert!(
                def.dimensions[0].comment.is_none(),
                "dim comment should default to None"
            );
            assert!(
                def.dimensions[0].synonyms.is_empty(),
                "dim synonyms should default to []"
            );

            // Metric metadata
            assert!(
                def.metrics[0].comment.is_none(),
                "metric comment should default to None"
            );
            assert!(
                def.metrics[0].synonyms.is_empty(),
                "metric synonyms should default to []"
            );
            assert_eq!(
                def.metrics[0].access,
                AccessModifier::Public,
                "metric access should default to Public"
            );

            // Fact metadata
            assert!(
                def.facts[0].comment.is_none(),
                "fact comment should default to None"
            );
            assert!(
                def.facts[0].synonyms.is_empty(),
                "fact synonyms should default to []"
            );
            assert_eq!(
                def.facts[0].access,
                AccessModifier::Public,
                "fact access should default to Public"
            );
        }

        #[test]
        fn metric_with_access_private_roundtrips() {
            let met = Metric {
                name: "internal_rev".to_string(),
                expr: "SUM(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Private,
                non_additive_by: vec![],
                window_spec: None,
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                json.contains(r#""access":"Private""#),
                "Private access must appear in JSON: {json}"
            );
            let rt: Metric = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.access, AccessModifier::Private);
        }

        #[test]
        fn metric_with_access_public_omits_field() {
            let met = Metric {
                name: "revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                source_table: None,
                output_type: None,
                using_relationships: vec![],
                comment: None,
                synonyms: vec![],
                access: AccessModifier::Public,
                non_additive_by: vec![],
                window_spec: None,
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                !json.contains("access"),
                "Public access (default) should be omitted from JSON: {json}"
            );
            // Also verify empty synonyms omitted
            assert!(
                !json.contains("synonyms"),
                "Empty synonyms should be omitted from JSON: {json}"
            );
            // Also verify None comment omitted
            assert!(
                !json.contains("comment"),
                "None comment should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn dimension_with_comment_and_synonyms_roundtrips() {
            let dim = Dimension {
                name: "region".to_string(),
                expr: "region".to_string(),
                source_table: None,
                output_type: None,
                comment: Some("Geographic region".to_string()),
                synonyms: vec!["area".to_string(), "territory".to_string()],
                is_filter: false,
            };
            let json = serde_json::to_string(&dim).unwrap();
            assert!(
                json.contains(r#""comment":"Geographic region""#),
                "comment in JSON: {json}"
            );
            assert!(
                json.contains(r#""synonyms":["area","territory"]"#),
                "synonyms in JSON: {json}"
            );
            let rt: Dimension = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.comment.as_deref(), Some("Geographic region"));
            assert_eq!(rt.synonyms, vec!["area", "territory"]);
        }

        #[test]
        fn table_ref_with_metadata_roundtrips() {
            let tr = TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                unique_constraints: vec![],
                comment: Some("Main orders table".to_string()),
                synonyms: vec!["order_facts".to_string()],
            };
            let json = serde_json::to_string(&tr).unwrap();
            assert!(
                json.contains(r#""comment":"Main orders table""#),
                "comment in JSON: {json}"
            );
            assert!(
                json.contains(r#""synonyms":["order_facts"]"#),
                "synonyms in JSON: {json}"
            );
            let rt: TableRef = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.comment.as_deref(), Some("Main orders table"));
            assert_eq!(rt.synonyms, vec!["order_facts"]);
        }

        #[test]
        fn fact_with_access_and_metadata_roundtrips() {
            let fact = Fact {
                name: "unit_price".to_string(),
                expr: "price / qty".to_string(),
                source_table: Some("o".to_string()),
                output_type: None,
                comment: Some("Price per unit".to_string()),
                synonyms: vec!["price_per_item".to_string()],
                is_filter: false,
                access: AccessModifier::Private,
            };
            let json = serde_json::to_string(&fact).unwrap();
            assert!(
                json.contains(r#""access":"Private""#),
                "access in JSON: {json}"
            );
            assert!(
                json.contains(r#""comment":"Price per unit""#),
                "comment in JSON: {json}"
            );
            let rt: Fact = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.access, AccessModifier::Private);
            assert_eq!(rt.comment.as_deref(), Some("Price per unit"));
            assert_eq!(rt.synonyms, vec!["price_per_item"]);
        }

        #[test]
        fn view_level_comment_roundtrips() {
            let def = SemanticViewDefinition {
                comment: Some("Revenue analytics view".to_string()),
                ..Default::default()
            };
            let json = serde_json::to_string(&def).unwrap();
            assert!(
                json.contains(r#""comment":"Revenue analytics view""#),
                "view comment in JSON: {json}"
            );
            let rt = SemanticViewDefinition::from_json("orders", &json).unwrap();
            assert_eq!(rt.comment.as_deref(), Some("Revenue analytics view"));
        }
    }

    mod phase47_non_additive_tests {
        use super::*;

        #[test]
        fn sort_order_default_is_asc() {
            assert_eq!(SortOrder::default(), SortOrder::Asc);
        }

        #[test]
        fn nulls_order_default_is_last() {
            assert_eq!(NullsOrder::default(), NullsOrder::Last);
        }

        #[test]
        fn sort_order_is_default() {
            assert!(SortOrder::Asc.is_default());
            assert!(!SortOrder::Desc.is_default());
        }

        #[test]
        fn nulls_order_is_default() {
            assert!(NullsOrder::Last.is_default());
            assert!(!NullsOrder::First.is_default());
        }

        #[test]
        fn non_additive_dim_with_defaults_skips_order_and_nulls() {
            let nad = NonAdditiveDim {
                dimension: "date_dim".to_string(),
                order: SortOrder::Asc,
                nulls: NullsOrder::Last,
            };
            let json = serde_json::to_string(&nad).unwrap();
            assert!(
                !json.contains("order"),
                "Default order (Asc) should be omitted: {json}"
            );
            assert!(
                !json.contains("nulls"),
                "Default nulls (Last) should be omitted: {json}"
            );
        }

        #[test]
        fn non_additive_dim_with_non_defaults_includes_fields() {
            let nad = NonAdditiveDim {
                dimension: "date_dim".to_string(),
                order: SortOrder::Desc,
                nulls: NullsOrder::First,
            };
            let json = serde_json::to_string(&nad).unwrap();
            assert!(
                json.contains(r#""order":"Desc""#),
                "Desc order should appear in JSON: {json}"
            );
            assert!(
                json.contains(r#""nulls":"First""#),
                "First nulls should appear in JSON: {json}"
            );
            // Roundtrip
            let rt: NonAdditiveDim = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.order, SortOrder::Desc);
            assert_eq!(rt.nulls, NullsOrder::First);
        }

        #[test]
        fn metric_without_non_additive_by_deserializes_with_empty_vec() {
            // Backward compat: pre-v0.6.0 JSON without non_additive_by field
            let json = r#"{"name":"revenue","expr":"SUM(amount)"}"#;
            let met: Metric = serde_json::from_str(json).unwrap();
            assert!(
                met.non_additive_by.is_empty(),
                "non_additive_by should default to [] for old JSON"
            );
        }

        #[test]
        fn metric_with_empty_non_additive_by_omits_field() {
            let met = Metric {
                name: "revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                non_additive_by: vec![],
                ..Default::default()
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                !json.contains("non_additive_by"),
                "Empty non_additive_by should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn metric_with_non_additive_by_roundtrips() {
            let met = Metric {
                name: "balance".to_string(),
                expr: "SUM(amount)".to_string(),
                source_table: Some("a".to_string()),
                non_additive_by: vec![
                    NonAdditiveDim {
                        dimension: "date_dim".to_string(),
                        order: SortOrder::Desc,
                        nulls: NullsOrder::First,
                    },
                    NonAdditiveDim {
                        dimension: "account".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    },
                ],
                ..Default::default()
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                json.contains("non_additive_by"),
                "non_additive_by with entries should appear in JSON: {json}"
            );
            let rt: Metric = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.non_additive_by.len(), 2);
            assert_eq!(rt.non_additive_by[0].dimension, "date_dim");
            assert_eq!(rt.non_additive_by[0].order, SortOrder::Desc);
            assert_eq!(rt.non_additive_by[0].nulls, NullsOrder::First);
            assert_eq!(rt.non_additive_by[1].dimension, "account");
            assert_eq!(rt.non_additive_by[1].order, SortOrder::Asc);
            assert_eq!(rt.non_additive_by[1].nulls, NullsOrder::Last);
        }
    }

    mod window_spec_tests {
        use super::*;

        #[test]
        fn window_spec_roundtrip_serde() {
            let ws = WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "total_qty".to_string(),
                extra_args: vec![],
                excluding_dims: vec!["date_dim".to_string()],
                partition_dims: vec![],
                order_by: vec![WindowOrderBy {
                    expr: "date_dim".to_string(),
                    order: SortOrder::Asc,
                    nulls: NullsOrder::Last,
                }],
                frame_clause: None,
            };
            let json = serde_json::to_string(&ws).unwrap();
            let rt: WindowSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.window_function, "AVG");
            assert_eq!(rt.inner_metric, "total_qty");
            assert_eq!(rt.excluding_dims, vec!["date_dim"]);
            assert_eq!(rt.order_by.len(), 1);
            assert_eq!(rt.order_by[0].expr, "date_dim");
            assert!(rt.frame_clause.is_none());
        }

        #[test]
        fn metric_without_window_spec_deserializes_from_old_json() {
            // Backward compat: pre-Phase 48 JSON has no window_spec field
            let json = r#"{"name":"revenue","expr":"SUM(amount)"}"#;
            let met: Metric = serde_json::from_str(json).unwrap();
            assert!(met.window_spec.is_none());
            assert!(!met.is_window());
        }

        #[test]
        fn window_spec_full_roundtrip() {
            let ws = WindowSpec {
                window_function: "LAG".to_string(),
                inner_metric: "balance".to_string(),
                extra_args: vec!["30".to_string()],
                excluding_dims: vec!["region".to_string(), "status".to_string()],
                partition_dims: vec![],
                order_by: vec![
                    WindowOrderBy {
                        expr: "date_dim".to_string(),
                        order: SortOrder::Desc,
                        nulls: NullsOrder::First,
                    },
                    WindowOrderBy {
                        expr: "account".to_string(),
                        order: SortOrder::Asc,
                        nulls: NullsOrder::Last,
                    },
                ],
                frame_clause: Some(
                    "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW".to_string(),
                ),
            };
            let json = serde_json::to_string(&ws).unwrap();
            let rt: WindowSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.window_function, "LAG");
            assert_eq!(rt.inner_metric, "balance");
            assert_eq!(rt.extra_args, vec!["30"]);
            assert_eq!(rt.excluding_dims, vec!["region", "status"]);
            assert_eq!(rt.order_by.len(), 2);
            assert_eq!(rt.order_by[0].expr, "date_dim");
            assert_eq!(rt.order_by[0].order, SortOrder::Desc);
            assert_eq!(rt.order_by[0].nulls, NullsOrder::First);
            assert_eq!(rt.order_by[1].expr, "account");
            assert_eq!(rt.order_by[1].order, SortOrder::Asc);
            assert_eq!(rt.order_by[1].nulls, NullsOrder::Last);
            assert_eq!(
                rt.frame_clause.as_deref(),
                Some("RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW")
            );
        }

        #[test]
        fn window_order_by_non_default_sort_nulls_roundtrips() {
            let wob = WindowOrderBy {
                expr: "ts".to_string(),
                order: SortOrder::Desc,
                nulls: NullsOrder::First,
            };
            let json = serde_json::to_string(&wob).unwrap();
            assert!(json.contains("\"order\":\"Desc\""));
            assert!(json.contains("\"nulls\":\"First\""));
            let rt: WindowOrderBy = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.order, SortOrder::Desc);
            assert_eq!(rt.nulls, NullsOrder::First);
        }

        #[test]
        fn metric_with_empty_window_spec_omits_field() {
            let met = Metric {
                name: "revenue".to_string(),
                expr: "SUM(amount)".to_string(),
                window_spec: None,
                ..Default::default()
            };
            let json = serde_json::to_string(&met).unwrap();
            assert!(
                !json.contains("window_spec"),
                "None window_spec should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn metric_is_window_returns_true_when_set() {
            let met = Metric {
                name: "avg_qty_7d".to_string(),
                expr: "AVG(total_qty) OVER (...)".to_string(),
                window_spec: Some(WindowSpec {
                    window_function: "AVG".to_string(),
                    inner_metric: "total_qty".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            };
            assert!(met.is_window());
        }
    }

    mod yaml_tests {
        use super::*;

        #[test]
        fn minimal_yaml_deserializes() {
            let yaml = "tables:\n  - alias: o\n    table: orders\ndimensions:\n  - name: region\n    expr: region\nmetrics:\n  - name: revenue\n    expr: SUM(amount)\n";
            let def = SemanticViewDefinition::from_yaml("orders", yaml).unwrap();
            assert_eq!(def.base_table(), "orders");
            assert_eq!(def.dimensions.len(), 1);
            assert_eq!(def.dimensions[0].name, "region");
            assert_eq!(def.dimensions[0].expr, "region");
            assert_eq!(def.metrics.len(), 1);
            assert_eq!(def.metrics[0].name, "revenue");
            assert_eq!(def.metrics[0].expr, "SUM(amount)");
        }

        #[test]
        fn full_yaml_all_fields() {
            let yaml = r#"
base_table: orders
tables:
  - alias: o
    table: orders
    pk_columns:
      - id
    unique_constraints:
      - - email
      - - first_name
        - last_name
    comment: Main orders table
    synonyms:
      - order_facts
  - alias: c
    table: customers
    pk_columns:
      - id
joins:
  - table: c
    from_alias: o
    fk_columns:
      - customer_id
    ref_columns:
      - id
    name: order_to_customer
    cardinality: ManyToOne
facts:
  - name: unit_price
    expr: "o.price / o.qty"
    source_table: o
    output_type: DOUBLE
    comment: Price per unit
    synonyms:
      - price_per_item
    access: Private
dimensions:
  - name: region
    expr: o.region
    source_table: o
    output_type: VARCHAR
    comment: Geographic region
    synonyms:
      - area
      - territory
metrics:
  - name: revenue
    expr: SUM(o.amount)
    source_table: o
    output_type: "DECIMAL(18,2)"
    comment: Total revenue
    synonyms:
      - total_revenue
    access: Public
    using_relationships:
      - order_to_customer
  - name: balance
    expr: SUM(o.amount)
    source_table: o
    non_additive_by:
      - dimension: date_dim
        order: Desc
        nulls: First
  - name: avg_qty_7d
    expr: AVG(total_qty)
    window_spec:
      window_function: AVG
      inner_metric: total_qty
      excluding_dims:
        - date_dim
      order_by:
        - expr: date_dim
          order: Asc
          nulls: Last
      frame_clause: "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW"
comment: Revenue analytics view
"#;
            let def = SemanticViewDefinition::from_yaml("orders", yaml).unwrap();
            // Tables
            assert_eq!(def.tables.len(), 2);
            assert_eq!(def.tables[0].alias, "o");
            assert_eq!(def.tables[0].table, "orders");
            assert_eq!(def.tables[0].pk_columns, vec!["id"]);
            assert_eq!(def.tables[0].unique_constraints.len(), 2);
            assert_eq!(def.tables[0].comment.as_deref(), Some("Main orders table"));
            assert_eq!(def.tables[0].synonyms, vec!["order_facts"]);
            // Joins
            assert_eq!(def.joins.len(), 1);
            assert_eq!(def.joins[0].table, "c");
            assert_eq!(def.joins[0].from_alias, "o");
            assert_eq!(def.joins[0].fk_columns, vec!["customer_id"]);
            assert_eq!(def.joins[0].ref_columns, vec!["id"]);
            assert_eq!(def.joins[0].name.as_deref(), Some("order_to_customer"));
            assert_eq!(def.joins[0].cardinality, Cardinality::ManyToOne);
            // Facts
            assert_eq!(def.facts.len(), 1);
            assert_eq!(def.facts[0].name, "unit_price");
            assert_eq!(def.facts[0].access, AccessModifier::Private);
            assert_eq!(def.facts[0].comment.as_deref(), Some("Price per unit"));
            assert_eq!(def.facts[0].synonyms, vec!["price_per_item"]);
            // Dimensions
            assert_eq!(def.dimensions.len(), 1);
            assert_eq!(def.dimensions[0].source_table.as_deref(), Some("o"));
            assert_eq!(def.dimensions[0].output_type.as_deref(), Some("VARCHAR"));
            assert_eq!(
                def.dimensions[0].comment.as_deref(),
                Some("Geographic region")
            );
            assert_eq!(def.dimensions[0].synonyms, vec!["area", "territory"]);
            // Metrics
            assert_eq!(def.metrics.len(), 3);
            assert_eq!(def.metrics[0].access, AccessModifier::Public);
            assert_eq!(
                def.metrics[0].using_relationships,
                vec!["order_to_customer"]
            );
            assert_eq!(def.metrics[0].comment.as_deref(), Some("Total revenue"));
            assert_eq!(def.metrics[0].synonyms, vec!["total_revenue"]);
            // Semi-additive metric
            assert_eq!(def.metrics[1].non_additive_by.len(), 1);
            assert_eq!(def.metrics[1].non_additive_by[0].dimension, "date_dim");
            assert_eq!(def.metrics[1].non_additive_by[0].order, SortOrder::Desc);
            assert_eq!(def.metrics[1].non_additive_by[0].nulls, NullsOrder::First);
            // Window metric
            let ws = def.metrics[2].window_spec.as_ref().unwrap();
            assert_eq!(ws.window_function, "AVG");
            assert_eq!(ws.inner_metric, "total_qty");
            assert_eq!(ws.excluding_dims, vec!["date_dim"]);
            assert_eq!(ws.order_by.len(), 1);
            assert_eq!(ws.order_by[0].expr, "date_dim");
            assert_eq!(ws.order_by[0].order, SortOrder::Asc);
            assert_eq!(ws.order_by[0].nulls, NullsOrder::Last);
            assert!(ws
                .frame_clause
                .as_deref()
                .unwrap()
                .contains("RANGE BETWEEN"));
            // View-level comment
            assert_eq!(def.comment.as_deref(), Some("Revenue analytics view"));
        }

        #[test]
        fn optional_fields_default_when_omitted() {
            let yaml = "base_table: t\ndimensions: []\nmetrics: []\n";
            let def = SemanticViewDefinition::from_yaml("test", yaml).unwrap();
            assert!(def.tables.is_empty());
            assert!(def.joins.is_empty());
            assert!(def.facts.is_empty());
            assert!(def.created_on.is_none());
            assert!(def.database_name.is_none());
            assert!(def.schema_name.is_none());
            assert!(def.comment.is_none());
        }

        #[test]
        fn enum_variants_roundtrip_yaml() {
            // AccessModifier::Private
            let yaml = "base_table: t\ndimensions: []\nmetrics:\n  - name: m\n    expr: SUM(x)\n    access: Private\n";
            let def = SemanticViewDefinition::from_yaml("test", yaml).unwrap();
            assert_eq!(def.metrics[0].access, AccessModifier::Private);

            // Cardinality::OneToOne
            let yaml2 = "base_table: t\ndimensions: []\nmetrics: []\njoins:\n  - table: c\n    cardinality: OneToOne\n";
            let def2 = SemanticViewDefinition::from_yaml("test", yaml2).unwrap();
            assert_eq!(def2.joins[0].cardinality, Cardinality::OneToOne);
        }

        #[test]
        fn yaml_json_produce_identical_structs() {
            let yaml = "base_table: orders\ndimensions:\n  - name: region\n    expr: region\nmetrics:\n  - name: revenue\n    expr: SUM(amount)\n";
            let json = r#"{"base_table":"orders","dimensions":[{"name":"region","expr":"region"}],"metrics":[{"name":"revenue","expr":"SUM(amount)"}]}"#;
            let from_yaml = SemanticViewDefinition::from_yaml("test", yaml).unwrap();
            let from_json = SemanticViewDefinition::from_json("test", json).unwrap();
            assert_eq!(from_yaml, from_json);
        }

        #[test]
        fn invalid_yaml_syntax_returns_error_with_name() {
            let result = SemanticViewDefinition::from_yaml("my_view", "{{invalid yaml");
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(
                err.contains("my_view"),
                "error should contain view name: {err}"
            );
            assert!(
                err.contains("invalid YAML definition"),
                "error should contain prefix: {err}"
            );
        }

        #[test]
        fn size_cap_rejects_oversized_input() {
            let oversized = "a".repeat(1_048_577); // 1 byte over cap
            let result = SemanticViewDefinition::from_yaml_with_size_cap("big", &oversized);
            assert!(result.is_err());
            let err = result.unwrap_err();
            assert!(err.contains("exceeds size limit"), "error: {err}");
            assert!(
                err.contains("1048577 bytes"),
                "should contain actual size: {err}"
            );
            assert!(
                err.contains("1048576 byte cap"),
                "should contain cap: {err}"
            );
        }

        #[test]
        fn size_cap_accepts_exactly_1mb() {
            // Build a valid YAML string padded to exactly 1MB
            let prefix = "base_table: t\ntables:\n  - alias: t\n    table: t\ndimensions:\n  - name: d\n    expr: t.d\n    source_table: t\nmetrics: []\n# ";
            let pad_len = SemanticViewDefinition::YAML_SIZE_CAP - prefix.len();
            let padded = format!("{prefix}{}", "x".repeat(pad_len));
            assert_eq!(padded.len(), SemanticViewDefinition::YAML_SIZE_CAP);
            let result = SemanticViewDefinition::from_yaml_with_size_cap("test", &padded);
            assert!(
                result.is_ok(),
                "exactly 1MB should be accepted: {}",
                result.unwrap_err()
            );
        }

        #[test]
        fn unknown_fields_accepted_in_yaml() {
            let yaml = "base_table: t\ndimensions: []\nmetrics: []\nextra_field: surprise\n";
            assert!(
                SemanticViewDefinition::from_yaml("test", yaml).is_ok(),
                "unknown fields must not cause rejection (no deny_unknown_fields)"
            );
        }

        #[test]
        fn yaml_json_roundtrip_via_serialize() {
            // Build a struct, serialize to both formats, deserialize both, assert equal
            let def = SemanticViewDefinition {
                dimensions: vec![Dimension {
                    name: "region".to_string(),
                    expr: "region".to_string(),
                    ..Default::default()
                }],
                metrics: vec![Metric {
                    name: "revenue".to_string(),
                    expr: "SUM(amount)".to_string(),
                    access: AccessModifier::Private,
                    ..Default::default()
                }],
                facts: vec![Fact {
                    name: "unit_price".to_string(),
                    expr: "price / qty".to_string(),
                    access: AccessModifier::Private,
                    ..Default::default()
                }],
                comment: Some("test view".to_string()),
                ..Default::default()
            };
            let json_str = serde_json::to_string(&def).unwrap();
            let yaml_str = yaml_serde::to_string(&def).unwrap();

            let from_json = SemanticViewDefinition::from_json("test", &json_str).unwrap();
            let from_yaml = SemanticViewDefinition::from_yaml("test", &yaml_str).unwrap();
            assert_eq!(from_json, from_yaml);
        }
    }

    mod phase54_materialization_tests {
        use super::*;

        #[test]
        fn materialization_json_roundtrip() {
            let mat = Materialization {
                name: "daily_rev".to_string(),
                table: "daily_revenue_agg".to_string(),
                dimensions: vec!["region".to_string()],
                metrics: vec!["revenue".to_string(), "order_count".to_string()],
            };
            let json = serde_json::to_string(&mat).unwrap();
            let rt: Materialization = serde_json::from_str(&json).unwrap();
            assert_eq!(rt.name, "daily_rev");
            assert_eq!(rt.table, "daily_revenue_agg");
            assert_eq!(rt.dimensions, vec!["region"]);
            assert_eq!(rt.metrics, vec!["revenue", "order_count"]);
        }

        #[test]
        fn materialization_yaml_roundtrip() {
            let mat = Materialization {
                name: "daily_rev".to_string(),
                table: "catalog.schema.daily_revenue_agg".to_string(),
                dimensions: vec!["region".to_string()],
                metrics: vec!["revenue".to_string()],
            };
            let yaml_str = yaml_serde::to_string(&mat).unwrap();
            let rt: Materialization = yaml_serde::from_str(&yaml_str).unwrap();
            assert_eq!(rt.name, "daily_rev");
            assert_eq!(rt.table, "catalog.schema.daily_revenue_agg");
            assert_eq!(rt.dimensions, vec!["region"]);
            assert_eq!(rt.metrics, vec!["revenue"]);
        }

        #[test]
        fn definition_with_materializations_json_roundtrip() {
            let def = SemanticViewDefinition {
                dimensions: vec![Dimension {
                    name: "region".to_string(),
                    expr: "region".to_string(),
                    ..Default::default()
                }],
                metrics: vec![Metric {
                    name: "revenue".to_string(),
                    expr: "SUM(amount)".to_string(),
                    ..Default::default()
                }],
                materializations: vec![Materialization {
                    name: "daily_rev".to_string(),
                    table: "daily_revenue_agg".to_string(),
                    dimensions: vec!["region".to_string()],
                    metrics: vec!["revenue".to_string()],
                }],
                ..Default::default()
            };
            let json = serde_json::to_string(&def).unwrap();
            assert!(
                json.contains("materializations"),
                "materializations should appear in JSON: {json}"
            );
            let rt = SemanticViewDefinition::from_json("test", &json).unwrap();
            assert_eq!(rt.materializations.len(), 1);
            assert_eq!(rt.materializations[0].name, "daily_rev");
            assert_eq!(rt.materializations[0].table, "daily_revenue_agg");
            assert_eq!(rt.materializations[0].dimensions, vec!["region"]);
            assert_eq!(rt.materializations[0].metrics, vec!["revenue"]);
        }

        #[test]
        fn old_json_without_materializations_deserializes_to_empty_vec() {
            // Backward compat: pre-v0.7.0 JSON without materializations field
            let json = r#"{"base_table":"orders","dimensions":[],"metrics":[]}"#;
            let def = SemanticViewDefinition::from_json("test", json).unwrap();
            assert!(
                def.materializations.is_empty(),
                "materializations should default to [] for old JSON"
            );
        }

        #[test]
        fn empty_materializations_omitted_from_json() {
            let def = SemanticViewDefinition {
                materializations: vec![],
                ..Default::default()
            };
            let json = serde_json::to_string(&def).unwrap();
            assert!(
                !json.contains("materializations"),
                "Empty materializations should be omitted from JSON: {json}"
            );
        }

        #[test]
        fn yaml_and_json_with_materializations_produce_identical_structs() {
            let yaml = r#"
base_table: orders
dimensions:
  - name: region
    expr: region
metrics:
  - name: revenue
    expr: SUM(amount)
materializations:
  - name: daily_rev
    table: daily_revenue_agg
    dimensions:
      - region
    metrics:
      - revenue
"#;
            let from_yaml = SemanticViewDefinition::from_yaml("test", yaml).unwrap();

            let json = serde_json::to_string(&from_yaml).unwrap();
            let from_json = SemanticViewDefinition::from_json("test", &json).unwrap();
            assert_eq!(from_yaml, from_json);
        }

        #[test]
        fn materialization_empty_dims_and_metrics_omitted_from_json() {
            let mat = Materialization {
                name: "test".to_string(),
                table: "t".to_string(),
                dimensions: vec![],
                metrics: vec![],
            };
            let json = serde_json::to_string(&mat).unwrap();
            assert!(
                !json.contains("dimensions"),
                "Empty dimensions should be omitted from Materialization JSON: {json}"
            );
            assert!(
                !json.contains("metrics"),
                "Empty metrics should be omitted from Materialization JSON: {json}"
            );
        }
    }
}
