use proptest::prelude::*;
use semantic_views::model::{
    AccessModifier, Cardinality, CortexSearchService, CustomInstructions, Dimension, Fact, Join,
    Materialization, MaxStaleness, Metric, NonAdditiveDim, NullsOrder, SemanticViewDefinition,
    SortOrder, TableRef, Tag, TagName, VerifiedQuery, WindowOrderBy, WindowSpec,
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

/// Tags vary on every object: the qualifiers independently present or absent,
/// payload-shaped names and values (quotes, unicode, punctuation).
fn arb_tags() -> impl Strategy<Value = Vec<Tag>> {
    proptest::collection::vec(
        (
            proptest::option::of(arb_payload()),
            proptest::option::of(arb_payload()),
            arb_payload(),
            arb_payload(),
        )
            .prop_map(|(database, schema, tag, value)| Tag {
                name: TagName {
                    database,
                    schema,
                    tag,
                },
                value,
            }),
        0..=2,
    )
}

/// A Cortex Search service; any text in every part (the serde round trip does
/// not validate).
fn arb_cortex_search_service() -> impl Strategy<Value = CortexSearchService> {
    (
        arb_payload(),
        proptest::option::of(arb_payload()),
        proptest::option::of(arb_payload()),
        proptest::option::of(arb_payload()),
    )
        .prop_map(
            |(service, literal_column, database, schema)| CortexSearchService {
                service,
                literal_column,
                database,
                schema,
            },
        )
}

/// Both forms of `MAX_STALENESS`. An interval is any text, including the
/// YAML-hostile scalars (`123`, `true`, `null`) that must come back as text,
/// not as the seconds form or a missing value.
fn arb_max_staleness() -> impl Strategy<Value = MaxStaleness> {
    prop_oneof![
        proptest::num::u64::ANY.prop_map(MaxStaleness::Seconds),
        arb_payload().prop_map(MaxStaleness::Interval),
    ]
}

fn arb_verified_query() -> impl Strategy<Value = VerifiedQuery> {
    (
        arb_name(),
        arb_payload(),
        proptest::option::of(proptest::num::u64::ANY),
        proptest::option::of(arb_payload()),
        proptest::bool::ANY,
        arb_payload(),
    )
        .prop_map(
            |(name, question, verified_at, verified_by, onboarding, sql)| VerifiedQuery {
                name,
                question,
                verified_at,
                verified_by,
                use_as_onboarding_question: onboarding,
                sql,
            },
        )
}

fn arb_table_ref() -> impl Strategy<Value = TableRef> {
    (
        arb_name(),
        arb_name(),
        proptest::collection::vec(arb_name(), 0..=2),
        proptest::collection::vec(proptest::collection::vec(arb_name(), 1..=2), 0..=2),
        proptest::option::of(arb_payload()),
        proptest::collection::vec(arb_payload(), 0..=2),
        arb_tags(),
        proptest::option::of(prop_oneof![
            Just("SELECT * FROM orders".to_string()),
            Just("SELECT id, ' – ☕' AS note\nFROM orders\nWHERE amount > 0".to_string()),
        ]),
    )
        .prop_map(
            |(
                alias,
                table,
                pk_columns,
                unique_constraints,
                comment,
                synonyms,
                tags,
                definition,
            )| {
                TableRef {
                    // An inline-query table (Phase 6.4) has no table name.
                    table: if definition.is_some() {
                        String::new()
                    } else {
                        table
                    },
                    definition,
                    alias,
                    pk_columns,
                    unique_constraints,
                    comment,
                    synonyms,
                    tags,
                }
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
        arb_tags(),
        proptest::collection::vec(arb_payload(), 0..=2),
        proptest::bool::ANY,
        proptest::option::of(arb_cortex_search_service()),
    )
        .prop_map(
            |(
                name,
                expr,
                source_table,
                comment,
                synonyms,
                is_filter,
                tags,
                sample_values,
                is_enum,
                cortex_search_service,
            )| Dimension {
                name,
                expr,
                source_table,
                output_type: None,
                comment,
                synonyms,
                tags,
                is_filter,
                cortex_search_service,
                is_enum,
                sample_values,
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
        arb_tags(),
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
                tags,
            )| {
                Metric {
                    name,
                    expr,
                    source_table,
                    output_type: None,
                    using_relationships: vec![],
                    comment,
                    synonyms,
                    tags,
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
        arb_tags(),
        proptest::collection::vec(arb_payload(), 0..=2),
    )
        .prop_map(
            |(
                name,
                expr,
                source_table,
                access,
                comment,
                synonyms,
                is_filter,
                tags,
                sample_values,
            )| Fact {
                name,
                expr,
                source_table,
                output_type: None,
                comment,
                synonyms,
                tags,
                is_filter,
                access,
                sample_values,
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
        arb_tags(),
        proptest::option::of(arb_max_staleness()),
        (
            proptest::option::of(arb_payload()),
            proptest::option::of(arb_payload()),
        ),
        proptest::collection::vec(arb_verified_query(), 0..=2),
    )
        .prop_map(
            |(
                tables,
                dimensions,
                metrics,
                joins,
                facts,
                comment,
                materializations,
                tags,
                max_staleness,
                (sql_generation, question_categorization),
                verified_queries,
            )| {
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
                    tags,
                    max_staleness,
                    module_custom_instructions: CustomInstructions {
                        sql_generation,
                        question_categorization,
                    },
                    verified_queries,
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
// does (`parse::rewrite::plan_create` blanks the whole statement before it
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
///
/// Every `Some` is refused, on each of two members, so its weight sets how
/// often the round-trip half runs at all (`the_oracle_is_not_all_refusals`).
fn arb_output_type() -> impl Strategy<Value = Option<String>> {
    prop_oneof![
        12 => Just(None),
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

/// One tag-name part: plain spellings, and the characters a renderer has to
/// protect (comment markers, quotes, list punctuation, keywords) — plus the
/// empty part, which must be REFUSED at import.
fn arb_oracle_tag_part() -> impl Strategy<Value = String> {
    prop_oneof![
        8 => Just("s_layer_kind".to_string()),
        4 => Just("S_LAYER_SPEC".to_string()),
        1 => Just("a--b".to_string()),
        1 => Just("x/*y".to_string()),
        1 => Just("we\"ird".to_string()),
        1 => Just("a = 'b', c".to_string()),
        1 => Just("tag".to_string()),
        1 => Just("select".to_string()),
        1 => Just("with".to_string()),
        1 => Just("Ünï code".to_string()),
        1 => Just(String::new()),
    ]
}

/// Tags for the oracle: the TAG axis of the YAML → `GET_DDL` → front door
/// contract. Refused shapes are generated on purpose (an empty part, a database
/// with no schema, a value one character over the limit, a case-folded
/// duplicate) next to ones that must survive, at rates that leave most
/// definitions importable (`the_oracle_is_not_all_refusals`).
fn arb_oracle_tags() -> impl Strategy<Value = Vec<Tag>> {
    let value = prop_oneof![
        8 => Just("dimension".to_string()),
        2 => Just("it's -- not /* a */ comment, ) TAG (x = 'y')".to_string()),
        1 => Just(String::new()),
        1 => Just("é".repeat(256)),
        1 => Just("v".repeat(257)),
    ];
    let p = arb_oracle_tag_part;
    let name = prop_oneof![
        6 => p().prop_map(|tag| TagName { database: None, schema: None, tag }),
        2 => (p(), p()).prop_map(|(schema, tag)| TagName { database: None, schema: Some(schema), tag }),
        1 => (p(), p(), p()).prop_map(|(database, schema, tag)| TagName {
            database: Some(database),
            schema: Some(schema),
            tag,
        }),
        1 => (p(), p()).prop_map(|(database, tag)| TagName { database: Some(database), schema: None, tag }),
    ];
    let tag = (name, value).prop_map(|(name, value)| Tag { name, value });
    (
        prop_oneof![1 => Just(Vec::new()), 1 => prop::collection::vec(tag, 1..=2)],
        prop::bool::weighted(0.05),
    )
        .prop_map(|(mut tags, dup)| {
            // A case-folded repeat of the first name.
            if dup {
                if let Some(first) = tags.first().cloned() {
                    let mut again = first;
                    again.name.tag = again.name.tag.to_ascii_uppercase();
                    tags.push(again);
                }
            }
            tags
        })
}

/// Text for the metadata clauses' quoted slots: plain text, and what a
/// renderer has to protect inside a single-quoted literal (a quote, comment
/// markers, list punctuation, a clause keyword, a line break).
fn arb_oracle_text() -> impl Strategy<Value = String> {
    prop_oneof![
        6 => Just("East".to_string()),
        2 => Just("it's -- not /* a */ comment, ) SAMPLE_VALUES ('x') IS_ENUM".to_string()),
        1 => Just(String::new()),
        1 => Just("line\nbreak".to_string()),
        1 => Just("Ünï code".to_string()),
    ]
}

/// A Cortex Search service on the dimension, of a shape YAML accepts. The
/// refused shapes come from [`arb_oracle_metadata_refusal`].
fn arb_oracle_cortex() -> impl Strategy<Value = CortexSearchService> {
    let p = || arb_oracle_tag_part().prop_filter("a non-empty part", |p| !p.is_empty());
    let name = prop_oneof![
        3 => p().prop_map(|service| (None, None, service)),
        2 => (p(), p()).prop_map(|(schema, service)| (None, Some(schema), service)),
        3 => (p(), p(), p()).prop_map(|(database, schema, service)| {
            (Some(database), Some(schema), service)
        }),
    ];
    (name, prop::option::of(p())).prop_map(|((database, schema, service), literal_column)| {
        CortexSearchService {
            service,
            literal_column,
            database,
            schema,
        }
    })
}

/// A `MAX_STALENESS` YAML accepts, in both forms.
fn arb_oracle_staleness() -> impl Strategy<Value = Option<MaxStaleness>> {
    prop_oneof![
        3 => Just(None),
        2 => Just(Some(MaxStaleness::Seconds(3600))),
        1 => Just(Some(MaxStaleness::Seconds(120))),
        2 => Just(Some(MaxStaleness::Interval("1 hour".to_string()))),
        1 => Just(Some(MaxStaleness::Interval("90 Minutes".to_string()))),
    ]
}

/// Verified queries YAML accepts: bare, quoted and keyword names, quoted ones
/// holding list punctuation or the `AS` keyword (kept apart by an index
/// suffix), and hostile text in every quoted slot.
fn arb_oracle_verified_queries() -> impl Strategy<Value = Vec<VerifiedQuery>> {
    let name = prop::sample::select(vec![
        "q",
        "\"Top Q\"",
        "select",
        "\"a,b\"",
        "\"q)\"",
        "\"AS\"",
    ]);
    let query = (
        name,
        arb_oracle_text(),
        prop::option::of(prop_oneof![
            Just(0_u64),
            Just(1_772_645_863),
            Just(u64::MAX)
        ]),
        prop::option::of(arb_oracle_text()),
        prop::bool::ANY,
        arb_oracle_text(),
    )
        .prop_map(
            |(name, question, verified_at, verified_by, onboarding, sql)| VerifiedQuery {
                name: name.to_string(),
                question,
                verified_at,
                verified_by,
                use_as_onboarding_question: onboarding,
                sql,
            },
        );
    prop_oneof![
        1 => Just(Vec::new()),
        2 => prop::collection::vec(query, 1..=2),
    ]
    .prop_map(|queries| {
        queries
            .into_iter()
            .enumerate()
            .map(|(i, mut q)| {
                if i > 0 {
                    q.name = match q.name.strip_suffix('"') {
                        Some(stripped) => format!("{stripped}{i}\""),
                        None => format!("{}{i}", q.name),
                    };
                }
                q
            })
            .collect()
    })
}

/// One metadata shape YAML must refuse, applied to a definition now and then.
/// Drawn as a single rare mutation rather than as arms of the generators
/// above, so the refusals stay a fixed small share of the sample and do not
/// starve the round-trip half (`the_oracle_is_not_all_refusals`).
#[derive(Debug, Clone, Copy)]
enum MetadataRefusal {
    ShortStaleness,
    StalenessNotAnInterval,
    DuplicateVerifiedQuery,
    DuplicateVerifiedQueryByQuotes,
    VerifiedQueryNameOfTwoWords,
    VerifiedQueryNameWithCommentMarker,
    VerifiedQueryNameWithComma,
    VerifiedQueryNameWithParen,
    VerifiedQueryNamedAs,
    QualifiedVerifiedQueryName,
    CortexDatabaseWithoutSchema,
    CortexEmptyPart,
    CortexEmptyColumn,
}

fn arb_oracle_metadata_refusal() -> impl Strategy<Value = Option<MetadataRefusal>> {
    use MetadataRefusal as R;
    prop_oneof![
        19 => Just(None),
        1 => prop::sample::select(vec![
            R::ShortStaleness,
            R::StalenessNotAnInterval,
            R::DuplicateVerifiedQuery,
            R::DuplicateVerifiedQueryByQuotes,
            R::VerifiedQueryNameOfTwoWords,
            R::VerifiedQueryNameWithCommentMarker,
            R::VerifiedQueryNameWithComma,
            R::VerifiedQueryNameWithParen,
            R::VerifiedQueryNamedAs,
            R::QualifiedVerifiedQueryName,
            R::CortexDatabaseWithoutSchema,
            R::CortexEmptyPart,
            R::CortexEmptyColumn,
        ])
        .prop_map(Some),
    ]
}

fn apply_metadata_refusal(def: &mut SemanticViewDefinition, refusal: MetadataRefusal) {
    use MetadataRefusal as R;
    let query = |name: &str| VerifiedQuery {
        name: name.to_string(),
        question: "q".to_string(),
        sql: "SELECT 1".to_string(),
        ..Default::default()
    };
    let cortex = |database: Option<&str>, schema: Option<&str>, service: &str, column: &str| {
        CortexSearchService {
            service: service.to_string(),
            literal_column: Some(column.to_string()),
            database: database.map(str::to_string),
            schema: schema.map(str::to_string),
        }
    };
    match refusal {
        R::ShortStaleness => def.max_staleness = Some(MaxStaleness::Seconds(119)),
        R::StalenessNotAnInterval => {
            def.max_staleness = Some(MaxStaleness::Interval("1 fortnight".to_string()));
        }
        R::DuplicateVerifiedQuery => def.verified_queries = vec![query("vq"), query("VQ")],
        R::DuplicateVerifiedQueryByQuotes => {
            def.verified_queries = vec![query("vq"), query("\"VQ\"")];
        }
        R::VerifiedQueryNameWithComma => def.verified_queries = vec![query("a,b")],
        R::VerifiedQueryNameWithParen => def.verified_queries = vec![query("q)")],
        R::VerifiedQueryNamedAs => def.verified_queries = vec![query("as")],
        R::QualifiedVerifiedQueryName => def.verified_queries = vec![query("db.q")],
        R::VerifiedQueryNameOfTwoWords => def.verified_queries = vec![query("two words")],
        R::VerifiedQueryNameWithCommentMarker => def.verified_queries = vec![query("a--b")],
        R::CortexDatabaseWithoutSchema => {
            def.dimensions[0].cortex_search_service = Some(cortex(Some("db"), None, "s", "c"));
        }
        R::CortexEmptyPart => {
            def.dimensions[0].cortex_search_service = Some(cortex(None, Some(""), "s", "c"));
        }
        R::CortexEmptyColumn => {
            def.dimensions[0].cortex_search_service = Some(cortex(None, None, "s", ""));
        }
    }
}

/// [`arb_oracle_def_untagged`], with tags on the view and its dimension, and
/// the metadata clauses: sample values, `IS_ENUM` and a Cortex Search service on
/// the dimension, and `MAX_STALENESS`, custom instructions and verified queries
/// on the view; now and then one metadata shape YAML must refuse.
fn arb_oracle_def() -> impl Strategy<Value = SemanticViewDefinition> {
    (
        arb_oracle_def_untagged(),
        arb_oracle_tags(),
        arb_oracle_tags(),
        prop_oneof![
            2 => Just(Vec::new()),
            1 => prop::collection::vec(arb_oracle_text(), 1..=3),
        ],
        prop::bool::ANY,
        prop::option::of(arb_oracle_cortex()),
        arb_oracle_staleness(),
        (
            prop::option::of(arb_oracle_text()),
            prop::option::of(arb_oracle_text()),
        ),
        arb_oracle_verified_queries(),
        arb_oracle_metadata_refusal(),
        prop_oneof![
            2 => Just(None),
            1 => arb_inline_query().prop_map(Some),
        ],
    )
        .prop_map(
            |(
                mut def,
                view_tags,
                dim_tags,
                sample_values,
                is_enum,
                cortex,
                max_staleness,
                (sql_generation, question_categorization),
                verified_queries,
                refusal,
                inline_query,
            )| {
                if let Some(query) = inline_query {
                    def.tables[0].table = String::new();
                    def.tables[0].definition = Some(query);
                }
                def.tags = view_tags;
                def.dimensions[0].tags = dim_tags;
                def.dimensions[0].sample_values = sample_values;
                def.dimensions[0].is_enum = is_enum;
                def.dimensions[0].cortex_search_service = cortex;
                def.max_staleness = max_staleness;
                def.module_custom_instructions = CustomInstructions {
                    sql_generation,
                    question_categorization,
                };
                def.verified_queries = verified_queries;
                if let Some(refusal) = refusal {
                    apply_metadata_refusal(&mut def, refusal);
                }
                def
            },
        )
}

/// The query of an inline-query table `o` (Phase 6.4), as YAML may carry it:
/// with the line break a `|` block ends in, surrounding blanks, a comment
/// (which the front door would blank on replay), comment markers inside a
/// string (which it would not), and shapes that are not one query. Refused
/// and accepted shapes are both generated on purpose.
fn arb_inline_query() -> impl Strategy<Value = String> {
    (
        prop_oneof![
            Just("SELECT * FROM orders"),
            Just("SELECT *\nFROM orders\nWHERE amount > 0"),
            Just("SELECT id, '--' AS a, '/* x */' AS b FROM orders"),
            Just("SELECT 1 AS id) AS x, (SELECT 2"),
            Just("SELECT 1 AS id;"),
        ],
        prop_oneof![Just(""), Just("  ")],
        prop_oneof![
            3 => Just(""),
            2 => Just("\n"),
            1 => Just(" -- a note"),
            1 => Just("\n/* a note */\n"),
        ],
    )
        .prop_map(|(body, lead, tail)| format!("{lead}{body}{tail}"))
}

/// A definition anchored at one table `o`, with one dimension, one base metric
/// and an optional window metric over them, plus an optional materialization.
/// Shapes that are supposed to be REFUSED are generated on purpose — the
/// property's contract covers both outcomes.
fn arb_oracle_def_untagged() -> impl Strategy<Value = SemanticViewDefinition> {
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
    Ok(kb.into_definition())
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
        prop_assert_eq!(&replayed.tags, &imported.tags, "view tags drift\n{}", ddl);
        prop_assert_eq!(
            &replayed.max_staleness, &imported.max_staleness, "MAX_STALENESS drift\n{}", ddl
        );
        prop_assert_eq!(
            &replayed.module_custom_instructions,
            &imported.module_custom_instructions,
            "custom instructions drift\n{}",
            ddl
        );
        prop_assert_eq!(
            &replayed.verified_queries,
            &imported.verified_queries,
            "verified queries drift\n{}",
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
fn generator_reaches_both_outcomes_on_the_tag_axis() {
    let defs = sample_defs(512);
    let tagged =
        |d: &SemanticViewDefinition| !d.tags.is_empty() || !d.dimensions[0].tags.is_empty();
    let imports = |d: &SemanticViewDefinition| {
        let yaml = yaml_serde::to_string(d).expect("YAML serialize");
        SemanticViewDefinition::from_yaml_with_size_cap(VIEW, &yaml)
    };
    assert!(
        defs.iter().any(|d| tagged(d) && imports(d).is_ok()),
        "no tagged definition ever imported — the TAG round trip is unreached"
    );
    for (label, needle) in [
        ("an empty name part", "name part is empty"),
        ("a database with no schema", "no schema"),
        ("a value over the limit", "257 characters"),
        ("a duplicate name", "duplicate tag"),
    ] {
        assert!(
            defs.iter()
                .any(|d| imports(d).is_err_and(|e| e.contains(needle))),
            "no definition was refused for {label}"
        );
    }
}

#[test]
fn generator_reaches_both_outcomes_on_the_metadata_axis() {
    // More samples than the other guards: each refusal shape is one rare draw,
    // and shows only on a definition no earlier check refuses.
    let defs = sample_defs(12288);
    let imports = |d: &SemanticViewDefinition| {
        let yaml = yaml_serde::to_string(d).expect("YAML serialize");
        SemanticViewDefinition::from_yaml_with_size_cap(VIEW, &yaml)
    };
    let imported: Vec<SemanticViewDefinition> =
        defs.iter().filter_map(|d| imports(d).ok()).collect();
    let dim = |d: &SemanticViewDefinition| d.dimensions[0].clone();
    for (label, hit) in [
        (
            "sample values",
            imported.iter().any(|d| !dim(d).sample_values.is_empty()),
        ),
        ("IS_ENUM", imported.iter().any(|d| dim(d).is_enum)),
        (
            "a three-part Cortex Search service with a column",
            imported.iter().any(|d| {
                dim(d)
                    .cortex_search_service
                    .is_some_and(|c| c.database.is_some() && c.literal_column.is_some())
            }),
        ),
        (
            "MAX_STALENESS in seconds",
            imported
                .iter()
                .any(|d| matches!(d.max_staleness, Some(MaxStaleness::Seconds(_)))),
        ),
        (
            "MAX_STALENESS as an interval",
            imported
                .iter()
                .any(|d| matches!(d.max_staleness, Some(MaxStaleness::Interval(_)))),
        ),
        (
            "both custom instructions",
            imported.iter().any(|d| {
                let c = &d.module_custom_instructions;
                c.sql_generation.is_some() && c.question_categorization.is_some()
            }),
        ),
        (
            "two verified queries, one fully written",
            imported.iter().any(|d| {
                d.verified_queries.len() == 2
                    && d.verified_queries.iter().any(|q| {
                        q.verified_at.is_some()
                            && q.verified_by.is_some()
                            && q.use_as_onboarding_question
                    })
            }),
        ),
    ] {
        assert!(hit, "no definition with {label} ever imported");
    }
    for (label, needle) in [
        ("a short MAX_STALENESS", "the minimum is 120 seconds"),
        (
            "a MAX_STALENESS that is not an interval",
            "is not an interval",
        ),
        (
            "a duplicate verified query",
            "duplicate verified query 'VQ'",
        ),
        (
            "a verified query repeated with quotes",
            "duplicate verified query '\"VQ\"'",
        ),
        (
            "a verified query name with a comma",
            "verified query name 'a,b'",
        ),
        (
            "a verified query name with a parenthesis",
            "verified query name 'q)'",
        ),
        ("a verified query named AS", "verified query name 'as'"),
        (
            "a qualified verified query name",
            "verified query name 'db.q'",
        ),
        (
            "a verified query name of two words",
            "verified query name 'two words",
        ),
        (
            "a verified query name with a comment marker",
            "verified query name 'a--b'",
        ),
        (
            "a Cortex database with no schema",
            "names a database but no schema",
        ),
        (
            "an empty Cortex name part",
            "Cortex Search Service name part is empty",
        ),
        ("an empty Cortex column", "USING column is empty"),
    ] {
        assert!(
            defs.iter()
                .any(|d| imports(d).is_err_and(|e| e.contains(needle))),
            "no definition was refused for {label}"
        );
    }
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
fn generator_reaches_both_outcomes_on_the_inline_query_axis() {
    // Each shape the YAML form must either store as GET_DDL -> CREATE stores
    // it, or refuse: a `|` block's trailing line break (stored trimmed), a
    // comment (refused), and comment markers inside a string (accepted).
    let imports = |d: &SemanticViewDefinition| {
        let yaml = yaml_serde::to_string(d).expect("YAML serialize");
        SemanticViewDefinition::from_yaml_with_size_cap(VIEW, &yaml).is_ok()
    };
    let defs = sample_defs(512);
    let with_query: Vec<(&str, bool)> = defs
        .iter()
        .filter_map(|d| d.tables[0].definition.as_deref().map(|q| (q, imports(d))))
        .collect();
    assert!(
        with_query.iter().any(|(q, ok)| *ok && q.ends_with('\n')),
        "no query with a trailing line break was imported"
    );
    assert!(
        with_query
            .iter()
            .any(|(q, ok)| !*ok && q.contains("-- a note")),
        "no query with a comment was refused"
    );
    assert!(
        with_query.iter().any(|(q, ok)| *ok && q.contains("'--'")),
        "no query with comment markers inside a string was imported"
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
