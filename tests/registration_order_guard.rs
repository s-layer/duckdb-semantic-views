//! Structural guard: the grammar must be the **last** entry in
//! `REGISTRATIONS` (LIFE-2, code-review 2026-08-08).
//!
//! `init_extension` registers everything in `REGISTRATIONS` order and returns
//! `Err` on the first failure. Nothing rolls back, so whatever was registered
//! before the failure stays live for the rest of the process. With the
//! semantic-view syntax registered first (then the parser hook, now the
//! grammar productions, `cpp/src/shim.cpp::sv_register_grammar_hooks`), a
//! failure in any read-side registration would leave the extension
//! *half-active*: semantic-view statements would parse, but the table
//! functions they lower to would be missing. `SHOW SEMANTIC VIEWS` would then
//! lower to `SELECT * FROM list_semantic_views()` and fail on an unresolved
//! function name, and `CREATE SEMANTIC VIEW` would write catalog rows nothing
//! could read back — a worse end state than a LOAD that simply did nothing.
//!
//! The dependency runs one way only:
//!
//! * Every read-side registration is a pure DuckDB Catalog-API call
//!   (`sv_register_table_function_core` / `sv_register_scalar_function_set`
//!   build a `TableFunction` / `ScalarFunction` and hand it to
//!   `Catalog::GetSystemCatalog(db)`). None of them parses SQL, so none of
//!   them needs the grammar.
//! * The grammar's *output* does need them: its productions lower
//!   `SHOW SEMANTIC VIEWS` to `list_semantic_views()`, `DESCRIBE SEMANTIC VIEW`
//!   to `describe_semantic_view(...)`, and `CREATE ... FROM YAML FILE` to
//!   `__sv_compute_create_from_yaml(...)`.
//!
//! So registering the grammar last is the safe order, and it is what this
//! test pins. The window is not closed entirely — the catalog schema
//! (`init_catalog`) is still created before the loop runs — but a partial
//! LOAD leaves semantic DDL *inert* (DuckDB's own
//! `syntax error at or near "SEMANTIC"`) rather than half-working.
//!
//! ## Known limitation
//!
//! This is a syntactic check over the `sv_registrations!` invocation in
//! `src/lib.rs`. It cannot see a dependency introduced *inside* a registration
//! wrapper on the C++ side; the one-way argument above has to be re-checked by
//! hand if a wrapper ever starts running SQL.

use syn::visit::Visit;
use syn::ItemMacro;

/// Collects the `sv_registrations!` invocation's token text from `src/lib.rs`.
struct MacroFinder {
    tokens: Option<String>,
}

impl<'ast> Visit<'ast> for MacroFinder {
    fn visit_item_macro(&mut self, m: &'ast ItemMacro) {
        if m.mac
            .path
            .segments
            .last()
            .is_some_and(|s| s.ident == "sv_registrations")
        {
            self.tokens = Some(m.mac.tokens.to_string());
        }
        syn::visit::visit_item_macro(self, m);
    }
}

/// Pull the string literals out of the macro's token text, in order. The
/// invocation is a list of `("<label>", sv_register_<sym>)` pairs, so the
/// literals are exactly the human labels `init_extension` iterates.
fn labels_in_order(tokens: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = tokens.chars();
    while let Some(c) = chars.next() {
        if c != '"' {
            continue;
        }
        let mut lit = String::new();
        for c in chars.by_ref() {
            if c == '"' {
                break;
            }
            lit.push(c);
        }
        out.push(lit);
    }
    out
}

#[test]
fn grammar_is_registered_last() {
    let src = std::fs::read_to_string("src/lib.rs").expect("read src/lib.rs");
    let file: syn::File = syn::parse_str(&src).expect("parse src/lib.rs");

    let mut finder = MacroFinder { tokens: None };
    finder.visit_file(&file);
    let tokens = finder
        .tokens
        .expect("src/lib.rs must contain an `sv_registrations!` invocation");

    let labels = labels_in_order(&tokens);
    assert!(
        labels.len() > 1,
        "expected several registration labels, got {labels:?} — the scanner \
         is probably out of step with the macro's shape"
    );
    assert_eq!(
        labels.iter().filter(|l| *l == "grammar").count(),
        1,
        "expected exactly one \"grammar\" entry in REGISTRATIONS; got {labels:?}"
    );
    assert_eq!(
        labels.last().map(String::as_str),
        Some("grammar"),
        "LIFE-2: the grammar must be the LAST entry in REGISTRATIONS. \
         `init_extension` returns Err on the first failing registration and \
         rolls nothing back, so registering the grammar earlier leaves a \
         failed LOAD parsing semantic-view statements into calls to read \
         table functions that are absent. Read-side registrations are pure \
         Catalog-API calls and never need the grammar, so it goes last. \
         Order found: {labels:?}"
    );
}
