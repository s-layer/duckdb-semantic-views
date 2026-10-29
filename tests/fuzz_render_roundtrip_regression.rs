use semantic_views::body_parser::parse_keyword_body;
use semantic_views::model::{Metric, SemanticViewDefinition, TableRef};
use semantic_views::render_ddl::render_create_ddl;

fn body_of(ddl: &str) -> Option<&str> {
    let rest = ddl.strip_prefix("CREATE OR REPLACE SEMANTIC VIEW ")?;
    let name_end = semantic_views::ident::find_identifier_end(rest, true);
    let mut after = &rest[name_end..];
    let trimmed = after.trim_start();
    if trimmed.len() >= 7 && trimmed.as_bytes()[..7].eq_ignore_ascii_case(b"COMMENT") {
        let after_kw = trimmed[7..].trim_start();
        let after_eq = after_kw.strip_prefix('=')?.trim_start();
        let (_, consumed) = semantic_views::util::extract_single_quoted_prefix(after_eq).ok()?;
        after = &after_eq[consumed..];
    }
    let trimmed = after.trim_start();
    if trimmed.len() >= 2 && trimmed.as_bytes()[..2].eq_ignore_ascii_case(b"AS") {
        Some(trimmed)
    } else {
        None
    }
}

/// The exact input CI's `fuzz_render_roundtrip` crashed on, replayed through
/// the same stages the target runs.
#[test]
fn ci_crash_input_no_longer_breaks_the_fixpoint() {
    let def = SemanticViewDefinition {
        tables: vec![TableRef {
            alias: "emanti\0\0\0\0".into(),
            table: "ew ".into(),
            definition: None,
            pk_columns: vec!["end trur".into()],
            unique_constraints: vec![vec![]],
            comment: Some(String::new()),
            synonyms: vec![],
            tags: Vec::new(),
        }],
        metrics: vec![
            Metric {
                name: "PRIVATE".into(),
                expr: " Regit0$ lRRRRRRRRRRR".into(),
                output_type: Some(String::new()),
                using_relationships: vec![String::new()],
                comment: Some("gio".into()),
                ..Default::default()
            },
            Metric {
                name: "m ?a Regit0,(lR]RRRRRRRRRR".into(),
                expr: String::new(),
                output_type: Some(String::new()),
                using_relationships: vec![String::new()],
                comment: Some("gio".into()),
                ..Default::default()
            },
        ],
        database_name: Some(" ".into()),
        schema_name: Some("a Regit0, lRRRRRRRR".into()),
        ..Default::default()
    };

    let rendered0 = render_create_ddl("fuzz_view", &def).expect("render0");
    let body0 = body_of(&rendered0).expect("body0");

    // TC-14 (code-review 2026-08-08): this used to be
    // `let Ok(kb1) = ... else { return; }` — a silent escape. The input is
    // FIXED, so "the parser happened to reject it" is not a case to skip past:
    // the assertion below evaporates with no signal, the RT-5 fuzz-oracle
    // shape. Making it explicit immediately showed the escape was live rather
    // than defensive — the body does NOT parse today.
    //
    // When TC-14 first made this explicit the body did NOT parse: the metric is
    // named `PRIVATE`, the renderer emitted it bare, and the parser peeled it as
    // the access modifier (review finding RT-9). That was pinned in an `Err` arm
    // as a knowingly-degraded state. RT-9 was fixed in the same review round —
    // `emit_member_name` now quote-protects an entry-initial name colliding with
    // PRIVATE/PUBLIC — so the body parses again and the degraded arm is gone.
    // The invariant the CI crash was about is asserted unconditionally: a
    // round-tripped definition must never yield a metric with no name.
    let kb1 = parse_keyword_body(body0, 0).unwrap_or_else(|e| {
        panic!("rendered body must re-parse (RT-9 quote protection): {e:?}\nbody: {body0}")
    });
    for m in &kb1.metrics {
        assert!(
            !m.name.trim().is_empty(),
            "parser produced a nameless metric: {kb1:#?}"
        );
    }
}
