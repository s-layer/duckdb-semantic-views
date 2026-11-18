# Snowflake's results for phase8-fixture-14e.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14E

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14E
```

| status |
|---|
| Schema SV_P8_FIX14E successfully created. |

## USE SCHEMA S_LAYER.SV_P8_FIX14E

```sql
USE SCHEMA S_LAYER.SV_P8_FIX14E
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE customers (customer_id INT, name VARCHAR, regio

```sql
CREATE TABLE customers (customer_id INT, name VARCHAR, region VARCHAR)
```

| status |
|---|
| Table CUSTOMERS successfully created. |

## CREATE TABLE orders (order_id INT, customer_id INT, store_id

```sql
CREATE TABLE orders (order_id INT, customer_id INT, store_id INT, category VARCHAR, amount NUMBER(10,2))
```

| status |
|---|
| Table ORDERS successfully created. |

## CREATE TABLE order_returns (return_id INT, order_id INT, rea

```sql
CREATE TABLE order_returns (return_id INT, order_id INT, reason VARCHAR)
```

| status |
|---|
| Table ORDER_RETURNS successfully created. |

## CREATE TABLE stores (store_id INT, region VARCHAR, district_

```sql
CREATE TABLE stores (store_id INT, region VARCHAR, district_id INT)
```

| status |
|---|
| Table STORES successfully created. |

## CREATE TABLE districts (district_id INT, region VARCHAR)

```sql
CREATE TABLE districts (district_id INT, region VARCHAR)
```

| status |
|---|
| Table DISTRICTS successfully created. |

## CREATE TABLE regions (region VARCHAR, manager VARCHAR)

```sql
CREATE TABLE regions (region VARCHAR, manager VARCHAR)
```

| status |
|---|
| Table REGIONS successfully created. |

## CREATE TABLE accts (customer_id INT, seg_id INT, zone_id INT

```sql
CREATE TABLE accts (customer_id INT, seg_id INT, zone_id INT)
```

| status |
|---|
| Table ACCTS successfully created. |

## CREATE TABLE segs (seg_id INT, region VARCHAR)

```sql
CREATE TABLE segs (seg_id INT, region VARCHAR)
```

| status |
|---|
| Table SEGS successfully created. |

## CREATE TABLE zones (zone_id INT, region VARCHAR)

```sql
CREATE TABLE zones (zone_id INT, region VARCHAR)
```

| status |
|---|
| Table ZONES successfully created. |

## INSERT INTO customers VALUES (1, 'Alice', 'North'), (2, 'Bob

```sql
INSERT INTO customers VALUES (1, 'Alice', 'North'), (2, 'Bob', 'North'), (3, 'Carol', 'South'), (4, 'Dan', 'South')
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO orders VALUES (101, 1, 1, 'Electronics', 400), (

```sql
INSERT INTO orders VALUES (101, 1, 1, 'Electronics', 400), (102, 1, 1, 'Electronics', 200),
  (103, 1, 2, 'Clothing', 300), (104, 1, 2, 'Clothing', 100), (105, 2, 1, 'Electronics', 200),
  (106, 2, 2, 'Clothing', 100), (107, 3, 2, 'Electronics', 300), (108, 3, 1, 'Electronics', 200),
  (109, 3, 2, 'Clothing', 100), (110, 4, 2, 'Electronics', 200)
```

| number of rows inserted |
|---|
| 10 |

## INSERT INTO order_returns VALUES (1, 101, 'damaged'), (2, 10

```sql
INSERT INTO order_returns VALUES (1, 101, 'damaged'), (2, 101, 'late'), (3, 103, 'late'), (4, 107, 'damaged'),
  (5, 102, 'late'), (6, 102, 'late'), (7, 104, NULL)
```

| number of rows inserted |
|---|
| 7 |

## INSERT INTO stores VALUES (1, 'North', 1), (2, 'South', 2)

```sql
INSERT INTO stores VALUES (1, 'North', 1), (2, 'South', 2)
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO districts VALUES (1, 'North'), (2, 'South')

```sql
INSERT INTO districts VALUES (1, 'North'), (2, 'South')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO regions VALUES ('North', 'Nina'), ('South', 'Sam

```sql
INSERT INTO regions VALUES ('North', 'Nina'), ('South', 'Sam')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO accts VALUES (1, 1, 1), (2, 1, 2), (3, 2, 1), (4

```sql
INSERT INTO accts VALUES (1, 1, 1), (2, 1, 2), (3, 2, 1), (4, 2, 2)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO segs VALUES (1, 'North'), (2, 'South')

```sql
INSERT INTO segs VALUES (1, 'North'), (2, 'South')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO zones VALUES (1, 'North'), (2, 'South')

```sql
INSERT INTO zones VALUES (1, 'North'), (2, 'South')
```

| number of rows inserted |
|---|
| 2 |

## V1

```sql
CREATE OR REPLACE SEMANTIC VIEW sales_child
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    order_returns AS order_returns PRIMARY KEY (return_id))
  RELATIONSHIPS (orders(customer_id) REFERENCES customers, order_returns(order_id) REFERENCES orders)
  FACTS (
    orders.order_amount AS amount,
    region_total_excl_customer AS SUM(orders.order_amount) EXCLUDE DIMENSIONS (customers.customer_id)
  )
  DIMENSIONS (customers.customer_id AS customer_id, customers.region AS region,
    orders.order_id AS order_id, order_returns.reason AS reason)
  METRICS (
    orders.total_revenue AS SUM(amount),
    order_returns.return_count AS COUNT(order_returns.return_id),
    customers.customer_count AS COUNT(customers.customer_id),
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    region_rev_via_exclude AS AVG(region_total_excl_customer),
    avg_orders_fixed AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id))
  )
```

| status |
|---|
| Semantic view SALES_CHILD successfully created. |

## E1

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS order_returns.return_count, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REGION | RETURN_COUNT | AVG_CUSTOMER_REV |
|---|---|---|
| North | 6 | 650.00000000 |
| South | 1 | 400.00000000 |

## E1x

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS order_returns.return_count, avg_customer_rev)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 6 | 6 | 6144 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REGION_0_7_0, SYS_VW_0.REGION_0_7_1), SYS_VW.RETURN_COUNT_0_6_1, SYS_VW_0."AVG("__INLINE_LOD_8c68b4d0")_0_9_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (SYS_VW.REGION_0_7_0 = SYS_VW_0.REGION_0_7_1) | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [COUNT(ORDER_RETURNS.RETURN_ID)], groupKeys: [CUSTOMERS.REGION] | None | None | None |
| 1 | 3 | [2] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 4 | [3] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 5 | [4] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 6 | [3] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 7 | [6] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | RETURN_ID, ORDER_ID | 1 | 1 | 1024 |
| 1 | 8 | [6] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 9 | [8] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 10 | [9] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 11 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(ORDERS.AMOUNT))))))], groupKeys: [SYS_VW.REGION_0_7_1] | None | None | None |
| 1 | 12 | [11] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))), COUNT(COUNT(SUM(SUM(SUM(ORDERS.AMOUNT)))))], groupKeys: [SYS_VW.REGION_0_7_1] | None | None | None |
| 1 | 13 | [12] | LeftOuterJoin | None | None | joinKey: (SYS_VW.CUSTOMER_ID_0_10_0 = CUSTOMERS.CUSTOMER_ID) AND (SYS_VW.REGION_0_7_1 = CUSTOMERS.REGION) | None | None | None |
| 1 | 14 | [13] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 15 | [14] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 16 | [13] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(SUM(SUM(SUM(ORDERS.AMOUNT))))], groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.REGION] | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT)))], groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 18 | [17] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT))], groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 19 | [18] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 20 | [19] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 21 | [20] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 22 | [19] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 23 | [22] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## E2

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS order_returns.return_count, region_rev_via_exclude) ORDER BY 1 NULLS LAST
```

| REGION | RETURN_COUNT | REGION_REV_VIA_EXCLUDE |
|---|---|---|
| North | 6 | 1300.00000000 |
| South | 1 | 800.00000000 |

## E3

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, order_returns.return_count, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | TOTAL_REVENUE | RETURN_COUNT | AVG_CUSTOMER_REV |
|---|---|---|---|
| damaged | 700.00 | 2 | 350.00000000 |
| late | 900.00 | 4 | 900.00000000 |
| None | 900.00 | 1 | 225.00000000 |

## E3b

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS order_returns.return_count, orders.total_revenue, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | RETURN_COUNT | TOTAL_REVENUE | AVG_CUSTOMER_REV |
|---|---|---|---|
| damaged | 2 | 700.00 | 350.00000000 |
| late | 4 | 900.00 | 900.00000000 |
| None | 1 | 900.00 | 225.00000000 |

## E3x

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, order_returns.return_count, avg_customer_rev)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 9 | 9 | 9216 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REASON_0_4_0, IFNULL(SYS_VW_0.REASON_0_4_0, SYS_VW_0.REASON_0_4_1)), SYS_VW.TOTAL_REVENUE_0_5_1, SYS_VW_0.RETURN_COUNT_0_7_1, SYS_VW_0."AVG("__INLINE_LOD_8c68b4d0")_0_11_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (IFNULL(SYS_VW.REASON_0_4_0, SYS_VW_0.REASON_0_4_0) = SYS_VW_0.REASON_0_4_1) | None | None | None |
| 1 | 2 | [1] | FullOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_4_0 = SYS_VW_0.REASON_0_4_0) | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 5 | [4] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 6 | [5] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 7 | [6] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 8 | [7] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 9 | [8] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 10 | [6] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID] | None | None | None |
| 1 | 11 | [10] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, AMOUNT | 1 | 1 | 1024 |
| 1 | 12 | [2] | Aggregate | None | None | aggExprs: [COUNT(ORDER_RETURNS.RETURN_ID)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 13 | [12] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | RETURN_ID, REASON | 1 | 1 | 1024 |
| 1 | 14 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(COUNT(COUNT(SUM(ORDERS.AMOUNT))))], groupKeys: [SYS_VW.REASON_0_4_1] | None | None | None |
| 1 | 15 | [14] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT))), COUNT(COUNT(SUM(ORDERS.AMOUNT)))], groupKeys: [SYS_VW.REASON_0_4_1] | None | None | None |
| 1 | 16 | [15] | LeftOuterJoin | None | None | joinKey: (SYS_VW.CUSTOMER_ID_0_10_0 = CUSTOMERS.CUSTOMER_ID) AND (SYS_VW.REASON_0_4_1 = ORDER_RETURNS.REASON) | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 18 | [17] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 19 | [18] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 20 | [19] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 21 | [20] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 22 | [21] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 23 | [19] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 24 | [23] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 25 | [24] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 26 | [25] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 27 | [24] | Aggregate | None | None | groupKeys: [ORDERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 28 | [27] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 29 | [28] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 30 | [29] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 31 | [16] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT)), COUNT(SUM(ORDERS.AMOUNT))], groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 32 | [31] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 33 | [32] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 34 | [33] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 35 | [34] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 36 | [35] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 37 | [36] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 38 | [37] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 39 | [35] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 40 | [39] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 41 | [40] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 42 | [41] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 43 | [42] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 44 | [40] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 45 | [44] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## E4

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS customers.customer_count, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | CUSTOMER_COUNT | AVG_CUSTOMER_REV |
|---|---|---|
| damaged | 2 | 350.00000000 |
| late | 1 | 1100.00000000 |
| None | 4 | 100.00000000 |

## E4x

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS customers.customer_count, avg_customer_rev)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 9 | 9 | 9216 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REASON_0_5_1, SYS_VW_0.REASON_0_5_1), SYS_VW.CUSTOMER_COUNT_0_6_0, SYS_VW_0."AVG("__INLINE_LOD_8c68b4d0")_0_8_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_5_1 = SYS_VW_0.REASON_0_5_1) | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [COUNT(CUSTOMERS.CUSTOMER_ID)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 5 | [4] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 6 | [5] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 7 | [6] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 8 | [7] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 9 | [5] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 10 | [9] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 11 | [10] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 12 | [11] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 13 | [10] | Aggregate | None | None | groupKeys: [ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 14 | [13] | Filter | None | None | ORDERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 15 | [14] | JoinFilter | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 16 | [15] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 17 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(ORDERS.AMOUNT))))))], groupKeys: [SYS_VW.REASON_0_5_1] | None | None | None |
| 1 | 18 | [17] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))), COUNT(COUNT(SUM(SUM(SUM(ORDERS.AMOUNT)))))], groupKeys: [SYS_VW.REASON_0_5_1] | None | None | None |
| 1 | 19 | [18] | LeftOuterJoin | None | None | joinKey: (SYS_VW.CUSTOMER_ID_0_9_0 = CUSTOMERS.CUSTOMER_ID) AND (SYS_VW.REASON_0_5_1 = ORDER_RETURNS.REASON) | None | None | None |
| 1 | 20 | [19] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 21 | [20] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 22 | [21] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 23 | [22] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 24 | [23] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 25 | [24] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 26 | [22] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 27 | [26] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 28 | [27] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 29 | [28] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 30 | [27] | Aggregate | None | None | groupKeys: [ORDERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 31 | [30] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 32 | [31] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 33 | [32] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 34 | [19] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(SUM(SUM(SUM(ORDERS.AMOUNT))))], groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 35 | [34] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT)))], groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 36 | [35] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT))], groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 37 | [36] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 38 | [37] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 39 | [38] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 40 | [37] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 41 | [40] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 42 | [41] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 43 | [41] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 44 | [43] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 45 | [44] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## E5

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS customers.customer_count, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REGION | CUSTOMER_COUNT | AVG_CUSTOMER_REV |
|---|---|---|
| North | 2 | 650.00000000 |
| South | 2 | 400.00000000 |

## E6

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS orders.total_revenue, avg_customer_rev WHERE order_returns.reason = 'late') ORDER BY 1 NULLS LAST
```

| REGION | TOTAL_REVENUE | AVG_CUSTOMER_REV |
|---|---|---|
| North | 900.00 | 900.00000000 |

## E6b

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS avg_customer_rev WHERE order_returns.reason = 'late') ORDER BY 1 NULLS LAST
```

| REGION | AVG_CUSTOMER_REV |
|---|---|
| North | 1100.00000000 |

## F1

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS avg_orders_fixed) ORDER BY 1 NULLS LAST
```

| REASON | AVG_ORDERS_FIXED |
|---|---|
| damaged | 3.500000 |
| late | 4.000000 |
| None | 2.333333 |

## F1x

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS avg_orders_fixed)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 5 | 5 | 5120 |
| 1 | 0 | None | Result | None | None | SYS_VW.REASON_0_7_1, (SUM(COUNT(ORDERS.ORDER_ID))) / (COUNT(COUNT(ORDERS.ORDER_ID))) | None | None | None |
| 1 | 1 | [0] | Aggregate | None | None | aggExprs: [SUM(COUNT(ORDERS.ORDER_ID)), COUNT(COUNT(ORDERS.ORDER_ID))], groupKeys: [SYS_VW.REASON_0_7_1] | None | None | None |
| 1 | 2 | [1] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = SYS_VW.CUSTOMER_ID_0_4_0) | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | aggExprs: [COUNT(ORDERS.ORDER_ID)], groupKeys: [CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 4 | [3] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 5 | [4] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 6 | [5] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 7 | [4] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 8 | [2] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 9 | [8] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 10 | [9] | JoinFilter | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = SYS_VW.CUSTOMER_ID_0_4_0) | None | None | None |
| 1 | 11 | [10] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 12 | [11] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 13 | [12] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 14 | [13] | TableScan | S_LAYER.SV_P8_FIX14E.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 15 | [11] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 16 | [15] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 18 | [17] | TableScan | S_LAYER.SV_P8_FIX14E.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 19 | [16] | Aggregate | None | None | groupKeys: [ORDERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 20 | [19] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 21 | [20] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 22 | [21] | TableScan | S_LAYER.SV_P8_FIX14E.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |

## F2

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, avg_orders_fixed) ORDER BY 1 NULLS LAST
```

| REASON | TOTAL_REVENUE | AVG_ORDERS_FIXED |
|---|---|---|
| damaged | 700.00 | 3.500000 |
| late | 900.00 | 4.000000 |
| None | 900.00 | 2.333333 |

## V2

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_long
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), districts AS districts PRIMARY KEY (district_id),
    regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (b_ord_cust AS orders(customer_id) REFERENCES customers, a_ord_store AS orders(store_id) REFERENCES stores,
    b_cust_reg AS customers(region) REFERENCES regions, a_store_dist AS stores(district_id) REFERENCES districts,
    a_dist_reg AS districts(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)))
```

| status |
|---|
| Semantic view SD_LONG successfully created. |

## G1q

```sql
SELECT * FROM SEMANTIC_VIEW(sd_long DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1000.00000000 |
| Sam | 1100.00000000 |

## V3

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_split
  TABLES (accts AS accts PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    segs AS segs PRIMARY KEY (seg_id), zones AS zones PRIMARY KEY (zone_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (m_ord_acct AS orders(customer_id) REFERENCES accts, z_acct_seg AS accts(seg_id) REFERENCES segs,
    a_acct_zone AS accts(zone_id) REFERENCES zones, a_seg_reg AS segs(region) REFERENCES regions,
    z_zone_reg AS zones(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (accts.customer_id AS customer_id, regions.manager AS manager)
  METRICS (avg_excl_customer AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (accts.customer_id)))
```

| status |
|---|
| Semantic view SD_SPLIT successfully created. |

## G2q

```sql
SELECT * FROM SEMANTIC_VIEW(sd_split DIMENSIONS regions.manager METRICS avg_excl_customer) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_EXCL_CUSTOMER |
|---|---|
| Nina | 1600.00000000 |
| Sam | 500.00000000 |

## V4

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_cust_first
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (a_ord_cust AS orders(customer_id) REFERENCES customers, b_ord_store AS orders(store_id) REFERENCES stores,
    a_cust_reg AS customers(region) REFERENCES regions, b_store_reg AS stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)))
```

| status |
|---|
| Semantic view SD_CUST_FIRST successfully created. |

## G3q

```sql
SELECT * FROM SEMANTIC_VIEW(sd_cust_first DIMENSIONS regions.manager METRICS avg_customer_rev WHERE stores.store_id = 1) ORDER BY 1 NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'REGIONS' and the base metric or dimension entity 'ORDERS' is not supported.
```

## DROP SCHEMA S_LAYER.SV_P8_FIX14E

```sql
DROP SCHEMA S_LAYER.SV_P8_FIX14E
```

| status |
|---|
| SV_P8_FIX14E successfully dropped. |
