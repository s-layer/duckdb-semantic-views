# Persona Report

**Generated:** 2026-11-12
**Audience:** Data engineers exploring semantic views (intermediate)
**Scope:** v0.19.0 changed and new pages. Scenarios S13 and S14 appended to `.doc-writer/scenarios.yaml`; S2, S3 and S12 re-run because the pages they rest on changed; S1 and S4-S11 reused, not re-run (their pages were not touched).
**Scenarios tested:** 5 (S2, S3, S12, S13, S14)
**Results (first evaluation):** 3 PASS, 2 PARTIAL, 0 FAIL
**Results (after fixes):** 5 PASS, 0 PARTIAL, 0 FAIL

## Summary

Both new features are easy to reach: the home page intro and the README name them with links, the How-To index lists them, and the navbar has each with a one-line summary. Each new page starts from table creation with data, so every example can be pasted into a fresh DuckDB session and the shown output reproduced. Snowflake behaviour is named where the reader would assume it (the ASOF start day counts, `DISTINCT RANGE` is not checked, two bridges are refused). There were two points of friction on the first pass. The ASOF section did not say what a `NULL` date does, and the error reference's fan trap and role-playing entries still described 0.18 refusals that disagree with fan-traps.rst. Both are fixed.

## Per-Page Verdicts

| Page | Scenario(s) | Before | After |
|------|-------------|--------|-------|
| docs/how-to/asof-and-range-relationships.rst | S13 | PARTIAL (ASOF NULL date; range join not described) | PASS |
| docs/how-to/many-to-many-relationships.rst | S14 | PASS | PASS |
| docs/how-to/fan-traps.rst | S2, S14 | PASS (minor: one refusal bullet the reader cannot produce) | PASS |
| docs/explanation/metric-grain.rst | S2 | PASS | PASS |
| docs/how-to/role-playing-dimensions.rst | S12 | PASS | PASS |
| docs/reference/error-messages.rst | S2, S12 | PARTIAL (fan trap and ambiguity causes disagree with the how-tos) | PASS |
| docs/explanation/snowflake-comparison.rst | S3, S13, S14 | PASS | PASS |
| docs/reference/create-semantic-view.rst | S13, S14 | PASS | PASS |
| docs/index.rst, docs/how-to/index.rst, docs/conf.py (nav) | S13, S14 | PASS | PASS |
| README.md | S13, S14 | PASS | PASS |

---

## Scenario S13: Join orders to the address in effect, and signups to the running campaign

**Verdict:** PASS (PARTIAL on first pass)

### Navigation Path

1. Started at: docs/index.rst. Found: intro links "ASOF and range relationships" to `howto-asof-range`.
2. Navigated to: how-to/asof-and-range-relationships.rst. Found: when to choose which; the ASOF syntax, the key it needs, the type rule; per-order matches, revenue by city and an address-table metric, each with output; the range syntax with its `DISTINCT RANGE` constraint and bound rules; per-signup matches and counts with output; the overlap warning; what can be grouped by, with the refusal text; explain, DESCRIBE, GET_DDL and YAML; the older-release warning; troubleshooting by error text.
   - Friction (first pass): the range section explains `NULL` bounds and a `NULL` signup date, but the ASOF section did not say what an order with no date does. Fixed with a probed sentence.
   - Friction (first pass): the generated SQL is shown for ASOF only. Fixed with a sentence on the range join's condition.
3. Followed: `ref-create-relationships`, `ref-create-range-relationships`, `ref-err-asof`, `ref-err-range`.

## Scenario S14: Page counts by author country without double-counting

**Verdict:** PASS

### Navigation Path

1. Started at: docs/index.rst. Found: intro link to `howto-many-to-many`; fan-traps.rst also links it from its bridge bullet.
2. Navigated to: how-to/many-to-many-relationships.rst. Found: the bridge needs no new syntax; book and page counts by country with output and an explanation of *Duo* and *Mixed*; why groups add up to more than the total, with the total; filters; bridge-table dimensions; dims-only pairs; a fact table as bridge; generated SQL; what is refused; troubleshooting with exact messages.
3. Followed: `ref-create-bridge-relationships`, `err-multi-path`, `howto-window-relationships`.

## Scenario S2: Model a star schema and know which combinations are safe at which grain

**Verdict:** PASS

fan-traps.rst opens with how a fan-out is counted (since 0.19.0) and lists the refused cases, each with a link. metric-grain.rst explains why and lists what is still refused. First pass: error-messages.rst described "a role-played table on the path" and an airport count by carrier as ambiguity, while fan-traps.rst lists the carrier case as a fan trap. A reader matching an error message to its entry got two stories. Fixed: both entries now agree with fan-traps.rst and the build.

## Scenario S3: Compare with Snowflake and Databricks

**Verdict:** PASS

The Concept Mapping table has rows for point-in-time and period joins and for many-to-many relationships. The metric-grain boundary bullets no longer call window metrics refused. Each new how-to names its Snowflake differences inline (GET_DDL keyword case, ties in an ASOF history).

## Scenario S12: Departures and arrivals per city in one query

**Verdict:** PASS

The page's outputs are unchanged and reproduce. The refreshed passages say a metric on the airports is answered by its own dimensions and refused only by a flight dimension, and a filter on the airports follows each metric's role (with arrivals/departures numbers on the SEMANTIC_VIEW reference). The diamond example's refusal text matches the build.
