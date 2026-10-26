//! RT-3 (code-review 2026-07-02): the parse ↔ render round-trip property.
//!
//! For every definition in the parser's CANONICAL form,
//!
//! ```text
//! parse_keyword_body(body_of(render_create_ddl(def))) == def
//! ```
//!
//! "Canonical form" means the definition's fields hold exactly what the body
//! parser stores: quoted identifiers RETAIN their quotes (`"my col"`),
//! bare identifiers are bare, expressions are trimmed, and `ref_columns`
//! is either empty (omitted in DDL) or an explicit list that differs from
//! the target's PRIMARY KEY (a PK-equal list is a render-side omission and
//! is re-populated by define-time inference, not by the parser).
//!
//! The hostile-shape generators live in `tests/common/mod.rs` (T-5,
//! code-review 2026-07-11) so the SAME shapes also drive the full
//! `plan_create` CREATE front door in `create_front_door_proptest.rs`.
//!
//! Window metrics are exercised by the fixpoint fuzz target
//! (`fuzz_render_roundtrip`) instead: their expression text is *rebuilt*
//! from `WindowSpec` at render time, so byte-equality of `expr` needs the
//! canonical formatter, not the parser.

use proptest::prelude::*;
use semantic_views::body_parser::parse_keyword_body;
use semantic_views::render_ddl::render_create_ddl;

mod common;
use common::arb_canonical_def;

/// Strip the fixed rendered header for the bare-safe test view name and
/// return the `AS ...` body.
fn body_of(ddl: &str) -> &str {
    ddl.strip_prefix("CREATE OR REPLACE SEMANTIC VIEW rt_view ")
        .expect("rendered header shape")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// parse(render(def)) == def for canonical definitions.
    #[test]
    fn parse_render_roundtrip_is_identity(def in arb_canonical_def()) {
        // At least one of DIMENSIONS/METRICS is required by the grammar.
        prop_assume!(!def.dimensions.is_empty() || !def.metrics.is_empty());

        let rendered = render_create_ddl("rt_view", &def)
            .expect("canonical definitions must render");
        let body = body_of(&rendered);
        let reparsed = parse_keyword_body(body, 0).unwrap_or_else(|e| {
            panic!(
                "rendered DDL failed to re-parse: {}\n--- rendered ---\n{rendered}",
                e.message
            )
        });
        prop_assert_eq!(&reparsed.tables, &def.tables, "tables drift\n{}", rendered);
        prop_assert_eq!(
            &reparsed.relationships,
            &def.joins,
            "relationships drift\n{}",
            rendered
        );
        prop_assert_eq!(&reparsed.facts, &def.facts, "facts drift\n{}", rendered);
        prop_assert_eq!(
            &reparsed.dimensions,
            &def.dimensions,
            "dimensions drift\n{}",
            rendered
        );
        prop_assert_eq!(&reparsed.metrics, &def.metrics, "metrics drift\n{}", rendered);
        prop_assert_eq!(
            &reparsed.materializations,
            &def.materializations,
            "materializations drift\n{}",
            rendered
        );
        prop_assert_eq!(&reparsed.view_tags, &def.tags, "view tags drift\n{}", rendered);
    }
}
