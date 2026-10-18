//! Read-side member type probing for `SHOW` / `DESCRIBE` (TECH-DEBT #51).
//!
//! `SHOW SEMANTIC {DIMENSIONS,METRICS,FACTS}`, `SHOW COLUMNS IN SEMANTIC VIEW`,
//! `SHOW SEMANTIC DIMENSIONS FOR METRIC` and `DESCRIBE SEMANTIC VIEW` report a
//! `data_type` per member. Snowflake reports the member's actual type there.
//! Nothing at CREATE records one (v0.10.0 removed CREATE-time inference, and
//! no DDL clause can declare a type — see TECH-DEBT #68), so the type is
//! computed here, at the SHOW / DESCRIBE bind: expand a request for the
//! member, `DESCRIBE` the expanded SQL on the per-call connection, and read the
//! column type back.
//!
//! # What is reported
//!
//! The type `semantic_view()` *returns* for that member — what `typeof()` of
//! its column says. `semantic_view()` plans the expanded SQL inside the
//! caller's query (TECH-DEBT #42), so that is the expanded SQL's own type in
//! `DuckDB`'s spelling: `SUM` over an integer column is a HUGEINT.
//!
//! Probing on the read side means the type never goes stale: a source column
//! whose type changes after CREATE is reported with its new type.
//!
//! # Cost: one probe per member kind, not per member
//!
//! A view's dimensions are probed together in one dimensions-only request, its
//! facts in one facts request, and its non-window metrics in one metrics-only
//! request; each kind is probed only when the caller needs it. A batch that
//! does not expand or does not bind — a fan trap between two members, a
//! semi-additive metric that cannot share a query with a derived one, a
//! dropped source table — falls back to probing that kind's members one at a
//! time, so one bad member cannot hide the others' types. A window metric is
//! always probed on its own, together with the dimensions its `PARTITION BY` /
//! `ORDER BY` requires, because it has no shape it can be queried in alone.
//!
//! So the common case is at most three binds per view, and the worst case
//! is one per member. `SHOW SEMANTIC … ` without `IN` probes every view, so it
//! is linear in the total member count.
//!
//! # When a member cannot be typed
//!
//! A member whose probe fails even on its own — typically because a source
//! table or column was dropped or renamed after CREATE — reports an empty
//! `data_type` (or the declared `output_type` of a legacy pre-#68 definition,
//! if it carries one). SHOW / DESCRIBE never fail because a member cannot be
//! typed; querying that member still reports the binder error.
//!
//! # Cancellation
//!
//! The one failure that is not swallowed. The probes run on the bind's own
//! connection, whose interrupt flag a cancel of the caller does not set, so
//! the caller's state is passed in (`cancelled`) and consulted before every
//! probe, and a probe the engine reports as interrupted
//! ([`ProbeError::Interrupted`]) is a cancellation, not an untypable member.
//! Either way typing stops at once with [`Interrupted`] — no fallback probes,
//! no further views — and the C++ bind helpers raise the caller's INTERRUPT.
//! They also forward the caller's flag into the probe connection while the
//! dispatcher runs, which is what stops a probe that is already binding.
//!
//! # PRIVATE members
//!
//! A PRIVATE fact or metric cannot be requested, but it still has a type, and
//! SHOW / DESCRIBE list it. The probe expands against a copy of the definition
//! in which every member is PUBLIC; the copy is never stored or queried.
//!
//! # Role-played members
//!
//! A member on (or reached only through) a table that several relationships
//! from one source point at cannot be queried without a role, so its own
//! probe is rejected as ambiguous. Its type does not depend on the role, so
//! such a member is retried against a copy of the definition that keeps only
//! the first-declared relationship of each such pair. The copy is used for
//! typing only; the ambiguity check still applies to every real query.

use std::collections::HashMap;

use libduckdb_sys as ffi;

use crate::expand::{expand, DimensionName, FactName, MetricName, QueryRequest};
use crate::model::{AccessModifier, SemanticViewDefinition};

/// Which member kinds a caller needs typed. A SHOW over one kind probes only
/// that kind.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Wanted {
    pub dimensions: bool,
    pub metrics: bool,
    pub facts: bool,
}

impl Wanted {
    /// Every member kind (DESCRIBE, SHOW COLUMNS).
    pub const ALL: Self = Self {
        dimensions: true,
        metrics: true,
        facts: true,
    };
    /// Dimensions only.
    pub const DIMENSIONS: Self = Self {
        dimensions: true,
        metrics: false,
        facts: false,
    };
    /// Metrics only.
    pub const METRICS: Self = Self {
        dimensions: false,
        metrics: true,
        facts: false,
    };
    /// Facts only.
    pub const FACTS: Self = Self {
        dimensions: false,
        metrics: false,
        facts: true,
    };
}

/// One output column of a probed statement: its name and the type
/// `semantic_view()` would declare for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbedColumn {
    pub name: String,
    pub data_type: String,
}

/// Why a probe yielded no columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// The caller cancelled the command. Typing stops and the cancellation is
    /// reported, never mistaken for an untypable member.
    Interrupted,
    /// Anything else: the members it was probing stay untyped.
    Failed(String),
}

/// Typing stopped because the caller cancelled the command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted;

impl From<Interrupted> for String {
    /// `DuckDB`'s own interrupt text. The C++ bind helpers raise a bare
    /// `InterruptException` for a cancelled caller ahead of this message; it
    /// only reaches the user if the probe connection alone was interrupted.
    fn from(_: Interrupted) -> Self {
        "Interrupted!".to_string()
    }
}

/// The probed type of each member of one view, keyed by the member's stored
/// name. A member that could not be typed is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemberTypes {
    dimensions: HashMap<String, String>,
    metrics: HashMap<String, String>,
    facts: HashMap<String, String>,
}

impl MemberTypes {
    /// The reported type of dimension `name`: the probed type, else the
    /// declared `output_type`, else empty.
    #[must_use]
    pub fn dimension(&self, name: &str, declared: Option<&String>) -> String {
        pick(self.dimensions.get(name), declared)
    }

    /// The reported type of metric `name` (see [`Self::dimension`]).
    #[must_use]
    pub fn metric(&self, name: &str, declared: Option<&String>) -> String {
        pick(self.metrics.get(name), declared)
    }

    /// The reported type of fact `name` (see [`Self::dimension`]).
    #[must_use]
    pub fn fact(&self, name: &str, declared: Option<&String>) -> String {
        pick(self.facts.get(name), declared)
    }
}

fn pick(probed: Option<&String>, declared: Option<&String>) -> String {
    probed.or(declared).cloned().unwrap_or_default()
}

/// Type every member of the `wanted` kinds of `def` by expanding requests for
/// them and handing the expanded SQL to `probe`, which returns the declared
/// output columns of a statement (see [`probe_output_types`]).
///
/// A member whose type cannot be established is left out of the result; that
/// is never an error. The one error is cancellation: `cancelled` is consulted
/// before every probe, and a probe that reports [`ProbeError::Interrupted`]
/// stops typing too, so a cancelled caller gets `Err(Interrupted)` promptly
/// instead of every remaining probe running. See the module docs for the
/// batching strategy and its cost.
///
/// # Errors
///
/// [`Interrupted`] when the caller cancelled while typing was in progress.
pub fn infer_member_types(
    view_name: &str,
    def: &SemanticViewDefinition,
    wanted: Wanted,
    cancelled: &dyn Fn() -> bool,
    probe: &mut dyn FnMut(&str) -> Result<Vec<ProbedColumn>, ProbeError>,
) -> Result<MemberTypes, Interrupted> {
    // PRIVATE members are typed too: probe a copy in which nothing is private.
    let mut def = def.clone();
    for m in &mut def.metrics {
        m.access = AccessModifier::Public;
    }
    for f in &mut def.facts {
        f.access = AccessModifier::Public;
    }
    // A member on (or below) a role-playing table cannot be queried without a
    // role, so its own probe is rejected as ambiguous. Its type does not
    // depend on the role, so such members are retried on a copy that keeps
    // one relationship per role-played pair.
    let one_role = single_role_copy(&def);
    let mut prober = Prober {
        view_name,
        cancelled,
        probe,
    };
    let mut out = MemberTypes::default();

    if wanted.dimensions {
        let names: Vec<String> = def.dimensions.iter().map(|d| d.name.clone()).collect();
        let build = |n: &[String]| Request {
            dimensions: n.to_vec(),
            ..Request::default()
        };
        out.dimensions = prober.batch_then_each(&def, &names, build)?;
        prober.retry_missing(one_role.as_ref(), &names, &mut out.dimensions, build)?;
    }
    if wanted.facts {
        let names: Vec<String> = def.facts.iter().map(|f| f.name.clone()).collect();
        let build = |n: &[String]| Request {
            facts: n.to_vec(),
            ..Request::default()
        };
        out.facts = prober.batch_then_each(&def, &names, build)?;
        prober.retry_missing(one_role.as_ref(), &names, &mut out.facts, build)?;
    }
    if wanted.metrics {
        let (window, plain): (Vec<_>, Vec<_>) =
            def.metrics.iter().partition(|m| m.window_spec.is_some());
        let names: Vec<String> = plain.iter().map(|m| m.name.clone()).collect();
        let build = |n: &[String]| Request {
            metrics: n.to_vec(),
            ..Request::default()
        };
        out.metrics = prober.batch_then_each(&def, &names, build)?;
        prober.retry_missing(one_role.as_ref(), &names, &mut out.metrics, build)?;
        // A window metric needs the dimensions its spec names; it has no
        // shape it can be queried in alone, so it is always probed singly.
        for m in window {
            for d in std::iter::once(&def).chain(one_role.as_ref()) {
                let req = Request {
                    dimensions: crate::expand::window_required_dimensions(d, m),
                    metrics: vec![m.name.clone()],
                    facts: Vec::new(),
                };
                if let Some(t) = prober.run(d, &req)?.and_then(|mut t| t.pop()) {
                    out.metrics.insert(m.name.clone(), t);
                    break;
                }
            }
        }
    }
    Ok(out)
}

/// A copy of `def` keeping only the first-declared relationship between each
/// (source, target) table pair, or `None` when no pair has more than one — the
/// definition plays no roles. Used only to type members the role-playing
/// ambiguity checks reject on their own; it is never stored or queried.
fn single_role_copy(def: &SemanticViewDefinition) -> Option<SemanticViewDefinition> {
    let mut seen = std::collections::HashSet::new();
    let joins: Vec<_> = def
        .joins
        .iter()
        .filter(|j| {
            seen.insert((
                crate::ident::normalize_ident_part(&j.from_alias),
                crate::ident::normalize_ident_part(&j.table),
            ))
        })
        .cloned()
        .collect();
    if joins.len() == def.joins.len() {
        return None;
    }
    let mut copy = def.clone();
    copy.joins = joins;
    Some(copy)
}

/// A probe request by member name. The members whose types are wanted are the
/// metrics or facts when there are any, else the dimensions.
#[derive(Default)]
struct Request {
    dimensions: Vec<String>,
    metrics: Vec<String>,
    facts: Vec<String>,
}

struct Prober<'a, 'p> {
    view_name: &'a str,
    cancelled: &'p dyn Fn() -> bool,
    probe: &'p mut dyn FnMut(&str) -> Result<Vec<ProbedColumn>, ProbeError>,
}

impl Prober<'_, '_> {
    /// Probe all of `names` in one request built by `build`; if that request
    /// fails, probe each name in a request of its own.
    fn batch_then_each(
        &mut self,
        def: &SemanticViewDefinition,
        names: &[String],
        build: impl Fn(&[String]) -> Request,
    ) -> Result<HashMap<String, String>, Interrupted> {
        let mut out = HashMap::new();
        if names.is_empty() {
            return Ok(out);
        }
        if let Some(types) = self.run(def, &build(names))? {
            out.extend(names.iter().cloned().zip(types));
            return Ok(out);
        }
        if names.len() == 1 {
            return Ok(out);
        }
        for n in names {
            if let Some(t) = self
                .run(def, &build(std::slice::from_ref(n)))?
                .and_then(|mut t| t.pop())
            {
                out.insert(n.clone(), t);
            }
        }
        Ok(out)
    }

    /// Probe each of `names` still missing from `out` on its own against
    /// `def` (the single-role copy), when there is one.
    fn retry_missing(
        &mut self,
        def: Option<&SemanticViewDefinition>,
        names: &[String],
        out: &mut HashMap<String, String>,
        build: impl Fn(&[String]) -> Request,
    ) -> Result<(), Interrupted> {
        let Some(def) = def else {
            return Ok(());
        };
        for n in names {
            if out.contains_key(n) {
                continue;
            }
            if let Some(t) = self
                .run(def, &build(std::slice::from_ref(n)))?
                .and_then(|mut t| t.pop())
            {
                out.insert(n.clone(), t);
            }
        }
        Ok(())
    }

    /// Expand and probe `req`; return the types of its wanted members (the
    /// trailing metrics / facts, or all dimensions when there are neither), in
    /// request order. `Ok(None)` when the request does not expand, does not
    /// bind, or returns columns that do not line up with the request;
    /// `Err(Interrupted)` when the caller has cancelled.
    fn run(
        &mut self,
        def: &SemanticViewDefinition,
        req: &Request,
    ) -> Result<Option<Vec<String>>, Interrupted> {
        if (self.cancelled)() {
            return Err(Interrupted);
        }
        let query = QueryRequest {
            dimensions: req
                .dimensions
                .iter()
                .map(|n| DimensionName::new(n.clone()))
                .collect(),
            metrics: req
                .metrics
                .iter()
                .map(|n| MetricName::new(n.clone()))
                .collect(),
            facts: req.facts.iter().map(|n| FactName::new(n.clone())).collect(),
            where_clause: None,
        };
        let Ok(sql) = expand(self.view_name, def, &query) else {
            return Ok(None);
        };
        let cols = match (self.probe)(&format!("{sql} LIMIT 0")) {
            Ok(cols) => cols,
            Err(ProbeError::Interrupted) => return Err(Interrupted),
            Err(ProbeError::Failed(_)) => return Ok(None),
        };
        // Output order is the request order: dimensions, then metrics or facts.
        let expected: Vec<&String> = req
            .dimensions
            .iter()
            .chain(req.metrics.iter())
            .chain(req.facts.iter())
            .collect();
        if cols.len() != expected.len()
            || !cols
                .iter()
                .zip(&expected)
                .all(|(c, n)| crate::ident::ident_matches(n, &c.name))
        {
            return Ok(None);
        }
        let wanted = if req.metrics.is_empty() && req.facts.is_empty() {
            0
        } else {
            req.dimensions.len()
        };
        Ok(Some(
            cols.into_iter().skip(wanted).map(|c| c.data_type).collect(),
        ))
    }
}

/// Probe the output columns of `expanded_sql` on `conn`: their names and
/// types, as `semantic_view()` returns them, via `DESCRIBE`.
///
/// # Safety
///
/// `conn` must be a valid, non-null `duckdb_connection` handle.
pub unsafe fn probe_output_types(
    conn: ffi::duckdb_connection,
    expanded_sql: &str,
) -> Result<Vec<ProbedColumn>, ProbeError> {
    let described = crate::query::raw_sql::describe_columns(conn, expanded_sql).map_err(|e| {
        if e.interrupted {
            ProbeError::Interrupted
        } else {
            ProbeError::Failed(e.message)
        }
    })?;
    Ok(described
        .into_iter()
        .map(|(name, data_type)| ProbedColumn { name, data_type })
        .collect())
}

/// [`infer_member_types`] probing on `conn` via [`probe_output_types`].
/// `cancelled` reports whether the command's caller has been cancelled (the
/// extension passes the bind caller's interrupt flag).
///
/// # Safety
///
/// `conn` must be a valid, non-null `duckdb_connection` handle for the whole
/// call.
///
/// # Errors
///
/// [`Interrupted`] when the caller cancelled while typing was in progress.
pub unsafe fn infer_member_types_on(
    conn: ffi::duckdb_connection,
    view_name: &str,
    def: &SemanticViewDefinition,
    wanted: Wanted,
    cancelled: &dyn Fn() -> bool,
) -> Result<MemberTypes, Interrupted> {
    infer_member_types(view_name, def, wanted, cancelled, &mut |sql| unsafe {
        probe_output_types(conn, sql)
    })
}

#[cfg(all(test, not(feature = "extension")))]
mod tests {
    use super::*;
    use crate::test_helpers::RawDb;

    /// Build a definition from a `CREATE SEMANTIC VIEW` body the way the
    /// CREATE front door does (parse + cardinality inference).
    fn def_from(body: &str) -> SemanticViewDefinition {
        let mut kb = crate::body_parser::parse_keyword_body(body, 0)
            .unwrap_or_else(|e| panic!("body must parse: {}", e.message));
        crate::graph::infer_cardinality(&kb.tables, &mut kb.relationships)
            .unwrap_or_else(|e| panic!("cardinality: {}", e.message));
        SemanticViewDefinition {
            tables: kb.tables,
            dimensions: kb.dimensions,
            metrics: kb.metrics,
            joins: kb.relationships,
            facts: kb.facts,
            materializations: kb.materializations,
            created_on: None,
            database_name: None,
            schema_name: None,
            resolution_schema_name: None,
            temp_tables: vec![],
            comment: None,
        }
    }

    fn db(setup: &[&str]) -> RawDb {
        let db = RawDb::open_in_memory();
        for s in setup {
            unsafe { db.exec(s) };
        }
        db
    }

    fn types_on(db: &RawDb, def: &SemanticViewDefinition, wanted: Wanted) -> MemberTypes {
        unsafe { infer_member_types_on(db.conn, "v", def, wanted, &|| false) }
            .expect("nothing cancels this call")
    }

    /// [`infer_member_types`] for a caller that is never cancelled.
    fn infer(
        def: &SemanticViewDefinition,
        wanted: Wanted,
        probe: &mut dyn FnMut(&str) -> Result<Vec<ProbedColumn>, ProbeError>,
    ) -> MemberTypes {
        infer_member_types("v", def, wanted, &|| false, probe).expect("nothing cancels this call")
    }

    const ORDERS: &[&str] = &[
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, \"Name\" VARCHAR, tier SMALLINT)",
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, cust_id INTEGER, amount DECIMAL(10,2), \
         qty INTEGER, region VARCHAR, placed DATE, score DOUBLE, tags VARCHAR[])",
        "INSERT INTO customers VALUES (1, 'Ann', 1)",
        "INSERT INTO orders VALUES (1, 1, 10.50, 2, 'EU', '2024-01-01', 1.5, ['a'])",
    ];

    const ORDERS_VIEW: &str = "AS
        TABLES (o AS orders PRIMARY KEY (id), c AS customers PRIMARY KEY (id))
        RELATIONSHIPS (oc AS o(cust_id) REFERENCES c)
        FACTS (o.line_total AS o.amount * o.qty, PRIVATE o.raw_score AS o.score)
        DIMENSIONS (
            o.region AS o.region,
            o.placed AS o.placed,
            o.tags AS o.tags,
            c.\"Customer Name\" AS c.\"Name\",
            c.tier AS c.tier
        )
        METRICS (
            o.order_count AS COUNT(o.id),
            o.avg_qty AS AVG(o.qty),
            o.total_qty AS SUM(o.qty),
            o.total_amount AS SUM(o.amount),
            PRIVATE o.score_sum AS SUM(o.raw_score),
            avg_order AS total_amount / order_count,
            o.qty_lag AS LAG(total_qty, 1) OVER (ORDER BY placed),
            o.closing_amount NON ADDITIVE BY (placed) AS SUM(o.amount)
        )";

    /// A member's reported type is what `typeof()` says of its column, in
    /// `DuckDB`'s own spelling, for every type: `semantic_view()` returns the
    /// expanded SQL's own types (TECH-DEBT #42), so nothing is narrowed.
    #[test]
    fn reported_types_are_typeof() {
        let db = db(&[]);
        // (expression, its type as DuckDB spells it)
        let cases: &[(&str, &str)] = &[
            ("TRUE", "BOOLEAN"),
            ("1::TINYINT", "TINYINT"),
            ("1::INTEGER", "INTEGER"),
            ("1::BIGINT", "BIGINT"),
            ("1::HUGEINT", "HUGEINT"),
            ("1::UHUGEINT", "UHUGEINT"),
            ("1::UBIGINT", "UBIGINT"),
            ("1::DOUBLE", "DOUBLE"),
            ("TIMESTAMP '2024-01-01'", "TIMESTAMP"),
            ("DATE '2024-01-01'", "DATE"),
            ("INTERVAL 1 DAY", "INTERVAL"),
            ("'a'", "VARCHAR"),
            ("'2024-01-01'::TIMESTAMPTZ", "TIMESTAMP WITH TIME ZONE"),
            ("gen_random_uuid()", "UUID"),
            ("1.5::DECIMAL(10,3)", "DECIMAL(10,3)"),
            ("[1, 2]", "INTEGER[]"),
            ("[1::HUGEINT]", "HUGEINT[]"),
            ("1::SMALLINT", "SMALLINT"),
            ("1::UTINYINT", "UTINYINT"),
            ("1::USMALLINT", "USMALLINT"),
            ("1::UINTEGER", "UINTEGER"),
            ("1::FLOAT", "FLOAT"),
            ("TIME '01:02:03'", "TIME"),
            ("'a'::BLOB", "BLOB"),
            ("'2024-01-01'::TIMESTAMP_S", "TIMESTAMP_S"),
            ("'2024-01-01'::TIMESTAMP_MS", "TIMESTAMP_MS"),
            ("'2024-01-01'::TIMESTAMP_NS", "TIMESTAMP_NS"),
            ("'01:02:03+00'::TIMETZ", "TIME WITH TIME ZONE"),
            ("'101'::BIT", "BIT"),
            ("[1, 2]::INTEGER[2]", "INTEGER[2]"),
            ("'x'::ENUM('x', 'y')", "ENUM('x', 'y')"),
            ("{'a': 1}", "STRUCT(a INTEGER)"),
            ("MAP {'a': 1}", "MAP(VARCHAR, INTEGER)"),
        ];
        for (expr, want) in cases {
            let got = unsafe { probe_output_types(db.conn, &format!("SELECT {expr} AS c")) }
                .unwrap_or_else(|e| panic!("probe of {expr} failed: {e:?}"));
            assert_eq!(
                got,
                vec![ProbedColumn {
                    name: "c".to_string(),
                    data_type: (*want).to_string()
                }],
                "reported type of {expr}"
            );
            let t = unsafe { probe_typeof(&db, &format!("SELECT typeof({expr}) AS t")) };
            assert_eq!(&t, want, "typeof({expr})");
        }
    }

    /// Read the single VARCHAR cell of `sql`.
    unsafe fn probe_typeof(db: &RawDb, sql: &str) -> String {
        let mut result = crate::query::raw_sql::execute_sql_checked(db.conn, sql).expect("typeof");
        let chunk = ffi::duckdb_result_get_chunk(result, 0);
        let v = crate::query::raw_sql::read_varchar_from_vector(chunk, 0, 0);
        ffi::duckdb_destroy_data_chunk(&mut { chunk });
        ffi::duckdb_destroy_result(&raw mut result);
        v
    }

    #[test]
    fn dimensions_report_their_column_types() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::DIMENSIONS);
        assert_eq!(t.dimension("region", None), "VARCHAR");
        assert_eq!(t.dimension("placed", None), "DATE");
        assert_eq!(t.dimension("tags", None), "VARCHAR[]");
    }

    #[test]
    fn quoted_mixed_case_dimension_on_a_joined_table_is_typed() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::DIMENSIONS);
        assert_eq!(t.dimension("\"Customer Name\"", None), "VARCHAR");
        assert_eq!(t.dimension("tier", None), "SMALLINT");
    }

    /// Aggregates whose type differs from their input's.
    #[test]
    fn aggregate_metrics_report_the_aggregate_type() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::METRICS);
        assert_eq!(t.metric("order_count", None), "BIGINT");
        assert_eq!(t.metric("avg_qty", None), "DOUBLE");
        // SUM over an integer column is a HUGEINT, and is reported as one.
        assert_eq!(t.metric("total_qty", None), "HUGEINT");
        assert_eq!(t.metric("total_amount", None), "DECIMAL(38,2)");
    }

    #[test]
    fn derived_metric_is_typed() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::METRICS);
        assert_eq!(t.metric("avg_order", None), "DOUBLE");
    }

    #[test]
    fn window_metric_is_typed_with_its_required_dimensions() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::METRICS);
        assert_eq!(t.metric("qty_lag", None), "HUGEINT");
    }

    /// The semi-additive metric cannot share a query with the derived one
    /// (`SemiAdditiveUnsupportedExpression`), so the metrics batch fails and
    /// every metric is typed by the per-member fallback.
    #[test]
    fn semi_additive_metric_is_typed_when_the_batch_cannot_expand() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::METRICS);
        assert_eq!(t.metric("closing_amount", None), "DECIMAL(38,2)");
        assert_eq!(t.metric("order_count", None), "BIGINT");
    }

    #[test]
    fn private_members_are_typed() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::ALL);
        assert_eq!(t.metric("score_sum", None), "DOUBLE");
        assert_eq!(t.fact("raw_score", None), "DOUBLE");
    }

    #[test]
    fn facts_report_their_row_level_type() {
        let db = db(ORDERS);
        let t = types_on(&db, &def_from(ORDERS_VIEW), Wanted::FACTS);
        assert_eq!(t.fact("line_total", None), "DECIMAL(18,2)");
    }

    /// A dropped source table empties only its own members' types.
    #[test]
    fn dropped_table_leaves_only_its_members_untyped() {
        let db = db(&[
            "CREATE TABLE keep (id INTEGER PRIMARY KEY, k VARCHAR, n BIGINT)",
            "CREATE TABLE gone (id INTEGER PRIMARY KEY, keep_id INTEGER, g DOUBLE)",
        ]);
        let def = def_from(
            "AS TABLES (k AS keep PRIMARY KEY (id), g AS gone PRIMARY KEY (id))
             RELATIONSHIPS (gk AS g(keep_id) REFERENCES k)
             DIMENSIONS (k.k AS k.k, g.g_dim AS g.g)
             METRICS (k.n_total AS SUM(k.n), g.g_total AS SUM(g.g))",
        );
        unsafe { db.exec("DROP TABLE gone") };
        let t = types_on(&db, &def, Wanted::ALL);
        assert_eq!(t.dimension("k", None), "VARCHAR");
        assert_eq!(t.dimension("g_dim", None), "");
        assert_eq!(t.metric("n_total", None), "HUGEINT");
        assert_eq!(t.metric("g_total", None), "");
    }

    /// A legacy definition's declared `output_type` is the fallback when the
    /// probe cannot type the member.
    #[test]
    fn declared_output_type_is_the_fallback_for_an_untypable_member() {
        let db = db(&[]);
        let mut def = def_from("AS TABLES (t AS missing PRIMARY KEY (id)) DIMENSIONS (t.d AS t.d)");
        def.dimensions[0].output_type = Some("VARCHAR(10)".to_string());
        let t = types_on(&db, &def, Wanted::DIMENSIONS);
        assert_eq!(
            t.dimension("d", def.dimensions[0].output_type.as_ref()),
            "VARCHAR(10)"
        );
        assert_eq!(t.dimension("d", None), "");
    }

    /// Cost guard: a view whose members can share a query costs one probe per
    /// member kind, however many members it has — and only the kinds asked for.
    #[test]
    fn members_are_probed_in_one_batch_per_kind() {
        let db = db(ORDERS);
        let def = def_from(
            "AS TABLES (o AS orders PRIMARY KEY (id))
             FACTS (o.f1 AS o.qty, o.f2 AS o.amount, o.f3 AS o.score)
             DIMENSIONS (o.d1 AS o.region, o.d2 AS o.placed, o.d3 AS o.qty, o.d4 AS o.score)
             METRICS (o.m1 AS SUM(o.qty), o.m2 AS COUNT(*), o.m3 AS AVG(o.score))",
        );
        let mut calls = 0usize;
        let t = infer(&def, Wanted::ALL, &mut |sql| {
            calls += 1;
            unsafe { probe_output_types(db.conn, sql) }
        });
        assert_eq!(calls, 3, "one probe per member kind");
        assert_eq!(t.dimension("d4", None), "DOUBLE");
        assert_eq!(t.metric("m2", None), "BIGINT");
        assert_eq!(t.fact("f2", None), "DECIMAL(10,2)");

        let mut calls = 0usize;
        let _ = infer(&def, Wanted::DIMENSIONS, &mut |sql| {
            calls += 1;
            unsafe { probe_output_types(db.conn, sql) }
        });
        assert_eq!(calls, 1, "only the wanted kind is probed");
    }

    /// When a batch fails, every member of that kind is retried on its own —
    /// and no more than that.
    #[test]
    fn a_failed_batch_falls_back_to_one_probe_per_member() {
        let db = db(ORDERS);
        let def = def_from(
            "AS TABLES (o AS orders PRIMARY KEY (id))
             DIMENSIONS (o.d1 AS o.region, o.d2 AS o.nope, o.d3 AS o.qty)",
        );
        let mut calls = 0usize;
        let t = infer(&def, Wanted::DIMENSIONS, &mut |sql| {
            calls += 1;
            unsafe { probe_output_types(db.conn, sql) }
        });
        assert_eq!(calls, 1 + 3, "one batch, then one probe per dimension");
        assert_eq!(t.dimension("d1", None), "VARCHAR");
        assert_eq!(t.dimension("d2", None), "");
        assert_eq!(t.dimension("d3", None), "INTEGER");
    }

    /// A probe column that does not line up with the requested member (a
    /// mis-mapped batch) must not be attributed to it.
    #[test]
    fn a_misaligned_probe_result_is_not_attributed() {
        let def = def_from(
            "AS TABLES (o AS orders PRIMARY KEY (id))
             DIMENSIONS (o.d1 AS o.region, o.d2 AS o.qty)",
        );
        let t = infer(&def, Wanted::DIMENSIONS, &mut |_sql| {
            Ok(vec![ProbedColumn {
                name: "something_else".to_string(),
                data_type: "INTEGER".to_string(),
            }])
        });
        assert_eq!(t, MemberTypes::default());
    }

    const FLIGHTS: &[&str] = &[
        "CREATE TABLE airports (code VARCHAR, city VARCHAR, elevation INTEGER)",
        "CREATE TABLE flights (id INTEGER, dep VARCHAR, arr VARCHAR, carrier VARCHAR, \
         dist DOUBLE)",
    ];

    /// Two relationships to `airports`: its members are ambiguous when
    /// queried without a role, so their own probes are rejected and they are
    /// typed on the single-role copy. Members that resolve without a role, or
    /// carry their own `USING`, keep their direct probe.
    const FLIGHTS_VIEW: &str = "AS
        TABLES (f AS flights PRIMARY KEY (id), a AS airports PRIMARY KEY (code))
        RELATIONSHIPS (dep_ap AS f(dep) REFERENCES a, arr_ap AS f(arr) REFERENCES a)
        FACTS (a.elev_m AS a.elevation * 0.3048, f.dist_km AS f.dist * 1.609)
        DIMENSIONS (a.city AS a.city, f.carrier AS f.carrier)
        METRICS (
            f.departures USING (dep_ap) AS COUNT(*),
            f.arrivals USING (arr_ap) AS COUNT(*),
            a.max_elevation AS MAX(a.elevation),
            f.total_dist AS SUM(f.dist)
        )";

    #[test]
    fn role_played_dimension_is_typed() {
        let db = db(FLIGHTS);
        let t = types_on(&db, &def_from(FLIGHTS_VIEW), Wanted::DIMENSIONS);
        assert_eq!(t.dimension("city", None), "VARCHAR");
        assert_eq!(t.dimension("carrier", None), "VARCHAR");
    }

    #[test]
    fn role_played_fact_is_typed() {
        let db = db(FLIGHTS);
        let t = types_on(&db, &def_from(FLIGHTS_VIEW), Wanted::FACTS);
        assert_eq!(t.fact("elev_m", None), "DECIMAL(15,4)");
        assert_eq!(t.fact("dist_km", None), "DOUBLE");
    }

    #[test]
    fn role_played_metric_is_typed() {
        let db = db(FLIGHTS);
        let t = types_on(&db, &def_from(FLIGHTS_VIEW), Wanted::METRICS);
        assert_eq!(t.metric("max_elevation", None), "INTEGER");
        assert_eq!(t.metric("departures", None), "BIGINT");
        assert_eq!(t.metric("arrivals", None), "BIGINT");
        assert_eq!(t.metric("total_dist", None), "DOUBLE");
    }

    // ---- Cancellation (PR #248 review) --------------------------------------
    //
    // SHOW / DESCRIBE probe on the bind's own connection, whose interrupt flag
    // nothing sets when the *caller* is cancelled. Typing must therefore stop
    // on the caller's word, and a probe the engine itself reports as
    // interrupted must stop it too — never be filed as "type unknown" and
    // followed by every remaining fallback probe.

    const CANCEL_VIEW: &str = "AS TABLES (o AS orders PRIMARY KEY (id))
         FACTS (o.f1 AS o.qty)
         DIMENSIONS (o.d1 AS o.region, o.d2 AS o.nope, o.d3 AS o.qty)
         METRICS (o.m1 AS SUM(o.qty))";

    /// A probe reporting an interrupt ends typing with `Interrupted`, after
    /// that one probe: no per-member fallback, no further member kinds.
    #[test]
    fn an_interrupted_probe_stops_typing_and_is_reported() {
        let def = def_from(CANCEL_VIEW);
        let mut calls = 0usize;
        let got = infer_member_types("v", &def, Wanted::ALL, &|| false, &mut |_sql| {
            calls += 1;
            Err(ProbeError::Interrupted)
        });
        assert_eq!(got, Err(Interrupted));
        assert_eq!(calls, 1, "nothing is probed after the interrupt");
    }

    /// The caller's cancellation is consulted before every probe, so a cancel
    /// that lands between probes stops the next one from running.
    #[test]
    fn a_cancelled_caller_stops_typing_before_the_next_probe() {
        let db = db(ORDERS);
        let def = def_from(CANCEL_VIEW);
        let calls = std::cell::Cell::new(0usize);
        // Cancelled as soon as the first probe has run (the dimension batch,
        // which fails on `o.nope` and would otherwise fall back per member).
        let cancelled = || calls.get() >= 1;
        let got = infer_member_types("v", &def, Wanted::ALL, &cancelled, &mut |sql| {
            calls.set(calls.get() + 1);
            unsafe { probe_output_types(db.conn, sql) }
        });
        assert_eq!(got, Err(Interrupted));
        assert_eq!(calls.get(), 1, "the cancel is seen before the second probe");

        let calls = std::cell::Cell::new(0usize);
        let got = infer_member_types("v", &def, Wanted::ALL, &|| true, &mut |_sql| {
            calls.set(calls.get() + 1);
            Ok(Vec::new())
        });
        assert_eq!(got, Err(Interrupted));
        assert_eq!(calls.get(), 0, "an already-cancelled caller probes nothing");
    }

    /// Ordinary probe failures are still per-member and never end typing,
    /// however many there are.
    #[test]
    fn ordinary_probe_failures_do_not_end_typing() {
        let def = def_from(CANCEL_VIEW);
        let mut calls = 0usize;
        let got = infer_member_types("v", &def, Wanted::ALL, &|| false, &mut |_sql| {
            calls += 1;
            Err(ProbeError::Failed("Binder Error: nope".to_string()))
        });
        assert_eq!(got, Ok(MemberTypes::default()));
        // dims: batch + 3 singles; facts: 1; metrics: 1.
        assert_eq!(calls, 4 + 1 + 1, "every fallback probe still runs");
    }

    /// A probe the engine interrupts is classified as `Interrupted`, not as an
    /// ordinary failure. The probe DESCRIBEs its statement, which binds it
    /// without running it, so a probe is over in about a millisecond and an
    /// interrupt fired from another thread lands inside one only some of the
    /// time (a statement resets the connection's flag when it starts). So the
    /// test probes until one is interrupted, and every probe the interrupt hit
    /// must say so.
    #[test]
    fn an_engine_interrupt_is_classified_as_interrupted() {
        struct SendConn(ffi::duckdb_connection);
        // SAFETY: `duckdb_interrupt` is the C API's cross-thread cancel entry
        // point; the handle outlives the interrupter (joined below).
        unsafe impl Send for SendConn {}

        let db = db(&[]);
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let conn = SendConn(db.conn);
        let stop = std::sync::Arc::clone(&done);
        let interrupter = std::thread::spawn(move || {
            let conn = conn;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                unsafe { ffi::duckdb_interrupt(conn.0) };
                std::thread::yield_now();
            }
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let mut interrupted = false;
        while !interrupted && std::time::Instant::now() < deadline {
            match unsafe {
                probe_output_types(db.conn, "SELECT sum(i) AS s FROM range(1000000000000) t(i)")
            } {
                Err(ProbeError::Interrupted) => interrupted = true,
                Ok(_) => {}
                Err(other) => panic!("an interrupted probe was not classified: {other:?}"),
            }
        }
        done.store(true, std::sync::atomic::Ordering::SeqCst);
        interrupter.join().expect("interrupter thread");
        assert!(interrupted, "no probe was interrupted within the deadline");
    }
}
