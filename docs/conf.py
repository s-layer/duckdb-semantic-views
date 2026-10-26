import os
import sys

sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__))))

# Opinionated defaults for Sphinx + Shibuya theme projects
# Source: based on Shibuya official docs and sphinx-autoapi best practices
# Template variables: {project_name}, {author_name}, {copyright_year}, {github_url}
# Used by /doc-writer:setup when no conf.py exists in the project

# -- Project metadata -------------------------------------------------------

project = "DuckDB Semantic Views"
author = "Anentropic"
copyright = "2026, Anentropic"

# -- Path exclusions ----------------------------------------------------------

exclude_patterns = ["_build", ".venv"]

# -- Extensions --------------------------------------------------------------

extensions = [
    "sphinx_design",
    "sphinx.ext.githubpages",
    "myst_parser",
]

# -- MyST parser -------------------------------------------------------------

myst_enable_extensions = ["colon_fence"]
myst_heading_anchors = 3
suppress_warnings = ["myst.header"]

# -- Shibuya theme -----------------------------------------------------------

html_theme = "shibuya"
html_theme_options = {
    "accent_color": "orange",
    "color_mode": "auto",
    "github_url": "https://github.com/s-layer/duckdb-semantic-views",
    "toctree_maxdepth": 3,
    "toctree_titles_only": True,
    "nav_links": [
        {"title": "Overview", "url": "index"},
        {
            "title": "Tutorials",
            "url": "tutorials/index",
            "children": [
                {
                    "title": "Getting Started",
                    "url": "tutorials/getting-started",
                    "summary": "Install the extension and run your first query in 5 minutes",
                },
                {
                    "title": "Multi-Table Semantic Views",
                    "url": "tutorials/multi-table",
                    "summary": "Model multi-table schemas with relationships",
                },
                {
                    "title": "Building a Model",
                    "url": "tutorials/building-a-model",
                    "summary": "Build a realistic model with facts and derived metrics",
                },
            ],
        },
        {
            "title": "How-To Guides",
            "url": "how-to/index",
            "children": [
                {
                    "title": "FACTS",
                    "url": "how-to/facts",
                    "summary": "Define reusable row-level expressions referenced in metrics",
                },
                {
                    "title": "Derived Metrics",
                    "url": "how-to/derived-metrics",
                    "summary": "Compose metrics from other metrics using arithmetic",
                },
                {
                    "title": "Role-Playing Dimensions",
                    "url": "how-to/role-playing-dimensions",
                    "summary": "Join the same table multiple times with distinct aliases",
                },
                {
                    "title": "Fan Traps",
                    "url": "how-to/fan-traps",
                    "summary": "Detect and resolve aggregation inflation from one-to-many joins",
                },
                {
                    "title": "Data Sources",
                    "url": "how-to/data-sources",
                    "summary": "Connect CSV, Parquet, Iceberg, and database tables to semantic views",
                },
                {
                    "title": "Filtering",
                    "url": "how-to/filtering",
                    "summary": "Filter before aggregation inside SEMANTIC_VIEW(...) or after it with an outer WHERE",
                },
                {
                    "title": "Metadata Annotations",
                    "url": "how-to/metadata-annotations",
                    "summary": "Add comments, synonyms, and access modifiers to view definitions",
                },
                {
                    "title": "Semi-Additive Metrics",
                    "url": "how-to/semi-additive-metrics",
                    "summary": "Define snapshot metrics with NON ADDITIVE BY for balances and inventory",
                },
                {
                    "title": "Window Metrics",
                    "url": "how-to/window-metrics",
                    "summary": "Define rolling averages, lag comparisons, and rankings with OVER clauses",
                },
                {
                    "title": "Wildcard Selection",
                    "url": "how-to/wildcard-selection",
                    "summary": "Select all items for a table alias using alias.* patterns",
                },
                {
                    "title": "Query Facts",
                    "url": "how-to/query-facts",
                    "summary": "Query facts directly as row-level columns without aggregation",
                },
                {
                    "title": "Materializations",
                    "url": "how-to/materializations",
                    "summary": "Route matching queries to pre-aggregated tables",
                },
                {
                    "title": "YAML Definitions",
                    "url": "how-to/yaml-definitions",
                    "summary": "Export and import semantic view definitions as YAML",
                },
            ],
        },
        {
            "title": "Explanation",
            "url": "explanation/index",
            "children": [
                {
                    "title": "Semantic Views vs Regular Views",
                    "url": "explanation/semantic-views-vs-regular-views",
                    "summary": "What semantic views add beyond standard SQL views",
                },
                {
                    "title": "Metric Grain",
                    "url": "explanation/metric-grain",
                    "summary": "Why each metric aggregates at its own grain and how multi-grain queries are assembled",
                },
                {
                    "title": "Snowflake Comparison",
                    "url": "explanation/snowflake-comparison",
                    "summary": "Feature comparison with Snowflake SQL DDL semantic views",
                },
                {
                    "title": "Databricks Comparison",
                    "url": "explanation/databricks-comparison",
                    "summary": "Feature comparison with Databricks metric views",
                },
                {
                    "title": "Transactional DDL and Limitations",
                    "url": "explanation/transactional-ddl-and-limitations",
                    "summary": "How DDL participates in transactions, plus read-only and concurrency caveats",
                },
            ],
        },
        {"title": "Reference", "url": "reference/index"},
        {"title": "Release notes", "url": "changelog"},
    ],
}
html_css_files = [
    'css/s-layer.css',
]

html_static_path = ["_static"]
templates_path = ["_templates"]


def setup(app):
    from _ext.duckdb_sql_lexer import DuckDBSqlLexer
    from _ext.sqlgrammar_lexer import SqlGrammarLexer

    app.add_lexer("sqlgrammar", SqlGrammarLexer)
    app.add_lexer("duckdb-sql", DuckDBSqlLexer)
