//! DDL reconstruction: renders `CREATE OR REPLACE SEMANTIC VIEW` DDL
//! from a stored `SemanticViewDefinition`.
//!
//! The DDL reconstruction follows the body parser clause ordering:
//! TABLES -> RELATIONSHIPS -> FACTS -> DIMENSIONS -> METRICS
//! with optional clauses omitted when empty.
//!
//! This module is always compiled (not feature-gated) so that unit tests
//! can run under `cargo test` without the `extension` feature.

use crate::model::{AccessModifier, NullsOrder, SemanticViewDefinition, SortOrder};

/// SQL single-quote escaping: `'` -> `''`.
///
/// Delegates to [`crate::sql_lit::SqlLit`], the single source of the quote-
/// doubling rule, so this logic lives in exactly one place and cannot drift
/// (code-review 2026-07-16 hygiene: this was the one `''`-escape copy outside
/// `SqlLit`). `render_ddl` keeps a thin named wrapper because its output is
/// `GET_DDL` *display* text rather than executable catalog SQL — a different
/// consumer, but an identical escaping rule.
fn escape_single_quote(s: &str) -> String {
    crate::sql_lit::SqlLit::escape(s).to_string()
}

/// Append ` COMMENT = '<escaped>'` to `out` if comment is present.
fn emit_comment(out: &mut String, comment: Option<&str>) {
    if let Some(c) = comment {
        out.push_str(" COMMENT = '");
        out.push_str(&escape_single_quote(c));
        out.push('\'');
    }
}

/// Append ` LABELS = (FILTER)` to `out` when the member is a named filter.
///
/// Emitted after COMMENT / WITH SYNONYMS so the annotation region renders in a
/// stable order; the parser accepts the clauses in any order, so the round-trip
/// does not depend on this choice.
fn emit_labels(out: &mut String, is_filter: bool) {
    if is_filter {
        out.push_str(" LABELS = (FILTER)");
    }
}

/// Append ` WITH SYNONYMS = ('<escaped1>', '<escaped2>')` to `out` if non-empty.
fn emit_synonyms(out: &mut String, synonyms: &[String]) {
    if !synonyms.is_empty() {
        out.push_str(" WITH SYNONYMS = (");
        for (i, s) in synonyms.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('\'');
            out.push_str(&escape_single_quote(s));
            out.push('\'');
        }
        out.push(')');
    }
}

/// Emit a stored single-identifier slot (table alias, relationship
/// `from_alias`, `REFERENCES` target alias) so it round-trips through the body
/// parser. Canonical values — bare words and well-formed `"quoted"`
/// identifiers — are emitted UNCHANGED; anything that would not re-parse to
/// itself as one identifier token (empty, embedded whitespace/comma/paren,
/// unbalanced quotes) is wrapped via [`crate::expand::quote_ident`], which
/// escapes internal `"` and produces a token that DOES round-trip. Idempotent:
/// an already-quoted value is recognised as round-tripping and left as-is.
fn emit_alias(s: &str) -> String {
    if crate::body_parser::identifier_slot_roundtrips_verbatim(s) {
        s.to_string()
    } else {
        crate::expand::quote_ident(s)
    }
}

/// Emit a stored column identifier (a PRIMARY KEY / UNIQUE / FK / REFERENCES
/// column-list entry) so it round-trips. Emitted UNCHANGED when it re-parses
/// verbatim as a single column (no depth-0 comma, balanced quotes/brackets, no
/// surrounding whitespace); otherwise wrapped via
/// [`crate::expand::quote_ident`]. Idempotent (see [`emit_alias`]).
fn emit_column(s: &str) -> String {
    if crate::body_parser::column_roundtrips_verbatim(s) {
        s.to_string()
    } else {
        crate::expand::quote_ident(s)
    }
}

/// Emit a comma-separated column list, each entry passed through
/// [`emit_column`] so a stored value containing a comma cannot silently split
/// into two columns on re-parse.
fn emit_column_list(cols: &[String]) -> String {
    cols.iter()
        .map(|c| emit_column(c))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Emit a stored source-table name (the `AS <table>` slot of a TABLES entry)
/// so it round-trips. Dot-aware: canonical bare and dotted names
/// (`orders`, `schema.orders`, `"db"."t"`) are emitted UNCHANGED; only names
/// that would not re-parse verbatim (empty, embedded whitespace/comma/paren,
/// unbalanced quotes, a bare reserved keyword) are wrapped via
/// [`crate::expand::quote_ident`]. Idempotent (see [`emit_alias`]).
fn emit_table(s: &str) -> String {
    if crate::body_parser::source_table_roundtrips_verbatim(s) {
        s.to_string()
    } else {
        crate::expand::quote_ident(s)
    }
}

/// Emit a member's `alias.` qualifier and its name.
///
/// Both are single-identifier slots read as one token each, exactly like a
/// table alias, so they get the same quote-if-it-would-not-round-trip treatment
/// via [`emit_alias`]. Emitting them RAW was a render-side gap of the same
/// family as RT-5/RT-6, found by `fuzz_render_roundtrip` once its escapes were
/// removed: a metric named `bli]ound` renders as `bli]ound AS sum(t.a)`, and
/// the entry parser — which tracks bracket depth — never sees the `AS` at depth
/// zero, so `GET_DDL` emits DDL this project cannot read back. Quoted, the
/// bracket is inert and the entry parses (probed both spellings).
///
/// Idempotent for the same reason `emit_alias` is: re-parsing a quoted name
/// stores it WITH its quotes, and a well-formed `"..."` round-trips verbatim,
/// so the second render emits it unchanged.
///
/// `entry_initial` says whether the name is the FIRST thing in its entry —
/// false when a `PRIVATE ` access modifier has already been emitted ahead of
/// it. See [`emit_entry_initial_name`] for what turns on that.
fn emit_member_name(
    out: &mut String,
    source_table: Option<&String>,
    name: &str,
    entry_initial: bool,
) {
    if let Some(src) = source_table {
        // A qualifier makes the entry start `<alias>.`, and the access-modifier
        // peel declines when the next non-space character is a dot, so an alias
        // spelled `private` is inert. Only the unqualified form is exposed.
        out.push_str(&emit_alias(src));
        out.push('.');
        out.push_str(&emit_alias(name));
    } else {
        out.push_str(&emit_entry_initial_name(name, entry_initial));
    }
}

/// [`emit_alias`], plus the one collision it cannot see: a bare name that IS an
/// access modifier.
///
/// RT-9 (code-review 2026-08-08). `emit_alias` quotes on LEXING grounds — a
/// value is quoted when it would not re-tokenize as one identifier — and
/// `private` tokenizes perfectly. What breaks is a layer up:
/// `parse_leading_access_modifier` peels an entry-initial `PRIVATE`/`PUBLIC`
/// followed by whitespace as the access modifier BEFORE the entry parser looks
/// for a name, so a YAML-declared derived metric named `private` rendered
/// `private AS total * 2` and replay failed with "Missing metric name before
/// 'AS'". Quoting it is the non-breaking fix — `"private"` is not the keyword,
/// round-trips verbatim, and the value stored is the one that was declared.
///
/// Only when the name is entry-INITIAL. Behind an emitted `PRIVATE ` modifier
/// the peel consumes the modifier and reads `private` as the name, which is
/// already correct; quoting there would store `"private"` where the definition
/// said `private` — a new drift in place of the old one.
///
/// Idempotent: the quoted form is not the bare keyword, so a second render
/// leaves it alone.
fn emit_entry_initial_name(name: &str, entry_initial: bool) -> String {
    if entry_initial
        && (name.eq_ignore_ascii_case("PRIVATE") || name.eq_ignore_ascii_case("PUBLIC"))
    {
        return crate::expand::quote_ident(name);
    }
    emit_alias(name)
}

/// Emit TABLES clause entries.
fn emit_tables(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("TABLES (\n");
    for (i, table) in def.tables.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&emit_alias(&table.alias));
        out.push_str(" AS ");
        out.push_str(&emit_table(&table.table));
        if !table.pk_columns.is_empty() {
            out.push_str(" PRIMARY KEY (");
            out.push_str(&emit_column_list(&table.pk_columns));
            out.push(')');
        }
        for uc in &table.unique_constraints {
            out.push_str(" UNIQUE (");
            out.push_str(&emit_column_list(uc));
            out.push(')');
        }
        emit_comment(out, table.comment.as_deref());
        emit_synonyms(out, &table.synonyms);
        if i + 1 < def.tables.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Emit RELATIONSHIPS clause entries.
fn emit_relationships(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("RELATIONSHIPS (\n");
    for (i, join) in def.joins.iter().enumerate() {
        out.push_str("    ");
        if let Some(ref rel_name) = join.name {
            out.push_str(rel_name);
        }
        out.push_str(" AS ");
        out.push_str(&emit_alias(&join.from_alias));
        out.push('(');
        out.push_str(&emit_column_list(&join.fk_columns));
        out.push_str(") REFERENCES ");
        out.push_str(&emit_alias(&join.table));
        // RT-1 (code-review 2026-07-02): a relationship declared against a
        // UNIQUE key (or any explicit column list differing from the
        // target's PRIMARY KEY) must render its `(ref_columns)` — omitting
        // them makes re-parsing resolve to the target's PK, silently
        // rewiring join semantics. When ref_columns match the target PK the
        // list is redundant and is omitted for the historical compact form.
        if !join.ref_columns.is_empty() && !ref_columns_match_target_pk(def, join) {
            out.push('(');
            out.push_str(&emit_column_list(&join.ref_columns));
            out.push(')');
        }
        if i + 1 < def.joins.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Do `join.ref_columns` equal the target table's PRIMARY KEY columns
/// (order-sensitive)? Returns `false` when the target table cannot be found
/// — the explicit list is then emitted defensively.
///
/// Column names are compared on their LOGICAL identity: a bare name folds
/// to lowercase (bare identifiers are case-insensitive), while a `"quoted"`
/// name compares by its exact unquoted content (quoted identifiers are
/// case-sensitive — `"Id"` != `"ID"`, but `id` == `"id"`). Comparing
/// case-insensitively across the board could wrongly omit `ref_columns`
/// and change join semantics on re-parse (PR #50 review). A false NEGATIVE
/// here merely emits a redundant column list, so ties break toward
/// emitting.
fn ref_columns_match_target_pk(def: &SemanticViewDefinition, join: &crate::model::Join) -> bool {
    let target_lower = join.table.to_ascii_lowercase();
    let Some(target) = def
        .tables
        .iter()
        .find(|t| t.alias.to_ascii_lowercase() == target_lower)
    else {
        return false;
    };
    target.pk_columns.len() == join.ref_columns.len()
        && target
            .pk_columns
            .iter()
            .zip(&join.ref_columns)
            .all(|(a, b)| logical_ident(a) == logical_ident(b))
}

/// Reduce a stored column identifier to its logical form: strip `"..."`
/// quoting (unescaping `""`) for quoted names, fold bare names to ASCII
/// lowercase.
fn logical_ident(s: &str) -> String {
    if let Some(inner) = s.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
        inner.replace("\"\"", "\"")
    } else {
        s.to_ascii_lowercase()
    }
}

/// Emit FACTS clause entries.
fn emit_facts(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("FACTS (\n");
    for (i, fact) in def.facts.iter().enumerate() {
        out.push_str("    ");
        let private = matches!(fact.access, AccessModifier::Private);
        if private {
            out.push_str("PRIVATE ");
        }
        emit_member_name(out, fact.source_table.as_ref(), &fact.name, !private);
        out.push_str(" AS ");
        out.push_str(&fact.expr);
        emit_comment(out, fact.comment.as_deref());
        emit_synonyms(out, &fact.synonyms);
        emit_labels(out, fact.is_filter);
        if i + 1 < def.facts.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Emit DIMENSIONS clause entries.
fn emit_dimensions(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("DIMENSIONS (\n");
    for (i, dim) in def.dimensions.iter().enumerate() {
        out.push_str("    ");
        // DIMENSIONS carry no access modifier, so the name is always first.
        emit_member_name(out, dim.source_table.as_ref(), &dim.name, true);
        out.push_str(" AS ");
        out.push_str(&dim.expr);
        emit_comment(out, dim.comment.as_deref());
        emit_synonyms(out, &dim.synonyms);
        emit_labels(out, dim.is_filter);
        if i + 1 < def.dimensions.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Render a `WindowSpec` back to its DDL expression form.
///
/// Single source of truth shared by `GET_DDL` (`emit_metrics`) and DESCRIBE's
/// `WINDOW_SPEC` property (C-5, code-review 2026-07-11: DESCRIBE carried an
/// inline copy that had drifted — it dropped `frame_clause` entirely and
/// emitted NULLS asymmetrically, contradicting `GET_DDL` for the same stored
/// object).
#[must_use]
pub(crate) fn render_window_spec(ws: &crate::model::WindowSpec) -> String {
    let mut s = String::new();
    emit_window_expr(&mut s, ws);
    s
}

/// Render `NON ADDITIVE BY` entries: `dim [DESC] NULLS FIRST|LAST, ...`.
///
/// NULLS is always explicit (avoids `DuckDB` version divergence). Shared by
/// `GET_DDL` (`emit_metrics`) and DESCRIBE's `NON_ADDITIVE_BY` property (C-5).
#[must_use]
pub(crate) fn render_non_additive_entries(entries: &[crate::model::NonAdditiveDim]) -> String {
    let mut out = String::new();
    for (j, na) in entries.iter().enumerate() {
        if j > 0 {
            out.push_str(", ");
        }
        out.push_str(&na.dimension);
        match na.order {
            SortOrder::Asc => {} // default, omit
            SortOrder::Desc => out.push_str(" DESC"),
        }
        match na.nulls {
            NullsOrder::Last => out.push_str(" NULLS LAST"),
            NullsOrder::First => out.push_str(" NULLS FIRST"),
        }
    }
    out
}

/// Emit a window metric expression reconstructed from its parsed `WindowSpec`.
///
/// Format: `FUNC(inner_metric[, extra_args]) OVER (PARTITION BY EXCLUDING d1, d2 [ORDER BY ...] [frame])`
fn emit_window_expr(out: &mut String, ws: &crate::model::WindowSpec) {
    // Function call: e.g., AVG(total_qty) or LAG(total_qty, 30)
    out.push_str(&ws.window_function);
    out.push('(');
    out.push_str(&ws.inner_metric);
    for arg in &ws.extra_args {
        out.push_str(", ");
        out.push_str(arg);
    }
    out.push(')');

    // OVER clause
    out.push_str(" OVER (");
    let has_partition = if !ws.excluding_dims.is_empty() {
        out.push_str("PARTITION BY EXCLUDING ");
        out.push_str(&ws.excluding_dims.join(", "));
        true
    } else if !ws.partition_dims.is_empty() {
        out.push_str("PARTITION BY ");
        out.push_str(&ws.partition_dims.join(", "));
        true
    } else {
        false
    };
    if !ws.order_by.is_empty() {
        if has_partition {
            out.push(' ');
        }
        out.push_str("ORDER BY ");
        for (j, ob) in ws.order_by.iter().enumerate() {
            if j > 0 {
                out.push_str(", ");
            }
            out.push_str(&ob.expr);
            match ob.order {
                SortOrder::Asc => {} // default, omit
                SortOrder::Desc => out.push_str(" DESC"),
            }
            // Always emit explicit NULLS to avoid DuckDB version divergence
            match ob.nulls {
                NullsOrder::Last => out.push_str(" NULLS LAST"),
                NullsOrder::First => out.push_str(" NULLS FIRST"),
            }
        }
    }
    if let Some(ref frame) = ws.frame_clause {
        if has_partition || !ws.order_by.is_empty() {
            out.push(' ');
        }
        out.push_str(frame);
    }
    out.push(')');
}

/// Emit METRICS clause entries.
fn emit_metrics(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("METRICS (\n");
    for (i, metric) in def.metrics.iter().enumerate() {
        out.push_str("    ");
        let private = matches!(metric.access, AccessModifier::Private);
        if private {
            out.push_str("PRIVATE ");
        }
        emit_member_name(out, metric.source_table.as_ref(), &metric.name, !private);
        if !metric.using_relationships.is_empty() {
            out.push_str(" USING (");
            out.push_str(&metric.using_relationships.join(", "));
            out.push(')');
        }
        if !metric.non_additive_by.is_empty() {
            out.push_str(" NON ADDITIVE BY (");
            out.push_str(&render_non_additive_entries(&metric.non_additive_by));
            out.push(')');
        }
        out.push_str(" AS ");
        if let Some(ref ws) = metric.window_spec {
            // Reconstruct the OVER clause from parsed WindowSpec for normalized formatting
            out.push_str(&render_window_spec(ws));
        } else {
            out.push_str(&metric.expr);
        }
        emit_comment(out, metric.comment.as_deref());
        emit_synonyms(out, &metric.synonyms);
        if i + 1 < def.metrics.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Emit MATERIALIZATIONS clause entries.
fn emit_materializations(out: &mut String, def: &SemanticViewDefinition) {
    out.push_str("MATERIALIZATIONS (\n");
    for (i, mat) in def.materializations.iter().enumerate() {
        out.push_str("    ");
        out.push_str(&mat.name);
        out.push_str(" AS (\n");
        out.push_str("        TABLE ");
        out.push_str(&mat.table);
        if !mat.dimensions.is_empty() || !mat.metrics.is_empty() {
            out.push_str(",\n");
        } else {
            out.push('\n');
        }
        if !mat.dimensions.is_empty() {
            out.push_str("        DIMENSIONS (");
            out.push_str(&mat.dimensions.join(", "));
            out.push(')');
            if !mat.metrics.is_empty() {
                out.push(',');
            }
            out.push('\n');
        }
        if !mat.metrics.is_empty() {
            out.push_str("        METRICS (");
            out.push_str(&mat.metrics.join(", "));
            out.push_str(")\n");
        }
        out.push_str("    )");
        if i + 1 < def.materializations.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str(")\n");
}

/// Reconstruct a `CREATE OR REPLACE SEMANTIC VIEW` DDL statement from a stored
/// definition, with an unqualified view name. Returns `Err` for legacy
/// definitions (empty `tables` vec).
///
/// The output follows body parser clause ordering:
/// TABLES -> RELATIONSHIPS -> FACTS -> DIMENSIONS -> METRICS
pub fn render_create_ddl(name: &str, def: &SemanticViewDefinition) -> Result<String, String> {
    render_create_ddl_qualified(name, def, false)
}

/// [`render_create_ddl`], with control over whether the rendered `CREATE` name
/// carries the view's own schema (TECH-DEBT #25).
///
/// Unqualified is the default — it is Snowflake's, whose
/// `GET_DDL(<type>, <name>[, <use_fully_qualified_names>])` renders a bare name
/// unless the third argument says otherwise. Qualified output is what a
/// dump/restore flow needs: re-running an unqualified `CREATE` puts the view in
/// whatever schema the executing session happens to be in, silently relocating
/// every view that did not live there.
///
/// The schema comes from the definition's `schema_name`, which mirrors the
/// catalog column the `(schema_name, name)` key scopes the view by — so the
/// rendered name is where the view actually is, not where the caller spelled it.
/// A definition recording no schema is an error rather than a silent fall back
/// to the bare name, which would reintroduce the relocation this exists to
/// prevent.
pub fn render_create_ddl_qualified(
    name: &str,
    def: &SemanticViewDefinition,
    qualify: bool,
) -> Result<String, String> {
    if def.tables.is_empty() {
        return Err(
            "Legacy definition format; please re-create using CREATE OR REPLACE SEMANTIC VIEW"
                .to_string(),
        );
    }

    let mut out = String::with_capacity(512);

    // Header. The stored name is bare (quotes were stripped at CREATE), so
    // re-quote when a bare emission would not round-trip — unquoted names
    // fold to lowercase on re-parse (PA-8), and names with whitespace /
    // dots / specials would mis-parse entirely (RT-2).
    out.push_str("CREATE OR REPLACE SEMANTIC VIEW ");
    if qualify {
        // Each part is quoted on its own account: quoting the joined string
        // would emit `"analytics.sales"`, a single name that happens to
        // contain a dot, and restore into a view called exactly that.
        let schema = def.schema_name.as_deref().ok_or_else(|| {
            format!(
                "semantic view '{name}' records no schema, so its DDL cannot be rendered with a \
                 fully-qualified name; render it unqualified, or open the database writable once \
                 so the schema-scoping migration records the schema"
            )
        })?;
        out.push_str(&crate::expand::quote_ident_if_needed(schema));
        out.push('.');
    }
    out.push_str(&crate::expand::quote_ident_if_needed(name));

    // View-level comment
    emit_comment(&mut out, def.comment.as_deref());

    // AS keyword
    out.push_str(" AS\n");

    emit_tables(&mut out, def);

    if !def.joins.is_empty() {
        emit_relationships(&mut out, def);
    }
    if !def.facts.is_empty() {
        emit_facts(&mut out, def);
    }
    if !def.dimensions.is_empty() {
        emit_dimensions(&mut out, def);
    }
    if !def.metrics.is_empty() {
        emit_metrics(&mut out, def);
    }
    if !def.materializations.is_empty() {
        emit_materializations(&mut out, def);
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Dimension, Fact, Join, Metric, TableRef};

    fn minimal_def() -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "revenue".to_string(),
                expr: "SUM(o.amount)".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn test_minimal_view() {
        let def = minimal_def();
        let ddl = render_create_ddl("my_view", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW my_view"));
        assert!(ddl.contains("TABLES ("));
        assert!(ddl.contains("DIMENSIONS ("));
        assert!(ddl.contains("METRICS ("));
        assert!(ddl.contains("o AS orders PRIMARY KEY (id)"));
        assert!(ddl.contains("o.region AS o.region"));
        assert!(ddl.contains("o.revenue AS SUM(o.amount)"));
    }

    // -------------------------------------------------------------------
    // RT-1 (code-review 2026-07-02): ref_columns rendering
    // -------------------------------------------------------------------

    #[test]
    fn test_ref_columns_emitted_when_differing_from_target_pk() {
        // Relationship declared against a UNIQUE key: REFERENCES c(alt_key)
        // must render its column list — bare `REFERENCES c` re-parses to the
        // PK and silently rewires the join.
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            unique_constraints: vec![vec!["alt_key".to_string()]],
            ..Default::default()
        });
        def.joins.push(Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_alt".to_string()],
            ref_columns: vec!["alt_key".to_string()],
            name: Some("o_to_c".to_string()),
            ..Default::default()
        });
        let ddl = render_create_ddl("v", &def).unwrap();
        assert!(
            ddl.contains("o_to_c AS o(customer_alt) REFERENCES c(alt_key)"),
            "ref_columns must be emitted: {ddl}"
        );
    }

    #[test]
    fn test_ref_columns_omitted_when_matching_target_pk() {
        // Historical compact form: when ref_columns equal the target PK the
        // list is redundant on re-parse.
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        });
        def.joins.push(Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ref_columns: vec!["id".to_string()],
            name: Some("o_to_c".to_string()),
            ..Default::default()
        });
        let ddl = render_create_ddl("v", &def).unwrap();
        assert!(
            ddl.contains("o_to_c AS o(customer_id) REFERENCES c\n")
                || ddl.contains("o_to_c AS o(customer_id) REFERENCES c,"),
            "PK-matching ref_columns stay omitted: {ddl}"
        );
    }

    #[test]
    fn test_ref_columns_quoted_case_difference_is_emitted() {
        // Quoted identifiers are case-sensitive: `"ID"` does not equal the
        // PK column `"Id"`, so the explicit list must be emitted (PR #50
        // review — case-insensitive comparison wrongly omitted it).
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["\"Id\"".to_string()],
            ..Default::default()
        });
        def.joins.push(Join {
            table: "c".to_string(),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            ref_columns: vec!["\"ID\"".to_string()],
            name: Some("o_to_c".to_string()),
            ..Default::default()
        });
        let ddl = render_create_ddl("v", &def).unwrap();
        assert!(
            ddl.contains("REFERENCES c(\"ID\")"),
            "quoted case-differing ref_columns must be emitted: {ddl}"
        );
    }

    #[test]
    fn test_ref_columns_bare_vs_quoted_logical_match_is_omitted() {
        // Bare `id` and quoted `"id"` are the same logical identifier, and
        // bare names compare case-insensitively — both omit the list.
        for (pk, rc) in [("\"id\"", "id"), ("id", "ID")] {
            let mut def = minimal_def();
            def.tables.push(TableRef {
                alias: "c".to_string(),
                table: "customers".to_string(),
                pk_columns: vec![pk.to_string()],
                ..Default::default()
            });
            def.joins.push(Join {
                table: "c".to_string(),
                from_alias: "o".to_string(),
                fk_columns: vec!["customer_id".to_string()],
                ref_columns: vec![rc.to_string()],
                name: Some("o_to_c".to_string()),
                ..Default::default()
            });
            let ddl = render_create_ddl("v", &def).unwrap();
            assert!(
                ddl.contains("REFERENCES c\n") || ddl.contains("REFERENCES c,"),
                "pk={pk} rc={rc}: logical match must omit the list: {ddl}"
            );
        }
    }

    // -------------------------------------------------------------------
    // RT-2 (code-review 2026-07-02): view-name quoting in the header
    // -------------------------------------------------------------------

    #[test]
    fn test_view_name_quoted_when_needed() {
        let def = minimal_def();
        // A name with uppercase must be quoted to render round-trippably.
        // (Stored view names fold to lowercase, but the renderer's quoting
        // predicate is exercised in isolation here.)
        let ddl = render_create_ddl("Sales", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"Sales\""));
        // Whitespace.
        let ddl = render_create_ddl("my view", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"my view\""));
        // Dot.
        let ddl = render_create_ddl("a.b", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"a.b\""));
        // Non-ASCII.
        let ddl = render_create_ddl("café", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"café\""));
        // Embedded quote escapes.
        let ddl = render_create_ddl("wei\"rd", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"wei\"\"rd\""));
    }

    #[test]
    fn test_view_name_bare_when_safe() {
        let def = minimal_def();
        let ddl = render_create_ddl("orders_sv_42", &def).unwrap();
        assert!(ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW orders_sv_42 "));
    }

    #[test]
    fn test_view_comment() {
        let mut def = minimal_def();
        def.comment = Some("My view comment".to_string());
        let ddl = render_create_ddl("cv", &def).unwrap();
        assert!(ddl.contains("COMMENT = 'My view comment'"));
        // COMMENT should come before AS
        let comment_pos = ddl.find("COMMENT = 'My view comment'").unwrap();
        let as_pos = ddl.find(" AS\n").unwrap();
        assert!(comment_pos < as_pos);
    }

    #[test]
    fn test_comment_with_single_quote() {
        let mut def = minimal_def();
        def.comment = Some("it's a test".to_string());
        let ddl = render_create_ddl("cv", &def).unwrap();
        assert!(ddl.contains("COMMENT = 'it''s a test'"));
    }

    #[test]
    fn test_relationships() {
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        });
        def.joins = vec![Join {
            name: Some("order_customer".to_string()),
            from_alias: "o".to_string(),
            fk_columns: vec!["customer_id".to_string()],
            table: "c".to_string(),
            ..Default::default()
        }];
        let ddl = render_create_ddl("rv", &def).unwrap();
        assert!(ddl.contains("RELATIONSHIPS ("));
        assert!(ddl.contains("order_customer AS o(customer_id) REFERENCES c"));
    }

    #[test]
    fn test_facts() {
        let mut def = minimal_def();
        def.facts = vec![Fact {
            name: "margin".to_string(),
            expr: "o.amount - o.cost".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }];
        let ddl = render_create_ddl("fv", &def).unwrap();
        assert!(ddl.contains("FACTS ("));
        assert!(ddl.contains("o.margin AS o.amount - o.cost"));
    }

    #[test]
    fn test_private_metric() {
        let mut def = minimal_def();
        def.metrics[0].access = AccessModifier::Private;
        let ddl = render_create_ddl("pv", &def).unwrap();
        assert!(ddl.contains("PRIVATE o.revenue AS SUM(o.amount)"));
    }

    #[test]
    fn test_using_relationships() {
        let mut def = minimal_def();
        def.metrics[0].using_relationships = vec!["rel1".to_string()];
        let ddl = render_create_ddl("uv", &def).unwrap();
        assert!(ddl.contains("USING (rel1)"));
        // USING should come before AS in the metric line
        let using_pos = ddl.find("USING (rel1)").unwrap();
        let as_pos = ddl[using_pos..].find(" AS ").unwrap() + using_pos;
        assert!(using_pos < as_pos);
    }

    #[test]
    fn test_synonyms() {
        let mut def = minimal_def();
        def.dimensions[0].synonyms = vec!["syn1".to_string(), "syn2".to_string()];
        let ddl = render_create_ddl("sv", &def).unwrap();
        assert!(ddl.contains("WITH SYNONYMS = ('syn1', 'syn2')"));
    }

    #[test]
    fn test_object_comments() {
        let mut def = minimal_def();
        def.tables[0].comment = Some("table comment".to_string());
        def.dimensions[0].comment = Some("dim comment".to_string());
        def.metrics[0].comment = Some("metric comment".to_string());

        let mut def2 = def.clone();
        def2.facts = vec![Fact {
            name: "f1".to_string(),
            expr: "o.x".to_string(),
            source_table: Some("o".to_string()),
            comment: Some("fact comment".to_string()),
            ..Default::default()
        }];

        let ddl = render_create_ddl("oc", &def).unwrap();
        assert!(ddl.contains("COMMENT = 'table comment'"));
        assert!(ddl.contains("COMMENT = 'dim comment'"));
        assert!(ddl.contains("COMMENT = 'metric comment'"));

        let ddl2 = render_create_ddl("oc2", &def2).unwrap();
        assert!(ddl2.contains("COMMENT = 'fact comment'"));
    }

    #[test]
    fn test_unique_constraints() {
        let mut def = minimal_def();
        def.tables[0].unique_constraints = vec![vec!["col1".to_string(), "col2".to_string()]];
        let ddl = render_create_ddl("uc", &def).unwrap();
        assert!(ddl.contains("UNIQUE (col1, col2)"));
    }

    #[test]
    fn test_empty_tables_error() {
        let def = SemanticViewDefinition {
            ..Default::default()
        };
        let result = render_create_ddl("legacy", &def);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Legacy definition format"));
    }

    #[test]
    fn test_omits_relationships_when_empty() {
        let def = minimal_def();
        let ddl = render_create_ddl("norels", &def).unwrap();
        assert!(!ddl.contains("RELATIONSHIPS"));
    }

    #[test]
    fn test_omits_facts_when_empty() {
        let def = minimal_def();
        let ddl = render_create_ddl("nofacts", &def).unwrap();
        assert!(!ddl.contains("FACTS"));
    }

    #[test]
    fn test_emit_comment_helper() {
        let mut out = String::new();
        emit_comment(&mut out, None);
        assert!(out.is_empty());

        let hello = "hello".to_string();
        emit_comment(&mut out, Some(hello.as_str()));
        assert_eq!(out, " COMMENT = 'hello'");
    }

    #[test]
    fn test_emit_synonyms_helper() {
        let mut out = String::new();
        emit_synonyms(&mut out, &[]);
        assert!(out.is_empty());

        emit_synonyms(&mut out, &["s1".to_string(), "s2".to_string()]);
        assert_eq!(out, " WITH SYNONYMS = ('s1', 's2')");
    }

    #[test]
    fn test_private_fact() {
        let mut def = minimal_def();
        def.facts = vec![Fact {
            name: "margin".to_string(),
            expr: "o.amount - o.cost".to_string(),
            source_table: Some("o".to_string()),
            access: AccessModifier::Private,
            ..Default::default()
        }];
        let ddl = render_create_ddl("pf", &def).unwrap();
        assert!(ddl.contains("PRIVATE o.margin AS o.amount - o.cost"));
    }

    #[test]
    fn test_derived_metric_no_source_table() {
        let mut def = minimal_def();
        def.metrics.push(Metric {
            name: "profit_margin".to_string(),
            expr: "revenue / cost".to_string(),
            source_table: None,
            ..Default::default()
        });
        let ddl = render_create_ddl("dm", &def).unwrap();
        // Derived metric should appear without source table prefix
        assert!(ddl.contains("    profit_margin AS revenue / cost"));
    }

    #[test]
    fn test_multiple_using_relationships() {
        let mut def = minimal_def();
        def.metrics[0].using_relationships = vec!["rel_a".to_string(), "rel_b".to_string()];
        let ddl = render_create_ddl("mu", &def).unwrap();
        assert!(ddl.contains("USING (rel_a, rel_b)"));
    }

    #[test]
    fn test_synonym_with_single_quote() {
        let mut def = minimal_def();
        def.dimensions[0].synonyms = vec!["it's syn".to_string()];
        let ddl = render_create_ddl("sq", &def).unwrap();
        assert!(ddl.contains("WITH SYNONYMS = ('it''s syn')"));
    }

    #[test]
    fn test_clause_ordering() {
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string()],
            ..Default::default()
        });
        def.joins = vec![Join {
            name: Some("r1".to_string()),
            from_alias: "o".to_string(),
            fk_columns: vec!["cid".to_string()],
            table: "c".to_string(),
            ..Default::default()
        }];
        def.facts = vec![Fact {
            name: "f1".to_string(),
            expr: "o.x".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        }];

        let ddl = render_create_ddl("ordered", &def).unwrap();
        let tables_pos = ddl.find("TABLES (").unwrap();
        let rels_pos = ddl.find("RELATIONSHIPS (").unwrap();
        let facts_pos = ddl.find("FACTS (").unwrap();
        let dims_pos = ddl.find("DIMENSIONS (").unwrap();
        let metrics_pos = ddl.find("METRICS (").unwrap();

        assert!(tables_pos < rels_pos);
        assert!(rels_pos < facts_pos);
        assert!(facts_pos < dims_pos);
        assert!(dims_pos < metrics_pos);
    }

    #[test]
    fn test_non_additive_by_single_dim_default_order() {
        use crate::model::{NonAdditiveDim, NullsOrder, SortOrder};
        let mut def = minimal_def();
        def.metrics[0].non_additive_by = vec![NonAdditiveDim {
            dimension: "date_dim".to_string(),
            order: SortOrder::Asc,
            nulls: NullsOrder::Last,
        }];
        let ddl = render_create_ddl("nav", &def).unwrap();
        assert!(
            ddl.contains("NON ADDITIVE BY (date_dim NULLS LAST)"),
            "Expected NON ADDITIVE BY clause: {ddl}"
        );
        // NON ADDITIVE BY should come before AS
        let na_pos = ddl.find("NON ADDITIVE BY").unwrap();
        let as_pos = ddl[na_pos..].find(" AS ").unwrap() + na_pos;
        assert!(na_pos < as_pos);
    }

    #[test]
    fn test_non_additive_by_desc_nulls_first() {
        use crate::model::{NonAdditiveDim, NullsOrder, SortOrder};
        let mut def = minimal_def();
        def.metrics[0].non_additive_by = vec![NonAdditiveDim {
            dimension: "date_dim".to_string(),
            order: SortOrder::Desc,
            nulls: NullsOrder::First,
        }];
        let ddl = render_create_ddl("nav2", &def).unwrap();
        assert!(
            ddl.contains("NON ADDITIVE BY (date_dim DESC NULLS FIRST)"),
            "Expected NON ADDITIVE BY with DESC NULLS FIRST: {ddl}"
        );
    }

    #[test]
    fn test_non_additive_by_roundtrip() {
        use crate::body_parser::parse_keyword_body;
        use crate::model::{NonAdditiveDim, NullsOrder, SortOrder};
        let mut def = minimal_def();
        def.dimensions.push(Dimension {
            name: "snapshot_date".to_string(),
            expr: "o.snapshot_date".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        def.metrics[0].non_additive_by = vec![NonAdditiveDim {
            dimension: "snapshot_date".to_string(),
            order: SortOrder::Desc,
            nulls: NullsOrder::First,
        }];
        let ddl1 = render_create_ddl("rt", &def).unwrap();
        // Parse the generated DDL body (everything after "AS\n")
        let as_pos = ddl1.find(" AS\n").unwrap();
        let body = format!("AS {}", &ddl1[as_pos + 4..]);
        let kb = parse_keyword_body(&body, 0).expect("Round-trip parse should succeed");
        // Reconstruct from parsed body
        let def2 = SemanticViewDefinition {
            tables: kb.tables,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            joins: kb.relationships,
            facts: kb.facts,
            ..Default::default()
        };
        let ddl2 = render_create_ddl("rt", &def2).unwrap();
        assert_eq!(ddl1, ddl2, "Round-trip DDL should be identical");
    }

    #[test]
    fn test_non_additive_by_with_using() {
        use crate::model::{NonAdditiveDim, NullsOrder, SortOrder};
        let mut def = minimal_def();
        def.metrics[0].using_relationships = vec!["rel1".to_string()];
        def.metrics[0].non_additive_by = vec![NonAdditiveDim {
            dimension: "region".to_string(),
            order: SortOrder::Desc,
            nulls: NullsOrder::First,
        }];
        let ddl = render_create_ddl("uv", &def).unwrap();
        // USING should come before NON ADDITIVE BY
        let using_pos = ddl.find("USING (rel1)").unwrap();
        let na_pos = ddl.find("NON ADDITIVE BY").unwrap();
        let as_pos = ddl[na_pos..].find(" AS ").unwrap() + na_pos;
        assert!(using_pos < na_pos, "USING should precede NON ADDITIVE BY");
        assert!(na_pos < as_pos, "NON ADDITIVE BY should precede AS");
    }

    #[test]
    fn test_multi_column_fk() {
        let mut def = minimal_def();
        def.tables.push(TableRef {
            alias: "c".to_string(),
            table: "customers".to_string(),
            pk_columns: vec!["id".to_string(), "region".to_string()],
            ..Default::default()
        });
        def.joins = vec![Join {
            name: Some("order_cust".to_string()),
            from_alias: "o".to_string(),
            fk_columns: vec!["cust_id".to_string(), "cust_region".to_string()],
            table: "c".to_string(),
            ..Default::default()
        }];
        let ddl = render_create_ddl("mfk", &def).unwrap();
        assert!(ddl.contains("order_cust AS o(cust_id, cust_region) REFERENCES c"));
    }

    #[test]
    fn test_window_spec_basic_emission() {
        use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};
        let mut def = minimal_def();
        def.metrics.push(Metric {
            name: "avg_qty".to_string(),
            expr: "AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month)".to_string(),
            source_table: Some("o".to_string()),
            window_spec: Some(WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "total_qty".to_string(),
                extra_args: vec![],
                excluding_dims: vec!["region".to_string()],
                partition_dims: vec![],
                order_by: vec![WindowOrderBy {
                    expr: "month".to_string(),
                    order: SortOrder::Asc,
                    nulls: NullsOrder::Last,
                }],
                frame_clause: None,
            }),
            ..Default::default()
        });
        let ddl = render_create_ddl("wv", &def).unwrap();
        assert!(
            ddl.contains(
                "AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month NULLS LAST)"
            ),
            "Expected window OVER clause in DDL: {ddl}"
        );
    }

    #[test]
    fn test_window_spec_with_frame_clause() {
        use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};
        let mut def = minimal_def();
        def.metrics.push(Metric {
            name: "avg_qty_7d".to_string(),
            expr: "AVG(total_qty) OVER (...)".to_string(),
            source_table: Some("o".to_string()),
            window_spec: Some(WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "total_qty".to_string(),
                extra_args: vec![],
                excluding_dims: vec!["region".to_string()],
                partition_dims: vec![],
                order_by: vec![WindowOrderBy {
                    expr: "month".to_string(),
                    order: SortOrder::Desc,
                    nulls: NullsOrder::First,
                }],
                frame_clause: Some(
                    "RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW".to_string(),
                ),
            }),
            ..Default::default()
        });
        let ddl = render_create_ddl("wv2", &def).unwrap();
        assert!(
            ddl.contains("AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY month DESC NULLS FIRST RANGE BETWEEN INTERVAL '6 days' PRECEDING AND CURRENT ROW)"),
            "Expected window OVER clause with frame in DDL: {ddl}"
        );
    }

    #[test]
    fn test_window_spec_roundtrip() {
        use crate::body_parser::parse_keyword_body;
        use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};
        let mut def = minimal_def();
        // Add a base metric that the window metric references
        def.metrics.push(Metric {
            name: "total_qty".to_string(),
            expr: "SUM(o.qty)".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        def.metrics.push(Metric {
            name: "avg_qty".to_string(),
            expr: "AVG(total_qty) OVER (PARTITION BY EXCLUDING region ORDER BY region)".to_string(),
            source_table: Some("o".to_string()),
            window_spec: Some(WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "total_qty".to_string(),
                extra_args: vec![],
                excluding_dims: vec!["region".to_string()],
                partition_dims: vec![],
                order_by: vec![WindowOrderBy {
                    expr: "region".to_string(),
                    order: SortOrder::Asc,
                    nulls: NullsOrder::Last,
                }],
                frame_clause: None,
            }),
            ..Default::default()
        });
        let ddl1 = render_create_ddl("rt", &def).unwrap();
        // Parse the generated DDL body
        let as_pos = ddl1.find(" AS\n").unwrap();
        let body = format!("AS {}", &ddl1[as_pos + 4..]);
        let kb = parse_keyword_body(&body, 0).expect("Round-trip parse should succeed");
        let def2 = SemanticViewDefinition {
            tables: kb.tables,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            joins: kb.relationships,
            facts: kb.facts,
            ..Default::default()
        };
        let ddl2 = render_create_ddl("rt", &def2).unwrap();
        assert_eq!(ddl1, ddl2, "Round-trip DDL should be identical");
    }

    #[test]
    fn test_window_spec_partition_by_explicit() {
        use crate::body_parser::parse_keyword_body;
        use crate::model::{NullsOrder, SortOrder, WindowOrderBy, WindowSpec};

        let mut def = minimal_def();
        def.dimensions.push(Dimension {
            name: "month".to_string(),
            expr: "o.month".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        def.metrics.push(Metric {
            name: "avg_rev".to_string(),
            expr: "AVG(revenue) OVER (PARTITION BY region ORDER BY month)".to_string(),
            source_table: Some("o".to_string()),
            window_spec: Some(WindowSpec {
                window_function: "AVG".to_string(),
                inner_metric: "revenue".to_string(),
                extra_args: vec![],
                excluding_dims: vec![],
                partition_dims: vec!["region".to_string()],
                order_by: vec![WindowOrderBy {
                    expr: "month".to_string(),
                    order: SortOrder::Asc,
                    nulls: NullsOrder::Last,
                }],
                frame_clause: None,
            }),
            ..Default::default()
        });
        let ddl = render_create_ddl("test", &def).unwrap();
        assert!(
            ddl.contains("PARTITION BY region"),
            "DDL should contain PARTITION BY region: {ddl}"
        );
        assert!(
            !ddl.contains("EXCLUDING"),
            "DDL should not contain EXCLUDING: {ddl}"
        );

        // Round-trip: parse the generated DDL and re-render
        let as_pos = ddl.find(" AS\n").unwrap();
        let body = format!("AS {}", &ddl[as_pos + 4..]);
        let kb = parse_keyword_body(&body, 0).expect("Round-trip parse should succeed");
        let ws = kb.metrics[1].window_spec.as_ref().unwrap();
        assert!(ws.excluding_dims.is_empty());
        assert_eq!(ws.partition_dims, vec!["region"]);
    }

    // -----------------------------------------------------------------------
    // Phase 54: MATERIALIZATIONS DDL reconstruction tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_materializations_emitted_when_present() {
        use crate::model::Materialization;
        let mut def = minimal_def();
        def.materializations = vec![Materialization {
            name: "daily_rev".to_string(),
            table: "daily_revenue_agg".to_string(),
            dimensions: vec!["region".to_string()],
            metrics: vec!["revenue".to_string()],
        }];
        let ddl = render_create_ddl("mv", &def).unwrap();
        assert!(
            ddl.contains("MATERIALIZATIONS ("),
            "DDL should contain MATERIALIZATIONS: {ddl}"
        );
        assert!(
            ddl.contains("daily_rev AS ("),
            "DDL should contain materialization name: {ddl}"
        );
        assert!(
            ddl.contains("TABLE daily_revenue_agg"),
            "DDL should contain TABLE sub-clause: {ddl}"
        );
        assert!(
            ddl.contains("DIMENSIONS (region)"),
            "DDL should contain DIMENSIONS sub-clause: {ddl}"
        );
        assert!(
            ddl.contains("METRICS (revenue)"),
            "DDL should contain METRICS sub-clause: {ddl}"
        );
    }

    #[test]
    fn test_materializations_omitted_when_empty() {
        let def = minimal_def();
        let ddl = render_create_ddl("nomats", &def).unwrap();
        assert!(
            !ddl.contains("MATERIALIZATIONS"),
            "DDL should not contain MATERIALIZATIONS when empty: {ddl}"
        );
    }

    #[test]
    fn test_materializations_clause_ordering() {
        use crate::model::Materialization;
        let mut def = minimal_def();
        def.materializations = vec![Materialization {
            name: "mat1".to_string(),
            table: "t1".to_string(),
            dimensions: vec!["region".to_string()],
            metrics: vec![],
        }];
        let ddl = render_create_ddl("ordered", &def).unwrap();
        let metrics_pos = ddl.find("METRICS (").unwrap();
        let mats_pos = ddl.find("MATERIALIZATIONS (").unwrap();
        assert!(
            metrics_pos < mats_pos,
            "MATERIALIZATIONS should come after METRICS"
        );
    }

    // -------------------------------------------------------------------
    // RT-4 (fuzz_render_roundtrip, 2026-07-18): render must be IDEMPOTENT on a
    // parser-produced definition (the converge-once invariant the fuzz target
    // asserts):
    //   d1 = parse(render(def));  render(parse(render(d1))) == render(d1)
    // The strong fixpoint render(parse(render(def))) == render(def) on an
    // ARBITRARY def is unsatisfiable — a free-form `expr` with surrounding
    // whitespace is trimmed by the parser and cannot be quote-protected. So we
    // normalize once into the parser's image, then assert re-rendering that def
    // is a fixpoint. The strict parse(render(def)) == def equality for CANONICAL
    // defs lives in tests/roundtrip_proptest.rs.
    // -------------------------------------------------------------------

    /// Mirror of `fuzz_render_roundtrip::body_of`: return the ` AS\n...`
    /// body suffix of a rendered DDL, locating the header's (possibly
    /// quoted) name and optional COMMENT the same quote-aware way the fuzz
    /// target and the body parser do.
    fn body_of(ddl: &str) -> Option<&str> {
        let rest = ddl.strip_prefix("CREATE OR REPLACE SEMANTIC VIEW ")?;
        let name_end = crate::ident::find_identifier_end(rest, true);
        let mut after = &rest[name_end..];
        let trimmed = after.trim_start();
        if trimmed.len() >= 7 && trimmed.as_bytes()[..7].eq_ignore_ascii_case(b"COMMENT") {
            let after_kw = trimmed[7..].trim_start();
            let after_eq = after_kw.strip_prefix('=')?.trim_start();
            let (_, consumed) = crate::util::extract_single_quoted_prefix(after_eq).ok()?;
            after = &after_eq[consumed..];
        }
        let trimmed = after.trim_start();
        if trimmed.len() >= 2 && trimmed.as_bytes()[..2].eq_ignore_ascii_case(b"AS") {
            Some(trimmed)
        } else {
            None
        }
    }

    /// The `fuzz_render_roundtrip` converge-once invariant as a deterministic
    /// assertion, mirroring that target exactly: normalize the (possibly
    /// arbitrary) `def` once via `parse(render(def))` to land on a
    /// parser-produced def, then assert `render` is IDEMPOTENT on it. A re-parse
    /// failure at either stage is tolerated (the strict equality for canonical
    /// defs lives in `roundtrip_proptest`); a SUCCESSFUL re-parse whose
    /// re-render differs is the drift this catches.
    fn assert_render_fixpoint(def: &SemanticViewDefinition) {
        use crate::body_parser::{parse_keyword_body, KeywordBody};
        fn kb_to_def(kb: KeywordBody) -> SemanticViewDefinition {
            SemanticViewDefinition {
                tables: kb.tables,
                joins: kb.relationships,
                facts: kb.facts,
                dimensions: kb.dimensions,
                metrics: kb.metrics,
                materializations: kb.materializations,
                ..Default::default()
            }
        }
        // Stage 0 GENERATES a candidate; it asserts nothing. The
        // `validate_ddl_representable` precondition RT-5 put here was removed
        // after eight rounds of fuzzing showed it converging on a second copy
        // of the parser's validation surface (the eighth rule was a semantic
        // cross-reference — a window metric's inner metric must resolve). The
        // full reasoning, and what this gives up, is on the fuzz target's
        // stage 0; TECH-DEBT #60 tracks it. Keep the two in step.
        let rendered0 = render_create_ddl("fuzz_view", def).expect("def renders");
        let Some(body0) = body_of(&rendered0) else {
            panic!("rendered DDL lost its AS body:\n{rendered0}");
        };
        let Ok(kb1) = parse_keyword_body(body0, 0) else {
            return; // `def` is not in the parser's image — not a contract break
        };
        let d1 = kb_to_def(kb1);
        // Assert render is idempotent on the parser-produced def.
        let rendered1 =
            render_create_ddl("fuzz_view", &d1).expect("parser-produced def must render");
        let Some(body1) = body_of(&rendered1) else {
            panic!("rendered DDL lost its AS body:\n{rendered1}");
        };
        // RT-5: no escape — `d1` came FROM the parser, so `render(d1)` must
        // re-parse. "Tolerated" is what let the contract break go unnoticed.
        let kb2 = parse_keyword_body(body1, 0).unwrap_or_else(|e| {
            panic!(
                "freshly-rendered DDL no longer re-parses: {}\n{rendered1}",
                e.message
            )
        });
        let d2 = kb_to_def(kb2);
        let rendered2 =
            render_create_ddl("fuzz_view", &d2).expect("re-parsed definition must render");
        let body2 = body_of(&rendered2).expect("re-rendered DDL kept its AS body");
        assert_eq!(
            body1, body2,
            "render not idempotent on a parser-produced def\nfirst:\n{rendered1}\nsecond:\n{rendered2}"
        );
    }

    #[test]
    fn test_render_fixpoint_unique_constraint_column_with_comma() {
        // A fully VALID view (a table with a dimension + metric) whose only
        // UNIQUE constraint column is the bare string `a,b`. Emitted verbatim it
        // would be `UNIQUE (a,b)`, which re-parses by splitting at the comma into
        // TWO columns; the quote-protection emits `UNIQUE ("a,b")` instead, so
        // the parser-produced def keeps one column `"a,b"` and re-render is
        // idempotent. Exercises the converge-once assert branch (both parses
        // succeed), not the re-parse-fails escape.
        let mut def = minimal_def();
        def.tables[0].unique_constraints = vec![vec!["a,b".to_string()]];
        assert_render_fixpoint(&def);
    }

    #[test]
    fn test_render_fixpoint_fuzz_seed_empty_alias_and_table() {
        // The first fuzz_render_roundtrip counterexample: empty alias / table,
        // a UNIQUE constraint whose first column carries a depth-0 comma and
        // whose second column is empty, plus assorted empty / edge synonyms and
        // a metric. The empty alias/table are quoted to `""`, which the parser
        // rejects, so the first-stage parse(render(def)) fails and converge-once
        // returns before asserting (the re-parse-fails escape).
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: String::new(),
                table: String::new(),
                pk_columns: vec![],
                unique_constraints: vec![vec![
                    "nticer ,roundtrip seed: tab".to_string(),
                    String::new(),
                ]],
                comment: Some(String::new()),
                synonyms: vec![
                    "".into(),
                    "".into(),
                    "".into(),
                    "n)\n r".into(),
                    "".into(),
                    "".into(),
                    "".into(),
                    "egion,".into(),
                ],
                ..Default::default()
            }],
            dimensions: vec![],
            metrics: vec![Metric {
                name: "b".into(),
                expr: "les ,".into(),
                source_table: Some("\0\0\0".into()),
                access: AccessModifier::Public,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_render_fixpoint(&def);
    }

    #[test]
    fn test_render_fixpoint_fuzz_seed_nul_alias_empty_pk() {
        // Second fuzz_render_roundtrip counterexample (2026-07-18): a NUL byte
        // inside the table alias, an EMPTY PRIMARY KEY column, and a metric
        // whose expr is a single space with a trailing COMMENT. `output_type`
        // (also NUL-bearing) is not rendered, so it is irrelevant. The metric
        // expr `" "` is emitted verbatim on the first render but TRIMMED to ""
        // by the parser on re-parse — a free-form expr cannot be quote-protected,
        // so the strong fixpoint is unsatisfiable. `render` must still be
        // idempotent on the parser-produced definition (converge-once).
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "emasemant\0".to_string(),
                table: "e".to_string(),
                pk_columns: vec![String::new()],
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "m".to_string(),
                expr: " ".to_string(),
                output_type: Some("gint\0".to_string()),
                comment: Some("im regp seeum".to_string()),
                access: AccessModifier::Public,
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_render_fixpoint(&def);
    }

    #[test]
    fn test_render_fixpoint_fuzz_seed_mismatched_brackets_in_window_metric() {
        // fuzz_render_roundtrip, #285: a window metric whose function and
        // arguments are runs of `[`, and a second metric whose USING names hold
        // `}`. Rendered verbatim, the `}`s close the `[`s on the first parse
        // (every bracket kind counted as one depth), which folds the second
        // metric into the first one's expression; re-rendered, that expression
        // no longer closes the METRICS clause.
        let def = SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "e".to_string(),
                table: "m".to_string(),
                comment: Some(String::new()),
                ..Default::default()
            }],
            metrics: vec![
                Metric {
                    name: "v".to_string(),
                    source_table: Some("e".to_string()),
                    using_relationships: vec!["m".to_string()],
                    synonyms: vec![String::new()],
                    window_spec: Some(crate::model::WindowSpec {
                        window_function: "[".to_string(),
                        inner_metric: "[[[[[[[[[".to_string(),
                        extra_args: vec!["[".to_string(), "[[[[[[[[[[".to_string()],
                        excluding_dims: vec![
                            String::new(),
                            String::new(),
                            String::new(),
                            ".".to_string(),
                        ],
                        partition_dims: vec![],
                        order_by: vec![],
                        frame_clause: Some(String::new()),
                    }),
                    ..Default::default()
                },
                Metric {
                    output_type: Some(String::new()),
                    using_relationships: vec![
                        "#\0\0}}\"\0\0\0\0\0\"}}}}}}}}}}}}}IC\u{15}\u{15}\u{15}".to_string(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        String::new(),
                        "_1".to_string(),
                        String::new(),
                        "troujdt}\"\0\0\0\0\0\"}}}}}}".to_string(),
                        "{".to_string(),
                    ],
                    comment: Some("}".to_string()),
                    synonyms: vec![
                        "I.\"]".to_string(),
                        "\"}IC\u{15}\u{15}\u{15}\u{15}\u{15}\u{15}".to_string(),
                    ],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert_render_fixpoint(&def);
    }

    #[test]
    fn test_materializations_ddl_roundtrip() {
        use crate::body_parser::parse_keyword_body;
        use crate::model::Materialization;
        let mut def = minimal_def();
        def.materializations = vec![
            Materialization {
                name: "daily_rev".to_string(),
                table: "daily_revenue_agg".to_string(),
                dimensions: vec!["region".to_string()],
                metrics: vec!["revenue".to_string()],
            },
            Materialization {
                name: "monthly_rev".to_string(),
                table: "monthly_agg".to_string(),
                dimensions: vec![],
                metrics: vec!["revenue".to_string()],
            },
        ];
        let ddl1 = render_create_ddl("rt", &def).unwrap();
        // Parse the generated DDL body
        let as_pos = ddl1.find(" AS\n").unwrap();
        let body = format!("AS {}", &ddl1[as_pos + 4..]);
        let kb = parse_keyword_body(&body, 0).expect("Round-trip parse should succeed");
        let def2 = SemanticViewDefinition {
            tables: kb.tables,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            joins: kb.relationships,
            facts: kb.facts,
            materializations: kb.materializations,
            ..Default::default()
        };
        let ddl2 = render_create_ddl("rt", &def2).unwrap();
        assert_eq!(ddl1, ddl2, "Round-trip DDL should be identical");
    }

    /// Covers BOTH member kinds, and locates each clause rather than just
    /// asserting the string appears somewhere: the previous version of this test
    /// flagged only `dimensions[0]` despite its name, so fact rendering was
    /// never actually exercised here (PR #174 review).
    #[test]
    fn test_labels_filter_renders_on_dimension_and_fact() {
        let mut def = minimal_def();
        def.facts.push(crate::model::Fact {
            name: "is_large".to_string(),
            expr: "o.amount > 100".to_string(),
            source_table: Some("o".to_string()),
            is_filter: true,
            ..Default::default()
        });
        def.dimensions[0].is_filter = true;
        let ddl = render_create_ddl("sv", &def).unwrap();

        assert_eq!(
            ddl.matches("LABELS = (FILTER)").count(),
            2,
            "one per labelled member, dimension AND fact: {ddl}"
        );
        // Anchor each occurrence to its own member so a regression that renders
        // both on one entry (or drops the fact wiring) cannot pass.
        assert!(
            ddl.contains(&format!(
                "{} LABELS = (FILTER)",
                def.dimensions[0].expr.clone()
            )),
            "dimension filter must render on the dimension entry: {ddl}"
        );
        assert!(
            ddl.contains("o.amount > 100 LABELS = (FILTER)"),
            "fact filter must render on the fact entry: {ddl}"
        );
    }

    /// A labelled fact alongside an unlabelled one: only the flagged member
    /// gets the clause.
    #[test]
    fn test_labels_renders_only_on_the_flagged_fact() {
        let mut def = minimal_def();
        def.facts.push(crate::model::Fact {
            name: "plain".to_string(),
            expr: "o.amount".to_string(),
            source_table: Some("o".to_string()),
            ..Default::default()
        });
        def.facts.push(crate::model::Fact {
            name: "flagged".to_string(),
            expr: "o.amount > 100".to_string(),
            source_table: Some("o".to_string()),
            is_filter: true,
            ..Default::default()
        });
        let ddl = render_create_ddl("sv", &def).unwrap();
        assert_eq!(ddl.matches("LABELS = (FILTER)").count(), 1, "{ddl}");
        assert!(ddl.contains("o.amount > 100 LABELS = (FILTER)"), "{ddl}");
    }

    #[test]
    fn test_labels_absent_renders_nothing() {
        let def = minimal_def();
        let ddl = render_create_ddl("sv", &def).unwrap();
        assert!(!ddl.contains("LABELS"), "no spurious LABELS: {ddl}");
    }

    #[test]
    fn test_emit_labels_helper() {
        let mut out = String::new();
        emit_labels(&mut out, false);
        assert!(out.is_empty());
        emit_labels(&mut out, true);
        assert_eq!(out, " LABELS = (FILTER)");
    }

    // -------------------------------------------------------------------
    // RT-9 (code-review 2026-08-08): a bare entry-initial name that IS an
    // access modifier
    // -------------------------------------------------------------------
    // `emit_member_name` quote-protects on LEXING grounds — a name is quoted
    // when it would not re-tokenize as one identifier. `private` tokenizes
    // fine; it is the METRICS entry parser that peels an entry-initial
    // `PRIVATE`/`PUBLIC` as the access modifier before it looks for a name. A
    // YAML-declared derived metric named `private` therefore rendered
    // `private AS total * 2`, and replay failed with "Missing metric name
    // before 'AS'". DDL cannot create the shape (the modifier is peeled there
    // too), so YAML is the only ingress — which is why quoting on emission is
    // the fix rather than rejecting at import: it keeps a definition that is
    // already storable, and `"private"` round-trips unchanged.
    mod access_modifier_name_collision_tests {
        use super::*;

        fn def_with_derived_metric(name: &str, access: AccessModifier) -> SemanticViewDefinition {
            let mut def = minimal_def();
            def.metrics.push(Metric {
                name: name.to_string(),
                expr: "revenue * 2".to_string(),
                source_table: None,
                access,
                ..Default::default()
            });
            def
        }

        /// Replay is the assertion, not the rendered bytes: the point is that
        /// the parser reads back the name that was stored.
        fn replayed_metric_names(def: &SemanticViewDefinition) -> Vec<String> {
            let ddl = render_create_ddl("rt9", def).expect("def renders");
            let body = ddl
                .strip_prefix("CREATE OR REPLACE SEMANTIC VIEW rt9 ")
                .expect("rendered header shape");
            let kb = crate::body_parser::parse_keyword_body(body, 0).unwrap_or_else(|e| {
                panic!("rendered DDL failed to re-parse: {}\n{ddl}", e.message)
            });
            kb.metrics.into_iter().map(|m| m.name).collect()
        }

        #[test]
        fn a_bare_derived_metric_named_private_replays_as_itself() {
            let def = def_with_derived_metric("private", AccessModifier::Public);
            assert_eq!(
                replayed_metric_names(&def),
                vec!["revenue".to_string(), "\"private\"".to_string()],
                "an entry-initial PRIVATE must be quoted so the parser sees a name"
            );
        }

        #[test]
        fn a_bare_derived_metric_named_public_replays_as_itself() {
            // Same peel, other keyword: `parse_leading_access_modifier` strips
            // PUBLIC identically, so fixing only PRIVATE would leave a twin.
            let def = def_with_derived_metric("PuBlIc", AccessModifier::Public);
            assert_eq!(
                replayed_metric_names(&def),
                vec!["revenue".to_string(), "\"PuBlIc\"".to_string()],
                "the collision is case-insensitive, like the keyword"
            );
        }

        #[test]
        fn a_private_metric_named_private_still_renders_its_name_bare() {
            // Control, and the reason the rule is "entry-INITIAL": with the
            // `PRIVATE ` modifier emitted ahead of it the name is no longer
            // first, the peel consumes the modifier, and `private` is read as
            // the name. Quoting unconditionally here would store `"private"`
            // where the definition said `private` — a new drift.
            let def = def_with_derived_metric("private", AccessModifier::Private);
            let ddl = render_create_ddl("rt9", &def).expect("def renders");
            assert!(
                ddl.contains("PRIVATE private AS revenue * 2"),
                "the name stays bare behind the modifier: {ddl}"
            );
            assert_eq!(
                replayed_metric_names(&def),
                vec!["revenue".to_string(), "private".to_string()]
            );
        }

        #[test]
        fn a_qualified_metric_on_an_alias_named_private_still_renders_bare() {
            // Control: `private.m` is not the modifier — the peel declines when
            // the next non-space character is a dot — so nothing needs quoting.
            let mut def = minimal_def();
            def.tables.push(TableRef {
                alias: "private".to_string(),
                table: "p".to_string(),
                ..Default::default()
            });
            def.metrics.push(Metric {
                name: "m".to_string(),
                expr: "SUM(private.x)".to_string(),
                source_table: Some("private".to_string()),
                ..Default::default()
            });
            let ddl = render_create_ddl("rt9", &def).expect("def renders");
            assert!(ddl.contains("private.m AS SUM(private.x)"), "{ddl}");
            assert_eq!(
                replayed_metric_names(&def),
                vec!["revenue".to_string(), "m".to_string()]
            );
        }
    }

    // -------------------------------------------------------------------
    // Fully-qualified header emission (TECH-DEBT #25)
    // -------------------------------------------------------------------
    // `GET_DDL`'s optional third argument asks for the view's own schema in
    // the rendered `CREATE` name, so re-running the output restores the view
    // where it came from rather than into the restoring session's schema.
    mod qualified_name_tests {
        use super::*;

        fn def_in_schema(schema: Option<&str>) -> SemanticViewDefinition {
            SemanticViewDefinition {
                schema_name: schema.map(str::to_string),
                ..minimal_def()
            }
        }

        /// The header carries `<schema>.<name>` when qualification is asked for.
        #[test]
        fn a_qualified_render_prefixes_the_recorded_schema() {
            let def = def_in_schema(Some("analytics"));
            let ddl = render_create_ddl_qualified("sales", &def, true).unwrap();
            assert!(
                ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW analytics.sales AS\n"),
                "{ddl}"
            );
        }

        /// Control, green before and after: the default stays unqualified —
        /// Snowflake's default too, so the two-argument call must not move.
        #[test]
        fn the_default_render_is_unqualified() {
            let def = def_in_schema(Some("analytics"));
            let ddl = render_create_ddl("sales", &def).unwrap();
            assert!(
                ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW sales AS\n"),
                "{ddl}"
            );
            assert!(!ddl.contains("analytics"), "{ddl}");
        }

        /// Each part is quoted on its own account. The wrong fix — quoting the
        /// joined string — yields `"analytics.sales"`, a single name that
        /// happens to contain a dot, and restores into a view called exactly
        /// that. Pinned by parsing the emitted header back.
        #[test]
        fn a_qualified_render_quotes_the_parts_separately() {
            let def = def_in_schema(Some("my schema"));
            let ddl = render_create_ddl_qualified("my view", &def, true).unwrap();
            assert!(
                ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW \"my schema\".\"my view\" AS\n"),
                "{ddl}"
            );
        }

        /// The emitted header is a two-part *reference*, not a one-part name.
        #[test]
        fn a_qualified_header_parses_back_as_schema_and_name() {
            let def = def_in_schema(Some("my schema"));
            let ddl = render_create_ddl_qualified("my view", &def, true).unwrap();
            let header = ddl
                .strip_prefix("CREATE OR REPLACE SEMANTIC VIEW ")
                .and_then(|rest| rest.split(" AS\n").next())
                .expect("header is present");
            let parsed = crate::ident::parse_view_ref(header).expect("header parses");
            assert_eq!(parsed.schema.as_deref(), Some("my schema"), "{ddl}");
            assert_eq!(parsed.name, "my view", "{ddl}");
            assert_eq!(parsed.database, None, "{ddl}");
        }

        /// A definition that records no schema cannot be qualified. Emitting
        /// the bare name instead would silently restore the view into the
        /// executing session's schema — the exact relocation this argument
        /// exists to prevent — so it is an error that names the view.
        #[test]
        fn a_qualified_render_without_a_recorded_schema_is_an_error() {
            let def = def_in_schema(None);
            let err = render_create_ddl_qualified("sales", &def, true).unwrap_err();
            assert!(err.contains("sales"), "{err}");
            assert!(err.contains("schema"), "{err}");
        }

        /// Control, green before and after: a schema-less definition still
        /// renders unqualified, because that asks nothing of the schema.
        #[test]
        fn an_unqualified_render_without_a_recorded_schema_is_fine() {
            let def = def_in_schema(None);
            let ddl = render_create_ddl_qualified("sales", &def, false).unwrap();
            assert!(
                ddl.starts_with("CREATE OR REPLACE SEMANTIC VIEW sales AS\n"),
                "{ddl}"
            );
        }

        /// A view-level comment still lands between the name and `AS`.
        #[test]
        fn a_qualified_render_keeps_the_view_comment_after_the_name() {
            let mut def = def_in_schema(Some("analytics"));
            def.comment = Some("orders".to_string());
            let ddl = render_create_ddl_qualified("sales", &def, true).unwrap();
            assert!(
                ddl.starts_with(
                    "CREATE OR REPLACE SEMANTIC VIEW analytics.sales COMMENT = 'orders' AS\n"
                ),
                "{ddl}"
            );
        }
    }
}
