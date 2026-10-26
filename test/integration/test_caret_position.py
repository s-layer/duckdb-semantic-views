#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
End-to-end error-position verification for the semantic_views extension.

Phase 21 integration tests verify error *messages* flow through the pipeline
via sqllogictest, but sqllogictest `statement error` only matches message
substrings. This test closes the gap by inspecting the Python exception text
to verify the right validation message reaches the caller for malformed DDL.

Validation errors arrive as `Parser Error: ... LINE 1: ... ^`: a grammar
production reports its error as a ParserException at a byte offset in the
query, and DuckDB renders the caret. A statement no production matches (a
misspelt prefix, a missing name, an unknown ALTER operation) gets DuckDB's own
syntax error, whose caret underlines the token where the match failed.

Tests assert both message text AND that `extract_caret_position(...)`
returns a non-None integer at the expected column. Resolves TECH-DEBT
item 22.

Three representative error types are covered:
  1. Structural error -- missing '(' after clause keyword (ERR-02)
  2. Clause-level error -- typo in AS-body keyword (ERR-01)
  3. Misspelt prefix -- DuckDB's syntax error

Usage:
    python3 test/integration/test_caret_position.py

    Or with a custom extension path:
    SEMANTIC_VIEWS_EXTENSION_PATH=build/debug/semantic_views.duckdb_extension \
        python3 test/integration/test_caret_position.py

Exit codes:
    0 = all tests passed
    1 = at least one test failed
"""

import platform
import sys
import traceback
from pathlib import Path

# Add test/integration to path for helpers import
sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

EXT_DIR = get_ext_dir()
EXT_PATH = get_extension_path()


def make_connection():
    """Create a DuckDB connection with the extension loaded."""
    import duckdb

    conn = duckdb.connect(
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": EXT_DIR,
        },
    )
    conn.execute(f"FORCE INSTALL '{EXT_PATH}'")
    conn.execute("LOAD semantic_views")
    return conn


def extract_caret_position(error_text: str):
    """Extract the caret (^) column position from DuckDB error output.

    DuckDB renders errors as:
        Parser Error: {message}

        LINE 1: {query_text}
                ^

    A syntax error underlines its token (`^^^^`). Returns the 0-based column
    of the first ^ relative to the query text
    (subtracting the "LINE 1: " prefix of 8 characters), or None if no
    caret line found.
    """
    lines = error_text.split("\n")
    for line in lines:
        stripped = line.strip()
        # The caret line contains only whitespace and a run of ^
        if stripped and set(stripped) == {"^"}:
            caret_col = line.index("^")
            # Subtract the "LINE 1: " prefix (8 characters)
            return caret_col - 8
    return None


def run_test(name, test_fn):
    """Run a single test with exception handling."""
    print(f"\n{'=' * 60}")
    print(f"TEST: {name}")
    print(f"{'=' * 60}")

    try:
        test_fn()
        print("  RESULT: PASS")
        return True
    except AssertionError as e:
        print(f"  RESULT: FAIL (assertion)")
        print(f"  ERROR: {e}")
        traceback.print_exc()
        return False
    except Exception as e:
        print(f"  RESULT: FAIL (unexpected exception)")
        print(f"  ERROR: {e}")
        traceback.print_exc()
        return False


# ---------------------------------------------------------------------------
# Test 1: Structural error -- missing '(' after clause keyword (ERR-02)
# ---------------------------------------------------------------------------

def test_caret_missing_paren():
    """B1: Validation error for missing '(' reaches the caller with caret."""
    conn = make_connection()
    try:
        query = "CREATE SEMANTIC VIEW myview AS TABLES x"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            assert "Expected '('" in error_text, (
                f"Expected error about missing '(' but got: {error_text}"
            )
            print(f"  Error: {error_text.splitlines()[0]} -- correct")
            # Phase 62: caret rendering restored. Assert the helper finds a
            # caret column and that it points at-or-near the offending token
            # ("x" — the lone alias after TABLES).
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported. error={error_text!r}"
            )
            assert caret_col >= 0, f"caret_col must be non-negative, got {caret_col}"
            # Validator emits position pointing to the failing character (the 'x'
            # alias where '(' was expected). query.index('x', ...) gives us the
            # offset; allow ±2 wiggle for whitespace handling between TABLES and x.
            expected = query.index("x", query.index("TABLES"))
            assert abs(caret_col - expected) <= 2, (
                f"caret_col {caret_col} should be near offset of 'x' "
                f"(expected ~{expected}); error={error_text!r}"
            )
            print(f"  caret_col = {caret_col} (expected near {expected})")
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# Test 2: Clause-level error -- typo in AS-body keyword (ERR-01)
# ---------------------------------------------------------------------------

def test_caret_clause_typo():
    """B2: Misspelled clause keyword 'TBLES' surfaces with caret on TBLES."""
    conn = make_connection()
    try:
        query = "CREATE SEMANTIC VIEW x AS TBLES (t AS tbl PRIMARY KEY (id)) DIMENSIONS (t.x AS x)"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            assert "TABLES" in error_text, (
                f"Expected suggestion for 'TABLES' but got: {error_text}"
            )
            print(f"  Error: {error_text.splitlines()[0]} -- correct")
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported. error={error_text!r}"
            )
            # Caret should land on the 'T' of TBLES.
            expected = query.index("TBLES")
            assert caret_col == expected, (
                f"caret_col {caret_col} should equal offset of 'TBLES' "
                f"({expected}); error={error_text!r}"
            )
            print(f"  caret_col = {caret_col} (matches offset of 'TBLES')")
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# Test 3: Near-miss prefix error (ERR-03)
# ---------------------------------------------------------------------------

def test_caret_near_miss():
    """B3: Misspelt prefix 'CRETAE' gets DuckDB's syntax error with caret on column 0."""
    conn = make_connection()
    try:
        query = "CRETAE SEMANTIC VIEW x (tables := [])"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            assert 'syntax error at or near "CRETAE"' in error_text, (
                f"Expected a syntax error at CRETAE but got: {error_text}"
            )
            print(f"  Error: {error_text.splitlines()[0]} -- correct")
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported. error={error_text!r}"
            )
            # The caret underlines CRETAE, the first token.
            assert caret_col == 0, (
                f"caret_col should be 0 (start of CRETAE), got {caret_col}; "
                f"error={error_text!r}"
            )
            print(f"  caret_col = {caret_col} (start of input)")
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# Phase 62 Wave 0 — additional caret coverage (B4, B5, B6, B7).
# ---------------------------------------------------------------------------
# Each test below issues the malformed query, captures the exception, and
# asserts the caret column. Phase 62 Wave 3 activated all assertions —
# pre-Phase-62 these tests printed SKIP markers because the synthesised
# SELECT error('...') workaround stripped caret rendering.

def test_caret_multiline_typo():
    """B4: caret column for multi-line CREATE — DuckDB renders LINE N (not LINE 1).

    Setup needs `orders` table to exist for validate to reach line 3; for
    THIS query the validator hits 'TABLES (t)' missing 'AS' on line 2 first.
    Either way, an exception is raised and caret rendering must happen.
    """
    conn = make_connection()
    try:
        # Use a query whose error is unambiguously multi-line. Validator reports
        # the first error encountered — for our walker that's the missing 'AS'
        # on line 2. DuckDB then renders "LINE 2: TABLES (t)" with caret.
        query = (
            "CREATE SEMANTIC VIEW v AS\n"
            "TABLES (t)\n"
            "DIMENSIONS (x)"
        )
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            print(f"  Error captured: {error_text.splitlines()[0]}")
            # DuckDB MUST render a "LINE N:" prefix even for multi-line input.
            assert "LINE" in error_text, (
                f"multi-line error should contain 'LINE N:' marker. "
                f"error={error_text!r}"
            )
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported even on "
                f"multi-line input. error={error_text!r}"
            )
            # We don't pin the exact column here — it's relative to the offending
            # line, not absolute. Just assert non-negative and the marker exists.
            assert caret_col >= 0, f"caret_col must be non-negative, got {caret_col}"
            print(f"  caret_col = {caret_col} (relative to LINE N)")
    finally:
        conn.close()


def test_caret_unicode_prefix():
    """B5: caret column with multibyte UTF-8 identifier before the typo.

    Contract pinned by Phase 62: ParseError::position is a BYTE offset
    into the user input. DuckDB's ParserException::SyntaxError uses that
    byte offset to slice the input into lines, then renders the offending
    line and prints whitespace + ^ aligned to the byte position within
    the line. The Python helper extract_caret_position uses str.index('^')
    which returns CHARACTER offset, so when the prefix contains multibyte
    chars the rendered caret_col may differ from a naive char count.

    This test asserts the caret is reported and lands at-or-after the
    character offset of TBLES (a byte-position translates to a char
    position equal-or-greater because no prefix character is wider than
    its byte representation). Wide tolerance because exact alignment
    depends on DuckDB's internal rendering of the line.
    """
    conn = make_connection()
    try:
        # vüé is multibyte: v=1 byte, ü=2 bytes, é=2 bytes (UTF-8).
        query = "CREATE SEMANTIC VIEW vüé AS TBLES (t)"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            print(f"  Error captured: {error_text.splitlines()[0]}")
            assert "TABLES" in error_text, (
                f"validator should suggest TABLES for TBLES; error={error_text!r}"
            )
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported even with "
                f"multibyte UTF-8 prefix. error={error_text!r}"
            )
            assert caret_col >= 0, f"caret_col must be non-negative, got {caret_col}"
            # The character-offset of TBLES in the Python str view of the query.
            tbles_char_offset = query.index("TBLES")
            # The byte-offset of TBLES (validator's internal position).
            tbles_byte_offset = query.encode("utf-8").index(b"TBLES")
            # CONTRACT pinned by Phase 62: although the validator's position is
            # a BYTE offset, DuckDB's ParserException::SyntaxError renders the
            # offending line in characters and aligns the caret under the
            # CHARACTER at that byte position. So Python str.index('^') gives
            # the CHARACTER offset of TBLES, not the byte offset. This is the
            # observed contract on DuckDB 1.10.502 with multibyte UTF-8 prefixes.
            assert caret_col == tbles_char_offset, (
                f"caret_col {caret_col} should equal CHARACTER offset of "
                f"TBLES ({tbles_char_offset}); byte offset is "
                f"{tbles_byte_offset}; error={error_text!r}"
            )
            print(
                f"  caret_col = {caret_col} "
                f"(char offset {tbles_char_offset}, byte offset {tbles_byte_offset})"
            )
    finally:
        conn.close()


def test_caret_alter_typo():
    """B6: caret column for ALTER with bad sub-operation keyword (RENAM)."""
    conn = make_connection()
    try:
        # Set up a view to ALTER. The RENAM typo is a syntax error, so it
        # surfaces whether or not the view exists. We still create one for
        # completeness.
        conn.execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, amount DECIMAL(10,2))"
        )
        conn.execute(
            "CREATE SEMANTIC VIEW v AS "
            "TABLES (t AS orders PRIMARY KEY (id)) "
            "DIMENSIONS (t.id AS id) "
            "METRICS (t.total AS SUM(t.amount))"
        )

        query = "ALTER SEMANTIC VIEW v RENAM TO w;"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            print(f"  Error captured: {error_text.splitlines()[0]}")
            assert 'syntax error at or near "RENAM"' in error_text, (
                f"expected a syntax error at RENAM; error={error_text!r}"
            )
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported. "
                f"error={error_text!r}"
            )
            # The caret underlines RENAM, where the ALTER stops matching.
            renam_start = query.index("RENAM")  # 22
            assert caret_col == renam_start, (
                f"caret_col {caret_col} should be {renam_start} (start of 'RENAM'); "
                f"error={error_text!r}"
            )
            print(f"  caret_col = {caret_col} (start of 'RENAM')")
    finally:
        conn.close()


def test_caret_drop_missing_name():
    """B7: caret column for DROP with missing view name."""
    conn = make_connection()
    try:
        query = "DROP SEMANTIC VIEW;"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            print(f"  Error captured: {error_text.splitlines()[0]}")
            assert 'syntax error at or near ";"' in error_text, (
                f"expected a syntax error where the name belongs; error={error_text!r}"
            )
            caret_col = extract_caret_position(error_text)
            assert caret_col is not None, (
                f"Phase 62 contract: caret column must be reported. "
                f"error={error_text!r}"
            )
            # The caret is on the `;` where the name belongs.
            prefix_end = len("DROP SEMANTIC VIEW")
            assert caret_col == prefix_end, (
                f"caret_col {caret_col} should be {prefix_end} (the ';'); "
                f"error={error_text!r}"
            )
            print(f"  caret_col = {caret_col} (the ';')")
    finally:
        conn.close()


# ---------------------------------------------------------------------------
# A member that does not bind at CREATE: no caret into generated SQL
# ---------------------------------------------------------------------------

# CREATE lowers to generated SQL that carries the stored definition as JSON.
# A position DuckDB attaches to an error raised while binding that SQL is an
# offset into the generated text, so the caret it would draw points into the
# JSON, which the user never wrote. Such errors carry no caret.
_NESTED_WINDOW_BASE = (
    "CREATE SEMANTIC VIEW cbe_v AS TABLES (o AS cbe_orders PRIMARY KEY (id)) "
    "DIMENSIONS (o.d AS o.d) "
    "METRICS (o.revenue AS SUM(o.amount), "
)


def _assert_bind_error_without_internal_caret(metrics):
    conn = make_connection()
    try:
        conn.execute("CREATE TABLE cbe_orders (id INTEGER, amount INTEGER, d DATE)")
        query = _NESTED_WINDOW_BASE + metrics + ")"
        try:
            conn.execute(query)
            assert False, "Expected exception"
        except Exception as e:
            error_text = str(e)
            print(f"  Error captured: {error_text.splitlines()[0]}")
            assert "metric 'sm2' does not bind" in error_text, f"error={error_text!r}"
            assert "LINE " not in error_text, (
                f"a caret into the generated CREATE SQL; error={error_text!r}"
            )
            assert '"expr"' not in error_text, (
                f"the stored JSON leaked into the error; error={error_text!r}"
            )
    finally:
        conn.close()


def test_create_bind_error_binder_no_internal_caret():
    """A binder error from a window metric over another window metric."""
    _assert_bind_error_without_internal_caret(
        "o.sm AS AVG(revenue) OVER (ORDER BY d), o.sm2 AS AVG(sm) OVER (ORDER BY d)"
    )


def test_create_bind_error_parser_no_internal_caret():
    """A parser error from a window metric over another window metric."""
    _assert_bind_error_without_internal_caret(
        "o.sm AS AVG(revenue) OVER (PARTITION BY EXCLUDING d ORDER BY d), "
        "o.sm2 AS AVG(sm) OVER (ORDER BY d)"
    )


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

ALL_TESTS = [
    ("test_caret_missing_paren", test_caret_missing_paren),
    ("test_caret_clause_typo", test_caret_clause_typo),
    ("test_caret_near_miss", test_caret_near_miss),
    # Phase 62 Wave 3 — all 7 caret tests are now actively asserting.
    ("test_caret_multiline_typo", test_caret_multiline_typo),
    ("test_caret_unicode_prefix", test_caret_unicode_prefix),
    ("test_caret_alter_typo", test_caret_alter_typo),
    ("test_caret_drop_missing_name", test_caret_drop_missing_name),
    ("test_create_bind_error_binder_no_internal_caret",
     test_create_bind_error_binder_no_internal_caret),
    ("test_create_bind_error_parser_no_internal_caret",
     test_create_bind_error_parser_no_internal_caret),
]


def main():
    import duckdb

    print("DuckDB Semantic Views - Caret Position Verification")
    print("=" * 60)
    print(f"Python: {sys.version}")
    print(f"Platform: {platform.platform()}")
    print(f"DuckDB: {duckdb.__version__}")
    print(f"Extension: {EXT_PATH}")
    print(f"Tests: {len(ALL_TESTS)}")

    passed = 0
    failed = 0
    results = []

    for name, test_fn in ALL_TESTS:
        ok = run_test(name, test_fn)
        results.append((name, ok))
        if ok:
            passed += 1
        else:
            failed += 1

    # Summary
    print(f"\n{'=' * 60}")
    print("SUMMARY")
    print(f"{'=' * 60}")
    for name, ok in results:
        status = "PASS" if ok else "FAIL"
        print(f"  [{status}] {name}")

    print(f"\nTotal: {passed + failed}, Passed: {passed}, Failed: {failed}")

    if failed > 0:
        print("\nFAILED - at least one caret position test failed")
        sys.exit(1)
    else:
        print("\nALL PASSED - caret positions verified end-to-end")
        sys.exit(0)


if __name__ == "__main__":
    main()
