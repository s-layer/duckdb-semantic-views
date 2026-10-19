# DuckDB Semantic Views

[![Docs](https://img.shields.io/badge/docs-GitHub%20Pages-blue)](https://s-layer.github.io/duckdb-semantic-views/)

A DuckDB extension that lets you define dimensions and metrics once, then query them in any combination. The extension writes the GROUP BY and JOIN logic for you.

Inspired by [Snowflake Semantic Views](https://docs.snowflake.com/en/sql-reference/sql/create-semantic-view), adapted for DuckDB as a loadable extension.

## How it works

You define a semantic view over one or more tables, declaring:

- **Dimensions** -- columns or expressions to group by (region, category, `date_trunc('month', created_at)`, etc.)
- **Metrics** -- aggregates (`sum(amount)`, `count(*)`, etc.)
- **Relationships** -- PK/FK join paths between tables, included only when the query needs them

Then you query by picking which dimensions and metrics you want. The extension generates the SQL -- SELECT, FROM, JOIN, GROUP BY -- and DuckDB executes it.

## Quick start

Requires DuckDB 2.0. Each release of the extension loads only in the DuckDB release it was built for; on DuckDB 1.5, stay on v0.14.0.

```sql
INSTALL semantic_views FROM community;
LOAD semantic_views;

CREATE TABLE orders (
    id INTEGER, region VARCHAR, category VARCHAR,
    amount DECIMAL(10,2)
);

CREATE SEMANTIC VIEW order_metrics AS
TABLES (
    o AS orders PRIMARY KEY (id)
)
DIMENSIONS (
    o.region AS o.region,
    o.category AS o.category
)
METRICS (
    o.revenue AS sum(o.amount),
    o.order_count AS count(*)
);

-- Pick any combination of dimensions and metrics
SELECT * FROM semantic_view('order_metrics',
    dimensions := ['region', 'category'],
    metrics := ['revenue', 'order_count']
);

-- Dimensions only (distinct values)
SELECT * FROM semantic_view('order_metrics',
    dimensions := ['region']
);

-- Metrics only (grand total)
SELECT * FROM semantic_view('order_metrics',
    metrics := ['revenue']
);

-- WHERE works on the result
SELECT * FROM semantic_view('order_metrics',
    dimensions := ['region'], metrics := ['revenue']
) WHERE region = 'East';

-- ...but an outer WHERE filters AFTER aggregation. To scope what a metric
-- measures, filter before it with where_clause:
SELECT * FROM semantic_view('order_metrics',
    dimensions := ['region'], metrics := ['revenue'],
    where_clause := 'category = ''hardware'''
);
```

> **Read-only databases:** queries (`semantic_view`, `SHOW SEMANTIC VIEWS`, `DESCRIBE SEMANTIC VIEW <name>`, etc.) work against a database opened with `read_only=True`. `CREATE` / `DROP` / `ALTER SEMANTIC VIEW` require a writable database. See the [transactional DDL and limitations](https://s-layer.github.io/duckdb-semantic-views/explanation/transactional-ddl-and-limitations.html#read-only-databases) explanation page for the bootstrap-then-reopen workflow.

## Multi-table (PK/FK relationships)

Define relationships between tables with PRIMARY KEY and REFERENCES. Only the tables needed for your requested dimensions and metrics get joined.

```sql
CREATE TABLE customers (id INTEGER, name VARCHAR, tier VARCHAR);
CREATE TABLE products (id INTEGER, name VARCHAR, category VARCHAR);
CREATE TABLE orders (
    id INTEGER, customer_id INTEGER, product_id INTEGER,
    amount DECIMAL(10,2), region VARCHAR
);

CREATE SEMANTIC VIEW analytics AS
TABLES (
    o AS orders PRIMARY KEY (id),
    c AS customers PRIMARY KEY (id),
    p AS products PRIMARY KEY (id)
)
RELATIONSHIPS (
    order_customer AS o(customer_id) REFERENCES c,
    order_product AS o(product_id) REFERENCES p
)
DIMENSIONS (
    c.customer_name AS c.name,
    p.product_name AS p.name,
    o.region AS o.region
)
METRICS (
    o.revenue AS sum(o.amount),
    o.order_count AS count(*)
);

-- Only customers table is joined (products not needed)
SELECT * FROM semantic_view('analytics',
    dimensions := ['customer_name'],
    metrics := ['revenue']
);

-- Both customers and products tables are joined
SELECT * FROM semantic_view('analytics',
    dimensions := ['customer_name', 'product_name'],
    metrics := ['revenue']
);
```

See the generated SQL with `explain_semantic_view`:

```sql
SELECT * FROM explain_semantic_view('analytics',
    dimensions := ['customer_name'],
    metrics := ['revenue']
);
```

```
┌────────────────────────────────────────────────────────────────────────┐
│                             explain_output                           │
│                                varchar                               │
├────────────────────────────────────────────────────────────────────────┤
│ -- Semantic View: analytics                                          │
│ -- Dimensions: customer_name                                         │
│ -- Metrics: revenue                                                  │
│ -- Materialization: none                                             │
│                                                                      │
│ -- Expanded SQL:                                                     │
│ SELECT                                                               │
│     c.name AS "customer_name",                                       │
│     sum(o.amount) AS "revenue"                                       │
│ FROM "memory"."main"."orders" AS "o"                                 │
│ LEFT JOIN "memory"."main"."customers" AS "c"                         │
│     ON "o"."customer_id" = "c"."id"                                  │
│ GROUP BY                                                             │
│     1                                                                │
│                                                                      │
│ -- DuckDB Plan:                                                      │
│ ...                                                                  │
├────────────────────────────────────────────────────────────────────────┤
│ 15+ rows                                                             │
└────────────────────────────────────────────────────────────────────────┘
```

## FACTS (reusable row-level expressions)

Name common row-level calculations once and reference them in metrics. Facts are inlined into metric expressions at expansion time.

> **Clause direction:** like Snowflake, each entry is `alias.<logical_name> AS <sql_expression>` —
> the **name comes before `AS`**, the SQL expression after. This is the reverse of a plain SQL
> `expression AS alias`. The logical name is what you query (`facts := ['net_price']`) and what
> `DESCRIBE` returns as the column. A fact may be named after its own column —
> `FACTS (s.unit_price AS s.unit_price)` defines a passthrough fact `unit_price`. The same
> direction applies to `DIMENSIONS` and `METRICS`.

```sql
CREATE SEMANTIC VIEW sales AS
TABLES (
    li AS line_items PRIMARY KEY (id)
)
FACTS (
    li.net_price AS li.extended_price * (1 - li.discount),
    li.tax_amount AS li.net_price * li.tax_rate
)
DIMENSIONS (
    li.region AS li.region
)
METRICS (
    li.total_net AS SUM(li.net_price),
    li.total_tax AS SUM(li.tax_amount)
);
```

Facts can reference other facts -- the extension resolves them in dependency order.

## Derived metrics (metric composition)

Combine base metrics without table prefixes. The extension substitutes the underlying expressions.

```sql
METRICS (
    li.revenue AS SUM(li.net_price),
    li.cost    AS SUM(li.unit_cost),
    profit     AS revenue - cost,
    margin     AS profit / revenue * 100
);
```

## Cardinality and fan trap detection

Relationship cardinality is **inferred** from the PRIMARY KEY / UNIQUE constraints declared in `TABLES` — you do not annotate it. The rule looks at the *FK side*: if a relationship's FK columns are themselves a PK or UNIQUE key on the table they are declared on, the relationship is one-to-one; otherwise it is many-to-one.

Traversing many-to-one is always safe. A query that would have to traverse a relationship in reverse (one-to-many) to reach a dimension is a fan trap, and the extension raises an error rather than returning an inflated number.

```sql
RELATIONSHIPS (
    li_to_order AS li(order_id) REFERENCES o,
    order_to_customer AS o(customer_id) REFERENCES c
)
```

Here `li(order_id)` is not a key on `line_items`, so `li_to_order` is many-to-one; the same holds for `order_to_customer`. (Explicit `ONE TO ONE` / `ONE TO MANY` / `MANY TO ONE` annotations were removed in v0.5.4 and are now rejected.)

Metrics that merely sit at *different grains from each other* are not an error: since v0.12.0 each is aggregated over its own table and the results are joined on the queried dimensions. See [metric grain](https://s-layer.github.io/duckdb-semantic-views/explanation/metric-grain.html) and [fan traps](https://s-layer.github.io/duckdb-semantic-views/how-to/fan-traps.html).

## Role-playing dimensions (USING RELATIONSHIPS)

When the same table is joined via multiple relationships (e.g., airports as both departure and arrival), use `USING` on metrics to select which join path to use.

```sql
CREATE SEMANTIC VIEW flight_analytics AS
TABLES (
    f AS flights PRIMARY KEY (flight_id),
    a AS airports PRIMARY KEY (airport_code)
)
RELATIONSHIPS (
    dep_airport AS f(departure_code) REFERENCES a,
    arr_airport AS f(arrival_code) REFERENCES a
)
DIMENSIONS (
    a.city    AS a.city,
    f.carrier AS f.carrier
)
METRICS (
    f.departures USING (dep_airport) AS COUNT(*),
    f.arrivals   USING (arr_airport) AS COUNT(*)
);
```

Without `USING`, queries that involve an ambiguous join path will error.

## DDL reference

```sql
-- Full clause order (all clauses after TABLES optional;
-- at least one of DIMENSIONS or METRICS required)
CREATE SEMANTIC VIEW name AS
  TABLES (...)
  RELATIONSHIPS (...)
  FACTS (...)
  DIMENSIONS (...)
  METRICS (...)
  MATERIALIZATIONS (...);

CREATE OR REPLACE SEMANTIC VIEW name AS ...;
CREATE SEMANTIC VIEW IF NOT EXISTS name AS ...;
CREATE SEMANTIC VIEW name FROM YAML '...';       -- define from YAML instead
DROP SEMANTIC VIEW [IF EXISTS] name;

ALTER SEMANTIC VIEW [IF EXISTS] name RENAME TO new_name;
ALTER SEMANTIC VIEW [IF EXISTS] name SET COMMENT = '...';
ALTER SEMANTIC VIEW [IF EXISTS] name UNSET COMMENT;

DESCRIBE SEMANTIC VIEW name;
SHOW SEMANTIC VIEWS;
SHOW SEMANTIC DIMENSIONS [FOR METRIC m] [LIKE '...'] [IN name];
SHOW SEMANTIC METRICS [LIKE '...'] [IN name];
SHOW SEMANTIC FACTS [LIKE '...'] [IN name];
SHOW SEMANTIC MATERIALIZATIONS [IN name];
SHOW COLUMNS IN SEMANTIC VIEW name;

SELECT get_ddl('SEMANTIC_VIEW', 'name');          -- round-trippable DDL
SELECT read_yaml_from_semantic_view('name');      -- YAML export
```

`CREATE`, `DROP` and `ALTER` participate in the surrounding transaction, so `BEGIN ... ROLLBACK` undoes them.

## Other features

Documented in full on the [docs site](https://s-layer.github.io/duckdb-semantic-views/):

- **[Pre-aggregation filtering](https://s-layer.github.io/duckdb-semantic-views/how-to/filtering.html)** -- `where_clause :=` filters rows *before* metrics are aggregated, which an outer `WHERE` cannot express.
- **[Multi-grain queries](https://s-layer.github.io/duckdb-semantic-views/explanation/metric-grain.html)** -- metrics at different grains are each aggregated over their own table and joined on the queried dimensions.
- **[Semi-additive metrics](https://s-layer.github.io/duckdb-semantic-views/how-to/semi-additive-metrics.html)** -- `NON ADDITIVE BY` picks one snapshot row per group before aggregating (balances, inventory levels).
- **[Window metrics](https://s-layer.github.io/duckdb-semantic-views/how-to/window-metrics.html)** -- `OVER (PARTITION BY ...)` for running totals and shares of parent.
- **[Materializations](https://s-layer.github.io/duckdb-semantic-views/how-to/materializations.html)** -- route matching queries to a pre-aggregated table.
- **[YAML definitions](https://s-layer.github.io/duckdb-semantic-views/how-to/yaml-definitions.html)** -- define views from YAML, and export back.
- **[Metadata annotations](https://s-layer.github.io/duckdb-semantic-views/how-to/metadata-annotations.html)** -- `COMMENT`, `WITH SYNONYMS`, `PRIVATE`, and `LABELS = (FILTER)`.
- **[Wildcard selection](https://s-layer.github.io/duckdb-semantic-views/how-to/wildcard-selection.html)** -- `dimensions := ['c.*']`.
- **[Querying facts directly](https://s-layer.github.io/duckdb-semantic-views/how-to/query-facts.html)** -- `facts := [...]` returns row-level values without aggregating.

## Documentation

Full documentation: [s-layer.github.io/duckdb-semantic-views](https://s-layer.github.io/duckdb-semantic-views/)

Includes getting-started and multi-table tutorials, full DDL and query reference, how-to guides for advanced features (FACTS, derived metrics, filtering, role-playing dimensions, fan traps, semi-additive and window metrics, materializations, YAML), and explanation pages covering metric grain, transactional DDL, and feature-by-feature comparisons with Snowflake and Databricks.

## Building

Rust, built on the [DuckDB extension template for Rust](https://github.com/duckdb/extension-template-rs).

You need: Rust (stable), just, make, CMake, Python 3. The first `just build` compiles the DuckDB engine from source, which takes a while; later builds reuse it. See [MAINTAINER.md](MAINTAINER.md) for details.

```bash
just setup     # one-time: installs dev tools, configures build
just build     # debug build
cargo test     # unit + property-based tests
just test-sql  # SQL logic tests (needs just build first)
just test-all  # everything
just lint      # fmt + clippy + cargo-deny
```

## License

MIT
