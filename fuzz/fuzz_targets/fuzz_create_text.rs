#![no_main]
//! Fuzz CREATE's text, not a struct (TECH-DEBT #60): what `plan_create`
//! accepts must render back to DDL that plans to the same definition.
//!
//! In the extension the grammar's CREATE production rebuilds the statement's
//! text from `DuckDB`'s tokens (`Tree::text`) and hands it to `plan_create`,
//! so `plan_create` is the planner every CREATE goes through. This target
//! fuzzes it on raw text: the token rebuild itself needs `DuckDB`'s tokenizer
//! and is not exercised here, and raw input can hold trivia (a tab, a comment)
//! that the rebuild would have normalised away, so it searches a superset of
//! the texts production plans. Input that is not already a CREATE gets a
//! `CREATE SEMANTIC VIEW fuzz_view AS ` prefix, so the fuzzer spends its time
//! in the body. Anything that fails to plan is skipped: arbitrary bytes
//! legitimately are not DDL. Anything that plans must:
//!   1. render (`render_create_ddl`);
//!   2. plan again from the rendered text; and
//!   3. render to the same text a second time.
//! Every definition asserted on came from the parser, so no precondition on
//! the input is needed, and the search runs in the direction
//! `fuzz_render_roundtrip` cannot: what the parser *accepts*.

use libfuzzer_sys::fuzz_target;
use semantic_views::model::SemanticViewDefinition;
use semantic_views::parse::{plan_create, RewriteAction};
use semantic_views::render_ddl::render_create_ddl;

fn create_def(text: &str) -> Option<SemanticViewDefinition> {
    match plan_create(text) {
        Ok(Some(RewriteAction::Create { def, .. })) => Some(*def),
        _ => None,
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(input) = std::str::from_utf8(data) else {
        return;
    };
    let lead = input.trim_start().as_bytes();
    let text = if lead.len() >= 6 && lead[..6].eq_ignore_ascii_case(b"create") {
        input.to_string()
    } else {
        format!("CREATE SEMANTIC VIEW fuzz_view AS {input}")
    };
    let Some(d0) = create_def(&text) else {
        return;
    };

    let rendered1 = render_create_ddl("fuzz_view", &d0)
        .unwrap_or_else(|e| panic!("an accepted definition must render: {e}\n{text}"));
    let d1 = create_def(&rendered1).unwrap_or_else(|| {
        panic!("rendered DDL of an accepted definition no longer plans:\n{rendered1}\nfrom:\n{text}")
    });
    let rendered2 =
        render_create_ddl("fuzz_view", &d1).expect("a re-planned definition must render");
    assert_eq!(
        rendered1, rendered2,
        "render is not a fixpoint on a planned definition.\nfrom:\n{text}"
    );
});
