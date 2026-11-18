# Snowflake's results for phase7-fixture-10b.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10B

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10B
```

| status |
|---|
| Schema SV_P7_FIX10B successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX10B

```sql
USE SCHEMA S_LAYER.SV_P7_FIX10B
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

## X3

```sql
CREATE OR REPLACE SEMANTIC VIEW x3
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day, o.cust AS cust_id)
  METRICS (
    o.revenue AS SUM(amount),
    o.prev AS LAG(revenue, 1) OVER (PARTITION BY cust)
  )
```

**Error:**

```
002061 (42601): SQL compilation error:
Window function type [LAG] requires ORDER BY in window specification.
```

## X4b

```sql
CREATE OR REPLACE SEMANTIC VIEW x4b
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.rn AS ROW_NUMBER() OVER (ORDER BY day)
  )
```

**Error:**

```
010256 (42601): SQL compilation error: error line 6 at position 6
Invalid metric definition for 'O.RN': Window functions in a metric must operate over other metrics on the same entity or aggregate-level expressions that could have been used to define a metric on this entity.
```

## SELECT * FROM SEMANTIC_VIEW(x4b DIMENSIONS day METRICS rn) O

```sql
SELECT * FROM SEMANTIC_VIEW(x4b DIMENSIONS day METRICS rn) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.X4B' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.X4B.
```

## X7

```sql
CREATE OR REPLACE SEMANTIC VIEW x7
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.prev AS LAG(revenue, 1) OVER (ORDER BY day)
  )
```

| status |
|---|
| Semantic view X7 successfully created. |

## X7q

```sql
SELECT * FROM SEMANTIC_VIEW(x7 DIMENSIONS day METRICS prev) ORDER BY ALL NULLS LAST
```

| DAY | PREV |
|---|---|
| 1 | None |
| 2 | 10 |
| 3 | 30 |
| 4 | 10 |

## Q1

```sql
CREATE OR REPLACE SEMANTIC VIEW q1
  TABLES (o AS orders PRIMARY KEY (order_id))
  DIMENSIONS (o.day AS day)
  METRICS (
    o.revenue AS SUM(amount),
    o.running AS SUM(revenue) OVER (ORDER BY o.day)
  )
```

| status |
|---|
| Semantic view Q1 successfully created. |

## Q1q

```sql
SELECT * FROM SEMANTIC_VIEW(q1 DIMENSIONS day METRICS running) ORDER BY ALL NULLS LAST
```

| DAY | RUNNING |
|---|---|
| 1 | 10 |
| 2 | 40 |
| 3 | 50 |
| 4 | 70 |

## CREATE OR REPLACE SEMANTIC VIEW ordf

```sql
CREATE OR REPLACE SEMANTIC VIEW ordf
  TABLES (
    li AS items  PRIMARY KEY (item_id),
    o  AS orders PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (li_order AS li (order_id) REFERENCES o)
  DIMENSIONS (o.day AS day, li.qty AS qty)
  METRICS (
    o.order_count AS COUNT(order_id),
    o.running_orders AS SUM(order_count) OVER (ORDER BY li.qty)
  )
```

**Error:**

```
010262 (42601): SQL compilation error: error line 10 at position 6
Invalid metric definition for 'O.RUNNING_ORDERS': The PARTITION BY and ORDER BY clauses of a window function metric must refer to dimensions from the same metric entity or a related entity at a lower level of granularity, or metrics from the same entity.
```

## CREATE OR REPLACE SEMANTIC VIEW ordfx

```sql
CREATE OR REPLACE SEMANTIC VIEW ordfx
  TABLES (
    li AS items  PRIMARY KEY (item_id),
    o  AS orders PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (li_order AS li (order_id) REFERENCES o)
  DIMENSIONS (o.day AS day, li.qty AS qty)
  METRICS (
    o.order_count AS COUNT(order_id),
    o.share_excl_qty AS SUM(order_count) OVER (PARTITION BY EXCLUDING li.qty)
  )
```

**Error:**

```
010259 (42601): SQL compilation error: error line 10 at position 6
Invalid metric definition for 'O.SHARE_EXCL_QTY': PARTITION BY EXCLUDING expressions in a window function must be dimensions accessible from the metric entity.
```

## F1

```sql
SELECT * FROM SEMANTIC_VIEW(ordf DIMENSIONS li.qty METRICS o.order_count, o.running_orders) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDF' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDF.
```

## F2

```sql
SELECT * FROM SEMANTIC_VIEW(ordfx DIMENSIONS li.qty METRICS o.order_count, o.share_excl_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDFX' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDFX.
```

## F4

```sql
SELECT * FROM SEMANTIC_VIEW(ordf DIMENSIONS o.day, li.qty METRICS o.running_orders) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDF' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDF.
```

## CREATE OR REPLACE SEMANTIC VIEW ordw1

```sql
CREATE OR REPLACE SEMANTIC VIEW ordw1
  TABLES (
    r  AS regions   PRIMARY KEY (region_id),
    c  AS customers PRIMARY KEY (cust_id),
    s  AS stores    PRIMARY KEY (store_id),
    o  AS orders    PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r
  )
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (
    o.rev_by_store USING (o_store) AS SUM(amount),
    o.running_store AS SUM(rev_by_store) OVER (ORDER BY r.region)
  )
```

**Error:**

```
010905 (42601): SQL compilation error: error line 17 at position 6
Metric defined with using relationship 'O.REV_BY_STORE' cannot be referenced in the definition of 'O.RUNNING_STORE'. Only derived metrics can refer to metrics defined with using relationship.
```

## CREATE OR REPLACE SEMANTIC VIEW ordw2

```sql
CREATE OR REPLACE SEMANTIC VIEW ordw2
  TABLES (
    r  AS regions   PRIMARY KEY (region_id),
    c  AS customers PRIMARY KEY (cust_id),
    s  AS stores    PRIMARY KEY (store_id),
    o  AS orders    PRIMARY KEY (order_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r
  )
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (
    o.rev_by_store USING (o_store) AS SUM(amount),
    o.rev_by_cust USING (o_cust) AS SUM(amount),
    running_store AS SUM(o.rev_by_store) OVER (ORDER BY r.region),
    running_store_by_day AS SUM(o.rev_by_store) OVER (ORDER BY o.day)
  )
```

**Error:**

```
010273 (42601): SQL compilation error:
Unsupported window metrics in derived metric RUNNING_STORE.
```

## D1

```sql
SELECT * FROM SEMANTIC_VIEW(ordw2 DIMENSIONS r.region METRICS o.rev_by_store, running_store) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDW2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDW2.
```

## D2

```sql
SELECT * FROM SEMANTIC_VIEW(ordw2 DIMENSIONS r.region METRICS o.rev_by_store, running_store, o.rev_by_cust) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDW2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDW2.
```

## D5

```sql
SELECT * FROM SEMANTIC_VIEW(ordw2 DIMENSIONS o.day METRICS running_store_by_day) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDW2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDW2.
```

## D6

```sql
SELECT * FROM SEMANTIC_VIEW(ordw2 DIMENSIONS r.region, o.day METRICS running_store_by_day) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.ORDW2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.ORDW2.
```

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

## CREATE OR REPLACE SEMANTIC VIEW air0

```sql
CREATE OR REPLACE SEMANTIC VIEW air0
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
    f.arrivals   USING (arr_airport) AS COUNT(flight_id)
  )
```

| status |
|---|
| Semantic view AIR0 successfully created. |

## R4

```sql
SELECT * FROM SEMANTIC_VIEW(air0 DIMENSIONS f.day METRICS f.arrivals, f.departures WHERE ap.city = 'Paris') ORDER BY ALL NULLS LAST
```

| DAY | ARRIVALS | DEPARTURES |
|---|---|---|
| 1 | None | 1 |
| 2 | 1 | None |
| 3 | None | 1 |

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
    running_arrivals_by_day AS SUM(f.arrivals) OVER (ORDER BY f.day),
    running_departures_by_day AS SUM(f.departures) OVER (ORDER BY f.day),
    running_departures_by_city AS SUM(f.departures) OVER (ORDER BY ap.city)
  )
```

**Error:**

```
010273 (42601): SQL compilation error:
Unsupported window metrics in derived metric RUNNING_DEPARTURES_BY_CITY.
```

## R0

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS f.day METRICS running_arrivals_by_day, running_departures_by_day) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.AIR.
```

## R1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city METRICS f.departures, running_departures_by_city) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.AIR.
```

## R2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS f.day METRICS running_arrivals_by_day WHERE ap.city = 'Paris') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.AIR.
```

## R3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS f.day METRICS running_arrivals_by_day, running_departures_by_day WHERE ap.city = 'Paris') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10B.AIR' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10B.AIR.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX10B

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX10B
```

| status |
|---|
| SV_P7_FIX10B successfully dropped. |
