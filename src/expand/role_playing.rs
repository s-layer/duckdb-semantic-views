use std::collections::HashSet;

use crate::model::SemanticViewDefinition;

use super::facts::{
    collect_derived_metric_using, collect_referenced_facts, collect_transitive_metric_names,
};
use super::join_resolver::scoped_join_alias;
use super::types::{AmbiguousMemberFactPathError, AmbiguousMetricPathError, ExpandError};

/// Determine which relationships point to a given table alias in the definition.
///
/// Returns the names of the relationships that have `to_alias` as their
/// target, sorted.
/// Used for ambiguity detection: if a table is reached by multiple named relationships,
/// dimensions from that table require USING context to disambiguate.
pub(super) fn relationships_to_table(
    def: &SemanticViewDefinition,
    target_alias: &str,
) -> Vec<String> {
    let target_lower = target_alias.to_ascii_lowercase();
    let mut names: Vec<String> = def
        .joins
        .iter()
        .filter(|j| !j.fk_columns.is_empty() && j.table.to_ascii_lowercase() == target_lower)
        .filter_map(|j| j.name.clone())
        .collect();
    // Sorted, so a message listing them reads the same whatever order the
    // relationships were declared in.
    names.sort_unstable();
    names
}

/// Is `target_alias` a genuinely *role-playing* target — i.e. does any single
/// source table declare two or more named relationships to it (e.g.
/// `orders(dep_code) REFERENCES airports` + `orders(arr_code) REFERENCES
/// airports`)? Only then can one query legitimately need multiple aliased
/// instances of the target, making an unqualified dimension on it ambiguous.
///
/// Multiple relationships converging from *different* source tables
/// (`li -> o`, `p -> o`) are NOT role-playing: the target joins as a single
/// bare instance and path resolution picks the unique connecting edge for
/// whichever source the query actually reaches it from. (A query that reaches
/// it along two such paths is refused by `multi_path`, unless a `USING` picks
/// one.) Treating that shape as ambiguous broke plain
/// child-fact + parent-dimension queries.
pub(super) fn is_role_playing_target(def: &SemanticViewDefinition, target_alias: &str) -> bool {
    let target_lower = target_alias.to_ascii_lowercase();
    let mut seen_from: HashSet<String> = HashSet::new();
    for j in &def.joins {
        if j.fk_columns.is_empty()
            || j.name.is_none()
            || j.table.to_ascii_lowercase() != target_lower
        {
            continue;
        }
        // A second relationship from the same source table to this target is
        // what makes it role-playing. `insert` returns false when the alias
        // was already present — linear time, no O(n²) `Vec::contains` scan.
        if !seen_from.insert(j.from_alias.to_ascii_lowercase()) {
            return true;
        }
    }
    false
}

/// A role-played table that a query reading rows from `from` (the metrics'
/// home tables, [`query_starts`]) crosses the other way round to reach
/// `table`: `f => a <- g`, a metric on `f` by a member on `g`, where `g` is
/// joined to whichever instance of `a` is joined. Only a start with no chain
/// to `table` is walked (a chain is [`role_playing_on_path`]'s), and only when
/// every path from it crosses those relationships (where some path does not,
/// there are two paths and no chain, which `multi_path` refuses). A start on
/// the role-played table itself reads its rows and names no role here.
///
/// Following the role a metric's `USING` names down to `g` is TECH-DEBT #111;
/// until then such a member is refused, with or without one.
fn role_crossed_backwards(
    def: &SemanticViewDefinition,
    from: &[String],
    table: &str,
) -> Option<String> {
    let table_lower = table.to_ascii_lowercase();
    let graph = crate::graph::RelationshipGraph::from_definition(def).ok()?;
    let mut crossed: Vec<String> = Vec::new();
    for start in from {
        let start = start.to_ascii_lowercase();
        if start == table_lower || graph_chain_reaches(&graph, &start, &table_lower) {
            continue;
        }
        if super::multi_path::undirected_path(def, &start, &table_lower, None).is_none() {
            continue;
        }
        for (source, rp) in role_playing_pairs(def) {
            // Not a role this walk picks: one on the start's own rows, or on
            // `table`'s (`table` being role-played, or the table that plays
            // the roles, whose own checks name them).
            if rp == start || rp == table_lower || source == table_lower {
                continue;
            }
            let avoided =
                super::multi_path::undirected_path(def, &start, &table_lower, Some((&source, &rp)));
            if avoided.is_none() && !crossed.contains(&rp) {
                crossed.push(rp);
            }
        }
    }
    crossed.sort_unstable();
    crossed.into_iter().next()
}

/// Each `(source, target)` pair of tables, lowercased, where `source` declares
/// two or more named relationships to `target` (the role-played table), sorted.
fn role_playing_pairs(def: &SemanticViewDefinition) -> Vec<(String, String)> {
    let mut counts: std::collections::BTreeMap<(String, String), usize> =
        std::collections::BTreeMap::new();
    for j in def
        .joins
        .iter()
        .filter(|j| !j.fk_columns.is_empty() && j.name.is_some())
    {
        *counts
            .entry((
                j.from_alias.to_ascii_lowercase(),
                j.table.to_ascii_lowercase(),
            ))
            .or_default() += 1;
    }
    counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(pair, _)| pair)
        .collect()
}

/// Whether `from` reaches `to` by a chain of relationships in `graph`.
fn graph_chain_reaches(graph: &crate::graph::RelationshipGraph, from: &str, to: &str) -> bool {
    let mut seen: HashSet<String> = HashSet::from([from.to_string()]);
    let mut stack = vec![from.to_string()];
    while let Some(current) = stack.pop() {
        if current == to {
            return true;
        }
        for next in graph.edges.get(&current).into_iter().flatten() {
            let next = next.to_ascii_lowercase();
            if seen.insert(next.clone()) {
                stack.push(next);
            }
        }
    }
    false
}

/// The first table in TABLES, lowercased: the table the base-anchored
/// emission reads rows from.
fn first_table(def: &SemanticViewDefinition) -> String {
    def.tables
        .first()
        .map(|t| t.alias.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The tables a query with `mets` reads rows from, each lowercased once: the
/// table of every metric each one is computed from (a derived metric's base
/// metrics, a window metric's inner one).
///
/// Empty for a query with no metrics, which [`role_playing_on_path`] reads as
/// "any table": such a query reaches these checks only when no single table
/// can be read for its members (`member_anchor` placed none), and falls to the
/// base-anchored emission, so any role-played table on the way to a member is
/// a role nothing in the query picks.
pub(super) fn query_starts(
    def: &SemanticViewDefinition,
    mets: &[&crate::model::Metric],
) -> Vec<String> {
    let root = first_table(def);
    let mut out: Vec<String> = Vec::new();
    for met in mets {
        for (_, home) in super::multi_path::base_components(def, met, &root) {
            if !out.contains(&home) {
                out.push(home);
            }
        }
    }
    out
}

/// The role-playing target on the join path from one of `from`, the tables a
/// query reads rows from ([`query_starts`]), to `table` (inclusive of `table`
/// itself), if any: the lowercased alias of a table that `table` *is*, or is
/// reached *through*. Such a table has an ambiguous join path: which of the
/// role-playing target's several relationship instances does it hang off? Used
/// to reject dimensions on descendants of a role-playing table (EXP-4) and
/// facts on/through one (EXP-5), one hop past the direct `AmbiguousPath` case.
///
/// A table is on the path when it reaches `table` by a chain of relationships
/// and a start reaches it: by a chain, when the start has a chain to `table`
/// (that chain is the path), or in either direction without passing through
/// `table` otherwise. A start itself is not on it (its rows are read once,
/// whatever references it). A role-played table beside the path, which
/// also references `table`, is not on it: with any `TABLES` order accepted,
/// walking up to the first-declared table instead made the answer depend on
/// that order. An empty `from` counts every table as a start.
///
/// A definition whose relationship graph cannot be rebuilt (missing FK metadata
/// or otherwise malformed), or one that contains a cycle, is reported as
/// [`ExpandError::UncheckableDefinition`] — the same fail-loud stance
/// `fan_trap::build_relationship_graph` takes — rather than silently treated as
/// "no role-playing on path", which would re-open the declaration-order-
/// dependent mis-binding this check exists to close.
pub(super) fn role_playing_on_path(
    view_name: &str,
    def: &SemanticViewDefinition,
    from: &[String],
    table: &str,
) -> Result<Option<String>, ExpandError> {
    let table_lower = table.to_ascii_lowercase();
    if is_role_playing_target(def, &table_lower) {
        return Ok(Some(table_lower));
    }
    if def.has_incomplete_relationships() {
        return Err(ExpandError::UncheckableDefinition {
            view_name: view_name.to_string(),
            reason: "one or more relationships are missing foreign-key column metadata \
                     (a legacy pre-Phase-24 definition format)"
                .to_string(),
        });
    }
    let graph = crate::graph::RelationshipGraph::from_definition(def).map_err(|reason| {
        ExpandError::UncheckableDefinition {
            view_name: view_name.to_string(),
            reason,
        }
    })?;
    // A cycle in the relationship graph is a definition-validity problem; fail
    // loudly rather than skip the ambiguity check.
    graph
        .toposort()
        .map_err(|reason| ExpandError::UncheckableDefinition {
            view_name: view_name.to_string(),
            reason,
        })?;
    // The tables on some start's path to `table`, the starts themselves left
    // out. A start with a chain of relationships to `table` follows a chain
    // (one chain wins over any other path, as in Snowflake), so only what it
    // reaches forward counts. A start with none reaches `table` some other
    // way, stepping back along a relationship (`u <- t -> p`), so anything it
    // reaches in either direction without passing through `table` counts.
    let reached: Option<HashSet<String>> = (!from.is_empty()).then(|| {
        let walk = |start: &str, undirected: bool| -> HashSet<String> {
            let mut seen: HashSet<String> = HashSet::from([start.to_string()]);
            if undirected {
                seen.insert(table_lower.clone());
            }
            let mut stack = vec![start.to_string()];
            let mut out: HashSet<String> = HashSet::new();
            while let Some(current) = stack.pop() {
                let forward = graph.edges.get(&current).into_iter().flatten();
                let backward = graph
                    .reverse
                    .get(&current)
                    .into_iter()
                    .flatten()
                    .filter(|_| undirected);
                for next in forward.chain(backward) {
                    let next = next.to_ascii_lowercase();
                    if seen.insert(next.clone()) {
                        out.insert(next.clone());
                        stack.push(next);
                    }
                }
            }
            out
        };
        let mut reached: HashSet<String> = HashSet::new();
        for start in from {
            let start = start.to_ascii_lowercase();
            if start == table_lower {
                // `table`'s own rows are read: nothing is on the way to them.
                continue;
            }
            let chain = walk(&start, false);
            if chain.contains(&table_lower) {
                reached.extend(chain);
            } else {
                reached.extend(walk(&start, true));
            }
        }
        reached
    });
    // Walk every table that references `table`, transitively, nearest first.
    // A table referenced by two different tables (a diamond, two children of
    // one parent) has several, so all of them are walked, each level sorted:
    // the first-declared one alone would make the answer depend on declaration
    // order.
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(table_lower.clone());
    let mut frontier = vec![table_lower];
    while !frontier.is_empty() {
        let mut next: Vec<String> = Vec::new();
        for current in &frontier {
            for parent in graph.reverse.get(current).into_iter().flatten() {
                let parent = parent.to_ascii_lowercase();
                if visited.insert(parent.clone()) {
                    next.push(parent);
                }
            }
        }
        next.sort_unstable();
        if let Some(rp) = next.iter().find(|p| {
            is_role_playing_target(def, p) && reached.as_ref().is_none_or(|r| r.contains(*p))
        }) {
            return Ok(Some(rp.clone()));
        }
        frontier = next;
    }
    Ok(None)
}

/// Reject a fact whose source table is (or is reached only through) a
/// role-playing table (EXP-5). Facts carry no `USING` context, so the role is
/// unresolvable; erroring beats silently binding to the first-declared
/// relationship.
pub(super) fn check_fact_role_playing_path(
    view_name: &str,
    def: &SemanticViewDefinition,
    from: &[String],
    fact: &crate::model::Fact,
) -> Result<(), ExpandError> {
    let Some(ref fact_table) = fact.source_table else {
        return Ok(());
    };
    if let Some(rp) = role_playing_on_path(view_name, def, from, fact_table)? {
        let available_relationships = relationships_to_table(def, &rp);
        return Err(ExpandError::AmbiguousFactPath {
            view_name: view_name.to_string(),
            fact_name: fact.name.clone(),
            fact_table: fact_table.to_ascii_lowercase(),
            role_playing_table: rp,
            available_relationships,
        });
    }
    Ok(())
}

/// Reject a `where_clause` member whose source table is (or is reached only
/// through) a role-playing table (EXP-10, code-review 2026-08-03).
///
/// The sibling of [`check_fact_role_playing_path`], and rejected for the same
/// reason: only a queried *dimension's* expression is rewritten to a scoped
/// alias ([`find_using_context`]), so a predicate member cannot name its role.
/// Left unchecked, the member's table rides the join resolver's
/// `fact_source_tables` path — which, unlike the dimension path, never consults
/// `role_playing_bare_aliases` — and binds to `tree_parent`, the first-declared
/// edge.
///
/// A co-queried metric's `USING` does **not** rescue it: the metric's own joins
/// are scoped to the named role while the bare instance the predicate filters on
/// stays on the first-declared one, so the query groups by one instance and
/// filters by another. Inferring the role from a co-queried metric would be a
/// guess (several metrics may name several roles, and a predicate is not bound
/// to any of them), and guessing is what produces a wrong number instead of an
/// error — the reasoning behind EXP-4/EXP-5.
///
/// # What counts as "the member's table"
///
/// EXP-31 (code-review 2026-08-08). A member forces a join not only of the
/// table it is *declared* on but of every table its expression reaches through
/// a fact reference ([`WhereMember::fact_tables`], the set #207 added to
/// `ResolvedWhere::source_tables` so those joins are emitted at all). Reading
/// only the declared table meant a member on an unambiguous base table whose
/// fact chain lands on a role-playing one sailed through and bound to
/// `tree_parent`, the first-declared relationship — the exact silent,
/// declaration-order-dependent binding this check exists to prevent, one hop
/// further along. Same blind spot #207 opened for fan traps (EXP-27), at a
/// different fence, so both walk the pair the same way.
///
/// [`WhereMember::fact_tables`]: super::where_clause::WhereMember::fact_tables
pub(super) fn check_where_clause_role_playing_path(
    view_name: &str,
    def: &SemanticViewDefinition,
    where_members: &[super::where_clause::WhereMember],
    resolved_mets: &[&crate::model::Metric],
) -> Result<(), ExpandError> {
    let from = query_starts(def, resolved_mets);
    for member in where_members {
        // A member declared without a table is base-table grain and picks no
        // role of its own — but its fact chain still can, so the chain is
        // walked either way.
        let joined_tables = member.table.iter().chain(member.fact_tables.iter());
        for member_table in joined_tables {
            let rp = match role_playing_on_path(view_name, def, &from, member_table)? {
                Some(rp) => Some(rp),
                None => role_crossed_backwards(def, &from, member_table),
            };
            if let Some(rp) = rp {
                let available_relationships = relationships_to_table(def, &rp);
                return Err(ExpandError::AmbiguousWhereClausePath {
                    view_name: view_name.to_string(),
                    member_name: member.name.clone(),
                    member_table: member_table.to_ascii_lowercase(),
                    role_playing_table: rp,
                    available_relationships,
                });
            }
        }
    }
    Ok(())
}

/// The base table's alias when every table the query touches is the base
/// table itself: each dimension's, each base metric's, each fact they reach,
/// each dimension a metric names (`NON ADDITIVE BY`, window partitioning and
/// ordering) and each `where_clause` member's. `None` otherwise.
///
/// The base table is exempt from the metric own-table check only while the
/// query touches no other table. It is the anchor of its own grain, never
/// joined through a relationship, so a query that stays on it has no role to
/// pick (v0.14.0 release review F1). Any other table, though, is reached from
/// it across the role-playing relationships, through the first-declared one,
/// so the exemption must not reach those queries (metamorphic (e) with a
/// role-played root: a filter on the referencing table made a base-table
/// metric's answer depend on declaration order).
pub(super) fn base_table_if_query_stays_on_it(
    def: &SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    bases: &[&crate::model::Metric],
    where_tables: &[String],
) -> Option<String> {
    let root = def.tables.first()?.alias.to_ascii_lowercase();
    let on_root = |t: Option<&str>| t.is_none_or(|t| t.eq_ignore_ascii_case(&root));
    let facts_on_root = |expr: &str| {
        collect_referenced_facts(expr, &def.facts)
            .iter()
            .all(|(_, t)| on_root(Some(t)))
    };
    let dim_on_root =
        |d: &crate::model::Dimension| on_root(d.source_table.as_deref()) && facts_on_root(&d.expr);
    // A dimension a metric references by name without it being queried (a
    // `NON ADDITIVE BY` snapshot's ordering, a window's partitioning) is joined
    // all the same. It is resolved the way CREATE resolves it, so a dotted
    // `a.snap_date` counts like `snap_date`. A name that does not resolve is
    // not known to be on the root.
    let named_dim_on_root =
        |name: &str| super::resolution::find_dimension(def, name).is_some_and(dim_on_root);
    let metric_on_root = |m: &crate::model::Metric| {
        let window_dims = m.window_spec.iter().flat_map(|ws| {
            ws.partition_dims
                .iter()
                .chain(&ws.excluding_dims)
                .map(String::as_str)
                .chain(ws.order_by.iter().map(|o| o.expr.as_str()))
        });
        on_root(m.source_table.as_deref())
            && facts_on_root(&m.expr)
            && m.non_additive_by
                .iter()
                .map(|na| na.dimension.as_str())
                .chain(window_dims)
                .all(named_dim_on_root)
    };
    let stays = resolved_dims.iter().all(|d| dim_on_root(d))
        && bases.iter().all(|m| metric_on_root(m))
        && where_tables.iter().all(|t| on_root(Some(t.as_str())));
    stays.then_some(root)
}

/// The role-played table a metric on `own_table` would be joined through to
/// reach `members`, the tables of the query's other members (its dimensions'
/// and `where_clause` members' tables, and the dimensions a snapshot or window
/// names, [`named_dimension_tables`]), `own_table` among them or not:
/// `own_table` itself when it is role-played and the query has any member (a
/// dimension on `own_table` is scoped to the role a co-queried metric names,
/// which this metric cannot name), or one on the way from another table to it
/// (`r <- a <= f`, a metric on `r` by a dimension on `f`; `g -> a <= f`, a
/// metric on `f` by a dimension on `g`). `None` for a metric read with no
/// member, which is computed over its own table's rows and needs no role.
///
/// The own-table check ([`check_member_fact_role_playing_path`]) refuses a
/// query on `Some`; the per-grain planner, which anchors each metric at its
/// own table, declines it the same way and no other.
pub(super) fn role_on_the_way_to_own_table(
    view_name: &str,
    def: &SemanticViewDefinition,
    own_table: &str,
    members: &[String],
) -> Result<Option<String>, ExpandError> {
    if members.is_empty() {
        return Ok(None);
    }
    if is_role_playing_target(def, own_table) {
        return Ok(Some(own_table.to_ascii_lowercase()));
    }
    let others: Vec<String> = members
        .iter()
        .filter(|t| !t.eq_ignore_ascii_case(own_table))
        .cloned()
        .collect();
    for other in &others {
        if let Some(rp) =
            role_playing_on_path(view_name, def, std::slice::from_ref(other), own_table)?
        {
            return Ok(Some(rp));
        }
    }
    Ok(role_crossed_backwards(def, &others, own_table))
}

/// The tables of a query's members as a metric on its own table sees them:
/// the queried dimensions', the `where_clause` members', and the dimensions
/// `met`'s snapshot or window names, lowercased, sorted and deduplicated.
fn member_tables(
    def: &SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    where_tables: &[String],
    met: &crate::model::Metric,
) -> Vec<String> {
    let mut out: Vec<String> = resolved_dims
        .iter()
        .filter_map(|d| d.source_table.as_ref().map(|t| t.to_ascii_lowercase()))
        .chain(where_tables.iter().map(|t| t.to_ascii_lowercase()))
        .chain(named_dimension_tables(def, met))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// The tables of the dimensions `met` names without their being queried: its
/// `NON ADDITIVE BY` dimensions and its window's partitioning and ordering.
/// A snapshot or a window joins them to the metric's rows, so they are tables
/// the metric reads beside its own. A name that does not resolve to a
/// dimension on a table adds nothing.
pub(super) fn named_dimension_tables(
    def: &SemanticViewDefinition,
    met: &crate::model::Metric,
) -> Vec<String> {
    let window = met.window_spec.iter().flat_map(|ws| {
        ws.partition_dims
            .iter()
            .chain(&ws.excluding_dims)
            .map(String::as_str)
            .chain(ws.order_by.iter().map(|o| o.expr.as_str()))
    });
    met.non_additive_by
        .iter()
        .map(|na| na.dimension.as_str())
        .chain(window)
        .filter_map(|name| super::resolution::find_dimension(def, name))
        .filter_map(|d| d.source_table.as_ref().map(|t| t.to_ascii_lowercase()))
        .collect()
}

/// Reject a dimension or metric whose expression reaches a FACT on a table that
/// is (or is reached only through) a role-playing table, unless the member can
/// name the role (TECH-DEBT #69 — the fence that change walks past).
///
/// #69 made a `USING` metric over a fact on the role-played table work: the
/// inlined fact is requalified to the scoped alias of the one relationship the
/// metric names (`facts::rewrite_using_scoped_refs`). That is the ONLY shape in
/// which a member-reached fact on a role-playing path has a role. Everything
/// else joined the fact's table through `tree_parent` — the first-declared
/// relationship — and returned that role's number silently:
///
/// - a metric with no `USING` (or several naming the same table);
/// - a dimension, which never carries `USING` (#54 made its expression
///   fact-inlined, so it reaches facts on other tables);
/// - a fact on a DESCENDANT of the role-playing table, which the scoped-alias
///   rewrite does not rename, so the descendant hangs off the bare instance.
///
/// A derived metric is checked through the base metrics it inlines, where the
/// fact references (and the `USING`) live, and a window metric through its
/// inner aggregate. A dimension's reference to a fact on its OWN table is left
/// to the checks that already govern that table (`find_using_context` for a
/// dimension on the role-playing table). A metric's own table gets no such
/// pass: a metric declared ON (or below) the role-playing table can never name
/// a role, so it is rejected with [`ExpandError::AmbiguousMetricPath`]
/// whatever it aggregates (PR #247 review).
pub(super) fn check_member_fact_role_playing_path(
    view_name: &str,
    def: &SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    resolved_mets: &[&crate::model::Metric],
    where_tables: &[String],
) -> Result<(), ExpandError> {
    // Not `def.facts.is_empty()`: a metric on a role-played table needs no fact
    // to bind to an arbitrary role (the own-table check below).
    if def.joins.is_empty() {
        return Ok(());
    }
    let from = query_starts(def, resolved_mets);
    let ambiguous = |member_name: &str, fact_name: String, fact_table: String, rp: String| {
        let available_relationships = relationships_to_table(def, &rp);
        ExpandError::AmbiguousMemberFactPath {
            detail: Box::new(AmbiguousMemberFactPathError {
                view_name: view_name.to_string(),
                member_name: member_name.to_string(),
                fact_name,
                fact_table,
                role_playing_table: rp,
                available_relationships,
            }),
        }
    };

    for dim in resolved_dims {
        let own = dim.source_table.as_ref().map(|t| t.to_ascii_lowercase());
        for (fact_name, fact_table) in collect_referenced_facts(&dim.expr, &def.facts) {
            if own.as_deref() == Some(fact_table.as_str()) {
                continue;
            }
            if let Some(rp) = role_playing_on_path(view_name, def, &from, &fact_table)? {
                return Err(ambiguous(&dim.name, fact_name, fact_table, rp));
            }
        }
    }

    // Qualified metrics only, each once: a derived metric contributes the
    // qualified metrics it inlines, and a window metric — itself qualified —
    // contributes its inner aggregate too (PR #247 review: stopping at the first
    // qualified metric never looked inside a window, so an unscoped inner
    // metric over a role-played fact was never checked).
    // `collect_transitive_metric_names` follows both edges and includes the
    // metric itself.
    let mut bases: Vec<&crate::model::Metric> = Vec::new();
    for met in resolved_mets {
        if met.source_table.is_some() && !bases.iter().any(|b| std::ptr::eq(*b, *met)) {
            bases.push(met);
        }
        for name in collect_transitive_metric_names(met, &def.metrics) {
            if let Some(base) = def.metrics.iter().find(|m| {
                m.source_table.is_some() && crate::ident::normalize_ident_part(&m.name) == name
            }) {
                if !bases.iter().any(|b| std::ptr::eq(*b, base)) {
                    bases.push(base);
                }
            }
        }
    }

    let exempt_base = base_table_if_query_stays_on_it(def, resolved_dims, &bases, where_tables);
    for base in bases {
        let own = base.source_table.as_ref().map(|t| t.to_ascii_lowercase());
        // A metric declared ON (or below) the role-playing table is joined
        // through `tree_parent` — the first-declared relationship — whatever it
        // aggregates, and it cannot name a role: a `USING` relationship must
        // originate from the metric's own table (`validate_using_relationships`),
        // so no relationship TO that table is ever nameable from it (PR #247
        // review). Checked before its fact references, which on its own table
        // the loop below deliberately skips. The base table is exempt when the
        // query stays on it (`exempt_base`, above).
        if let Some(own_table) = own
            .as_deref()
            .filter(|t| Some(*t) != exempt_base.as_deref())
        {
            // The tables the query's members are on, each walked to the
            // metric's own table: a role-played table on that way (`r <- a <=
            // f`, a metric on `r` by a dimension on `f`) is one the metric's
            // rows would be joined through. A metric read with no member is
            // computed over its own table's rows, and needs no role.
            let members = member_tables(def, resolved_dims, where_tables, base);
            if let Some(rp) = role_on_the_way_to_own_table(view_name, def, own_table, &members)? {
                let available_relationships = relationships_to_table(def, &rp);
                return Err(ExpandError::AmbiguousMetricPath {
                    detail: Box::new(AmbiguousMetricPathError {
                        view_name: view_name.to_string(),
                        metric_name: base.name.clone(),
                        metric_table: own_table.to_string(),
                        role_playing_table: rp,
                        available_relationships,
                    }),
                });
            }
        }
        for (fact_name, fact_table) in collect_referenced_facts(&base.expr, &def.facts) {
            if own.as_deref() == Some(fact_table.as_str()) {
                continue;
            }
            let Some(rp) = role_playing_on_path(view_name, def, &from, &fact_table)? else {
                continue;
            };
            // The one legal shape: the fact sits ON the role-playing table and
            // the metric's USING names exactly one relationship to it — the
            // same name match the join resolver uses to emit the scoped join.
            if fact_table == rp {
                // Distinct by the EDGE each spelling names
                // (`graph::RelNameMatch`), so `dep`, `DEP` and `"dep"` are one
                // role of a view that declares one of them, and two roles of a
                // view stored before #249 that declares `dep` AND `"dep"`.
                let mut roles: Vec<&crate::model::Join> = Vec::new();
                for rel in &base.using_relationships {
                    let m = crate::graph::RelNameMatch::new(&def.joins, rel);
                    let edge = def
                        .joins
                        .iter()
                        .find(|j| m.matches_join(j) && j.table.to_ascii_lowercase() == rp);
                    if let Some(edge) = edge {
                        if !roles.iter().any(|r| std::ptr::eq(*r, edge)) {
                            roles.push(edge);
                        }
                    }
                }
                if roles.len() == 1 {
                    continue;
                }
            }
            return Err(ambiguous(&base.name, fact_name, fact_table, rp));
        }
    }
    Ok(())
}

/// Determine the scoped alias for a dimension from a role-playing table.
///
/// Checks whether the dimension's `source_table` is reached by multiple relationships.
/// If so, looks at co-queried metrics' `using_relationships` to determine which
/// relationship (and thus which scoped alias) to use for the dimension.
///
/// A dimension on a table reached only THROUGH a role-played table, by a
/// single chain of relationships (`descendant_chain`), takes the role the same
/// way and is scoped as `{table}__{rel}` (Phase 6.3b).
///
/// Returns:
/// - `Ok(None)` if the dimension's table is not a role-playing table (single or no relationship)
///   and is not reached through one
/// - `Ok(Some(scoped_alias))` if exactly one USING path disambiguates: `a__dep` for the
///   role-played table, `r__dep` for a table past it
/// - `Err(ExpandError::AmbiguousPath)` if ambiguous with no single USING context, or
///   `Err(ExpandError::AmbiguousDescendantPath)` for a table past it
pub(super) fn find_using_context(
    view_name: &str,
    def: &SemanticViewDefinition,
    dim: &crate::model::Dimension,
    resolved_mets: &[&crate::model::Metric],
) -> Result<Option<String>, ExpandError> {
    let Some(ref dim_table) = dim.source_table else {
        return Ok(None); // No source table -> base table, no scoping needed
    };
    let dim_table_lower = dim_table.to_ascii_lowercase();

    // Find all relationships pointing to this table
    let rels = relationships_to_table(def, &dim_table_lower);
    if rels.len() <= 1 || !is_role_playing_target(def, &dim_table_lower) {
        // The dimension's own table is not role-played: one inbound
        // relationship, or several from different tables (a convergent
        // parent), each the only way there from its table. Its dimension stays
        // on the bare alias, which `resolve_joins_pkfk` joins; a `USING` naming
        // one of those relationships scopes only the facts its metric reads
        // there (TECH-DEBT #81). A query reaching it along two paths was
        // refused by `multi_path` unless a `USING` picks one.
        //
        // EXP-4: but if it is reachable only THROUGH a role-playing table, its join
        // path is ambiguous: which instance of the role-played table does it
        // hang off? Phase 6.3b (Snowflake's transitive hop): the metrics' `USING`
        // answers that as it does for the role-played table itself, and the
        // chain onward is followed under scoped aliases (`r__dep`) joined off
        // that instance (`descendant_chain`). Without a single role it is
        // refused rather than silently bound to the first-declared relationship.
        let from = query_starts(def, resolved_mets);
        if let Some(rp) = role_crossed_backwards(def, &from, &dim_table_lower) {
            return Err(ExpandError::AmbiguousDescendantPath {
                view_name: view_name.to_string(),
                dimension_name: dim.name.clone(),
                dimension_table: dim_table_lower,
                available_relationships: relationships_to_table(def, &rp),
                role_playing_table: rp,
            });
        }
        if let Some(rp) = role_playing_on_path(view_name, def, &from, &dim_table_lower)? {
            if descendant_chain(def, &rp, &dim_table_lower).is_some()
                && resolved_mets
                    .iter()
                    .all(|met| names_a_role_for(def, met, &rp) && !met_is_on(met, &rp))
            {
                if let [(_, rel)] = using_edges_to(def, &rp, resolved_mets).as_slice() {
                    return Ok(Some(scoped_join_alias(&dim_table_lower, rel)));
                }
            }
            let available_relationships = relationships_to_table(def, &rp);
            return Err(ExpandError::AmbiguousDescendantPath {
                view_name: view_name.to_string(),
                dimension_name: dim.name.clone(),
                dimension_table: dim_table_lower,
                role_playing_table: rp,
                available_relationships,
            });
        }
        return Ok(None); // Single or no relationship -> unambiguous, use bare alias
    }

    // Multiple relationships -> role-playing table. Look for USING context.
    let using_rels_for_table = using_edges_to(def, &dim_table_lower, resolved_mets);

    // Phase 6.3 (Snowflake parity, decided by Anentropic 2026-10-28): EVERY
    // co-queried metric must name its own path to a role-played table. A metric
    // with no `USING` used to borrow the role a co-queried metric named, so its
    // numbers changed with what else was in the query; Snowflake refuses it
    // ("Multi-path relationship ... is not supported").
    let every_metric_names_a_role = resolved_mets
        .iter()
        .all(|met| names_a_role_for(def, met, &dim_table_lower));
    if !every_metric_names_a_role && is_role_playing_target(def, &dim_table_lower) {
        return Err(ExpandError::AmbiguousPath {
            view_name: view_name.to_string(),
            dimension_name: dim.name.clone(),
            dimension_table: dim_table_lower,
            available_relationships: rels,
        });
    }

    if using_rels_for_table.len() == 1 {
        // Exactly one USING path disambiguates -> return scoped alias
        let scoped = scoped_join_alias(&dim_table_lower, &using_rels_for_table[0].1);
        Ok(Some(scoped))
    } else {
        // No (or conflicting) USING context for a role-playing target ->
        // ambiguous.
        Err(ExpandError::AmbiguousPath {
            view_name: view_name.to_string(),
            dimension_name: dim.name.clone(),
            dimension_table: dim_table_lower,
            available_relationships: rels,
        })
    }
}

/// For a dimension on a table several *different* tables reference (a
/// convergent parent, not role-played), the scoped alias of the one
/// relationship into it that `mets`' `USING` names, or `None`.
/// [`find_using_context`] leaves such a dimension on the bare alias; a
/// semi-additive snapshot ranks over its metric's own rows instead, so it
/// joins the table under this alias off the metric's table.
pub(super) fn convergent_using_alias(
    def: &SemanticViewDefinition,
    dim: &crate::model::Dimension,
    mets: &[&crate::model::Metric],
) -> Option<String> {
    let table = dim.source_table.as_ref()?.to_ascii_lowercase();
    if relationships_to_table(def, &table).len() <= 1 || is_role_playing_target(def, &table) {
        return None;
    }
    match using_edges_to(def, &table, mets).as_slice() {
        [(_, rel)] => Some(scoped_join_alias(&table, rel)),
        _ => None,
    }
}

/// The relationships to `table` that the metrics' `USING` names, a window
/// metric through its inner metric and a derived one through its components.
///
/// Each entry is (edge, spelling as written): deduplicated by the EDGE the
/// spelling names (`graph::RelNameMatch`), and the alias built from the written
/// spelling, which `scoped_join_alias` normalizes. By edge, so `dep` and
/// `"dep"` are one role of a view declaring one of them, and two roles of a
/// view stored before #249 that declares both (which was ambiguous then too).
fn using_edges_to<'a>(
    def: &'a SemanticViewDefinition,
    table: &str,
    resolved_mets: &[&crate::model::Metric],
) -> Vec<(&'a crate::model::Join, String)> {
    let mut using_rels_for_table: Vec<(&crate::model::Join, String)> = Vec::new();
    let mut note_using = |using_rel: &str| {
        let m = crate::graph::RelNameMatch::new(&def.joins, using_rel);
        let edge = def
            .joins
            .iter()
            .find(|j| m.matches_join(j) && j.table.eq_ignore_ascii_case(table));
        if let Some(edge) = edge {
            if !using_rels_for_table
                .iter()
                .any(|(e, _)| std::ptr::eq(*e, edge))
            {
                using_rels_for_table.push((edge, using_rel.to_string()));
            }
        }
    };
    for met in resolved_mets {
        for using_rel in &met.using_relationships {
            note_using(using_rel);
        }
        // A window metric reads its inner metric through that metric's path.
        if let Some(inner) = window_inner(def, met) {
            for using_rel in &inner.using_relationships {
                note_using(using_rel);
            }
            if inner.source_table.is_none() {
                for using_rel in collect_derived_metric_using(inner, &def.metrics) {
                    note_using(&using_rel);
                }
            }
        }
        // Also check derived metrics: walk their transitive dependencies
        if met.source_table.is_none() {
            for using_rel in collect_derived_metric_using(met, &def.metrics) {
                note_using(&using_rel);
            }
        }
    }
    using_rels_for_table
}

/// Whether `met` is declared on `table` itself.
fn met_is_on(met: &crate::model::Metric, table: &str) -> bool {
    met.source_table
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case(table))
}

/// The relationships from the role-played table `rp` down to `table`, which is
/// reachable only through it, in join order (`a -> r`, then `r -> country`):
/// the one chain of relationships from `rp` to `table`, the path a role's
/// instance continues along. Tables on the way may be referenced by other
/// tables too (since 0.19.0 CREATE accepts that); only the chain from `rp`
/// matters. `None` when there is no such chain, more than one, or a step on it
/// is itself two relationships (another role to name).
pub(super) fn descendant_chain<'a>(
    def: &'a SemanticViewDefinition,
    rp: &str,
    table: &str,
) -> Option<Vec<&'a crate::model::Join>> {
    let rp = rp.to_ascii_lowercase();
    let table = table.to_ascii_lowercase();
    let mut found: Vec<Vec<&crate::model::Join>> = Vec::new();
    let mut stack: Vec<(String, Vec<&crate::model::Join>)> = vec![(rp.clone(), Vec::new())];
    while let Some((current, chain)) = stack.pop() {
        if current == table {
            found.push(chain);
            if found.len() > 1 {
                return None;
            }
            continue;
        }
        if chain.len() > def.joins.len() {
            return None; // A cycle; validation refuses these at CREATE.
        }
        for edge in def
            .joins
            .iter()
            .filter(|j| !j.fk_columns.is_empty() && j.from_alias.eq_ignore_ascii_case(&current))
        {
            let mut next = chain.clone();
            next.push(edge);
            stack.push((edge.table.to_ascii_lowercase(), next));
        }
    }
    let chain = found.pop()?;
    // Two relationships between one pair on the way are two chains above;
    // `found` holding one means every step is a single relationship.
    Some(chain)
}

/// The metric a window metric is computed over, when `met` is one (and that
/// metric is not itself a window, so a malformed stored row cannot recurse).
fn window_inner<'a>(
    def: &'a SemanticViewDefinition,
    met: &crate::model::Metric,
) -> Option<&'a crate::model::Metric> {
    let inner = met.window_spec.as_ref()?.inner_metric.as_str();
    def.metrics
        .iter()
        .find(|m| crate::ident::ident_matches(&m.name, inner))
        .filter(|m| m.window_spec.is_none())
}

/// Whether `met` names its own path to the role-played table `table`: a base
/// metric through its `USING`, a window metric through its inner metric, a
/// derived one through every base metric it is built from. A metric ON the
/// table needs no path to it.
fn names_a_role_for(def: &SemanticViewDefinition, met: &crate::model::Metric, table: &str) -> bool {
    if let Some(inner) = window_inner(def, met) {
        return names_a_role_for(def, inner, table);
    }
    let names_one = |m: &crate::model::Metric| {
        m.source_table
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case(table))
            || m.using_relationships.iter().any(|rel| {
                crate::graph::find_relationship(&def.joins, rel)
                    .is_some_and(|j| j.table.eq_ignore_ascii_case(table))
            })
    };
    if met.source_table.is_some() {
        return names_one(met);
    }
    let names = collect_transitive_metric_names(met, &def.metrics);
    def.metrics
        .iter()
        .filter(|m| names.contains(&crate::ident::normalize_ident_part(&m.name)))
        .filter(|m| {
            m.source_table.is_some() || crate::graph::contains_aggregate_function(&m.expr).is_some()
        })
        .all(names_one)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Join, SemanticViewDefinition, TableRef};

    /// A two-table definition whose only relationship is missing its FK column
    /// metadata (a legacy pre-Phase-24 row): the graph cannot be safely rebuilt.
    fn incomplete_rel_def() -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![
                TableRef {
                    alias: "o".to_string(),
                    table: "o".to_string(),
                    ..Default::default()
                },
                TableRef {
                    alias: "c".to_string(),
                    table: "c".to_string(),
                    ..Default::default()
                },
            ],
            joins: vec![Join {
                from_alias: "o".to_string(),
                table: "c".to_string(),
                fk_columns: vec![], // incomplete: missing FK metadata
                ref_columns: vec![],
                name: Some("o_c".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn role_playing_on_path_fails_loudly_on_uncheckable_definition() {
        // Copilot review (#148): a definition whose relationship graph cannot be
        // safely rebuilt must surface `UncheckableDefinition`, not silently
        // report "no role-playing on path" — the latter would re-open the
        // declaration-order-dependent mis-binding this check exists to close.
        let def = incomplete_rel_def();
        let err = role_playing_on_path("v", &def, &[], "c").unwrap_err();
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "expected UncheckableDefinition, got: {err:?}"
        );
    }

    #[test]
    fn check_fact_role_playing_path_propagates_uncheckable() {
        // The fact-path check surfaces the same fail-loud error (facts have no
        // fan-trap pre-check, so this is their only guard).
        let def = incomplete_rel_def();
        let fact = crate::model::Fact {
            name: "f".to_string(),
            expr: "c.x".to_string(),
            source_table: Some("c".to_string()),
            ..Default::default()
        };
        let err = check_fact_role_playing_path("v", &def, &[], &fact).unwrap_err();
        assert!(
            matches!(err, ExpandError::UncheckableDefinition { .. }),
            "expected UncheckableDefinition, got: {err:?}"
        );
    }
}
