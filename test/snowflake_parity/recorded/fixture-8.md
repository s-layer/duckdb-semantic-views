# Snowflake's results for phase7-fixture-8.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX8

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX8
```

| status |
|---|
| Schema SV_P7_FIX8 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX8

```sql
USE SCHEMA S_LAYER.SV_P7_FIX8
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE regions (region_id INT, region VARCHAR)

```sql
CREATE TABLE regions (region_id INT, region VARCHAR)
```

| status |
|---|
| Table REGIONS successfully created. |

## CREATE TABLE orders (order_id INT, region_id INT, placed_at

```sql
CREATE TABLE orders (order_id INT, region_id INT, placed_at DATE)
```

| status |
|---|
| Table ORDERS successfully created. |

## CREATE TABLE line_items (item_id INT, order_id INT, qty INT)

```sql
CREATE TABLE line_items (item_id INT, order_id INT, qty INT)
```

| status |
|---|
| Table LINE_ITEMS successfully created. |

## CREATE TABLE notes (note_id INT, order_id INT, order_id2 INT

```sql
CREATE TABLE notes (note_id INT, order_id INT, order_id2 INT, kind VARCHAR, noted_at DATE)
```

| status |
|---|
| Table NOTES successfully created. |

## INSERT INTO regions VALUES (1, 'EU'), (2, 'US')

```sql
INSERT INTO regions VALUES (1, 'EU'), (2, 'US')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO orders VALUES (1, 1, '2024-01-10'), (2, NULL, '2

```sql
INSERT INTO orders VALUES (1, 1, '2024-01-10'), (2, NULL, '2024-02-10'), (3, 2, '2024-03-10')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO line_items VALUES (1, 1, 5), (2, 1, 1), (3, 2, 2

```sql
INSERT INTO line_items VALUES (1, 1, 5), (2, 1, 1), (3, 2, 2), (4, 3, 7)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO notes VALUES

```sql
INSERT INTO notes VALUES
  (1, 1, 3,    'gift', '2024-01-15'),
  (2, 1, NULL, 'rush', '2024-02-15'),
  (3, 2, 1,    'gift', '2024-03-15')
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW s_v

```sql
CREATE OR REPLACE SEMANTIC VIEW s_v
  TABLES (
    r  AS regions    PRIMARY KEY (region_id),
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    o_region AS o (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o,
    n_order  AS n (order_id)  REFERENCES o
  )
  DIMENSIONS (r.region AS region, o.placed_at AS placed_at, li.qty AS qty, n.kind AS kind)
  METRICS (
    r.region_count AS COUNT(region_id),
    o.order_count  AS COUNT(order_id),
    li.item_count  AS COUNT(item_id),
    n.note_count   AS COUNT(note_id)
  )
```

| status |
|---|
| Semantic view S_V successfully created. |

## S1

```sql
SHOW SEMANTIC DIMENSIONS IN s_v FOR METRIC order_count
```

| table_name | name | data_type | required | synonyms | comment | labels |
|---|---|---|---|---|---|---|
| LI | QTY | NUMBER(38,0) | false | None | None | None |
| N | KIND | VARCHAR(16777216) | false | None | None | None |
| O | PLACED_AT | DATE | false | None | None | None |
| R | REGION | VARCHAR(16777216) | false | None | None | None |

## S2

```sql
SHOW SEMANTIC DIMENSIONS IN s_v FOR METRIC item_count
```

| table_name | name | data_type | required | synonyms | comment | labels |
|---|---|---|---|---|---|---|
| LI | QTY | NUMBER(38,0) | false | None | None | None |
| N | KIND | VARCHAR(16777216) | false | None | None | None |
| O | PLACED_AT | DATE | false | None | None | None |
| R | REGION | VARCHAR(16777216) | false | None | None | None |

## S3

```sql
SHOW SEMANTIC DIMENSIONS IN s_v FOR METRIC region_count
```

| table_name | name | data_type | required | synonyms | comment | labels |
|---|---|---|---|---|---|---|
| LI | QTY | NUMBER(38,0) | false | None | None | None |
| N | KIND | VARCHAR(16777216) | false | None | None | None |
| O | PLACED_AT | DATE | false | None | None | None |
| R | REGION | VARCHAR(16777216) | false | None | None | None |

## S4

```sql
SELECT * FROM SEMANTIC_VIEW(s_v DIMENSIONS n.kind METRICS o.order_count) ORDER BY ALL NULLS LAST
```

| KIND | ORDER_COUNT |
|---|---|
| gift | 2 |
| rush | 1 |
| None | 1 |

## M1

```sql
SELECT * FROM SEMANTIC_VIEW(s_v DIMENSIONS li.qty, n.kind, r.region) ORDER BY ALL NULLS LAST
```

| QTY | KIND | REGION |
|---|---|---|
| 1 | gift | EU |
| 1 | rush | EU |
| 5 | gift | EU |
| 5 | rush | EU |

## M2

```sql
CREATE OR REPLACE SEMANTIC VIEW m_regions_metric
  TABLES (
    r  AS regions    PRIMARY KEY (region_id),
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    o_region AS o (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o,
    n_order  AS n (order_id)  REFERENCES o
  )
  DIMENSIONS (r.region AS region, li.qty AS qty, n.kind AS kind)
  METRICS (r.region_count AS COUNT(region_id))
```

| status |
|---|
| Semantic view M_REGIONS_METRIC successfully created. |

## SELECT * FROM SEMANTIC_VIEW(m_regions_metric DIMENSIONS li.q

```sql
SELECT * FROM SEMANTIC_VIEW(m_regions_metric DIMENSIONS li.qty, n.kind, r.region) ORDER BY ALL NULLS LAST
```

| QTY | KIND | REGION |
|---|---|---|
| 1 | gift | EU |
| 1 | rush | EU |
| 5 | gift | EU |
| 5 | rush | EU |

## M2b

```sql
SELECT * FROM SEMANTIC_VIEW(m_regions_metric DIMENSIONS li.qty, n.kind) ORDER BY ALL NULLS LAST
```

| QTY | KIND |
|---|---|
| 1 | gift |
| 1 | rush |
| 2 | gift |
| 5 | gift |
| 5 | rush |

## M3

```sql
CREATE OR REPLACE SEMANTIC VIEW m_orders_metric
  TABLES (
    r  AS regions    PRIMARY KEY (region_id),
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    o_region AS o (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o,
    n_order  AS n (order_id)  REFERENCES o
  )
  DIMENSIONS (r.region AS region, li.qty AS qty, n.kind AS kind)
  METRICS (o.order_count AS COUNT(order_id))
```

| status |
|---|
| Semantic view M_ORDERS_METRIC successfully created. |

## SELECT * FROM SEMANTIC_VIEW(m_orders_metric DIMENSIONS li.qt

```sql
SELECT * FROM SEMANTIC_VIEW(m_orders_metric DIMENSIONS li.qty, n.kind, r.region) ORDER BY ALL NULLS LAST
```

| QTY | KIND | REGION |
|---|---|---|
| 1 | gift | EU |
| 1 | rush | EU |
| 5 | gift | EU |
| 5 | rush | EU |

## M4

```sql
SELECT * FROM SEMANTIC_VIEW(m_orders_metric DIMENSIONS li.qty WHERE r.region = 'EU' AND n.kind = 'gift') ORDER BY ALL NULLS LAST
```

| QTY |
|---|
| 1 |
| 5 |

## T1

```sql
CREATE OR REPLACE SEMANTIC VIEW t_asof
  TABLES (
    o  AS orders     PRIMARY KEY (order_id, placed_at) UNIQUE (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    li_order AS li (order_id) REFERENCES o (order_id),
    n_order  AS n (order_id, noted_at) REFERENCES o (order_id, ASOF placed_at)
  )
  DIMENSIONS (li.qty AS qty, n.kind AS kind)
  METRICS (o.order_count AS COUNT(order_id))
```

| status |
|---|
| Semantic view T_ASOF successfully created. |

## SELECT * FROM SEMANTIC_VIEW(t_asof DIMENSIONS li.qty, n.kind

```sql
SELECT * FROM SEMANTIC_VIEW(t_asof DIMENSIONS li.qty, n.kind) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'N' and 'LI' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## T1b

```sql
SELECT * FROM SEMANTIC_VIEW(t_asof DIMENSIONS n.kind METRICS o.order_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
010956 (42601): SQL compilation error:
Invalid dimension specified: The dimension entity 'N' has a higher level of granularity than the base metric entity 'O'. Consider selecting a dimension from an entity that is related to and has a lower level of granularity than 'O'.
```

## T1c

```sql
CREATE OR REPLACE SEMANTIC VIEW t_asof_pk
  TABLES (
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    li_order AS li (order_id) REFERENCES o,
    n_order  AS n (order_id, noted_at) REFERENCES o (order_id, ASOF placed_at)
  )
  DIMENSIONS (li.qty AS qty, n.kind AS kind)
  METRICS (o.order_count AS COUNT(order_id))
```

| status |
|---|
| Semantic view T_ASOF_PK successfully created. |

## SELECT * FROM SEMANTIC_VIEW(t_asof_pk DIMENSIONS li.qty, n.k

```sql
SELECT * FROM SEMANTIC_VIEW(t_asof_pk DIMENSIONS li.qty, n.kind) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'N' and 'LI' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## T2

```sql
CREATE TABLE periods (period_id INT, start_ts DATE, end_ts DATE, label VARCHAR)
```

| status |
|---|
| Table PERIODS successfully created. |

## CREATE TABLE timed_items (item_id INT, at_ts DATE, qty INT)

```sql
CREATE TABLE timed_items (item_id INT, at_ts DATE, qty INT)
```

| status |
|---|
| Table TIMED_ITEMS successfully created. |

## CREATE TABLE period_notes (note_id INT, period_id INT, kind

```sql
CREATE TABLE period_notes (note_id INT, period_id INT, kind VARCHAR)
```

| status |
|---|
| Table PERIOD_NOTES successfully created. |

## INSERT INTO periods VALUES (1, '2024-01-01', '2024-02-01', '

```sql
INSERT INTO periods VALUES (1, '2024-01-01', '2024-02-01', 'jan'), (2, '2024-02-01', NULL, 'feb_on')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO timed_items VALUES (1, '2024-01-05', 5), (2, '20

```sql
INSERT INTO timed_items VALUES (1, '2024-01-05', 5), (2, '2024-02-05', 1), (3, '2023-12-01', 7)
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO period_notes VALUES (1, 1, 'gift'), (2, 1, 'rush

```sql
INSERT INTO period_notes VALUES (1, 1, 'gift'), (2, 1, 'rush'), (3, 2, 'gift')
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW t_range

```sql
CREATE OR REPLACE SEMANTIC VIEW t_range
  TABLES (
    p  AS periods      PRIMARY KEY (period_id)
      CONSTRAINT period_range DISTINCT RANGE BETWEEN start_ts AND end_ts EXCLUSIVE,
    ti AS timed_items  PRIMARY KEY (item_id),
    pn AS period_notes PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    ti_period AS ti (at_ts) REFERENCES p (BETWEEN start_ts AND end_ts EXCLUSIVE),
    pn_period AS pn (period_id) REFERENCES p
  )
  DIMENSIONS (p.label AS label, ti.qty AS qty, pn.kind AS kind)
  METRICS (p.period_count AS COUNT(period_id), ti.item_count AS COUNT(item_id))
```

| status |
|---|
| Semantic view T_RANGE successfully created. |

## SELECT * FROM SEMANTIC_VIEW(t_range DIMENSIONS ti.qty, pn.ki

```sql
SELECT * FROM SEMANTIC_VIEW(t_range DIMENSIONS ti.qty, pn.kind) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'PN' and 'TI' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## F1

```sql
SELECT * FROM SEMANTIC_VIEW(t_range DIMENSIONS ti.qty METRICS p.period_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
010956 (42601): SQL compilation error:
Invalid dimension specified: The dimension entity 'TI' has a higher level of granularity than the base metric entity 'P'. Consider selecting a dimension from an entity that is related to and has a lower level of granularity than 'P'.
```

## F2

```sql
SELECT * FROM SEMANTIC_VIEW(t_range METRICS p.period_count WHERE ti.qty = 5)
```

**Error:**

```
010956 (42601): SQL compilation error:
Invalid dimension specified: The dimension entity 'TI' has a higher level of granularity than the base metric entity 'P'. Consider selecting a dimension from an entity that is related to and has a lower level of granularity than 'P'.
```

## F3

```sql
SELECT * FROM SEMANTIC_VIEW(t_range DIMENSIONS p.label METRICS ti.item_count) ORDER BY ALL NULLS LAST
```

| LABEL | ITEM_COUNT |
|---|---|
| feb_on | 1 |
| jan | 1 |
| None | 1 |

## F4

```sql
SHOW SEMANTIC DIMENSIONS IN t_range FOR METRIC period_count
```

| table_name | name | data_type | required | synonyms | comment | labels |
|---|---|---|---|---|---|---|
| P | LABEL | VARCHAR(16777216) | false | None | None | None |
| PN | KIND | VARCHAR(16777216) | false | None | None | None |

## T3

```sql
CREATE OR REPLACE SEMANTIC VIEW t_roles
  TABLES (
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    li_order  AS li (order_id) REFERENCES o,
    n_order   AS n (order_id)  REFERENCES o,
    n_order2  AS n (order_id2) REFERENCES o
  )
  DIMENSIONS (li.qty AS qty, n.kind AS kind)
  METRICS (
    o.order_count AS COUNT(order_id),
    n.notes_by_first  USING (n_order)  AS COUNT(note_id),
    n.notes_by_second USING (n_order2) AS COUNT(note_id)
  )
```

| status |
|---|
| Semantic view T_ROLES successfully created. |

## SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS li.qty, n.kin

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS li.qty, n.kind) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## T3b

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS li.qty METRICS n.notes_by_second) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX8

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX8
```

| status |
|---|
| SV_P7_FIX8 successfully dropped. |
