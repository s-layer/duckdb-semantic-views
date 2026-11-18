# Snowflake's results for phase7-fixture-10.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10
```

| status |
|---|
| Schema SV_P7_FIX10 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX10

```sql
USE SCHEMA S_LAYER.SV_P7_FIX10
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

## CREATE TABLE customers (cust_id INT, region_id INT)

```sql
CREATE TABLE customers (cust_id INT, region_id INT)
```

| status |
|---|
| Table CUSTOMERS successfully created. |

## CREATE TABLE stores (store_id INT, region_id INT)

```sql
CREATE TABLE stores (store_id INT, region_id INT)
```

| status |
|---|
| Table STORES successfully created. |

## CREATE TABLE orders (order_id INT, cust_id INT, store_id INT

```sql
CREATE TABLE orders (order_id INT, cust_id INT, store_id INT, amount INT, day INT)
```

| status |
|---|
| Table ORDERS successfully created. |

## CREATE TABLE items (item_id INT, order_id INT, qty INT)

```sql
CREATE TABLE items (item_id INT, order_id INT, qty INT)
```

| status |
|---|
| Table ITEMS successfully created. |

## INSERT INTO regions VALUES (1, 'EU'), (2, 'US')

```sql
INSERT INTO regions VALUES (1, 'EU'), (2, 'US')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO customers VALUES (1, 1), (2, 2)

```sql
INSERT INTO customers VALUES (1, 1), (2, 2)
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO stores VALUES (1, 1), (2, 2)

```sql
INSERT INTO stores VALUES (1, 1), (2, 2)
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO orders VALUES (1, 1, 2, 10, 1), (2, 1, 1, 30, 2)

```sql
INSERT INTO orders VALUES (1, 1, 2, 10, 1), (2, 1, 1, 30, 2), (3, 2, 1, 10, 3), (4, 2, 2, 20, 4)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO items VALUES (1, 1, 1), (2, 1, 2), (3, 2, 2), (4

```sql
INSERT INTO items VALUES (1, 1, 1), (2, 1, 2), (3, 2, 2), (4, 3, 5)
```

| number of rows inserted |
|---|
| 4 |

## X1

```sql
CREATE OR REPLACE SEMANTIC VIEW x1
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.moving AS SUM(revenue) OVER (ROWS BETWEEN 1 PRECEDING AND CURRENT ROW)
  )
```

**Error:**

```
002305 (42601): SQL compilation error: error line 6 at position 29
Window frame requires an ORDER BY clause.
```

## X1q

```sql
SELECT * FROM SEMANTIC_VIEW(x1 DIMENSIONS day METRICS moving) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.X1' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.X1.
```

## X2

```sql
CREATE OR REPLACE SEMANTIC VIEW x2
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.prev AS LAG(revenue, 1) OVER ()
  )
```

**Error:**

```
002061 (42601): SQL compilation error:
Window function type [LAG] requires ORDER BY in window specification.
```

## SELECT * FROM SEMANTIC_VIEW(x2 DIMENSIONS day METRICS prev)

```sql
SELECT * FROM SEMANTIC_VIEW(x2 DIMENSIONS day METRICS prev) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.X2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.X2.
```

## X3

```sql
CREATE OR REPLACE SEMANTIC VIEW x3
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day, o.cust_id AS cust)
  METRICS (
    o.revenue AS SUM(amount),
    o.prev AS LAG(revenue, 1) OVER (PARTITION BY cust)
  )
```

**Not reached:**

```
The connection failed (TimeoutError) before Snowflake answered.
```

## SELECT * FROM SEMANTIC_VIEW(x3 DIMENSIONS cust, day METRICS

```sql
SELECT * FROM SEMANTIC_VIEW(x3 DIMENSIONS cust, day METRICS prev) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.X3' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.X3.
```

## X4

```sql
CREATE OR REPLACE SEMANTIC VIEW x4
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.rn AS ROW_NUMBER() OVER ()
  )
```

**Error:**

```
002061 (42601): SQL compilation error:
Window function type [ROW_NUMBER] requires ORDER BY in window specification.
```

## SELECT * FROM SEMANTIC_VIEW(x4 DIMENSIONS day METRICS rn) OR

```sql
SELECT * FROM SEMANTIC_VIEW(x4 DIMENSIONS day METRICS rn) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.X4' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.X4.
```

## X5

```sql
CREATE OR REPLACE SEMANTIC VIEW x5
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.first_rev AS FIRST_VALUE(revenue) OVER ()
  )
```

**Error:**

```
002061 (42601): SQL compilation error:
Window function type [FIRST_VALUE] requires ORDER BY in window specification.
```

## SELECT * FROM SEMANTIC_VIEW(x5 DIMENSIONS day METRICS first_

```sql
SELECT * FROM SEMANTIC_VIEW(x5 DIMENSIONS day METRICS first_rev) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.X5' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.X5.
```

## X6

```sql
CREATE OR REPLACE SEMANTIC VIEW x6
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.grand AS SUM(revenue) OVER ()
  )
```

| status |
|---|
| Semantic view X6 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(x6 DIMENSIONS day METRICS grand)

```sql
SELECT * FROM SEMANTIC_VIEW(x6 DIMENSIONS day METRICS grand) ORDER BY ALL NULLS LAST
```

| DAY | GRAND |
|---|---|
| 1 | 70 |
| 2 | 70 |
| 3 | 70 |
| 4 | 70 |

## CREATE OR REPLACE SEMANTIC VIEW ord

```sql
CREATE OR REPLACE SEMANTIC VIEW ord
  TABLES (
    r  AS regions   PRIMARY KEY (region_id),
    c  AS customers PRIMARY KEY (cust_id),
    s  AS stores    PRIMARY KEY (store_id),
    li AS items     PRIMARY KEY (item_id),
    o  AS orders    PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o
  )
  DIMENSIONS (o.day AS day, li.qty AS qty, r.region AS region)
  METRICS (
    o.revenue AS SUM(amount),
    o.order_count AS COUNT(order_id),
    o.running_rev AS SUM(revenue) OVER (ORDER BY day),
    o.rev_by_store USING (o_store) AS SUM(amount),
    o.rev_by_cust USING (o_cust) AS SUM(amount),
    o.last_day_store USING (o_store) NON ADDITIVE BY (day) AS SUM(amount)
  )
```

| status |
|---|
| Semantic view ORD successfully created. |

## CREATE OR REPLACE SEMANTIC VIEW ordf

```sql
CREATE OR REPLACE SEMANTIC VIEW ordf
  TABLES (
    r  AS regions   PRIMARY KEY (region_id),
    c  AS customers PRIMARY KEY (cust_id),
    s  AS stores    PRIMARY KEY (store_id),
    li AS items     PRIMARY KEY (item_id),
    o  AS orders    PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o
  )
  DIMENSIONS (o.day AS day, li.qty AS qty, r.region AS region)
  METRICS (
    o.order_count AS COUNT(order_id),
    o.running_orders AS SUM(order_count) OVER (ORDER BY qty),
    o.share_excl_qty AS SUM(order_count) OVER (PARTITION BY EXCLUDING qty)
  )
```

**Error:**

```
000904 (42000): SQL compilation error: error line 19 at position 56
invalid identifier 'QTY'
```

## CREATE OR REPLACE SEMANTIC VIEW ordw

```sql
CREATE OR REPLACE SEMANTIC VIEW ordw
  TABLES (
    r  AS regions   PRIMARY KEY (region_id),
    c  AS customers PRIMARY KEY (cust_id),
    s  AS stores    PRIMARY KEY (store_id),
    li AS items     PRIMARY KEY (item_id),
    o  AS orders    PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r,
    li_order AS li (order_id) REFERENCES o
  )
  DIMENSIONS (o.day AS day, li.qty AS qty, r.region AS region)
  METRICS (
    o.rev_by_store USING (o_store) AS SUM(amount),
    o.rev_by_cust USING (o_cust) AS SUM(amount),
    o.running_store AS SUM(rev_by_store) OVER (ORDER BY region)
  )
```

**Error:**

```
000904 (42000): SQL compilation error: error line 20 at position 56
invalid identifier 'REGION'
```

## F1

```sql
SELECT * FROM SEMANTIC_VIEW(ordf DIMENSIONS qty METRICS order_count, running_orders) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.ORDF' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.ORDF.
```

## F2

```sql
SELECT * FROM SEMANTIC_VIEW(ordf DIMENSIONS qty METRICS order_count, share_excl_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.ORDF' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.ORDF.
```

## F3

```sql
SELECT * FROM SEMANTIC_VIEW(ord DIMENSIONS day METRICS running_rev WHERE qty = 2) ORDER BY ALL NULLS LAST
```

| DAY | RUNNING_REV |
|---|---|
| 1 | 10 |
| 2 | 40 |

## M1

```sql
SELECT * FROM SEMANTIC_VIEW(ord DIMENSIONS day METRICS revenue, running_rev) ORDER BY ALL NULLS LAST
```

| DAY | REVENUE | RUNNING_REV |
|---|---|---|
| 1 | 10 | 10 |
| 2 | 30 | 40 |
| 3 | 10 | 50 |
| 4 | 20 | 70 |

## D1

```sql
SELECT * FROM SEMANTIC_VIEW(ordw DIMENSIONS region METRICS rev_by_store, running_store) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.ORDW' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.ORDW.
```

## D2

```sql
SELECT * FROM SEMANTIC_VIEW(ordw DIMENSIONS region METRICS rev_by_store, running_store, rev_by_cust) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.ORDW' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.ORDW.
```

## D3

```sql
SELECT * FROM SEMANTIC_VIEW(ord DIMENSIONS region METRICS last_day_store) ORDER BY ALL NULLS LAST
```

| REGION | LAST_DAY_STORE |
|---|---|
| EU | 10 |
| US | 20 |

## D4

```sql
SELECT * FROM SEMANTIC_VIEW(ord METRICS last_day_store)
```

| LAST_DAY_STORE |
|---|
| 20 |

## CREATE TABLE airports (code VARCHAR, city VARCHAR)

```sql
CREATE TABLE airports (code VARCHAR, city VARCHAR)
```

| status |
|---|
| Table AIRPORTS successfully created. |

## CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHA

```sql
CREATE TABLE flights (flight_id INT, dep VARCHAR, arr VARCHAR, day INT)
```

| status |
|---|
| Table FLIGHTS successfully created. |

## INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'Londo

```sql
INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'London'), ('JFK', 'New York')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO flights VALUES (1, 'CDG', 'LHR', 1), (2, 'LHR',

```sql
INSERT INTO flights VALUES (1, 'CDG', 'LHR', 1), (2, 'LHR', 'CDG', 2), (3, 'CDG', 'JFK', 3)
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW air

```sql
CREATE OR REPLACE SEMANTIC VIEW air
  TABLES (
    ap AS airports PRIMARY KEY (code),
    f  AS flights  PRIMARY KEY (flight_id)
  )
  RELATIONSHIPS (
    dep_airport AS f (dep) REFERENCES ap,
    arr_airport AS f (arr) REFERENCES ap
  )
  DIMENSIONS (ap.city AS city, f.day AS day)
  METRICS (
    f.departures USING (dep_airport) AS COUNT(flight_id),
    f.arrivals   USING (arr_airport) AS COUNT(flight_id),
    f.running_arrivals_by_day AS SUM(arrivals) OVER (ORDER BY day),
    f.running_departures_by_day AS SUM(departures) OVER (ORDER BY day)
  )
```

**Error:**

```
010905 (42601): SQL compilation error: error line 15 at position 6
Metric defined with using relationship 'F.DEPARTURES' cannot be referenced in the definition of 'F.RUNNING_DEPARTURES_BY_DAY'. Only derived metrics can refer to metrics defined with using relationship.
```

## CREATE OR REPLACE SEMANTIC VIEW airc

```sql
CREATE OR REPLACE SEMANTIC VIEW airc
  TABLES (
    ap AS airports PRIMARY KEY (code),
    f  AS flights  PRIMARY KEY (flight_id)
  )
  RELATIONSHIPS (
    dep_airport AS f (dep) REFERENCES ap,
    arr_airport AS f (arr) REFERENCES ap
  )
  DIMENSIONS (ap.city AS city, f.day AS day)
  METRICS (
    f.departures USING (dep_airport) AS COUNT(flight_id),
    f.running_departures AS SUM(departures) OVER (ORDER BY city)
  )
```

**Not reached:**

```
The connection failed (TimeoutError) before Snowflake answered.
```

## R0

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS day METRICS running_arrivals_by_day, running_departures_by_day) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.AIR.
```

## R1

```sql
SELECT * FROM SEMANTIC_VIEW(airc DIMENSIONS city METRICS departures, running_departures) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.AIRC' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.AIRC.
```

## R2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS day METRICS running_arrivals_by_day WHERE city = 'Paris') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.AIR.
```

## R3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS day METRICS running_arrivals_by_day, running_departures_by_day WHERE city = 'Paris') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10.AIR.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX10

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX10
```

| status |
|---|
| SV_P7_FIX10 successfully dropped. |
