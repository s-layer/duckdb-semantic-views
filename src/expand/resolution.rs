use crate::model::SemanticViewDefinition;

/// Double-quote a SQL identifier, escaping embedded double quotes.
///
/// `DuckDB` uses `"` for identifier quoting. Internal `"` must be escaped
/// as `""` per the SQL standard.
///
/// # Examples
///
/// ```
/// # use semantic_views::expand::quote_ident;
/// assert_eq!(quote_ident("orders"), "\"orders\"");
/// assert_eq!(quote_ident("col\"name"), "\"col\"\"name\"");
/// ```
#[must_use]
pub fn quote_ident(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Double-quote `ident` only when a bare emission would not round-trip.
///
/// A name is bare-safe iff it matches `[a-z_][a-z0-9_]*` — anything else
/// (uppercase letters, whitespace, dots, quotes, non-ASCII, leading digits,
/// empty) must be quoted: bare view names fold to lowercase on re-parse
/// (PA-8), so an unquoted mixed-case or special-character name silently
/// resolves to a different view or fails to parse (RT-2, code-review
/// 2026-07-02).
///
/// A keyword `DuckDB` refuses as a bare name (`select`, `order`, `left`) is
/// quoted too, as the extension's own DDL refuses it bare.
///
/// Used by `render_ddl` for the stored (bare, quote-stripped) view name so
/// `get_ddl` output re-parses to the same catalog key while common
/// lowercase names keep rendering unquoted.
#[must_use]
pub fn quote_ident_if_needed(ident: &str) -> String {
    let bytes = ident.as_bytes();
    let bare_safe = !bytes.is_empty()
        && !bytes[0].is_ascii_digit()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
        && !crate::ident::is_refused_as_name(ident);
    if bare_safe {
        ident.to_string()
    } else {
        quote_ident(ident)
    }
}

/// Emit a *stored* identifier name (a dimension / metric / fact name, which
/// retains whatever quoting the user wrote) as ONE canonical double-quoted SQL
/// identifier — for an output-column alias or a materialization column
/// reference.
///
/// Stored names keep their quotes (`"order date"` is stored verbatim, quotes
/// included), so feeding one straight to [`quote_ident`] double-quotes it —
/// `quote_ident("\"order date\"")` is `"""order date"""`, an output column
/// literally named with quote characters and a materialization reference to a
/// physically-impossible column (EXP-7, code-review 2026-07-18). Strip to the
/// logical value first — CASE-PRESERVED, since quoted identifiers are
/// case-sensitive (unlike [`crate::ident::normalize_ident_part`], which folds
/// case for *matching*) — then quote exactly once.
///
/// A bare stored name (`region`) or an already-canonical key round-trips
/// unchanged, so non-quoted names emit identically to a plain [`quote_ident`].
/// A name that fails to parse as an identifier falls back to quoting the raw
/// string (fail-clean).
///
/// ```
/// use semantic_views::expand::quote_stored_ident;
/// assert_eq!(quote_stored_ident("region"), "\"region\"");
/// assert_eq!(quote_stored_ident("\"order date\""), "\"order date\"");
/// assert_eq!(quote_stored_ident("\"Order Date\""), "\"Order Date\"");
/// ```
#[must_use]
pub fn quote_stored_ident(stored_name: &str) -> String {
    match crate::ident::parse_qualified_identifier(stored_name) {
        Ok(parts) => parts
            .iter()
            .map(|p| quote_ident(p))
            .collect::<Vec<_>>()
            .join("."),
        Err(_) => quote_ident(stored_name),
    }
}

/// Quote a potentially dot-qualified table reference, normalising already-quoted input.
///
/// Delegates to [`crate::ident::parse_qualified_identifier`] so we operate on the
/// UNQUOTED logical parts of the identifier, then re-emit each part via
/// [`quote_ident`]. This makes the function **idempotent** on already-quoted
/// input — repeated application produces the same canonical form.
///
/// Behaviour:
/// - Bare:                `orders`               -> `"orders"`
/// - Two-part bare:       `jaffle.raw_orders`    -> `"jaffle"."raw_orders"`
/// - Three-part bare:     `catalog.schema.table` -> `"catalog"."schema"."table"`
/// - Already quoted:      `"memory"."main"."v"`  -> `"memory"."main"."v"`  (idempotent)
/// - Mixed quoting:       `main."v"`             -> `"main"."v"`
/// - Embedded `""` escape: `"with""q"`           -> `"with""q"`            (preserved)
/// - Dot inside quoted part: `"a.b"`             -> `"a.b"`                (single part)
///
/// Inputs that fail to parse as a SQL identifier (legacy / malformed strings)
/// are emitted verbatim wrapped in a single pair of quotes via [`quote_ident`];
/// this preserves the bare-name fallback behaviour and never produces
/// double-quoting.
#[must_use]
pub fn quote_table_ref(table: &str) -> String {
    match crate::ident::parse_qualified_identifier(table) {
        Ok(parts) => parts
            .iter()
            .map(|p| quote_ident(p))
            .collect::<Vec<_>>()
            .join("."),
        Err(_) => quote_ident(table),
    }
}

/// The database and schema a bare table name in the view's definition
/// resolves in: the ones stamped at CREATE.
///
/// The schema is the one the BODY resolves in, not the one the view lives in.
/// Those differ when the CREATE name carried a qualifier — a view created as
/// `analytics.sales` from `main` still means `main.orders` by a bare `orders`,
/// following `DuckDB`'s rule that a view body resolves in the creating session's
/// context. Rows written before views were schema-scoped have no
/// `resolution_schema_name`; falling back to `schema_name` is exactly what they
/// always did.
#[must_use]
pub fn body_qualifier(def: &SemanticViewDefinition) -> (Option<&str>, Option<&str>) {
    let schema = def
        .resolution_schema_name
        .as_deref()
        .or(def.schema_name.as_deref());
    (def.database_name.as_deref(), schema)
}

/// The name a `TABLES` entry or materialization table would be found by in
/// the temporary schema: its unquoted name when it is written without a
/// schema, or with the `main` schema (`DuckDB` looks for `main.x` in
/// `temp.main` first). `None` for any other spelling.
fn temp_reachable_name(table: &str) -> Option<String> {
    match crate::ident::parse_qualified_identifier(table) {
        Ok(parts) => match parts.as_slice() {
            [name] => Some(name.clone()),
            [schema, name] if schema.eq_ignore_ascii_case("main") => Some(name.clone()),
            _ => None,
        },
        Err(_) => Some(table.to_string()),
    }
}

/// The unquoted name of `table` when it resolved to a temporary table at
/// CREATE ([`SemanticViewDefinition::temp_tables`]): that table is
/// `temp.main.<name>`.
#[must_use]
pub fn temp_table_name(table: &str, def: &SemanticViewDefinition) -> Option<String> {
    temp_reachable_name(table)
        .filter(|name| def.temp_tables.iter().any(|t| t.eq_ignore_ascii_case(name)))
}

/// The tables the definition names in a way a temporary table could answer
/// (see `temp_reachable_name`), by unquoted name. CREATE records which of
/// these resolve to a temporary table in the creating session
/// ([`SemanticViewDefinition::temp_tables`]).
#[must_use]
pub fn temp_table_candidates(def: &SemanticViewDefinition) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let tables = def.tables.iter().map(|t| t.table.as_str());
    let materialized = def.materializations.iter().map(|m| m.table.as_str());
    for name in tables.chain(materialized).filter_map(temp_reachable_name) {
        if !names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            names.push(name);
        }
    }
    names
}

/// Qualify a table name with the definition's catalog/schema, then quote it.
///
/// If the table name is already dot-qualified (more than one structural part),
/// it is used as-is to avoid double-qualification. Otherwise, `database_name`
/// and `schema_name` from the definition are prepended as available.
///
/// This ensures the expanded SQL uses fully-qualified table references, which
/// is required for execution contexts (e.g. ADBC) that don't inherit the
/// connection's default catalog/schema search path.
///
/// We use structural part-count from [`crate::ident::parse_qualified_identifier`]
/// rather than a raw substring-dot heuristic, so quoted parts that contain
/// a literal `.` (e.g. `"a.b"`) are correctly recognised as single-part bare
/// names that should receive the db/schema prefix. If the input fails to parse
/// (legacy / malformed strings), we fall through to the prepend path with
/// `quote_ident(table)` — the safest option since prepending the catalog
/// context cannot cause downstream re-quote bugs.
#[must_use]
pub fn qualify_and_quote_table_ref(table: &str, def: &SemanticViewDefinition) -> String {
    // Structural "is already qualified" test: a parsed identifier with more
    // than one part means the user already wrote `db.t` / `db.schema.t` /
    // `"db"."schema"."t"` etc. and we must not prepend a second qualifier.
    // A table that resolved to a temporary table at CREATE is that one.
    if let Some(name) = temp_table_name(table, def) {
        return format!("\"temp\".\"main\".{}", quote_ident(&name));
    }

    let is_qualified = matches!(
        crate::ident::parse_qualified_identifier(table),
        Ok(ref parts) if parts.len() > 1
    );
    if is_qualified {
        return quote_table_ref(table);
    }

    let mut parts = Vec::new();
    let (db, schema) = body_qualifier(def);
    if let Some(db) = db {
        parts.push(quote_ident(db));
    }
    if let Some(schema) = schema {
        parts.push(quote_ident(schema));
    }
    // `table` here is logically single-part. If it parses cleanly we emit
    // its unquoted form via quote_ident; if not (malformed) we fall back to
    // quote_ident on the raw string, which is the same shape quote_table_ref
    // uses for its Err branch.
    let last = match crate::ident::parse_qualified_identifier(table) {
        Ok(p) if p.len() == 1 => quote_ident(&p[0]),
        _ => quote_ident(table),
    };
    parts.push(last);
    parts.join(".")
}

/// True when a qualified request's table part matches an item's declared
/// `source_table` (case-insensitive). Items declared WITHOUT a table
/// qualifier (`source_table == None`) are base-table items everywhere else in
/// the expansion layer, so they match when the requested alias is the
/// base/root (first declared) table's alias.
fn source_table_matches(
    source_table: Option<&str>,
    alias: &str,
    def: &SemanticViewDefinition,
) -> bool {
    // PARSE-8: `ident_matches` on the qualifier too. The NAME half of a
    // qualified reference already resolved this way, so `"C"."Region"` matched
    // its dimension and then failed on its own alias — quoting a qualifier is
    // not a different table. For unquoted aliases this is identical to the
    // former `eq_ignore_ascii_case`.
    match source_table {
        Some(st) => crate::ident::ident_matches(st, alias),
        None => def
            .tables
            .first()
            .is_some_and(|t| crate::ident::ident_matches(&t.alias, alias)),
    }
}

/// Look up a dimension by name under `DuckDB`'s case-insensitive identifier
/// rule ([`crate::ident::ident_matches`]): matching ignores case and quoting
/// alike. For unquoted names this is identical to the former
/// `eq_ignore_ascii_case`.
///
/// Supports table-qualified names: if `name` contains a '.' (e.g., "o.region"),
/// splits into (alias, `bare_name`) and matches only dimensions whose
/// `source_table` is that alias (unqualified declarations count as the base
/// table). There is deliberately NO fallback to a bare-name match when the
/// table part doesn't match (SG-14): `x.region` must not silently resolve to
/// a `region` dimension on some other table — the caller surfaces the
/// standard unknown-dimension error (with suggestions) instead.
pub(super) fn find_dimension<'a>(
    def: &'a SemanticViewDefinition,
    name: &str,
) -> Option<&'a crate::model::Dimension> {
    if let Some(dot_pos) = crate::ident::first_unquoted_dot(name) {
        let alias = &name[..dot_pos];
        let bare = &name[dot_pos + 1..];
        def.dimensions.iter().find(|d| {
            crate::ident::ident_matches(&d.name, bare)
                && source_table_matches(d.source_table.as_deref(), alias, def)
        })
    } else {
        def.dimensions
            .iter()
            .find(|d| crate::ident::ident_matches(&d.name, name))
    }
}

/// Look up a fact by name, bare or qualified by its table (`o.amount`), with
/// [`find_dimension`]'s rules: case and quoting are ignored, and a qualifier
/// that is not the fact's own table finds nothing (SG-14).
pub(super) fn find_fact<'a>(
    def: &'a SemanticViewDefinition,
    name: &str,
) -> Option<&'a crate::model::Fact> {
    if let Some(dot_pos) = crate::ident::first_unquoted_dot(name) {
        let alias = &name[..dot_pos];
        let bare = &name[dot_pos + 1..];
        def.facts.iter().find(|f| {
            crate::ident::ident_matches(&f.name, bare)
                && source_table_matches(f.source_table.as_deref(), alias, def)
        })
    } else {
        def.facts
            .iter()
            .find(|f| crate::ident::ident_matches(&f.name, name))
    }
}

/// Canonical match key for a dimension *reference* — bare (`report_date`),
/// dotted (`o.report_date`), or with quoted parts (`o."order date"`).
///
/// Resolves the reference to its declared dimension via [`find_dimension`] and
/// returns that dimension's [`crate::ident::normalize_ident_part`] key, so a
/// reference matches the queried-dimension set regardless of how it is spelled.
///
/// When the reference resolves to no declared dimension it falls back to a
/// **sentinel-prefixed** normalized key ([`UNRESOLVED_DIM_KEY_PREFIX`]). The
/// prefix guarantees an unresolved reference can never equal a resolved
/// dimension's key — a resolved key is a plain normalized identifier and can
/// never begin with the `\0`-delimited sentinel. Without it, an unresolved
/// dotted reference (`s.date`) would collide with a dimension whose stored name
/// is a *quoted* dotted identifier (`"s.date"` also normalizes to `s.date`),
/// making a required-dimension check pass when it should fail (the later SQL
/// emission would then bind-fail instead of erroring cleanly). Unknown
/// references are already hard-errored at CREATE, so this is defence-in-depth
/// for a now-shared helper, keeping it correct independent of caller
/// validation. The sentinel key is still deterministic per reference, so the
/// same unresolved reference keys consistently across all call sites.
///
/// Shared by the `NON ADDITIVE BY` (semi-additive) and window dimension-side
/// matchers so a quoted/dotted reference classifies identically to its bare
/// spelling (TECH-DEBT #28/#30).
pub(super) fn dim_ref_key(def: &SemanticViewDefinition, reference: &str) -> String {
    find_dimension(def, reference).map_or_else(
        || {
            format!(
                "{UNRESOLVED_DIM_KEY_PREFIX}{}",
                crate::ident::normalize_ident_part(reference)
            )
        },
        |d| crate::ident::normalize_ident_part(&d.name),
    )
}

/// Sentinel prefix for [`dim_ref_key`]'s unresolved-reference fallback. Uses NUL
/// delimiters, which a normalized identifier key produced from a declared
/// dimension name never begins with, so an unresolved reference's key can never
/// collide with a real dimension's key.
const UNRESOLVED_DIM_KEY_PREFIX: &str = "\u{0}unresolved\u{0}";

/// Look up a metric by name under `DuckDB`'s case-insensitive identifier rule
/// ([`crate::ident::ident_matches`]): matching ignores case and quoting alike.
///
/// Supports table-qualified names: if `name` contains a '.' (e.g., "o.revenue"),
/// splits into (alias, `bare_name`) and matches only metrics whose
/// `source_table` is that alias (unqualified declarations count as the base
/// table). As with [`find_dimension`], there is NO fallback to a bare-name
/// match when the table part doesn't match (SG-14).
pub(super) fn find_metric<'a>(
    def: &'a SemanticViewDefinition,
    name: &str,
) -> Option<&'a crate::model::Metric> {
    if let Some(dot_pos) = crate::ident::first_unquoted_dot(name) {
        let alias = &name[..dot_pos];
        let bare = &name[dot_pos + 1..];
        def.metrics.iter().find(|m| {
            crate::ident::ident_matches(&m.name, bare)
                && source_table_matches(m.source_table.as_deref(), alias, def)
        })
    } else {
        def.metrics
            .iter()
            .find(|m| crate::ident::ident_matches(&m.name, name))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        dim_ref_key, find_dimension, find_metric, qualify_and_quote_table_ref, quote_ident,
        quote_table_ref,
    };
    use crate::model::SemanticViewDefinition;

    /// Minimal SemanticViewDefinition fixture with optional db_name / schema_name.
    ///
    /// All other vectors are empty — `qualify_and_quote_table_ref` only reads
    /// `database_name` and `schema_name`, so we don't need a full view.
    fn def_with_db_schema(db: Option<&str>, schema: Option<&str>) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![],
            dimensions: vec![],
            metrics: vec![],
            joins: vec![],
            facts: vec![],
            materializations: vec![],
            created_on: None,
            database_name: db.map(str::to_string),
            schema_name: schema.map(str::to_string),
            resolution_schema_name: schema.map(str::to_string),
            temp_tables: vec![],
            comment: None,
            tags: Vec::new(),
        }
    }

    mod find_lookup_tests {
        use super::*;
        use crate::model::{Dimension, Metric, TableRef};

        /// Base table `o` + joined table `c`. `region` lives on `c`;
        /// `status` is declared without a source table (base-table item);
        /// metric `revenue` lives on `c`, metric `order_count` is
        /// unqualified (None).
        fn lookup_def() -> SemanticViewDefinition {
            let mut def = def_with_db_schema(None, None);
            def.tables = vec![
                TableRef {
                    alias: "o".to_string(),
                    table: "orders".to_string(),
                    ..Default::default()
                },
                TableRef {
                    alias: "c".to_string(),
                    table: "customers".to_string(),
                    ..Default::default()
                },
            ];
            def.dimensions = vec![
                Dimension {
                    name: "region".to_string(),
                    expr: "c.region".to_string(),
                    source_table: Some("c".to_string()),
                    ..Default::default()
                },
                Dimension {
                    name: "status".to_string(),
                    expr: "status".to_string(),
                    source_table: None,
                    ..Default::default()
                },
            ];
            def.metrics = vec![
                Metric {
                    name: "revenue".to_string(),
                    expr: "sum(c.amount)".to_string(),
                    source_table: Some("c".to_string()),
                    ..Default::default()
                },
                Metric {
                    name: "order_count".to_string(),
                    expr: "count(*)".to_string(),
                    source_table: None,
                    ..Default::default()
                },
            ];
            def
        }

        #[test]
        fn qualified_dimension_matching_table_resolves() {
            let def = lookup_def();
            let d = find_dimension(&def, "c.region").expect("c.region must resolve");
            assert_eq!(d.name, "region");
        }

        #[test]
        fn qualified_dimension_wrong_table_returns_none() {
            // SG-14: no silent fallback to a dimension on another table.
            let def = lookup_def();
            assert!(find_dimension(&def, "o.region").is_none());
            assert!(find_dimension(&def, "warehouse.region").is_none());
        }

        #[test]
        fn base_alias_matches_unqualified_dimension() {
            // `status` has source_table == None (base-table item), so the
            // base alias qualifies it; a non-base alias does not.
            let def = lookup_def();
            assert!(find_dimension(&def, "o.status").is_some());
            assert!(find_dimension(&def, "O.STATUS").is_some());
            assert!(find_dimension(&def, "c.status").is_none());
        }

        // PARSE-8 follow-on (code-review 2026-08-06): the NAME half of a
        // qualified reference resolved through `ident_matches`, but the ALIAS
        // half still used raw `eq_ignore_ascii_case`, which compares the quote
        // characters as data. So `"C"."Region"` matched its dimension name and
        // then failed on its own qualifier, and `find_dimension` returned None.
        //
        // Found by the PARSE-8 end-to-end test: once CREATE accepted a quoted
        // dotted `NON ADDITIVE BY ("O"."Snap_Date")`, the snapshot fell to the
        // unresolved arm and emitted the raw text as an ORDER BY term, which
        // DuckDB rejected as an unknown column. Quoting a qualifier is not a
        // different table.
        #[test]
        fn quoted_qualifier_resolves_like_its_unquoted_form() {
            let def = lookup_def();
            assert!(
                find_dimension(&def, "\"c\".region").is_some(),
                "a quoted alias names the same table"
            );
            assert!(
                find_dimension(&def, "\"C\".\"Region\"").is_some(),
                "case and quoting are both immaterial on BOTH halves"
            );
            assert!(
                find_metric(&def, "\"C\".\"Revenue\"").is_some(),
                "the metric lookup shares the qualifier rule"
            );
            // The base-table branch takes the same rule.
            assert!(
                find_dimension(&def, "\"O\".\"Status\"").is_some(),
                "an unqualified declaration is reached through the quoted base alias"
            );
        }

        /// Control: relaxing the qualifier must not erase SG-14 — a quoted
        /// reference to the WRONG table must still fail to resolve.
        #[test]
        fn quoted_wrong_qualifier_still_returns_none() {
            let def = lookup_def();
            assert!(find_dimension(&def, "\"o\".\"Region\"").is_none());
            assert!(find_dimension(&def, "\"warehouse\".region").is_none());
        }

        #[test]
        fn qualified_metric_wrong_table_returns_none() {
            let def = lookup_def();
            assert!(find_metric(&def, "c.revenue").is_some());
            assert!(find_metric(&def, "o.revenue").is_none());
            assert!(find_metric(&def, "x.revenue").is_none());
        }

        #[test]
        fn base_alias_matches_unqualified_metric() {
            let def = lookup_def();
            assert!(find_metric(&def, "o.order_count").is_some());
            assert!(find_metric(&def, "c.order_count").is_none());
        }

        #[test]
        fn bare_lookup_unchanged() {
            let def = lookup_def();
            assert!(find_dimension(&def, "region").is_some());
            assert!(find_dimension(&def, "STATUS").is_some());
            assert!(find_metric(&def, "revenue").is_some());
            assert!(find_dimension(&def, "nonexistent").is_none());
        }

        /// A quoted mixed-case stored dimension (`"Region"`) matches
        /// case-INsensitively under DuckDB's identifier rule (revised
        /// 2026-07-12): quoting does not make it case-sensitive, so `"Region"`,
        /// `"region"`, `region`, `REGION`, etc. all resolve to it.
        fn quoted_dim_def() -> SemanticViewDefinition {
            let mut def = def_with_db_schema(None, None);
            def.tables = vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                ..Default::default()
            }];
            def.dimensions = vec![
                // Stored as CREATE captured it — a quoted name retains its
                // surrounding quotes in the stored string.
                Dimension {
                    name: "\"Region\"".to_string(),
                    expr: "o.region".to_string(),
                    source_table: Some("o".to_string()),
                    ..Default::default()
                },
                Dimension {
                    name: "status".to_string(),
                    expr: "o.status".to_string(),
                    source_table: Some("o".to_string()),
                    ..Default::default()
                },
            ];
            def
        }

        #[test]
        fn quoted_stored_name_is_case_insensitive() {
            let def = quoted_dim_def();
            // A quoted mixed-case stored name resolves for a reference of any
            // case, quoted or not (DuckDB case-insensitive; quotes stripped).
            assert!(find_dimension(&def, "\"Region\"").is_some());
            assert!(find_dimension(&def, "\"region\"").is_some());
            assert!(find_dimension(&def, "\"REGION\"").is_some());
            assert!(find_dimension(&def, "region").is_some());
            assert!(find_dimension(&def, "Region").is_some());
            // A genuinely different name still does not resolve.
            assert!(find_dimension(&def, "country").is_none());
        }

        #[test]
        fn unquoted_stored_name_matches_any_case_quoted_or_not() {
            // Unquoted stored `status`: matched by any-case unquoted and any
            // any-case quoted reference alike (quoting is irrelevant to
            // matching in DuckDB).
            let def = quoted_dim_def();
            assert!(find_dimension(&def, "status").is_some());
            assert!(find_dimension(&def, "STATUS").is_some());
            assert!(find_dimension(&def, "\"status\"").is_some());
            assert!(find_dimension(&def, "\"Status\"").is_some());
        }

        /// `dim_ref_key`: a resolved reference keys on its declared dimension's
        /// normalized name.
        #[test]
        fn dim_ref_key_resolves_to_declared_dimension_key() {
            let def = lookup_def();
            // Bare, dotted (matching table), and quoted spellings all key the
            // same as the stored `region` dimension.
            assert_eq!(dim_ref_key(&def, "region"), "region");
            assert_eq!(dim_ref_key(&def, "c.region"), "region");
            assert_eq!(dim_ref_key(&def, "\"Region\""), "region");
        }

        /// `dim_ref_key`: an UNRESOLVED reference's fallback key carries the
        /// sentinel prefix, so it can never collide with a real dimension key —
        /// even one whose stored name is a *quoted dotted* identifier that
        /// normalizes to the same text (`"s.date"` → `s.date`, the same
        /// normalization an unresolved qualified `s.date` would produce). Copilot
        /// review on #130.
        #[test]
        fn dim_ref_key_unresolved_never_collides_with_quoted_dotted_dim_name() {
            let mut def = def_with_db_schema(None, None);
            def.tables = vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                ..Default::default()
            }];
            // Stored dimension name is a QUOTED DOTTED identifier: `"s.date"`.
            // Its canonical key is the plain `s.date`.
            def.dimensions = vec![Dimension {
                name: "\"s.date\"".to_string(),
                expr: "o.sale_date".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }];

            // The declared dimension resolves to the plain key.
            assert_eq!(dim_ref_key(&def, "\"s.date\""), "s.date");

            // A qualified reference `s.date` (alias `s`, name `date`) matches no
            // declared dimension, so it must NOT key as `s.date` (which would
            // let a required-dimension check treat it as the `"s.date"` dim).
            let unresolved = dim_ref_key(&def, "s.date");
            assert_ne!(
                unresolved, "s.date",
                "unresolved reference must not collide with the quoted-dotted dim key"
            );
            assert!(
                unresolved.starts_with('\u{0}'),
                "unresolved key must carry the sentinel prefix: {unresolved:?}"
            );
        }
    }

    mod quote_ident_tests {
        use super::*;

        #[test]
        fn simple_identifier() {
            assert_eq!(quote_ident("orders"), "\"orders\"");
        }

        #[test]
        fn reserved_word() {
            assert_eq!(quote_ident("select"), "\"select\"");
        }

        #[test]
        fn embedded_double_quote() {
            assert_eq!(quote_ident("col\"name"), "\"col\"\"name\"");
        }

        #[test]
        fn identifier_with_spaces() {
            assert_eq!(quote_ident("my table"), "\"my table\"");
        }
    }

    mod quote_table_ref_tests {
        use super::*;

        #[test]
        fn simple_table_name() {
            assert_eq!(quote_table_ref("orders"), "\"orders\"");
        }

        #[test]
        fn catalog_qualified() {
            assert_eq!(
                quote_table_ref("jaffle.raw_orders"),
                "\"jaffle\".\"raw_orders\""
            );
        }

        #[test]
        fn fully_qualified() {
            assert_eq!(
                quote_table_ref("catalog.schema.table"),
                "\"catalog\".\"schema\".\"table\""
            );
        }

        #[test]
        fn reserved_word_parts() {
            assert_eq!(quote_table_ref("select.from"), "\"select\".\"from\"");
        }

        #[test]
        fn embedded_quotes_in_parts() {
            // Input `my"db.my"table` is malformed under the new strict parser
            // (bare parts cannot abut a `"`), so it falls through to the
            // `quote_ident` fallback path: wrap the entire string in a single
            // pair of quotes and escape any internal `"` via `""`.
            assert_eq!(
                quote_table_ref("my\"db.my\"table"),
                "\"my\"\"db.my\"\"table\""
            );
        }

        // -----------------------------------------------------------------
        // Phase 64-03: idempotency / already-quoted input handling.
        // -----------------------------------------------------------------

        #[test]
        fn already_quoted_simple() {
            assert_eq!(quote_table_ref("\"orders\""), "\"orders\"");
        }

        #[test]
        fn already_quoted_two_part() {
            assert_eq!(
                quote_table_ref("\"jaffle\".\"raw_orders\""),
                "\"jaffle\".\"raw_orders\"",
            );
        }

        #[test]
        fn already_quoted_three_part() {
            assert_eq!(
                quote_table_ref("\"memory\".\"main\".\"orders\""),
                "\"memory\".\"main\".\"orders\"",
            );
        }

        #[test]
        fn mixed_quoting_first_quoted() {
            assert_eq!(quote_table_ref("\"main\".orders"), "\"main\".\"orders\"",);
        }

        #[test]
        fn mixed_quoting_last_quoted() {
            assert_eq!(quote_table_ref("main.\"orders\""), "\"main\".\"orders\"",);
        }

        #[test]
        fn mixed_quoting_middle_quoted() {
            assert_eq!(
                quote_table_ref("db.\"schema\".table"),
                "\"db\".\"schema\".\"table\"",
            );
        }

        #[test]
        fn embedded_double_quote_in_quoted_part() {
            assert_eq!(quote_table_ref("\"with\"\"q\""), "\"with\"\"q\"");
        }

        #[test]
        fn dot_inside_quoted_part() {
            // The `.` is data (single quoted part), not a separator.
            assert_eq!(quote_table_ref("\"a.b\""), "\"a.b\"");
        }

        #[test]
        fn whitespace_inside_quoted_part() {
            assert_eq!(quote_table_ref("\"my table\""), "\"my table\"");
        }

        #[test]
        fn idempotent_property_bare() {
            let once = quote_table_ref("orders");
            let twice = quote_table_ref(&once);
            assert_eq!(once, twice);
        }

        #[test]
        fn idempotent_property_fqn() {
            let once = quote_table_ref("memory.main.orders");
            let twice = quote_table_ref(&once);
            assert_eq!(once, twice);
        }

        #[test]
        fn idempotent_property_already_quoted_fqn() {
            // Direct regression coverage for the reported triple-quote bug:
            // re-quoting an already-quoted FQN must not change it.
            let input = "\"memory\".\"main\".\"orders_sv\"";
            assert_eq!(quote_table_ref(input), input);
            let twice = quote_table_ref(&quote_table_ref(input));
            assert_eq!(twice, input);
        }

        #[test]
        fn malformed_falls_back() {
            // `"unterminated` has an unterminated quote → parser returns Err →
            // fallback emits `quote_ident("\"unterminated")` which escapes the
            // lone `"` as `""` and wraps the whole thing in one pair of quotes.
            assert_eq!(quote_table_ref("\"unterminated"), "\"\"\"unterminated\"",);
        }
    }

    mod qualify_and_quote_table_ref_tests {
        use super::*;

        #[test]
        fn bare_name_gets_db_schema_prepended() {
            let def = def_with_db_schema(Some("db"), Some("schema"));
            assert_eq!(
                qualify_and_quote_table_ref("t", &def),
                "\"db\".\"schema\".\"t\"",
            );
        }

        #[test]
        fn bare_name_with_only_schema() {
            let def = def_with_db_schema(None, Some("schema"));
            assert_eq!(qualify_and_quote_table_ref("t", &def), "\"schema\".\"t\"",);
        }

        #[test]
        fn bare_name_no_db_no_schema() {
            let def = def_with_db_schema(None, None);
            assert_eq!(qualify_and_quote_table_ref("t", &def), "\"t\"");
        }

        #[test]
        fn a_table_that_was_temporary_at_create_is_qualified_with_temp() {
            let mut def = def_with_db_schema(Some("db"), Some("schema"));
            def.temp_tables = vec!["Scratch".to_string()];
            assert_eq!(
                qualify_and_quote_table_ref("scratch", &def),
                "\"temp\".\"main\".\"scratch\"",
            );
            assert_eq!(
                qualify_and_quote_table_ref("t", &def),
                "\"db\".\"schema\".\"t\"",
            );
            // `main.scratch` reached the temporary table at CREATE too.
            assert_eq!(
                qualify_and_quote_table_ref("main.scratch", &def),
                "\"temp\".\"main\".\"scratch\"",
            );
            // Written with another schema, the name is that table only.
            assert_eq!(
                qualify_and_quote_table_ref("schema.scratch", &def),
                "\"schema\".\"scratch\"",
            );
        }

        #[test]
        fn quoted_bare_name_with_dot_inside_treated_as_single_part() {
            // `"a.b"` is a SINGLE quoted part (the `.` is data, not a separator).
            // The old substring-dot heuristic mistakenly treated this as
            // qualified and skipped the prepend. The structural test sees one
            // part, so db/schema are correctly prepended.
            let def = def_with_db_schema(Some("db"), Some("schema"));
            assert_eq!(
                qualify_and_quote_table_ref("\"a.b\"", &def),
                "\"db\".\"schema\".\"a.b\"",
            );
        }

        #[test]
        fn already_qualified_two_part() {
            let def = def_with_db_schema(Some("db"), Some("schema"));
            assert_eq!(
                qualify_and_quote_table_ref("jaffle.raw_orders", &def),
                "\"jaffle\".\"raw_orders\"",
            );
        }

        #[test]
        fn already_qualified_quoted_two_part() {
            let def = def_with_db_schema(Some("db"), Some("schema"));
            assert_eq!(
                qualify_and_quote_table_ref("\"jaffle\".\"raw_orders\"", &def),
                "\"jaffle\".\"raw_orders\"",
            );
        }

        #[test]
        fn already_qualified_three_part() {
            let def = def_with_db_schema(Some("db"), Some("schema"));
            assert_eq!(
                qualify_and_quote_table_ref("a.b.c", &def),
                "\"a\".\"b\".\"c\"",
            );
        }

        #[test]
        fn already_qualified_quoted_three_part_idempotent() {
            // The reported bug shape: already-quoted FQN must not be re-quoted.
            let def = def_with_db_schema(Some("ignored"), Some("ignored"));
            let input = "\"memory\".\"main\".\"orders\"";
            assert_eq!(qualify_and_quote_table_ref(input, &def), input);
        }

        #[test]
        fn malformed_falls_through_to_prepend() {
            // `"unterminated` fails to parse. The structural test returns
            // is_qualified == false, so we fall through to the prepend path.
            // The bare-name slot uses quote_ident("\"unterminated") which
            // escapes the lone `"` and wraps once. Result must not panic and
            // must contain the db/schema prefix.
            let def = def_with_db_schema(Some("db"), Some("schema"));
            let result = qualify_and_quote_table_ref("\"unterminated", &def);
            assert!(
                !result.is_empty(),
                "qualify_and_quote_table_ref must not return empty on malformed input",
            );
            assert!(
                result.starts_with("\"db\".\"schema\"."),
                "expected db/schema prefix on malformed input, got: {result}",
            );
        }
    }

    mod temp_table_candidates_tests {
        use super::*;
        use crate::expand::resolution::temp_table_candidates;
        use crate::model::{Materialization, TableRef};

        fn table(table: &str) -> TableRef {
            TableRef {
                alias: "a".to_string(),
                table: table.to_string(),
                ..TableRef::default()
            }
        }

        #[test]
        fn names_written_without_a_schema_or_in_main_are_listed() {
            let mut def = def_with_db_schema(Some("db"), Some("main"));
            def.tables = vec![
                table("orders"),
                table("\"Line Items\""),
                table("ORDERS"),
                table("MAIN.customers"),
            ];
            def.materializations = vec![Materialization {
                table: "daily".to_string(),
                ..Materialization::default()
            }];
            assert_eq!(
                temp_table_candidates(&def),
                ["orders", "Line Items", "customers", "daily"]
            );
        }

        #[test]
        fn names_in_another_schema_or_database_are_not_listed() {
            let mut def = def_with_db_schema(Some("db"), Some("main"));
            def.tables = vec![table("sales.orders"), table("db.main.customers")];
            def.materializations = vec![Materialization {
                table: "agg.daily".to_string(),
                ..Materialization::default()
            }];
            assert!(temp_table_candidates(&def).is_empty());
        }
    }
}
