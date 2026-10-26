#!/usr/bin/env python3
# /// script
# dependencies = ["duckdb==2.0.0"]
# requires-python = ">=3.10"
# ///
"""
Differential-testing harness (TC-2, code-review 2026-07-02).

Compares `semantic_view()` results against independently hand-written
SQL over a seeded, randomized star schema — thousands of rows with NULL
foreign keys, NULL measures, and skewed group sizes. Every bounded
combination of requested dimensions and metrics is executed through both
paths and the result multisets must match exactly (floats within 1e-9
relative tolerance).

This harness is the acceptance net for the Phase R1 expansion-correctness
work: SQL-string shape assertions can't prove the generated SQL computes
the right numbers; this does.

SCOPE (deliberate): the schema and requests stay inside the engine's
*supported core* — all metrics live on the base (fact) table, dimension
tables hang off it via ManyToOne PK relationships, one grain. Scenarios
the 2026-07-02 review identified as broken (multi-grain metric×metric
fan-out SG-1, COUNT(*) on child tables SG-8, semi-additive ties SG-4, …)
are intentionally NOT generated here yet — each R1 fix PR must extend
this harness with the scenario it repairs, using this file's helpers.

Extensions so far:
  - SG-3 (single-pass derived-metric inlining): `net_revenue` is derived
    from metrics whose expressions contain a column (`o.amount`) named
    like another metric (`amount`) — the exact re-scan poison scenario.
  - SG-2 (declaration-order-independent join emission): the `cat_name`
    dimension is two joins away (orders -> products -> categories) with
    the p->cat relationship declared FIRST — pre-fix this emitted a
    forward-referencing join and dropped the o->p connecting join.
  - E-1 + SG-4 (code-review 2026-07-11, T-1): semi-additive snapshot
    section over a seeded snapshots table with (a) a dimension whose
    expression differs from its bare column (`upper(s.region)` over
    mixed-case region values — the E-1 alias-shadowing poison: pre-fix
    the RANK() partitioned on the raw column) and (b) guaranteed ties at
    the snapshot date within groups (the SG-4 RANK-vs-ROW_NUMBER
    distinction only matters with ties). Reference SQL uses an
    independent MAX-date semi-join formulation, not RANK.
  - T-1 window-metric section: running totals compared against a
    hand-written aggregate-then-window reference.
  - T-1 wildcard section: `['*']` requests must equal the explicit
    full-list requests (routing equivalence, incl. a window metric).
  - Phase 5 standard-SQL form: every request is also run as
    `SELECT ... FROM diff_sv ... GROUP BY ...` (four spellings of the GROUP
    BY keys, with WHERE and HAVING in rotation) against its own reference.

Usage:
    uv run test/integration/test_differential.py

Exit codes:
    0 = every combination matched
    1 = at least one mismatch (details printed)
"""

from __future__ import annotations

import itertools
import math
import random
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_ducklake_helpers import get_ext_dir, get_extension_path

SEED = 20260702
N_CUSTOMERS = 50
N_PRODUCTS = 30
N_ORDERS = 4000

# Dimension name -> (semantic dim name, reference SQL expr, table needed)
# `cat_name` lives two joins away (orders -> products -> categories) and its
# relationship is DECLARED FIRST in the view DDL — the SG-2 poison ordering:
# pre-fix, join emission picked the first declared join mentioning an alias,
# emitting `products ON p.category_id = cat.id` as a forward reference and
# dropping the o->p connecting join entirely.
DIMS = {
    "region": ("region", "o.region", None),
    "tier": ("tier", "c.tier", "c"),
    "category": ("category", "p.category", "p"),
    "cat_name": ("cat_name", "cat.cat_name", "cat"),
}
N_CATEGORIES = 8

# Metric name -> reference SQL aggregate (all on the base table — see SCOPE).
# `amount` deliberately shares its name with the orders.amount COLUMN, and
# `net_revenue` is a derived metric whose resolution inlines `tax_total`
# (whose expression contains `o.amount`): the SG-3 poison scenario — pre-fix,
# derived-metric inlining could re-scan inserted text and corrupt this into
# invalid SQL on a hash-seed-dependent fraction of runs.
METRICS = {
    "revenue": "SUM(o.amount)",
    "order_count": "COUNT(*)",
    "avg_amount": "AVG(o.amount)",
    "max_qty": "MAX(o.qty)",
    "min_amount": "MIN(o.amount)",
    "amount": "SUM(o.amount)",
    "tax_total": "SUM(o.amount * 0.1)",
    "net_revenue": "SUM(o.amount) - SUM(o.amount * 0.1)",
}


def seed_schema(conn) -> None:
    rng = random.Random(SEED)
    conn.execute("CREATE TABLE customers (id INTEGER PRIMARY KEY, name VARCHAR, tier VARCHAR)")
    conn.execute("CREATE TABLE categories (id INTEGER PRIMARY KEY, cat_name VARCHAR)")
    conn.execute(
        "CREATE TABLE products (id INTEGER PRIMARY KEY, name VARCHAR, "
        "category VARCHAR, category_id INTEGER)"
    )
    conn.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER, "
        "product_id INTEGER, region VARCHAR, amount DECIMAL(10,2), qty INTEGER)"
    )

    tiers = ["gold", "silver", "bronze", None]
    for i in range(N_CUSTOMERS):
        conn.execute(
            "INSERT INTO customers VALUES (?, ?, ?)",
            [i, f"cust_{i}", rng.choice(tiers)],
        )
    for i in range(N_CATEGORIES):
        conn.execute("INSERT INTO categories VALUES (?, ?)", [i, f"cat_{i}"])
    cats = ["alpha", "beta", "gamma"]
    for i in range(N_PRODUCTS):
        cat_id = None if rng.random() < 0.10 else rng.randrange(N_CATEGORIES)
        conn.execute(
            "INSERT INTO products VALUES (?, ?, ?, ?)",
            [i, f"prod_{i}", rng.choice(cats), cat_id],
        )

    regions = ["east", "west", "north", "south"]
    rows = []
    for i in range(N_ORDERS):
        cust = None if rng.random() < 0.10 else rng.randrange(N_CUSTOMERS)
        prod = None if rng.random() < 0.10 else rng.randrange(N_PRODUCTS)
        amount = None if rng.random() < 0.08 else round(rng.uniform(1, 500), 2)
        rows.append((i, cust, prod, rng.choice(regions), amount, rng.randrange(1, 20)))
    conn.executemany("INSERT INTO orders VALUES (?, ?, ?, ?, ?, ?)", rows)


CREATE_VIEW = """
CREATE SEMANTIC VIEW diff_sv AS
  TABLES (
    o AS orders PRIMARY KEY (id),
    c AS customers PRIMARY KEY (id),
    p AS products PRIMARY KEY (id),
    cat AS categories PRIMARY KEY (id)
  )
  RELATIONSHIPS (
    product_category AS p(category_id) REFERENCES cat,
    order_customer AS o(customer_id) REFERENCES c,
    order_product AS o(product_id) REFERENCES p
  )
  DIMENSIONS (
    o.region AS o.region,
    c.tier AS c.tier,
    p.category AS p.category,
    cat.cat_name AS cat.cat_name
  )
  METRICS (
    o.revenue AS SUM(o.amount),
    o.order_count AS COUNT(*),
    o.avg_amount AS AVG(o.amount),
    o.max_qty AS MAX(o.qty),
    o.min_amount AS MIN(o.amount),
    o.amount AS SUM(o.amount),
    o.tax_total AS SUM(o.amount * 0.1),
    net_revenue AS revenue - tax_total
  )
"""


def reference_sql(dims: list[str], mets: list[str], where: str = "", having: str = "") -> str:
    """Independent hand-written equivalent of the semantic_view() request.
    `where` and `having` are reference-SQL predicates (over `o`, and over the
    aggregates), added to the query as written."""
    select_items = [DIMS[d][1] for d in dims] + [METRICS[m] for m in mets]
    joins = []
    needed = {DIMS[d][2] for d in dims} - {None}
    if "cat" in needed:
        needed.add("p")  # categories hangs off products
    if "c" in needed:
        joins.append("LEFT JOIN customers c ON o.customer_id = c.id")
    if "p" in needed:
        joins.append("LEFT JOIN products p ON o.product_id = p.id")
    if "cat" in needed:
        joins.append("LEFT JOIN categories cat ON p.category_id = cat.id")
    sql = f"SELECT {'DISTINCT ' if not mets else ''}{', '.join(select_items)}\n"
    sql += "FROM orders o\n" + "\n".join(joins)
    if where:
        sql += f"\nWHERE {where}"
    if dims and mets:
        sql += f"\nGROUP BY {', '.join(str(i + 1) for i in range(len(dims)))}"
    if having:
        sql += f"\nHAVING {having}"
    return sql


# Phase 5: the same request written as a standard-SQL query over the view by
# name. Each request takes one of these spellings in turn, so every way the
# rewrite resolves a GROUP BY key (by name, qualified by the view's alias and
# by ordinal, GROUP BY ALL, by SELECT alias) meets every dims×metrics shape;
# every second request also filters (WHERE, which moves into the construct),
# and every third grouped one with metrics filters on an aggregate (HAVING,
# which the rewrite keeps outside it).
STANDARD_FORM_WHERE = ("region <> 'east'", "o.region <> 'east'")
STANDARD_FORM_HAVING = ("AGG(order_count) > 30", "COUNT(*) > 30")


def standard_form_sql(i: int, dims: list[str], mets: list[str], where: str, having: str) -> str:
    """The request as `SELECT ... FROM diff_sv ... GROUP BY ...`."""
    spelling = i % 4
    q = "s." if spelling == 1 else ""
    dim_items = [f"{q}{d}" + (f" AS g{k}" if spelling == 3 else "") for k, d in enumerate(dims)]
    met_items = [f"AGG({q}{m})" for m in mets]
    sql = f"SELECT {', '.join(dim_items + met_items)} FROM diff_sv"
    if spelling == 1:
        sql += " s"
    if where:
        sql += f" WHERE {where}"
    if dims:
        keys = {
            0: dims,
            1: [str(k + 1) for k in range(len(dims))],
            2: ["ALL"],
            3: [f"g{k}" for k in range(len(dims))],
        }[spelling]
        sql += f" GROUP BY {', '.join(keys)}"
    if having:
        sql += f" HAVING {having}"
    return sql


def _is_num(v) -> bool:
    from decimal import Decimal

    return isinstance(v, (int, float, Decimal)) and not isinstance(v, bool)


def normalize(rows: list[tuple]) -> list[tuple]:
    """Sort rows with None-safe, numeric-type-agnostic keys so both result
    sets compare positionally even if one side returns e.g. DECIMAL where
    the other returns DOUBLE for the same value."""

    def key(row: tuple):
        out = []
        for v in row:
            if v is None:
                out.append((2, ""))
            elif _is_num(v):
                out.append((0, float(v)))
            else:
                out.append((1, str(v)))
        return tuple(out)

    return sorted(rows, key=key)


def values_equal(a, b) -> bool:
    if a is None or b is None:
        return a is None and b is None
    if _is_num(a) and _is_num(b):
        return math.isclose(float(a), float(b), rel_tol=1e-9, abs_tol=1e-9)
    return a == b


def rows_equal(got: list[tuple], want: list[tuple]) -> bool:
    if len(got) != len(want):
        return False
    return all(
        len(g) == len(w) and all(values_equal(gv, wv) for gv, wv in zip(g, w))
        for g, w in zip(got, want)
    )


# ---------------------------------------------------------------------------
# T-1 (code-review 2026-07-11): semi-additive / window / wildcard sections.
# ---------------------------------------------------------------------------

N_SNAPSHOTS = 800
# Semantic dim name -> reference SQL expr (over `snapshots s LEFT JOIN
# customers c`). `region_norm`'s expression differs from its bare column and
# the seeded values collide case-insensitively — the E-1 poison: pre-fix, the
# snapshot CTE's RANK() partitioned on the raw `s.region` column instead of
# `upper(s.region)` (DuckDB binds a window-clause identifier to a same-named
# FROM-clause column before the lateral select alias), silently splitting
# 'us'/'US' into separate partitions and summing both snapshot dates.
SEMI_DIMS = {
    "region_norm": "upper(s.region)",
    "tier": "c.tier",
}


def seed_snapshots(conn) -> None:
    rng = random.Random(SEED + 1)
    conn.execute(
        "CREATE TABLE snapshots (id INTEGER PRIMARY KEY, customer_id INTEGER, "
        "region VARCHAR, snap_date DATE, balance DECIMAL(10,2))"
    )
    # Mixed-case regions (upper() collapses them) and few distinct dates over
    # many rows: every (group, max-date) slice has multiple tied rows, so the
    # SG-4 RANK path is genuinely exercised (ROW_NUMBER would drop ties).
    regions = ["us", "US", "Us", "eu", "EU", "apac"]
    dates = ["2024-01-05", "2024-02-05", "2024-03-05", "2024-04-05"]
    rows = []
    for i in range(N_SNAPSHOTS):
        cust = None if rng.random() < 0.10 else rng.randrange(N_CUSTOMERS)
        bal = None if rng.random() < 0.08 else round(rng.uniform(-100, 1000), 2)
        rows.append((i, cust, rng.choice(regions), rng.choice(dates), bal))
    conn.executemany("INSERT INTO snapshots VALUES (?, ?, ?, ?, ?)", rows)


def semi_reference_sql(dims: list[str]) -> str:
    """Independent reference for a NON ADDITIVE BY (snap_date) metric: the
    default direction selects the LATEST snapshot (F-1, Snowflake semantics),
    so the reference is a MAX-date semi-join per group (NOT the engine's RANK
    formulation, so the two paths share no mechanism). NULL-safe group join via
    IS NOT DISTINCT FROM (tier contains NULLs)."""
    sel = ", ".join(f"{SEMI_DIMS[d]} AS g{i}" for i, d in enumerate(dims))
    base = (
        "SELECT " + (sel + ", " if sel else "")
        + "s.snap_date AS sd, s.balance AS bal "
        "FROM snapshots s LEFT JOIN customers c ON s.customer_id = c.id"
    )
    if not dims:
        return f"WITH b AS ({base}) SELECT SUM(bal) FROM b WHERE sd = (SELECT MAX(sd) FROM b)"
    gcols = ", ".join(f"g{i}" for i in range(len(dims)))
    on = " AND ".join(
        [f"b.g{i} IS NOT DISTINCT FROM m.g{i}" for i in range(len(dims))] + ["b.sd = m.md"]
    )
    outer = ", ".join(f"b.g{i}" for i in range(len(dims)))
    return (
        f"WITH b AS ({base}), "
        f"m AS (SELECT {gcols}, MAX(sd) AS md FROM b GROUP BY {gcols}) "
        f"SELECT {outer}, SUM(b.bal) FROM b JOIN m ON {on} GROUP BY {outer}"
    )


def _check(conn, label: str, sv_sql: str, ref_sql: str) -> int:
    """Run both sides, compare multisets; returns 1 on failure, 0 on match."""
    try:
        got = normalize(conn.execute(sv_sql).fetchall())
        want = normalize(conn.execute(ref_sql).fetchall())
    except Exception as exc:  # noqa: BLE001 — report and continue
        print(f"FAIL {label}: query error: {exc}")
        return 1
    if rows_equal(got, want):
        print(f"  PASS: {label}")
        return 0
    print(f"FAIL {label}: result mismatch")
    print(f"  semantic_view rows={len(got)}, reference rows={len(want)}")
    for g, w in list(zip(got, want))[:3]:
        if not (len(g) == len(w) and all(values_equal(a, b) for a, b in zip(g, w))):
            print(f"  first differing row: got={g!r} want={w!r}")
            break
    return 1


def run_semi_additive_section(conn) -> tuple[int, int]:
    seed_snapshots(conn)
    conn.execute(
        "CREATE SEMANTIC VIEW diff_semi AS "
        "TABLES (s AS snapshots PRIMARY KEY (id), c AS customers PRIMARY KEY (id)) "
        "RELATIONSHIPS (snap_cust AS s(customer_id) REFERENCES c) "
        "DIMENSIONS (s.region_norm AS upper(s.region), c.tier AS c.tier, "
        "s.snap_date AS s.snap_date) "
        "METRICS (s.latest_balance NON ADDITIVE BY (snap_date) AS SUM(s.balance), "
        "s.total_balance AS SUM(s.balance))"
    )
    total, failures = 0, 0

    # Active semi-additive: NA dim absent from the query.
    for dims in ([], ["region_norm"], ["tier"], ["region_norm", "tier"]):
        total += 1
        parts = []
        if dims:
            parts.append("dimensions := [" + ", ".join(f"'{d}'" for d in dims) + "]")
        parts.append("metrics := ['latest_balance']")
        sv = f"SELECT * FROM semantic_view('diff_semi', {', '.join(parts)})"
        failures += _check(conn, f"semi-additive dims={dims}", sv, semi_reference_sql(dims))

    # Co-query: semi-additive + regular metric in one request.
    total += 1
    co_ref = (
        "WITH b AS (SELECT upper(s.region) AS g0, s.snap_date AS sd, s.balance AS bal "
        "FROM snapshots s LEFT JOIN customers c ON s.customer_id = c.id), "
        "m AS (SELECT g0, MAX(sd) AS md FROM b GROUP BY g0), "
        "sm AS (SELECT b.g0, SUM(b.bal) AS latest FROM b JOIN m "
        "ON b.g0 IS NOT DISTINCT FROM m.g0 AND b.sd = m.md GROUP BY b.g0), "
        "tt AS (SELECT g0, SUM(bal) AS total FROM b GROUP BY g0) "
        "SELECT sm.g0, sm.latest, tt.total FROM sm JOIN tt "
        "ON sm.g0 IS NOT DISTINCT FROM tt.g0"
    )
    failures += _check(
        conn,
        "semi-additive co-query with regular metric",
        "SELECT * FROM semantic_view('diff_semi', dimensions := ['region_norm'], "
        "metrics := ['latest_balance', 'total_balance'])",
        co_ref,
    )

    # Effectively regular: ALL NA dims queried -> plain aggregation.
    total += 1
    failures += _check(
        conn,
        "semi-additive effectively-regular (NA dim queried)",
        "SELECT * FROM semantic_view('diff_semi', "
        "dimensions := ['region_norm', 'snap_date'], metrics := ['latest_balance'])",
        "SELECT upper(region), snap_date, SUM(balance) FROM snapshots GROUP BY 1, 2",
    )
    return total, failures


def run_window_section(conn) -> tuple[int, int]:
    conn.execute(
        "CREATE SEMANTIC VIEW diff_win AS "
        "TABLES (s AS snapshots PRIMARY KEY (id)) "
        "DIMENSIONS (s.region_norm AS upper(s.region), s.snap_date AS s.snap_date) "
        "METRICS (s.total_balance AS SUM(s.balance), "
        "s.running_total AS SUM(total_balance) OVER "
        "(PARTITION BY EXCLUDING snap_date ORDER BY snap_date ASC NULLS LAST))"
    )
    total, failures = 0, 0

    # Window metric: aggregate per (region, date), then running SUM within
    # region ordered by date. Reference is hand-written aggregate-then-window.
    total += 1
    failures += _check(
        conn,
        "window metric running total",
        "SELECT * FROM semantic_view('diff_win', "
        "dimensions := ['region_norm', 'snap_date'], metrics := ['running_total'])",
        "WITH agg AS (SELECT upper(region) AS g, snap_date AS d, SUM(balance) AS t "
        "FROM snapshots GROUP BY 1, 2) "
        "SELECT g, d, SUM(t) OVER (PARTITION BY g ORDER BY d ASC NULLS LAST) FROM agg",
    )

    # Window + aggregate metrics in one request is an EXPLICIT engine
    # rejection (mirrors the SG-5 decomposability contract) — pin the error.
    total += 1
    try:
        conn.execute(
            "SELECT * FROM semantic_view('diff_win', "
            "dimensions := ['region_norm', 'snap_date'], "
            "metrics := ['total_balance', 'running_total'])"
        ).fetchall()
        failures += 1
        print("FAIL window+aggregate co-query: expected mix error, query succeeded")
    except Exception as exc:  # noqa: BLE001
        if "cannot mix window function metrics" in str(exc):
            print("  PASS: window+aggregate co-query raises the mix error")
        else:
            failures += 1
            print(f"FAIL window+aggregate co-query: wrong error: {exc}")
    return total, failures


def run_wildcard_section(conn) -> tuple[int, int]:
    total, failures = 0, 0
    # Wildcards are alias-qualified (`alias.*`; bare `*` is rejected,
    # matching Snowflake). `o.*` on metrics also includes the unqualified
    # derived metric `net_revenue` (SG-15: source_table == None items belong
    # to the base alias). Wildcard requests must equal explicit full lists.
    total += 1
    failures += _check(
        conn,
        "alias wildcards == explicit lists (diff_sv)",
        "SELECT * FROM semantic_view('diff_sv', "
        "dimensions := ['o.*', 'c.*', 'p.*', 'cat.*'], metrics := ['o.*'])",
        "SELECT * FROM semantic_view('diff_sv', "
        "dimensions := [" + ", ".join(f"'{d}'" for d in DIMS) + "], "
        "metrics := [" + ", ".join(f"'{m}'" for m in METRICS) + "])",
    )
    # Bare `*` is a pinned rejection.
    total += 1
    try:
        conn.execute(
            "SELECT * FROM semantic_view('diff_sv', dimensions := ['*'])"
        ).fetchall()
        failures += 1
        print("FAIL bare-star wildcard: expected rejection, query succeeded")
    except Exception as exc:  # noqa: BLE001
        if "unqualified wildcard '*' is not supported" in str(exc):
            print("  PASS: bare '*' wildcard rejected with actionable message")
        else:
            failures += 1
            print(f"FAIL bare-star wildcard: wrong error: {exc}")
    # Wildcard routing with a WINDOW metric in the view: `s.*` expands to
    # {total_balance, running_total}, which is the window+aggregate mix —
    # the expansion must surface the same mix error as the explicit list.
    total += 1
    try:
        conn.execute(
            "SELECT * FROM semantic_view('diff_win', "
            "dimensions := ['region_norm', 'snap_date'], metrics := ['s.*'])"
        ).fetchall()
        failures += 1
        print("FAIL wildcard-window mix: expected mix error, query succeeded")
    except Exception as exc:  # noqa: BLE001
        if "cannot mix window function metrics" in str(exc):
            print("  PASS: s.* expanding to window+aggregate raises the mix error")
        else:
            failures += 1
            print(f"FAIL wildcard-window mix: wrong error: {exc}")
    # Wildcard equivalence including a window metric, mix-free: request the
    # window metric explicitly alongside a dims wildcard.
    total += 1
    failures += _check(
        conn,
        "dims wildcard + explicit window metric (diff_win)",
        "SELECT * FROM semantic_view('diff_win', "
        "dimensions := ['s.*'], metrics := ['running_total'])",
        "SELECT * FROM semantic_view('diff_win', "
        "dimensions := ['region_norm', 'snap_date'], metrics := ['running_total'])",
    )
    return total, failures


# ---------------------------------------------------------------------------
# T-11 (code-review 2026-07-16): role-playing USING section. A single dimension
# table (airports) is reached from one fact table (flights) via TWO distinct
# relationships (departure / arrival). Each USING-scoped metric must join
# through its OWN edge — the alias-binding-under-joins family that produced E-1.
# The reference is an independent single-edge JOIN (no shared mechanism with the
# engine's relationship resolver).
# ---------------------------------------------------------------------------

N_FLIGHTS = 600
N_AIRPORTS = 12


def seed_role_playing(conn) -> None:
    rng = random.Random(SEED + 2)
    conn.execute("CREATE TABLE rp_airports (id INTEGER PRIMARY KEY, city VARCHAR)")
    # Cities collide (several airports per city) so GROUP BY city is non-trivial
    # and the departure vs arrival paths yield genuinely different totals.
    cities = [
        "NYC", "NYC", "LON", "LON", "TYO", "SFO",
        "SFO", "PAR", "BER", "SYD", "SYD", "DXB",
    ]
    conn.executemany(
        "INSERT INTO rp_airports VALUES (?, ?)",
        [(i, cities[i]) for i in range(N_AIRPORTS)],
    )
    conn.execute(
        "CREATE TABLE rp_flights (id INTEGER PRIMARY KEY, dep_id INTEGER, arr_id INTEGER)"
    )
    # FKs are always valid (non-NULL, in range), so the engine's INNER/LEFT
    # choice does not change results — the reference joins are unambiguous.
    rows = [
        (i, rng.randrange(N_AIRPORTS), rng.randrange(N_AIRPORTS))
        for i in range(N_FLIGHTS)
    ]
    conn.executemany("INSERT INTO rp_flights VALUES (?, ?, ?)", rows)


def run_role_playing_section(conn) -> tuple[int, int]:
    seed_role_playing(conn)
    conn.execute(
        "CREATE SEMANTIC VIEW diff_role AS "
        "TABLES (f AS rp_flights PRIMARY KEY (id), a AS rp_airports PRIMARY KEY (id)) "
        "RELATIONSHIPS (dep AS f(dep_id) REFERENCES a, arr AS f(arr_id) REFERENCES a) "
        "DIMENSIONS (a.city AS a.city) "
        "METRICS (f.dep_count USING (dep) AS COUNT(f.id), "
        "f.arr_count USING (arr) AS COUNT(f.id))"
    )
    total, failures = 0, 0

    # dep_count by city -> flights grouped by their DEPARTURE airport's city.
    total += 1
    failures += _check(
        conn,
        "role-playing dep_count by city (departure edge)",
        "SELECT * FROM semantic_view('diff_role', dimensions := ['city'], "
        "metrics := ['dep_count'])",
        "SELECT a.city, COUNT(f.id) FROM rp_flights f "
        "JOIN rp_airports a ON f.dep_id = a.id GROUP BY a.city",
    )

    # arr_count by city -> flights grouped by their ARRIVAL airport's city. The
    # SAME dimension resolves through a DIFFERENT edge, so the totals differ.
    total += 1
    failures += _check(
        conn,
        "role-playing arr_count by city (arrival edge)",
        "SELECT * FROM semantic_view('diff_role', dimensions := ['city'], "
        "metrics := ['arr_count'])",
        "SELECT a.city, COUNT(f.id) FROM rp_flights f "
        "JOIN rp_airports a ON f.arr_id = a.id GROUP BY a.city",
    )

    # Metric-only (no dimension): the USING edge still scopes the metric; with
    # all FKs valid the count equals the flight total on either edge.
    total += 1
    failures += _check(
        conn,
        "role-playing dep_count total (no dim)",
        "SELECT * FROM semantic_view('diff_role', metrics := ['dep_count'])",
        "SELECT COUNT(f.id) FROM rp_flights f JOIN rp_airports a ON f.dep_id = a.id",
    )

    return total, failures


def run_harness() -> int:
    import duckdb

    ext_dir = get_ext_dir()
    ext_path = get_extension_path()
    if not ext_path.exists():
        print(f"ERROR: extension not found at {ext_path}")
        print("Run `just build` first.")
        return 1

    conn = duckdb.connect(
        config={
            "allow_unsigned_extensions": "true",
            "extension_directory": ext_dir,
        }
    )
    conn.execute(f"FORCE INSTALL '{ext_path}'")
    conn.execute("LOAD semantic_views")

    seed_schema(conn)
    conn.execute(CREATE_VIEW)

    dim_names = list(DIMS)
    met_names = list(METRICS)

    # Bounded request set: every dims subset (incl. empty) × (each single
    # metric, one representative pair, all metrics, and — for non-empty dims —
    # no metrics at all). ~70 requests.
    metric_choices: list[list[str]] = (
        [[m] for m in met_names] + [["revenue", "order_count"]] + [met_names]
    )
    dim_choices: list[list[str]] = [
        list(c) for r in range(len(dim_names) + 1) for c in itertools.combinations(dim_names, r)
    ]

    total = 0
    failures = 0
    std_i = 0
    for dims in dim_choices:
        for mets in metric_choices + ([[]] if dims else []):
            if not dims and not mets:
                continue
            total += 1
            dim_arg = ", ".join(f"'{d}'" for d in dims)
            met_arg = ", ".join(f"'{m}'" for m in mets)
            parts = []
            if dims:
                parts.append(f"dimensions := [{dim_arg}]")
            if mets:
                parts.append(f"metrics := [{met_arg}]")
            sv_sql = f"SELECT * FROM semantic_view('diff_sv', {', '.join(parts)})"
            ref_sql = reference_sql(dims, mets)

            try:
                got = normalize(conn.execute(sv_sql).fetchall())
                want = normalize(conn.execute(ref_sql).fetchall())
            except Exception as exc:  # noqa: BLE001 — report and continue
                failures += 1
                print(f"FAIL dims={dims} mets={mets}: query error: {exc}")
                continue

            if not rows_equal(got, want):
                failures += 1
                print(f"FAIL dims={dims} mets={mets}: result mismatch")
                print(f"  semantic_view rows={len(got)}, reference rows={len(want)}")
                for g, w in list(zip(got, want))[:3]:
                    if not (len(g) == len(w) and all(values_equal(a, b) for a, b in zip(g, w))):
                        print(f"  first differing row: got={g!r} want={w!r}")
                        break

            # The standard-SQL form of the request, against its own reference.
            # Its own counter, so the rotations below cover every residue.
            total += 1
            std_i += 1
            where = STANDARD_FORM_WHERE if std_i % 2 else ("", "")
            having = STANDARD_FORM_HAVING if dims and mets and std_i % 3 == 0 else ("", "")
            failures += _check(
                conn,
                f"standard form #{std_i} spelling={std_i % 4} dims={dims} mets={mets} "
                f"where={bool(where[0])} having={bool(having[0])}",
                standard_form_sql(std_i, dims, mets, where[0], having[0]),
                reference_sql(dims, mets, where[1], having[1]),
            )

    # --- Multi-grain (SG-1 / v0.12.0 per-grain): each metric at its own grain -
    # A separate view with a metric on a ManyToOne child of the base. The
    # pre-SG-1 engine silently joined the child and inflated the base-grain SUM
    # per child row; v0.11.0 raised a fan-trap error instead. Since v0.12.0 the
    # pair is computed per-grain, so it must SUCCEED and match a reference that
    # aggregates each side separately.
    conn.execute(
        "CREATE TABLE line_items (id INTEGER PRIMARY KEY, order_id INTEGER, "
        "line_amount DECIMAL(10,2))"
    )
    conn.execute(
        "INSERT INTO line_items SELECT s.range, s.range % 500, 5.00 "
        "FROM range(1500) s"
    )
    conn.execute(
        "CREATE SEMANTIC VIEW diff_mg AS "
        "TABLES (o AS orders PRIMARY KEY (id), li AS line_items PRIMARY KEY (id)) "
        "RELATIONSHIPS (li_order AS li(order_id) REFERENCES o) "
        "DIMENSIONS (o.region AS o.region, li.line_id AS li.id) "
        "METRICS (o.revenue AS SUM(o.amount), li.item_count AS COUNT(*))"
    )
    total += 3
    # Each grain aggregated on its own table, then combined — the reference is
    # written that way too (never one joined row set, which is what inflated).
    got = normalize(
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', metrics := ['revenue', 'item_count'])"
        ).fetchall()
    )
    want = normalize(
        conn.execute(
            "SELECT (SELECT SUM(amount) FROM orders), (SELECT COUNT(*) FROM line_items)"
        ).fetchall()
    )
    if rows_equal(got, want):
        print("  PASS: multi-grain metric pair computed per-grain")
    else:
        failures += 1
        print(f"FAIL multi-grain: metric pair mismatch got={got} want={want}")

    # The same pair grouped by a base-table dimension both grains reach: the
    # child grain reaches `region` through its FK, the base grain owns it.
    got = normalize(
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', "
            "dimensions := ['region'], metrics := ['revenue', 'item_count'])"
        ).fetchall()
    )
    want = normalize(
        conn.execute(
            "SELECT COALESCE(a.region, b.region), a.revenue, b.item_count FROM "
            "(SELECT region, SUM(amount) AS revenue FROM orders GROUP BY 1) a "
            "FULL OUTER JOIN "
            "(SELECT o.region AS region, COUNT(*) AS item_count FROM line_items li "
            " LEFT JOIN orders o ON li.order_id = o.id GROUP BY 1) b "
            "ON a.region IS NOT DISTINCT FROM b.region"
        ).fetchall()
    )
    if rows_equal(got, want):
        print("  PASS: multi-grain metric pair by region computed per-grain")
    else:
        failures += 1
        print(f"FAIL multi-grain: by-region mismatch got={got} want={want}")

    # The boundary: a dimension BELOW a metric's grain is still rejected —
    # per-grain aggregation cannot define it either.
    try:
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', "
            "dimensions := ['line_id'], metrics := ['revenue'])"
        ).fetchall()
        failures += 1
        print("FAIL multi-grain: dimension below the metric grain was accepted")
    except Exception as exc:  # noqa: BLE001
        if "fan trap detected" in str(exc):
            print("  PASS: dimension below a metric's grain still raises fan-trap")
        else:
            failures += 1
            print(f"FAIL multi-grain: wrong error: {exc}")
    # Base-grain metric alone must still prune the child join and match.
    got = normalize(
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', metrics := ['revenue'])"
        ).fetchall()
    )
    want = normalize(conn.execute("SELECT SUM(amount) FROM orders").fetchall())
    if rows_equal(got, want):
        print("  PASS: base-grain metric alone matches (child join pruned)")
    else:
        failures += 1
        print(f"FAIL multi-grain: revenue-alone mismatch got={got} want={want}")

    # Child-grain COUNT(*) alone (SG-8): the engine rewrites COUNT(*) to
    # COUNT(<child pk>) so NULL-extended rows from childless parents are not
    # counted. Reference uses COUNT(li.id) — the semantically correct count.
    # (Orders 500+ in the fixture have no line items.)
    total += 2
    got = normalize(
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', metrics := ['item_count'])"
        ).fetchall()
    )
    want = normalize(
        conn.execute(
            "SELECT COUNT(li.id) FROM orders o "
            "LEFT JOIN line_items li ON li.order_id = o.id"
        ).fetchall()
    )
    if rows_equal(got, want):
        print("  PASS: child-grain COUNT(*) alone matches COUNT(pk) reference")
    else:
        failures += 1
        print(f"FAIL SG-8: item_count-alone mismatch got={got} want={want}")
    got = normalize(
        conn.execute(
            "SELECT * FROM semantic_view('diff_mg', "
            "dimensions := ['region'], metrics := ['item_count'])"
        ).fetchall()
    )
    want = normalize(
        conn.execute(
            "SELECT o.region, COUNT(li.id) FROM orders o "
            "LEFT JOIN line_items li ON li.order_id = o.id GROUP BY 1"
        ).fetchall()
    )
    if rows_equal(got, want):
        print("  PASS: child-grain COUNT(*) by region matches COUNT(pk) reference")
    else:
        failures += 1
        print(f"FAIL SG-8: item_count-by-region mismatch got={got} want={want}")

    t, f = run_semi_additive_section(conn)
    total += t
    failures += f
    t, f = run_window_section(conn)
    total += t
    failures += f
    t, f = run_wildcard_section(conn)
    total += t
    failures += f
    t, f = run_role_playing_section(conn)
    total += t
    failures += f

    print()
    print(f"Ran {total} dims×metrics combinations over {N_ORDERS} orders "
          f"({N_CUSTOMERS} customers, {N_PRODUCTS} products, seed={SEED})")
    if failures:
        print(f"FAILED: {failures}/{total} combinations mismatched")
        return 1
    print("ALL PASSED — semantic_view() matches hand-written SQL on every combination")
    return 0


if __name__ == "__main__":
    sys.exit(run_harness())
