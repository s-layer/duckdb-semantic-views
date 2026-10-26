use std::collections::{HashMap, HashSet, VecDeque};

use crate::graph::RelationshipGraph;
use crate::model::{Join, SemanticViewDefinition, TableRef};

use super::facts::{
    collect_derived_metric_source_tables, collect_derived_metric_using,
    collect_referenced_fact_tables, collect_transitive_metric_names,
};
use super::resolution::{
    qualify_and_quote_table_ref, quote_ident, quote_ident_if_needed, quote_stored_ident,
};

/// Build a role-playing scoped alias in the documented `{table}__{rel}` format.
///
/// The result is the alias's *logical* value — never SQL text. Both parts go
/// through [`crate::ident::normalize_ident_part`], so a stored, quoted
/// relationship name (`"de p"`, embedded `""` and all) contributes its
/// unquoted, case-folded value (`de p`) rather than its delimiters (TECH-DEBT
/// #70, MM-2). Every emission site then quotes the alias exactly once: the
/// JOIN clause via `quote_ident`, a rewritten member reference via
/// [`rewrite_to_scoped_alias`]. Concatenating the raw stored text instead put
/// quote characters *inside* the alias, which the two sites then quoted
/// differently (`"ap__""de p"""` vs `ap__"de p".city`) — unparseable SQL.
///
/// This is the single source of the format so every producer — this module's
/// USING-relationship join synthesis, `role_playing::find_using_context` and
/// the per-grain emitter — can never drift (E-10, code-review 2026-07-11).
pub(super) fn scoped_join_alias(table: &str, rel: &str) -> String {
    format!(
        "{}__{}",
        crate::ident::normalize_ident_part(table),
        crate::ident::normalize_ident_part(rel)
    )
}

/// Rewrite every reference qualified by `table` in `expr` to the role-playing
/// `scoped` alias (as built by [`scoped_join_alias`]), emitting the alias as
/// one SQL identifier.
///
/// A bare-safe alias (`ap__r_dep`) is emitted unquoted, byte-identical to the
/// pre-#70 output; anything else (`ap__de p`, non-ASCII, upper case) is
/// double-quoted, matching the `quote_ident` the JOIN clause applies to the
/// same logical alias. This is the one reference-site spelling, shared by the
/// dimension emitters and the role-played fact rewrite (TECH-DEBT #69).
pub(super) fn rewrite_to_scoped_alias(expr: &str, table: &str, scoped: &str) -> String {
    crate::expr_tokens::rewrite_qualifier(expr, table, &quote_ident_if_needed(scoped))
}

/// Synthesize an ON clause from PK/FK column declarations (Phase 26).
///
/// Zips `join.fk_columns` with the referenced table's `pk_columns` to produce
/// `from_alias.fk = to_alias.pk` pairs, joined by ` AND `.
/// Uses `join.from_alias` for the FROM side and `join.table` for the TO side.
pub(super) fn synthesize_on_clause(join: &Join, tables: &[TableRef]) -> String {
    on_clause(join, tables, &quote_stored_ident(&join.table))
}

/// Synthesize an ON clause with a potentially scoped alias on the PK (target) side.
///
/// Like `synthesize_on_clause`, but uses `to_alias` instead of `join.table` in the
/// generated SQL. This supports role-playing dimensions where the same physical table
/// appears multiple times with different scoped aliases (e.g., `a__dep_airport`).
///
/// Phase 33: Prefers `join.ref_columns` (resolved during inference) over looking up
/// `pk_columns` from the target table. Falls back to target PK for backward compat
/// (legacy joins without `ref_columns`).
pub(super) fn synthesize_on_clause_scoped(
    join: &Join,
    tables: &[TableRef],
    to_alias: &str,
) -> String {
    on_clause(join, tables, &quote_ident(to_alias))
}

/// The ON clause of `join` with `to_sql`, an already-quoted alias, on the
/// referenced side. Stored aliases and key columns keep any quotes they were
/// written with, so each is stripped and quoted once ([`quote_stored_ident`]); a
/// scoped alias is a logical value and is quoted as is.
fn on_clause(join: &Join, tables: &[TableRef], to_sql: &str) -> String {
    // Phase 33: Prefer ref_columns (resolved during inference).
    // Fall back to target PK for backward compat (legacy joins without ref_columns).
    let ref_cols: &[String] = if join.ref_columns.is_empty() {
        let to_alias_lower = join.table.to_ascii_lowercase();
        tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == to_alias_lower)
            .map_or(&[] as &[String], |t| &t.pk_columns)
    } else {
        &join.ref_columns
    };

    let pairs: Vec<String> = join
        .fk_columns
        .iter()
        .zip(ref_cols.iter())
        .map(|(fk, pk)| {
            format!(
                "{}.{} = {}.{}",
                quote_stored_ident(&join.from_alias),
                quote_stored_ident(fk),
                to_sql,
                quote_stored_ident(pk),
            )
        })
        .collect();
    pairs.join(" AND ")
}

/// A join edge chosen by the resolver, in emission order.
///
/// Carries everything the emitters need to render the JOIN clause directly:
/// no re-searching `def.joins` by alias (SG-2) and no re-parsing of scoped
/// alias strings at `__` (SG-12).
pub(super) struct ResolvedJoin<'a> {
    /// SQL alias emitted after `AS` — the bare table alias (lowercased) or a
    /// role-playing scoped alias in the documented `{bare}__{rel}` format.
    pub emit_alias: String,
    /// The bare (lowercased) table alias, used to look up the physical table
    /// name in `def.tables`.
    pub bare_alias: String,
    /// The join edge connecting `emit_alias` to an already-emitted table.
    pub join: &'a Join,
    /// Whether `emit_alias` is a role-playing scoped alias. Scoped joins put
    /// the scoped alias on the PK side of the synthesized ON clause.
    pub scoped: bool,
}

/// Render `LEFT JOIN` clauses for resolver-selected edges onto `sql`.
///
/// `prefix` carries the newline + indentation + keyword for the emission site
/// (e.g. `"\nLEFT JOIN "` in flat queries, `"\n    LEFT JOIN "` inside CTEs).
pub(super) fn push_join_clauses(
    sql: &mut String,
    joins: &[ResolvedJoin<'_>],
    def: &SemanticViewDefinition,
    prefix: &str,
) {
    for rj in joins {
        let table_ref = def
            .tables
            .iter()
            .find(|t| t.alias.to_ascii_lowercase() == rj.bare_alias);
        let physical_table = table_ref.map_or(rj.bare_alias.as_str(), |t| t.table.as_str());
        sql.push_str(prefix);
        sql.push_str(&qualify_and_quote_table_ref(physical_table, def));
        sql.push_str(" AS ");
        // A scoped alias is a logical value; a bare one is stored as written.
        sql.push_str(&if rj.scoped {
            quote_ident(&rj.emit_alias)
        } else {
            quote_stored_ident(&rj.emit_alias)
        });
        sql.push_str(" ON ");
        if rj.scoped {
            sql.push_str(&synthesize_on_clause_scoped(
                rj.join,
                &def.tables,
                &rj.emit_alias,
            ));
        } else {
            sql.push_str(&synthesize_on_clause(rj.join, &def.tables));
        }
    }
}

/// Undirected BFS from the graph root over PK/FK join edges.
///
/// Returns, for each reachable non-root alias, its tree parent (the neighbor
/// on the path toward the root) and the `Join` edge connecting the two. This
/// is the edge the emitters must use for the alias's ON clause: any other join
/// mentioning the alias would reference a table that is not yet (or never)
/// joined (SG-2). Traversing edges in both directions also covers tables on
/// the FK side of the root (SG-10).
fn build_tree_parents<'a>(
    def: &'a SemanticViewDefinition,
    root: &str,
) -> HashMap<String, (String, &'a Join)> {
    let mut tree_parent: HashMap<String, (String, &'a Join)> = HashMap::new();
    let mut visited: HashSet<String> = HashSet::new();
    visited.insert(root.to_string());
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(root.to_string());
    while let Some(current) = queue.pop_front() {
        // Visit edges in declaration order for determinism.
        for join in &def.joins {
            if join.fk_columns.is_empty() {
                continue; // Legacy join -- not part of the PK/FK graph
            }
            let from = join.from_alias.to_ascii_lowercase();
            let to = join.table.to_ascii_lowercase();
            let neighbor = if from == current {
                to
            } else if to == current {
                from
            } else {
                continue;
            };
            if visited.insert(neighbor.clone()) {
                tree_parent.insert(neighbor.clone(), (current.clone(), join));
                queue.push_back(neighbor.clone());
            }
        }
    }
    tree_parent
}

/// Append a bare-alias join to `result`, choosing the connecting edge.
///
/// Reachable aliases use their BFS tree edge. Aliases disconnected from the
/// root (degenerate definitions that predate stricter validation) fall back to
/// the first declared join mentioning the alias; aliases with no join at all
/// are skipped, matching the previous emitters' behavior.
fn push_bare_join<'a>(
    result: &mut Vec<ResolvedJoin<'a>>,
    def: &'a SemanticViewDefinition,
    tree_parent: &HashMap<String, (String, &'a Join)>,
    alias: String,
) {
    let join = if let Some((_, join)) = tree_parent.get(&alias) {
        *join
    } else {
        let Some(join) = def.joins.iter().find(|j| {
            j.table.to_ascii_lowercase() == alias || j.from_alias.to_ascii_lowercase() == alias
        }) else {
            return;
        };
        join
    };
    result.push(ResolvedJoin {
        emit_alias: alias.clone(),
        bare_alias: alias,
        join,
        scoped: false,
    });
}

/// `resolved_mets` plus, transitively, every metric a window metric among them
/// names as its inner aggregate, each once, in first-reached order.
fn with_window_inner_metrics<'m>(
    resolved_mets: &[&'m crate::model::Metric],
    all_metrics: &'m [crate::model::Metric],
) -> Vec<&'m crate::model::Metric> {
    let mut out: Vec<&crate::model::Metric> = resolved_mets.to_vec();
    let mut i = 0;
    while i < out.len() {
        if let Some(ref ws) = out[i].window_spec {
            if let Some(inner) = all_metrics
                .iter()
                .find(|m| crate::ident::ident_matches(&m.name, &ws.inner_metric))
            {
                if !out.iter().any(|m| std::ptr::eq(*m, inner)) {
                    out.push(inner);
                }
            }
        }
        i += 1;
    }
    out
}

/// Resolve which joins are needed using graph-based PK/FK resolution (Phase 26+32).
///
/// Builds a `RelationshipGraph` from the definition, collects needed table
/// aliases from resolved dimensions, metrics, and fact source tables, includes
/// every table on the path between the root and a needed alias (in both edge
/// directions — SG-10), and returns structured join edges in emission order:
/// each edge's ON clause references only the new alias and already-emitted
/// tables (SG-2).
///
/// `fact_source_tables` carries fact source aliases for the facts path
/// (`expand_facts`); other callers pass `&[]`. Fact-driven joins are appended
/// after dimension-driven joins, preserving the historical clause order.
///
/// Phase 32: When metrics have `using_relationships`, generates scoped aliases
/// (`{to_alias}__{rel_name}`) instead of bare aliases. Scoped joins are placed
/// after all bare joins, sorted by alias for deterministic output.
#[allow(clippy::too_many_lines)]
pub(super) fn resolve_joins_pkfk<'a>(
    def: &'a SemanticViewDefinition,
    resolved_dims: &[&crate::model::Dimension],
    resolved_mets: &[&crate::model::Metric],
    fact_source_tables: &[String],
) -> Vec<ResolvedJoin<'a>> {
    let Ok(graph) = RelationshipGraph::from_definition(def) else {
        return Vec::new(); // Graph was validated at define time
    };

    // A window metric's inner aggregate is computed in `__sv_agg` against these
    // joins, so the inner metric needs them as much as a queried one does: its
    // `USING` scoped joins and the tables its inlined facts reach. A window
    // metric is qualified (it carries the source table of its declaration), so
    // none of the derived-metric walks below reach it (PR #247 review — the
    // inlined fact named `a__dep_airport` while no scoped join was emitted).
    let with_inner = with_window_inner_metrics(resolved_mets, &def.metrics);
    let resolved_mets: &[&crate::model::Metric] = &with_inner;

    let root = &graph.root;

    // Phase 32: Collect scoped joins from metrics with USING relationships.
    // Each entry carries the scoped alias and the resolved Join edge, so the
    // emitters never re-derive the relationship from the alias string (SG-12).
    let mut scoped_joins: Vec<(String, &Join)> = Vec::new();
    // Also track which bare aliases are role-playing (have multiple relationships).
    let mut role_playing_bare_aliases: HashSet<String> = HashSet::new();

    let add_scoped = |scoped_joins: &mut Vec<(String, &'a Join)>,
                      role_playing: &mut HashSet<String>,
                      using_rel: &str| {
        if let Some(join) = crate::graph::find_relationship(&def.joins, using_rel) {
            let to_alias = join.table.to_ascii_lowercase();
            // `scoped_join_alias` normalizes the name, so every spelling of
            // one relationship yields the same alias.
            let scoped = scoped_join_alias(&to_alias, using_rel);
            if !scoped_joins.iter().any(|(s, _)| *s == scoped) {
                scoped_joins.push((scoped, join));
            }
            role_playing.insert(to_alias);
        }
    };

    for met in resolved_mets {
        if !met.using_relationships.is_empty() {
            for using_rel in &met.using_relationships {
                add_scoped(&mut scoped_joins, &mut role_playing_bare_aliases, using_rel);
            }
        } else if met.source_table.is_none() {
            // Derived metric: walk transitive USING relationships
            let transitive_using = collect_derived_metric_using(met, &def.metrics);
            for using_rel in transitive_using {
                add_scoped(
                    &mut scoped_joins,
                    &mut role_playing_bare_aliases,
                    &using_rel,
                );
            }
        }
    }

    // Collect needed bare aliases from source_table fields (lowercased).
    let mut needed: HashSet<String> = HashSet::new();
    for dim in resolved_dims {
        if let Some(ref st) = dim.source_table {
            let alias = st.to_ascii_lowercase();
            if alias != *root {
                // If this is a role-playing table, the dimension alias will be resolved
                // via find_using_context() in expand(). Don't add the bare alias here;
                // the scoped aliases are already tracked.
                if !role_playing_bare_aliases.contains(&alias) {
                    needed.insert(alias);
                }
            }
        }
    }
    for met in resolved_mets {
        if met.using_relationships.is_empty() {
            if let Some(ref st) = met.source_table {
                let alias = st.to_ascii_lowercase();
                if alias != *root {
                    needed.insert(alias);
                }
            } else {
                // Derived metric without direct USING: walk dependency graph for bare tables
                let transitive_tables = collect_derived_metric_source_tables(met, &def.metrics);
                for alias in transitive_tables {
                    if alias != *root && !role_playing_bare_aliases.contains(&alias) {
                        needed.insert(alias);
                    }
                }
            }
        }
        // Metrics WITH using_relationships: their source_table is the base table
        // (e.g., "f" for flights). Only add if it's not root.
        if !met.using_relationships.is_empty() {
            if let Some(ref st) = met.source_table {
                let alias = st.to_ascii_lowercase();
                if alias != *root {
                    needed.insert(alias);
                }
            }
        }
    }

    // PAR-6 (TECH-DEBT #53): a member's *expression* may reference a fact
    // declared on another logical table — Snowflake's supported way to cross
    // tables. `inline_facts` splices that fact's expression in at the reference
    // site, so its table has to be joined too or the emitted SQL names an alias
    // that is not in scope. Collected from the expression rather than from
    // `source_table`, which by construction only ever names the member's own
    // table.
    let add_referenced_fact_tables = |expr: &str, needed: &mut HashSet<String>| {
        for alias in collect_referenced_fact_tables(expr, &def.facts) {
            if alias != *root && !role_playing_bare_aliases.contains(&alias) {
                needed.insert(alias);
            }
        }
    };
    // Dimensions as well as metrics, since TECH-DEBT #54: a dimension's
    // expression is now fact-inlined on the same footing, so a fact it reaches
    // on another table has to be joined for the spliced expression to bind.
    // (#200 collected these before the inlining existed, which was the wrong
    // order -- it joined a relation to satisfy a reference that was never
    // substituted. With inlining in place the collection is what makes the
    // reference work.)
    for dim in resolved_dims {
        add_referenced_fact_tables(&dim.expr, &mut needed);
    }
    for met in resolved_mets {
        add_referenced_fact_tables(&met.expr, &mut needed);
        // A derived metric's own expression names metrics, not facts; the fact
        // references live in the base metrics it resolves to, whose expressions
        // are inlined into it before the facts pass runs.
        if met.source_table.is_none() {
            for name in collect_transitive_metric_names(met, &def.metrics) {
                if let Some(base) = def
                    .metrics
                    .iter()
                    .find(|m| crate::ident::normalize_ident_part(&m.name) == name)
                {
                    add_referenced_fact_tables(&base.expr, &mut needed);
                }
            }
        }
    }

    // Fact source tables (facts path only), in declaration order.
    let mut fact_needed: Vec<String> = Vec::new();
    for st in fact_source_tables {
        let alias = st.to_ascii_lowercase();
        if alias != *root && !fact_needed.contains(&alias) {
            fact_needed.push(alias);
        }
    }

    // If no bare aliases, fact aliases, or scoped aliases needed, return empty
    if needed.is_empty() && scoped_joins.is_empty() && fact_needed.is_empty() {
        return Vec::new();
    }

    // Map each reachable alias to its connecting edge on the path to the root.
    let tree_parent = build_tree_parents(def, root);

    // Include every table on the path between the root and each needed alias.
    let mut all_needed: HashSet<String> = HashSet::new();
    for alias in &needed {
        if tree_parent.contains_key(alias) {
            // Reachable: walk the parent chain to the root, adding intermediaries.
            let mut current = alias.clone();
            while current != *root && all_needed.insert(current.clone()) {
                let Some((parent, _)) = tree_parent.get(&current) else {
                    break;
                };
                current = parent.clone();
            }
        } else {
            // Disconnected from the root (degenerate definition): legacy
            // reverse-edge walk, preserving pre-existing emission behavior.
            all_needed.insert(alias.clone());
            let mut to_visit = vec![alias.clone()];
            while let Some(current) = to_visit.pop() {
                if let Some(parents) = graph.reverse.get(&current) {
                    for parent in parents {
                        if parent != root && all_needed.insert(parent.clone()) {
                            to_visit.push(parent.clone());
                        }
                    }
                }
            }
        }
    }

    // Base ordering: topological order over the PK/FK graph.
    let Ok(topo_order) = graph.toposort() else {
        return Vec::new(); // Should not happen -- validated at define time
    };
    let mut ordered: Vec<String> = topo_order
        .into_iter()
        .filter(|alias| all_needed.contains(alias))
        .collect();

    // Stable re-sort: each reachable alias's tree parent must be emitted first
    // (or be the root). Kahn order already satisfies this for root-outward
    // trees; FK-side chains below the root need the fix (SG-10).
    let mut emission: Vec<String> = Vec::with_capacity(ordered.len());
    while !ordered.is_empty() {
        let pos = ordered
            .iter()
            .position(|alias| match tree_parent.get(alias) {
                Some((parent, _)) => parent == root || !ordered.contains(parent),
                None => true, // Disconnected: no ordering constraint
            });
        // Reachable aliases always have their full parent chain included, so
        // some alias is always emittable; fall back to the front defensively.
        emission.push(ordered.remove(pos.unwrap_or(0)));
    }

    let mut result: Vec<ResolvedJoin<'a>> = Vec::new();
    for alias in emission {
        push_bare_join(&mut result, def, &tree_parent, alias);
    }

    // Fact-driven joins: appended after dimension-driven joins, each with its
    // path intermediaries in root-outward order (SG-10 on the facts path).
    for alias in fact_needed {
        if result.iter().any(|rj| rj.emit_alias == alias) {
            continue;
        }
        if tree_parent.contains_key(&alias) {
            let mut path: Vec<String> = Vec::new();
            let mut current = alias;
            while current != *root {
                path.push(current.clone());
                let Some((parent, _)) = tree_parent.get(&current) else {
                    break;
                };
                current = parent.clone();
            }
            for path_alias in path.into_iter().rev() {
                if result.iter().any(|rj| rj.emit_alias == path_alias) {
                    continue;
                }
                push_bare_join(&mut result, def, &tree_parent, path_alias);
            }
        } else {
            push_bare_join(&mut result, def, &tree_parent, alias);
        }
    }

    // Sort scoped joins by alias for deterministic output, then append.
    scoped_joins.sort_by(|a, b| a.0.cmp(&b.0));
    for (scoped_alias, join) in scoped_joins {
        result.push(ResolvedJoin {
            bare_alias: join.table.to_ascii_lowercase(),
            emit_alias: scoped_alias,
            join,
            scoped: true,
        });
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expand::test_helpers::{orders_view, TestFixtureExt};
    use crate::model::{Join, TableRef};

    #[test]
    fn test_synthesize_on_clause_single_column() {
        let join = Join {
            table: "customers".to_string(),
            from_alias: "orders".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "customers".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert_eq!(result, r#""orders"."customer_id" = "customers"."id""#);
    }

    #[test]
    fn test_synthesize_on_clause_composite_keys() {
        let join = Join {
            table: "target".to_string(),
            from_alias: "source".to_string(),
            fk_columns: vec!["a".to_string(), "b".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "target".to_string(),
            table: "target".to_string(),
            pk_columns: vec!["x".to_string(), "y".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert!(
            result.contains(r#"= "target"."x""#),
            "Should contain target.x"
        );
        assert!(
            result.contains("AND"),
            "Should contain AND for composite keys"
        );
        assert!(
            result.contains(r#"= "target"."y""#),
            "Should contain target.y"
        );
    }

    #[test]
    fn test_synthesize_on_clause_empty_fk_columns() {
        let join = Join {
            table: "customers".to_string(),
            from_alias: "orders".to_string(),
            fk_columns: vec![],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "customers".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause(&join, &tables);
        assert_eq!(
            result, "",
            "Empty fk_columns should produce empty ON clause"
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_uses_to_alias() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_airport_code".to_string()],
            ref_columns: vec!["code".to_string()],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, "airports__dep_airport");
        assert!(
            result.contains(r#""airports__dep_airport""#),
            "Should use scoped alias, got: {result}"
        );
        assert!(
            !result.contains(r#""airports"."#),
            "Should not use bare table alias on right side"
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_prefers_ref_columns() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_code".to_string()],
            ref_columns: vec!["code".to_string()],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, "airports__dep");
        assert!(
            result.contains(r#""code""#),
            "Should use ref_columns, got: {result}"
        );
        assert!(
            !result.contains(r#""id""#),
            "Should not use pk_columns when ref_columns are present"
        );
    }

    /// Key columns are stored as written, so a quoted one keeps its quotes;
    /// the ON clause must not quote it a second time.
    #[test]
    fn test_synthesize_on_clause_quoted_key_columns_are_quoted_once() {
        let join = Join {
            table: "p".to_string(),
            from_alias: "c".to_string(),
            fk_columns: vec!["\"Parent Ref\"".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "p".to_string(),
            table: "parent".to_string(),
            pk_columns: vec!["\"Key Col\"".to_string()],
            ..Default::default()
        }];
        assert_eq!(
            synthesize_on_clause_scoped(&join, &tables, "p"),
            r#""c"."Parent Ref" = "p"."Key Col""#
        );
    }

    #[test]
    fn test_synthesize_on_clause_scoped_falls_back_to_pk() {
        let join = Join {
            table: "airports".to_string(),
            from_alias: "flights".to_string(),
            fk_columns: vec!["dep_code".to_string()],
            ref_columns: vec![],
            ..Default::default()
        };
        let tables = vec![TableRef {
            alias: "airports".to_string(),
            table: "airports".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        }];
        let result = synthesize_on_clause_scoped(&join, &tables, "airports__dep");
        assert!(
            result.contains(r#""id""#),
            "Should fall back to pk_columns when ref_columns are empty, got: {result}"
        );
    }

    #[test]
    fn test_resolve_joins_pkfk_no_joins() {
        let def = orders_view();
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(result.is_empty(), "No joins should produce empty result");
    }

    #[test]
    fn test_resolve_joins_pkfk_single_join() {
        let def = orders_view()
            .with_table("orders", "orders", &["id"])
            .with_table("customers", "customers", &["id"])
            .with_dimension("cust_name", "name", Some("customers"))
            .with_pkfk_join(
                "orders_customers",
                "orders",
                "customers",
                &["customer_id"],
                &["id"],
            );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(
            result.iter().any(|rj| rj.emit_alias == "customers"),
            "Should include customers join, got: {:?}",
            result.iter().map(|rj| &rj.emit_alias).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_resolve_joins_pkfk_with_using_relationship() {
        // Build a role-playing scenario: flights -> airports via dep_airport and arr_airport
        let def = orders_view()
            .with_table("flights", "flights", &[])
            .clear_dimensions()
            .clear_metrics()
            .with_table("flights", "flights", &["id"])
            .with_table("airports", "airports", &["code"])
            .with_dimension("airport_name", "name", Some("airports"))
            .with_metric("flight_count", "count(*)", Some("flights"))
            .with_using_relationship("flight_count", &["dep_airport"])
            .with_pkfk_join(
                "dep_airport",
                "flights",
                "airports",
                &["dep_code"],
                &["code"],
            )
            .with_pkfk_join(
                "arr_airport",
                "flights",
                "airports",
                &["arr_code"],
                &["code"],
            );
        let resolved_dims: Vec<&_> = def.dimensions.iter().collect();
        let resolved_mets: Vec<&_> = def.metrics.iter().collect();
        let result = resolve_joins_pkfk(&def, &resolved_dims, &resolved_mets, &[]);
        assert!(
            result
                .iter()
                .any(|rj| rj.emit_alias == "airports__dep_airport" && rj.scoped),
            "Should include scoped alias airports__dep_airport, got: {:?}",
            result.iter().map(|rj| &rj.emit_alias).collect::<Vec<_>>()
        );
    }
}
