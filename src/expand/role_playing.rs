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

/// The relationships into `rp` that are its roles: those of a table that
/// references `rp` more than once, sorted. A relationship from a table that
/// references it once (gates to their airport) is no role a `USING` can pick,
/// so a message offering the roles leaves it out. All of them when no table
/// references `rp` twice.
fn roles_of(def: &SemanticViewDefinition, rp: &str) -> Vec<String> {
    let rp = rp.to_ascii_lowercase();
    let into: Vec<&crate::model::Join> = def
        .joins
        .iter()
        .filter(|j| !j.fk_columns.is_empty() && j.table.to_ascii_lowercase() == rp)
        .collect();
    let mut names: Vec<String> = into
        .iter()
        .filter(|j| {
            into.iter()
                .filter(|k| k.from_alias.eq_ignore_ascii_case(&j.from_alias))
                .count()
                > 1
        })
        .filter_map(|j| j.name.clone())
        .collect();
    if names.is_empty() {
        return relationships_to_table(def, &rp);
    }
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
/// [`role_above`] reports the table this finds: when every metric's `USING`
/// names one of its roles, [`role_path`] follows that role down to `g`
/// (TECH-DEBT #111, Snowflake fixture 6), and otherwise the member is refused.
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

/// Whether a query reading rows from `start` reaches the role-played table
/// `rp` only across one of its roles. A start with a chain of foreign keys to
/// `rp` crosses them when every such chain follows a relationship of a table
/// that declares two or more to `rp`; one with no chain, when every way to
/// `rp` over relationships in either direction does. The flights that play
/// the airports' roles cross them; the lounges, which reference the airports
/// once, and the regions above them do not, and Snowflake asks those no role
/// (fixture 9, L1 and P1). A chain is preferred to a fan-out: a table whose
/// one chain to `rp` is a role crosses it even when a fan-out through a third
/// table reaches `rp` too. `rp` itself, and a start that does not reach it,
/// cross nothing.
pub(super) fn crosses_roles(def: &SemanticViewDefinition, start: &str, rp: &str) -> bool {
    let start = start.to_ascii_lowercase();
    let rp = rp.to_ascii_lowercase();
    if start == rp {
        return false;
    }
    let roles: Vec<(String, String)> = role_playing_pairs(def)
        .into_iter()
        .filter(|(_, target)| *target == rp)
        .collect();
    if roles.is_empty() || super::multi_path::undirected_path(def, &start, &rp, None).is_none() {
        return false;
    }
    let is_role = |j: &crate::model::Join| {
        roles.contains(&(
            j.from_alias.to_ascii_lowercase(),
            j.table.to_ascii_lowercase(),
        ))
    };
    if chain_reaches(def, &start, &rp, &|_| false) {
        return !chain_reaches(def, &start, &rp, &is_role);
    }
    !undirected_reaches(def, &start, &rp, &is_role)
}

/// Whether `from` reaches `to` by a chain of foreign keys (each relationship
/// followed from its referencing table), never along one `skip` accepts.
fn chain_reaches(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    skip: &dyn Fn(&crate::model::Join) -> bool,
) -> bool {
    let mut seen: HashSet<String> = HashSet::from([from.to_string()]);
    let mut stack = vec![from.to_string()];
    while let Some(current) = stack.pop() {
        if current == to {
            return true;
        }
        for j in &def.joins {
            if j.from_alias.eq_ignore_ascii_case(&current) && !skip(j) {
                let next = j.table.to_ascii_lowercase();
                if seen.insert(next.clone()) {
                    stack.push(next);
                }
            }
        }
    }
    false
}

/// The metrics of `mets` that reach the role-played table `rp` only across
/// one of its roles ([`crosses_roles`]) from a table they read rows from
/// ([`query_starts`]): the ones a role must be named for.
fn metrics_crossing<'a>(
    def: &SemanticViewDefinition,
    mets: &[&'a crate::model::Metric],
    rp: &str,
) -> Vec<&'a crate::model::Metric> {
    mets.iter()
        .copied()
        .filter(|met| {
            query_starts(def, &[met])
                .iter()
                .any(|home| crosses_roles(def, home, rp))
        })
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
/// A role-playing target counts only for a start that reaches it across its
/// roles ([`crosses_roles`]): lounges, which reference the airports once, and
/// regions above them, reach the airports by a relationship of their own, so
/// no role is asked of them (Snowflake fixture 9, L1 and P1).
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
    if is_role_playing_target(def, &table_lower)
        && (from.is_empty() || from.iter().any(|s| crosses_roles(def, s, &table_lower)))
    {
        return Ok(Some(table_lower));
    }
    role_playing_past(view_name, def, from, &table_lower)
}

/// [`role_playing_on_path`] without `table` itself: a role-playing target on
/// the way from one of `from` to `table`, other than `table`.
fn role_playing_past(
    view_name: &str,
    def: &SemanticViewDefinition,
    from: &[String],
    table: &str,
) -> Result<Option<String>, ExpandError> {
    let table_lower = table.to_ascii_lowercase();
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
            is_role_playing_target(def, p)
                && reached.as_ref().is_none_or(|r| r.contains(*p))
                && (from.is_empty() || from.iter().any(|s| crosses_roles(def, s, p)))
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
/// Unless every metric names the same role: then the predicate is read on
/// that role's instance, as Snowflake reads it (fixture 6, R5: arrivals
/// filtered on a terminal are filtered on the arriving airport's), and
/// `scopes` ([`where_role_scopes`]) carries the scoped alias of each such
/// table, which the base-anchored emission rewrites the predicate to and joins.
/// Otherwise the role is not inferred: several metrics may name several roles,
/// and a predicate is not bound to any of them, and guessing is what produces
/// a wrong number instead of an error — the reasoning behind EXP-4/EXP-5.
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
    scopes: &[(String, String)],
) -> Result<(), ExpandError> {
    let from = query_starts(def, resolved_mets);
    for member in where_members {
        // A member declared without a table is base-table grain and picks no
        // role of its own — but its fact chain still can, so the chain is
        // walked either way.
        let joined_tables = member.table.iter().chain(member.fact_tables.iter());
        for member_table in joined_tables {
            if scopes
                .iter()
                .any(|(t, _)| t.eq_ignore_ascii_case(member_table))
                || reads_own_rows(def, resolved_mets, member_table)
            {
                continue;
            }
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

/// The scoped alias each `where_clause` member's table takes when every one
/// of `mets` names the same role for the role-played table it is, or is
/// reached through or back across, as a queried dimension on it would
/// ([`find_using_context`]): `(table, scoped alias)`, lowercased, each table
/// once. A table with no role, or with no single one, is left out, for
/// [`check_where_clause_role_playing_path`] to refuse.
pub(super) fn where_role_scopes(
    def: &SemanticViewDefinition,
    where_members: &[super::where_clause::WhereMember],
    mets: &[&crate::model::Metric],
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for member in where_members {
        for table in member.table.iter().chain(member.fact_tables.iter()) {
            let table = table.to_ascii_lowercase();
            if out.iter().any(|(t, _)| *t == table) {
                continue;
            }
            let dim = crate::model::Dimension {
                name: member.name.clone(),
                source_table: Some(table.clone()),
                ..Default::default()
            };
            if let Ok(Some(scoped)) = find_using_context("", def, &dim, mets) {
                out.push((table, scoped));
            }
        }
    }
    out
}

/// How a base-anchored emitter joins a predicate's tables given its role
/// scopes ([`where_role_scopes`]): the tables to join bare, and a stand-in
/// dimension for each table read on a role, which the join resolver joins
/// under the role's scoped alias as it would a dimension on it.
pub(super) fn split_where_tables(
    source_tables: &[String],
    scopes: &[(String, String)],
) -> (Vec<String>, Vec<crate::model::Dimension>) {
    let bare = source_tables
        .iter()
        .filter(|t| !scopes.iter().any(|(s, _)| s == *t))
        .cloned()
        .collect();
    let on_roles = scopes
        .iter()
        .map(|(table, _)| crate::model::Dimension {
            source_table: Some(table.clone()),
            ..Default::default()
        })
        .collect();
    (bare, on_roles)
}

/// The predicate `sql` rewritten to name each scoped table by its role's
/// alias ([`where_role_scopes`]).
pub(super) fn rewrite_where_to_scopes(sql: &str, scopes: &[(String, String)]) -> String {
    let mut out = sql.to_string();
    for (table, scoped) in scopes {
        out = super::join_resolver::rewrite_to_scoped_alias(&out, table, scoped);
    }
    out
}

/// A stand-in dimension on each of `where_tables` that a grain CTE reads on a
/// role: one some metric of `mets`, asked alone, reads on its role, and no
/// metric is refused for, or the role-played table itself. The
/// per-grain planner resolves each group's roles over these beside the queried
/// dimensions, so a grain CTE joins such a table under the role's scoped alias
/// and its predicate is rewritten to name it.
pub(super) fn where_role_dims(
    def: &SemanticViewDefinition,
    where_tables: &[String],
    mets: &[&crate::model::Metric],
) -> Vec<crate::model::Dimension> {
    let mut out: Vec<crate::model::Dimension> = Vec::new();
    for table in where_tables {
        let table = table.to_ascii_lowercase();
        if out
            .iter()
            .any(|d| d.source_table.as_deref() == Some(table.as_str()))
        {
            continue;
        }
        let dim = crate::model::Dimension {
            source_table: Some(table.clone()),
            ..Default::default()
        };
        // Read on one role, or (unscoped) on the rows of the role-played
        // table every metric is on. Each metric is asked alone: one filter
        // beside metrics naming two roles is read along each metric's own,
        // as Snowflake reads it (fixture 9, W3), in the group's grain CTE.
        let mut read = !mets.is_empty();
        let mut any_role = is_role_playing_target(def, &table);
        for met in mets {
            match find_using_context("", def, &dim, &[met]) {
                Ok(Some(_)) => any_role = true,
                Ok(None) => {}
                Err(_) => read = false,
            }
        }
        let read = read && any_role;
        if read {
            out.push(dim);
        }
    }
    out
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
    let own = own_table.to_ascii_lowercase();
    let others: Vec<String> = members
        .iter()
        .filter(|t| !t.eq_ignore_ascii_case(&own))
        .cloned()
        .collect();
    // A role-played table's own rows are read the same whichever role a
    // co-queried metric names, and so are the rows of a table reached from
    // them without crossing one of its roles (its one-to-one details). A
    // member reached only across one of them (the flights that play them) is
    // joined through one role, which this metric cannot name. TECH-DEBT #111:
    // this was refused for any member, its own table's included.
    if is_role_playing_target(def, &own) {
        let roles: Vec<(String, String)> = role_playing_pairs(def)
            .into_iter()
            .filter(|(_, rp)| *rp == own)
            .collect();
        let across_a_role = others.iter().any(|other| {
            !undirected_reaches(def, &own, &other.to_ascii_lowercase(), &|j| {
                roles.contains(&(
                    j.from_alias.to_ascii_lowercase(),
                    j.table.to_ascii_lowercase(),
                ))
            })
        });
        if across_a_role {
            return Ok(Some(own));
        }
    }
    for other in &others {
        if let Some(rp) = role_playing_past(view_name, def, std::slice::from_ref(other), &own)? {
            return Ok(Some(rp));
        }
    }
    Ok(role_crossed_backwards(def, &others, &own))
}

/// Whether `from` reaches `to` over relationships in either direction, never
/// along one `skip` accepts.
fn undirected_reaches(
    def: &SemanticViewDefinition,
    from: &str,
    to: &str,
    skip: &dyn Fn(&crate::model::Join) -> bool,
) -> bool {
    super::multi_path::undirected_path_over(def, from, to, None, &|j| !skip(j)).is_some()
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
/// single path ([`role_path`]), takes the role the same
/// way and is scoped as `{table}__{rel}` (Phase 6.3b).
///
/// Only the metrics that reach the role-played table across its roles
/// ([`crosses_roles`]) give or need a role: with none of them the dimension
/// needs no scope, and a mix of metrics that cross and metrics that do not is
/// refused here as ambiguous: one call reads the dimension one way, and the
/// per-grain planner asks each grain's metrics on their own.
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
        // that instance (`role_path`). Without a single role it is
        // refused rather than silently bound to the first-declared relationship.
        let from = query_starts(def, resolved_mets);
        if let Some(rp) = role_above(view_name, def, &from, &dim_table_lower)? {
            // Only the metrics that cross its roles name one (fixture 9). A
            // query mixing them with metrics that don't is answered per grain,
            // each metric asked alone; read across the query it has no one
            // alias, and is refused.
            let crossing = metrics_crossing(def, resolved_mets, &rp);
            if role_path(def, &rp, &dim_table_lower).is_some()
                && crossing.len() == resolved_mets.len()
                && crossing
                    .iter()
                    .all(|met| names_a_role_for(def, met, &rp) && !met_is_on(met, &rp))
            {
                if let [(_, rel)] = using_edges_to(def, &rp, &crossing).as_slice() {
                    return Ok(Some(scoped_join_alias(&dim_table_lower, rel)));
                }
            }
            // A table that itself references the role-played one (a gate of
            // an airport), other than one-to-one (the airports' details, which
            // `role_path` follows), is below it: reaching it crosses a role and
            // then fans out, which no USING mends (Snowflake refuses a role
            // followed by a fan-out, fixture 9).
            let fans_out = role_path(def, &rp, &dim_table_lower).is_none()
                && crate::graph::RelationshipGraph::from_definition(def)
                    .is_ok_and(|g| graph_chain_reaches(&g, &dim_table_lower, &rp));
            let available_relationships = roles_of(def, &rp);
            return Err(ExpandError::AmbiguousDescendantPath {
                view_name: view_name.to_string(),
                dimension_name: dim.name.clone(),
                dimension_table: dim_table_lower,
                role_playing_table: rp,
                available_relationships,
                fans_out,
            });
        }
        return Ok(None); // Single or no relationship -> unambiguous, use bare alias
    }

    // Multiple relationships -> role-playing table. A query whose every
    // metric is on the table reads the dimension from the metrics' own rows,
    // which no role picks (TECH-DEBT #111: an airport count by city).
    if reads_own_rows(def, resolved_mets, &dim_table_lower) {
        return Ok(None);
    }
    // Snowflake asks a role only of a metric that reaches the table across one
    // (fixture 9): lounges, which reference the airports once, and regions,
    // above them, read a city bare. A query mixing those with metrics that
    // cross a role is answered per grain, each metric asked alone; read across
    // the query it has no one alias, and is refused below.
    let crossing = metrics_crossing(def, resolved_mets, &dim_table_lower);
    if crossing.is_empty() && !resolved_mets.is_empty() {
        return Ok(None);
    }
    let resolved_mets: &[&crate::model::Metric] = if crossing.len() == resolved_mets.len() {
        &crossing
    } else {
        return Err(ExpandError::AmbiguousPath {
            view_name: view_name.to_string(),
            dimension_name: dim.name.clone(),
            dimension_table: dim_table_lower.clone(),
            available_relationships: roles_of(def, &dim_table_lower),
        });
    };
    // Otherwise look for USING context.
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
            dimension_table: dim_table_lower.clone(),
            available_relationships: roles_of(def, &dim_table_lower),
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
            dimension_table: dim_table_lower.clone(),
            available_relationships: roles_of(def, &dim_table_lower),
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

/// Whether every one of `mets`, and there is one, reads its rows from
/// `table`: declared there, or (a derived metric) built only from metrics
/// declared there ([`query_starts`]). A member there is read from the
/// metrics' own rows, which no role picks (TECH-DEBT #86, #111).
pub(super) fn reads_own_rows(
    def: &SemanticViewDefinition,
    mets: &[&crate::model::Metric],
    table: &str,
) -> bool {
    let table = table.to_ascii_lowercase();
    let homes = query_starts(def, mets);
    !mets.is_empty() && !homes.is_empty() && homes.iter().all(|home| *home == table)
}

/// Whether `met` is declared on `table` itself.
fn met_is_on(met: &crate::model::Metric, table: &str) -> bool {
    met.source_table
        .as_deref()
        .is_some_and(|t| t.eq_ignore_ascii_case(table))
}

/// One step of the way from a role-played table to a table past it
/// ([`role_path`]): `edge` followed from its referencing table to the table it
/// references, or (`reverse`) back from the referenced table to a table that
/// references it one-to-one.
#[derive(Clone, Copy, Debug)]
pub(super) struct RoleStep<'a> {
    pub edge: &'a crate::model::Join,
    pub reverse: bool,
}

impl RoleStep<'_> {
    /// The table this step joins, lowercased.
    pub(super) fn joined(&self) -> String {
        if self.reverse {
            self.edge.from_alias.to_ascii_lowercase()
        } else {
            self.edge.table.to_ascii_lowercase()
        }
    }
}

/// The way from the role-played table `rp` to `table`, which a query reaches
/// only through it, in join order: the path a role's instance continues along,
/// each table on it joined under that role's scoped alias.
///
/// The one chain of relationships from `rp` to `table` (`a -> r`, then
/// `r -> country`), when there is a chain. Tables on the way may be referenced
/// by other tables too (since 0.19.0 CREATE accepts that); only the chain from
/// `rp` matters. `None` when there are two chains, or a step on it is itself
/// two relationships (another role to name).
///
/// With no chain, the one path from `rp` to `table` over relationships in
/// either direction, when every step on it back against a relationship is
/// along a one-to-one one, not ASOF or range (`airport_details` referencing
/// the airports one-to-one, Snowflake fixture 6): such a step joins one row
/// per row, as a chain does. `None` when there is another path, or a step
/// back would fan out (TECH-DEBT #111). The relationships between a table and
/// the one it plays roles for are not on any path: each is a role.
pub(super) fn role_path<'a>(
    def: &'a SemanticViewDefinition,
    rp: &str,
    table: &str,
) -> Option<Vec<RoleStep<'a>>> {
    // The number of chains from each table to `table`, capped at 2 and
    // memoized, so each table is walked once however many paths a dense graph
    // below `rp` has. Two relationships between one pair are two chains.
    // `None` marks a table being walked: reaching it again is a cycle, which
    // validation refuses at CREATE.
    fn count<'a>(
        node: &str,
        table: &str,
        out_of: &dyn Fn(&str) -> Vec<&'a crate::model::Join>,
        memo: &mut std::collections::HashMap<String, Option<usize>>,
    ) -> Option<usize> {
        if node == table {
            return Some(1);
        }
        match memo.get(node) {
            Some(Some(n)) => return Some(*n),
            Some(None) => return None,
            None => {}
        }
        memo.insert(node.to_string(), None);
        let mut n = 0;
        for edge in out_of(node) {
            n = (n + count(&edge.table.to_ascii_lowercase(), table, out_of, memo)?).min(2);
        }
        memo.insert(node.to_string(), Some(n));
        Some(n)
    }
    let rp = rp.to_ascii_lowercase();
    let table = table.to_ascii_lowercase();
    let out_of = |node: &str| {
        def.joins
            .iter()
            .filter(move |j| !j.fk_columns.is_empty() && j.from_alias.eq_ignore_ascii_case(node))
            .collect::<Vec<_>>()
    };
    let mut memo = std::collections::HashMap::new();
    match count(&rp, &table, &out_of, &mut memo)? {
        0 => return path_back_one_to_one(def, &rp, &table),
        1 => {}
        _ => return None,
    }
    // Exactly one chain: follow the one relationship at each step whose far
    // table still reaches `table`.
    let mut chain: Vec<RoleStep<'a>> = Vec::new();
    let mut current = rp;
    while current != table {
        let edge = out_of(&current).into_iter().find(|j| {
            let to = j.table.to_ascii_lowercase();
            to == table || memo.get(&to).copied().flatten().unwrap_or(0) > 0
        })?;
        chain.push(RoleStep {
            edge,
            reverse: false,
        });
        current = edge.table.to_ascii_lowercase();
    }
    Some(chain)
}

/// [`role_path`] with no chain: the one path from `rp` to `table` over
/// relationships in either direction, every relationship between a pair of
/// tables that it relates twice (a role) left out, when each step back is
/// along a one-to-one relationship that is not ASOF or range.
///
/// The walks are `multi_path`'s, with the roles left out, so this "one path"
/// and the multi-path refusal read the graph the same way.
fn path_back_one_to_one<'a>(
    def: &'a SemanticViewDefinition,
    rp: &str,
    table: &str,
) -> Option<Vec<RoleStep<'a>>> {
    let roles = role_playing_pairs(def);
    let pair = |j: &crate::model::Join| {
        (
            j.from_alias.to_ascii_lowercase(),
            j.table.to_ascii_lowercase(),
        )
    };
    let keep = |j: &crate::model::Join| !roles.contains(&pair(j));
    if super::multi_path::two_undirected_paths_over(def, rp, table, &keep).is_some() {
        return None;
    }
    let tables = super::multi_path::undirected_path_over(def, rp, table, None, &keep)?;
    // With the roles left out no two relationships relate the same pair of
    // tables (`CREATE` names each of a pair's relationships, and two named
    // ones are a role), so each step is one relationship.
    let path: Vec<RoleStep<'a>> = tables
        .windows(2)
        .map(|step| {
            def.joins
                .iter()
                .filter(|j| !j.fk_columns.is_empty() && keep(j))
                .find_map(|edge| match pair(edge) {
                    (from, to) if from == step[0] && to == step[1] => Some(RoleStep {
                        edge,
                        reverse: false,
                    }),
                    (from, to) if from == step[1] && to == step[0] => Some(RoleStep {
                        edge,
                        reverse: true,
                    }),
                    _ => None,
                })
        })
        .collect::<Option<_>>()?;
    path.iter()
        .all(|step| {
            !step.reverse
                || (step.edge.cardinality == crate::model::Cardinality::OneToOne
                    && step.edge.asof.is_none()
                    && step.edge.range_end.is_none())
        })
        .then_some(path)
}

/// The role-played table a member on `table` is joined off, for a query that
/// reads rows from `from` ([`query_starts`]): one `table` is reached only
/// through ([`role_playing_on_path`]), or one the way from a start crosses the
/// other way round ([`role_crossed_backwards`]). `None` when `table` is itself
/// role-played, or no role-played table is on the way to it.
pub(super) fn role_above(
    view_name: &str,
    def: &SemanticViewDefinition,
    from: &[String],
    table: &str,
) -> Result<Option<String>, ExpandError> {
    let table = table.to_ascii_lowercase();
    if let Some(rp) = role_crossed_backwards(def, from, &table) {
        return Ok(Some(rp));
    }
    Ok(role_playing_on_path(view_name, def, from, &table)?.filter(|rp| *rp != table))
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

    /// `role_path` below a dense layered graph: `rp` references three
    /// tables, each of those references all three of the next layer, and so
    /// on for 14 layers (3^14 forward paths), while `side` hangs off `rp` by
    /// one relationship and `back` references `rp` one-to-one. The one chain
    /// to `side` is found without walking every path (review of #342: the
    /// walk built and cloned each of them), and so is the one step back to
    /// `back`.
    #[test]
    fn role_path_is_bounded_below_a_dense_graph() {
        let join = |from: &str, to: &str, col: &str| Join {
            from_alias: from.to_string(),
            table: to.to_string(),
            fk_columns: vec![col.to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some(format!("{from}_{to}")),
            ..Default::default()
        };
        let mut joins = vec![join("rp", "side", "side_id"), {
            let mut j = join("back", "rp", "rp_id");
            j.cardinality = crate::model::Cardinality::OneToOne;
            j
        }];
        let layer = |l: usize| (0..3).map(move |k| format!("l{l}_{k}"));
        for to in layer(0) {
            joins.push(join("rp", &to, &format!("{to}_id")));
        }
        for l in 0..14 {
            for from in layer(l) {
                for to in layer(l + 1) {
                    joins.push(join(&from, &to, &format!("{to}_id")));
                }
            }
        }
        let def = SemanticViewDefinition {
            joins,
            ..Default::default()
        };
        let start = std::time::Instant::now();
        let chain = role_path(&def, "rp", "side").expect("one chain to side");
        assert_eq!(chain.len(), 1);
        assert!(role_path(&def, "rp", "l14_0").is_none(), "many chains");
        let back = role_path(&def, "rp", "back").expect("one step back to back");
        assert!(back.len() == 1 && back[0].reverse && back[0].joined() == "back");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(1),
            "took {:?}",
            start.elapsed()
        );
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
