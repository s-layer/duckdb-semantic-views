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
