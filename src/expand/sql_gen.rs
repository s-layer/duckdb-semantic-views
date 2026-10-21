use crate::model::{AccessModifier, Dimension, Fact, Metric, SemanticViewDefinition};
use crate::util::suggest_closest;

use super::facts::{
    collect_transitive_metric_names, inline_derived_metrics_with_joins, inline_facts,
    toposort_facts,
};
use super::fan_trap::{check_fan_traps, validate_fact_table_path};
use super::join_resolver::resolve_joins_pkfk;
use super::resolution::{find_dimension, find_metric, quote_ident, quote_stored_ident};
use super::role_playing::{
    check_fact_role_playing_path, check_where_clause_role_playing_path, find_using_context,
};
use super::select_spec::{FromSource, GroupBy, SelectItem, SelectSpec};
use super::types::{ExpandError, QueryRequest, ResolvedDim};

/// An entity kind resolvable by name against a [`SemanticViewDefinition`]
/// (dimensions, metrics, facts). Encapsulates lookup, the PRIVATE-access
/// policy, and the three error variants so [`resolve_names`] takes the
/// definition plus the requested names — not nine positional closures (R-5).
///
/// Modelling the error variants per kind is what makes a slot transposition
/// unrepresentable: the old positional API let the dimension call sites pass
/// `DuplicateDimension` in the private-error slot (harmless only because
/// dimensions are never private), a mistake the compiler could not catch.
trait Resolvable: Sized {
    /// Find this entity by (possibly qualified) name in the definition.
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self>;
    /// Is this resolved entity PRIVATE — barred from direct querying?
    fn is_private(&self) -> bool;
    /// All declared names of this kind, for the not-found error + suggestion.
    fn available(def: &SemanticViewDefinition) -> Vec<String>;
    /// Error: the same entity was requested twice (keyed on resolved identity).
    fn duplicate_err(view_name: String, name: String) -> ExpandError;
    /// Error: no entity of this kind by that name.
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError;
    /// Error: the entity is PRIVATE. Never called for kinds whose
    /// [`is_private`](Self::is_private) is always `false`.
    fn private_err(view_name: String, name: String) -> ExpandError;
}

impl Resolvable for Fact {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        def.facts
            .iter()
            .find(|f| crate::ident::ident_matches(&f.name, name))
    }
    fn is_private(&self) -> bool {
        self.access == AccessModifier::Private
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.facts.iter().map(|f| f.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateFact { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownFact {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(view_name: String, name: String) -> ExpandError {
        ExpandError::PrivateFact { view_name, name }
    }
}

impl Resolvable for Dimension {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        find_dimension(def, name)
    }
    fn is_private(&self) -> bool {
        // Dimensions carry no access modifier — never private.
        false
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.dimensions.iter().map(|d| d.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateDimension { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownDimension {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(_view_name: String, _name: String) -> ExpandError {
        // `is_private` is always false for dimensions, so `resolve_names`
        // never reaches this. There is no `PrivateDimension` variant; the old
        // positional API filled this slot with `DuplicateDimension` (dead but
        // misleading) — the trait removes the footgun entirely.
        unreachable!("dimensions cannot be private")
    }
}

impl Resolvable for Metric {
    fn find<'a>(def: &'a SemanticViewDefinition, name: &str) -> Option<&'a Self> {
        find_metric(def, name)
    }
    fn is_private(&self) -> bool {
        self.access == AccessModifier::Private
    }
    fn available(def: &SemanticViewDefinition) -> Vec<String> {
        def.metrics.iter().map(|m| m.name.clone()).collect()
    }
    fn duplicate_err(view_name: String, name: String) -> ExpandError {
        ExpandError::DuplicateMetric { view_name, name }
    }
    fn unknown_err(
        view_name: String,
        name: String,
        available: Vec<String>,
        suggestion: Option<String>,
    ) -> ExpandError {
        ExpandError::UnknownMetric {
            view_name,
            name,
            available,
            suggestion,
        }
    }
    fn private_err(view_name: String, name: String) -> ExpandError {
        ExpandError::PrivateMetric { view_name, name }
    }
}

/// Resolve a list of requested names to their [`Resolvable`] definitions,
/// checking for unknown names, duplicates, and PRIVATE access.
///
/// Duplicate detection keys on the RESOLVED item's identity, not the raw
/// request string (SG-14): `region` and `o.region` resolve to the same
/// dimension and are rejected as duplicates instead of emitting the same
/// column twice.
fn resolve_names<'a, T: Resolvable, N: AsRef<str>>(
    names: &[N],
    view_name: &str,
    def: &'a SemanticViewDefinition,
) -> Result<Vec<&'a T>, ExpandError> {
    let mut resolved = Vec::with_capacity(names.len());
    let mut seen: std::collections::HashSet<*const T> = std::collections::HashSet::new();
    for name in names {
        let name_str = name.as_ref();
        let item = T::find(def, name_str).ok_or_else(|| {
            let available = T::available(def);
            let suggestion = suggest_closest(name_str, &available);
            T::unknown_err(
                view_name.to_string(),
                name_str.to_string(),
                available,
                suggestion,
            )
        })?;
        if !seen.insert(std::ptr::from_ref(item)) {
            return Err(T::duplicate_err(
                view_name.to_string(),
                name_str.to_string(),
            ));
        }
        if item.is_private() {
            return Err(T::private_err(view_name.to_string(), name_str.to_string()));
        }
        resolved.push(item);
    }
    Ok(resolved)
}

/// The `<child pk> IS NOT NULL` predicate that removes phantom (NULL-extended)
/// rows from a ROW-LEVEL query, or `None` when there is nothing to remove
/// (EXP-28, EXP-29 — code-review 2026-08-08).
///
/// Every synthesized join is a LEFT JOIN anchored at the base table, so a base
/// row with no matching child row survives as one all-NULL row. An AGGREGATE
/// query fences that row inside each aggregate (SG-8 + `guard_aggregate_args`),
/// but a fact query and a dimensions-only `SELECT DISTINCT` emit rows
/// directly — the phantom becomes a result row that does not exist in the data,
/// and its NULLs are indistinguishable from genuine ones.
///
/// `member_tables` are the `source_table`s of the queried members (`None` means
/// the base table, which is never NULL-extended). The guard applies only when
/// they all name ONE table that is **below** the base — reached by following
/// foreign keys INTO the base, so it sits at a finer grain — and that table
/// declares a PRIMARY KEY. That table is then the query's grain, and filtering
/// to its real rows is exactly "anchor at the common grain table, joining up
/// for the rest", which stays fan-free because the walk from a child to its
/// parents is many-to-one.
///
/// The direction is load-bearing. A member on a table ABOVE the base (a parent,
/// reached by the base's own foreign key) is an ATTRIBUTE of each base row: a
/// base row whose foreign key is NULL or dangling is still a row of the view,
/// and its NULL attribute is part of the answer, not a join artifact. Filtering
/// there would delete real rows — `multi_hop_join_proptest` catches it
/// immediately. Below the base the situation is reversed: the base row has no
/// counterpart at the finer grain at all, so the NULL-extended row is
/// manufactured.
///
/// When the members span several tables the natural grain is ambiguous (a base
/// member legitimately keeps its row even with no child), and when the table
/// declares no PRIMARY KEY there is no column that distinguishes a phantom;
/// both keep today's behaviour. See TECH-DEBT #67.
fn phantom_row_filter(
    def: &SemanticViewDefinition,
    member_tables: &[Option<String>],
) -> Option<String> {
    let base = def.tables.first()?.alias.to_ascii_lowercase();
    let mut grain: Option<String> = None;
    for member in member_tables {
        let alias = member
            .as_ref()
            .map_or_else(|| base.clone(), |source| source.to_ascii_lowercase());
        match &grain {
            None => grain = Some(alias),
            Some(seen) if *seen == alias => {}
            Some(_) => return None, // members span several tables
        }
    }
    let alias = grain?;
    if alias == base || !reaches_base_by_foreign_key(def, &alias, &base) {
        return None;
    }
    let pk = def
        .tables
        .iter()
        .find(|t| t.alias.to_ascii_lowercase() == alias)?
        .pk_columns
        .first()?;
    Some(format!(
        "{}.{} IS NOT NULL",
        quote_ident(&alias),
        quote_ident(pk)
    ))
}

/// Whether `alias` sits BELOW `base` in the relationship graph — i.e. following
/// its declared foreign keys upwards (`from_alias` -> `table`, the referencing
/// side to the referenced side) reaches the base table.
///
/// Breadth-first with a visited set, so a diamond costs no more than a chain and
/// a mis-declared cycle terminates instead of looping.
fn reaches_base_by_foreign_key(def: &SemanticViewDefinition, alias: &str, base: &str) -> bool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue = std::collections::VecDeque::from([alias.to_ascii_lowercase()]);
    while let Some(current) = queue.pop_front() {
        if !seen.insert(current.clone()) {
            continue;
        }
        for join in &def.joins {
            if join.from_alias.to_ascii_lowercase() != current {
                continue;
            }
            let referenced = join.table.to_ascii_lowercase();
            if referenced == base {
                return true;
            }
            queue.push_back(referenced);
        }
    }
    false
}

/// `AND` the phantom-row guard onto the query's own predicate.
///
/// The user's predicate is parenthesized so an `OR` at its top level cannot
/// swallow the guard. With no guard the predicate is passed through byte for
/// byte — the emitted SQL of every query that has no phantom to remove is
/// unchanged.
fn and_phantom_filter(predicate: Option<String>, guard: Option<String>) -> Option<String> {
    match (predicate, guard) {
        (Some(p), Some(g)) => Some(format!("({p}) AND {g}")),
        (Some(p), None) => Some(p),
        (None, g) => g,
    }
}

/// Expand a fact query into unaggregated SQL.
///
/// Facts are row-level expressions — the generated SQL has no GROUP BY and no
/// aggregation. Fact expressions are resolved via `inline_facts` (DAG resolution)
/// just like metric expansion inlines facts into aggregate expressions.
///
/// Dimensions, when present, add columns to SELECT but do NOT trigger GROUP BY
/// (unlike metric queries where dims + metrics => GROUP BY).
#[allow(clippy::too_many_lines)]
fn expand_facts(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // 1. Validate + resolve requested facts.
    let resolved_facts = resolve_names::<Fact, _>(&req.facts, view_name, def)?;

    // 2. Resolve requested dimensions (same logic as expand()).
    let resolved_dims = resolve_names::<Dimension, _>(&req.dimensions, view_name, def)?;

    // 2b. Resolve the pre-aggregation predicate, if any. Done before the path
    // check so the members it references take part in it: Snowflake's
    // same-logical-table rule counts WHERE-clause members explicitly ("all facts
    // and dimensions used in the query, INCLUDING those specified in the WHERE
    // clause"). A filter on a table that fans out against the queried facts is
    // the same row-multiplication hazard as selecting from it.
    let resolved_where = req
        .where_clause
        .as_deref()
        // A blank predicate is absent, not an empty condition: `Some("")` would
        // otherwise emit a bare `WHERE ` with nothing after it. The FFI maps an
        // empty parameter to None, but `expand()` is public and reachable
        // directly (found by `fuzz_where_predicate`).
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| super::where_clause::resolve_where_clause(view_name, def, raw))
        .transpose()?;

    // 3. Validate table path constraint (FACT-04).
    let fact_tables: Vec<String> = resolved_facts
        .iter()
        .filter_map(|f| f.source_table.clone())
        .collect();
    let mut dim_tables: Vec<String> = resolved_dims
        .iter()
        .filter_map(|d| d.source_table.clone())
        .collect();
    if let Some(rw) = &resolved_where {
        dim_tables.extend(rw.source_tables.iter().cloned());
    }
    validate_fact_table_path(view_name, def, &fact_tables, &dim_tables)?;

    // 3b. Role-playing ambiguity detection (SG-17), mirroring the metrics
    // path in expand(). Fact queries carry no metrics, so there is never a
    // USING context to disambiguate: a dimension on a table reached by
    // multiple named relationships always raises AmbiguousPath here — the
    // same error the metrics path raises when no co-queried metric supplies
    // USING. Previously the facts path skipped this check and silently bound
    // the dimension to an arbitrary relationship edge.
    for dim in &resolved_dims {
        let _ = find_using_context(view_name, def, dim, &[])?;
    }

    // 3c. EXP-5: a fact sourced on (or reached only through) a role-playing
    // table has no USING context to pick a role — reject rather than silently
    // binding to the first-declared relationship, mirroring the dimension
    // check above.
    for fact in &resolved_facts {
        check_fact_role_playing_path(view_name, def, fact)?;
    }
    // TECH-DEBT #69: a queried dimension's expression may reach a fact on a
    // role-playing path too (#54 inlines it), and a fact query has no USING.
    super::role_playing::check_member_fact_role_playing_path(
        view_name,
        def,
        &resolved_dims,
        &[],
        &[],
    )?;

    // 3d. EXP-10: a `where_clause` member on (or reached only through) a
    // role-playing table has no way to name its role either — the predicate is
    // spliced as the member's own expression, never rewritten to a scoped
    // alias. Same reasoning as 3b/3c.
    if let Some(rw) = &resolved_where {
        check_where_clause_role_playing_path(view_name, def, &rw.members)?;
    }

    // 4. Resolve fact expressions via DAG inlining (fact-to-fact dependencies).
    let topo_order = toposort_facts(&def.facts).map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;

    // 5. Build the SELECT list (no DISTINCT, no aggregation).
    let mut items: Vec<SelectItem> = Vec::new();

    // Dimensions first
    for dim in &resolved_dims {
        items.push(SelectItem::new(
            super::facts::inline_dimension_facts(&dim.expr, &def.facts),
            dim.output_type.clone(),
            quote_stored_ident(&dim.name),
        ));
    }

    // Then facts (inlined expressions, no aggregation)
    for fact in &resolved_facts {
        let resolved_expr = inline_facts(&fact.expr, &def.facts, &topo_order);
        items.push(SelectItem::new(
            resolved_expr,
            fact.output_type.clone(),
            quote_stored_ident(&fact.name),
        ));
    }

    // 6. JOIN clauses — resolve required joins for dim + fact source tables.
    // Fact queries have no metrics; fact source tables are resolved through
    // the same path walk as dimensions (SG-10) and their joins are appended
    // after the dimension-driven joins.
    // Tables named only by the predicate still have to be joined, or the
    // filter would reference an alias that is not in the FROM.
    let mut fact_sources: Vec<String> = resolved_facts
        .iter()
        .filter_map(|f| f.source_table.clone())
        .collect();
    // PAR-6, the facts-path sibling: a queried fact's own expression may
    // reference a fact on a THIRD table, which `inline_facts` splices in here
    // exactly as it does inside a metric. `source_table` names only the fact's
    // own table, so the chain has to be walked for the rest.
    for fact in &resolved_facts {
        fact_sources.extend(super::facts::collect_referenced_fact_tables(
            &fact.expr, &def.facts,
        ));
    }
    if let Some(rw) = &resolved_where {
        fact_sources.extend(rw.source_tables.iter().cloned());
    }
    let joins = resolve_joins_pkfk(def, &resolved_dims, &[], &fact_sources);

    // 7. A fact query is an unaggregated top-level SELECT over the base table
    //    (+ joins): no DISTINCT, no GROUP BY. The predicate is a plain WHERE —
    //    nothing is aggregated, so there is no "before" to be careful about.
    //    EXP-28: when every queried fact lives on one LEFT-JOINed table, that
    //    table is the query's grain and its phantom rows are filtered out.
    let fact_member_tables: Vec<Option<String>> = resolved_facts
        .iter()
        .map(|f| f.source_table.clone())
        .collect();
    let phantom_guard = phantom_row_filter(def, &fact_member_tables);
    Ok(SelectSpec {
        where_clause: and_phantom_filter(resolved_where.map(|rw| rw.sql), phantom_guard),
        distinct: false,
        items,
        from: FromSource::BaseTable { def, joins },
        group_by: GroupBy::None,
    }
    .render())
}

/// Expand a semantic view definition into a SQL query string.
///
/// Takes a view name (for error messages), its definition, and a query request
/// specifying which dimensions and metrics to include. Returns the generated SQL
/// or an `ExpandError` if the request is invalid.
///
/// # Errors
///
/// Returns `ExpandError` if:
/// - Neither dimensions nor metrics are requested (`EmptyRequest`)
/// - A requested dimension or metric name is not found (`UnknownDimension`, `UnknownMetric`)
/// - A dimension or metric name is duplicated (`DuplicateDimension`, `DuplicateMetric`)
pub fn expand(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // The predicate's subqueries become CTEs ahead of the expansion, so that
    // they are uncorrelated (`where_clause::hoist_subqueries`). What is left in
    // the predicate is resolved as before: a `(SELECT * FROM __sv_sqN)` is
    // still a subquery to the member scan.
    let Some(hoisted) = req
        .where_clause
        .as_deref()
        .map(super::where_clause::hoist_subqueries)
        .filter(|h| !h.ctes.is_empty())
    else {
        return expand_unhoisted(view_name, def, req);
    };
    let req = QueryRequest {
        where_clause: Some(hoisted.predicate),
        ..req.clone()
    };
    let sql = expand_unhoisted(view_name, def, &req)?;
    Ok(super::where_clause::with_ctes(&sql, &hoisted.ctes))
}

/// [`expand`], for a predicate whose subqueries are already hoisted.
#[allow(clippy::too_many_lines)]
fn expand_unhoisted(
    view_name: &str,
    def: &SemanticViewDefinition,
    req: &QueryRequest,
) -> Result<String, ExpandError> {
    // 0. Facts and metrics are mutually exclusive.
    if !req.facts.is_empty() && !req.metrics.is_empty() {
        return Err(ExpandError::FactsMetricsMutualExclusion {
            view_name: view_name.to_string(),
        });
    }

    // 1. Validate: at least one dimension, metric, or fact is required.
    if req.dimensions.is_empty() && req.metrics.is_empty() && req.facts.is_empty() {
        return Err(ExpandError::EmptyRequest {
            view_name: view_name.to_string(),
        });
    }

    // Dispatch to fact expansion path when facts are requested.
    if !req.facts.is_empty() {
        return expand_facts(view_name, def, req);
    }

    // 2. Resolve requested dimensions to their definitions.
    let resolved_dims = resolve_names::<Dimension, _>(&req.dimensions, view_name, def)?;

    // 3. Resolve requested metrics to their definitions.
    // Phase 43: PRIVATE access check -- private metrics cannot be queried directly.
    // Derived metrics that reference private bases still work because
    // inline_derived_metrics resolves expressions, not access modifiers.
    let resolved_mets = resolve_names::<Metric, _>(&req.metrics, view_name, def)?;

    // Resolve the pre-aggregation predicate up front: it decides materialization
    // routing below, participates in the fan-trap checks, and is rejected by the
    // strategies that cannot yet inject it.
    let resolved_where = req
        .where_clause
        .as_deref()
        // A blank predicate is absent, not an empty condition: `Some("")` would
        // otherwise emit a bare `WHERE ` with nothing after it. The FFI maps an
        // empty parameter to None, but `expand()` is public and reachable
        // directly (found by `fuzz_where_predicate`).
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| super::where_clause::resolve_where_clause(view_name, def, raw))
        .transpose()?;

    // Phase 55: Materialization routing.
    // Attempt to route to a pre-aggregated table if an exact match exists.
    // Returns None if no match, or if any metric is semi-additive / window.
    //
    // A pre-aggregated table cannot answer a predicate: its rows are already
    // aggregated, so filtering them is a post-aggregation filter over whatever
    // members it happens to carry — not the pre-aggregation filter that was
    // asked for. Skip routing entirely whenever a predicate is present and
    // compute from the base tables (correct, if slower). Routing a filtered
    // query when every referenced member is a materialized dimension is a
    // separate, later change.
    if resolved_where.is_none() {
        if let Some(routed_sql) =
            super::materialization::try_route_materialization(def, &resolved_dims, &resolved_mets)
        {
            return Ok(routed_sql);
        }
    }

    // 4. Pre-compute all metric expressions: inline facts into base metrics,
    //    then inline metric references into derived metrics.
    let topo_order = toposort_facts(&def.facts).map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;
    let resolved = inline_derived_metrics_with_joins(
        &def.metrics,
        &def.facts,
        &topo_order,
        &def.tables,
        &def.joins,
    )
    .map_err(|e| ExpandError::CycleDetected {
        view_name: view_name.to_string(),
        cycle_description: e,
    })?;

    // TECH-DEBT #35 / v0.12.0: decide whether this query must be computed at
    // each metric's OWN grain. `plan` returns None for everything the
    // base-anchored path already answers correctly (so single-grain SQL is
    // unchanged) and for multi-grain shapes it cannot express (so those keep
    // their fan-trap error). Decided before the checks below because both of
    // them exist only to guard the base-anchored topology.
    // A `where_clause` member's tables are joined into whichever CTE evaluates
    // the predicate, so both decisions below have to account for them alongside
    // the dimension and metric tables.
    let where_tables: Vec<String> = resolved_where
        .as_ref()
        .map(|w| w.source_tables.clone())
        .unwrap_or_default();
    // EXP-22: a member the predicate names that declares NO source table is an
    // unqualified expression, exactly like the dimensions all three deciders
    // already refuse to re-anchor — it contributes no entry to `where_tables`,
    // so without this the deciders could not see it at all.
    let where_has_unqualified_member = resolved_where
        .as_ref()
        .is_some_and(|w| w.members.iter().any(|m| m.table.is_none()));
    let grain_plan = super::per_grain::plan(
        def,
        &resolved_dims,
        &resolved_mets,
        &resolved.exprs,
        &where_tables,
        where_has_unqualified_member,
    );

    // An all-window query whose inner aggregate lives at a non-root grain is
    // answered by anchoring `__sv_agg` there instead of at the base table
    // (TECH-DEBT #36). Decided BEFORE the checks below, because the checks the
    // anchor supersedes are the ones that would otherwise reject this shape —
    // the same reason `grain_plan` is decided before them.
    let window_anchor = super::per_grain::window_cte_anchor(
        def,
        &resolved_dims,
        &resolved_mets,
        &where_tables,
        where_has_unqualified_member,
    );

    // An ACTIVE semi-additive metric at a non-root grain is answered by
    // anchoring `__sv_snapshot` there instead of at the base table (TECH-DEBT
    // #36). Decided here for the same reason as `window_anchor`: the checks
    // below are the ones this supersedes, so it cannot be decided after them.
    //
    // `queried_dim_keys` are canonical (quote-stripped + folded) so a dotted or
    // quoted NA reference resolves against the queried dims (#30); the same set
    // is reused by the semi-additive dispatch further down.
    let queried_dim_keys: std::collections::HashSet<String> = resolved_dims
        .iter()
        .map(|d| crate::ident::normalize_ident_part(&d.name))
        .collect();
    let has_active_semi_additive = resolved_mets
        .iter()
        .any(|m| super::semi_additive::is_active_semi_additive(def, m, &queried_dim_keys));
    // EXP-19/EXP-20 (code-review 2026-08-06): `is_active_semi_additive` asks
    // only about a metric's OWN `non_additive_by`, so a metric that merely
    // DEPENDS on an active semi-additive one — a derived metric referencing it,
    // or a window metric naming it as its inner aggregate — routed to the
    // regular path and inlined the dependency's raw aggregate, evaluating it
    // over every row and silently dropping `NON ADDITIVE BY`. The result did
    // not even agree with the same snapshot queried directly (`double_balance`
    // != 2 x `balance`).
    //
    // Checked here, before any dispatch, because every emission path shares the
    // defect: the base-anchored path inlines the raw aggregate, the window path
    // feeds it to `__sv_agg`, and per-grain's `is_snapshot_group` classifies by
    // the group's own metrics so the dependent metric lands in a PLAIN group.
    //
    // Composing a snapshot with an outer expression is a feature, not a patch
    // (TECH-DEBT #55); until it lands this errors, the same call SG-5 makes for
    // co-queried shapes its CTE cannot decompose. Iteration is over `def.metrics`
    // rather than the dependency `HashSet` so the metric named in the error is
    // deterministic (declaration order) when a metric reaches two of them.
    for met in &resolved_mets {
        if super::semi_additive::is_active_semi_additive(def, met, &queried_dim_keys) {
            continue; // Takes the snapshot path itself; SG-5 governs the rest.
        }
        let met_key = crate::ident::normalize_ident_part(&met.name);
        let deps = collect_transitive_metric_names(met, &def.metrics);
        for dep in &def.metrics {
            let dep_key = crate::ident::normalize_ident_part(&dep.name);
            if dep_key == met_key || !deps.contains(&dep_key) {
                continue;
            }
            if super::semi_additive::is_active_semi_additive(def, dep, &queried_dim_keys) {
                return Err(ExpandError::SemiAdditiveThroughDependency {
                    view_name: view_name.to_string(),
                    metric_name: met.name.clone(),
                    semi_metric_name: dep.name.clone(),
                    non_additive_by: dep
                        .non_additive_by
                        .iter()
                        .map(|na| na.dimension.clone())
                        .collect(),
                });
            }
        }
    }

    let snapshot_anchor = if has_active_semi_additive {
        let mut extra = super::semi_additive::na_dim_source_tables(
            view_name,
            def,
            &resolved_mets,
            &queried_dim_keys,
        );
        extra.extend(where_tables.iter().cloned());
        super::per_grain::snapshot_cte_anchor(
            def,
            &resolved_dims,
            &resolved_mets,
            &extra,
            where_has_unqualified_member,
        )
    } else {
        None
    };

    // SG-8: fail loudly when a REQUESTED metric (directly, via a derived
    // metric, or as a window metric's inner aggregate) depends on a COUNT(*)
    // that could not be rewritten to COUNT(<pk>) — a non-base source table
    // with no PRIMARY KEY declared. Emitting it as-is would count
    // NULL-extended LEFT JOIN rows (one per childless base row).
    //
    // On the per-grain path there is no such row: the metric's table anchors
    // its own CTE instead of being LEFT JOINed to the base table, so a bare
    // COUNT(*) counts exactly that table's rows and needs no PRIMARY KEY.
    //
    // An anchored window CTE removes the row for the same reason: `__sv_agg`
    // anchors at the inner aggregate's own table, so a bare COUNT(*) there
    // counts exactly that table's rows. The one shape that would re-fan it is a
    // dimension BELOW the anchor's grain, which pulls a "many"-side join back
    // into the CTE — and the retained metric × dimension fan-trap check below
    // rejects that before emission (`anchored_window_count_star_...` pair in
    // `tests_per_grain`). PR #175 review: without this the guard rejected
    // eligible anchored-window queries, because it ran while `window_anchor`
    // was still computed further down.
    if grain_plan.is_none()
        && window_anchor.is_none()
        && snapshot_anchor.is_none()
        && !resolved.count_star_no_pk.is_empty()
    {
        for met in &resolved_mets {
            for name in collect_transitive_metric_names(met, &def.metrics) {
                if let Some(table_alias) = resolved.count_star_no_pk.get(&name) {
                    let metric_name = def
                        .metrics
                        .iter()
                        .find(|m| crate::ident::ident_matches(&m.name, &name))
                        .map_or(name.clone(), |m| m.name.clone());
                    return Err(ExpandError::CountStarRequiresPrimaryKey {
                        view_name: view_name.to_string(),
                        metric_name,
                        table_alias: table_alias.clone(),
                    });
                }
            }
        }
    }
    let resolved_exprs = resolved.exprs;

    // Phase 31: Check for fan traps before generating SQL. In per-grain mode the
    // fence keeps only the metric × dimension check — the other two guard the
    // base-anchored topology the per-grain plan replaces. An anchored window CTE
    // replaces that topology in exactly the same way, so it uses the same mode;
    // the retained metric × dimension check is what still reports a dimension
    // below the metric's own grain.
    check_fan_traps(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
        grain_plan.is_some() || window_anchor.is_some() || snapshot_anchor.is_some(),
    )?;
    // PAR-6 (TECH-DEBT #53): a member reaching a fact on another table now
    // pulls that table's join, so the fence has to rule out a fanning one.
    // Runs on every emission path — the referenced fact is inlined inside its
    // member's expression, so per-grain aggregation does not separate them.
    super::fan_trap::check_referenced_fact_fan_traps(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
    )?;
    // TECH-DEBT #69: and the role-playing seam beside it — a referenced fact on
    // a role-playing path binds to a role only through a base metric's single
    // `USING`; anything else would join the first-declared relationship. The
    // `where_clause` tables decide the base-table exemption.
    super::role_playing::check_member_fact_role_playing_path(
        view_name,
        def,
        &resolved_dims,
        &resolved_mets,
        &where_tables,
    )?;
    if let Some(rw) = &resolved_where {
        super::fan_trap::check_where_clause_fan_traps(view_name, def, &rw.members, &resolved_mets)?;
        // EXP-10: and the role-playing seam the fan-trap check does not cover —
        // reaching the member's table is unambiguous only if no role-playing
        // table sits on the path. Runs on every emission path because it is
        // decided here, above the per-grain / anchored / base-anchored split.
        check_where_clause_role_playing_path(view_name, def, &rw.members)?;
    }

    if let Some(plan) = grain_plan {
        // The predicate goes inside EACH grain CTE, so every metric aggregates
        // over only the matching rows. On the outer query it would instead
        // filter the already-combined result — a post-aggregation filter
        // wearing a pre-aggregation name.
        return super::per_grain::expand_per_grain(
            view_name,
            def,
            &resolved_dims,
            &resolved_mets,
            &resolved_exprs,
            &plan,
            resolved_where.as_ref(),
        );
    }

    // Phase 32: pair each resolved dimension with its role-playing scoped alias
    // (e.g. "a__dep_airport"). R-8 (code-review 2026-07-11): zipped into
    // `ResolvedDim` so the alias travels with its dimension instead of a
    // position-indexed side array (`dim_scoped_aliases[i]`).
    let mut resolved: Vec<ResolvedDim> = Vec::with_capacity(resolved_dims.len());
    for &dim in &resolved_dims {
        let scoped_alias = find_using_context(view_name, def, dim, &resolved_mets)?;
        resolved.push(ResolvedDim { dim, scoped_alias });
    }

    // Phase 47: a semi-additive metric only needs CTE treatment when at least
    // one of its NA dims is NOT in the queried dimension set. When ALL are in
    // the query it acts as regular (Snowflake semantics). Decided above, with
    // the snapshot anchor that depends on it.
    if has_active_semi_additive {
        return super::semi_additive::expand_semi_additive(
            view_name,
            def,
            &resolved,
            &resolved_mets,
            &resolved_exprs,
            resolved_where.as_ref(),
            snapshot_anchor.as_deref(),
        );
    }

    // Phase 48: Check if any resolved metric is a window function metric.
    let has_window = resolved_mets.iter().any(|m| m.is_window());
    if has_window {
        // Window metrics cannot be mixed with aggregate metrics.
        let window_names: Vec<String> = resolved_mets
            .iter()
            .filter(|m| m.is_window())
            .map(|m| m.name.clone())
            .collect();
        let aggregate_names: Vec<String> = resolved_mets
            .iter()
            .filter(|m| !m.is_window())
            .map(|m| m.name.clone())
            .collect();
        if !aggregate_names.is_empty() {
            return Err(ExpandError::WindowAggregateMixing {
                view_name: view_name.to_string(),
                window_metrics: window_names,
                aggregate_metrics: aggregate_names,
            });
        }
        return super::window::expand_window_metrics(
            view_name,
            def,
            &resolved,
            &resolved_mets,
            &resolved_exprs,
            resolved_where.as_ref(),
            window_anchor.as_deref(),
        );
    }

    // 5. Build the top-level SELECT.
    //    Dimensions-only (no metrics): SELECT DISTINCT, no GROUP BY.
    //    Metrics-only (no dimensions): SELECT (global aggregate), no GROUP BY.
    //    Both: SELECT with an ordinal GROUP BY over the dimensions.
    let distinct = !resolved_dims.is_empty() && resolved_mets.is_empty();

    let mut items: Vec<SelectItem> = Vec::new();
    for rd in &resolved {
        let dim = rd.dim;
        let mut base_expr = super::facts::inline_dimension_facts(&dim.expr, &def.facts);
        // Phase 32: If this dimension has a scoped alias, rewrite the expression.
        if let Some(ref scoped) = rd.scoped_alias {
            if let Some(ref st) = dim.source_table {
                // Rewrite the source-table qualifier to the scoped alias
                // e.g., "a.city" -> "a__dep_airport.city"
                base_expr = super::join_resolver::rewrite_to_scoped_alias(&base_expr, st, scoped);
            }
        }
        items.push(SelectItem::new(
            base_expr,
            dim.output_type.clone(),
            quote_stored_ident(&dim.name),
        ));
    }
    for met in &resolved_mets {
        // Look up the pre-computed resolved expression (handles both base +
        // derived metrics) by the metric's canonical key, matching how
        // `inline_derived_metrics` keys the map (EXP-6).
        let resolved_expr = resolved_exprs
            .get(&crate::ident::normalize_ident_part(&met.name))
            .cloned()
            .unwrap_or_else(|| met.expr.clone());
        items.push(SelectItem::new(
            resolved_expr,
            met.output_type.clone(),
            quote_stored_ident(&met.name),
        ));
    }

    // 6. Join resolution via PK/FK graph.
    //    The resolver returns structured edges in emission order; role-playing
    //    scoped joins (e.g. "a__dep_airport") follow the bare joins.
    // Tables named only by the predicate must still be joined, or the filter
    // would reference an alias absent from the FROM.
    let where_tables: Vec<String> = resolved_where
        .as_ref()
        .map(|rw| rw.source_tables.clone())
        .unwrap_or_default();
    let joins = resolve_joins_pkfk(def, &resolved_dims, &resolved_mets, &where_tables);

    // 7. GROUP BY (only when both dimensions and metrics are present).
    //    Ordinal positions avoid ambiguity when an expression matches its alias
    //    (e.g. `status AS "status"`) — see push_group_by_ordinals (E-1).
    let group_by = if !resolved_dims.is_empty() && !resolved_mets.is_empty() {
        GroupBy::Ordinals(resolved_dims.len())
    } else {
        GroupBy::None
    };

    // EXP-29: a dimensions-only `SELECT DISTINCT` emits rows directly, so it has
    // the same phantom-row exposure as a fact query — a childless parent
    // contributes a manufactured NULL that reads as a genuine data NULL. Only
    // that branch: with metrics present the phantom is fenced inside each
    // aggregate instead. Role-played dimensions are skipped — their table is
    // joined under a scoped alias the guard does not name.
    let phantom_guard = if distinct && resolved.iter().all(|rd| rd.scoped_alias.is_none()) {
        let dim_member_tables: Vec<Option<String>> = resolved_dims
            .iter()
            .map(|d| d.source_table.clone())
            .collect();
        phantom_row_filter(def, &dim_member_tables)
    } else {
        None
    };

    Ok(SelectSpec {
        // Rendered between the joins and the GROUP BY, so rows are filtered on
        // their way INTO the aggregation — Snowflake's "applied before the
        // metrics are computed".
        where_clause: and_phantom_filter(resolved_where.map(|rw| rw.sql), phantom_guard),
        distinct,
        items,
        from: FromSource::BaseTable { def, joins },
        group_by,
    }
    .render())
}
