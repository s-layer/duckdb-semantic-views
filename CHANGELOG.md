# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

<!-- docs-include-start -->

## [Unreleased]

### Added

- **Snowflake DDL pastes as written.** `CREATE SEMANTIC VIEW sales TABLES (...)` works without `AS` before `TABLES`, as Snowflake writes it, including with `OR REPLACE`, `IF NOT EXISTS` and a view `COMMENT` before `TABLES`. `AS` is still accepted. On a fact or dimension, `LABELS = (FILTER)` may sit between the name and `AS` (`o.is_large LABELS = (FILTER) AS o.amount > 100`), Snowflake's placement; the trailing placement after the expression still works, but not both on one entry. `GET_DDL('SEMANTIC VIEW', ...)`, the spelling in Snowflake's `GET_DDL` reference, is accepted alongside `'SEMANTIC_VIEW'`.
- **Paging with `SHOW SEMANTIC VIEWS ... LIMIT n FROM '<name>'`.** As in Snowflake, `FROM` returns the views whose name sorts after `'<name>'` (case-sensitive), and `LIMIT` then applies. Pass the last name of one page to get the next. `SHOW TERSE SEMANTIC VIEWS` takes it too; the other `SHOW SEMANTIC` commands reject it, as Snowflake's do.
- **Snowflake's Cortex Analyst clauses are accepted and stored.** `SAMPLE_VALUES ('East', 'West')` on a fact or dimension, `IS_ENUM` and `WITH CORTEX SEARCH SERVICE db.schema.service USING column` on a dimension, and `MAX_STALENESS`, `AI_SQL_GENERATION '...'`, `AI_QUESTION_CATEGORIZATION '...'`, `AI_VERIFIED_QUERIES (...)` and `COPY GRANTS` on the view are read with Snowflake's syntax, so Snowflake DDL pastes as written. They are metadata: no query result changes. `DESCRIBE` shows them, with Snowflake's `CUSTOM_INSTRUCTIONS` and `AI_VERIFIED_QUERY` rows for the view-level ones, and `GET_DDL` and the YAML export write them back (the YAML uses Snowflake's field names, such as `sample_values` and `verified_queries`). `MAX_STALENESS` takes whole seconds or a quoted interval such as `'1 hour'`, at least 120 seconds. `COPY GRANTS` is accepted and ignored. The trailing `WITH EXTENSION (CA = '<json>')` that Snowflake's `GET_DDL` can add is not accepted (it is not part of Snowflake's `CREATE` syntax and its format is undocumented); the error names it, and its sample values can be written as `SAMPLE_VALUES`.
- **`ALTER SEMANTIC VIEW ... SET MAX_STALENESS` and `UNSET MAX_STALENESS`.** `UNSET` now takes a list of properties, as in Snowflake: `ALTER SEMANTIC VIEW sales UNSET COMMENT, MAX_STALENESS`.
- **`ALTER SEMANTIC VIEW ... SET TAG` and `UNSET TAG`.** `ALTER SEMANTIC VIEW [IF EXISTS] sales SET TAG owner = 'finance', tier = 'gold'` adds tags to the view or replaces their values, and `UNSET TAG tier` removes them. Tag names match case-insensitively, and unsetting a tag the view does not carry is not an error. As in Snowflake, `ALTER` changes the view's own tags only. `DESCRIBE` and `GET_DDL` show the change.
- **Metrics with different `USING` paths can be queried together.** Following Snowflake's multi-path relationships, departures and arrivals can be grouped by city in one query: `DIMENSIONS city METRICS departures, arrivals`, where `departures` is `USING (dep_airport)` and `arrivals` is `USING (arr_airport)`. Each metric is grouped through its own relationship and the results are merged on the queried dimensions, so a city with arrivals but no departures shows `NULL` departures. A derived metric over such metrics (`movements AS departures + arrivals`) is computed from the merged parts, and a window metric reads its inner metric through that metric's path. These queries used to be refused as ambiguous.
- **A dimension past a role-played table follows the metric's `USING`.** As in Snowflake, a dimension on a table reached only through a role-played one, such as a region on `regions` referenced by `airports`, takes the path each metric names: `DIMENSIONS region METRICS departures` groups by the departure airport's region, and `departures, arrivals` by region are merged per region like by city. A metric with no `USING` is still refused as ambiguous. These queries used to be refused whatever the metrics named.
- **A metric can be written over another metric on its own table.** As in Snowflake, a table-qualified metric's expression can name other metrics on the same table, which is how Snowflake writes one metric per role: `f.departures USING (dep_airport) AS f.flight_count` over `f.flight_count AS COUNT(*)`. The referenced aggregate is computed through the referencing metric's own `USING` path. A name inside a call to a built-in aggregate (its arguments, `FILTER` and `WITHIN GROUP`) or a lambda parameter is still a column, so existing views keep their meaning. `CREATE` refuses a reference to a window metric, a semi-additive metric or a metric with its own `USING`, and a cycle of references.
- **Tables defined by a query.** As in Snowflake, a `TABLES` entry can be a query instead of a physical table: `customer_info AS (SELECT * FROM customer JOIN customer_address ON ...) PRIMARY KEY (c_cust_id)`. The alias is required, and keys, relationships, comments, synonyms and tags work as on a physical table. Every query of the view reads the table through its query, which may join, filter or aggregate. The query is bound at `CREATE`, and its table names resolve in the schema the view was created in, as in a view body. `DESCRIBE` shows it in a `DEFINITION` row in place of the `BASE_TABLE_*` rows, `GET_DDL` writes it back, and the YAML form carries it as `definition:` in place of `table:`. A view with such a table cannot be read by an earlier version of the extension, which reports its stored definition as invalid (`missing field 'table'`).
- **View variables.** As in Snowflake, a `VARIABLES` clause between `RELATIONSHIPS` and `FACTS` declares named values that facts, dimensions and metrics read by bare name: `VARIABLES (min_amount INTEGER DEFAULT 1000, multiplier DOUBLE := 1.0, target_region VARCHAR COMMENT 'the region to report on')`. Each entry is `name [type] [{DEFAULT | :=} <expr>] [COMMENT '...']`; a default is an expression each query evaluates, such as `current_date`, cast to the type. A query sets values with `VARIABLES min_amount = 2000, target_region => 'West'` as the last clause of `SEMANTIC_VIEW(...)`, or with `variables := {'min_amount': 2000}` in `semantic_view()` and `explain_semantic_view()`, and a value can be a prepared-statement parameter. The query's `WHERE` and expression items read variables too. A variable left unset takes its default, and one with no default must be set by every query of the view, as in Snowflake. An unqualified name is the variable and a qualified one (`sales.amount`) the column. `CREATE` refuses a variable named like a fact, dimension or metric, a list, `STRUCT`, `MAP`, `UNION`, `VARIANT` or `JSON` type, a default with a subquery or parameter, a variable in a window frame bound, and a variable that does not bind. A standard-SQL query over the view cannot set variables, so defaults apply. A materialization covering a member that reads a variable is not used for routing. `DESCRIBE` shows `VARIABLE` rows, `SHOW SEMANTIC DIMENSIONS`, `METRICS` and `FACTS` type such members with the variable's type, and `GET_DDL` and the YAML form (`variables:`) write them back.

### Changed

- **Every metric grouped by a role-played dimension needs its own `USING`.** A metric without `USING` used to take the path of a co-queried metric that had one, so `DIMENSIONS city METRICS departures, flight_count` grouped `flight_count` by departure city, and the same metric gave different numbers depending on what it was queried with. It is now refused with `dimension 'city' is ambiguous`, as Snowflake refuses it. Give the metric its own `USING`, for example `f.flight_count_out USING (dep_airport) AS f.flight_count`. A metric on a table that has no relationship to the role-played table, such as a count of flight legs grouped by departure city, cannot name a path, so that query is refused too, as in Snowflake. The error's advice now reads `Give every metric in the query a USING that names one of them`, and says to query the metrics separately when they already name different paths in a query that cannot be split by path (two window metrics, or metrics beside a semi-additive one). Such a query that also has metrics at several grains now reports this error rather than `fan trap detected`.
- **`SHOW SEMANTIC DIMENSIONS` and `SHOW SEMANTIC FACTS` have a `labels` column.** A ninth column, after `comment`, holds the member's labels as a JSON array: `["FILTER"]` for a named filter, `[]` otherwise, as in Snowflake. `SHOW SEMANTIC METRICS` is unchanged. Code that reads these commands by column position, or `SELECT *` into a fixed-width table, needs updating.
- **`SHOW SEMANTIC DIMENSIONS ... FOR METRIC` has `synonyms` and `comment` columns.** They follow `required`, as in Snowflake: `synonyms` is a JSON array (`[]` when none) and `comment` is an empty string when none is set.
- **`GET_DDL` writes `LABELS` before `AS`.** A named filter renders as `o.is_large LABELS = (FILTER) AS o.amount > 100`, Snowflake's placement, instead of after the expression. Both forms re-create the same view.
- **Reworded error for a `CREATE` body that is not recognized.** Since `AS` is optional, it now reads `Expected 'AS', 'TABLES' or 'FROM YAML' after view name. Use: CREATE SEMANTIC VIEW name [AS] TABLES (...) ...`.
- **A member is read in the clause it is listed in**, following Snowflake's `2026_07` behaviour change. A query that lists facts in `FACTS` and dimensions in `DIMENSIONS` (`facts := [...]` with `dimensions := [...]`) is now refused with `querying FACTS with DIMENSIONS is not supported`; list the dimensions in `FACTS` instead, where a dimension is read row by row as a column of the fact rows, in the order listed (`FACTS region, net_price`). A fact listed in `DIMENSIONS` is grouped on, like a dimension, so it gives one row per distinct value and can be queried with metrics. In a standard SQL query, a fact in `SELECT` that a `GROUP BY` query neither groups nor aggregates is now refused with `'<fact>' must be passed to an aggregate or appear in GROUP BY`, as in any SQL, rather than read row by row.

### Fixed

- **A table aliased `labels` or `comment` can be read in a member's expression.** `DIMENSIONS (labels.region AS labels.region)` failed because the `labels.` qualifier was taken for the start of a `LABELS = (...)` annotation (and `comment.` for `COMMENT = ...`). A keyword followed by a `.` is now read as a name.
- **A fact can be named by its table in `FACTS`.** `FACTS o.amount` (and `facts := ['o.amount']`) failed with an unknown-fact error; a table-qualified fact name now resolves, as a qualified dimension or metric name does.

## [0.17.0] - 2026-10-26

### Added

- **Standard SQL queries.** Query a semantic view by name with standard SQL, as in Snowflake: `SELECT region, AGG(revenue) FROM analytics WHERE channel = 'web' GROUP BY region`. `GROUP BY` expressions (by name, position, `SELECT` alias or `GROUP BY ALL`) are the dimensions (a grouped fact gives one row per value), each metric is passed to `AGG` (`MIN`, `MAX` and `ANY_VALUE` of a metric mean the same), any other aggregate is an ad-hoc metric, and a `SELECT` item that is neither grouped nor aggregated is a fact. `WHERE` filters before aggregation and `HAVING` after it. The query turns into the `SEMANTIC_VIEW(...)` construct, so results, checks and errors are the construct's. It works anywhere a query does (subqueries, CTEs, `UNION`, `CREATE TABLE AS`, `INSERT`, `UPDATE`, `DELETE`, `SET VARIABLE`, `COPY`, `EXPLAIN`, `DESCRIBE`, prepared statements, `CREATE VIEW`), in any clause that holds a subquery, and from Python through `duckdb.sql()`, `con.sql()` and `con.execute()`, including a relation read by its variable name in a later query. Following Snowflake's rules, a metric outside `AGG`, a dimension missing from `GROUP BY`, `HAVING` over anything but aggregated metrics, a qualified reference to an enclosing query, and `SELECT *`, joins, `PIVOT`, window functions, `QUALIFY` and `GROUPING SETS` in the query block that reads the view are refused with an error naming the view. A name that is a table, a view, a CTE in scope or a Python DataFrame or relation keeps its usual meaning. Two rules are this extension's own: `COUNT(*)` alone is refused, since it reads no member to say which table's rows it counts (declare a `COUNT(*)` metric on a table and pass it to `AGG`); and a bare name in `WHERE` that is no member is passed to DuckDB as written, as a raw column in the construct's `WHERE` is, so when no table of the view has a column of that name it binds to the enclosing query.
- **Tags.** As in Snowflake, `[WITH] TAG (name = 'value', ...)` can be written on a logical table, a fact, a dimension or a metric, and on the view itself after its last clause. A tag name may be qualified (`schema.tag`, `database.schema.tag`) and is stored as written, since DuckDB has no tag objects; a value is a string of up to 256 characters. `DESCRIBE SEMANTIC VIEW` shows each object's tags as a `TAGS` row holding a JSON object, and `GET_DDL` and the YAML export write them back (YAML in Snowflake's `tags: [{name: {tag: ...}, value: ...}]` shape, accepted by `FROM YAML` too). Tags are metadata only and `SHOW` commands don't list them, matching Snowflake.

### Changed

- **`CREATE SEMANTIC VIEW` binds every member.** Each dimension, fact and metric,
  `PRIVATE` ones included, is bound against the view's tables when the view is created,
  as Snowflake compiles each expression at `CREATE`. A member DuckDB cannot bind (an
  unknown column, table or function, a syntax error, or an argument such as
  `date_trunc('fortnight', ...)`) now refuses the `CREATE` with
  `semantic view '<view>': <kind> '<name>' does not bind: <DuckDB error>`, instead of
  being stored and failing only when a query reached it. Every form is checked: `OR
  REPLACE`, `IF NOT EXISTS`, `FROM YAML` and `FROM YAML FILE`. A `CREATE` that would
  change nothing binds nothing: `IF NOT EXISTS` on a view that exists stays a no-op, and
  a plain `CREATE` of one still reports that it already exists. The bind runs in the
  creating session, so temporary tables and tables created earlier in the same
  transaction are visible. Every table in `TABLES` must exist, including one no member
  reads (`table '<alias>' does not bind`). Materialization tables are not checked. A view created
  under 0.16.0 with a member that does not bind keeps working as before until it is
  re-created; replaying its `GET_DDL` or YAML export is then refused with the same error,
  so fix the member before replaying.

### Fixed

- **Fact queries over two referenced tables.** A fact query whose dimensions are on two tables that the fact's table references, such as a fact on `orders` with a dimension on `customers` and one on `products`, runs. It was refused with "incompatible table paths", though each order joins one customer and one product. A query that reaches those two tables only through a table it does not read is still refused, as its rows would repeat.
- **Quoted key columns and table aliases.** A view whose `PRIMARY KEY`, relationship
  columns or table alias were written quoted (`PRIMARY KEY ("Key Col")`,
  `"Ord Lines" AS orders`) quoted them twice in the generated SQL, so nearly every query
  of the view failed with a "not found" error.
- **A `USING` semi-additive metric on a role-played table.** Queried without its
  `NON ADDITIVE BY` dimension, a semi-additive metric whose snapshot dimension sits on a
  table reached through its `USING` relationship failed with `Referenced table
  "<table>__<rel>" not found`.
- **A relationship named `AS` in `GET_DDL`.** A view imported from YAML with a
  relationship named `as` rendered DDL that did not parse back (`as AS o(fk)
  REFERENCES c`). The name is now quoted.

## [0.16.0] - 2026-10-26

### Added

- **`SHOW` and `DESCRIBE` statements work as subqueries.** `DESCRIBE SEMANTIC VIEW`,
  `SHOW [TERSE] SEMANTIC VIEWS`, `SHOW SEMANTIC DIMENSIONS / METRICS / FACTS /
  MATERIALIZATIONS` (with `FOR METRIC` too) and `SHOW COLUMNS IN SEMANTIC VIEW` can now be
  used wherever DuckDB accepts a query: in `FROM`, in a CTE or in a view body, so you can
  filter, join and aggregate their output, as in
  `SELECT * FROM (DESCRIBE SEMANTIC VIEW v) WHERE object_kind = 'METRIC'`.

- **Semantic-view DDL can share a query string with other statements.** One `execute()`
  call can now hold, say, `CREATE TABLE …; INSERT …; CREATE SEMANTIC VIEW …`: DuckDB
  splits the string and runs each statement in order. Before, any `CREATE`, `DROP` or
  `ALTER SEMANTIC VIEW` in such a string was refused with "semantic-view DDL must be sent
  as its own statement, one statement per call".

- **The `SEMANTIC_VIEW(...)` query construct.** You can query a semantic view the way
  Snowflake does, in `FROM`:
  `SELECT * FROM SEMANTIC_VIEW(sales DIMENSIONS c.region AS r METRICS o.revenue WHERE o.year = 2024)`.
  It stands wherever a table can: in joins, CTEs, subqueries, `PIVOT` and view bodies, with
  a table alias and column aliases. Clauses can come in any order, and the order you write
  them in is the order of the columns. `WHERE` filters the rows the metrics aggregate, as
  `where_clause` does. A view over the construct expands against the semantic view's
  current definition each time it is queried. A prepared statement's
  parameter (`?`, `$1`, `$name`) in its `WHERE` is one of the statement's parameters, as
  in `PREPARE p AS SELECT * FROM SEMANTIC_VIEW(sales METRICS o.revenue WHERE o.year = ?)`.

- **Expressions as query items.** A `DIMENSIONS`, `METRICS` or `FACTS` item, in the
  construct or in a `semantic_view()` list, can be an expression over the view's members,
  as in Snowflake:
  `SEMANTIC_VIEW(sales DIMENSIONS DATE_PART('year', o.order_date) AS year METRICS o.revenue / o.orders AS avg_order, max(o.amount) AS biggest)`.
  A dimension or fact expression uses dimensions and facts of one table. A metric
  expression uses metrics, and aggregates of the dimensions and facts of one table, each
  computed over its own table. An expression item is grouped, joined and checked for fan
  traps as a member declared on its table would be, so `COUNT(DISTINCT ...)`, ratios and
  metrics of different tables come out right. An expression reads only the view's members:
  any other name is refused, so an item cannot read a column the view does not declare,
  such as the one under a `PRIVATE` fact. Keywords, numbers, lambda parameters, named
  arguments and collations are fine. Without an alias, the column is named by the
  expression as written.

- **Output names and column order for `semantic_view()`.** A `dimensions`, `metrics` or
  `facts` item can name its output column with `AS`, as in `'o.region AS r'`, and the new
  `column_order` parameter (for example `['metrics', 'dimensions']`) sets which list's
  columns come first. Two columns with the same name are an error.

- **Values for `where_clause` in `params`.** `semantic_view()` and `explain_semantic_view()`
  take a `params` row, and `where_clause` refers to its values as `$1`, `$2`, ..., as in
  `where_clause := 'o.year = $1', params := row(2024)`. Each value goes into the predicate
  as a value of its own type, never as SQL text, so user-supplied values no longer need to
  be quoted into the predicate.

### Changed

- **Semantic-view statements are part of DuckDB's own grammar.** The extension no longer
  uses DuckDB's parser-override hook, so `allow_parser_override_extension` no longer
  affects semantic-view statements, and they parse the same way at the top of a query
  string, after other statements and as a subquery. What you can see of this:
  - A statement the grammar cannot match, such as one with a misspelt keyword
    (`CREAT SEMANTIC VIEW`), a missing view name or an `ALTER` with no operation, gets
    DuckDB's own `syntax error at or near "…"` with its `LINE 1: … ^` pointer, in place
    of the extension's messages ("Did you mean 'CREATE SEMANTIC VIEW'?", "Missing view
    name", "Missing ALTER operation" and the like). Errors the extension raises from a
    statement it does match, such as an unknown table alias, keep their messages.
  - A view name that is a DuckDB reserved word, such as `select`, `order` or `left`, has
    to be quoted in every statement, `CREATE` included, as DuckDB requires for its own
    names: `CREATE SEMANTIC VIEW "select" AS …`. `CREATE` used to accept it unquoted.
    `GET_DDL` now quotes such a name, so its output runs as written. It also quotes a
    member or table alias named `AS` or `USING`, which it used to write bare
    (`o.AS AS o.region`), so that `CREATE` split it at the wrong `AS`.
  - `CREATE SEMANTIC VIEW` stores each expression as DuckDB's tokens spell it: a tab
    becomes a space, a run of blank lines one line break, and comments are dropped. So
    `GET_DDL` and `DESCRIBE` show that spelling. The query a view expands to is the same.
  - Loading the extension no longer switches other open connections over from the
    loading connection's thread, which could race with a query they were parsing.
    Each one now switches itself when it next starts or finishes a query (see Known
    limitations).

- **Two query errors name both query forms.** A query with no items now reads
  `specify at least one of DIMENSIONS, METRICS or FACTS (dimensions := [...], metrics := [...] or facts := [...]).`,
  and one with both facts and metrics ends `Use FACTS or METRICS (facts := [...] or metrics := [...]), not both.`,
  instead of naming only the `semantic_view()` arguments.

- **The unclosed-bracket error names its clause like the others.** It now reads
  `Unclosed '(' in clause 'METRICS'.`, as the other bracket and quote errors for a clause
  do, instead of `... for clause 'METRICS'.`

- **`GET_DDL` and `READ_YAML_FROM_SEMANTIC_VIEW` follow `search_path`.** An unqualified
  view name now resolves through the session's `search_path`, as it already did for the
  DDL statements and `semantic_view()`: when several schemas hold a view of that name,
  the first one on the path wins, where before these two raised an ambiguity error
  whatever the path said. Called from DuckDB's Python relational API, they follow the
  path too, instead of requiring a unique match.

- **CREATE rejects an empty view.** A view with no tables (`TABLES ()`, or a YAML
  definition with `tables: []` or no `tables`), or whose `DIMENSIONS` and `METRICS`
  clauses are all empty (`DIMENSIONS () METRICS ()`, a lone `METRICS ()`), is now refused at
  `CREATE SEMANTIC VIEW`, as Snowflake does. Before, such a view was created but could not
  be queried, and `GET_DDL` either failed on it or returned DDL that `CREATE` refused. A
  YAML export of a view stored in the legacy format, which lists no tables, can no longer be
  imported either; re-create that view with `CREATE OR REPLACE SEMANTIC VIEW`.

- **CREATE rejects an aggregate in a dimension or fact, and a parameter in any member.** A
  dimension such as `o.revenue AS SUM(o.amount)` is now refused, as Snowflake does: a
  dimension is a row-level value that queries group by, so no query could use it. A fact
  is refused the same way, as the CREATE reference already said it was. Define a metric
  instead. An aggregate inside a scalar subquery, as in `(SELECT max(t) FROM tiers)`, and a
  windowed aggregate, as in `SUM(o.amount) OVER (PARTITION BY o.region)`, are still accepted.
  The check knows every DuckDB aggregate (`arg_max`, `first`, `list`, `quantile_cont` and
  the rest), and a derived metric now refuses those too. A prepared statement parameter
  (`?`, `$1`, `$name`) in a dimension, fact or metric, including a window metric's
  arguments, `ORDER BY` and frame, is refused too: it has no value inside the view, and
  before, every query that used the member failed. Put parameters in the query's `WHERE` clause instead. A view
  stored before this release with either shape still fails at query time as it did.

- **A member named with the word `as` has to be quoted in a `semantic_view()` list item.**
  Since an item can now give its column an alias with `AS`, a member whose name contains
  the word `as` on its own (`Sales as of`) has to be quoted there (`'"Sales as of"'`).
  Names with spaces otherwise work unquoted as before.

- **A member name inside a `where_clause` subquery is the subquery's own.** Before, a
  member could be named inside a subquery to correlate it with the outer query, as in
  `EXISTS (SELECT 1 FROM t WHERE t.k = region)`. Now `region` there refers to the
  subquery's own tables, as in Snowflake, where the `WHERE` clause takes uncorrelated
  subqueries. A subquery sees none of the semantic view's tables, so a name its own tables
  lack is DuckDB's `Referenced column ... not found` error. Write a condition on the member
  outside the subquery. This also fixes a silent wrong filter: a name inside a subquery,
  such as `amt` in `amt IN (SELECT amt FROM targets)`, was replaced with the member of that
  name, so the subquery compared each row with itself and filtered nothing, or failed to
  parse.

- **`SET max_execution_time` counts from when a `semantic_view()` query starts running.**
  As with DuckDB's own queries, time DuckDB spends planning a source that is slow to bind,
  such as a long list of files, is not limited.

- **`semantic_view()` runs as part of your query.** DuckDB now plans the semantic view's
  expanded SQL inside the query that calls it, instead of the extension running it
  separately and copying the rows across. The rows stream instead of being collected
  first, scans run in parallel, and DuckDB can push the query's filters and column
  choices down into it. Three things you can see change:
  - **Column types are DuckDB's own.** `SUM` over an integer column is a `HUGEINT` (it was
    narrowed to `BIGINT`), and `ENUM`, `STRUCT`, `MAP` and `UHUGEINT` columns keep their
    types (they were turned into `VARCHAR` and `UBIGINT`). The `data_type` that `SHOW` and
    `DESCRIBE` report follows. Cast in your query if you relied on the old types.
  - **Errors in the expanded SQL are DuckDB's.** A metric over a dropped column, or a cast
    that fails on the data, now fails with DuckDB's own error, with its caret on the
    `semantic_view(...)` call. The `semantic_view: SQL execution failed: ... (expanded SQL:
    ...)` and `type inference failed for query` wrappers are gone; `explain_semantic_view`
    still shows the SQL.
  - **`where_clause` can use a column of an enclosing query**, as in
    `FROM t, LATERAL (SELECT * FROM semantic_view('v', metrics := ['revenue'], where_clause := 'region = t.r'))`.

### Fixed

- **A `where_clause` parameter written straight against a `$` is refused.** In
  `k = $3$$`, the `$` after `$3` continues the token, as it does in a name, so the token is
  no `$<n>` and the query is refused as holding a parameter it cannot fill. It used to
  take `$3` as a parameter all the same, which left the `$$` free to open a dollar-quoted
  string and changed what the rest of the predicate meant.

- **A long error message reaches you whole.** An error from a semantic-view statement
  used to be cut off after 1023 bytes, so a message quoting a long name lost its end.

- **A registry build no longer fails on one network error while it installs Rust.** The
  build installs the pinned Rust toolchain itself before compiling, retrying a failed
  download, instead of leaving it to a single attempt inside `cargo build`. v0.15.0's
  macOS (Apple silicon) build in the community registry failed that way, so `INSTALL
  semantic_views` found no v0.15.0 binary for DuckDB 2.0 on any platform.

- **A YAML member expression holding a bare `COMMENT`, `LABELS` or `WITH SYNONYMS`, or a
  metric expression naming a bare `USING`, is refused.** In DDL those words, outside
  brackets and quotes, end a member's expression and start its annotations (`USING`, after
  a metric's `AS`, opens its relationship list), so such a YAML view imported and then gave
  a `GET_DDL` that `CREATE` refused. Quote the word if it names a column (`o."comment"`);
  `o.comment` and the word inside brackets or a string are unaffected.

- **A `$` inside a name is part of the name.** A column such as `price$usd$` used in a
  semantic view's expression was read as the start of a `$usd$ … $usd$` string, so
  `CREATE` failed with an unrelated syntax error. As in DuckDB, a `$` right after a
  letter, digit or `_` now continues the name, and a dollar-quoted string opens only at
  the start of a token. A fact named `price` or `usd` is not mistaken for part of it.

- **An unquoted bracket in a name is refused.** `CREATE` accepted a dimension, fact, metric
  or relationship name such as `o.(region)`, though DuckDB never reads a bracket as part of
  a name, and an unbalanced one (`o.(region AS …`) silently pulled the following entries
  into that one. `CREATE` now says to quote the name (`o."(region)"`).

- **`semantic_view()` sees your open transaction.** Rows you insert, update or delete
  inside a transaction are now counted by a semantic-view query in that transaction.
  Before, the query ran on a connection of its own and saw only committed data.

- **Semantic-view reads see your open transaction's DDL.** A semantic view you create,
  drop, rename or alter inside a transaction now reads that way in the transaction before
  you commit: `semantic_view()`, `explain_semantic_view()`, `SHOW`, `DESCRIBE`, `GET_DDL`
  and `READ_YAML_FROM_SEMANTIC_VIEW` all see it, and other connections still see only
  what is committed. Before, every read saw the last committed definitions, so a view
  created in an open transaction could not be queried or described until you committed.
  The member types `SHOW` and `DESCRIBE` report, and the plan `explain_semantic_view()`
  shows, follow a source table changed in the transaction too. These reads scan the
  catalog table directly, so it must be a DuckDB table: if the primary database comes
  from a storage extension instead, reads report that `semantic_layer._definitions` is
  not a DuckDB table.

- **A semantic view over a temporary table can be queried** in the session that has the
  table. Each table in `TABLES` is the one CREATE found: a temporary table if that is what
  its name meant then, otherwise the table in the database and schema the view was created
  in. Before, the query could not see temporary tables at all. `SHOW` and `DESCRIBE`
  report the members' types for such a view, and `explain_semantic_view` shows its plan.

- **A prepared `semantic_view()` query follows a later `SET search_path`.** `EXECUTE`
  now looks the view up on the search path in effect when it runs, as DuckDB does for
  its own tables. Before, it kept the view found when the statement was prepared.

### Known limitations

- **A connection open before another one runs `LOAD semantic_views` needs one ordinary
  statement first.** Until it starts or finishes a query, it parses without the
  semantic-view statements, so `CREATE` / `SHOW SEMANTIC …` and the rest fail there with
  a syntax error, and a statement that fails to parse never starts a query, so retrying
  it fails the same way. Running any statement DuckDB parses on its own, such as
  `LOAD semantic_views` or `SELECT 1`, switches that connection over. Connections opened
  after the `LOAD`, and the connection that ran it, are not affected.

## [0.15.0] - 2026-10-21

### Added

- **`SET max_execution_time` stops semantic-view queries.** DuckDB 2.0's per-query time
  limit now applies to `semantic_view()` and to the `SHOW` / `DESCRIBE` commands that look
  up member types, which fail with DuckDB's own `Query exceeded maximum execution time`
  like any other query instead of running past the limit.

### Changed

- **The extension now runs on DuckDB 2.0, and only there.** This release is built
  against DuckDB `v2.0.0` and loads only in that exact DuckDB release. If you stay on
  DuckDB 1.5, keep v0.14.1, the last release for 1.5.
- **Two identifier errors now come from DuckDB.** In semantic-view DDL, an unterminated
  quoted identifier and an empty one (`""`) are now rejected by DuckDB 2.0 before the
  extension sees the statement. The errors read `unterminated quoted identifier` and
  `zero-length delimited identifier` instead of the extension's own wording.

### Fixed

- **Brackets in semantic-view DDL must pair up by kind.** `CREATE SEMANTIC VIEW` now refuses
  a clause where a closing bracket doesn't match the one it closes (a `]` or `}` closing a
  `(`), or where one is never opened or never closed, with an error that names the bracket
  and the clause. Such a body used to be accepted, and could be split into members
  differently from how it was written, so `GET_DDL` rendered back a different view.
  `CREATE SEMANTIC VIEW ... FROM YAML` refuses a member expression with such brackets for
  the same reason, and `GET_DDL` quotes a column name that has them. A view created before
  0.15.0 with such brackets still loads, but `CREATE` refuses its `GET_DDL` output, so
  correct the brackets before you re-create it from that output.

### Known limitations

- **ADBC needs `import duckdb` first.** With the DuckDB 2.0 Python wheel, a connection from
  `adbc_driver_duckdb` cannot `INSTALL` or `LOAD` any extension, this one included, unless
  the `duckdb` Python module has been imported in the same process first. Add
  `import duckdb` before you import the ADBC driver.

## [0.14.2] - 2026-10-25

### Fixed

- **The extension can be installed from the community registry on DuckDB 1.5.6 again.** The
  registry's Windows build of v0.14.1 failed, and the registry publishes a release only when
  every platform builds, so `INSTALL semantic_views` on DuckDB 1.5.6 still fetched v0.13.0 on
  every platform. The registry generates DuckDB's single-file source on
  Windows with Windows line endings, which the build's Windows-only source patch did not
  expect. The build now accepts either line ending. This release is for DuckDB 1.5 only;
  DuckDB 2.0 users stay on 0.15.0 and later.

## [0.14.1] - 2026-10-16

### Changed

- **Loading the extension into the wrong DuckDB release now fails with a clear message
  instead of crashing.** The extension now uses DuckDB's stable C extension API and checks
  the DuckDB version itself when it loads. A build for one DuckDB release loaded into a newer
  one stops with `semantic_views was built for DuckDB X and cannot run in DuckDB Y`. When the
  running DuckDB is a tagged release, the message also suggests
  `FORCE INSTALL semantic_views FROM community;`, which fetches the matching build if one has
  been published. See *Error messages* in the docs.

### Fixed

- **Windows: an error from a `SHOW` or `DESCRIBE` command, or from `list_semantic_views()`,
  no longer crashes DuckDB later.** On Windows these commands skipped the extension's
  cleanup when they raised an error (for example, naming a view that does not exist). In
  v0.14.0 that left a background thread running against freed memory, which crashed the
  process with an access violation soon afterwards. Errors now clean up as they do on Linux
  and macOS.
- **A member named like a SQL type or keyword no longer breaks expressions that use that
  word.** With a fact named `blob`, a `where_clause` such as `blob = '\x41'::BLOB` failed
  with a parser error, because the `BLOB` type was replaced with the fact's expression. The
  same happened to the type in `CAST(… AS type)`, the type of a literal written as
  `DATE '2020-01-01'`, the field in `EXTRACT(year FROM …)`, the unit in `INTERVAL 1 DAY`, and
  `AT TIME ZONE`, whenever a dimension, fact or metric had that name. This affected
  `where_clause` predicates and member expressions in a view definition, such as a
  `ts::DATE` dimension in a view with a fact named `date`. These words are now left alone.

## [0.14.0] - 2026-10-15

### Changed

- **Every metric is now aggregated from its own table, so a metric on a child table can
  return different numbers when queried on its own.** Following Snowflake, a metric such as
  `li.total_qty AS SUM(li.qty)` on a line-items table is computed from the line items
  themselves and then joined out to the queried dimensions, rather than from the base table
  `LEFT JOIN`ed to its children. Two things change for a child-table metric queried by
  itself: a child row whose foreign key is NULL is now counted, under a NULL dimension
  value, where it used to be left out; and a parent with no children no longer gets a row
  (it used to get `0` for a count and `NULL` otherwise). When a parent-table metric is
  queried alongside, the childless parent's row is still there, with the child metric
  `NULL`. `COUNT(*)` or `COUNT(1)` on a child table with no `PRIMARY KEY` is now answered
  instead of raising an error. Dimensions-only and `facts := [...]` queries are unchanged.
  A member expression that uses an unqualified column belonging to *another* table (which
  Snowflake rejects) may now fail with a "not found" error in queries where that table is
  no longer joined; qualify the column or define a fact on the other table.
  See *Metric grain* in the docs for the full rules.
- **`CREATE SEMANTIC VIEW` rejects two relationships with the same name.** Relationship
  names are one namespace per view, compared the way DuckDB compares identifiers, so `r`,
  `R` and `"r"` are the same name. Two relationships sharing a name used to be accepted
  unless both pointed at the same table, and `USING (r)` then silently meant whichever
  was declared first. `CREATE` (and `CREATE … FROM YAML`) now fails with
  `duplicate relationship name '<name>'`, naming both relationships. A role-playing pair
  that repeats a name reports this error instead of the former "diamond" error. A name
  keeps its parts: the quoted `"x.y"` is one identifier and a different name from the
  two-part `x.y` (also spelled `"x"."y"`), and `USING` matches it the same way. Two
  role-playing relationships to one table whose names differ only that way are rejected,
  because both would be joined under the same alias. Views already stored keep loading,
  and a `USING` in them keeps naming the relationship it named before, including in a
  view that holds two relationships whose names differ only in quoting.
- **`CREATE SEMANTIC VIEW` rejects a single-table member qualified by an undeclared
  table alias.** In a view whose only table is `o`, a dimension or metric named
  `x.foo` was accepted. It now fails with `unknown source table 'x'; did you mean 'o'?`,
  the error a multi-table view already gave. An unqualified member, or one qualified by
  the declared alias in any case or quoting (`o`, `O`, `"O"`), is still accepted. Views
  already stored keep loading.

### Fixed

- **A metric's value no longer depends on which other metrics are queried with it.**
  Adding a metric from another table to a query used to switch a child-table metric over to
  per-table aggregation, so the same metric could return, for example, `12` on its own and
  `112` next to a parent-table metric (the difference being line items with no order), or
  `0` on its own and `NULL` co-queried for an order with no line items. Both queries now
  return the same value. This also holds for derived, semi-additive (`NON ADDITIVE BY`) and
  window metrics over a child-table metric, and for `where_clause` filters on parent or
  child members.
- **Grouping a child-table metric by a child-table dimension no longer produces an extra
  `NULL` group** for parents with no children.
- **`SHOW SEMANTIC DIMENSIONS` / `METRICS` / `FACTS`, `SHOW COLUMNS IN SEMANTIC VIEW`,
  `SHOW SEMANTIC DIMENSIONS FOR METRIC` and `DESCRIBE SEMANTIC VIEW` report each member's
  data type again.** The `data_type` column (and the `DATA_TYPE` rows of `DESCRIBE`) has been
  empty for every view created since v0.10.0. It now reports the type the member's column
  has in a `semantic_view()` query, in DuckDB's spelling: an aggregate's own type rather than
  its input's (`COUNT` is `BIGINT`, `AVG` is `DOUBLE`, `SUM` of an integer column is `BIGINT`,
  as the query returns it), for dimensions, facts, derived, window, semi-additive, PRIVATE
  and role-played members alike. The type is worked out when the command runs, so it follows a source column
  whose type changes after `CREATE`, once that change is committed. A member whose source table or column no longer exists
  reports an empty `data_type` instead of making the whole command fail. The cross-view forms
  (`SHOW SEMANTIC METRICS` without `IN`) take about a millisecond or two longer per view
  over tables in the local catalog; a view over remote files (an S3 glob, `read_csv`) binds
  those sources to type its members and can take noticeably longer.
  Cancelling one of these commands (Ctrl-C, `interrupt()`, an ADBC cancel) stops the type
  lookup promptly with an `INTERRUPT` error, rather than once every view has been typed.
- **`SHOW SEMANTIC DIMENSIONS FOR METRIC` marks a window metric's dimensions `required` however
  they are spelled.** A dimension named in the window's `ORDER BY` or `PARTITION BY [EXCLUDING]`
  as `alias.name` (`ORDER BY s.placed`) or quoted in a different case (`ORDER BY "PLACED"`) was
  reported `required = false`, although querying the metric without it fails.
- **A `USING` metric can aggregate a fact on the role-playing table.** A metric such as
  `f.dep_elev USING (dep_airport) AS SUM(a.elev)`, where `elev` is a fact on the
  role-played `airports` table, used to fail with `Referenced table "a" not found`. It now
  reads the fact from the instance of the table its `USING` names, so each role returns its
  own number — also when the metric is the inner aggregate of a window metric, is
  semi-additive, or spells its relationship more than once in a different case
  (`USING (dep_airport, DEP_AIRPORT)`).
- **A member that reaches a fact on a role-playing table without naming a role is rejected
  instead of silently using the first-declared relationship.** A metric without `USING`, a
  dimension, or a fact on a table reached only through the role-playing table previously
  returned the first-declared relationship's numbers; such a query now raises an ambiguity
  error naming the member, the fact and the relationships to choose from. This includes the
  inner metric of a window metric.
- **A metric declared on a role-playing table is rejected instead of silently using the
  first-declared relationship.** A metric such as `a.total_elev AS SUM(a.elevation)` on the
  role-played `airports` table cannot name a role (a `USING` relationship must start at the
  metric's own table), yet where no fan trap stopped it — the relationships declared
  one-to-one — it returned the first-declared relationship's numbers. Such a query (and one
  on a table reached only through the role-playing table) now raises an ambiguity error;
  declare the metric on the referencing table with `USING` instead. When the role-playing
  table is the view's base table (declared first in `TABLES`), a metric on it needs no
  relationship and is still answered when it is queried without dimensions, alongside only
  other base-table metrics, and with a filter that names only base-table members (see Known
  limitations).
- **A window or derived metric over a metric that reaches a fact across a fanning
  relationship is rejected, like the metric itself.** Querying such a metric directly
  raised a fan-trap error, but through a window metric or a derived metric it joined the
  fanning table without the check.
- **Quoted relationship names work on role-playing relationships.** A relationship named
  with a quoted identifier (`"Dep Airport"`, or a name with an embedded `""`) used to make
  every query that went through it fail with a parser error.
- **A table not connected to the base table is rejected at `CREATE`.** If the first table in
  `TABLES` had no relationship, the relationships formed two separate groups, or the view
  listed several tables with no `RELATIONSHIPS` at all, the view was accepted and every
  query that needed an unconnected table failed with a binder error. `CREATE SEMANTIC VIEW`
  (and `CREATE … FROM YAML`) now rejects the definition and names the unreachable table.
  Views already stored keep loading. When several tables are unconnected, the error now
  names the same one every time.
- **A YAML relationship without `fk_columns` is rejected at `CREATE`.** It used to be
  accepted, and every query on the view then failed with a misleading "legacy
  definition format" error.
- **Relationship names match the way DuckDB matches identifiers.** A relationship declared
  quoted (`"Dep_Airport"`) is now named by `USING (dep_airport)`, and the converse; both
  used to be rejected as an unknown relationship. Two role-playing relationships named
  `dep` and `"dep"` are now rejected as a duplicate name, as `dep` and `DEP` already were.
- **Semantic-view DDL inside a multi-statement query string now says what is wrong.** It
  used to fail with `parser_override is not active for this connection … Re-enable with: SET
  allow_parser_override_extension='FALLBACK'`, although the setting was fine and changing
  it did nothing. The error now says that semantic-view DDL must be sent as its own
  statement, one statement per call. The original message is kept for the case it was
  written for, when the setting really has been switched off.
- **`examples/metadata_and_metrics.py` runs again.** It wrote `NON ADDITIVE BY` after `AS`,
  a parenthesized `PARTITION BY EXCLUDING` list and a window metric over a column instead
  of a metric — all rejected at `CREATE` — and queried a metric as a fact. It also printed
  the wrong columns of `SHOW COLUMNS`, `SHOW SEMANTIC VIEWS` and `DESCRIBE`.
  `examples/snowflake_parity.py` printed the creation time instead of the view name. Every
  example now runs in CI, so an example that stops working fails the build.

### Known limitations

- **Semantic-view DDL must be sent one statement per call.** A query string that combines
  it with other statements (for example one Python `execute()` call holding `CREATE TABLE
  …; CREATE SEMANTIC VIEW …`) is rejected. Split the string into one call per statement.
  See "One Statement per Call" in the transactional DDL and limitations page.
- **A view whose base table is a role-playing table has two gaps.** When the table declared
  first in `TABLES` is the target of two relationships, a dimension on it is rejected as
  ambiguous even in a query that joins nothing else, and a window metric over a `USING`
  metric from the referencing table is computed through the first-declared relationship.
  Declare the referencing table first to avoid both.

## [0.13.0] - 2026-09-30

### Added

- **Every function the extension registers is documented in `duckdb_functions()`.** Each now
  carries a description, real parameter names (`semantic_view(view_name, dimensions, …)`
  rather than `col0`), and a runnable example, so tools and agents that discover functions
  over a SQL connection can find and call them correctly. The table functions behind
  `SHOW` / `DESCRIBE SEMANTIC …` say so and point to the statement to use instead, and every
  table function notes that its `search_path` parameter is reserved for the extension.

### Changed

- **The "semantic view not found" error now suggests `SHOW SEMANTIC VIEWS`** instead of
  `FROM list_semantic_views()`. The documentation and the read-only example likewise use
  `SHOW SEMANTIC VIEWS` / `DESCRIBE SEMANTIC VIEW` in place of the `list_semantic_views()` /
  `describe_semantic_view()` table functions those statements run on. The functions remain
  available — `list_semantic_views()` is still the way to use the view listing as a `FROM`
  source, e.g. to feed `GET_DDL` for every view.
- DuckDB version pin bumped to `v1.5.6`.

## [0.12.1] - 2026-08-28

### Fixed

- **The extension builds correctly against an alternative DuckDB distribution.** Two
  build-system assumptions silently assumed upstream DuckDB, and the
  [Haybarn](https://github.com/Query-farm-haybarn) distribution's build of this extension
  ran into both.

  The first stopped that build outright, on every platform: the test runner's `duckdb` pip
  package was pinned to the exact upstream release number, which exists in no
  distribution's own package index, so `make configure` failed before compiling anything.
  That pin now applies only when the build actually targets the upstream release it names,
  and yields to an explicit `DUCKDB_TEST_VERSION` either way.

  The second was an ABI hazard rather than a visible failure: the vendored DuckDB
  amalgamation the C++ parser shim is compiled into is an ABI contract with the engine that
  loads it, but it was always downloaded from the upstream release regardless of which
  engine the build targeted — so a distribution's binary was compiled against headers
  describing a different engine than the one loading it. It is now generated from the engine
  source tree the CI harness supplies (`./duckdb`) when there is one, checked against the
  release that tree is actually based on (from its own `OVERRIDE_GIT_DESCRIBE` or
  `git describe`, so a mismatched checkout stops the build instead of being relabelled), and
  recorded in `cpp/include/.amalgamation_id` so switching engines rebuilds instead of
  silently reusing the other one's headers.

  Local builds and upstream CI are unchanged — with no engine tree present the pinned
  release is downloaded exactly as before.

## [0.12.0] - 2026-08-10

### Added

- **Each metric is now computed at its own grain, so multi-grain queries return results instead of a fan-trap error.** Until now every generated query was anchored `FROM <base table>` with `LEFT JOIN`s outward, which meant a metric whose table was not the base table could be aggregated over the multiplied join; v0.11.0 made those shapes raise `fan trap detected` rather than silently inflate. They are now *answered* the way Snowflake answers them — each metric pre-aggregated over its own table, the results joined on the queried dimensions. Three shapes become available:
  - a metric on a **parent table** the base table references (`SUM(customers.balance)` in an `orders`-based view), queried alone or with dimensions at or above its own grain — customers are no longer counted once per order, and a customer with no orders is no longer dropped;
  - **metrics at two different grains queried together** — a base-table metric with a child-table metric (fan trap), or metrics on two different children of one parent (chasm trap);
  - a **single derived metric that fuses two grains** (`avg AS order_total / item_count`) — each component is aggregated at its own grain and the expression evaluated over the two pre-aggregates, so the denominator is the true count rather than the fanned one.

  Dimension groups are combined with a NULL-safe `FULL OUTER JOIN`, so a group present at one grain and absent at another survives with a `NULL` metric rather than disappearing; a query with no dimensions yields one row per grain, combined with `CROSS JOIN`. Single-grain queries are unaffected and generate exactly the SQL they did before — the per-grain path is entered only where the query would previously have been rejected.
- A `COUNT(*)` metric on a table with **no declared `PRIMARY KEY`** is answerable in a multi-grain query. The `PRIMARY KEY` requirement exists only because the base-anchored path reaches such a table through a `LEFT JOIN`, whose NULL-extended rows `COUNT(*)` would count; when the table anchors its own grain CTE there are none. Querying that metric on the base-anchored path still requires the key.

- **A window metric whose inner aggregate lives on a non-base table is now computed at its own grain**, instead of raising `fan trap detected` (TECH-DEBT #36, first of three sub-items). A window metric wraps an inner aggregate — `SUM(total_balance) OVER (PARTITION BY segment)` — and the window function runs over the already-grouped `__sv_agg` CTE, so it is not grain-sensitive; the *inner* aggregate is. Anchoring that CTE at the base table joined the two and counted the inner metric's rows once per base-table row: with `total_balance = SUM(c.balance)` on a parent `customers`, a customer with three orders contributed its balance three times. The CTE is now anchored at the inner aggregate's own table, so it sees one row per record there. A pre-aggregation `where_clause` is still applied inside that CTE, before the inner aggregate, so a filtered window number is recomputed rather than filtered after the fact.

  This extends to a bare `COUNT(*)` on a table that declares `UNIQUE (cols)` but no `PRIMARY KEY`. Such a count previously raised "uses `COUNT(*)` on joined table" — the guard against counting the NULL-extended rows a `LEFT JOIN` produces — because `COUNT(*)` cannot be rewritten to `COUNT(<pk>)` without a primary key. The anchored CTE has no such rows, so the count is emitted as written and returns the number of records at that grain.

  Unchanged: a window metric whose inner aggregate is already at the base grain emits exactly the SQL it did before, and a dimension *below* the metric's grain remains an error in both engines — that check is what keeps a below-grain dimension from re-fanning the anchored CTE. Window metrics whose inner aggregates sit at **different** grains still error — those grains would have to be joined before the window runs, which is a later increment.

- **A view that declares role-playing no longer loses multi-grain query support across the board** (TECH-DEBT #36). Role-playing — one table reached from the same source through two named relationships, such as `flights(dep_code)` and `flights(arr_code)` both referencing `airports` — made per-grain aggregation decline for *every* query against that view, because the eligibility test asked whether the **definition** contained role-playing anywhere. A query grouping carrier-grain and flight-grain metrics by carrier raised `fan trap detected` purely because some third table was reachable two ways, and nothing in that query went near it. The test now asks what the **query** reaches: a dimension, a metric grain, or a `where_clause` member that sits on a role-played table, or can reach one only by passing through it. The predicate counts because its tables are joined into every grain CTE just as a dimension's are. Queries that stay clear of it are computed at their own grain as usual.

- **`USING` now selects the role in multi-grain queries too** (TECH-DEBT #36). When a table is reached two ways — `flights(dep_code)` and `flights(arr_code)` both referencing `airports` — a dimension on it is ambiguous until a co-queried metric's `USING (dep)` says which is meant. The base-anchored path has always honoured that; multi-grain queries declined it. Each grain CTE now joins the named relationship under its scoped alias and groups by the dimension bound to it, so grouping flight-grain and leg-grain metrics by departure city returns departure numbers — the same answer the single-grain query gives.

  The rescue covers a queried dimension's own table. A `where_clause` member on a role-played table, a metric aggregated *at* one, or a table reachable only by passing *through* one still error: only a dimension's expression is rewritten to the scoped alias, so nothing else can name its role without guessing, and guessing is what produces a wrong number rather than an error.

  Unchanged: a query that reaches a role-played table without any `USING` to disambiguate it still errors. A grain CTE would have to pick one of that table's relationship instances, and picking silently is the declaration-order-dependent mis-binding the fan-trap fence exists to prevent — so without `USING` to name the role, it keeps the error.

- **A semi-additive metric is computed at its own grain, and can be queried alongside metrics at other grains** (TECH-DEBT #36). `NON ADDITIVE BY` picks the latest snapshot row per group, but when the metric's table sat above the base table the ranking ran over a join that had already duplicated those rows — every copy tied for the win, so the balance was added once per base-table row. Such queries previously raised `fan trap detected`; they are now computed, with the snapshot CTE anchored at the metric's own table. A `NON ADDITIVE BY` dimension declared on a *different* table is joined into that CTE so its ordering still resolves.

  The same holds when that snapshot is one grain among several. Asking for a latest-balance metric and an order count in one query also raised `fan trap detected`, because the snapshot needed its own aggregation shape and there was only one query to give it. Each grain is now computed separately — the semi-additive one ranks its rows before aggregating them, the others aggregate directly — and the results are joined on the queried dimensions. Grouping a customer-grain latest balance beside an order-grain count by segment now returns both correctly, where the balance was previously multiplied by the number of orders per customer.

  A metric on a table *below* the base table is deliberately unchanged: the existing query already counts its rows once, and re-anchoring would drop groups that have no matching rows (an order with no line items would vanish rather than reporting zero).

  This completes the multi-grain work: window metrics, role-playing, and semi-additive metrics can all now appear in a query spanning several grains.

- **Pre-aggregation filtering via a new `where_clause` query parameter**, the equivalent of Snowflake's `SEMANTIC_VIEW( … WHERE <predicate> )`. The predicate names declared dimensions and facts and is applied *before* metrics are aggregated, so a filter on a member that is not in the output finally works: `where_clause := 'ordered_at >= DATE ''2024-01-01'''` recomputes each group's revenue over the matching rows. An outer SQL `WHERE` on the result cannot express this — by then the aggregation has run over every row, and the member being filtered on is not in the output to filter by. Members the predicate references are joined in and counted by the same reachability and fan-out checks as queried dimensions, matching Snowflake's rule that WHERE-clause members participate in the same-logical-table constraint. Referencing a **metric** is rejected, also matching Snowflake: the filter runs before aggregation, so an aggregate has no value yet.

  The parameter is spelled `where_clause` rather than `where` because DuckDB reserves `where` in named-parameter position — `where := '…'` is a parse error before the extension is ever consulted.

  The predicate is applied before aggregation on **every** emission path: before the `GROUP BY` on the base-anchored and fact paths, inside *each* grain CTE for a multi-grain query, inside the `__sv_snapshot` CTE *before* the `RANK` for a semi-additive metric (so filtering changes which row wins the snapshot, which is what "before the metrics are computed" has to mean there), and inside `__sv_agg` before a window function runs. Tables named only by the predicate are joined into whichever CTE evaluates it.

  One constraint follows from that: a predicate naming an **unqualified** member — one declared without a source table, so its binding moves with whatever `FROM` the query emits — keeps the query on the base-anchored path rather than re-anchoring it at a metric's own grain. A multi-grain query filtering on such a member therefore still reports `fan trap detected`; qualifying the member's declaration with its table makes the query answerable.

  Each member's expression is substituted **parenthesized**, so a member that binds looser than its surrounding context keeps its grouping: a filter defined as `o.country = 'US' OR o.country = 'EU'` used as `where_clause := 'us_or_eu AND is_large'` means `(US OR EU) AND large`, not the `US OR (EU AND large)` that a bare textual splice would have produced. The visible consequence is a redundant but harmless `WHERE (o.region) = 'EU'` when a member's expression is a plain column.

- **Named filters — `LABELS = (FILTER)` on a fact or dimension**, matching Snowflake. A named filter is a boolean-valued member that exists to be reused in a query's pre-aggregation predicate rather than selected as output: declare `o.is_domestic AS o.country = 'US' LABELS = (FILTER)` once, then query `where_clause := 'is_domestic AND is_large'`. The annotation sits alongside `COMMENT` and `WITH SYNONYMS` in any order, accepts the `=`-less `LABELS (FILTER)` spelling like `WITH SYNONYMS`, and survives a full round-trip through `GET_DDL`, YAML export, and `DESCRIBE SEMANTIC VIEW` — which reports it as a `LABELS` property row valued `["FILTER"]`, emitted only on labelled members. `SHOW SEMANTIC DIMENSIONS` / `FACTS` keep their existing eight columns; as with `ACCESS_MODIFIER`, the per-member flag is reported through `DESCRIBE`.

  `LABELS` is accepted only on a fact or dimension entry. The annotation tail is parsed by one shared routine that `TABLES`, `METRICS` and the view-level trailing `COMMENT` also use, so each of those rejects `LABELS` explicitly rather than parsing it and discarding it — the same reasoning that rejects an unrecognised label.

  The label declares *intent* and drives introspection — it does not change resolution or restrict access. `where_clause` already substituted any declared fact or dimension name, and a labelled member remains selectable as an ordinary dimension or fact (use `PRIVATE` to make something unqueryable). The `BOOLEAN` requirement is enforced by DuckDB's binder when the filter is first used, not at `CREATE`: typing an arbitrary SQL expression needs a binder, so a non-boolean filter raises DuckDB's own type error at query time rather than a guess at define time. Labels other than `FILTER` — Snowflake's tags, for instance — are **rejected** rather than ignored, so a definition cannot round-trip having quietly lost a label you wrote.

- **`SHOW SEMANTIC DIMENSIONS` / `METRICS` / `FACTS` accept the `IN` scope clause**, matching `SHOW SEMANTIC VIEWS` and Snowflake (TECH-DEBT #25). Previously only the views listing could be narrowed to a schema or database; on the other three, `SHOW SEMANTIC DIMENSIONS IN SCHEMA main` read `SCHEMA` as a *view name* and failed with `Unexpected tokens: 'main'` — even though the reference pages documented the clause, with worked examples, for all of them.

  The whole of Snowflake's scope grammar is now accepted on every SHOW command:

  ```
  IN { view_name | ACCOUNT | DATABASE [ db_name ] | SCHEMA [ [db_name.]schema_name ] }
  ```

  The name is optional: bare `IN SCHEMA` and `IN DATABASE` mean the current one. That is exact rather than approximate, because the schema recorded against a view is itself captured from `current_schema()` when it is created — both sides are the same function. `IN ACCOUNT` is accepted for compatibility and narrows nothing, DuckDB having no account.

  **One behaviour change to be aware of.** On these three commands `IN SCHEMA` previously meant "the view named `SCHEMA`", and now means the scope. A view whose name collides with a scope keyword is still reachable by quoting it — `IN "schema"` — and a name that merely begins with one (`IN schemas`) was never affected, since the keyword must match a whole word.

- **Semantic views are scoped to a schema** (TECH-DEBT #25). A `<schema>.` qualifier on the name is now honoured everywhere it can be written: `CREATE SEMANTIC VIEW analytics.sales` puts the view in `analytics` regardless of the session's current schema, `DROP SEMANTIC VIEW staging.sales` drops only that one, and `semantic_view('analytics.sales')` reads it. Two schemas may each hold a view of the same name — previously `CREATE SEMANTIC VIEW staging.v` collided with an existing `analytics.v` and failed with a key violation for a view that, by name, did not exist. Naming a schema that does not exist is now an error rather than a silent fall back to the current schema, as `CREATE TABLE nosuch.t` is.

  The schema is recorded in the catalog's own spelling rather than the caller's, so `USE MYSCHEMA` and `USE myschema` no longer file two views in one schema under two different names. `ALTER SEMANTIC VIEW analytics.sales RENAME TO staging.sales` moves the view; an unqualified `RENAME TO` leaves it where it is.

  Unqualified table names in a view body still resolve in the **creating session's** schema, not the view's, following DuckDB's rule for a view body — so semantic views can live in their own schema while reading tables from another.

  **A reference that names no schema follows the session's search path**, the way it does for every other DuckDB object: the first schema on `search_path` holding a view of that name wins, so `SET search_path = 'staging'` makes a bare `sales` mean `staging.sales`. A view that is the only one of its name resolves whether or not its schema is on the path, which keeps the ordinary single-schema case unchanged. Reads and writes resolve identically — `DROP SEMANTIC VIEW v` removes the view `semantic_view('v')` returns.

  A name that exists *only* in schemas off the path is a miss, but not a bare one: the error names the schemas it does live in, the path that was searched, and the two ways out (qualify it, or add the schema to `search_path`). `IF EXISTS` does not absorb that — it means "do not complain if the view is absent", and a view sitting off the path is not absent.

  Two exceptions, both narrow: `GET_DDL` and `READ_YAML_FROM_SEMANTIC_VIEW` are scalar functions, which cannot take the search path, so they still resolve a bare name to the unique match and report an error when several schemas hold it.

  A `<database>.` prefix is now checked rather than discarded. Semantic views are single-catalog, so `CREATE SEMANTIC VIEW otherdb.analytics.v` cannot be honoured — it used to drop the prefix and quietly create the view in the current database's `analytics` instead, and now says so.

  Existing databases are migrated in place on the next `LOAD`. A read-only database whose catalog predates schema scoping refuses to load with an instruction to open it writable once first.

- **`GET_DDL` takes Snowflake's third argument, `use_fully_qualified_names`** (TECH-DEBT #25). `GET_DDL('SEMANTIC_VIEW', 'analytics.sales', true)` renders `CREATE OR REPLACE SEMANTIC VIEW analytics.sales AS …`, so re-running the output puts the view back in `analytics`. Without it — still the default, as in Snowflake — the rendered name is bare, and replaying a dump recreates every view in whatever schema the restoring session happens to be in. That was harmless while views shared one flat namespace; now that they are schema-scoped, a restore silently relocates every view that does not live in the restoring session's schema.

  The schema rendered is where the view *is*, not how the lookup was spelled: a bare `sales` that lives in `analytics` renders `analytics.sales`. (As a scalar, `GET_DDL` cannot be handed the search path, so a bare name resolves to the unique view of that name and reports an ambiguity error when several schemas hold one — `SET search_path` does not disambiguate it.) Schema and view name are quoted independently — `"my schema"."my view"`, never `"my schema.my view"`, which would restore into a view whose name literally contains a dot. A `NULL` third argument returns `NULL`, since "qualified or not?" has no answer there and either spelling would be a guess.

### Changed

- **Documented Snowflake's dimension-granularity rule as the source of the `fan trap detected` error.** The rule a query must satisfy — the dimension's logical table must be *related* to the metric's and have "an equal or lower level of granularity" — is Snowflake's own, and the comparison and fan-trap pages now cite it rather than presenting it as this extension's reasoning. The two remaining query-semantics gaps are stated precisely alongside it: the pre-aggregation `WHERE` predicate (which Snowflake applies before metrics are computed) and `LABELS = (FILTER)` named filters, which previously sat inside an unrelated "not planned" row.

- DuckDB version pin bumped to `v1.5.5`.

- **`SHOW SEMANTIC … LIMIT 0` is documented as accepted, and its error message now matches.** The parse has always taken any non-negative count, and `LIMIT 0` returns no rows the way it does anywhere else in DuckDB, but the error raised for a rejected value promised "a positive integer" — and five reference pages repeated it. Both now say non-negative.

- **Corrected what the `data_type` column reports.** `SHOW SEMANTIC DIMENSIONS` / `METRICS` / `FACTS`, `SHOW COLUMNS IN SEMANTIC VIEW`, and `DESCRIBE SEMANTIC VIEW` report the type a definition *declared*, and nothing is inferred; a newly created view leaves the column empty. The reference pages had continued to describe the CREATE-time `typeof` inference removed in v0.10.0, and showed example output (`VARCHAR`, `DOUBLE`, `DECIMAL(10,2)`) that no view created since can produce. The gap against Snowflake, which populates the column, is now recorded as a known limitation rather than living only in a test comment.

- **Stated the expression-scope rule, and corrected the `PRIVATE`-on-dimensions comparison.** A member expression may reference only its own logical table's columns — the same rule Snowflake enforces, though Snowflake rejects a violation at `CREATE` where this extension surfaces it as a DuckDB binder error at query time. Rejecting `PRIVATE` on a dimension, previously listed as a way this extension is narrower than Snowflake, turns out to match Snowflake exactly ("Dimensions are always public"), and the comparison page now says so.

- **Window metrics reject expression text outside `FUNC(args) OVER (...)` instead of silently ignoring it.** `AVG(qty) OVER (PARTITION BY store) + 100` previously parsed, stored only the windowed aggregate, and then computed a number 100 short of what the definition said; `1 + AVG(qty) OVER (...)` stored `1 + AVG` as the function name; a second `OVER` term was dropped entirely. Composing a window function with surrounding arithmetic is not supported, and the error now names the offending text and points at it. Definitions already stored are unaffected at load, but a view created this way before the fix renders DDL that no longer re-creates — recreate it with the arithmetic moved into the inner metric.

- **`CREATE SEMANTIC VIEW … FROM YAML` no longer accepts `output_type` on a dimension, fact or metric.** No DDL clause can express a member's type, so `GET_DDL` dropped the field and a restored view silently lost the `CAST` it implied — and every member referring to that one had always computed on the uncast expression anyway, since references inline the raw expression. Write the cast into the expression instead (`expr: CAST(o.ordered_at AS DATE)`), which round-trips correctly and applies everywhere the member is used. Existing stored views are unaffected. As a consequence the `data_type` column in `SHOW SEMANTIC DIMENSIONS` / `METRICS` / `FACTS` is now empty for every newly created view regardless of how it was defined.

### Fixed

- **A row from a table with no matching related rows no longer leaks into results.** Every generated join is a `LEFT JOIN` from the view's base table, so a base row with no match — an order with no line items — survives as a row of NULLs. Aggregates on the joined table now exclude it whatever they aggregate; previously only `COUNT(*)` and a short list of aggregates over a plain literal did, so `COUNT(DISTINCT 1)`, `COUNT(1+0)`, `MIN(1)`, `SUM(COALESCE(li.qty, 99))` and a metric referencing a fact on another table all counted a row that does not exist (returning 1, 1, 1, 99, and 25 instead of 20 respectively). Empty-group semantics come back with it: a parent with no children reports `0` for a count and `NULL` for a sum, min, max or average.

- **`facts := [...]` and dimensions-only queries no longer return a manufactured all-NULL row per unmatched parent** when every queried member lives on one table below the base — the row-level form of the same defect, where the NULL was indistinguishable from one in the data. Members on a table *above* the base are deliberately unaffected: a base row whose foreign key is NULL or dangling is a real row of the view, and its NULL attribute is part of the answer.

- **Escape-string and typed-literal introducers (`e'…'`, `E'…'`, `DATE'…'`) are no longer read as identifier references**, so a fact, dimension or table alias named `e` or `date` can no longer corrupt an expression containing such a literal, nor register as a phantom dependency that changes which tables get joined.

- **`--` comments now end at a bare carriage return**, as DuckDB's own scanner does; previously the rest of the line — including live SQL following a lone `\r` — was treated as commented out.

- **A SQL comment marker in a YAML member expression is refused** instead of silently merging or destroying sibling members when the definition is restored from `GET_DDL`.

- **YAML import now runs the same cross-reference validation as the DDL parser** — `NON ADDITIVE BY` dimensions, a window metric's inner metric and its `PARTITION BY` / `ORDER BY` / `EXCLUDING` dimensions, and materialization names and references. These were checked only for `CREATE … AS`, so a YAML definition could name members that do not exist and then render DDL this extension's own parser rejects. A window specification is additionally verified to survive `GET_DDL` unchanged: a hostile `frame_clause` could previously inject an extra metric into the restored view, and a partition dimension named `rows`, `range` or `groups` silently became a frame clause.

- **A metric named `private` or `public` is quoted in `GET_DDL` output**, so a view declaring one can be restored rather than failing to re-parse as a bare access modifier.

- **`SHOW SEMANTIC … STARTS WITH` matches a literal prefix.** `_` and `%` in the prefix reached the pattern matcher as wildcards, so `STARTS WITH 'a_b'` also returned `axb`.

- **A quoted wildcard qualifier (`"O".*`) resolves against its table alias** instead of reporting an unknown alias.

- **Opening a database read-only that still holds an unimported v0.1.0 companion file now refuses with an actionable message** instead of silently reporting every semantic view as nonexistent.

- **A definition imported from YAML is now checked against the same identifier rules the DDL grammar enforces.** YAML is the only way to define a semantic view without going through the DDL parser, and it accepted identifier slots the parser never could — a relationship with no name, a dimension or fact with no table, a name containing a space, a table qualifier smuggled into a member's `source_table`, a column containing a SQL comment marker. `GET_DDL` then rendered those definitions back as DDL that fails to parse, or — for a qualifier like `source_table: a.b` — as DDL that parses *successfully into a different definition* than the one stored. Such YAML is now rejected at import with an error naming the offending slot. The clause *structure* is checked too: the grammar requires a `DIMENSIONS` or a `METRICS` clause, so YAML declaring only tables — which `GET_DDL` rendered as a lone `TABLES` clause that no longer parses — is rejected as well, as is a dimension, fact or metric with an empty expression, which rendered as a bare `o.total AS ` with nothing after the `AS`. Legitimate shapes are unaffected: quoted identifiers; derived metrics, which carry no table by design; dot-qualified relationship names and schema-qualified materialization tables, both of which the DDL grammar accepts; and physical table names needing quotes, which `GET_DDL` quotes for you.

- **A derived metric can now reference another metric by its table-qualified name.** `revenue_x2 AS li.item_rev * 2` is the same reference as `item_rev * 2`, and validation has always accepted both — the qualified form is the documented way to compose metrics that live on different tables. But only the bare spelling was substituted, so the qualified one reached DuckDB as a literal column reference: unaggregated, ungrouped, and unresolvable. The multi-grain path already handled it, so whether a definition worked depended on which query shape it was used in. Both spellings now produce identical SQL, and quoting or case differences on either half of the reference are immaterial, as elsewhere in the language. A reference qualified with a *different* table still resolves to nothing, as before.

- **A quoted reference now resolves at `CREATE` in every clause that accepts one.** DuckDB matches identifiers case-insensitively whether or not they are quoted, and the query layer already followed that rule — but five validators that run at `CREATE` compared the quote characters as data. `PARTITION BY EXCLUDING "Region"` was rejected against a `region` dimension while `ORDER BY "Region"` in the same window clause resolved, so a definition the engine could have computed was refused at definition time. The affected slots were window `EXCLUDING`, window `PARTITION BY`, `NON ADDITIVE BY` (both the bare and the alias-qualified forms), and the dimension and metric lists of a `MATERIALIZATIONS` entry. All five now use the same identifier rule as the rest of the language; a reference to a genuinely undeclared name is still rejected, with the same "did you mean" suggestion as before. The same correction was needed one level down: a qualified reference resolved its member name case-insensitively but compared its *table qualifier* literally, so `"O"."Snap_Date"` matched the dimension and then failed on its own alias — reaching the query as an unknown column. Both halves of a qualified reference now follow the identifier rule; a reference to a genuinely different table still resolves to nothing.

- **Escape strings (`E'…'`) are now understood in member expressions.** DuckDB accepts the Postgres spelling in which a backslash escapes the following character, so `e'\''` is a one-character literal holding a quote. Every scanner in the extension read `\` as an ordinary byte and took the middle and closing quotes for a single `''` escape pair, so the literal appeared to run to the end of the statement. A metric like `count_if(x = e'\'')` was rejected outright as an unterminated string literal; worse, when such an expression was followed by more members, the entry separator was swallowed along with everything else, silently collapsing two members into one malformed member. Both spellings of the escape are honoured (`\'` and `''`), and `\\` escapes the backslash itself rather than a quote after it. Typed literals whose type name ends in `E` — `DATE'2020-01-01'`, `TIME'12:00:00'` — are unaffected: their `E` belongs to the type name, so they keep the ordinary string rules, as does any plain `'…'` literal containing a backslash.

- **A column named `comment` or `labels` is now usable unquoted in a member expression.** The scanner that finds trailing `COMMENT = '…'` / `LABELS = (FILTER)` annotations treated `.` as a word boundary, so in `o.comment = 'keep'` it took the `comment` for the annotation keyword: the dimension was stored as the dangling expression `o.`, the user's predicate was discarded, and `CREATE` reported success. The damage surfaced only later, as a syntax error at query time, and `GET_DDL` round-tripped the corrupted definition. `o.labels IS NOT NULL` failed at `CREATE` for the same reason. DuckDB accepts both words as ordinary unquoted column names, so a qualified reference to one is now read as part of the expression. The bare keywords still introduce annotations, and the quoted form (`o."comment"`) is unchanged.

- **`COUNT(1)` on a joined table no longer over-counts.** A synthesized join is a `LEFT JOIN`, so a base row with no matching child rows survives as one NULL-extended row; `COUNT(*)` has been rewritten to `COUNT(<primary key>)` for non-base tables since v0.10 precisely so it does not count that phantom row. `COUNT(1)` — the same idiom spelled differently — was not covered by that rewrite and counted it, returning one too many per childless parent, and could sit in the same result row as a correct `COUNT(*)`. Any aggregate over a *constant* has the same blind spot, since its argument is never NULL: `COUNT(1)`, `SUM(1)`, `COUNT('x')`, `PRODUCT`, `LIST` and `ARRAY_AGG` over a constant are now guarded by the source table's primary key. This also restores the empty-group semantics such a metric should have had — `0` for a count, `NULL` for a sum, where a parent has no child rows at all. As with `COUNT(*)`, the guard needs a declared `PRIMARY KEY` on the joined table; without one the query errors rather than returning an inflated number. Metrics on the base table, aggregates over a column, and `MIN`/`MAX`/`AVG` over a constant are unaffected.

- **A metric that depends on a semi-additive metric no longer silently ignores `NON ADDITIVE BY`.** The rule that routes a query through the snapshot aggregation asked only whether the *requested* metric carried `NON ADDITIVE BY`. A derived metric referencing a semi-additive one (`double_balance AS balance * 2`), or a window metric naming it as its inner aggregate, therefore took the ordinary path and had the raw aggregate substituted in — summing every snapshot row instead of the selected one. The results did not even agree with each other: `double_balance` came back as something other than twice `balance`. Such a query now reports that the metric depends on a semi-additive metric, and names both the way out (query the semi-additive metric on its own) and the alternative (add its `NON ADDITIVE BY` dimensions to the query, which makes it effectively regular and legal, as before). Composing a snapshot with an outer expression is not yet supported; the error replaces a wrong number, not a working feature.

- **A dimension expression can now reference a declared fact.** Facts were inlined into metric expressions and into other facts', but never into a dimension's — so `size_band AS CASE WHEN o.net_line >= 49 THEN 'big' ELSE 'small' END`, over a fact `net_line` on the dimension's own table, reached DuckDB with `o.net_line` intact and failed on the unknown column. This is the form Snowflake's validation rules permit explicitly ("expressions can refer to base table columns or other expressions on the same logical table"), and it was broken for the plain same-table case, not only across tables. Every path that renders a dimension now inlines first: the main SELECT, the per-grain CTEs, fact queries, semi-additive snapshots and their ordering, window CTEs, and `where_clause` predicates. A dimension reaching a fact on another table pulls that table's join and is fan-trap checked, on the same footing as a metric.

- **A member expression that references a raw column of another logical table is now rejected at `CREATE`**, with a message naming the rule and the supported alternative, instead of being accepted and failing later as a DuckDB error about an unknown alias. `o.margin AS o.amount - c.discount` reports that a metric may only reference columns of its own table and points at defining a fact on `c` instead. Snowflake applies the same rule and rejects at definition time; only the enforcement point differed. The legal cross-table forms are unaffected — a named fact reference (`c.cust_discount`) and a derived metric composing metrics that live on other tables both still validate — as are expressions whose qualifier names no declared table, which remain DuckDB's to resolve.

- **A metric or dimension that references a named fact on another table now joins that table, instead of failing to bind.** Defining a fact on the table whose columns it uses and referring to it from a connected table — `o.net_total AS SUM(o.amount - p.prod_markup)`, where `prod_markup` is a fact on `products` — is the supported way to cross tables. The fact's expression was inlined at the reference site as intended, but the join for its table was collected only from each member's own declared table and from *queried* facts, never from facts a member merely references. The result was `SUM(o.amount - (p.markup)) FROM "orders" AS "o"`, which DuckDB rejected for the unknown alias `p`. The tables reached through fact references are now collected from the expression, transitively, on both the base-anchored and per-grain paths.

  A fact on a table that **fans** the referencing member's — reached across a one-to-many edge, so joining it multiplies that member's rows — is rejected with `fan trap detected` naming the fact and the relationship, rather than answered with an inflated aggregate. Facts on the parent side (one row per member row) are the usual case and join safely. Only the named-fact form has a join behind it — referencing a raw *column* of another table is rejected at `CREATE` (see the scoping bullet above).

- **A fact query no longer rejects a dimension that is genuinely reachable, when the base table is referenced by more than one other table.** In a view whose base table `orders` is referenced by both `line_items` and `shipments`, a fact on `shipments` queried with a dimension on `customers` was rejected — "tables … are not on the same root-to-leaf path" — even though every hop of `shipments → orders → customers` is many-to-one and the join is safe. The same cause hid those dimensions from `SHOW SEMANTIC DIMENSIONS … FOR METRIC`. Whether two tables can be queried together is now decided by whether the join path between them can be walked without multiplying rows, rather than by their position in a tree whose shape depended on the order the relationships happened to be declared in. Pairs that genuinely would multiply rows — a fact on `line_items` with a dimension on `shipments`, say — are still rejected, and the error they raise now explains that joining the two would duplicate rows rather than describing their position in a tree. Its `fact query references objects from incompatible table paths` prefix is unchanged.
- **A metric grouped by a dimension on a *sibling* table now raises `fan trap detected` instead of silently returning inflated numbers.** When two tables both reference a third — `line_items` and `shipments` both referencing `orders` — neither is an ancestor of the other, and the fan-trap check walked parent chains to find the path between a metric's table and a dimension's table. For siblings that walk finds no path at all, so the pair was skipped and the query was accepted: joining both children multiplies each one's rows by the other's (an order with 2 line items and 2 shipments contributes 4 rows), inflating the aggregate. The check now walks the relationship graph itself, which finds the real path (`line_items → orders → shipments`) and the fanning leg on it.

- **A semi-additive metric whose `NON ADDITIVE BY` dimension sits on a fanning table now raises `fan trap detected` instead of silently double-counting.** `NON ADDITIVE BY` picks the latest snapshot row per group, and the dimension it ranks by has to be joined in even when it is not among the queried dimensions. Where that join multiplied the metric's own rows — a base-table metric ranked by a dimension on a child table, say — the ranking ran over the duplicates, and ties between copies of one row are indistinguishable from ties between genuinely different rows, so the snapshot could not tell them apart and added the value once per copy. A single order with two line items sharing a timestamp reported twice its balance. The fan-out check now covers the `NON ADDITIVE BY` dimension's table alongside the queried ones, so the query errors rather than answering wrongly.

  This closes an asymmetry rather than adding a restriction: *querying* the same dimension has always raised the error for the identical join — it was the un-queried case that was permissive. A `NON ADDITIVE BY` dimension on a table that does not fan the metric (reached one row per row) is unaffected and still computes.

- **A view whose `RELATIONSHIPS` form a cycle is now rejected at query time instead of being certified as safe** (TECH-DEBT #48). Cycles are rejected at `CREATE`, but the fan-trap check rebuilds the relationship graph from the *stored* definition and did not repeat that check — it only rejected a table referencing itself. So a cyclic definition that reached the catalog another way was accepted, and worse, actively passed the fan-out check: with `a` referencing `b` and `b` referencing `a`, the check found the forward reference first and concluded the hop could not multiply rows, never considering the reverse one that does. Such a query now fails with `cannot verify the query is safe from fan traps`, naming the cycle.

  A pair of relationships pointing both ways asserts "many `a` per `b`" and "many `b` per `a`" at once, so there is no row count the answer could be checked against — which is why this reports rather than picking a direction. Views whose relationships form a tree, which is every view `CREATE` accepts, are unaffected.

- **A read naming a different database is now rejected instead of quietly answering from this one** (TECH-DEBT #49). Semantic views are single-catalog: the definitions live in the database the extension was loaded into. `DROP SEMANTIC VIEW otherdb.analytics.v` has always errored rather than dropping the current database's `analytics.v` under a name that does not refer to it — but the read paths captured the `otherdb.` prefix and then ignored it, so `semantic_view('otherdb.analytics.v')` returned this catalog's data under another database's name, with nothing to signal that the database asked for was never consulted. Every read surface — `semantic_view()`, `explain_semantic_view()`, `DESCRIBE SEMANTIC VIEW`, the `SHOW SEMANTIC …` listings, `GET_DDL`, `READ_YAML_FROM_SEMANTIC_VIEW` — now reports the mismatch, naming both the database written and the one holding the catalog.

  A qualifier naming the catalog's own database still resolves, case-insensitively as everywhere else, so `memory.main.v` and `MEMORY.main.v` are unaffected. Because reads are pinned to that catalog regardless of `USE`, a reference is judged against it and not against the session's current database: naming an attached database is rejected even from a session sitting in it.

- **A view imported from a v0.1.0 companion file is now listed by `SHOW SEMANTIC VIEWS`** (TECH-DEBT #49). The one-time import of a pre-v0.2 `.semantic_views` file recorded the view's schema in the catalog column but not inside the stored definition, and the listings read the definition. Such a view appeared with an empty schema in `SHOW SEMANTIC VIEWS` and was missing entirely from `SHOW SEMANTIC VIEWS IN SCHEMA main`, though querying it by name worked. Existing catalogs are unaffected — the file is imported once and deleted, so this reaches only databases upgrading from v0.1.0 now.

- **A metric that mixes an unqualified aggregate with a metric on another table is no longer computed against the wrong table** (TECH-DEBT #50). A stored metric that aggregates without naming a table reads the base table's columns, but contributed no table of its own, so combining it with a metric on a *different* table left only that other table recognised as a grain — and the whole expression was then aggregated there. A derived metric fusing an unqualified `SUM(amount)` with a customer-grain balance emitted `SUM(amount)` against `customers`, returning that table's `amount` if it happened to have such a column and an unhelpful binder error if it did not. Such a query now raises `fan trap detected` rather than answering from the wrong table.

  Views created by any current release are unaffected: a metric must name its table (`o.revenue AS SUM(amount)`), so it has always carried a grain. Only a definition that reached the catalog with an unqualified aggregate — a legacy stored row, or one constructed directly — can hit this. Qualifying that metric is also the fix: written as `o.root_total AS SUM(amount)`, the same query is computed at both grains and returns a result.

- **Cancelling a `semantic_view()` query now actually stops it** (TECH-DEBT #42). `duckdb_interrupt()` — and with it ADBC's `adbc_cancel()`, the CLI's Ctrl-C, and any client-side timeout built on them — had no effect: the query ran to completion and the cancellation was reported only afterwards. A client that stopped waiting therefore left the work running, with no consumer for the result.

  DuckDB's interrupt flag belongs to the connection that is executing, and `semantic_view()` runs its expanded SQL on its own short-lived connection. The cancel was being set on the caller's connection and read on the inner one, so nothing ever observed it. The inner query is now driven task by task, with the caller's flag checked between tasks and forwarded to it, so the work stops within a task of the cancel rather than at the end of the query. Measured against a ~3.9s aggregate cancelled at 0.4s: previously 3.9s, now 0.4s — matching what the same aggregate does as hand-written SQL.

  The error is reported as a plain `Interrupted!`, not wrapped in `semantic_view: SQL execution failed: …`, so ADBC clients see a cancellation rather than an internal error.

  Not covered: the type-inference probe that runs while the query is being planned, which stays uninterruptible. It is a `LIMIT 0` query that DuckDB short-circuits before touching any data (measured at 4ms against an aggregate whose full run takes 7.9s), so there is no meaningful window to cancel in.

- **A misplaced `USING` or `NON ADDITIVE BY` is now rejected at `CREATE`** instead of being absorbed into the metric's expression (TECH-DEBT #38). Both clauses belong before the `AS` — `o.balance NON ADDITIVE BY (as_of) AS sum(o.v)`. Written after it, the clause became part of the expression text: the metric was stored as an ordinary additive metric with its declared semantics silently dropped, `DESCRIBE` reported no non-additive dimensions, `GET_DDL` round-tripped the malformed text so the definition survived a dump and reload, and the only complaint came at query time as a confusing parser error pointing inside generated SQL. The error now names the clause and the position it belongs in, at the point the mistake is made. The same words inside a string literal or quoted identifier are still data, not syntax, and an `OVER (...)` clause after `AS` is unaffected — that one legitimately belongs there.

- **A quoted name in a `SHOW SEMANTIC …` filter clause is now honoured** (TECH-DEBT #25). The `IN SCHEMA` / `IN DATABASE`, `IN <view>` and `FOR METRIC` slots peeled their identifier at the first whitespace, so `SHOW SEMANTIC DIMENSIONS IN "my view"` truncated the name and failed with `Unexpected tokens: 'view"'`. They now use the same quote-aware scan as every other name slot, so whitespace inside `"…"` is part of the name.

  The schema and database slots had a second, quieter failure: their value goes into a `schema_name = '…'` comparison with nothing downstream to unquote it, so `SHOW SEMANTIC VIEWS IN SCHEMA "main"` compared against `"main"` **including the quote characters** and returned no rows at all — no error, no warning, just an empty result. Those two now strip the quotes (and unescape a doubled `""`), while an unterminated quote is reported as an invalid name instead of being swallowed. The view and metric slots keep their raw text, as their lookups already normalise it.

  Relatedly, `SHOW SEMANTIC DIMENSIONS … FOR METRIC "revenue"` now finds a metric declared unquoted as `revenue`. Metric names were compared case-insensitively but without stripping quotes, so a quoted reference to an unquoted declaration reported `metric '"revenue"' not found` — while helpfully suggesting `revenue`.

  Unquoted names, which is what every example uses, are unaffected.

- **`SHOW SEMANTIC … IN SCHEMA` / `IN DATABASE` now match case-insensitively**, like every other name in the language and like DuckDB's own identifier resolution (TECH-DEBT #25). `IN SCHEMA MYSCHEMA` previously matched nothing against a schema created as `MySchema`.

  This mattered more than a spelling nicety, because the schema and database recorded against a view are captured from `current_schema()` / `current_database()` when it is created, and DuckDB reports those as the spelling you last wrote in `USE` rather than the catalog's own. Two views created in the *same* schema — one after `USE MySchema`, one after `USE myschema` — were therefore recorded differently, and an exact-match filter could return at most one of them. No spelling returned both, including the one the catalog itself holds. Both spellings are now folded, so quoting still makes no difference (`"MySchema"` and `MySchema` behave identically) and the filter is an equality rather than a pattern, so a `%` or `_` in a schema name stays a literal character.

  Not changed: the recorded value itself, so `SHOW SEMANTIC VIEWS` may still *display* two spellings of one schema across rows created under different `USE` statements.

- **`SHOW SEMANTIC VIEWS IN SCHEMA <database>.<schema>` now works** instead of silently returning nothing (TECH-DEBT #25). Naming a schema together with its database is how Snowflake writes this filter, and it was accepted here without complaint — but the two halves were rejoined with a dot and compared against the recorded schema, which holds a bare name. Nothing ever matched `mydb.analytics`, and because an empty result is a legitimate answer there was no error to notice.

  Both halves are now applied, so a schema of the same name in a *different* database is correctly excluded rather than folded in. A dot inside quotes stays part of the name: a schema actually called `"a.b"` is still found, and is not read as database `a`, schema `b`.

  Two spellings that previously produced the same silent empty result are now errors that say what was expected: a three-part schema name (`a.b.c`), and a qualified database name (`IN DATABASE a.b` — a database has nothing to qualify it with).

- Corrected the documented annotation order: `COMMENT`, `WITH SYNONYMS` and `LABELS` may appear in **any** order on an entry. The DDL reference and the metadata-annotations how-to previously stated that `COMMENT` must precede `WITH SYNONYMS` and that the reverse was a parse error; the parser has always accepted either order, requiring only that the annotation region be tiled by recognized clauses with no leftover text.

### Known limitations

- **`data_type` is empty for every view created today.** No type is inferred at `CREATE` (v0.10.0 removed that pass) and none is probed on read, so `SHOW SEMANTIC DIMENSIONS` / `METRICS` / `FACTS`, `SHOW COLUMNS IN SEMANTIC VIEW`, and `DESCRIBE SEMANTIC VIEW` report only a type the definition declared — and no surface can declare one: the DDL grammar has no clause for it, and the YAML `output_type` field is now rejected at import because `GET_DDL` could not carry it (a restored view silently lost the cast). The column is populated only for views stored before that change. Snowflake populates it. Query results are unaffected — `semantic_view()` infers each output column's type when the query is bound; only the catalog metadata is silent.

- A **dimension below a metric's own grain** (`SUM(customers.balance)` grouped by an order-grain dimension) remains an error in both engines: the metric's rows genuinely fan across the dimension's values, so there is no single correct value per group. Snowflake likewise requires dimensions to be reachable from a metric's table through many-to-one relationships.

## [0.11.0] - 2026-07-20

### Changed

- **View name case normalization**: view names now fold to lowercase in every DDL statement and in `semantic_view()` / `explain_semantic_view()` lookup arguments — whether written quoted or not — so `CREATE SEMANTIC VIEW Sales`, `DROP SEMANTIC VIEW SALES`, and `DROP SEMANTIC VIEW "sales"` all refer to the same view. This follows DuckDB's identifier semantics, where double-quoted identifiers are case-insensitive too; quoting only lets a name carry whitespace or special characters, it does not make it case-sensitive. Previously unquoted names were byte-exact case-sensitive. **Migration**: lookups fold the requested name to lowercase and match the stored catalog name exactly, so a view is only reachable if its stored name is lowercase. Unquoted `CREATE` always stored a lowercase name, so those views are unaffected; only a view created earlier via a *quoted* mixed-case identifier (e.g. `CREATE SEMANTIC VIEW "Sales"`) kept its original casing and is no longer reachable by any spelling — drop and recreate it, or rename its catalog row to lowercase.
- **Dimension / metric / fact query references are matched case-insensitively**, following the same DuckDB identifier semantics as view names: a reference matches regardless of case whether written unquoted (`region`, `REGION`) or double-quoted (`"Region"`, `"region"`) — DuckDB treats double-quoted identifiers as case-insensitive too. A quoted reference is also correctly *stripped* of its quotes before matching (previously a quoted stored name was only reachable by a reference carrying the identical quote characters). The same key governs the adjacent query surfaces so they stay consistent: CREATE-time name-uniqueness validation (names differing only in case or quoting — `region`, `REGION`, `"Region"` — collide as duplicates) and `alias.*` wildcard de-duplication. Table-qualified references are split quote-aware, so a quoted name containing a dot (`"a.b"`) is no longer mis-split. Names embedded in stored **expressions** — derived-metric operands and inlined fact references — now match case- and quote-insensitively too, via the shared reference tokenizer (see Fixed). The internal name-field matchers are unified on the same key too — for example a quoted name in a `MATERIALIZATIONS` clause routes to its unquoted declaration — as do the window and semi-additive dimension references detailed under Fixed.

### Added

- Machine-checked round-trip guarantee between `CREATE SEMANTIC VIEW` parsing and `GET_DDL` rendering: a property test asserts `parse(render(definition)) == definition` over generated definitions (including quoted, unicode, and keyword-bearing identifiers), and two new fuzz targets exercise the body parser directly and enforce render/parse fixpoint stability.
- Stored semantic view definitions now carry a storage-format `schema_version`. Freshly created (or replaced) views are stamped with the current version, and a one-time, non-destructive upgrade pass on extension load stamps existing definitions that are verifiably current-format — giving future format changes a clean migration point (following the v0.1.0 companion-file migration precedent).
- **Broader Snowflake-syntax acceptance for easier DDL porting**: the table alias is now optional in the `TABLES` clause (`TABLES (orders PRIMARY KEY (id))` defaults the alias to the table name, matching Snowflake's `[alias AS] table`); a view-level `COMMENT = '...'` may be written in Snowflake's trailing position (after the last clause) as well as between the name and `AS` (specifying both is rejected); an explicit `PUBLIC` modifier is accepted on dimensions (a no-op, since public is the default — `PRIVATE` on a dimension is still rejected rather than silently downgraded); `WITH SYNONYMS (...)` is accepted without the `=`; and `DESC SEMANTIC VIEW` is accepted as an abbreviation of `DESCRIBE SEMANTIC VIEW`.

### Fixed

- **Three fan-trap safety-fence gaps that let a query silently return inflated aggregates now raise a clear `fan trap detected` error.** The whole point of the fence is to error rather than silently inflate, but three query shapes that *looked* safe slipped through it while their already-checked neighbours correctly errored:
  - A **metric defined on a table the base table references** — a parent / "one" side of a many-to-one relationship, e.g. `SUM(customers.balance)` in a view whose base table is `orders` — was aggregated at the base-table grain, counting each parent row once per referencing base row (and dropping parent rows with no children). Because the query is always anchored `FROM <base table>`, this inflated even when the metric was queried alone, or with only a dimension on that same parent table, so neither pairwise check fired. Such a query is now rejected.
  - A **single derived or window metric that internally combines aggregates from two tables** across a many-to-one join — e.g. `avg AS order_total / item_count`, fusing an order-grain metric and a line-item-grain metric — inflated the parent-side component over the fanned join. Folding two grains into one metric used to bypass the metric-versus-metric check (which only compared *distinct* metrics); the metric is now checked against its own grain span and rejected, just as querying the two base metrics together already was.
  - An **active semi-additive (`NON ADDITIVE BY`) metric queried alongside a dimension on a fanning child table** ran its snapshot (`RANK`/`ROW_NUMBER`) query over the already-multiplied join, where ties across the fanned duplicates of one source row are indistinguishable from ties across distinct rows, so it could double-count. Such metrics previously skipped the fan-trap check entirely on an unproven assumption that the snapshot neutralised the fan; they now get the same check as any other metric. Snapshots grouped only by safe, root-ward dimensions are unaffected.
- A semantic view whose `RELATIONSHIPS` form a **cycle** (e.g. `a` references `b` and `b` references `a`) no longer hangs a query with unbounded memory growth. Such a definition parses successfully, and a query against it previously sent the fan-trap safety check's join-tree ancestor walk into an infinite loop — a cyclic relationship graph yields a cyclic parent map — allocating until the process was killed. The parent-chain walks now stop at the first revisited node, so expansion terminates. (Found by fuzzing.)
- A stray or leading comma in any clause list — `DIMENSIONS (a AS x,, b AS y)`, `TABLES (,o AS orders ...)` — is now rejected instead of being silently dropped. A single trailing comma (`METRICS (a AS ..., )`) is still tolerated.
- Malformed identifier slots in a view body are rejected instead of being silently stored as unqueryable names: a whitespace-separated multi-token name (`o.d junk AS ...`, which previously named the dimension `d junk`) and an empty quoted identifier `""` in a name or alias slot now error, matching the checks already applied to view names. An unqualified dimension/metric entry name whose expression happens to contain a dot (`region AS upper(o.region)`) now reports the missing `alias.name` qualifier instead of a misleading "Expected 'AS'".
- **Dollar-quoted strings (`$$…$$` and `$tag$…$tag$`) inside a dimension or metric expression are now parsed as one opaque literal** instead of being silently mis-split. A comma inside one — `DIMENSIONS (o.label AS $$a, b$$)` — previously split the single entry into two garbage dimensions; a `)` inside one could close the clause list early, and a keyword (`AS`, `USING`, `COMMENT`) inside one could be misread as structural syntax. The body parser's depth-0 entry splitter and its clause tokenizer now recognise dollar-quoting through the same tag grammar already used by comment-blanking and the `FROM YAML` extractor, so — matching DuckDB/PostgreSQL — the literal's contents are inert, and only the matching close tag ends the region (a different inner tag does not). A `$1` positional parameter and a lone `$` are unchanged (neither opens a dollar-quote).
- Documentation corrected against the implemented grammar and Snowflake's own syntax: the README no longer documents the `ONE TO ONE` / `ONE TO MANY` / `MANY TO ONE` cardinality annotations removed in v0.5.4 (cardinality is inferred from PK/UNIQUE constraints) and states the at-least-one-of-`DIMENSIONS`/`METRICS` rule correctly; the Snowflake comparison page no longer shows an `AS` keyword in the Snowflake `CREATE SEMANTIC VIEW` example (Snowflake has none) or an invalid `SEMANTIC_VIEW()` query form, adds pre-aggregation `WHERE` to the not-yet-supported list, and the DDL reference no longer shows `NON ADDITIVE BY` on a derived metric (which the parser rejects).

- **`NON ADDITIVE BY` snapshot polarity corrected to match Snowflake (breaking).** The rows are sorted by the non-additive dimensions and the rows sharing the *last ordering value* of that sort are aggregated (ties at that value all aggregate, via `RANK()`), so the default (ascending) direction now selects the **latest** snapshot and `DESC` selects the **earliest** — previously the mapping was inverted, and a view ported from Snowflake (or written to Snowflake's documented semantics) silently returned the opposite-end snapshot. **Migration**: a view that wrote `NON ADDITIVE BY (d DESC)` to get the latest snapshot should drop the `DESC` (write `NON ADDITIVE BY (d)`); one that wrote no direction to get the earliest should now add `DESC`. `NULLS` placement is unchanged and is kept as declared (only the direction is reversed internally). The default NULLS placement still follows the direction (`ASC` → `NULLS LAST`, `DESC` → `NULLS FIRST`), so a bare `NON ADDITIVE BY (d)` (latest, `NULLS LAST`) never lets a NULL key outrank a real snapshot, while `NON ADDITIVE BY (d DESC)` (earliest, `NULLS FIRST`) does; add an explicit `NULLS LAST` to exclude NULL keys regardless of direction.
- **Semi-additive snapshots are now deterministic when several fact rows tie at the snapshot value.** The snapshot CTE selects the rows at `RANK() = 1` rather than the previous `ROW_NUMBER()`, so when multiple rows share the last ordering value within a group — for example two accounts under one customer, each with a row at the latest date, grouped by customer — they now **all** aggregate together deterministically, whereas `ROW_NUMBER()` kept one arbitrary row. A group with a single row at the snapshot value is unaffected. (This is also why the fan-trap fence rejects an active semi-additive metric queried alongside a fanning child dimension — see the third fan-trap bullet above — since there `RANK` ties across fanned duplicates are indistinguishable from ties across distinct rows.)
- **A dimension reference written with a dotted qualifier (`o."order date"`) or quotes now resolves correctly on the window and semi-additive snapshot paths.** A window metric's `OVER (… ORDER BY / PARTITION BY [EXCLUDING] …)` reference and a `NON ADDITIVE BY` dimension reference were each accepted at `CREATE` but, at query time, matched against dimension names by **bare name only** — so they failed the required-dimension check even when the dimension was queried, and were emitted as a doubled-quote non-column in the generated `OVER` / snapshot `ORDER BY` clause (a clean bind-time failure). Every window and non-additive dimension reference — classification, grouping, partition/order emission, and the snapshot join — now resolves through the same bare-and-dotted, quote-aware resolver as the rest of the query layer and is emitted as the aggregation CTE's column alias, so the query binds and runs; a dotted or quoted reference classifies, partitions, orders, and joins exactly like its bare spelling, and when such a dimension is itself queried the metric is treated as effectively additive.
- A **window or semi-additive metric whose inner / base metric was declared with a quoted name** (e.g. `METRICS (li."Item Count" AS COUNT(*))` referenced by a window metric) now resolves that metric's fully-processed expression instead of silently falling back to its raw text. The map of resolved metric expressions was keyed by the stored name with its quotes retained, while every consumer looked it up by the quote-stripped, case-folded key — so a quoted-name base metric missed the lookup and lost both fact inlining and the `COUNT(*)` → `COUNT(<primary key>)` rewrite that excludes NULL-extended `LEFT JOIN` rows, silently overcounting. The resolved-expression map (and the companion `COUNT(*)`-without-primary-key tracking used for the `CountStarRequiresPrimaryKey` guard) are now keyed by the same canonical identifier key the rest of the pipeline already uses, so a quoted metric name resolves identically to its unquoted spelling.
- A **semi-additive metric that snapshots on a role-playing dimension** — a metric written `m USING (<relationship>) NON ADDITIVE BY (<dim on the role-played table>)`, where that dimension is not itself in the query — now selects the snapshot for the role its `USING` clause names, instead of an arbitrary one. When one table is joined through several distinctly-named relationships (e.g. `flights → airports` as `dep_airport` / `arr_airport`), the snapshot's ranking dimension was resolved to a bare join instance whose edge was the first-declared relationship, ignoring the metric's `USING` — so a metric scoped to the arrival airport could be ranked by the departure airport's value, silently returning the wrong snapshot (and emitting a redundant extra join). The non-additive dimension is now resolved through the same role-playing `USING` context as ordinary queried dimensions, so it ranks by — and joins — the correct role; a metric with such a dimension but *no* `USING` to disambiguate the role is now rejected as ambiguous at query time (the same error a directly-queried role-playing dimension raises) rather than snapshotting by an arbitrary role.
- **Role-playing ambiguity is now caught beyond one hop.** A table that plays multiple roles — reached from a single source table via two or more distinctly-named relationships (e.g. `flights → airports` as both departure and arrival) — makes a dimension directly on it ambiguous unless a co-queried metric's `USING` picks the role. (A table reached from *several different* child tables through one relationship each — fan-in onto a shared parent, not role-playing — stays unflagged.) Two shapes slipped past that check and silently bound to the first-declared relationship, a declaration-order-dependent wrong answer:
  - A **dimension on a *descendant* of a role-playing table** — e.g. `region_name` on `regions`, where `airports` references `regions` — reaches its table only *through* the role-playing table, so which role's rows it groups by was ambiguous, yet it silently used the departure instance. Such a query is now rejected. Unlike a dimension directly on the role-playing table, a descendant cannot be disambiguated by `USING`; give the target a distinct alias per role, or query it through a non-role-playing table.
  - A **fact sourced on a role-playing table (or a descendant of one)** now errors as well. Facts carry no `USING` context at all, so the role was always unresolvable — it previously read the first-declared relationship's rows silently.
- Ambiguous join "diamonds" are now rejected at `CREATE` instead of silently resolving to an arbitrary path. When a table is reachable from two *different* source tables (e.g. `orders → a → shared` and `orders → b → shared`), the join path is ambiguous; previously such a definition was accepted as long as the relationships were named, and a query then silently joined the shared table through whichever relationship was declared first — producing wrong numbers when the two paths point at different rows. Role-playing (multiple distinctly-named relationships from a *single* source table to one target, e.g. `flights → airports` via `dep`/`arr`) remains supported, and fan-in onto the base table is unaffected.
- Parse-error carets point more precisely at the offending token: an entry after a comma no longer drifts the caret left into the inter-entry whitespace, and a missing `(` after `NON ADDITIVE BY` / `USING` in a metric written with a leading `PRIVATE`/`PUBLIC` modifier now points at the expected-paren location instead of drifting left by the modifier's width.
- Dollar-quoted `FROM YAML` bodies now use one shared definition of a valid `$tag$` for both comment-blanking and extraction. A body opened with something that is not a valid tag — a digit-started `$1$` (`$1` is a bind parameter) or a tag containing whitespace — is rejected with a clear error instead of having the `--` / `/* */` runs inside its payload corrupted as if they were SQL comments.
- Derived-metric and fact expression inlining is now driven by one quote- and case-aware reference tokenizer, closing a class of substitution bugs: a metric/fact name appearing as the column part of a *different* table's qualified reference (`x.revenue`) or inside a single-quoted string literal (`'total revenue'`) is left untouched instead of being rewritten into invalid SQL, while a bare reference — written in any case, or double-quoted (`"Revenue"`) — inlines correctly against its declaration (a fact's own qualified form, `alias.name`, is still inlined). The fact/derived-metric CREATE-time validators and dependency scans share the same tokenizer, so they and the inliner can no longer disagree about what an expression references; a non-ASCII character abutting a name (`revenueΩ`) still never splits it into a spurious reference. Every remaining expression-text scanner now rides the same engine: a derived metric may reference a quoted metric name that contains a space (`"Total Revenue"`) and have it resolve as one reference at CREATE time (previously such a name was split and falsely reported as unknown), and the role-playing rewrite that scopes a dimension's source-table alias to a role alias touches only genuine qualifiers — a source-alias-like word inside a string literal, a same-named function call, or another table's qualified column is left intact rather than being corrupted by a blind text replace.
- Non-ASCII input no longer panics or corrupts: keyword scanning over UTF-8 text (`SHOW SEMANTIC VIEWS aΩΩ`, bodies containing multi-byte characters) previously raised "internal error (panic)", and `COMMENT` / `WITH SYNONYMS` payloads and quoted identifiers containing non-ASCII characters (`'café'`, `"café"`) were silently stored as mojibake. Error messages truncated to the FFI buffer are now cut on a character boundary instead of producing invalid UTF-8.
- DDL prefix keywords now require a word boundary: `DROP SEMANTIC VIEWS` (plural typo) no longer silently drops a view named `s`, and `CREATE SEMANTIC VIEWfoo` is no longer recognised.
- Name-only statements (`DROP` / `DESCRIBE` / `SHOW COLUMNS IN SEMANTIC VIEW`) now error on trailing garbage (`DROP SEMANTIC VIEW a b c`) instead of executing and silently discarding it; `ALTER` sub-operations do the same and now tolerate arbitrary whitespace between keywords.
- Body scanners are now quote- and string-aware: a `COMMENT = 'the PRIMARY KEY (id) lives here'` no longer fabricates a primary key from comment text; quoted identifiers containing commas, parens, or dots (`"a,b"`, `"tbl)x"`, `"a.b"`) no longer mis-split entries, close clauses early, or split at the inner dot; table-level `COMMENT` / `WITH SYNONYMS` on tables without PK/UNIQUE are stored instead of silently dropped; a column literally named `comment` is usable when quoted.
- SQL comments are handled correctly across the DDL surface: trailing comments are no longer absorbed into stored expressions or `ALTER ... RENAME TO` targets, comment text can no longer corrupt clause scanning, comments may appear between prefix keywords, and block comments nest per the SQL standard.
- `GET_DDL` output now re-parses to the same definition: relationships declared against a `UNIQUE` key render their `REFERENCES (columns)` list (previously dropped, silently rewiring the join to the primary key on re-parse), and view names that need quoting (mixed case, whitespace, non-ASCII) are quoted in the rendered header.
- `SHOW ... STARTS WITH` / `LIMIT` require word boundaries (`STARTSWITH`, `LIMIT5` are rejected); `NON ADDITIVE BY` accepts flexible whitespace including the no-space `BY(dim)` form; `READ_YAML_FROM_SEMANTIC_VIEW` resolves qualified names with quote awareness instead of splitting at dots inside quoted parts.
- Querying a semantic view whose stored relationships lack foreign-key column metadata (a legacy pre-Phase-24 definition format) now fails with a clear "re-create it with `CREATE OR REPLACE SEMANTIC VIEW`" error instead of silently skipping the fan-trap safety check and returning mis-aggregated results — the relationship graph builds empty for such rows, so the check would otherwise pass vacuously.
- A **window metric whose inner-metric reference resolves to no declared metric** no longer emits structurally invalid SQL. The reference was interpolated into the pre-aggregation CTE as a bare, unquoted column expression, so an inner metric written as a quoted identifier carrying special characters (an embedded quote, whitespace) leaked a lone quote character and produced unbalanced quotes. The fallback now quotes the reference like every other emitted identifier — matching the window `OVER`-clause dimension fallback — so the generated query is always well-formed (a broken reference now surfaces as a normal "column not found" error at execution rather than corrupt SQL). (Found by fuzzing.)
- **Quoted dimension, metric, and fact names no longer leak literal quote characters into output column names or materialization references.** A name declared with quotes (`"order date"`) is stored *with* its quotes, so re-quoting it at each emission site produced a triple-quoted alias (`... AS """order date"""`) — the result column was literally named `"order date"`, quote characters and all, and a materialization declared for such a name referenced a physically-impossible column, so routed queries bind-failed. Stored names are now stripped to their logical value (case preserved — quoted identifiers are case-sensitive) and quoted exactly once at every alias and CTE-reference site (the standard, semi-additive, and window paths, plus materialization routing), so the output column is named `order date` and materialization routing binds. Queries over unquoted names are unaffected. **Behaviour change:** the output column for a quoted-name dimension or metric is now named by its logical value (`order date`) rather than the quote-laden `"order date"`; a consumer selecting the old column name must update.

## [0.10.4] - 2026-06-27

### Changed

- DuckDB version pin bumped to `v1.5.4`.

## [0.10.3] - 2026-06-13

### Fixed

- **Windows community-extension build fixed against the updated MSVC toolchain.** The vendored DuckDB amalgamation bundles `fmt` 6.1.2, which on MSVC selected `stdext::checked_array_iterator` (under `#ifdef _SECURE_SCL`). Recent Microsoft STL versions removed that symbol entirely, so the `windows-latest` build began failing to compile the amalgamation with `error C2653: 'stdext': is not a class or namespace name`. The Windows build-time patch applied to `duckdb.cpp` now disables that branch so `fmt` takes its portable raw-pointer path — the same code every non-Windows platform already compiled. No functional or API changes from 0.10.2; this only restores the Windows build so the community-extensions registry can keep producing a Windows binary as new DuckDB versions ship.

## [0.10.2] - 2026-06-04

### Fixed

- **Passthrough facts named after their own column now work.** A fact whose expression references its own name — the natural 1:1 passthrough, e.g. `FACTS (s.unit_price AS s.unit_price)` — was rejected at `CREATE` time with `cycle detected in facts: unit_price -> unit_price`, and even when the expression differed slightly it expanded incorrectly. Two issues are fixed: (1) a fact's own name in its own expression is now treated as a reference to the physical column, not a self-cycle (genuine cycles between distinct facts are still rejected); and (2) fact-reference inlining replaced the qualified (`alias.name`) and unqualified (`name`) forms in two sequential passes, so for an identity fact the second pass re-scanned the first's output and produced corrupt SQL (`s.unit_price` → `(s.(s.unit_price))`) — replacement now happens in a single non-re-scanning pass. `DESCRIBE SELECT * FROM semantic_view('v', facts := ['unit_price'])` returns the declared fact name as the output column. Note: as in Snowflake, each clause entry reads `name AS expression` (logical name before `AS`, SQL expression after) — the reverse of a plain SQL `expression AS alias`.

## [0.10.1] - 2026-06-03

### Fixed

- **Community-extension build now loads on DuckDB 1.5.3.** The v0.10.0 binary published to the community-extensions registry was compiled against DuckDB 1.5.2 (the `duckdb`/`libduckdb-sys` crates were pinned to `=1.10502.0`) and stamped for v1.5.2, so on DuckDB 1.5.3 `INSTALL semantic_views FROM community; LOAD semantic_views;` failed with `The file was built specifically for DuckDB version 'v1.5.2' and can only be loaded with that version of DuckDB`. All DuckDB version pins are bumped to 1.5.3 — `.duckdb-version`, the `duckdb`/`libduckdb-sys` crates, the distribution workflows, and the Python test/example headers — so the rebuilt extension targets and loads on DuckDB 1.5.3. No functional or API changes from 0.10.0.

## [0.10.0] - 2026-05-27

Connection-lifecycle and ADBC fixes. Two downstream regressions reported against v0.8.0/v0.9.0 — an in-process `read_only=True` reopen that hung indefinitely, and `SELECT … FROM semantic_view(...)` failing with `Catalog Error: Table X does not exist` through ADBC — were both rooted in extension-owned long-lived `duckdb_connection` handles whose catalog/schema search path diverged from the caller's. v0.10.0 retires both long-lived handles and moves every read-side callback to a per-call `Connection(*context.db)` that inherits the caller's context natively. The two symptoms resolve as consequences. PK auto-inference from `duckdb_constraints()` is removed in the same release as the architectural pivot (see Changed).

### Changed

- **In-process `read_only=True` reopen no longer hangs.** After a writable handle that did `LOAD semantic_views` + `CREATE SEMANTIC VIEW` is closed, a subsequent `duckdb.connect(path, read_only=True)` against the same path returns within milliseconds in the same Python process. Previously the reopen hung indefinitely (>45 s observed) because the extension's long-lived catalog connection kept the `Database` alive past the caller's `close()`. Both extension-owned long-lived `duckdb_connection` handles (catalog read connection and query expansion connection) are retired from `init_extension`; every read-side and DDL-rewrite callback now opens its own per-call `Connection(*context.db)` borrowed from the caller's `ClientContext`. A structural Rust test (`tests/no_long_lived_conn.rs`) fails CI if anyone re-introduces a long-lived native handle inside `init_extension`.
- **`SELECT … FROM semantic_view(...)` now works through ADBC and other clients with diverging catalog search paths.** All seven physical-table emission sites in the expansion engine (main expansion, FACTS, semi-additive metrics, window metrics, materialization routing, and `EXPLAIN SEMANTIC VIEW`) now emit fully-qualified `database.schema.table` references. Previously the main expansion path was qualified (since v0.9.0) but the four feature paths still emitted unqualified `FROM "table"` references that resolved against the extension's separate connection; through ADBC the caller's catalog search path was not visible to the extension's connection, surfacing as `Catalog Error: Table X does not exist`. A new `just test-adbc-queries` recipe runs seven end-to-end ADBC scenarios covering main / FACTS / semi-additive / window / materialization / multi-DB ATTACH; all pass on v0.10.0 and fail on v0.9.0.
- **PK auto-inference from `duckdb_constraints()` removed (BREAKING).** When `TABLES (a AS t)` declared no `PRIMARY KEY` and `t` had a physical PK in the catalog, the extension previously imported the catalog PK at DDL-time. This auto-fallback is gone: PRIMARY KEY in a semantic view is now treated as a logical user assertion (the Snowflake-aligned model), not a physical catalog import. Users must explicitly declare `PRIMARY KEY (cols)` or `UNIQUE (cols)` in the TABLES clause, or use `REFERENCES target(cols)` shorthand on the foreign side. The DDL fails fast with a clear "primary key required" error pointing at the explicit-declaration alternative. Migration: add the explicit `PRIMARY KEY (...)` clause to any TABLES entry that previously relied on the auto-fallback.
- `semantic_view(...)` and the `SHOW`/`DESCRIBE` family now raise a clear `Binder Error` when DuckDB's column-type inference fails at bind time. Previously the bind path fell back to `VARCHAR` or `DECIMAL(18,3)` silently, masking the underlying problem. If a query that previously succeeded with the wrong column type now fails, the error message will name the underlying cause — typically a missing source table, a broken `expr`, or a permissions issue surfaced by the expanded SQL.

### Fixed

- `DROP SEMANTIC VIEW` and `ALTER SEMANTIC VIEW` against a read-only database that was never bootstrapped now report `semantic view 'X' does not exist`. Previously the lower-level `Catalog Error: Table _definitions does not exist` leaked through.
- Extension registration failures during `LOAD semantic_views` now surface the underlying DuckDB exception message in the user-visible error. Previously the message was dropped and callers saw only a generic `Failed to register …`, which made ADBC, JDBC, and Python users unable to diagnose load-time failures.
- `LOAD semantic_views` is now idempotent. Repeated loads in the same process no longer accumulate duplicate parser-extension hooks in `DBConfig`.
- Quoted source-table references with embedded whitespace or SQL keywords in the `TABLES (...)` clause (e.g. `TABLES (a AS "my table" PRIMARY KEY (id))`) are now parsed correctly. Previously the body parser tokenised on whitespace before respecting quoted-identifier boundaries, causing the source-table name to be truncated. Resolves TECH-DEBT #24.
- Non-additive dimension lists and window `OVER (ORDER BY ...)` clauses are now tokenised with identifier-quoting awareness, so quoted columns containing whitespace or commas no longer split across tokens.
- Removed unreachable single-shot fallback branches in four table-function exec callbacks that would have produced unbounded row streams if local state were ever absent. Table-function registration now refuses callbacks without an `init_local`, so the invariant is enforced at registration time rather than papered over per call.

### Security

- Eliminated a SQL-injection surface in the `CREATE SEMANTIC VIEW v FROM YAML FILE '<path>'` helper. The path argument is now read via DuckDB's `FileSystem` API directly, removing the `SELECT content FROM read_text('…')` indirection that relied on quote-doubling. `enable_external_access` gating is preserved natively by `LocalFileSystem`.

### Removed

- Internal `type_cache` module and `type_id_to_display_name` helper. Both were unused after the read-side rebuild in v0.10.0 and have been deleted (~317 LOC purge). No user-visible API impact.

## [0.9.0] - 2026-05-17

### Added

- **Read-only database LOAD support.** ``LOAD semantic_views`` now succeeds on a read-only DuckDB database. Previously-defined semantic views can be queried via ``list_semantic_views()``, ``describe_semantic_view()``, and ``FROM semantic_view(...)`` against a database opened with ``read_only=True`` (Python) or ``--readonly`` (CLI). On a read-only database that was never bootstrapped, the catalog is treated as empty rather than raising a missing-table error: ``list_semantic_views()`` returns zero rows, and ``describe_semantic_view('x')`` / ``FROM semantic_view('x', ...)`` return the standard ``semantic view 'x' does not exist`` error for any name.
- **Read-only DDL surfaces DuckDB's standard error.** ``CREATE SEMANTIC VIEW``, ``DROP SEMANTIC VIEW``, and ``ALTER SEMANTIC VIEW`` against a read-only database fail with DuckDB's standard ``Cannot execute statement of type "..." on database "..." which is attached in read-only mode!`` error rather than the previous confusing schema-create failure at LOAD time.
- **`examples/readonly_load.py`** demonstrating the open-writable → bootstrap → close → reopen-readonly → query → catch-DDL-error workflow.
- **`just test-readonly` recipe + `test/integration/test_readonly_load.py`** Python integration test (three scenarios: fresh read-only file, bootstrapped reopen, DDL rejection) and `test/sql/readonly_load.test` writable-side smoke fixture. Wired into `just test-all`.

### Fixed

- **Quoted identifier handling in `CREATE / DROP / ALTER / DESCRIBE / SHOW COLUMNS SEMANTIC VIEW`.** All five DDL forms now accept any combination of quoted, partially-quoted, and unquoted fully-qualified names — e.g. `CREATE OR REPLACE SEMANTIC VIEW "memory"."main"."orders_sv" AS ...`, `CREATE SEMANTIC VIEW main."orders_sv" AS ...`, and `CREATE SEMANTIC VIEW orders_sv AS ...` all store the same bare key, and `FROM semantic_view('orders_sv', ...)` resolves them uniformly. `ALTER ... RENAME TO` normalises both the source name and the new-name target. Error messages reference the unquoted bare name. Previously, a quoted FQN was stored verbatim (quotes and all) as the lookup key, which made any subsequent `semantic_view('orders_sv', ...)` call return "view does not exist".
- **Triple-quoted identifiers in expanded SQL.** When a `TABLES (o AS "memory"."main"."orders" ...)` clause used a quoted source-table reference, the expansion path re-quoted each part producing strings like `"""memory"""."""main"""."""orders"""` in the generated `FROM` clause. The expansion now operates on parsed identifier parts and emits exactly one pair of quotes per part regardless of input shape, restoring `EXPLAIN SEMANTIC VIEW` legibility and (in the rare case the source-table reference had embedded special chars) correctness of the generated SQL.

### Known limitations

- The v0.1.0 → v0.2.0 companion-file migration cannot run on a read-only database (the migration INSERTs into ``semantic_layer._definitions`` which requires write access). Practical impact is near-zero — the companion-file format is four milestone versions stale and any database last opened with v0.2.0+ has already been migrated. If you have a v0.1.0-era database that has never been opened by any newer release, open it once writable to complete the migration before reverting to read-only.

## [0.8.0] - 2026-05-06

### Added

- **Transactional DDL.** `CREATE`, `DROP`, and `ALTER SEMANTIC VIEW` now participate in the caller's transaction. `BEGIN ... ROLLBACK` rolls back uncommitted catalog changes and `BEGIN ... COMMIT` persists them, matching the contract that ADBC, dbt, and other transaction-aware clients expect.
- **`parser_override` extension hook.** Recognised DDL is rewritten into native `INSERT` / `UPDATE` / `DELETE` against `semantic_layer._definitions` and executed on the caller's connection. Non-matching statements fall through to DuckDB's default parser unchanged.
- **All four `CREATE` forms transactional:** inline `AS` keyword body, inline `FROM YAML $$ ... $$`, `FROM YAML FILE '<path>'` (including `https://` and S3 paths via httpfs), and `CREATE OR REPLACE` / `CREATE IF NOT EXISTS` variants.
- **DROP / ALTER race guards.** Non-`IF EXISTS` `DROP SEMANTIC VIEW` and `ALTER SEMANTIC VIEW … RENAME / SET COMMENT / UNSET COMMENT` now emit a snapshot-consistent existence check on the caller's connection before the DML. If a concurrent commit lands between the catalog pre-check (committed-state read on a separate connection) and the DML, the user sees `semantic view '<name>' was concurrently dropped` instead of a silent no-op. `IF EXISTS` variants keep their silent-no-op contract.
- **`CatalogReader` RAII.** `prepared_lookup` and `execute_list_all` use internal `PreparedStmt` and `QueryResult` guards. Manual `duckdb_destroy_*` calls along error paths are gone.
- **`ParserOptions` size assert.** A static assert pins `sizeof(ParserOptions) == 32` against DuckDB v1.5.2 (the upstream version pinned via the `=1.10502.0` duckdb-rs crate). Silent layout drift previously surfaced as garbage parser errors at position 0; future DuckDB bumps now fail fast at compile time.
- **Actionable error when `allow_parser_override_extension` is `DEFAULT` or `STRICT`** (e.g. after `CALL disable_peg_parser()` resets the setting). Issuing semantic DDL on such a connection now produces `Parser Error: semantic_views: parser_override is not active for this connection (allow_parser_override_extension is 'DEFAULT' or 'STRICT'). Re-enable with: SET allow_parser_override_extension='FALLBACK';` with caret positioned at the start of the statement.
- **ADBC end-to-end test** (`test/integration/test_adbc_transactions.py`, runnable via `just test-adbc`) exercising `autocommit=False` rollback / commit semantics for inline, FROM YAML FILE, ALTER, and DROP forms — proves the original ADBC bug is fixed end-to-end.
- **Concurrent-CREATE Python integration test** (`test/integration/test_concurrent_ddl.py`, runnable via `just test-concurrent`).
- **`INSERT OR REPLACE` row-count, byte-identical rollback (MD5), and same-txn `list_semantic_views` visibility cases** in `v080_transactional_ddl.test`.
- **Type-inference under `BEGIN/COMMIT`** in `test_type_inference.py`.
- **Arbitrary-bytes FFI fuzz target** (`fuzz_parser_override_ffi`).
- **Caret-rendering sqllogictest fixtures** pinning caret alignment across CREATE / DROP / ALTER / multi-line / UTF-8 / multi-DB / extension-reload paths.
- **`peg_compat.test` regression coverage** that the override path keeps working under DuckDB's experimental PEG parser, so v0.8.0's transactional DDL survives the upcoming parser switch. Under PEG, every DDL form (including `DESCRIBE` and `SHOW`) works because parser_override fires before whichever parser is active.

### Changed

- **Architectural unification.** `parser_override` is the sole DDL entry point. Every recognised form — `CREATE` (all four variants), `DROP`, `ALTER`, `DESCRIBE`, `SHOW SEMANTIC *`, `GET_DDL`, `READ_YAML_FROM_SEMANTIC_VIEW` — is rewritten by a single Rust dispatch and re-parsed by DuckDB on the caller's connection. The legacy `parse_function` / `sv_ddl_internal` table-function fallback was retired (~1500 LOC net deletion). One execution path means transactional semantics, error reporting, and PEG/Bison compatibility are all uniform.
- **`CatalogState` HashMap removed.** All catalog reads now query `_definitions` directly through a single shared `CatalogReader`. This eliminates the divergence risk between the HashMap and the on-disk table that the old write-through-both pattern carried.

### Fixed

- **FFI UTF-8 hardening.** `sv_parser_override_rust` now validates input bytes with checked `from_utf8` instead of `from_utf8_unchecked`. Malformed input cleanly defers to the default parser instead of triggering UB.
- **`parse_table_function_call` tightening.** The internal helper now rejects `foo(,)`, `foo('a',)` (trailing comma), and `foo('a' 'b')` (missing comma between args). Previously these silently parsed as zero-arg or merged-arg calls.
- **Validation errors arrive as parse-time errors with caret rendering.** `CREATE`, `DROP`, and `ALTER SEMANTIC VIEW` validation failures (e.g. `semantic view 'X' does not exist`, unknown clause) surface as `Parser Error: ... LINE 1: ... ^` with the caret aligned to the offending token, matching DuckDB's native parser-error rendering. Internally, `parser_override` keeps the success / transactional path (rewrite to native SQL, re-parse on caller's connection); validation failures defer (`DISPLAY_ORIGINAL_ERROR`), the default parser fails on the unrecognised DDL prefix, and DuckDB calls `parse_function`, which re-runs validation and returns `DISPLAY_EXTENSION_ERROR` with `error_location` set to the offending byte offset.

### Known limitations

- `semantic_view(...)` queries do not see uncommitted writes to user tables in the same transaction. Expansion runs on a separate `query_conn`, which only sees committed state. Workaround: commit the user-table writes before querying. Inline expansion will be revisited when DuckDB 2.0's PEG grammar-extension API ships.
- A `CREATE SEMANTIC VIEW` issued in the same uncommitted transaction is not visible to subsequent reads in that transaction (e.g. `SHOW SEMANTIC VIEWS` will not list it until commit). With the HashMap gone, reads see only committed catalog state. Workaround: commit before reading. See TECH-DEBT item 19.
- `CALL disable_peg_parser()` resets `allow_parser_override_extension` to `default`, which silently bypasses parser_override hooks. Workaround: re-issue `SET allow_parser_override_extension='FALLBACK'` after disabling PEG. The extension installs `FALLBACK` on load, so a process that never enables PEG never sees this. See TECH-DEBT item 21.
- `CREATE SEMANTIC VIEW IF NOT EXISTS` is silent-no-op only against rows visible in the caller's MVCC snapshot. Two parallel processes that each see the row absent will both attempt the INSERT and the loser sees `ConstraintException: Duplicate key "name: <view>" violates primary key constraint` at commit — the same shape plain `CREATE` produces under contention. Multi-process bootstrap scripts should catch this and treat it as success. See TECH-DEBT item 23.

## [0.7.2] - 2026-05-01

### Fixed

- Parser hook now strips leading SQL comments before matching `CREATE / ALTER / DROP / SHOW SEMANTIC VIEW` DDL. Previously, any statement preceded by a `/* ... */` block comment or `-- ... \n` line comment was misclassified as not-our-statement and DuckDB surfaced `Parser Error: syntax error at or near "SEMANTIC"`. This made the extension unusable through dbt-duckdb (which unconditionally prepends a query annotation comment to every statement) and any other tool that prefixes annotations (sqlfluff, BI tools that prepend session/user metadata, etc.). Reported and diagnosed by an external user. Block comments are non-nesting, matching PostgreSQL/DuckDB semantics. Error-position byte offsets are preserved across the consumed comment span, so error carets continue to reference the original query string.

## [0.7.1] - 2026-04-26

### Added

- DDL-time type inference for dimensions and metrics: `data_type` / `DATA_TYPE` columns in SHOW and DESCRIBE output now display inferred types (VARCHAR, BIGINT, DOUBLE, DATE, etc.) instead of empty strings
- Type inference runs automatically at `CREATE SEMANTIC VIEW` time on file-backed databases via a LIMIT 0 probe query
- Supported types: VARCHAR, BOOLEAN, integer types (TINYINT through UBIGINT), FLOAT, DOUBLE, DATE, TIME, TIMESTAMP (all variants), INTERVAL, UUID, BLOB, BIT; DECIMAL and parameterized types intentionally left empty to avoid lossy CAST
- Derived metrics also receive inferred types when resolvable
- In-memory databases continue to show empty `data_type` (no persist connection available)

## [0.7.0] - 2026-04-24

### Added

- YAML definition format: `CREATE SEMANTIC VIEW name FROM YAML $$ ... $$` as an alternative to SQL DDL keyword body
- YAML file loading: `CREATE SEMANTIC VIEW name FROM YAML FILE '/path/to/file.yaml'` with DuckDB `enable_external_access` security enforcement
- Dollar-quoting for inline YAML: both untagged (`$$...$$`) and tagged (`$yaml$...$yaml$`) forms
- YAML export: `READ_YAML_FROM_SEMANTIC_VIEW('name')` scalar function with lossless round-trip fidelity
- Materialization declarations: `MATERIALIZATIONS` clause in SQL DDL and YAML for declaring pre-aggregated tables
- Materialization routing engine: transparent query redirection to pre-aggregated tables on exact dimension/metric match
- Semi-additive and window function metrics excluded from materialization routing (always expand from raw sources)
- `explain_semantic_view()` now includes materialization routing decision (`-- Materialization: <name>` or `-- Materialization: none`)
- `DESCRIBE SEMANTIC VIEW` includes MATERIALIZATION rows with table, dimensions, and metrics properties
- `SHOW SEMANTIC MATERIALIZATIONS [IN view_name]` command with LIKE/STARTS WITH/LIMIT filtering

## [0.6.0] - 2026-04-14

### Added

- Metadata annotations: COMMENT, SYNONYMS (aliases), PRIVATE/PUBLIC access modifiers on views, tables, dimensions, metrics, and facts
- ALTER SEMANTIC VIEW SET COMMENT / UNSET COMMENT DDL for modifying view-level comments after creation
- GET_DDL('SEMANTIC_VIEW', 'name') scalar function for reconstructing re-executable CREATE OR REPLACE DDL from stored definitions
- SHOW TERSE SEMANTIC VIEWS for reduced-column introspection output
- SHOW COLUMNS IN SEMANTIC VIEW for a unified list of all dims, facts, and metrics with a `kind` column
- IN SCHEMA / IN DATABASE scope filtering for all SHOW SEMANTIC commands
- Wildcard selection (`table_alias.*`) in dimensions and metrics query parameters, expanding to all matching PUBLIC items
- Queryable FACTS via `facts := [...]` parameter in the table function for row-level unaggregated results
- Semi-additive metrics via NON ADDITIVE BY (dimension [ASC|DESC] [NULLS FIRST|LAST]) for snapshot-style aggregation using CTE-based ROW_NUMBER
- Window function metrics via PARTITION BY EXCLUDING for non-aggregated, partition-aware computation
- Synonyms and comment columns in all SHOW SEMANTIC command output
- Comment and access_modifier properties in DESCRIBE SEMANTIC VIEW output
- Mutual exclusion: facts + metrics in same query produces a blocking error
- Mutual exclusion: window function metrics + aggregate metrics in same query produces a blocking error
- SHOW SEMANTIC DIMENSIONS FOR METRIC shows `required=TRUE` for window partition dimensions

### Changed

- FFI catch_unwind wrapping on all 25 entry points (Rust panics no longer unwind through C++ stack frames)
- Graceful lock-poison handling across all catalog and query paths (error return instead of panic)
- Cycle detection and MAX_DERIVATION_DEPTH=64 limit for derived metrics and facts
- DimensionName/MetricName newtypes with case-insensitive semantics replace bare strings in query resolution
- Resolution loop deduplication via generic resolve_names helper

## [0.5.5] - 2026-04-05

### Added

- Snowflake-aligned column schemas for all SHOW SEMANTIC commands (VIEWS, DIMENSIONS, METRICS, FACTS)
- Snowflake-aligned DESCRIBE SEMANTIC VIEW property-per-row format
- Metadata fields: created_on timestamp, database_name, schema_name on semantic view model
- Per-fact output_type metadata

### Changed

- Refactored expand.rs into expand/ module directory (7 submodules)
- Refactored graph.rs into graph/ module directory (5 submodules)
- Extracted shared util.rs and errors.rs as leaf modules to break circular dependencies

## [0.5.4] - 2026-03-31

### Added

- UNIQUE constraints on tables in TABLES clause with automatic cardinality inference for relationships
- Implicit PK reference resolution (REFERENCES target without column list resolves to target's PRIMARY KEY)
- ALTER SEMANTIC VIEW RENAME TO for renaming views
- SHOW SEMANTIC DIMENSIONS / METRICS / FACTS introspection commands
- LIKE, STARTS WITH, and LIMIT filtering for all SHOW SEMANTIC commands
- Documentation site (Sphinx + Shibuya theme on GitHub Pages)
- Community Extension Registry descriptor (description.yml)
- MAINTAINER.md contributor documentation

### Changed

- DuckDB version support: 1.5.x (latest) + 1.4.x LTS with dual CI matrix
- Relationship cardinality inferred from PK/UNIQUE constraints instead of explicit keywords

### Removed

- Explicit cardinality keywords on relationships (breaking: views must be recreated)

## [0.5.3] - 2026-03-15

### Added

- FACTS clause for named reusable row-level sub-expressions in semantic view definitions
- Derived metrics (metric-on-metric composition with DAG resolution and cycle detection)
- Fan trap detection with blocking errors for one-to-many aggregation fan-out
- Role-playing dimensions (same table via multiple join paths)
- USING RELATIONSHIPS clause for explicit join path selection in queries
- Multi-level fact inlining with proper parenthesization for operator precedence

## [0.5.2] - 2026-03-13

### Added

- SQL keyword DDL body: TABLES, RELATIONSHIPS, DIMENSIONS, METRICS clauses replace function-call syntax
- PK/FK relationship model with table aliases and graph-validated JOIN synthesis
- Alias-based query expansion with qualified column names (direct FROM+JOIN instead of CTE flattening)
- Parser robustness: token-based keyword matching tolerates arbitrary whitespace
- Adversarial input hardening (null bytes, embedded semicolons, Unicode homoglyphs, control characters)

### Removed

- Function-call DDL body syntax (breaking: `define_semantic_view()` interface retired)

## [0.5.1] - 2026-03-09

### Added

- DROP SEMANTIC VIEW and DROP SEMANTIC VIEW IF EXISTS
- CREATE OR REPLACE SEMANTIC VIEW
- CREATE SEMANTIC VIEW IF NOT EXISTS
- DESCRIBE SEMANTIC VIEW
- SHOW SEMANTIC VIEWS
- Error location reporting with character positions (caret indicators in DuckDB output)
- Clause-level error hints and "did you mean?" fuzzy suggestions for misspelled clause/view names
- Parser property-based tests (proptests) for DDL parsing

## [0.5.0] - 2026-03-08

### Added

- Native `CREATE SEMANTIC VIEW` DDL syntax via C++ parser extension hook
- Parser fallback hook registration (C_STRUCT entry + C++ helper)
- Rust FFI trampoline for detecting `CREATE SEMANTIC VIEW` prefix
- Statement rewriting pipeline (native DDL to function-based execution)
- Dedicated DDL connection to avoid lock conflicts

## [0.4.0] - 2026-03-03

### Changed

- Time truncation expressed via dimension `expr` directly (e.g., `date_trunc('month', created_at)`)
- DDL simplified from 6 to 4 named parameters
- Query function simplified from 3 to 2 named parameters

### Removed

- `time_dimensions` DDL parameter (breaking)
- `granularities` query parameter (breaking)

## [0.3.0] - 2026-03-03

### Changed

- Replaced binary-read dispatch with zero-copy vector references (`duckdb_vector_reference_vector`)
- Streaming chunk-by-chunk output instead of collect-all-then-write
- Type mismatches handled at SQL generation time via `build_execution_sql` cast wrapper

### Removed

- ~600 LOC of per-type read/write dispatch code

## [0.2.0] - 2026-03-03

### Added

- C++ shim infrastructure for Rust+C++ boundary (vendored DuckDB amalgamation via cc crate)
- Time dimensions with granularity coarsening and per-query granularity override
- `pragma_query_t` catalog persistence (replaced sidecar file with DuckDB-native table persistence)
- Scalar function DDL interface (`define_semantic_view()`)
- Snowflake-aligned STRUCT/LIST DDL syntax
- EXPLAIN support for expanded SQL inspection
- Typed output columns (zero-copy vector reference with runtime type validation)
- DuckDB type-mapping with property-based tests
- DuckLake integration test suite and CI

### Removed

- Sidecar file persistence (replaced by pragma_query_t)

## [0.1.0] - 2026-02-28

### Added

- Initial extension scaffold using `duckdb/extension-template-rs`
- Multi-platform CI build matrix (Linux x86_64/arm64, macOS x86_64/arm64, Windows x86_64)
- Scheduled DuckDB version monitor with automated PR creation
- Code quality gates: `rustfmt`, `clippy` (pedantic), `cargo-deny`, 80% coverage
- Developer task runner (`just`) with `just setup` one-command dev environment
- Pre-commit hooks via `cargo-husky` (rustfmt + clippy)
- Semantic view definition storage and round-trip persistence across DuckDB restarts
- Expansion engine: automatic GROUP BY and JOIN generation from dimension/metric declarations
- Query interface via table function `semantic_view('view', dimensions := [...], metrics := [...])`
- `list_semantic_views()` and `describe_semantic_view()` introspection functions
- Fuzz targets for FFI boundary testing

[Unreleased]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.17.0...HEAD
[0.17.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.16.0...v0.17.0
[0.16.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.15.0...v0.16.0
[0.15.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.14.1...v0.15.0
[0.14.2]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.14.1...v0.14.2
[0.14.1]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.14.0...v0.14.1
[0.14.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.12.1...v0.13.0
[0.12.1]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.12.0...v0.12.1
[0.12.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.10.4...v0.11.0
[0.10.4]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.10.3...v0.10.4
[0.10.3]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.10.2...v0.10.3
[0.10.2]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.10.1...v0.10.2
[0.10.1]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.8.0...v0.9.0
[0.8.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.7.2...v0.8.0
[0.7.2]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.5...v0.6.0
[0.5.5]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.4...v0.5.5
[0.5.4]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.3...v0.5.4
[0.5.3]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.2...tags/v0.5.3
[0.5.2]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/s-layer/duckdb-semantic-views/compare/v1.0...v0.2.0
[0.1.0]: https://github.com/s-layer/duckdb-semantic-views/releases/tag/v1.0
