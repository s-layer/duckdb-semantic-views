# Snowflake's results for phase7-fixture-10c.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10C

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX10C
```

| status |
|---|
| Schema SV_P7_FIX10C successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX10C

```sql
USE SCHEMA S_LAYER.SV_P7_FIX10C
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

## CREATE TABLE orders (order_id INT, region_id INT, amount INT

```sql
CREATE TABLE orders (order_id INT, region_id INT, amount INT, day INT)
```

| status |
|---|
| Table ORDERS successfully created. |

## INSERT INTO regions VALUES (1, 'EU'), (2, 'US')

```sql
INSERT INTO regions VALUES (1, 'EU'), (2, 'US')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO orders VALUES (1, 1, 10, 1), (2, 1, 30, 2), (3,

```sql
INSERT INTO orders VALUES (1, 1, 10, 1), (2, 1, 30, 2), (3, 2, 10, 3), (4, 2, 20, 4)
```

| number of rows inserted |
|---|
| 4 |

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

## V1

```sql
CREATE OR REPLACE SEMANTIC VIEW v1
  TABLES (r AS regions PRIMARY KEY (region_id), o AS orders PRIMARY KEY (order_id))
  RELATIONSHIPS (o_region AS o (region_id) REFERENCES r)
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (o.revenue AS SUM(amount), o.running AS SUM(revenue) OVER (ORDER BY r.region))
```

| status |
|---|
| Semantic view V1 successfully created. |

## V1q

```sql
SELECT * FROM SEMANTIC_VIEW(v1 DIMENSIONS r.region METRICS o.revenue, o.running) ORDER BY ALL NULLS LAST
```

| REGION | REVENUE | RUNNING |
|---|---|---|
| EU | 40 | 40 |
| US | 30 | 70 |

## V2

```sql
CREATE OR REPLACE SEMANTIC VIEW v2
  TABLES (r AS regions PRIMARY KEY (region_id), o AS orders PRIMARY KEY (order_id))
  RELATIONSHIPS (o_region AS o (region_id) REFERENCES r)
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (o.revenue AS SUM(amount), o.running AS SUM(revenue) OVER (PARTITION BY r.region ORDER BY o.day))
```

| status |
|---|
| Semantic view V2 successfully created. |

## V2q

```sql
SELECT * FROM SEMANTIC_VIEW(v2 DIMENSIONS r.region, o.day METRICS o.running) ORDER BY ALL NULLS LAST
```

| REGION | DAY | RUNNING |
|---|---|---|
| EU | 1 | 10 |
| EU | 2 | 40 |
| US | 3 | 10 |
| US | 4 | 30 |

## V3

```sql
CREATE OR REPLACE SEMANTIC VIEW v3
  TABLES (r AS regions PRIMARY KEY (region_id), o AS orders PRIMARY KEY (order_id))
  RELATIONSHIPS (o_region AS o (region_id) REFERENCES r)
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (o.revenue AS SUM(amount), o.running AS SUM(revenue) OVER (ORDER BY region))
```

**Error:**

```
000904 (42000): SQL compilation error: error line 5 at position 78
invalid identifier 'REGION'
```

## V4

```sql
CREATE OR REPLACE SEMANTIC VIEW v4
  TABLES (r AS regions PRIMARY KEY (region_id), o AS orders PRIMARY KEY (order_id))
  RELATIONSHIPS (o_region AS o (region_id) REFERENCES r)
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (o.revenue AS SUM(amount), o.excl AS SUM(revenue) OVER (PARTITION BY EXCLUDING r.region))
```

| status |
|---|
| Semantic view V4 successfully created. |

## V4q

```sql
SELECT * FROM SEMANTIC_VIEW(v4 DIMENSIONS r.region, o.day METRICS o.revenue, o.excl) ORDER BY ALL NULLS LAST
```

| REGION | DAY | REVENUE | EXCL |
|---|---|---|---|
| EU | 1 | 10 | 10 |
| EU | 2 | 30 | 30 |
| US | 3 | 10 | 10 |
| US | 4 | 20 | 20 |

## V5

```sql
CREATE OR REPLACE SEMANTIC VIEW v5
  TABLES (r AS regions PRIMARY KEY (region_id), o AS orders PRIMARY KEY (order_id))
  RELATIONSHIPS (o_region AS o (region_id) REFERENCES r)
  DIMENSIONS (o.day AS day, r.region AS region)
  METRICS (o.revenue AS SUM(amount), running AS SUM(o.revenue) OVER (ORDER BY o.day))
```

**Error:**

```
010273 (42601): SQL compilation error:
Unsupported window metrics in derived metric RUNNING.
```

## V5q

```sql
SELECT * FROM SEMANTIC_VIEW(v5 DIMENSIONS o.day METRICS running) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10C.V5' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10C.V5.
```

## V6

```sql
CREATE OR REPLACE SEMANTIC VIEW v6
  TABLES (ap AS airports PRIMARY KEY (code), f AS flights PRIMARY KEY (flight_id))
  RELATIONSHIPS (dep_airport AS f (dep) REFERENCES ap, arr_airport AS f (arr) REFERENCES ap)
  DIMENSIONS (ap.city AS city, f.day AS day)
  METRICS (
    f.arrivals USING (arr_airport) AS COUNT(flight_id),
    running_arrivals AS SUM(f.arrivals) OVER (ORDER BY f.day)
  )
```

**Error:**

```
010273 (42601): SQL compilation error:
Unsupported window metrics in derived metric RUNNING_ARRIVALS.
```

## V6q

```sql
SELECT * FROM SEMANTIC_VIEW(v6 DIMENSIONS f.day METRICS running_arrivals) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10C.V6' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10C.V6.
```

## V6w

```sql
SELECT * FROM SEMANTIC_VIEW(v6 DIMENSIONS f.day METRICS running_arrivals WHERE ap.city = 'Paris') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10C.V6' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10C.V6.
```

## V7

```sql
CREATE OR REPLACE SEMANTIC VIEW v7
  TABLES (ap AS airports PRIMARY KEY (code), f AS flights PRIMARY KEY (flight_id))
  RELATIONSHIPS (dep_airport AS f (dep) REFERENCES ap, arr_airport AS f (arr) REFERENCES ap)
  DIMENSIONS (ap.city AS city, f.day AS day)
  METRICS (
    f.arrivals USING (arr_airport) AS COUNT(flight_id),
    f.double_arrivals AS arrivals * 2
  )
```

**Error:**

```
010905 (42601): SQL compilation error: error line 7 at position 6
Metric defined with using relationship 'F.ARRIVALS' cannot be referenced in the definition of 'F.DOUBLE_ARRIVALS'. Only derived metrics can refer to metrics defined with using relationship.
```

## V8

```sql
CREATE OR REPLACE SEMANTIC VIEW v8
  TABLES (ap AS airports PRIMARY KEY (code), f AS flights PRIMARY KEY (flight_id))
  RELATIONSHIPS (dep_airport AS f (dep) REFERENCES ap, arr_airport AS f (arr) REFERENCES ap)
  DIMENSIONS (ap.city AS city, f.day AS day)
  METRICS (f.flights AS COUNT(flight_id), f.running AS SUM(flights) OVER (ORDER BY ap.city))
```

**Error:**

```
010227 (42601): SQL compilation error:
Invalid metric definition for 'F.RUNNING': Expressions on entity 'F' cannot refer to expressions on entity 'AP'. Such reference in multi-path or many-to-many relationships is not supported.
```

## V8q

```sql
SELECT * FROM SEMANTIC_VIEW(v8 DIMENSIONS ap.city METRICS f.flights, f.running) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX10C.V8' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX10C.V8.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX10C

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX10C
```

| status |
|---|
| SV_P7_FIX10C successfully dropped. |
