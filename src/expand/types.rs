use std::fmt;
use std::marker::PhantomData;

/// A query-request name (dimension or metric) with case- **and quote**-
/// insensitive equality and hashing.
///
/// Semantic-view names are matched under `DuckDB`'s identifier rule — case is
/// ignored, and a double-quoted name matches its unquoted spelling (`"Region"`,
/// `REGION`, `region` are the same name) — so this newtype provides
/// `PartialEq`/`Eq`/`Hash` on the canonical key from
/// [`crate::ident::normalize_ident_part`], the same rule
/// [`crate::ident::ident_matches`] and the resolution layer use. This
/// centralizes the ad-hoc `eq_ignore_ascii_case` / `to_ascii_lowercase` calls
/// that used to live throughout the resolution code (and closes the residual
/// gap where those folded case but did not strip quotes — TECH-DEBT #28
/// Slice 3). The `K` kind marker (see [`DimensionName`] and [`MetricName`])
/// keeps the flavors distinct at the type level so a dimension name can't be
/// passed where a metric name is expected — one impl, several types (R-7,
/// code-review 2026-07-11, replacing the former per-flavor copy-paste twins).
pub struct CiName<K> {
    raw: String,
    // `fn() -> K` keeps `CiName<K>: Send + Sync` regardless of `K` and marks the
    // kind purely at compile time (the marker types are never constructed).
    _kind: PhantomData<fn() -> K>,
}

impl<K> CiName<K> {
    pub fn new(s: impl Into<String>) -> Self {
        Self {
            raw: s.into(),
            _kind: PhantomData,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The canonical match key: quote-stripped and ASCII-case-folded via
    /// [`crate::ident::normalize_ident_part`]. Used by `Hash` for the (rare)
    /// quoted path; `Eq` uses the equivalent [`crate::ident::ident_matches`],
    /// which is allocation-free when neither side is quoted.
    fn key(&self) -> String {
        crate::ident::normalize_ident_part(&self.raw)
    }
}

impl<K> Clone for CiName<K> {
    fn clone(&self) -> Self {
        Self::new(self.raw.clone())
    }
}

impl<K> fmt::Debug for CiName<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CiName").field(&self.raw).finish()
    }
}

impl<K> PartialEq for CiName<K> {
    fn eq(&self, other: &Self) -> bool {
        // Allocation-free when neither side is quoted (plain
        // `eq_ignore_ascii_case`); only a quoted side takes the
        // strip-and-normalize path — see `ident::ident_matches`.
        crate::ident::ident_matches(&self.raw, &other.raw)
    }
}

impl<K> Eq for CiName<K> {}

impl<K> std::hash::Hash for CiName<K> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Must agree with `PartialEq` — equal names (any case/quoting) hash
        // identically — while staying allocation-free on the common unquoted
        // path. Hash the canonical key's bytes one at a time: for an unquoted
        // name that is exactly its ASCII-lowercased bytes (no allocation, and
        // byte-identical to the pre-quote-aware impl); a quoted name is
        // normalized (quotes stripped) first, so `"Region"` hashes like
        // `region` — consistent with the quote-insensitive `Eq`.
        if self.raw.as_bytes().contains(&b'"') {
            for b in self.key().bytes() {
                b.hash(state);
            }
        } else {
            for b in self.raw.bytes() {
                b.to_ascii_lowercase().hash(state);
            }
        }
    }
}

impl<K> fmt::Display for CiName<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl<K> std::ops::Deref for CiName<K> {
    type Target = str;
    fn deref(&self) -> &str {
        &self.raw
    }
}

impl<K> AsRef<str> for CiName<K> {
    fn as_ref(&self) -> &str {
        &self.raw
    }
}

impl<K> From<String> for CiName<K> {
    fn from(s: String) -> Self {
        Self::new(s)
    }
}

impl<K> From<&str> for CiName<K> {
    fn from(s: &str) -> Self {
        Self::new(s)
    }
}

/// Kind marker for [`DimensionName`]; never constructed.
pub enum DimensionKind {}

/// Kind marker for [`MetricName`]; never constructed.
pub enum MetricKind {}

/// Kind marker for [`FactName`]; never constructed.
pub enum FactKind {}

/// A dimension name with case- and quote-insensitive equality and hashing (see [`CiName`]).
pub type DimensionName = CiName<DimensionKind>;

/// A metric name with case- and quote-insensitive equality and hashing (see [`CiName`]).
pub type MetricName = CiName<MetricKind>;

/// A fact name with case- and quote-insensitive equality and hashing (see [`CiName`]).
pub type FactName = CiName<FactKind>;

/// A request to expand a semantic view into SQL.
///
/// Contains the names of dimensions and metrics to include in the query.
/// At least one dimension, metric, or fact must be specified. Supported modes:
/// - Dimensions only: `SELECT DISTINCT` (no aggregation)
/// - Metrics only: global aggregate (no `GROUP BY`)
/// - Both: grouped aggregation with `GROUP BY`
/// - Facts mode: row-level query (facts cannot be combined with metrics)
///
/// [`Self::where_clause`] carries the pre-aggregation filter — Snowflake's
/// `SEMANTIC_VIEW( … WHERE <predicate> )`, surfaced here as the
/// `where_clause := '…'` named parameter.
#[derive(Debug, Clone, Default)]
pub struct QueryRequest {
    pub dimensions: Vec<DimensionName>,
    pub metrics: Vec<MetricName>,
    pub facts: Vec<FactName>,
    /// The raw pre-aggregation predicate, exactly as the caller wrote it, or
    /// `None` when no `where_clause` was supplied.
    ///
    /// It references declared dimension and fact *names*; those are resolved to
    /// their expressions before emission. It is applied BEFORE metrics are
    /// aggregated — which is the whole point, and why it cannot be expressed by
    /// wrapping the generated query in an outer `WHERE`. Snowflake's rule that
    /// the predicate may not reference metrics is enforced during resolution.
    pub where_clause: Option<String>,
}

/// A resolved dimension paired with its role-playing scoped alias, if any.
///
/// R-8 (code-review 2026-07-11): replaces the former parallel slices
/// `resolved_dims: &[&Dimension]` and `dim_scoped_aliases: &[Option<String>]`,
/// which were threaded together through several expansion functions and indexed
/// by position (`dim_scoped_aliases[i]`) — a silent-wrong-results footgun if the
/// two ever fell out of sync. Zipping them into one value makes the pairing
/// structural, so an index can't reach the wrong alias.
pub(crate) struct ResolvedDim<'a> {
    /// The resolved dimension definition (borrowed from the view definition).
    pub dim: &'a crate::model::Dimension,
    /// The role-playing scoped alias for this dimension's source table
    /// (e.g. `Some("a__dep_airport")`), or `None` when the table is not
    /// role-played for this query.
    pub scoped_alias: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_name_case_insensitive_eq() {
        assert_eq!(DimensionName::new("Foo"), DimensionName::new("foo"));
        assert_eq!(DimensionName::new("FOO"), DimensionName::new("foo"));
        assert_ne!(DimensionName::new("foo"), DimensionName::new("bar"));
    }

    #[test]
    fn dimension_name_case_insensitive_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(DimensionName::new("Foo"));
        assert!(set.contains(&DimensionName::new("foo")));
        assert!(set.contains(&DimensionName::new("FOO")));
        assert!(!set.contains(&DimensionName::new("bar")));
    }

    #[test]
    fn dimension_name_quote_insensitive_eq_and_hash() {
        use std::collections::HashSet;
        // A double-quoted name matches its unquoted spelling (quotes stripped +
        // case folded), consistent with `ident::ident_matches` — TECH-DEBT #28.
        assert_eq!(
            DimensionName::new("\"Region\""),
            DimensionName::new("region")
        );
        assert_eq!(
            DimensionName::new("\"REGION\""),
            DimensionName::new("Region")
        );
        let mut set = HashSet::new();
        set.insert(DimensionName::new("region"));
        assert!(set.contains(&DimensionName::new("\"Region\"")));
        // A quoted name that carries a space still matches its unquoted key.
        assert_eq!(
            MetricName::new("\"Total Revenue\""),
            MetricName::new("total revenue")
        );
    }

    #[test]
    fn metric_name_case_insensitive_eq() {
        assert_eq!(MetricName::new("Revenue"), MetricName::new("revenue"));
        assert_ne!(MetricName::new("revenue"), MetricName::new("cost"));
    }

    #[test]
    fn metric_name_case_insensitive_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(MetricName::new("Revenue"));
        assert!(set.contains(&MetricName::new("revenue")));
        assert!(!set.contains(&MetricName::new("cost")));
    }

    #[test]
    fn dimension_name_display() {
        let name = DimensionName::new("Region");
        assert_eq!(format!("{name}"), "Region");
    }

    #[test]
    fn metric_name_deref_to_str() {
        let name = MetricName::new("total_revenue");
        let s: &str = &name;
        assert_eq!(s, "total_revenue");
    }

    #[test]
    fn dimension_name_from_string() {
        let name: DimensionName = "foo".into();
        assert_eq!(name.as_str(), "foo");
        let name2: DimensionName = String::from("bar").into();
        assert_eq!(name2.as_str(), "bar");
    }

    #[test]
    fn ci_name_shared_impl_covers_both_kinds() {
        // R-7 (code-review 2026-07-11): `DimensionName` and `MetricName` are now
        // `CiName<K>` aliases sharing one impl. Exercise the surface (Clone,
        // Deref, AsRef, case-insensitive Eq) through both kinds so the generic
        // impl stays covered for each.
        let dim = DimensionName::new("Region");
        let dim_clone = dim.clone();
        assert_eq!(dim, dim_clone);
        assert_eq!(dim, DimensionName::new("REGION")); // case-insensitive Eq
        assert_eq!(&*dim, "Region"); // Deref<Target = str>
        let as_ref: &str = dim.as_ref(); // AsRef<str>
        assert_eq!(as_ref, "Region");

        let met = MetricName::new("Total_Revenue");
        assert_eq!(met, MetricName::new("total_revenue"));
        assert_eq!(&*met.clone(), "Total_Revenue");
        assert_eq!(met.as_ref() as &str, "Total_Revenue");

        // Facts share the same case-insensitive impl (R-7 follow-up: the third
        // `CiName<K>` type the original change omitted, replacing the former
        // stringly `facts: Vec<String>`).
        let fact = FactName::new("Line_Total");
        assert_eq!(fact, FactName::new("line_total"));
        assert_eq!(&*fact.clone(), "Line_Total");
        assert_eq!(fact.as_ref() as &str, "Line_Total");
    }

    #[test]
    fn expand_error_stays_under_large_err_threshold() {
        // R-9 (code-review 2026-07-11): the two fattest variants (FanTrap,
        // MetricFanTrap) are boxed so `ExpandError` fits under clippy's
        // `result_large_err` threshold (128 bytes) and the `Result<_,
        // ExpandError>` allows could be dropped. Pin the size so a future fat
        // variant can't silently reintroduce the bloat (box it instead).
        assert!(
            std::mem::size_of::<ExpandError>() <= 128,
            "ExpandError is {} bytes (> 128); box the newly-added fat variant (see R-9)",
            std::mem::size_of::<ExpandError>()
        );
    }
}

/// Detail payload for [`ExpandError::FanTrap`], boxed so the enum stays small
/// (R-9, code-review 2026-07-11 — this variant was one of the two fattest).
#[derive(Debug)]
pub struct FanTrapError {
    pub view_name: String,
    pub metric_name: String,
    pub metric_table: String,
    pub dimension_name: String,
    pub dimension_table: String,
    pub relationship_name: String,
}

/// Detail payload for [`ExpandError::ReferencedFactFanTrap`], boxed for the
/// same reason as its two neighbours (R-9): six `String` fields would put
/// `ExpandError` over clippy's `result_large_err` threshold on their own.
#[derive(Debug)]
pub struct ReferencedFactFanTrapError {
    pub view_name: String,
    pub member_name: String,
    pub member_table: String,
    pub fact_name: String,
    pub fact_table: String,
    pub relationship_name: String,
}

/// Detail payload for [`ExpandError::AmbiguousMemberFactPath`], boxed for the
/// same reason as [`ReferencedFactFanTrapError`] (R-9): five `String`s and a
/// `Vec` would put `ExpandError` over clippy's `result_large_err` threshold.
#[derive(Debug)]
pub struct AmbiguousMemberFactPathError {
    pub view_name: String,
    pub member_name: String,
    pub fact_name: String,
    pub fact_table: String,
    pub role_playing_table: String,
    pub available_relationships: Vec<String>,
}

/// Detail payload for [`ExpandError::AmbiguousMetricPath`], boxed for the same
/// reason as [`AmbiguousMemberFactPathError`].
#[derive(Debug)]
pub struct AmbiguousMetricPathError {
    pub view_name: String,
    pub metric_name: String,
    pub metric_table: String,
    pub role_playing_table: String,
    pub available_relationships: Vec<String>,
}

/// Detail payload for [`ExpandError::MetricFanTrap`], boxed so the enum stays
/// small (R-9, code-review 2026-07-11 — the other fat variant).
#[derive(Debug)]
pub struct MetricFanTrapError {
    pub view_name: String,
    pub metric_name: String,
    pub metric_table: String,
    pub other_metric_name: String,
    pub other_metric_table: String,
    pub relationship_name: String,
}

/// Errors that can occur during semantic view expansion.
#[derive(Debug)]
pub enum ExpandError {
    /// The request contained neither dimensions nor metrics.
    EmptyRequest { view_name: String },
    /// A requested dimension name does not exist in the view definition.
    UnknownDimension {
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    },
    /// A requested metric name does not exist in the view definition.
    UnknownMetric {
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    },
    /// A dimension name was requested more than once.
    DuplicateDimension { view_name: String, name: String },
    /// A metric name was requested more than once.
    DuplicateMetric { view_name: String, name: String },
    /// A metric aggregates across a one-to-many boundary, risking inflated results.
    FanTrap { detail: Box<FanTrapError> },
    /// Two queried metrics sit at different grains (source tables) and the
    /// join path between those tables crosses a fan-out edge: joining both
    /// source tables multiplies `metric_table`'s rows, silently inflating
    /// `metric_name` (fan trap / chasm trap between metric grains).
    MetricFanTrap { detail: Box<MetricFanTrapError> },
    /// A metric aggregates a table that fans out relative to the query's base
    /// (root) table. The generated SQL is always anchored `FROM <root>`, so if
    /// the metric's source table is a parent/ancestor of the root across a
    /// many-to-one edge, the metric's rows are duplicated once per root row and
    /// the aggregate is silently inflated — even when the metric is queried
    /// alone with no other metric or dimension to trigger the pairwise checks
    /// (EXP-1, code-review 2026-07-18).
    RootGrainFanTrap {
        view_name: String,
        metric_name: String,
        metric_table: String,
        relationship_name: String,
    },
    /// The stored definition's relationship graph could not be rebuilt at
    /// query time, so safety checks (fan-trap detection) cannot run.
    UncheckableDefinition { view_name: String, reason: String },
    /// A dimension from a role-playing table is ambiguous because multiple
    /// relationships reach that table and no co-queried metric provides USING
    /// context to disambiguate.
    AmbiguousPath {
        view_name: String,
        dimension_name: String,
        dimension_table: String,
        available_relationships: Vec<String>,
    },
    /// A dimension whose table is reached *only through* a role-playing table
    /// (a descendant of it), queried without a single role: some metric names
    /// no path to the role-playing table, or the metrics name several in a query
    /// that cannot be split by path (EXP-4, code-review 2026-07-18). Since
    /// Phase 6.3b a single role is followed onward, as Snowflake does; before
    /// EXP-4 this bound silently to the first-declared relationship.
    AmbiguousDescendantPath {
        view_name: String,
        dimension_name: String,
        dimension_table: String,
        role_playing_table: String,
        available_relationships: Vec<String>,
    },
    /// A fact whose source table is (or is reached only through) a role-playing
    /// table. Facts carry no USING context, so the role is unresolvable
    /// (EXP-5, code-review 2026-07-18); previously bound silently to the
    /// first-declared relationship.
    AmbiguousFactPath {
        view_name: String,
        fact_name: String,
        fact_table: String,
        role_playing_table: String,
        available_relationships: Vec<String>,
    },
    /// A `where_clause` member whose table is (or is reached only through) a
    /// role-playing table. Only a queried *dimension's* expression is rewritten
    /// to a scoped alias, so a predicate member has no way to name its role
    /// (EXP-10, code-review 2026-08-03); previously the member's table was
    /// joined on the first-declared relationship and the predicate filtered
    /// through it silently — even when a co-queried metric's `USING` named the
    /// other role, so a query could group by one instance and filter by another.
    AmbiguousWhereClausePath {
        view_name: String,
        member_name: String,
        member_table: String,
        role_playing_table: String,
        available_relationships: Vec<String>,
    },
    /// A dimension or metric whose expression reaches a FACT on a table that is
    /// (or is reached only through) a role-playing table, with no single role
    /// to bind it to (TECH-DEBT #69). A base metric over a fact on the
    /// role-playing table itself is accepted when its `USING` names exactly one
    /// relationship to that table — the fact is then inlined against that
    /// role's scoped alias. Anything else (no `USING`, a dimension, or a fact on
    /// a descendant the scoped alias does not rename) previously joined the
    /// first-declared relationship silently.
    AmbiguousMemberFactPath {
        detail: Box<AmbiguousMemberFactPathError>,
    },
    /// A metric declared ON a table that is (or is reached only through) a
    /// role-playing table (PR #247 review). Such a metric cannot carry a
    /// `USING` that names a role — a `USING` relationship must originate from
    /// the metric's own table — so its table can only be joined through
    /// `tree_parent`, the first-declared relationship: a silent,
    /// declaration-order-dependent number. The metric analogue of
    /// [`Self::AmbiguousDescendantPath`] (EXP-4) and [`Self::AmbiguousFactPath`]
    /// (EXP-5).
    AmbiguousMetricPath {
        detail: Box<AmbiguousMetricPathError>,
    },
    /// A requested metric is marked PRIVATE and cannot be queried directly.
    PrivateMetric { view_name: String, name: String },
    /// A requested fact is marked PRIVATE and cannot be queried directly.
    PrivateFact { view_name: String, name: String },
    /// Facts and metrics cannot be combined in the same query.
    FactsMetricsMutualExclusion { view_name: String },
    /// Facts and dimensions cannot be queried together (Snowflake's `2026_07`
    /// behaviour change): a dimension goes in FACTS, or a fact in DIMENSIONS.
    FactsWithDimensions { view_name: String },
    /// A requested fact name does not exist in the view definition.
    UnknownFact {
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    },
    /// A fact name was requested more than once.
    DuplicateFact { view_name: String, name: String },
    /// A fact query references two tables that cannot be joined without
    /// multiplying rows — the path between them fans out whichever way it is
    /// walked.
    FactPathViolation {
        view_name: String,
        table_a: String,
        table_b: String,
    },
    /// The `where_clause` predicate references a metric. Snowflake's rule for
    /// the `WHERE` inside `SEMANTIC_VIEW(…)`: "you can only refer to dimensions,
    /// facts, and expressions that use dimensions and facts". A metric is an
    /// aggregate, and the predicate is applied *before* aggregation, so there is
    /// no value to compare against.
    WhereClauseReferencesMetric {
        view_name: String,
        metric_name: String,
    },
    /// The `where_clause` predicate holds a `;` outside its literals. It is
    /// spliced into the expansion as written, so the `;` would end the
    /// expansion's statement and start another.
    WhereClauseNotOnePredicate { view_name: String },
    /// The `where_clause` predicate holds a prepared statement's parameter
    /// other than `$1`, `$2`, ..., which refer to the values of `params`. The
    /// expansion is bound in the caller's statement, so it would take one of
    /// the caller's parameters (TECH-DEBT #105).
    WhereClauseParameter {
        view_name: String,
        parameter: String,
    },
    /// The `where_clause` predicate refers to a value of `params` past the
    /// last one supplied.
    WhereClauseParamMissing {
        view_name: String,
        reference: String,
        supplied: usize,
    },
    /// A `where_clause` member sits across a fan-out edge from a metric's
    /// grain. Filtering on a table requires joining it, and that join
    /// multiplies the metric's rows exactly as a grouping join would — so the
    /// metric would be inflated by the very filter meant to narrow it.
    WhereClauseFanTrap {
        view_name: String,
        metric_name: String,
        member_name: String,
        member_table: String,
        relationship_name: String,
    },
    /// A member's expression references a named fact declared on a table that
    /// **fans** relative to the member's own. Reaching the fact requires
    /// joining its table, and that join multiplies the member's rows — for a
    /// metric that inflates the aggregate, for a dimension it duplicates the
    /// output rows. PAR-6 / TECH-DEBT #53: before the referenced fact's table
    /// was joined at all, this shape failed as a `DuckDB` unknown-alias error, so
    /// no query is losing an answer it used to get.
    ReferencedFactFanTrap {
        detail: Box<ReferencedFactFanTrapError>,
    },
    /// Window function metrics cannot be mixed with aggregate metrics.
    WindowAggregateMixing {
        view_name: String,
        window_metrics: Vec<String>,
        aggregate_metrics: Vec<String>,
    },
    /// A dimension required by a window metric (EXCLUDING or ORDER BY) is not in the query.
    WindowMetricRequiredDimension {
        view_name: String,
        metric_name: String,
        dimension_name: String,
        reason: String,
    },
    /// The catalog `RwLock` is poisoned (a previous thread panicked while holding the lock).
    CatalogPoisoned { view_name: String },
    /// A cycle was detected in derived metric or fact dependencies at query expansion time.
    CycleDetected {
        view_name: String,
        cycle_description: String,
    },
    /// Derived metric nesting exceeds the maximum allowed depth.
    MaxDepthExceeded {
        view_name: String,
        depth: usize,
        max_depth: usize,
    },
    /// A metric co-queried with an active semi-additive metric cannot be
    /// decomposed for the snapshot CTE (SG-5). The CTE captures each metric's
    /// inner expression per row and re-aggregates it outside the snapshot
    /// filter, which is only sound for a single bare aggregate call
    /// `FUNC(args)` with FUNC in SUM/COUNT/AVG/MIN/MAX, no `*`, no DISTINCT.
    SemiAdditiveCoQueryUnsupported {
        view_name: String,
        metric_name: String,
        metric_expr: String,
        semi_metric_name: String,
        reason: String,
    },
    /// An active semi-additive metric's own expression cannot be decomposed
    /// for the snapshot CTE (same shape requirements as co-queried metrics).
    SemiAdditiveUnsupportedExpression {
        view_name: String,
        metric_name: String,
        metric_expr: String,
        reason: String,
    },
    /// A `COUNT(*)` metric on a non-base source table cannot be made safe
    /// (SG-8). Synthesized joins are LEFT JOINs, so the source table is
    /// NULL-extended by one row per unmatched base row and `COUNT(*)` would
    /// silently over-count. The expansion rewrites such metrics to
    /// `COUNT(<first PK column>)`, which requires the source table to declare
    /// a PRIMARY KEY.
    CountStarRequiresPrimaryKey {
        view_name: String,
        metric_name: String,
        table_alias: String,
    },
    /// A requested metric DEPENDS on an active semi-additive metric — a
    /// derived metric referencing it, or a window metric naming it as its
    /// inner aggregate (EXP-19/EXP-20, code-review 2026-08-06).
    ///
    /// The routing predicate `is_active_semi_additive` inspects only a
    /// metric's own `non_additive_by`, so such a metric classified as regular
    /// and the dependency's raw aggregate was inlined and evaluated over every
    /// row — silently discarding `NON ADDITIVE BY` and returning a number that
    /// did not even agree with the same snapshot queried directly.
    ///
    /// Composing a snapshot with an outer expression is a feature, not a
    /// defect to patch (TECH-DEBT #55); until it lands the fence errors, the
    /// same call the SG-5 co-query guard makes for shapes its CTE cannot
    /// decompose.
    SemiAdditiveThroughDependency {
        view_name: String,
        /// The requested metric that reaches the semi-additive one.
        metric_name: String,
        /// The active semi-additive metric it depends on.
        semi_metric_name: String,
        /// That metric's `NON ADDITIVE BY` dimensions, so the message can name
        /// the ones which — if queried — make it effectively regular and the
        /// query legal.
        non_additive_by: Vec<String>,
    },
    /// An active semi-additive metric reached the snapshot's outer
    /// re-aggregation without belonging to any `NON ADDITIVE BY` group, so no
    /// rank column can be named for it (EXP-18).
    ///
    /// Not reachable from any query: `collect_na_groups` and the routing
    /// predicate share `is_active_semi_additive` over the same `resolved_mets`
    /// index space. It exists so a future divergence between those two sides
    /// fails loudly instead of aliasing the metric onto the first group's rank
    /// column — snapshotting it at another group's ordering and returning a
    /// wrong number silently (the #129/#32 failure shape).
    SemiAdditiveRankColumnUnresolved {
        view_name: String,
        metric_name: String,
    },
}

impl fmt::Display for ExpandError {
    #[allow(clippy::too_many_lines)]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // R-16 (code-review 2026-07-11): this arm is the single source of the
            // empty-request wording. `QueryError::EmptyRequest` renders by
            // delegating to it, so the two can no longer drift apart (they had:
            // this side lacked the `facts` option and the DESCRIBE hint). Both
            // are reachable — the FFI binder short-circuits with the QueryError
            // form; a direct `expand()` call hits this form.
            Self::EmptyRequest { view_name } => {
                write!(
                    f,
                    "semantic view '{view_name}': specify at least one of DIMENSIONS, METRICS or FACTS (dimensions := [...], metrics := [...] or facts := [...])."
                )?;
                write!(
                    f,
                    " Run DESCRIBE SEMANTIC VIEW {view_name} to see available dimensions, metrics, and facts."
                )
            }
            Self::UnknownDimension {
                view_name,
                name,
                available,
                suggestion,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': unknown dimension '{name}'. Available: [{}]",
                    available.join(", ")
                )?;
                if let Some(s) = suggestion {
                    write!(f, ". Did you mean '{s}'?")?;
                }
                Ok(())
            }
            Self::UnknownMetric {
                view_name,
                name,
                available,
                suggestion,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': unknown metric '{name}'. Available: [{}]",
                    available.join(", ")
                )?;
                if let Some(s) = suggestion {
                    write!(f, ". Did you mean '{s}'?")?;
                }
                Ok(())
            }
            Self::DuplicateDimension { view_name, name } => {
                write!(
                    f,
                    "semantic view '{view_name}': duplicate dimension '{name}'"
                )
            }
            Self::DuplicateMetric { view_name, name } => {
                write!(f, "semantic view '{view_name}': duplicate metric '{name}'")
            }
            Self::FanTrap { detail } => {
                let FanTrapError {
                    view_name,
                    metric_name,
                    metric_table,
                    dimension_name,
                    dimension_table,
                    relationship_name,
                } = &**detail;
                write!(
                    f,
                    "semantic view '{view_name}': fan trap detected -- metric '{metric_name}' \
                     (table '{metric_table}') would be duplicated when joined to dimension \
                     '{dimension_name}' (table '{dimension_table}') via relationship \
                     '{relationship_name}' (many-to-one cardinality, inferred: FK is not PK/UNIQUE). \
                     This would inflate aggregation results. \
                     Remove the dimension, use a metric from the same table, or restructure the \
                     relationship."
                )
            }
            Self::MetricFanTrap { detail } => {
                let MetricFanTrapError {
                    view_name,
                    metric_name,
                    metric_table,
                    other_metric_name,
                    other_metric_table,
                    relationship_name,
                } = &**detail;
                write!(
                    f,
                    "semantic view '{view_name}': fan trap detected -- metric '{metric_name}' \
                     (table '{metric_table}') and metric '{other_metric_name}' (table \
                     '{other_metric_table}') aggregate at different grains: joining their source \
                     tables via relationship '{relationship_name}' (many-to-one cardinality) \
                     duplicates rows of '{metric_table}' and would inflate '{metric_name}'. \
                     Query the metrics separately, or restructure the relationship."
                )
            }
            Self::RootGrainFanTrap {
                view_name,
                metric_name,
                metric_table,
                relationship_name,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': fan trap detected -- metric '{metric_name}' \
                     (table '{metric_table}') aggregates a table that fans out relative to the \
                     query's base table via relationship '{relationship_name}' (many-to-one \
                     cardinality): the query is anchored FROM the base table, so '{metric_table}' \
                     rows are duplicated once per base-table row and '{metric_name}' would be \
                     inflated. Query this metric at the base table's grain, or restructure the \
                     relationship."
                )
            }
            Self::UncheckableDefinition { view_name, reason } => {
                write!(
                    f,
                    "semantic view '{view_name}': cannot verify the query is safe from fan traps \
                     -- the stored definition's relationship graph is unusable: {reason}. \
                     The definition likely predates current validation rules; re-create it with \
                     CREATE OR REPLACE SEMANTIC VIEW."
                )
            }
            Self::AmbiguousPath {
                view_name,
                dimension_name,
                dimension_table,
                available_relationships,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': dimension '{dimension_name}' is ambiguous -- \
                     table '{dimension_table}' is reached via multiple relationships: [{}]. \
                     Give every metric in the query a USING that names one of them, or use a \
                     dimension from a non-ambiguous table. If every metric already has one, \
                     this query cannot be split by path (two window metrics, or metrics \
                     beside a semi-additive one): query the metrics separately.",
                    available_relationships.join(", ")
                )
            }
            Self::AmbiguousDescendantPath {
                view_name,
                dimension_name,
                dimension_table,
                role_playing_table,
                available_relationships,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': dimension '{dimension_name}' is ambiguous -- \
                     its table '{dimension_table}' is reachable only through the role-playing \
                     table '{role_playing_table}', which is joined via multiple relationships: \
                     [{}]. Give every metric in the query a USING that names one of them, or use \
                     a dimension from a non-ambiguous table. If every metric already has one, \
                     this query cannot be split by path (two window metrics, or metrics \
                     beside a semi-additive one): query the metrics separately.",
                    available_relationships.join(", ")
                )
            }
            Self::AmbiguousFactPath {
                view_name,
                fact_name,
                fact_table,
                role_playing_table,
                available_relationships,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': fact '{fact_name}' is ambiguous -- reaching its \
                     table '{fact_table}' requires the role-playing table '{role_playing_table}', \
                     joined via multiple relationships: [{}], and fact queries carry no USING \
                     context to pick a role. Restructure the relationship or query via a \
                     non-role-playing table.",
                    available_relationships.join(", ")
                )
            }
            Self::AmbiguousWhereClausePath {
                view_name,
                member_name,
                member_table,
                role_playing_table,
                available_relationships,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': where_clause member '{member_name}' is \
                     ambiguous -- reaching its table '{member_table}' requires the role-playing \
                     table '{role_playing_table}', joined via multiple relationships: [{}], and \
                     a where_clause carries no USING context to pick a role. Filter on a member \
                     from a non-role-playing table, or give the target table a distinct alias \
                     per role.",
                    available_relationships.join(", ")
                )
            }
            Self::AmbiguousMemberFactPath { detail } => {
                let AmbiguousMemberFactPathError {
                    view_name,
                    member_name,
                    fact_name,
                    fact_table,
                    role_playing_table,
                    available_relationships,
                } = detail.as_ref();
                write!(
                    f,
                    "semantic view '{view_name}': '{member_name}' is ambiguous -- it references \
                     fact '{fact_name}' on table '{fact_table}', which requires the role-playing \
                     table '{role_playing_table}', joined via multiple relationships: [{}]. A \
                     metric over a fact on '{role_playing_table}' itself must name exactly one \
                     of them with USING; a dimension, or a fact on a table reached only through \
                     '{role_playing_table}', has no way to pick a role.",
                    available_relationships.join(", ")
                )
            }
            Self::AmbiguousMetricPath { detail } => {
                let AmbiguousMetricPathError {
                    view_name,
                    metric_name,
                    metric_table,
                    role_playing_table,
                    available_relationships,
                } = detail.as_ref();
                write!(
                    f,
                    "semantic view '{view_name}': metric '{metric_name}' is ambiguous -- it \
                     aggregates table '{metric_table}', which requires the role-playing table \
                     '{role_playing_table}', joined via multiple relationships: [{}]. A metric \
                     on that table cannot name a role (USING relationships must originate from \
                     the metric's own table); declare it on the referencing table with USING \
                     instead, e.g. <table>.<metric> USING (<relationship>) AS SUM(<fact>).",
                    available_relationships.join(", ")
                )
            }
            Self::PrivateMetric { view_name, name } => {
                write!(
                    f,
                    "semantic view '{view_name}': metric '{name}' is private and cannot be queried directly. \
                     Private metrics can only be used in derived metric expressions."
                )
            }
            Self::PrivateFact { view_name, name } => {
                write!(
                    f,
                    "semantic view '{view_name}': fact '{name}' is private and cannot be queried directly. \
                     Private facts can only be used in derived expressions."
                )
            }
            Self::FactsMetricsMutualExclusion { view_name } => {
                write!(
                    f,
                    "semantic view '{view_name}': cannot combine facts and metrics in the same query. \
                     Use FACTS or METRICS (facts := [...] or metrics := [...]), not both."
                )
            }
            Self::FactsWithDimensions { view_name } => {
                write!(
                    f,
                    "semantic view '{view_name}': querying FACTS with DIMENSIONS is not supported. \
                     List the dimensions in FACTS for one row per source row, or the facts in \
                     DIMENSIONS for one row per distinct value."
                )
            }
            Self::UnknownFact {
                view_name,
                name,
                available,
                suggestion,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': unknown fact '{name}'. Available: [{}]",
                    available.join(", ")
                )?;
                if let Some(s) = suggestion {
                    write!(f, ". Did you mean '{s}'?")?;
                }
                Ok(())
            }
            Self::DuplicateFact { view_name, name } => {
                write!(f, "semantic view '{view_name}': duplicate fact '{name}'")
            }
            Self::FactPathViolation {
                view_name,
                table_a,
                table_b,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': fact query references objects from incompatible \
                     table paths -- neither table '{table_a}' nor '{table_b}' can be reached from \
                     the other without crossing a one-to-many relationship, so joining them would \
                     duplicate the rows returned"
                )
            }
            Self::WhereClauseReferencesMetric {
                view_name,
                metric_name,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': where_clause cannot reference the metric \
                     '{metric_name}' -- the filter is applied before metrics are computed, so \
                     only dimensions and facts (and expressions over them) can appear in it"
                )
            }
            Self::WhereClauseNotOnePredicate { view_name } => {
                write!(
                    f,
                    "semantic view '{view_name}': where_clause is one predicate, so it cannot \
                     contain ';' outside a string"
                )
            }
            Self::WhereClauseParameter {
                view_name,
                parameter,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': where_clause cannot hold a prepared \
                     statement parameter such as {parameter}; pass the values in params \
                     and refer to params as $1, $2, ..."
                )
            }
            Self::WhereClauseParamMissing {
                view_name,
                reference,
                supplied,
            } => {
                let values = if *supplied == 1 { "value" } else { "values" };
                write!(
                    f,
                    "semantic view '{view_name}': where_clause refers to {reference}, but \
                     params has {supplied} {values}"
                )
            }
            Self::WhereClauseFanTrap {
                view_name,
                metric_name,
                member_name,
                member_table,
                relationship_name,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': fan trap detected -- filtering on \
                     '{member_name}' (table '{member_table}') requires joining that table, and \
                     relationship '{relationship_name}' fans out along the way, so metric \
                     '{metric_name}' would be aggregated over multiplied rows. Filter on a \
                     member reachable from the metric's table without fanning out."
                )
            }
            Self::ReferencedFactFanTrap { detail } => {
                let ReferencedFactFanTrapError {
                    view_name,
                    member_name,
                    member_table,
                    fact_name,
                    fact_table,
                    relationship_name,
                } = &**detail;
                write!(
                    f,
                    "semantic view '{view_name}': fan trap detected -- '{member_name}' (table \
                     '{member_table}') references the fact '{fact_name}' on table \
                     '{fact_table}', and relationship '{relationship_name}' fans out on the way \
                     there, so joining it would multiply '{member_name}'s rows. Reference a fact \
                     on a table reachable without fanning out, or define the fact on \
                     '{member_table}'."
                )
            }
            Self::WindowAggregateMixing {
                view_name,
                window_metrics,
                aggregate_metrics,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': cannot mix window function metrics [{}] \
                     with aggregate metrics [{}] in the same query",
                    window_metrics.join(", "),
                    aggregate_metrics.join(", ")
                )
            }
            Self::WindowMetricRequiredDimension {
                view_name,
                metric_name,
                dimension_name,
                reason,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': window function metric '{metric_name}' requires \
                     dimension '{dimension_name}' to be included in the query (used in {reason})"
                )
            }
            Self::CatalogPoisoned { view_name } => {
                write!(
                    f,
                    "semantic view '{view_name}': internal error -- catalog lock is poisoned \
                     (a previous operation panicked). Restart DuckDB to recover."
                )
            }
            Self::CycleDetected {
                view_name,
                cycle_description,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': cycle detected in metric/fact dependencies \
                     during query expansion: {cycle_description}"
                )
            }
            Self::MaxDepthExceeded {
                view_name,
                depth,
                max_depth,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': derived metric nesting depth {depth} exceeds \
                     maximum allowed depth of {max_depth}"
                )
            }
            Self::SemiAdditiveCoQueryUnsupported {
                view_name,
                metric_name,
                metric_expr,
                semi_metric_name,
                reason,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': metric '{metric_name}' (expression: \
                     {metric_expr}) cannot be co-queried with semi-additive metric \
                     '{semi_metric_name}': {reason}. Snapshot expansion for NON ADDITIVE BY \
                     requires every co-queried metric to be a single aggregate call \
                     SUM/COUNT/AVG/MIN/MAX(<expression>) without '*', DISTINCT, or surrounding \
                     expression text. Query '{metric_name}' and '{semi_metric_name}' separately."
                )
            }
            Self::SemiAdditiveThroughDependency {
                view_name,
                metric_name,
                semi_metric_name,
                non_additive_by,
            } => {
                let dims = non_additive_by.join(", ");
                write!(
                    f,
                    "semantic view '{view_name}': metric '{metric_name}' depends on \
                     semi-additive metric '{semi_metric_name}', whose NON ADDITIVE BY \
                     ({dims}) cannot be honoured through the dependency — the snapshot \
                     would be discarded and the result would not agree with \
                     '{semi_metric_name}' queried on its own. Query '{semi_metric_name}' \
                     directly, or add its NON ADDITIVE BY dimension(s) ({dims}) to the \
                     query, which makes it effectively regular."
                )
            }
            Self::SemiAdditiveUnsupportedExpression {
                view_name,
                metric_name,
                metric_expr,
                reason,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': semi-additive metric '{metric_name}' \
                     (expression: {metric_expr}) cannot be expanded: {reason}. NON ADDITIVE BY \
                     snapshot expansion requires the metric to be a single aggregate call \
                     SUM/COUNT/AVG/MIN/MAX(<expression>) without '*' or DISTINCT."
                )
            }
            Self::CountStarRequiresPrimaryKey {
                view_name,
                metric_name,
                table_alias,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': metric '{metric_name}' aggregates over COUNT(*) \
                     or a constant argument (COUNT(1), SUM(1), ...) on joined table \
                     '{table_alias}'. The generated LEFT JOIN produces one NULL-extended \
                     row per base-table row with no match in '{table_alias}', and neither \
                     COUNT(*) nor an aggregate over a constant can tell that row apart from a \
                     real one -- so the expansion rewrites them against '{table_alias}'s primary \
                     key, but table '{table_alias}' has no PRIMARY KEY declared \
                     in the TABLES clause. Add PRIMARY KEY (cols) to '{table_alias}' or use an \
                     explicit column: COUNT({table_alias}.<column>)."
                )
            }
            Self::SemiAdditiveRankColumnUnresolved {
                view_name,
                metric_name,
            } => {
                write!(
                    f,
                    "semantic view '{view_name}': internal error -- semi-additive metric \
                     '{metric_name}' is not in any NON ADDITIVE BY group, so its snapshot rank \
                     column cannot be determined. This is a bug in the expansion planner, not a \
                     problem with the query or the view; please report it."
                )
            }
        }
    }
}

impl std::error::Error for ExpandError {}
