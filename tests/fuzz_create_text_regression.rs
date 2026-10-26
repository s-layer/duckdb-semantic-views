//! Inputs CI's `fuzz_create_text` crashed on, replayed through the same
//! stages the target runs: plan, render, plan the render, render again.

use semantic_views::model::SemanticViewDefinition;
use semantic_views::parse::{plan_create, RewriteAction};
use semantic_views::render_ddl::render_create_ddl;

fn create_def(text: &str) -> Option<SemanticViewDefinition> {
    match plan_create(text) {
        Ok(Some(RewriteAction::Create { def, .. })) => Some(*def),
        _ => None,
    }
}

fn assert_fixpoint(text: &str) {
    let d0 = create_def(text).expect("the text plans");
    let rendered1 = render_create_ddl("fuzz_view", &d0).expect("an accepted definition renders");
    let d1 = create_def(&rendered1).expect("the rendered DDL plans again");
    let rendered2 = render_create_ddl("fuzz_view", &d1).expect("a re-planned definition renders");
    assert_eq!(
        rendered1, rendered2,
        "render is not a fixpoint.\nfrom:\n{text:?}"
    );
}

/// Run 40187299120: a stray `'` in the view name, and a `--` inside a
/// primary-key column list. The comment pass blanked nothing after the
/// quote, so `-- a` was planned into the key column; the rendered DDL has no
/// quote, so re-planning dropped it. The text is now refused, as `DuckDB`'s
/// tokenizer refuses it.
#[test]
fn quote_in_name_then_line_comment_in_key_list() {
    let mut text = String::from(
        "CREATE SEMANTIC VIEW vics' AS TABLES (o AS or\0\0\0\0Cers PRIMARY KEY (\
         \x19\x19\x19\x19\x19\x19\x19\x19\x19\x19\x19\x05\t\t\t\t\t\t\t\t\t\x08\t\t\t\t\t\t\tL\
         \t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\t\r\t\tI\t\t\t",
    );
    text.push_str(&"\n".repeat(82));
    text.push_str(
        " comment */ -- a\n\0\x02COMMENT\x19\x19\x19\x19\x19\x19\x19\x19\x19\x19\x19\x19id),c.tier) \
         METRICS (t AS COUNT(*));\n",
    );
    let err = plan_create(&text).expect_err("an unterminated quote is refused");
    assert_eq!(err.message, "Unterminated string literal.");
    // Without the stray quote the same text plans, and its render is a
    // fixpoint: the comment is blanked on both plans.
    assert_fixpoint(&text.replacen("vics'", "vics", 1));
}

/// The same shape with a `$` in the name: `v$a$` is an identifier in DuckDB,
/// so it opens no dollar quote and the comment after it is blanked.
#[test]
fn dollar_in_name_then_line_comment_in_key_list() {
    assert_fixpoint(
        "CREATE SEMANTIC VIEW v$a$ AS TABLES (o AS t PRIMARY KEY (x -- a\n y)) \
         METRICS (o.n AS COUNT(*))",
    );
}

/// Run 40184213171: a YAML dimension whose expression holds a depth-0
/// `COMMENT '…'`. The keyword grammar reads that as the entry's comment, so
/// the rendered DDL no longer planned. YAML now refuses the expression.
#[test]
fn yaml_expression_with_a_bare_comment_keyword() {
    let text = "CREATE SEMANTIC VIEW yv FROM YAML $$\n\
                base_table: orders\n\
                tables:\n  - alias: o\n    table: ord\n\
                dimensions:\n  - name: region\n    \
                expr: o.OCREATE SEMANTIC VIEW v COMMENT '= 'orMME region\n    \
                source_table: o\n\
                metrics:\n  - name: revenue\n    expr: SUM(o.amout)\n    qource_table: o\n\
                $$\n";
    let err = plan_create(text).expect_err("an expression the keyword grammar splits is refused");
    assert!(
        err.message.contains("'COMMENT' at depth 0"),
        "{}",
        err.message
    );
    // With the word quoted, the expression imports and renders to a fixpoint.
    assert_fixpoint(&text.replacen("v COMMENT '= '", "v \"COMMENT\" '= '", 1));
}

/// Run 40191639265: a `(` inside a dimension name, `o.(egion`. The clause
/// splitter counted it as an open paren, so the entry ran on to the next
/// unmatched `)`, and the name was stored as `(egion`. The render quoted the
/// name and kept the expression's stray `)`, which then closed DIMENSIONS
/// early. A name slot now refuses an unquoted bracket.
#[test]
fn paren_in_dimension_name() {
    let text = "CREATE SEMANTIC VIEW v COMMENT = 'orders analytics' AS TABLES (o AS orders \
                PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) RELATIONSHIPS \
                (order_customer AS o(customer_id) REFERENCES c) DIMENSIONS (o.(egion AS \
                o.region COGMENT =\x16'sales region', c.tier AS c.tier) METRICS (o.revenue \
                A\x03\0\0\0\0\0\0\0amounu), o.order_count AS COUNT(*));\n";
    let err = plan_create(text).expect_err("an unquoted bracket in a name is refused");
    assert!(
        err.message.contains("'(egion' holds an unquoted '('"),
        "{}",
        err.message
    );
    // Quoted, the name is one identifier and the clause splits as written.
    assert_fixpoint(
        "CREATE SEMANTIC VIEW v AS TABLES (o AS orders PRIMARY KEY (id)) \
         DIMENSIONS (o.\"(egion\" AS o.region, o.tier AS o.tier) METRICS (o.n AS COUNT(*))",
    );
}

/// Run 40189930546 (on main): the same bracket, `o.(o AS …`, in a dimension
/// name, balanced this time by a `)` in the expression.
#[test]
fn balanced_paren_in_dimension_name() {
    let text = "CREATE SEMANTIC VIEW v COMMENT = 'orders analqtics' AS TABLES \
                (o\x01\0\0\0\0\0\0\x04\0\0\x03\0\0\0\0\0\0\0ers PRIMARY KEY (id), c AS customers \
                PRIMARY KEY (id)) RELATIONSHIPS (order_cusJtomer AS o(customer_id) REFERENCES c) \
                DIMENSIONS (o.(o AS orders PRIMARY KEY .amount), os\0 .order_count AS \
                CODUNT(*));\n";
    let err = plan_create(text).expect_err("an unquoted bracket in a name is refused");
    assert!(
        err.message.contains("'(o' holds an unquoted '('"),
        "{}",
        err.message
    );
}

/// Run 40195607830: `AS$$$reg…` with no space after `AS`. The lexer ended the
/// name at the `$`, so `AS` was read as the keyword and `$$$reg…` as the
/// expression; the render puts a space after `AS`, and there `$$` opens a
/// dollar-quoted string that runs to the end. A `$` continues a name, as in
/// DuckDB, so `AS$$$reg` is one name and the entry has no `AS`.
#[test]
fn dollar_quote_glued_to_as() {
    let text = "CREATE SEMANTIC VIEW v COMMENT = 'orders analytics' AS TABLES (o AS orders \
                PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) RELATIONSHIPS \
                (order_customer AS o(customer_id) REFERENCES c) DIMENSIONS (o.region \
                AS$$$reg\0\x7fn \0\0\0\0\0\0\0\0\0COMMENT = 'sales region', c.tier AS c.tier) \
                METRICS (o.revenue AS SUM(o.amount) AS COUNT(*));\n";
    let err = plan_create(text).expect_err("an entry with no `AS` is refused");
    assert!(err.message.contains("Expected 'AS'"), "{}", err.message);
}

/// Run 40231464274: a `'` opened in the view name and closed in the body, in
/// `/*$$'`. The quote hid the `/*` from the comment pass, so it was planned
/// into a relationship column; the rendered DDL has no quote before it, so
/// re-planning read a comment. A quote the name opens is refused wherever it
/// closes, as run 40187299120's unterminated one is, and named for what it
/// is: a string in the name, not an unterminated one.
#[test]
fn quote_in_name_closed_in_the_body() {
    let text = "CREATE SEMANTIC VIEW' AS TABLES(o AS s PRIMARY KEY(),E AS s PRIMARY KEY())\
                RELATIONSHIPS(r AS o(/*$$'a$$)REFERENCES c)DIMENSIONS(o.n AS r)";
    let err = plan_create(text).expect_err("a quote opened in the name is refused");
    assert!(
        err.message
            .starts_with("A view name cannot be a string literal"),
        "{}",
        err.message
    );
    assert_eq!(err.position, Some(text.find('\'').unwrap()));
    // The same for a dollar quote that closes in a comment in the body.
    let text = "CREATE SEMANTIC VIEW $a$ AS TABLES (o AS t PRIMARY KEY (x /*$a$ y))) \
                METRICS (o.n AS COUNT(*))";
    let err = plan_create(text).expect_err("a dollar quote opened in the name is refused");
    assert!(
        err.message
            .starts_with("A view name cannot be a dollar-quoted string"),
        "{}",
        err.message
    );
    assert_eq!(err.position, Some(text.find('$').unwrap()));
    // And for a string that is the whole name.
    let text = "CREATE SEMANTIC VIEW 'my view' AS TABLES (o AS t PRIMARY KEY (id)) \
                METRICS (o.n AS COUNT(*))";
    let err = plan_create(text).expect_err("a string as the name is refused");
    assert!(
        err.message
            .starts_with("A view name cannot be a string literal"),
        "{}",
        err.message
    );
}

/// Run 40234410533: a YAML dimension named `AS` rendered as `o.AS AS o.region`,
/// and the entry parser split it at the first `AS`. A name the entry grammar
/// reads as a keyword (`AS`, and `USING` before a metric's expression) is now
/// rendered quoted, in a member's name and in a table alias, which qualifies
/// every member.
#[test]
fn keyword_names_render_quoted() {
    // `twice` refers to the metric quoted, so that a metric named `USING` can
    // be used at all.
    let yaml = |alias: &str, dim: &str, metric: &str, twice: &str| {
        format!(
            "CREATE SEMANTIC VIEW yv FROM YAML $$\n\
             base_table: orders\n\
             tables:\n  - alias: {alias}\n    table: orders\n    pk_columns:\n      - id\n\
             dimensions:\n  - name: {dim}\n    expr: {alias}.region\n    source_table: {alias}\n\
             metrics:\n  - name: {metric}\n    expr: SUM({alias}.amount)\n    source_table: {alias}\n\
             \x20 - name: twice\n    expr: {twice}\n\
             $$\n"
        )
    };
    // Each case on its own, so that every one is seen to fail without the fix.
    let failing: Vec<_> = [
        ("o", "AS", "total"),
        ("o", "as", "total"),
        ("o", "region", "AS"),
        ("o", "region", "USING"),
        ("AS", "region", "total"),
        ("USING", "region", "total"),
    ]
    .into_iter()
    .filter(|&(alias, dim, metric)| {
        let text = yaml(alias, dim, metric, &format!("'\"{metric}\" * 2'"));
        std::panic::catch_unwind(|| assert_fixpoint(&text)).is_err()
    })
    .collect();
    assert!(failing.is_empty(), "no fixpoint for {failing:?}");
    // Named bare in an expression, `USING` is refused at import: after `AS`
    // the metric grammar reads a depth-0 `USING` as a misplaced clause.
    let err = plan_create(&yaml("o", "region", "USING", "USING * 2")).expect_err("refused");
    assert!(
        err.message.contains("'USING (...)' must come BEFORE 'AS'"),
        "{}",
        err.message
    );
}

/// `create_front_door_proptest`: a relationship named `as` rendered as
/// `as AS t1(fk) REFERENCES t0`, and the entry parser split it at that first
/// `AS`, leaving no name. Only a YAML definition can store the name bare; it
/// is now rendered quoted, and a dotted name (`a.b`) stays verbatim.
#[test]
fn relationship_named_as_renders_quoted() {
    for rel in ["as", "AS"] {
        let text = format!(
            "CREATE SEMANTIC VIEW yv FROM YAML $$\n\
             tables:\n  - alias: o\n    table: orders\n    pk_columns: [id]\n\
             \x20 - alias: c\n    table: customers\n    pk_columns: [id]\n\
             joins:\n  - name: {rel}\n    from_alias: o\n    table: c\n    fk_columns: [cid]\n\
             dimensions: []\n\
             metrics:\n  - name: total\n    expr: SUM(o.amount)\n    source_table: o\n\
             $$\n"
        );
        assert_fixpoint(&text);
    }
}

/// `fuzz_ddl_parse`, run 40198249801: leading non-breaking spaces, which
/// `str::trim` strips but the ASCII lead count does not, put the view-name
/// offset inside a character and the slice panicked. The target's own check
/// follows: what plans must render.
#[test]
fn unicode_space_before_create() {
    let text = format!(
        "{}CREATE SEMANTIC VIEW v\u{a0} AS \u{a8}TABLES (o AS orders PRIMARY KEY (id)) \
         FACTS (o.net_price AS o.net_p.tax_rate) DIMENSIONS ()",
        "\u{a0}".repeat(15)
    );
    if let Ok(Some(RewriteAction::Create { name, def, .. })) = plan_create(&text) {
        render_create_ddl(&name.name, &def).expect("an accepted definition renders");
    }
}
