#![no_main]
// RT-3 (code-review 2026-07-02): machine-check for parse ↔ render drift.
//
// An ARBITRARY `SemanticViewDefinition` is NOT in the image of the parser: a
// metric `expr` may carry surrounding whitespace the parser trims (`" "` →
// `""`), an alias may be a NUL-bearing byte string no lexer emits, a stored
// column may hold a bare depth-0 comma. So the strong fixpoint
// `render(parse(render(def))) == render(def)` is UNSATISFIABLE on arbitrary
// defs — a free-form `expr` cannot be quote-protected, and the parser will
// always re-normalize it (fuzz_render_roundtrip, 2026-07-18).
//
// The satisfiable, meaningful invariant is IDEMPOTENCE OF `render` ON A
// PARSER-PRODUCED def. We normalize once — render the arbitrary def and parse
// it back to land on a def the parser could actually produce — then assert that
// re-rendering that def is a fixpoint:
//
//   d1 = parse(render(def))                 // parser-produced (canonical)
//   render(parse(render(d1))) == render(d1) // render is idempotent on d1
//
// Genuine grammar drift between `render_ddl.rs` and the body parser (a dropped
// field, a reordered clause, a mis-quoted special identifier) still breaks this.
//
// Only the FIRST parse (`def` → `d1`) may fail without a panic, and only
// because an arbitrary `def` is not in the parser's image — that is the
// normalization step, not an assertion. Once `d1` exists it came from the
// parser, so every parse and render after it is a contract with no escape.
// See the long note on stage 0 for why the "YAML-storable ⇒ parseable DDL"
// implication is enforced at the YAML entry point instead of here.
use libfuzzer_sys::fuzz_target;
use semantic_views::body_parser::{parse_keyword_body, KeywordBody};
use semantic_views::model::SemanticViewDefinition;
use semantic_views::render_ddl::render_create_ddl;

/// Strip the rendered header (`CREATE OR REPLACE SEMANTIC VIEW <name>
/// [COMMENT = '...'] `) and return the ` AS\n...` body suffix. Rendered
/// output always uses the exact ` AS\n` separator, and the quoted name /
/// comment ahead of it are located with the same quote-aware helpers the
/// parser uses, so a name containing " AS\n" cannot fool the split.
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

/// Assemble the subset of a parsed [`KeywordBody`] that `render_create_ddl`
/// consumes into a `SemanticViewDefinition`.
fn kb_to_def(kb: KeywordBody) -> SemanticViewDefinition {
    kb.into_definition()
}

fuzz_target!(|def: SemanticViewDefinition| {
    // --- Stage 0: use the arbitrary def only to GENERATE candidate DDL ---
    //
    // This stage deliberately asserts NOTHING, and that is a considered
    // reversal of RT-5. The reasoning, and why the intermediate design failed:
    //
    // RT-5 removed stage 0's escape on the theory that
    // `validate_ddl_representable` characterizes the parser's image — "a def
    // that passes it must render to parseable DDL". Making that true meant
    // teaching the validator every rule the grammar enforces. Eight rounds of
    // fuzzing produced eight such rules (blank member expr; unstorable pk
    // column; raw-emitted member names; derived-metric USING / NON ADDITIVE BY
    // / OVER; expression well-formedness), and round eight was
    //
    //     Window metric '"c "': inner metric 'iu' not found in semantic view
    //
    // — a SEMANTIC cross-reference rule, with `USING` relationship names,
    // `NON ADDITIVE BY` dimensions and materialization targets queued behind
    // it. Completing the precondition means re-implementing the parser's whole
    // validation surface in the model layer, where it is free to drift from
    // the real one. That is a worse defect than the one it closes.
    //
    // So the invariant this target owns is the one its header has always
    // described and the one that is actually true: render is a fixpoint on a
    // PARSER-PRODUCED definition. `d1` below comes from the parser, so it is in
    // the parser's image by construction and needs no precondition at all.
    //
    // What that gives up, stated plainly per the coverage-forward rule: this
    // target no longer asserts "YAML-storable ⇒ renders to parseable DDL".
    // That contract did not go away — it moved to where it can be checked
    // without a second parser: `validate_ddl_representable` still enforces it
    // at the YAML entry point, covered by the unit tests in `src/model.rs` and
    // `test/sql/cr20260806_yaml_ddl_contract.test`. The eight rules above are
    // all still enforced there. TECH-DEBT #60 records what would be needed to
    // assert the implication here too.
    let Ok(rendered0) = render_create_ddl("fuzz_view", &def) else {
        return; // legacy-format defs (empty tables) don't render
    };
    let Some(body0) = body_of(&rendered0) else {
        return;
    };
    let Ok(kb1) = parse_keyword_body(body0, 0) else {
        return; // `def` is not in the parser's image — not a contract break
    };
    let d1 = kb_to_def(kb1); // parser-produced (canonical)

    // --- Assert render is idempotent on the parser-produced def ---
    let rendered1 =
        render_create_ddl("fuzz_view", &d1).expect("parser-produced definition must render");
    let Some(body1) = body_of(&rendered1) else {
        panic!("rendered DDL lost its AS body: {rendered1}");
    };
    // RT-5: no escape here at all. `d1` came FROM the parser, so
    // `render(d1)` failing to re-parse is a contract break by definition. The
    // old comment deferred to `tests/roundtrip_proptest.rs` — whose generators
    // pin `name: Some(...)` and `source_table: Some(...)`, so it structurally
    // could not reach these shapes. Each half pointed at the other.
    let kb2 = parse_keyword_body(body1, 0).unwrap_or_else(|e| {
        panic!(
            "freshly-rendered canonical DDL no longer re-parses: {}\n{rendered1}",
            e.message
        )
    });
    let d2 = kb_to_def(kb2);
    let rendered2 = render_create_ddl("fuzz_view", &d2).expect("re-parsed definition must render");
    let Some(body2) = body_of(&rendered2) else {
        panic!("re-rendered DDL lost its AS body: {rendered2}");
    };
    assert_eq!(
        body1, body2,
        "render is not idempotent on a parser-produced definition — grammar drift \
         between render_ddl and the body parser.\nfirst:\n{rendered1}\nsecond:\n{rendered2}"
    );
});
