use proptest::prelude::*;
use semantic_views::model::{
    AccessModifier, Cardinality, Dimension, Fact, Join, Materialization, Metric, NonAdditiveDim,
    NullsOrder, SemanticViewDefinition, SortOrder, TableRef, WindowOrderBy, WindowSpec,
};

// ---------------------------------------------------------------------------
// Proptest strategies for model types
// ---------------------------------------------------------------------------

/// Generate an arbitrary non-empty identifier. The alphabet includes quoted,
/// unicode, keyword, and whitespace-bearing arms (TC-3, code-review
/// 2026-07-02 — the previous [a-z][a-z0-9_]* alphabet systematically missed
/// the shapes behind the UTF-8 and quoting regressions; YAML must round-trip
/// them all as plain string scalars).
fn arb_name() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-z][a-z0-9_]{0,19}".boxed(),
        1 => "[A-Za-zéàçΩ東京☕][A-Za-zéàçΩ東京☕ _.-]{0,10}".boxed(),
        1 => "\"[a-zA-Z ,.()]{1,10}\"".boxed(),
        1 => prop::sample::select(vec![
            "SELECT".to_string(),
            "primary key".to_string(),
            "wéird name".to_string(),
        ]).boxed(),
        // YAML-hostile scalars: bare forms a naive serializer would emit
        // unquoted and re-read as null / bool / number / mapping / comment
        // rather than as the original string. A correct serializer must quote
        // them so they round-trip as plain string scalars; if any of these
        // breaks the round-trip it is a real serializer bug, not a test bug.
        2 => prop::sample::select(vec![
            "null", "~", "no", "on", "yes", "true", "false", "123", "1.5",
            "-0", "a: b", "x #y", " padded ", "line\nbreak",
        ]).prop_map(str::to_string).boxed(),
        1 => Just("has \"embedded\" quote".to_string()).boxed(),
    ]
}

/// Free-text payload for COMMENT / SYNONYMS fields. Reuses `arb_name`'s
/// alphabet (including the YAML-hostile scalars) so those optional fields
/// actually exercise the round-trip instead of being hardcoded empty.
fn arb_payload() -> impl Strategy<Value = String> {
    arb_name()
}

/// Generate an arbitrary SQL-like expression.
fn arb_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        arb_name(),
        arb_name().prop_map(|n| format!("SUM({n})")),
        arb_name().prop_map(|n| format!("COUNT({n})")),
        arb_name().prop_map(|n| format!("AVG({n})")),
        arb_name().prop_map(|n| format!("concat({n}, ' – ☕')")),
    ]
}

fn arb_sort_order() -> impl Strategy<Value = SortOrder> {
    prop_oneof![Just(SortOrder::Asc), Just(SortOrder::Desc),]
}

fn arb_nulls_order() -> impl Strategy<Value = NullsOrder> {
    prop_oneof![Just(NullsOrder::Last), Just(NullsOrder::First),]
}

fn arb_access() -> impl Strategy<Value = AccessModifier> {
    prop_oneof![Just(AccessModifier::Public), Just(AccessModifier::Private),]
}

fn arb_cardinality() -> impl Strategy<Value = Cardinality> {
    prop_oneof![Just(Cardinality::ManyToOne), Just(Cardinality::OneToOne),]
}

fn arb_table_ref() -> impl Strategy<Value = TableRef> {
    (
        arb_name(),
        arb_name(),
        proptest::collection::vec(arb_name(), 0..=2),
        proptest::collection::vec(proptest::collection::vec(arb_name(), 1..=2), 0..=2),
        proptest::option::of(arb_payload()),
        proptest::collection::vec(arb_payload(), 0..=2),
    )
        .prop_map(
            |(alias, table, pk_columns, unique_constraints, comment, synonyms)| TableRef {
                alias,
                table,
                pk_columns,
                unique_constraints,
                comment,
                synonyms,
            },
        )
}

fn arb_dimension() -> impl Strategy<Value = Dimension> {
    (
        arb_name(),
        arb_expr(),
        proptest::option::of(arb_name()),
        proptest::option::of(arb_payload()),
        proptest::collection::vec(arb_payload(), 0..=2),
        // VARIED, not defaulted: a generator that left `is_filter` false would
        // make any assertion on it vacuous (CLAUDE.md), so the YAML round-trip
        // would silently stop covering LABELS = (FILTER).
        proptest::bool::ANY,
    )
        .prop_map(
            |(name, expr, source_table, comment, synonyms, is_filter)| Dimension {
                name,
                expr,
                source_table,
                output_type: None,
                comment,
                synonyms,
                is_filter,
            },
        )
}

fn arb_non_additive_dim() -> impl Strategy<Value = NonAdditiveDim> {
    (arb_name(), arb_sort_order(), arb_nulls_order()).prop_map(|(dimension, order, nulls)| {
        NonAdditiveDim {
            dimension,
            order,
            nulls,
        }
    })
}

fn arb_window_order_by() -> impl Strategy<Value = WindowOrderBy> {
    (arb_name(), arb_sort_order(), arb_nulls_order())
        .prop_map(|(expr, order, nulls)| WindowOrderBy { expr, order, nulls })
}

fn arb_window_spec() -> impl Strategy<Value = WindowSpec> {
    (
        arb_name(),
        arb_name(),
        proptest::collection::vec(arb_name(), 0..=1),
        proptest::collection::vec(arb_window_order_by(), 0..=2),
        proptest::collection::vec(arb_name(), 0..=2),
        proptest::collection::vec(arb_name(), 0..=2),
        proptest::option::of(arb_payload()),
    )
        .prop_map(
            |(
                window_function,
                inner_metric,
                excluding_dims,
                order_by,
                extra_args,
                partition_dims,
                frame_clause,
            )| WindowSpec {
                window_function,
                inner_metric,
                extra_args,
                excluding_dims,
                partition_dims,
                order_by,
                frame_clause,
            },
        )
}

fn arb_metric() -> impl Strategy<Value = Metric> {
    (
        arb_name(),
        arb_expr(),
        proptest::option::of(arb_name()),
        arb_access(),
        proptest::collection::vec(arb_non_additive_dim(), 0..=1),
        proptest::option::of(arb_window_spec()),
        proptest::option::of(arb_payload()),
        proptest::collection::vec(arb_payload(), 0..=2),
    )
        .prop_map(
            |(
                name,
                expr,
                source_table,
                access,
                non_additive_by,
                window_spec,
                comment,
                synonyms,
            )| {
                Metric {
                    name,
                    expr,
                    source_table,
                    output_type: None,
                    using_relationships: vec![],
                    comment,
                    synonyms,
                    access,
                    non_additive_by,
                    window_spec,
                }
            },
        )
}

fn arb_fact() -> impl Strategy<Value = Fact> {
    (
        arb_name(),
        arb_expr(),
        proptest::option::of(arb_name()),
        arb_access(),
        proptest::option::of(arb_payload()),
        proptest::collection::vec(arb_payload(), 0..=2),
        // Varied independently of `access` — see `arb_dimension`.
        proptest::bool::ANY,
    )
        .prop_map(
            |(name, expr, source_table, access, comment, synonyms, is_filter)| Fact {
                name,
                expr,
                source_table,
                output_type: None,
                comment,
                synonyms,
                is_filter,
                access,
            },
        )
}

fn arb_join() -> impl Strategy<Value = Join> {
    (
        arb_name(),
        arb_name(),
        proptest::collection::vec(arb_name(), 0..=2),
        arb_cardinality(),
        proptest::collection::vec(arb_name(), 0..=2),
        proptest::option::of(arb_name()),
    )
        .prop_map(
            |(table, from_alias, fk_columns, cardinality, ref_columns, name)| Join {
                table,
                from_alias,
                fk_columns,
                ref_columns,
                name,
                cardinality,
            },
        )
}

fn arb_materialization() -> impl Strategy<Value = Materialization> {
    (
        arb_name(),
        arb_name(),
        proptest::collection::vec(arb_name(), 0..=3),
        proptest::collection::vec(arb_name(), 0..=3),
    )
        .prop_map(|(name, table, dimensions, metrics)| Materialization {
            name,
            table,
            dimensions,
            metrics,
        })
}

fn arb_definition() -> impl Strategy<Value = SemanticViewDefinition> {
    (
        proptest::collection::vec(arb_table_ref(), 0..=2),
        proptest::collection::vec(arb_dimension(), 1..=3),
        proptest::collection::vec(arb_metric(), 1..=3),
        proptest::collection::vec(arb_join(), 0..=2),
        proptest::collection::vec(arb_fact(), 0..=2),
        proptest::option::of("[a-z ]{1,30}"),
        proptest::collection::vec(arb_materialization(), 0..=2),
    )
        .prop_map(
            |(tables, dimensions, metrics, joins, facts, comment, materializations)| {
                SemanticViewDefinition {
                    tables,
                    dimensions,
                    metrics,
                    joins,
                    facts,
                    materializations,
                    created_on: None,
                    database_name: None,
                    schema_name: None,
                    resolution_schema_name: None,
                    temp_tables: vec![],
                    comment,
                }
            },
        )
}

// ---------------------------------------------------------------------------
// Property-based test
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn yaml_json_roundtrip_equivalence(def in arb_definition()) {
        // Serialize the arbitrary struct to both JSON and YAML
        let json_str = serde_json::to_string(&def).expect("JSON serialize");
        let yaml_str = yaml_serde::to_string(&def).expect("YAML serialize");

        // Deserialize both
        let from_json = SemanticViewDefinition::from_json("proptest", &json_str)
            .expect("JSON deserialize");
        let from_yaml = SemanticViewDefinition::from_yaml("proptest", &yaml_str)
            .expect("YAML deserialize");

        // Assert structural equality
        prop_assert_eq!(from_json, from_yaml);
    }

    #[test]
    fn materialization_json_roundtrip(mat in arb_materialization()) {
        let json_str = serde_json::to_string(&mat).expect("JSON serialize");
        let roundtripped: Materialization = serde_json::from_str(&json_str).expect("JSON deserialize");
        prop_assert_eq!(mat, roundtripped);
    }

    #[test]
    fn yaml_export_roundtrip(def in arb_definition()) {
        let yaml_str = semantic_views::render_yaml::render_yaml_export(&def)
            .expect("YAML export should succeed");
        let reimported = SemanticViewDefinition::from_yaml("proptest", &yaml_str)
            .expect("Re-import should succeed");

        // Strip internal fields from original for comparison
        let mut expected = def.clone();
        expected.created_on = None;
        expected.database_name = None;
        expected.schema_name = None;

        prop_assert_eq!(expected, reimported);
    }
}

// ===========================================================================
// The YAML → GET_DDL → FRONT DOOR → model oracle (code-review 2026-08-08 §6)
// ===========================================================================
//
// The three properties above are serde round-trips: YAML in, YAML out, never
// touching the DDL renderer. `roundtrip_proptest` covers the other half —
// render then parse — but it feeds `parse_keyword_body` DIRECTLY, so the front
// door's `blank_sql_comments` pre-pass is not in its loop, and it starts from a
// hand-built canonical definition rather than from YAML, so the YAML gate is
// not in its loop either. Nothing anywhere diffed a pre-render definition
// against a replayed one.
//
// That shape is exactly why RT-7 (`output_type` dropped by `GET_DDL`) and RT-8
// (a `--` in a member expression merging two members on replay) were invisible
// to the oracles while being trivially reachable from YAML. This property
// closes the loop the review describes:
//
//     YAML -> from_yaml_with_size_cap -> render_create_ddl
//          -> blank_sql_comments -> parse_keyword_body -> compare
//
// with the comment-blanking step included, because that is what production
// does (`parse::rewrite::plan_rewrite` blanks the whole statement before it
// looks at anything).

use semantic_views::body_parser::parse_keyword_body;
use semantic_views::expand::quote_ident;
use semantic_views::render_ddl::render_create_ddl;
use semantic_views::util::blank_sql_comments;

const VIEW: &str = "rt_view";

/// A small pool of DDL-safe bare identifiers. The oracle is about the
/// render→parse contract for the FIELDS the slot validators do not cover, so
/// the names are deliberately tame; hostile identifiers are `roundtrip_proptest`
/// and `create_front_door_proptest`'s axis.
fn arb_safe_name(prefix: &'static str) -> impl Strategy<Value = String> {
    (0usize..4).prop_map(move |i| format!("{prefix}{i}"))
}

/// Names for the DERIVED metric, which renders unqualified and therefore
/// entry-initial. `private` / `public` are the RT-9 axis: they lex as ordinary
/// identifiers but the METRICS entry parser peels them as the access modifier
/// before it looks for a name, so an unprotected emission replays as
/// "Missing metric name before 'AS'".
fn arb_derived_metric_name() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => Just("derived".to_string()),
        2 => Just("private".to_string()),
        1 => Just("PUBLIC".to_string()),
    ]
}

/// Expression fragments over a fixed alias. The `--`, `/* */` and `'--'` arms
/// are the RT-8 axis: the first two MUST be refused at import, the third MUST
/// be accepted (a comment marker inside a string literal is not a comment), and
/// the guard below asserts all three are actually generated.
fn arb_member_expr() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => Just("o.amount".to_string()),
        4 => Just("sum(o.amount)".to_string()),
        2 => Just("coalesce(o.amount, 0) || ')'".to_string()),
        2 => Just("o.amount || '--'".to_string()),
        2 => Just("o.amount -- trailing".to_string()),
        1 => Just("o.amount /* inline */ + 1".to_string()),
    ]
}

/// `output_type` is VARIED, not pinned. `yaml_proptest.rs` pinned it at `None`
/// in all three member generators (PBT-11), which is precisely why the field
/// could be silently dropped by `GET_DDL` for the whole of its life without a
/// single test noticing. RT-7's resolution is that it is refused at import, and
/// this generator is what keeps that assertion honest — the property asserts
/// the outcome either way rather than assuming which one applies.
fn arb_output_type() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        6 => Just(None),
        2 => Just(Some("VARCHAR".to_string())),
        1 => Just(Some("DECIMAL(10, 2)".to_string())),
        1 => Just(Some(String::new())),
    ]
}

/// A window spec over the declared members. Every field is varied — including
/// `frame_clause`, `extra_args` and `order_by`, which `emit_window_expr` writes
/// out RAW and which MODEL-1 showed could inject an entire extra metric.
fn arb_oracle_window_spec(dim: String, metric: String) -> impl Strategy<Value = WindowSpec> {
    (
        prop_oneof![
            4 => Just("AVG".to_string()),
            2 => Just("LAG".to_string()),
            1 => Just("1 + AVG".to_string()),
        ],
        prop_oneof![
            Just(Vec::new()),
            Just(vec!["1".to_string()]),
            Just(vec!["1, 2".to_string()])
        ],
        prop::bool::ANY,
        prop::bool::ANY,
        prop_oneof![
            4 => Just(None),
            2 => Just(Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW".to_string())),
            1 => Just(Some("ROWS UNBOUNDED PRECEDING -- x".to_string())),
            1 => Just(Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) , junk AS (".to_string())),
        ],
    )
        .prop_map(
            move |(window_function, extra_args, partition, order, frame_clause)| WindowSpec {
                window_function,
                inner_metric: metric.clone(),
                extra_args,
                excluding_dims: Vec::new(),
                partition_dims: if partition {
                    vec![dim.clone()]
                } else {
                    Vec::new()
                },
                order_by: if order {
                    vec![WindowOrderBy {
                        expr: dim.clone(),
                        order: SortOrder::Desc,
                        nulls: NullsOrder::First,
                    }]
                } else {
                    Vec::new()
                },
                frame_clause,
            },
        )
}

/// A definition anchored at one table `o`, with one dimension, one base metric
/// and an optional window metric over them, plus an optional materialization.
/// Shapes that are supposed to be REFUSED are generated on purpose — the
/// property's contract covers both outcomes.
fn arb_oracle_def() -> impl Strategy<Value = SemanticViewDefinition> {
    (
        arb_safe_name("d"),
        arb_safe_name("m"),
        arb_member_expr(),
        arb_member_expr(),
        arb_output_type(),
        arb_output_type(),
        prop::bool::ANY,
        prop::bool::ANY,
        arb_access(),
        arb_derived_metric_name(),
    )
        .prop_flat_map(
            |(
                dim,
                met,
                dim_expr,
                met_expr,
                dim_type,
                met_type,
                want_window,
                want_mat,
                access,
                derived,
            )| {
                let ws = arb_oracle_window_spec(dim.clone(), met.clone());
                (
                    Just((
                        dim,
                        met,
                        dim_expr,
                        met_expr,
                        dim_type,
                        met_type,
                        want_window,
                        want_mat,
                        access,
                        derived,
                    )),
                    ws,
                )
            },
        )
        .prop_map(
            |(
                (
                    dim,
                    met,
                    dim_expr,
                    met_expr,
                    dim_type,
                    met_type,
                    want_window,
                    want_mat,
                    access,
                    derived,
                ),
                ws,
            )| {
                let mut metrics = vec![Metric {
                    name: met.clone(),
                    expr: met_expr,
                    source_table: Some("o".to_string()),
                    output_type: met_type,
                    access,
                    ..Default::default()
                }];
                if want_window {
                    metrics.push(Metric {
                        name: format!("w_{met}"),
                        expr: "placeholder".to_string(),
                        source_table: Some("o".to_string()),
                        window_spec: Some(ws),
                        ..Default::default()
                    });
                }
                // A DERIVED metric: unqualified, so its name is the first token
                // of its entry — the RT-9 shape.
                metrics.push(Metric {
                    name: derived,
                    expr: format!("{met} * 2"),
                    source_table: None,
                    ..Default::default()
                });
                let materializations = if want_mat {
                    vec![Materialization {
                        name: "mat".to_string(),
                        table: "mt".to_string(),
                        dimensions: vec![dim.clone()],
                        metrics: vec![met],
                    }]
                } else {
                    Vec::new()
                };
                SemanticViewDefinition {
                    tables: vec![TableRef {
                        alias: "o".to_string(),
                        table: "orders".to_string(),
                        pk_columns: vec!["id".to_string()],
                        ..Default::default()
                    }],
                    dimensions: vec![Dimension {
                        name: dim,
                        expr: dim_expr,
                        source_table: Some("o".to_string()),
                        output_type: dim_type,
                        ..Default::default()
                    }],
                    metrics,
                    materializations,
                    ..Default::default()
                }
            },
        )
}

/// A replayed member name may be the stored one VERBATIM, or exactly its
/// quote-protected spelling — and nothing else.
///
/// Emission quote-protection is the RT-5/RT-6 design, not drift: `emit_alias`
/// wraps any stored value that would not re-tokenize as one identifier, and
/// RT-9 adds the one collision lexing cannot see (an entry-initial `private`
/// is peeled as the access modifier). The parser stores a quoted identifier
/// WITH its quotes, so the first render of a non-canonical name lands on the
/// quoted spelling and every render after that is a fixpoint — which the
/// property asserts separately. Allowing exactly the two spellings, rather
/// than comparing case-folded logical identity, keeps the check tight: a name
/// that came back as anything else is still a failure.
fn names_agree(replayed: &str, stored: &str) -> bool {
    replayed == stored || replayed == quote_ident(stored)
}

/// Replay a rendered `CREATE` exactly as the front door does: blank comments
/// over the whole statement FIRST, then hand the body to the clause parser.
fn front_door_replay(ddl: &str) -> Result<SemanticViewDefinition, String> {
    let blanked = blank_sql_comments(ddl);
    let body = blanked
        .strip_prefix(&format!("CREATE OR REPLACE SEMANTIC VIEW {VIEW} ")[..])
        .ok_or_else(|| format!("rendered header shape changed:\n{ddl}"))?;
    let kb = parse_keyword_body(body, 0).map_err(|e| e.message)?;
    Ok(SemanticViewDefinition {
        tables: kb.tables,
        joins: kb.relationships,
        facts: kb.facts,
        dimensions: kb.dimensions,
        metrics: kb.metrics,
        materializations: kb.materializations,
        ..Default::default()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// The contract `validate_ddl_representable` exists to hold: a definition
    /// YAML accepts renders `GET_DDL` that the FRONT DOOR reads back as the
    /// same model. Either the import is refused, or the round trip is exact.
    #[test]
    fn a_yaml_importable_definition_survives_get_ddl_through_the_front_door(
        def in arb_oracle_def()
    ) {
        let yaml = yaml_serde::to_string(&def).expect("YAML serialize");
        let Ok(imported) = SemanticViewDefinition::from_yaml_with_size_cap(VIEW, &yaml) else {
            // Refused at the choke point — the contract holds vacuously for
            // this case, and the guards below prove refusals are not the only
            // thing this generator produces.
            return Ok(());
        };

        let ddl = render_create_ddl(VIEW, &imported)
            .expect("an importable definition must render");
        let replayed = front_door_replay(&ddl).map_err(|e| TestCaseError::fail(
            format!("YAML-importable definition rendered DDL the front door rejects: {e}\n{ddl}")
        ))?;

        prop_assert_eq!(&replayed.tables, &imported.tables, "tables drift\n{}", ddl);
        prop_assert_eq!(&replayed.joins, &imported.joins, "relationships drift\n{}", ddl);
        prop_assert_eq!(&replayed.facts, &imported.facts, "facts drift\n{}", ddl);
        prop_assert_eq!(
            &replayed.dimensions, &imported.dimensions, "dimensions drift\n{}", ddl
        );
        prop_assert_eq!(
            &replayed.materializations,
            &imported.materializations,
            "materializations drift\n{}",
            ddl
        );

        // Metrics are compared field-by-field with ONE documented exemption: a
        // window metric's `expr` is REBUILT from its `WindowSpec` at render
        // time (`emit_metrics`), so the replayed text is the canonical spelling
        // rather than whatever was stored — the same reason `roundtrip_proptest`
        // leaves window metrics to the fixpoint fuzz target. `window_spec`
        // itself, which is what drives expansion, is compared exactly, and the
        // render fixpoint below closes the text side.
        prop_assert_eq!(replayed.metrics.len(), imported.metrics.len(), "metric count drift\n{}", ddl);
        for (r, i) in replayed.metrics.iter().zip(&imported.metrics) {
            prop_assert!(
                names_agree(&r.name, &i.name),
                "metric name drift: {:?} -> {:?}\n{}",
                i.name,
                r.name,
                ddl
            );
            prop_assert_eq!(&r.source_table, &i.source_table, "metric table drift\n{}", ddl);
            prop_assert_eq!(&r.access, &i.access, "metric access drift\n{}", ddl);
            prop_assert_eq!(&r.window_spec, &i.window_spec, "window spec drift\n{}", ddl);
            prop_assert_eq!(
                &r.non_additive_by, &i.non_additive_by, "NON ADDITIVE BY drift\n{}", ddl
            );
            prop_assert_eq!(&r.output_type, &i.output_type, "output_type drift\n{}", ddl);
            if i.window_spec.is_none() {
                prop_assert_eq!(&r.expr, &i.expr, "metric expression drift\n{}", ddl);
            }
        }

        // Render fixpoint: whatever the parser gave back must render to the
        // SAME bytes. This is what pins the window-metric expression text that
        // the exemption above leaves out of the field comparison.
        let rerendered = render_create_ddl(VIEW, &replayed)
            .expect("a parser-produced definition must render");
        prop_assert_eq!(&rerendered, &ddl, "render is not a fixpoint");
    }
}

// --- Anti-vacuity guards, one per axis this generator added -----------------
//
// Each of these would have been the difference between a property that tests
// the axis and a property that merely mentions it (CLAUDE.md: "the field being
// PRESENT in a struct literal is not coverage").

fn sample_defs(n: usize) -> Vec<SemanticViewDefinition> {
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    (0..n)
        .map(|_| arb_oracle_def().new_tree(&mut runner).unwrap().current())
        .collect()
}

#[test]
fn generator_varies_output_type_and_window_specs() {
    let defs = sample_defs(512);
    assert!(
        defs.iter().any(|d| d.dimensions[0].output_type.is_some())
            && defs.iter().any(|d| d.metrics[0].output_type.is_some()),
        "output_type never left its inert default — the RT-7 axis is unreached \
         (this is the pin PBT-11 recorded and RT-7 was hiding behind)"
    );
    assert!(
        defs.iter()
            .any(|d| d.metrics.iter().any(|m| m.window_spec.is_some())),
        "no window metric was ever generated"
    );
    assert!(
        defs.iter().any(|d| !d.materializations.is_empty()),
        "no materialization was ever generated"
    );
    assert!(
        defs.iter().any(|d| d
            .metrics
            .iter()
            .any(|m| m.source_table.is_none() && m.name.eq_ignore_ascii_case("private"))),
        "no derived metric was ever named `private` — the RT-9 axis is unreached, \
         and it is only reachable through an UNQUALIFIED metric (a qualifier makes \
         the access-modifier peel decline)"
    );
    assert!(
        defs.iter().any(|d| d
            .metrics
            .iter()
            .any(|m| m.source_table.is_none() && m.name.eq_ignore_ascii_case("public"))),
        "no derived metric was ever named `public` — RT-9's twin keyword is unreached"
    );
}

#[test]
fn generator_varies_every_raw_window_slot() {
    let defs = sample_defs(512);
    let specs: Vec<&WindowSpec> = defs
        .iter()
        .flat_map(|d| d.metrics.iter().filter_map(|m| m.window_spec.as_ref()))
        .collect();
    assert!(!specs.is_empty(), "no window specs sampled");
    for (label, hit) in [
        (
            "frame_clause",
            specs.iter().any(|w| w.frame_clause.is_some()),
        ),
        ("extra_args", specs.iter().any(|w| !w.extra_args.is_empty())),
        (
            "partition_dims",
            specs.iter().any(|w| !w.partition_dims.is_empty()),
        ),
        ("order_by", specs.iter().any(|w| !w.order_by.is_empty())),
        (
            "a frame clause that escapes its parens",
            specs.iter().any(|w| {
                w.frame_clause.as_deref()
                    == Some("ROWS BETWEEN 1 PRECEDING AND CURRENT ROW) , junk AS (")
            }),
        ),
        (
            "a non-identifier window function",
            specs.iter().any(|w| w.window_function == "1 + AVG"),
        ),
    ] {
        assert!(hit, "the generator never varied {label}");
    }
}

#[test]
fn generator_varies_comment_markers_in_expressions() {
    let defs = sample_defs(512);
    let exprs: Vec<&str> = defs
        .iter()
        .flat_map(|d| {
            d.dimensions
                .iter()
                .map(|m| m.expr.as_str())
                .chain(d.metrics.iter().map(|m| m.expr.as_str()))
        })
        .collect();
    assert!(
        exprs.iter().any(|e| e.contains("--") && !e.contains('\'')),
        "no expression carried a bare line-comment marker — the RT-8 axis is unreached"
    );
    assert!(
        exprs.iter().any(|e| e.contains("/*")),
        "no expression carried a block-comment marker"
    );
    assert!(
        exprs.iter().any(|e| e.contains("'--'")),
        "no expression carried a comment marker inside a string literal — without \
         this arm the property cannot tell a correct rule from one that refuses \
         every `--`"
    );
}

#[test]
fn the_oracle_is_not_all_refusals() {
    // The property returns early when import is refused, so a generator that
    // only produced refusable definitions would make it vacuous.
    let defs = sample_defs(512);
    let accepted = defs
        .iter()
        .filter(|d| {
            let yaml = yaml_serde::to_string(d).expect("YAML serialize");
            SemanticViewDefinition::from_yaml_with_size_cap(VIEW, &yaml).is_ok()
        })
        .count();
    assert!(
        accepted > defs.len() / 10,
        "only {accepted}/{} sampled definitions were importable — the round-trip \
         half of the property almost never runs",
        defs.len()
    );
}

// ===========================================================================
// RT-7 — why `output_type` is REFUSED rather than rendered as a CAST
// ===========================================================================

/// Measured, not assumed. The other candidate fix for RT-7 was to render
/// `output_type` into the emitted DDL as `CAST(expr AS T)`, so the field would
/// survive `GET_DDL` in effect if not in name. This test is the evidence that
/// it would silently change what OTHER members compute:
/// `expand::facts::inline_facts` splices a fact's RAW `expr` into every
/// expression that references it and never applies the cast, so moving the cast
/// into `expr` moves it into all of those referrers too.
///
/// The two expansions below differ, and that difference is the whole argument:
/// same declared semantics for the fact, different SQL for the metric that
/// references it. (It also would not have restored the field — a replayed
/// definition would still carry `output_type: None` — so the contract would
/// have stayed false as well as the numbers changing.)
#[test]
fn output_type_is_not_applied_when_a_member_is_inlined() {
    use semantic_views::expand::{expand, MetricName, QueryRequest};

    fn def_with(fact_expr: &str, fact_type: Option<&str>) -> SemanticViewDefinition {
        SemanticViewDefinition {
            tables: vec![TableRef {
                alias: "o".to_string(),
                table: "orders".to_string(),
                pk_columns: vec!["id".to_string()],
                ..Default::default()
            }],
            facts: vec![Fact {
                name: "qty".to_string(),
                expr: fact_expr.to_string(),
                source_table: Some("o".to_string()),
                output_type: fact_type.map(str::to_string),
                ..Default::default()
            }],
            dimensions: vec![Dimension {
                name: "region".to_string(),
                expr: "o.region".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            metrics: vec![Metric {
                name: "total".to_string(),
                expr: "SUM(qty)".to_string(),
                source_table: Some("o".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    let req = QueryRequest {
        metrics: vec![MetricName::new("total".to_string())],
        ..Default::default()
    };

    // (a) the field as it exists today: the cast is declared on the fact...
    let declared = expand("v", &def_with("o.qty", Some("DOUBLE")), &req).expect("expands");
    // ...and does NOT reach the metric that references the fact.
    assert!(
        declared.contains("SUM((o.qty))"),
        "a referencing metric inlines the fact's RAW expression: {declared}"
    );
    assert!(
        !declared.contains("CAST"),
        "output_type on a fact must not appear in a referencing metric today — if \
         it does, this test's premise has changed and RT-7's decision needs \
         revisiting: {declared}"
    );

    // (b) the rejected alternative: the same cast written into the expression,
    // which is what rendering `CAST(expr AS T)` into the DDL would produce on
    // replay. The metric now aggregates a different expression.
    let as_cast = expand("v", &def_with("CAST(o.qty AS DOUBLE)", None), &req).expect("expands");
    assert!(
        as_cast.contains("CAST(o.qty AS DOUBLE)"),
        "the cast written into the expression reaches the referrer: {as_cast}"
    );
    assert_ne!(
        declared, as_cast,
        "if these were identical, rendering output_type as a CAST would be \
         semantics-preserving and RT-7 could have taken option (b)"
    );
}
