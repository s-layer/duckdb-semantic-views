use proptest::prelude::*;
use semantic_views::parse::*;

// ---------------------------------------------------------------------------
// Strategy helpers
// ---------------------------------------------------------------------------

/// Generate a random upper/lower case variation of each character in `prefix`.
fn arb_case_variant(prefix: &'static str) -> impl Strategy<Value = String> {
    let chars: Vec<char> = prefix.chars().collect();
    let len = chars.len();
    proptest::collection::vec(proptest::bool::ANY, len).prop_map(move |bools| {
        chars
            .iter()
            .zip(bools.iter())
            .map(|(c, &upper)| {
                if upper {
                    c.to_ascii_uppercase()
                } else {
                    c.to_ascii_lowercase()
                }
            })
            .collect::<String>()
    })
}

/// Generate random leading whitespace (spaces/tabs, 0-10 chars).
fn arb_whitespace() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[ \t]{0,10}").unwrap()
}

/// Generate a valid view name: starts with letter/underscore, then alphanumeric/underscore.
fn arb_view_name() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z_][a-z0-9_]{0,29}").unwrap()
}

/// Generate trailing semicolons (0-3).
fn arb_semicolons() -> impl Strategy<Value = String> {
    proptest::string::string_regex(";{0,3}").unwrap()
}

// ---------------------------------------------------------------------------
// DDL forms: prefix string to DdlKind mapping
// ---------------------------------------------------------------------------

/// The 3 CREATE-with-body forms: (prefix, DdlKind, function_name).
const CREATE_FORMS: &[(&str, DdlKind, &str)] = &[
    (
        "create semantic view",
        DdlKind::Create,
        "create_semantic_view",
    ),
    (
        "create or replace semantic view",
        DdlKind::CreateOrReplace,
        "create_or_replace_semantic_view",
    ),
    (
        "create semantic view if not exists",
        DdlKind::CreateIfNotExists,
        "create_semantic_view_if_not_exists",
    ),
];

/// Build a minimal valid AS-body suffix: AS TABLES (...) DIMENSIONS (...) METRICS (...)
fn build_as_body_suffix(name: &str) -> String {
    format!(
        " {name} AS TABLES (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.region AS region) METRICS (t.revenue AS SUM(amount))"
    )
}

// ---------------------------------------------------------------------------
// Rewrite properties (TEST-02)
// ---------------------------------------------------------------------------

proptest! {
    /// Rewrite of CREATE forms via plan_create produces a Create action carrying the view name.
    #[test]
    fn rewrite_create_forms(
        form_idx in 0..3usize,
        name in arb_view_name(),
    ) {
        let (prefix, _kind, _fn_name) = CREATE_FORMS[form_idx];
        let ddl = format!("{prefix}{}", build_as_body_suffix(&name));
        let result = plan_create(&ddl);
        prop_assert!(
            result.is_ok(),
            "Expected Ok for valid AS-body DDL, got: {:?}",
            result
        );
        // A rewrite was produced: all three CREATE forms map to
        // RewriteAction::Create carrying the (bare, normalized) view name.
        match result.unwrap().unwrap() {
            RewriteAction::Create { name: n, .. } => {
                // The generator emits bare names, so the reference must be
                // unqualified as well as carrying the right name — a qualifier
                // appearing from nowhere would change which schema the CREATE
                // targets.
                prop_assert_eq!(n.schema, None);
                prop_assert_eq!(n.name, name);
            }
            other => prop_assert!(false, "Expected RewriteAction::Create, got: {:?}", other),
        }
    }

}

// ---------------------------------------------------------------------------
// TC-3 (code-review 2026-07-02): quoted / unicode / keyword name arms. The
// original arb_view_name() alphabet is uniformly [a-z_][a-z0-9_]* — exactly
// the shapes that never trip PA-1/PA-2/PA-8.
// ---------------------------------------------------------------------------

/// Hostile view-name content that must ride inside `"..."` quoting:
/// whitespace, dots, uppercase, keywords, non-ASCII, embedded quotes.
fn arb_hostile_name_content() -> impl Strategy<Value = String> {
    prop_oneof![
        // whitespace / punctuation / uppercase ASCII
        "[A-Za-z][A-Za-z0-9 ,.()_-]{0,12}",
        // non-ASCII
        "[a-zéàçΩ東京☕][a-zéàçΩ東京☕ ]{0,8}",
        // keywords and grammar-colliding shapes
        prop::sample::select(vec![
            "SELECT".to_string(),
            "drop semantic view".to_string(),
            "PRIMARY KEY (id)".to_string(),
            "wéird name".to_string(),
            "we\"ird".to_string(),
        ]),
    ]
    .prop_filter(
        "content must survive trim and not end with whitespace",
        |s| !s.trim().is_empty() && s.trim() == s,
    )
}

proptest! {
    /// A quoted hostile name in CREATE's name slot is captured whole and
    /// folded to lowercase (PA-8), whatever it holds.
    #[test]
    fn create_captures_quoted_hostile_names(
        form_idx in 0..3usize,
        content in arb_hostile_name_content(),
    ) {
        let (prefix, _kind, _fn_name) = CREATE_FORMS[form_idx];
        let quoted = format!("\"{}\"", content.replace('"', "\"\""));
        let ddl = format!("{prefix}{}", build_as_body_suffix(&quoted));
        match plan_create(&ddl) {
            Ok(Some(RewriteAction::Create { name, .. })) => {
                prop_assert_eq!(name.schema, None);
                prop_assert_eq!(name.name, content.to_ascii_lowercase());
            }
            other => prop_assert!(false, "unexpected plan for {:?}: {:?}", ddl, other),
        }
    }

    /// The CREATE prefix matches in any case, after leading whitespace, and
    /// with trailing semicolons.
    #[test]
    fn create_prefix_tolerates_case_whitespace_and_semicolons(
        (form_idx, cased) in (0..3usize)
            .prop_flat_map(|i| (Just(i), arb_case_variant(CREATE_FORMS[i].0))),
        ws in arb_whitespace(),
        semis in arb_semicolons(),
        name in arb_view_name(),
    ) {
        let want = match CREATE_FORMS[form_idx].1 {
            DdlKind::Create => CreateMode::Create,
            DdlKind::CreateOrReplace => CreateMode::OrReplace,
            _ => CreateMode::IfNotExists,
        };
        let ddl = format!("{ws}{cased}{}{semis}", build_as_body_suffix(&name));
        match plan_create(&ddl) {
            Ok(Some(RewriteAction::Create { mode, .. })) => prop_assert_eq!(mode, want),
            other => prop_assert!(false, "unexpected plan for {:?}: {:?}", ddl, other),
        }
    }
}

// ---------------------------------------------------------------------------
// Position invariant properties (TEST-03)
// ---------------------------------------------------------------------------

proptest! {
    /// Non-AS-body syntax returns "Expected 'AS' or 'FROM YAML'" error with position.
    #[test]
    fn position_invariant_paren_body_rejected(
        spaces in "[ ]{0,20}",
    ) {
        let query = format!("{spaces}CREATE SEMANTIC VIEW x (tbles := [])");
        let err = plan_create(&query).unwrap_err();
        prop_assert!(
            err.message.contains("Expected 'AS' or 'FROM YAML'"),
            "Expected 'Expected AS or FROM YAML' error, got: {}",
            err.message
        );
        prop_assert!(err.position.is_some());
    }

    /// Error position for missing 'AS' after view name accounts for whitespace.
    #[test]
    fn position_invariant_missing_as(
        spaces in "[ ]{0,20}",
        name in arb_view_name(),
    ) {
        let query = format!("{spaces}CREATE SEMANTIC VIEW {name}");
        let err = plan_create(&query).unwrap_err();
        let pos = err.position.unwrap();
        // Position should point after the view name.
        prop_assert!(
            pos >= spaces.len() + "CREATE SEMANTIC VIEW ".len() + name.len(),
            "Position {} should be at or after view name end in query: {:?}",
            pos, query
        );
        prop_assert!(
            err.message.contains("Expected 'AS'"),
            "Expected \"Expected 'AS'\" error, got: {}",
            err.message
        );
    }

    /// plan_create returns Ok(Some(_)) for valid AS-body DDL with varying whitespace.
    #[test]
    fn valid_ddl_with_whitespace_succeeds(
        spaces in "[ ]{0,20}",
        name in arb_view_name(),
    ) {
        let query = format!(
            "{spaces}CREATE SEMANTIC VIEW{}", build_as_body_suffix(&name)
        );
        let result = plan_create(&query);
        prop_assert!(
            result.is_ok(),
            "Expected Ok for valid DDL, got: {:?}",
            result
        );
        prop_assert!(
            result.unwrap().is_some(),
            "Expected Some(_) for valid DDL"
        );
    }
}

// ---------------------------------------------------------------------------
// Near-miss safety properties (TEST-04)
// ---------------------------------------------------------------------------

proptest! {}

// ---------------------------------------------------------------------------
// AS-body keyword syntax properties (TEST-06)
// ---------------------------------------------------------------------------

proptest! {

    /// AS-body plan_create returns Ok(Some(_)) for valid keyword body,
    /// producing the CREATE route (RewriteAction::Create, mode Create) with the
    /// view name carried structurally.
    #[test]
    fn as_body_validate_and_rewrite_succeeds(
        name in arb_view_name(),
    ) {
        let query = format!("CREATE SEMANTIC VIEW{}", build_as_body_suffix(&name));
        let result = plan_create(&query);
        prop_assert!(
            result.is_ok(),
            "Expected Ok for valid AS-body DDL, got: {:?}",
            result
        );
        let action = result.unwrap();
        prop_assert!(
            action.is_some(),
            "Expected Some(_) for valid AS-body DDL"
        );
        // Plain CREATE takes the create route (mode Create) and embeds the name.
        match action.unwrap() {
            RewriteAction::Create {
                name: n,
                mode: CreateMode::Create,
                ..
            } => {
                prop_assert_eq!(n.schema, None);
                prop_assert_eq!(n.name, name);
            }
            other => prop_assert!(
                false,
                "Expected create route (mode Create), got: {:?}",
                other
            ),
        }
    }

    /// Error position inside AS-body clause points at the typo byte offset.
    /// This property verifies the base_offset threading invariant from RESEARCH.md Pitfall 1.
    /// Leading whitespace includes spaces, tabs, and newlines — all are valid SQL whitespace.
    #[test]
    fn as_body_position_invariant_clause_typo(
        leading in "[ \t\n]{0,20}",
    ) {
        let query = format!("{leading}CREATE SEMANTIC VIEW x AS TABLSE (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))");
        let err = plan_create(&query).unwrap_err();
        let pos = err.position.unwrap();
        // Position must point at "TABLSE" in the original query.
        prop_assert_eq!(
            &query[pos..pos + 6],
            "TABLSE",
            "Position {} does not point at 'TABLSE' in query: {:?}",
            pos, query
        );
        prop_assert!(
            err.message.contains("TABLES") || err.message.contains("tables"),
            "Expected 'did you mean TABLES' error, got: {}",
            err.message
        );
    }

    /// Clause keywords are case-insensitive (tables/TABLES/Tables all accepted).
    #[test]
    fn as_body_clause_keywords_case_insensitive(
        name in arb_view_name(),
        tables_case in 0..3usize,
        dims_case in 0..3usize,
        metrics_case in 0..3usize,
    ) {
        let tables_kw = ["TABLES", "tables", "Tables"][tables_case];
        let dims_kw = ["DIMENSIONS", "dimensions", "Dimensions"][dims_case];
        let metrics_kw = ["METRICS", "metrics", "Metrics"][metrics_case];
        let query = format!(
            "CREATE SEMANTIC VIEW {name} AS {tables_kw} (t AS orders PRIMARY KEY (id)) \
             {dims_kw} (t.region AS region) {metrics_kw} (t.rev AS SUM(amount))"
        );
        let result = plan_create(&query);
        prop_assert!(
            result.is_ok(),
            "Expected Ok for case variant tables={tables_kw} dims={dims_kw} metrics={metrics_kw}, got: {:?}",
            result
        );
    }
}

// ---------------------------------------------------------------------------
// TEST-07: Prefix inter-keyword whitespace variants
// ---------------------------------------------------------------------------
// These tests MUST FAIL against the current starts_with_ci implementation and
// MUST PASS after Plan 02's token-based fix is in place.
// The current detect_ddl_kind uses literal byte prefix matching so it only
// accepts single ASCII spaces between keywords.

fn arb_inter_keyword_ws() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[ \t\n\r]{1,4}").unwrap()
}

proptest! {

    /// For the 3 CREATE forms, replacing prefix spaces with non-space whitespace
    /// and calling plan_create must return Ok(Some(RewriteAction::Create { .. })).
    #[test]
    fn prefix_whitespace_rewrite_roundtrip(
        form_idx in 0..3usize,
        sep in arb_inter_keyword_ws(),
        name in arb_view_name(),
    ) {
        let (prefix, _kind, _fn_name) = CREATE_FORMS[form_idx];
        let rejoined = prefix.split(' ').collect::<Vec<_>>().join(&sep);
        let query = format!("{rejoined}{}", build_as_body_suffix(&name));
        let result = plan_create(&query);
        prop_assert!(
            result.is_ok(),
            "Expected Ok for whitespace-variant CREATE prefix, got: {:?}",
            result
        );
        let action = result.unwrap();
        prop_assert!(
            action.is_some(),
            "Expected Some(_) for whitespace-variant CREATE prefix"
        );
        prop_assert!(
            matches!(action.unwrap(), RewriteAction::Create { .. }),
            "Whitespace-variant CREATE prefix must produce a Create action"
        );
    }
}

// ---------------------------------------------------------------------------
// TEST-08: Adversarial inputs
// These tests document safe behavior for inputs that are not expected in normal
// DuckDB usage but must not cause panics or exploitable behavior.
// ---------------------------------------------------------------------------

#[test]
fn test_large_input_no_panic() {
    // 1 MB of non-DDL text must be refused as "not a CREATE" in one linear
    // pass (comment blanking scans all of it), without panicking. The bound
    // leaves room for a debug build on a loaded runner (about 30ms locally)
    // while still catching a quadratic scan, which would take seconds.
    let s: String = std::iter::repeat('A').take(1_000_000).collect();
    let start = std::time::Instant::now();
    let result = plan_create(&s);
    let elapsed = start.elapsed();
    assert!(
        matches!(result, Ok(None)),
        "1MB non-DDL should not plan as a CREATE, got {result:?}"
    );
    assert!(
        elapsed.as_millis() < 500,
        "1MB scan took {}ms, expected <500ms",
        elapsed.as_millis()
    );
}

#[test]
fn test_unicode_homoglyph() {
    // Cyrillic 'С' (U+0421) looks like Latin 'C' but is a different byte
    // sequence. The ASCII-only prefix matcher must not read it as CREATE.
    let query =
        "СREATE SEMANTIC VIEW x AS TABLES (t AS t PRIMARY KEY (id)) DIMENSIONS (t.a AS t.a)";
    assert!(
        matches!(plan_create(query), Ok(None)),
        "Cyrillic С prefix must not plan as a CREATE"
    );
}

#[test]
fn test_null_byte_in_name() {
    // Null bytes are valid UTF-8 in Rust but may truncate C strings at the FFI boundary.
    // The parser must not panic. It may return Ok (if null byte in name is tolerated)
    // or Err (if the name validator rejects it). Either is acceptable.
    let query = "CREATE SEMANTIC VIEW x\x00bad (tables := [], dimensions := [])";
    let result = std::panic::catch_unwind(|| plan_create(query));
    assert!(result.is_ok(), "plan_create panicked on null byte in name");
}

/// A `;` in the view-name position is rejected — the name scanner stops at it,
/// so the trailing text is left unconsumed and the statement fails to parse.
///
/// TC-12: the invariant this test's comment used to claim ("the rewritten SQL
/// must start with `SELECT * FROM`") stopped being expressible here when
/// `plan_create` began returning a structured `RewriteAction` instead of a SQL
/// string — the only variant that carries SQL at all is `Passthrough`, and a
/// CREATE never produces one. All three match arms were consequently left
/// assertion-free, so the test could only have failed by panicking.
///
/// Each shape is paired with the identical statement minus the `;injection`,
/// which must parse. Without that control the rejections would be attributable
/// to anything in the query, and the test would pass just as happily if the
/// grammar broke entirely.
#[test]
fn test_semicolon_in_name() {
    // (with `;injection` in the name position, same statement without it)
    let pairs = [(
        "CREATE SEMANTIC VIEW x;injection AS TABLES (t) DIMENSIONS (t.a AS t.a)",
        "CREATE SEMANTIC VIEW x AS TABLES (t) DIMENSIONS (t.a AS t.a)",
    )];

    for (injected, control) in pairs {
        let Err(err) = plan_create(injected) else {
            panic!(
                "a ';' in the name position must be rejected, not absorbed \
                 into the name: {injected:?} was accepted"
            )
        };
        assert!(
            err.position.is_some(),
            "the rejection must carry a caret position: {injected:?} -> {err:?}"
        );

        // Anti-vacuity: the same statement without the `;injection` parses, so
        // the rejection above is attributable to the `;` and not to the rest of
        // the query being malformed.
        let ok = plan_create(control)
            .unwrap_or_else(|e| panic!("control statement must parse: {control:?} -> {e:?}"));
        assert!(
            ok.is_some(),
            "control statement must be recognised as ours: {control:?}"
        );
    }

    // The historical input from this test, kept for continuity. It is malformed
    // for a second reason (no `AS`), so it carries no control pair.
    assert!(
        plan_create("CREATE SEMANTIC VIEW x;injection (tables := [], dimensions := [])").is_err(),
        "legacy named-argument CREATE form with an injected ';' must be rejected"
    );
}

#[test]
fn test_control_char_in_name() {
    // Control characters (0x01-0x1F except whitespace) in view names must not panic.
    let query = "CREATE SEMANTIC VIEW x\x01bad (tables := [], dimensions := [])";
    let result = std::panic::catch_unwind(|| plan_create(query));
    assert!(
        result.is_ok(),
        "plan_create panicked on control char in name"
    );
}

// ---------------------------------------------------------------------------
// TEST-09: FACTS clause adversarial input (Phase 29)
// ---------------------------------------------------------------------------

/// Generate a valid SQL identifier for use in FACTS entries.
fn arb_identifier() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9_]{0,9}")
        .unwrap()
        .prop_filter("must not be a SQL keyword or start with AS", |s| {
            let upper = s.to_ascii_uppercase();
            // Reject identifiers that ARE or START WITH common SQL keywords
            // that confuse the dot-qualified entry parser (e.g., "as_" looks
            // like "AS" + "_" to the token splitter).
            !matches!(
                upper.as_str(),
                "AS" | "BY"
                    | "ON"
                    | "IN"
                    | "IS"
                    | "OR"
                    | "TO"
                    | "IF"
                    | "DO"
                    | "REFERENCES"
                    | "PRIMARY"
                    | "KEY"
                    | "MANY"
                    | "ONE"
                    | "SUM"
                    | "COUNT"
                    | "AVG"
                    | "MIN"
                    | "MAX"
            ) && !upper.starts_with("AS_")
                && !upper.starts_with("AS ")
        })
}

/// Generate a simple SQL expression for fact entries.
fn arb_simple_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        // Simple column reference
        arb_identifier().prop_map(|id| id),
        // Binary expression
        (arb_identifier(), arb_identifier()).prop_map(|(a, b)| format!("{a} + {b}")),
        // Function call
        arb_identifier().prop_map(|id| format!("SUM({id})")),
        // Multiply with paren group
        (arb_identifier(), arb_identifier()).prop_map(|(a, b)| format!("{a} * (1 - {b})")),
    ]
}

/// Generate a FACTS clause: `FACTS (alias.name AS expr, ...)`
fn arb_facts_clause() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        (arb_identifier(), arb_identifier(), arb_simple_expr()),
        0..=3,
    )
    .prop_map(|entries| {
        if entries.is_empty() {
            String::new()
        } else {
            let items: Vec<String> = entries
                .iter()
                .map(|(alias, name, expr)| format!("{alias}.{name} AS {alias}.{expr}"))
                .collect();
            format!("FACTS ({})", items.join(", "))
        }
    })
}

proptest! {
    /// FACTS clause with adversarial input: parse_keyword_body either succeeds
    /// or returns a well-formed ParseError (no panics, no crashes).
    #[test]
    fn facts_clause_no_panic(
        facts_clause in arb_facts_clause(),
        name in arb_view_name(),
    ) {
        let query = format!(
            "CREATE SEMANTIC VIEW {name} AS TABLES (t AS orders PRIMARY KEY (id)) {facts_clause} DIMENSIONS (t.region AS region) METRICS (t.rev AS SUM(amount))"
        );
        let result = std::panic::catch_unwind(|| plan_create(&query));
        prop_assert!(
            result.is_ok(),
            "plan_create panicked on FACTS clause: {facts_clause}"
        );
        // The result is either Ok(Some(sql)) for valid DDL, Ok(None) for non-DDL, or Err(ParseError)
        // All are acceptable -- the key invariant is no panics.
    }

    /// Empty FACTS clause is valid (no entries).
    #[test]
    fn empty_facts_clause_valid(
        name in arb_view_name(),
    ) {
        let query = format!(
            "CREATE SEMANTIC VIEW {name} AS \
             TABLES (t AS orders PRIMARY KEY (id)) \
             FACTS () \
             DIMENSIONS (t.region AS region) \
             METRICS (t.rev AS SUM(amount))"
        );
        let result = plan_create(&query);
        prop_assert!(
            result.is_ok(),
            "Empty FACTS () should be accepted, got: {:?}",
            result
        );
        prop_assert!(
            result.unwrap().is_some(),
            "Empty FACTS should produce valid rewrite"
        );
    }
}

// ---------------------------------------------------------------------------
// TEST-10: Derived metric parsing and expression substitution (Phase 30)
// ---------------------------------------------------------------------------

/// Generate a valid metric name (no dots allowed for derived metrics).
fn arb_metric_name() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9_]{0,14}").unwrap()
}

proptest! {
    /// Derived metric entries in METRICS clause: parse_and_rewrite either succeeds
    /// or returns a well-formed error (no panics).
    #[test]
    fn derived_metric_parsing_no_panic(
        name in arb_view_name(),
        derived_name in arb_metric_name(),
        base_name in arb_metric_name(),
    ) {
        // Build a DDL with one base metric and one derived metric
        let query = format!(
            "CREATE SEMANTIC VIEW {name} AS \
             TABLES (t AS orders PRIMARY KEY (id)) \
             DIMENSIONS (t.region AS region) \
             METRICS (t.{base_name} AS SUM(t.amount), {derived_name} AS {base_name} + 1)"
        );
        let result = std::panic::catch_unwind(|| plan_create(&query));
        prop_assert!(
            result.is_ok(),
            "plan_create panicked on derived metric entry"
        );
    }

    /// Mixed qualified and unqualified metric entries: no panics.
    #[test]
    fn mixed_metrics_no_panic(
        name in arb_view_name(),
        alias in arb_identifier(),
        base1 in arb_metric_name(),
        base2 in arb_metric_name(),
        derived in arb_metric_name(),
    ) {
        let query = format!(
            "CREATE SEMANTIC VIEW {name} AS \
             TABLES ({alias} AS orders PRIMARY KEY (id)) \
             DIMENSIONS ({alias}.region AS region) \
             METRICS ({alias}.{base1} AS SUM({alias}.amount), \
                      {alias}.{base2} AS COUNT(*), \
                      {derived} AS {base1} + {base2})"
        );
        let result = std::panic::catch_unwind(|| plan_create(&query));
        prop_assert!(
            result.is_ok(),
            "plan_create panicked on mixed qualified/unqualified metrics"
        );
    }
}

// ---------------------------------------------------------------------------
// TEST-11: Word-boundary replacement safety (Phase 30)
// ---------------------------------------------------------------------------

use semantic_views::expand::quote_ident;

proptest! {
    /// replace_word_boundary (tested through expand infrastructure):
    /// quote_ident with random strings does not panic and produces valid output.
    #[test]
    fn quote_ident_never_panics(
        input in "[a-zA-Z0-9_\"]{0,50}",
    ) {
        let result = std::panic::catch_unwind(|| quote_ident(&input));
        prop_assert!(
            result.is_ok(),
            "quote_ident panicked on input: {input:?}"
        );
        let quoted = result.unwrap();
        prop_assert!(quoted.starts_with('"'), "Must start with double quote");
        prop_assert!(quoted.ends_with('"'), "Must end with double quote");
    }
}

// ---------------------------------------------------------------------------
// TEST-12: Relationship inference (Phase 33 -- replaces Phase 31 cardinality keyword tests)
// ---------------------------------------------------------------------------

proptest! {
    /// Relationship entries without cardinality keywords parse successfully
    /// and infer cardinality from PK/UNIQUE constraints (ManyToOne by default).
    #[test]
    fn relationship_no_cardinality_defaults(
        name in arb_view_name().prop_filter("avoid SQL keywords", |n| !matches!(n.as_str(), "as" | "primary" | "key" | "references" | "unique")),
        alias_from in arb_identifier(),
        alias_to in arb_identifier(),
        fk_col in arb_identifier(),
    ) {
        let input = format!(
            "{name} AS {alias_from}({fk_col}) REFERENCES {alias_to}"
        );
        let ddl = format!(
            "CREATE SEMANTIC VIEW v AS TABLES ({alias_from} AS orders PRIMARY KEY (id), {alias_to} AS customers PRIMARY KEY (id)) RELATIONSHIPS ({input}) DIMENSIONS ({alias_from}.r AS region) METRICS ({alias_from}.m AS SUM(amount))"
        );
        let result = plan_create(&ddl);
        prop_assert!(
            result.is_ok(),
            "Failed to parse relationship without cardinality: {:?}",
            result.unwrap_err()
        );
    }

    /// Metrics with USING clause containing random relationship names parse
    /// successfully through the full DDL pipeline and produce valid JSON.
    #[test]
    fn metric_using_clause_roundtrip(
        // Exclude names starting with "as" to avoid collision with the AS keyword parser
        rel_name in proptest::string::string_regex("[b-z][a-z0-9_]{1,15}").unwrap(),
        metric_name in proptest::string::string_regex("[b-z][a-z0-9_]{1,15}").unwrap(),
        using_kw in arb_case_variant("using"),
    ) {
        // Build DDL with a USING clause on a metric
        let ddl = format!(
            "CREATE SEMANTIC VIEW v AS \
             TABLES (f AS flights PRIMARY KEY (id), a AS airports PRIMARY KEY (id)) \
             RELATIONSHIPS ({rel_name} AS f(airport_id) REFERENCES a) \
             DIMENSIONS (a.name AS airport_name) \
             METRICS (f.{metric_name} {using_kw} ({rel_name}) AS COUNT(*))"
        );
        let result = plan_create(&ddl);
        prop_assert!(
            result.is_ok(),
            "Failed to parse metric with USING clause (rel={}, met={}, kw={}): {:?}",
            rel_name,
            metric_name,
            using_kw,
            result.unwrap_err()
        );
        let action = result.unwrap();
        prop_assert!(
            action.is_some(),
            "Expected Some(_) for valid DDL with USING"
        );
        // The serialized definition should carry the USING relationship data —
        // the same JSON payload the old string form embedded.
        match action.unwrap() {
            RewriteAction::Create { def, .. } => {
                let json = serde_json::to_string(def.as_ref()).unwrap();
                prop_assert!(
                    json.contains("using_relationships"),
                    "Serialized definition should contain using_relationships: {json}"
                );
            }
            other => prop_assert!(false, "Expected RewriteAction::Create, got: {:?}", other),
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 62 — `ParseError::position` MUST be a byte offset into the user's
// input string, NOT into the rewritten SQL. RESEARCH.md Q1 proves this is
// already true; these tests pin it as a contract across arbitrary leading
// whitespace + comments so a future refactor that breaks the invariant
// fails loudly.
// ---------------------------------------------------------------------------

/// Smoke test: position arithmetic for a clause-typo error.
///
/// `TABLSE` is the transposition typo proven to land position at the typo
/// byte offset (see `as_body_position_invariant_clause_typo` above). Whatever
/// prefix we stick before the canonical CREATE, the reported byte offset
/// must shift by exactly the prefix's byte length.
#[test]
fn parse_error_position_is_byte_offset_into_user_input_smoke() {
    let canonical = "CREATE SEMANTIC VIEW v AS TABLSE (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))";
    let bad_token_offset = canonical.find("TABLSE").unwrap();

    // Canonical case — no prefix.
    let err = plan_create(canonical).expect_err("should fail on TABLSE");
    let pos = err
        .position
        .expect("position should be set for clause typo");
    assert_eq!(pos, bad_token_offset, "smoke: canonical case");

    // Whitespace prefix shifts the position by its byte length.
    let prefixed = format!("   \n\t{canonical}");
    let prefix_len = prefixed.len() - canonical.len();
    let err2 = plan_create(&prefixed).expect_err("should fail with whitespace");
    let pos2 = err2.position.expect("position should be set");
    assert_eq!(
        pos2,
        prefix_len + bad_token_offset,
        "smoke: with whitespace prefix"
    );

    // Line-comment prefix.
    let with_comment = format!("-- a comment\n{canonical}");
    let comment_len = with_comment.len() - canonical.len();
    let err3 = plan_create(&with_comment).expect_err("should fail with comment");
    let pos3 = err3.position.expect("position should be set");
    assert_eq!(
        pos3,
        comment_len + bad_token_offset,
        "smoke: with line comment prefix"
    );
}

proptest! {
    /// Position is preserved as user-input byte offset for arbitrary
    /// safe ASCII whitespace + line-comment prefix.
    #[test]
    fn position_byte_offset_preserved_for_arbitrary_prefix(
        ws in proptest::collection::vec(prop_oneof![Just(' '), Just('\t'), Just('\n')], 0..32),
        line_comment in r"-- [a-zA-Z0-9 ]{0,40}\n",
    ) {
        let prefix: String = ws.into_iter().collect::<String>() + &line_comment;
        let canonical = "CREATE SEMANTIC VIEW v AS TABLSE (t AS orders PRIMARY KEY (id)) DIMENSIONS (t.r AS r) METRICS (t.m AS SUM(1))";
        let bad_offset = canonical.find("TABLSE").unwrap();
        let full = format!("{prefix}{canonical}");
        let prefix_len = prefix.len();
        let err = plan_create(&full).expect_err("must fail on TABLSE");
        let pos = err.position.expect("position should always be set for clause typo");
        prop_assert_eq!(pos, prefix_len + bad_offset);
    }
}
