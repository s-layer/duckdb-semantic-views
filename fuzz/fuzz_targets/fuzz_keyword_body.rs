#![no_main]
// TC-9 (code-review 2026-07-02): direct fuzz target for the AS-body keyword
// parser — previously it was only reached through the full DDL prefix, so
// most fuzz energy was spent failing prefix detection instead of exploring
// clause parsing. Seeds in fuzz/seeds/fuzz_keyword_body/ include non-ASCII
// and annotation-bearing bodies.
use libfuzzer_sys::fuzz_target;
use semantic_views::body_parser::parse_keyword_body;
use semantic_views::model::SemanticViewDefinition;
use semantic_views::render_ddl::render_create_ddl;

fuzz_target!(|data: &[u8]| {
    let Ok(body) = std::str::from_utf8(data) else {
        return;
    };
    // Must never panic; on success the parsed definition must render
    // (renderability is part of the parse contract — get_ddl runs on
    // anything CREATE accepted).
    if let Ok(kb) = parse_keyword_body(body, 0) {
        // CREATE refuses a view with no tables (TECH-DEBT #94), so the
        // parser never returns one.
        assert!(
            !kb.tables.is_empty(),
            "parse_keyword_body accepted TABLES ()"
        );
        let def = SemanticViewDefinition {
            tables: kb.tables,
            joins: kb.relationships,
            facts: kb.facts,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            materializations: kb.materializations,
            ..Default::default()
        };
        let rendered =
            render_create_ddl("fuzz_view", &def).expect("parsed definition must render");
        assert!(
            rendered.starts_with("CREATE OR REPLACE SEMANTIC VIEW "),
            "unexpected render prefix: {rendered}"
        );
    }
});
