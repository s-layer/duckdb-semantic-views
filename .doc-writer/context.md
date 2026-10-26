# Doc Writer Context

**Generated:** 2026-03-21
**Source:** /doc-writer:setup researcher
**Editable:** Yes -- manual edits are preserved until the next --refresh-context run. To make permanent changes, edit config.yaml and re-run setup.

## Project Summary

DuckDB Semantic Views is a loadable DuckDB extension (written in Rust + C++) that implements semantic views -- a declarative layer where you define dimensions, metrics, relationships, and facts once, then query them in any combination. The extension writes all GROUP BY and JOIN logic automatically, joining only the tables needed for each query. Inspired by and closely modeled on Snowflake Semantic Views, it targets data engineers who want an open-source, DuckDB-native alternative. The user interface is native SQL DDL (`CREATE SEMANTIC VIEW ... AS`) and, for querying, the `SEMANTIC_VIEW(...)` construct and the `semantic_view()` table function it lowers to. The project is pre-release (v0.5.3), not yet on the DuckDB community extension registry.

Inferred from: README.md, Cargo.toml (`semantic_views` v0.5.0, MIT), source module structure (model, expand, graph, body_parser, catalog, parse, ddl, query).

## User Persona: Data engineers exploring semantic views

### Profile
- **Skill level:** Intermediate
- **What they know:**
  - SQL: fluent in SELECT, JOIN, GROUP BY, window functions, CTEs, and subqueries. Understands query execution plans and can reason about join order and aggregation behavior.
  - DuckDB basics: knows how to install DuckDB, load extensions, run queries from CLI or Python, and connect to file-based sources (CSV, Parquet). Familiar with DuckDB's in-process architecture and analytical focus.
  - Data engineering concepts: understands ETL/ELT pipelines, data warehousing patterns (star/snowflake schemas, fact and dimension tables), and the role of a transformation layer.
  - Data source connectivity: experienced connecting analytics tools to CSV, Parquet, Postgres, and Iceberg table formats. Understands the value of querying data in place without copying it.
  - Familiarity with Snowflake/Databricks ecosystems: has used or evaluated Snowflake's semantic views or Databricks' semantic layer features. Understands the concept of a managed semantic layer but wants an open-source, local-first alternative.
- **What to always explain:**
  - Semantic view concepts: always explain what a semantic view is and how it differs from a regular SQL view. A regular view stores a fixed query; a semantic view stores a model (dimensions, metrics, relationships) and generates queries on demand based on what the user requests.
  - DDL syntax: always show the full `CREATE SEMANTIC VIEW` syntax with clause order (TABLES, RELATIONSHIPS, FACTS, DIMENSIONS, METRICS). Never assume the reader has memorized the grammar. Each clause's purpose and structure must be introduced before use.
  - Modeling best practices: always explain why certain modeling choices are preferred -- when to use FACTS vs inline expressions, when relationships need cardinality annotations, how to name dimensions and metrics for clarity, and how to avoid fan traps.
  - Differences from Snowflake/Databricks: always call out where this extension's syntax or behavior differs from Snowflake's SQL DDL interface (`CREATE SEMANTIC VIEW`) or Databricks semantic layer. Readers coming from those platforms will assume identical behavior unless told otherwise. **Important:** Snowflake has two distinct interfaces — the SQL DDL (`CREATE SEMANTIC VIEW`, the newer recommended approach) and the YAML spec (`CREATE SEMANTIC VIEW FROM YAML`, the older Cortex Analyst approach). All comparisons must target the SQL DDL interface only. The YAML spec has extra concepts (e.g., `time_dimensions`, `custom_instructions`, `access_modifier`) that exist to serve the AI SQL generation layer (Cortex Analyst) and have no equivalent in the SQL DDL. Do not reference or compare against YAML-spec-only features.
  - Relationship modeling: always explain how PK/FK relationships work in semantic views -- the TABLES clause with PRIMARY KEY, the RELATIONSHIPS clause with REFERENCES, cardinality annotations (MANY TO ONE, ONE TO ONE), and how the extension decides which tables to join.
  - Dimension/metric definitions: always explain the anatomy of a dimension (name, expression, source table) and a metric (name, aggregate expression, source table, optional USING clause). Show how the extension maps these to SELECT and GROUP BY.
- **Their world:** These are analytics engineers and data engineers working in the modern data stack. They build and maintain data pipelines, define metrics and dimensions for reporting, and serve data to dashboards and analytics applications. Their daily work involves writing SQL transformations, managing data models, and ensuring consistent metric definitions across the organization. They face the problem of metric inconsistency -- different teams computing revenue or churn differently -- and see semantic layers as the solution. They already use tools like dbt, Airflow, and BI platforms. They are evaluating DuckDB as a lightweight, embeddable analytics engine -- particularly attractive for the "DuckDB + Iceberg tables + analytics web app" stack where DuckDB runs inside an application server, queries Iceberg tables directly, and serves results to a web frontend without a separate warehouse.
- **How they found this library:** They heard about semantic views through Snowflake's feature announcements or Databricks' semantic layer marketing. They liked the concept -- define metrics once, query flexibly -- but wanted an open-source alternative that runs locally on DuckDB. They searched for "DuckDB semantic layer" or "DuckDB semantic views" and found this extension. Some discovered it while exploring the DuckDB community extensions ecosystem. They are comparing it against dbt's semantic layer, Cube.dev, and writing raw SQL views.

### Common Tasks
- **Define semantic views with dimensions, metrics, and relationships:** Write `CREATE SEMANTIC VIEW` statements that declare tables with PKs, relationships between them, and the dimensions/metrics to expose. Involves choosing table aliases, defining join paths, writing aggregate expressions, and optionally adding FACTS for reusable row-level calculations.
- **Query semantic views:** Use `SELECT * FROM SEMANTIC_VIEW(name DIMENSIONS ... METRICS ... WHERE ...)`, Snowflake's construct (since v0.16.0), to request any combination of dimensions and metrics, or expressions over them; `semantic_view('name', dimensions := [...], metrics := [...])` is the equivalent table function for application code. Understand the three query modes: dimensions-only (distinct values), metrics-only (grand totals), and both (grouped aggregation). Filter the rows the metrics aggregate with the WHERE inside the construct, and the result with WHERE on the outer query.
- **Connect diverse data sources:** Load data from Parquet files, Iceberg tables, Postgres (via `postgres_scanner`), CSV, or other DuckDB-supported sources, then define semantic views over those tables. Understand that semantic views work over any table DuckDB can see.
- **Compare features with Snowflake/Databricks:** Evaluate which Snowflake Semantic View features are supported, which behave differently, and which are not yet implemented. Make informed decisions about whether this extension fits their use case.
- **Model semantic layers following best practices:** Design star/snowflake schemas with proper relationship declarations, use FACTS for shared calculations, apply cardinality annotations to catch fan traps, and use derived metrics for composed calculations like profit margins.
- **Build a DuckDB + Iceberg + analytics app stack:** Set up DuckDB as an embedded engine in an application server, connect it to Iceberg tables (via `iceberg` extension), define semantic views for the data model, and serve query results to a web application frontend.

### Writing Guidance for This Persona
- When explaining DDL syntax, assume they understand SQL grammar conventions (keywords, clauses, optional elements) but spell out the specific clause order and semantics unique to semantic views
- When showing multi-table examples, assume they understand star schemas and PK/FK joins but spell out how the RELATIONSHIPS clause maps to those concepts
- Use data engineering terminology naturally (fact table, dimension table, grain, fan-out, cardinality) but define semantic-view-specific terms (FACTS clause, derived metrics, role-playing dimensions, USING RELATIONSHIPS)
- Examples should show realistic analytics scenarios: revenue by region, customer segmentation, product performance, time-series aggregations over Iceberg tables
- When comparing to Snowflake, always compare against their SQL DDL interface (`CREATE SEMANTIC VIEW`), never the YAML spec. Use Snowflake's SQL DDL syntax as a known reference point ("If you have used Snowflake's CREATE SEMANTIC VIEW, the TABLES clause works the same way, but...")
- Include the generated SQL (via `explain_semantic_view`) in examples so readers can verify the extension does what they expect

## Tone: warm-businesslike

### Writing Rules
- Brief (1-2 sentence) introduction explaining what the page covers and why it matters for the reader's workflow
- Multiple examples per concept: basic single-table usage first, then multi-table with relationships, then advanced features (FACTS, derived metrics, USING)
- Include "Common Mistakes" or "Troubleshooting" sections where relevant -- especially around fan traps, ambiguous join paths, and syntax errors
- Transition sentences between major sections for reading flow
- Use admonitions for tips (best practices), warnings (gotchas and Snowflake differences), and important notes (prerequisites)
- Warm but professional -- no jokes, no first person, no casual asides
- When documenting Snowflake differences, use a consistent pattern: state what Snowflake's SQL DDL does, state what this extension does differently, explain why or what to do instead. Never reference Snowflake's YAML spec or Cortex Analyst features in comparisons.

## Framework Preferences: sphinx-shibuya

### Navigation
- Top navbar tabs using Diataxis quadrants via `nav_links` in conf.py: Tutorials, How-To Guides, Explanation, Reference
- Left sidebar shows contents of the current section (scoped to the active tab)
- Left nav limited to 3 levels maximum (section heading, page, sub-page)
- Each Diataxis section has an `index.rst` landing page with a toctree listing its pages
- Root `index.rst` uses a hidden toctree listing all section indexes

### Features to Use
- **Admonitions** (`.. tip::`, `.. warning::`, `.. danger::`, `.. note::`, `.. versionadded::`): use specific types, not generic `.. note::` for everything. Do not stack multiple admonitions back-to-back; consolidate into one with a list.
- **Code blocks** (`.. code-block:: sql`): always specify the language (almost always `sql` for this project). Use `:emphasize-lines:` to highlight critical lines. Use `:caption:` for file paths when showing config files.
- **Tab sets** (`.. tab-set::` / `.. tab-item::`): use for alternative approaches (e.g., single-table vs multi-table setup, different data source connections). Use `:sync-group:` to synchronize related tab sets across a page.
- **Grids and cards** (`.. grid::` / `.. grid-item-card::`): use on section landing pages and the home page for feature overviews and navigation. Grid values `1 2 3 3` for responsive layout.
- **Dropdowns** (`.. dropdown::`): use for advanced or optional content most readers can skip (e.g., "How the generated SQL works under the hood").
- **Cross-references** (`:ref:`label``): use `:ref:` with labels for all internal links. Place `.. _label-name:` above headings. Avoid `:doc:` path references.
- **Intersphinx**: configured for DuckDB docs (`duckdb`). Use `:ref:`duckdb:...`` for linking to DuckDB documentation where helpful.

### Features NOT to Use
- **sphinx-autoapi / autodoc**: not configured. This is a Rust+C++ extension with a SQL interface; there is no Python API to auto-document. API reference pages are written manually as SQL syntax reference.
- **MyST parser**: not configured. All documentation uses reStructuredText (RST), not Markdown.
- **Bare `::` code blocks**: never use for copyable code. Always use `.. code-block:: sql` with explicit language.
- **`:doc:` references**: strongly discouraged. Use `:ref:` with labels so links survive page moves.

### API Reference Style
- Manual pages, not auto-generated
- Follow Snowflake's SQL reference page pattern: show the grammar/syntax of the statement at the top of the page, then describe each clause and its options in turn, then illustrate with code examples
- Any behavioral differences from Snowflake's SQL DDL should be called out with a `.. warning::` admonition
- Group reference pages by DDL verb: CREATE, DROP, DESCRIBE, SHOW, plus the `semantic_view()` and `explain_semantic_view()` query functions
