"""Custom Pygments lexer for DuckDB SQL with dollar-quoting support.

Extends the standard SQL lexer to handle dollar-quoted string literals
(``$$...$$`` and ``$tag$...$tag$``), which DuckDB uses for inline YAML
bodies in ``CREATE SEMANTIC VIEW ... FROM YAML $$ ... $$``, and DuckDB's
struct literals (``{'name': value}``), as in ``variables := {...}``.

Without this, Sphinx's ``-W`` flag treats Pygments' lexing failure on
the ``$`` token as a fatal error.
"""

from pygments.lexer import inherit
from pygments.lexers.sql import SqlLexer
from pygments.token import Punctuation, String


class DuckDBSqlLexer(SqlLexer):
    name = "DuckDB SQL"
    aliases = ["duckdb-sql"]

    tokens = {
        "root": [
            # Simple dollar-quoting: $$...$$
            (r"\$\$", String, "dollar_string"),
            # Tagged dollar-quoting: $yaml$...$yaml$
            (r"\$[a-zA-Z_][a-zA-Z0-9_]*\$", String, "dollar_tag"),
            # Struct literal braces: {'min_amount': 2000}
            (r"[{}]", Punctuation),
            # Everything else: inherit the full SQL lexer
            inherit,
        ],
        "dollar_string": [
            (r"\$\$", String, "#pop"),
            (r"[^$]+", String),
            (r"\$", String),
        ],
        "dollar_tag": [
            (r"\$[a-zA-Z_][a-zA-Z0-9_]*\$", String, "#pop"),
            (r"[^$]+", String),
            (r"\$", String),
        ],
    }
