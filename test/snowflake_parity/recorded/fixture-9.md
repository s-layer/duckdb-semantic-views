# Snowflake's results for phase7-fixture-9.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX9

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX9
```

| status |
|---|
| Schema SV_P7_FIX9 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX9

```sql
USE SCHEMA S_LAYER.SV_P7_FIX9
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE airports (code VARCHAR, city VARCHAR, region_id

```sql
CREATE TABLE airports (code VARCHAR, city VARCHAR, region_id INT)
```

| status |
|---|
| Table AIRPORTS successfully created. |

## CREATE TABLE regions (region_id INT, region VARCHAR)

```sql
CREATE TABLE regions (region_id INT, region VARCHAR)
```

| status |
|---|
| Table REGIONS successfully created. |

## CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHA

```sql
CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHAR)
```

| status |
|---|
| Table FLIGHTS successfully created. |

## CREATE TABLE airport_details (code VARCHAR, terminal VARCHAR

```sql
CREATE TABLE airport_details (code VARCHAR, terminal VARCHAR)
```

| status |
|---|
| Table AIRPORT_DETAILS successfully created. |

## CREATE TABLE gates (gate_id INT, code VARCHAR, gate VARCHAR)

```sql
CREATE TABLE gates (gate_id INT, code VARCHAR, gate VARCHAR)
```

| status |
|---|
| Table GATES successfully created. |

## CREATE TABLE terminals (terminal VARCHAR, operator VARCHAR)

```sql
CREATE TABLE terminals (terminal VARCHAR, operator VARCHAR)
```

| status |
|---|
| Table TERMINALS successfully created. |

## CREATE TABLE lounges (lounge_id INT, code VARCHAR)

```sql
CREATE TABLE lounges (lounge_id INT, code VARCHAR)
```

| status |
|---|
| Table LOUNGES successfully created. |

## INSERT INTO airports VALUES ('CDG', 'Paris', 1), ('LHR', 'Lo

```sql
INSERT INTO airports VALUES ('CDG', 'Paris', 1), ('LHR', 'London', 1), ('JFK', 'New York', 2)
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO regions VALUES (1, 'EU'), (2, 'NA')

```sql
INSERT INTO regions VALUES (1, 'EU'), (2, 'NA')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO flights VALUES (1, 'CDG', 'LHR'), (2, 'LHR', 'CD

```sql
INSERT INTO flights VALUES (1, 'CDG', 'LHR'), (2, 'LHR', 'CDG'), (3, 'CDG', 'JFK')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO airport_details VALUES ('CDG', 'T2'), ('LHR', 'T

```sql
INSERT INTO airport_details VALUES ('CDG', 'T2'), ('LHR', 'T5'), ('JFK', 'T4')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO gates VALUES (1, 'CDG', 'G1'), (2, 'CDG', 'G2'),

```sql
INSERT INTO gates VALUES (1, 'CDG', 'G1'), (2, 'CDG', 'G2'), (3, 'LHR', 'G3')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO terminals VALUES ('T2', 'ADP'), ('T5', 'HAL'), (

```sql
INSERT INTO terminals VALUES ('T2', 'ADP'), ('T5', 'HAL'), ('T4', 'PANYNJ')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO lounges VALUES (1, 'CDG'), (2, 'CDG'), (3, 'JFK'

```sql
INSERT INTO lounges VALUES (1, 'CDG'), (2, 'CDG'), (3, 'JFK')
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW air

```sql
CREATE OR REPLACE SEMANTIC VIEW air
  TABLES (
    f  AS flights         PRIMARY KEY (flight_id),
    ap AS airports        PRIMARY KEY (code),
    d  AS airport_details PRIMARY KEY (code),
    g  AS gates           PRIMARY KEY (gate_id),
    t  AS terminals       PRIMARY KEY (terminal),
    l  AS lounges         PRIMARY KEY (lounge_id),
    rg AS regions         PRIMARY KEY (region_id)
  )
  RELATIONSHIPS (
    dep_airport AS f (dep)      REFERENCES ap,
    arr_airport AS f (arr)      REFERENCES ap,
    d_airport   AS d (code)     REFERENCES ap,
    g_airport   AS g (code)     REFERENCES ap,
    d_terminal  AS d (terminal) REFERENCES t,
    l_airport   AS l (code)     REFERENCES ap,
    ap_region   AS ap (region_id) REFERENCES rg
  )
  DIMENSIONS (ap.city AS city, d.terminal AS terminal, g.gate AS gate, t.operator AS operator)
  METRICS (
    f.flight_count AS COUNT(flight_id),
    f.arrivals   USING (arr_airport) AS COUNT(flight_id),
    f.departures USING (dep_airport) AS COUNT(flight_id),
    ap.airport_count AS COUNT(code),
    l.lounge_count AS COUNT(lounge_id),
    rg.region_count AS COUNT(region_id)
  )
```

| status |
|---|
| Semantic view AIR successfully created. |

## W1

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.arrivals WHERE ap.city = 'Paris')
```

| ARRIVALS |
|---|
| 1 |

## W2

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.departures WHERE ap.city = 'Paris')
```

| DEPARTURES |
|---|
| 2 |

## W3

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.arrivals, f.departures WHERE ap.city = 'Paris')
```

| ARRIVALS | DEPARTURES |
|---|---|
| 1 | 2 |

## W4

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.arrivals, f.departures WHERE d.terminal = 'T2')
```

| ARRIVALS | DEPARTURES |
|---|---|
| 1 | 2 |

## W5

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS f.arrivals WHERE d.terminal = 'T2') ORDER BY ALL NULLS LAST
```

| CITY | ARRIVALS |
|---|---|
| Paris | 1 |

## W6

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.flight_count WHERE ap.city = 'Paris')
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'AP' and the base metric or dimension entity 'F' is not supported.
```

## W7

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS f.arrivals, f.departures WHERE ap.city = 'Paris') ORDER BY ALL NULLS LAST
```

**Not reached:**

```
The connection failed (DatabaseError) before Snowflake answered.
```

## A1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS ap.airport_count) ORDER BY ALL NULLS LAST
```

| CITY | AIRPORT_COUNT |
|---|---|
| London | 1 |
| New York | 1 |
| Paris | 1 |

## A2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS ap.airport_count) ORDER BY ALL NULLS LAST
```

| TERMINAL | AIRPORT_COUNT |
|---|---|
| T2 | 1 |
| T4 | 1 |
| T5 | 1 |

## A3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS g.gate METRICS ap.airport_count) ORDER BY ALL NULLS LAST
```

| GATE | AIRPORT_COUNT |
|---|---|
| G1 | 1 |
| G2 | 1 |
| G3 | 1 |
| None | 1 |

## A4

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS ap.airport_count, f.arrivals) ORDER BY ALL NULLS LAST
```

| CITY | AIRPORT_COUNT | ARRIVALS |
|---|---|---|
| London | 1 | 1 |
| New York | 1 | 1 |
| Paris | 1 | 1 |

## A5

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS ap.airport_count WHERE d.terminal = 'T2')
```

| AIRPORT_COUNT |
|---|
| 1 |

## A6

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS t.operator METRICS ap.airport_count) ORDER BY ALL NULLS LAST
```

| OPERATOR | AIRPORT_COUNT |
|---|---|
| ADP | 1 |
| HAL | 1 |
| PANYNJ | 1 |

## G1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS g.gate METRICS f.arrivals) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## G2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS g.gate METRICS f.departures) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## G3

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.arrivals WHERE g.gate = 'G1')
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## C1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS t.operator METRICS f.arrivals) ORDER BY ALL NULLS LAST
```

| OPERATOR | ARRIVALS |
|---|---|
| ADP | 1 |
| HAL | 1 |
| PANYNJ | 1 |

## C2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS t.operator METRICS f.departures) ORDER BY ALL NULLS LAST
```

| OPERATOR | DEPARTURES |
|---|---|
| ADP | 2 |
| HAL | 1 |

## C3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS t.operator, d.terminal METRICS f.arrivals, f.departures) ORDER BY ALL NULLS LAST
```

| OPERATOR | TERMINAL | ARRIVALS | DEPARTURES |
|---|---|---|---|
| ADP | T2 | 1 | 2 |
| HAL | T5 | 1 | 1 |
| PANYNJ | T4 | 1 | None |

## L1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS l.lounge_count) ORDER BY ALL NULLS LAST
```

| CITY | LOUNGE_COUNT |
|---|---|
| New York | 1 |
| Paris | 2 |

## L2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS l.lounge_count) ORDER BY ALL NULLS LAST
```

| TERMINAL | LOUNGE_COUNT |
|---|---|
| T2 | 2 |
| T4 | 1 |

## L3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS l.lounge_count, f.arrivals) ORDER BY ALL NULLS LAST
```

| CITY | LOUNGE_COUNT | ARRIVALS |
|---|---|---|
| London | None | 1 |
| New York | 1 | 1 |
| Paris | 2 | 1 |

## L4

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS l.lounge_count WHERE ap.city = 'Paris')
```

| LOUNGE_COUNT |
|---|
| 2 |

## P1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS rg.region_count) ORDER BY ALL NULLS LAST
```

| CITY | REGION_COUNT |
|---|---|
| London | 1 |
| New York | 1 |
| Paris | 1 |

## P2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS g.gate METRICS rg.region_count) ORDER BY ALL NULLS LAST
```

| GATE | REGION_COUNT |
|---|---|
| G1 | 1 |
| G2 | 1 |
| G3 | 1 |
| None | 1 |

## P3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS rg.region_count) ORDER BY ALL NULLS LAST
```

| TERMINAL | REGION_COUNT |
|---|---|
| T2 | 1 |
| T4 | 1 |
| T5 | 1 |

## P4

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS rg.region METRICS f.arrivals) ORDER BY ALL NULLS LAST
```

**Error:**

```
000904 (42000): SQL compilation error: error line 1 at position 43
invalid identifier 'RG.REGION'
```

## CREATE TABLE orders (order_id INT, region_id INT)

```sql
CREATE TABLE orders (order_id INT, region_id INT)
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
CREATE TABLE notes (note_id INT, order_id INT, order_id2 INT, kind VARCHAR)
```

| status |
|---|
| Table NOTES successfully created. |

## INSERT INTO orders VALUES (1, 1), (2, NULL), (3, 2)

```sql
INSERT INTO orders VALUES (1, 1), (2, NULL), (3, 2)
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

## INSERT INTO notes VALUES (1, 1, 3, 'gift'), (2, 1, NULL, 'ru

```sql
INSERT INTO notes VALUES (1, 1, 3, 'gift'), (2, 1, NULL, 'rush'), (3, 2, 1, 'gift')
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW t_roles

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
  DIMENSIONS (o.order_id AS order_no, li.qty AS qty, n.kind AS kind)
  METRICS (
    o.order_count AS COUNT(order_id),
    li.item_count AS COUNT(item_id),
    n.notes_by_first  USING (n_order)  AS COUNT(note_id),
    n.notes_by_second USING (n_order2) AS COUNT(note_id)
  )
```

**Error:**

```
000904 (42000): SQL compilation error: error line 12 at position 28
invalid identifier 'ORDER_NO'
```

## O1

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS li.qty METRICS o.order_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX9.T_ROLES' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX9.T_ROLES.
```

## O2

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS o.order_no METRICS o.order_count) ORDER BY ALL NULLS LAST
```

**Not reached:**

```
The connection failed (DatabaseError) before Snowflake answered.
```

## O3

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS n.kind METRICS o.order_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX9.T_ROLES' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX9.T_ROLES.
```

## O4

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS li.qty METRICS n.notes_by_first) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX9.T_ROLES' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX9.T_ROLES.
```

## O5

```sql
SELECT * FROM SEMANTIC_VIEW(t_roles DIMENSIONS n.kind METRICS li.item_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX9.T_ROLES' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX9.T_ROLES.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX9

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX9
```

| status |
|---|
| SV_P7_FIX9 successfully dropped. |
