# Snowflake's results for phase8-fixture-14d.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14D

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14D
```

| status |
|---|
| Schema SV_P8_FIX14D successfully created. |

## USE SCHEMA S_LAYER.SV_P8_FIX14D

```sql
USE SCHEMA S_LAYER.SV_P8_FIX14D
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

## CREATE TABLE order_returns_nn (return_id INT, order_id INT,

```sql
CREATE TABLE order_returns_nn (return_id INT, order_id INT, reason VARCHAR)
```

| status |
|---|
| Table ORDER_RETURNS_NN successfully created. |

## CREATE TABLE stores (store_id INT, region VARCHAR)

```sql
CREATE TABLE stores (store_id INT, region VARCHAR)
```

| status |
|---|
| Table STORES successfully created. |

## CREATE TABLE regions (region VARCHAR, manager VARCHAR)

```sql
CREATE TABLE regions (region VARCHAR, manager VARCHAR)
```

| status |
|---|
| Table REGIONS successfully created. |

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

## INSERT INTO order_returns_nn SELECT * FROM order_returns WHE

```sql
INSERT INTO order_returns_nn SELECT * FROM order_returns WHERE reason IS NOT NULL
```

| number of rows inserted |
|---|
| 6 |

## INSERT INTO stores VALUES (1, 'North'), (2, 'South')

```sql
INSERT INTO stores VALUES (1, 'North'), (2, 'South')
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
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    region_rev_via_exclude AS AVG(region_total_excl_customer),
    max_order_rev AS MAX(SUM(orders.order_amount) INCLUDE DIMENSIONS (orders.order_id)),
    avg_orders_fixed AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id))
  )
```

| status |
|---|
| Semantic view SALES_CHILD successfully created. |

## V2

```sql
CREATE OR REPLACE SEMANTIC VIEW sales_child_nn
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    order_returns AS order_returns_nn PRIMARY KEY (return_id))
  RELATIONSHIPS (orders(customer_id) REFERENCES customers, order_returns(order_id) REFERENCES orders)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, customers.region AS region, order_returns.reason AS reason)
  METRICS (
    orders.total_revenue AS SUM(amount),
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id))
  )
```

| status |
|---|
| Semantic view SALES_CHILD_NN successfully created. |

## C1

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, avg_customer_rev)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 8 | 8 | 8192 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REASON_0_4_0, SYS_VW_0.REASON_0_4_1), SYS_VW.TOTAL_REVENUE_0_5_1, SYS_VW_0."AVG("__INLINE_LOD_8c68b4d0")_0_9_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_4_0 = SYS_VW_0.REASON_0_4_1) | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 5 | [4] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 6 | [5] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 7 | [6] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 8 | [7] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 9 | [5] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID] | None | None | None |
| 1 | 10 | [9] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, AMOUNT | 1 | 1 | 1024 |
| 1 | 11 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(COUNT(COUNT(SUM(ORDERS.AMOUNT))))], groupKeys: [SYS_VW.REASON_0_4_1] | None | None | None |
| 1 | 12 | [11] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT))), COUNT(COUNT(SUM(ORDERS.AMOUNT)))], groupKeys: [SYS_VW.REASON_0_4_1] | None | None | None |
| 1 | 13 | [12] | LeftOuterJoin | None | None | joinKey: (SYS_VW.CUSTOMER_ID_0_8_0 = CUSTOMERS.CUSTOMER_ID) AND (SYS_VW.REASON_0_4_1 = ORDER_RETURNS.REASON) | None | None | None |
| 1 | 14 | [13] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 15 | [14] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 16 | [15] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 18 | [17] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 19 | [18] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 20 | [16] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 21 | [20] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 22 | [21] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 23 | [22] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 24 | [21] | Aggregate | None | None | groupKeys: [ORDERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 25 | [24] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 26 | [25] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 27 | [26] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 28 | [13] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT)), COUNT(SUM(ORDERS.AMOUNT))], groupKeys: [CUSTOMERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 29 | [28] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 30 | [29] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 31 | [30] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 32 | [31] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 33 | [32] | Aggregate | None | None | groupKeys: [CUSTOMERS.CUSTOMER_ID, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 34 | [33] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 35 | [34] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 36 | [32] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 37 | [36] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 38 | [37] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 39 | [38] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 40 | [39] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 41 | [37] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 42 | [41] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## C1r

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | TOTAL_REVENUE | AVG_CUSTOMER_REV |
|---|---|---|
| damaged | 700.00 | 350.00000000 |
| late | 900.00 | 900.00000000 |
| None | 900.00 | 225.00000000 |

## C2

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, region_rev_via_exclude)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 5 | 5 | 5120 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REASON_0_4_0, SYS_VW_0.REASON_0_4_1), SYS_VW.TOTAL_REVENUE_0_5_1, SYS_VW_0."AVG("".REGION_TOTAL_EXCL_CUSTOMER)_0_6_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_4_0 = SYS_VW_0.REASON_0_4_1) | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 5 | [4] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 6 | [5] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 7 | [6] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 8 | [7] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 9 | [5] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID] | None | None | None |
| 1 | 10 | [9] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, AMOUNT | 1 | 1 | 1024 |
| 1 | 11 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(COUNT(COUNT(SUM(ORDERS.AMOUNT))))], groupKeys: [SYS_VW.REASON_0_4_0] | None | None | None |
| 1 | 12 | [11] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT))), COUNT(COUNT(SUM(ORDERS.AMOUNT)))], groupKeys: [SYS_VW.REASON_0_4_0] | None | None | None |
| 1 | 13 | [12] | LeftOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_4_0 = ORDER_RETURNS.REASON) | None | None | None |
| 1 | 14 | [13] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 15 | [14] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | REASON | 1 | 1 | 1024 |
| 1 | 16 | [13] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT)), COUNT(SUM(ORDERS.AMOUNT))], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDER_RETURNS.REASON] | None | None | None |
| 1 | 18 | [17] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 19 | [18] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 20 | [19] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 21 | [20] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 22 | [21] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 23 | [22] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 24 | [20] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID] | None | None | None |
| 1 | 25 | [24] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, AMOUNT | 1 | 1 | 1024 |

## C2r

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS orders.total_revenue, region_rev_via_exclude) ORDER BY 1 NULLS LAST
```

| REASON | TOTAL_REVENUE | REGION_REV_VIA_EXCLUDE |
|---|---|---|
| damaged | 700.00 | 700.00000000 |
| late | 900.00 | 900.00000000 |
| None | 900.00 | 900.00000000 |

## C3

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region, order_returns.reason METRICS orders.total_revenue, region_rev_via_exclude)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 9 | 9 | 9216 |
| 1 | 0 | None | Result | None | None | IFNULL(SYS_VW.REGION_0_7_1, SYS_VW_0.REGION_0_7_2), IFNULL(SYS_VW.REASON_0_8_0, SYS_VW_0.REASON_0_8_1), SYS_VW.TOTAL_REVENUE_0_9_2, SYS_VW_0."AVG("".REGION_TOTAL_EXCL_CUSTOMER)_0_10_0" | None | None | None |
| 1 | 1 | [0] | FullOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_8_0 = SYS_VW_0.REASON_0_8_1) AND (SYS_VW.REGION_0_7_1 = SYS_VW_0.REGION_0_7_2) | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 3 | [2] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 5 | [4] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 6 | [5] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 7 | [6] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 8 | [7] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 9 | [5] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 10 | [9] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 11 | [10] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 12 | [11] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 13 | [12] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 14 | [10] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 15 | [14] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |
| 1 | 16 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))), COUNT(COUNT(COUNT(SUM(ORDERS.AMOUNT))))], groupKeys: [SYS_VW.REGION_0_7_1, SYS_VW.REASON_0_8_0] | None | None | None |
| 1 | 17 | [16] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT))), COUNT(COUNT(SUM(ORDERS.AMOUNT)))], groupKeys: [SYS_VW.REGION_0_7_1, SYS_VW.REASON_0_8_0] | None | None | None |
| 1 | 18 | [17] | LeftOuterJoin | None | None | joinKey: (SYS_VW.REASON_0_8_0 = ORDER_RETURNS.REASON) AND (SYS_VW.REGION_0_7_1 = CUSTOMERS.REGION) | None | None | None |
| 1 | 19 | [18] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 20 | [19] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 21 | [20] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 22 | [21] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 23 | [22] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 24 | [23] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 25 | [21] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 26 | [25] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 27 | [26] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 28 | [27] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 29 | [26] | Aggregate | None | None | groupKeys: [ORDERS.CUSTOMER_ID, ORDERS.ORDER_ID] | None | None | None |
| 1 | 30 | [29] | Filter | None | None | ORDERS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 31 | [30] | JoinFilter | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 32 | [31] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID | 1 | 1 | 1024 |
| 1 | 33 | [18] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT)), COUNT(SUM(ORDERS.AMOUNT))], groupKeys: [ORDER_RETURNS.REASON, CUSTOMERS.REGION] | None | None | None |
| 1 | 34 | [33] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 35 | [34] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 36 | [35] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, CUSTOMERS.REGION, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 37 | [36] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 38 | [37] | Aggregate | None | None | groupKeys: [CUSTOMERS.REGION, CUSTOMERS.CUSTOMER_ID] | None | None | None |
| 1 | 39 | [38] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 40 | [39] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 41 | [37] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID, ORDER_RETURNS.REASON] | None | None | None |
| 1 | 42 | [41] | LeftOuterJoin | None | None | joinKey: (ORDER_RETURNS.ORDER_ID = ORDERS.ORDER_ID) | None | None | None |
| 1 | 43 | [42] | Aggregate | None | None | groupKeys: [ORDER_RETURNS.REASON, ORDER_RETURNS.ORDER_ID] | None | None | None |
| 1 | 44 | [43] | Filter | None | None | ORDER_RETURNS.ORDER_ID IS NOT NULL | None | None | None |
| 1 | 45 | [44] | TableScan | S_LAYER.SV_P8_FIX14D.ORDER_RETURNS | ORDER_RETURNS | ORDER_ID, REASON | 1 | 1 | 1024 |
| 1 | 46 | [42] | Aggregate | None | None | groupKeys: [ORDERS.AMOUNT, ORDERS.ORDER_ID, ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 47 | [46] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | ORDER_ID, CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## C3r

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region, order_returns.reason METRICS orders.total_revenue, region_rev_via_exclude) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| REGION | REASON | TOTAL_REVENUE | REGION_REV_VIA_EXCLUDE |
|---|---|---|---|
| North | damaged | 400.00 | 400.00000000 |
| North | late | 900.00 | 900.00000000 |
| North | None | 400.00 | 400.00000000 |
| South | damaged | 300.00 | 300.00000000 |
| South | None | 500.00 | None |
| None | None | None | 500.00000000 |

## C4

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child_nn DIMENSIONS order_returns.reason METRICS orders.total_revenue, avg_customer_rev) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| REASON | TOTAL_REVENUE | AVG_CUSTOMER_REV |
|---|---|---|
| damaged | 700.00 | 350.00000000 |
| late | 900.00 | 900.00000000 |
| None | 900.00 | 225.00000000 |

## C5

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS order_returns.return_count, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | RETURN_COUNT | AVG_CUSTOMER_REV |
|---|---|---|
| damaged | 2 | 350.00000000 |
| late | 4 | 1100.00000000 |
| None | 1 | 100.00000000 |

## C6

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS avg_orders_fixed, avg_customer_rev) ORDER BY 1 NULLS LAST
```

| REASON | AVG_ORDERS_FIXED | AVG_CUSTOMER_REV |
|---|---|---|
| damaged | 3.500000 | 350.00000000 |
| late | 4.000000 | 1100.00000000 |
| None | 2.333333 | 100.00000000 |

## C7

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS avg_customer_rev, max_order_rev) ORDER BY 1 NULLS LAST
```

| REASON | AVG_CUSTOMER_REV | MAX_ORDER_REV |
|---|---|---|
| damaged | 350.00000000 | 400.00 |
| late | 1100.00000000 | 400.00 |
| None | 100.00000000 | 100.00 |

## C8

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.customer_id, order_returns.reason METRICS orders.total_revenue, avg_customer_rev) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| CUSTOMER_ID | REASON | TOTAL_REVENUE | AVG_CUSTOMER_REV |
|---|---|---|---|
| 1 | damaged | 400.00 | 400.00000000 |
| 1 | late | 900.00 | 900.00000000 |
| 1 | None | 100.00 | 100.00000000 |
| 2 | None | 300.00 | None |
| 3 | damaged | 300.00 | 300.00000000 |
| 3 | None | 300.00 | None |
| 4 | None | 200.00 | None |
| None | None | None | 266.66666667 |

## C9

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.customer_id, order_returns.reason METRICS orders.total_revenue) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| CUSTOMER_ID | REASON | TOTAL_REVENUE |
|---|---|---|
| 1 | damaged | 400.00 |
| 1 | late | 900.00 |
| 1 | None | 100.00 |
| 2 | None | 300.00 |
| 3 | damaged | 300.00 |
| 3 | None | 300.00 |
| 4 | None | 200.00 |

## V3

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_cust_first
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (a_ord_cust AS orders(customer_id) REFERENCES customers, b_ord_store AS orders(store_id) REFERENCES stores,
    a_cust_reg AS customers(region) REFERENCES regions, b_store_reg AS stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    avg_store_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (stores.store_id)),
    avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id))
  )
```

| status |
|---|
| Semantic view SD_CUST_FIRST successfully created. |

## D1

```sql
SELECT * FROM SEMANTIC_VIEW(sd_cust_first DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1300.00000000 |
| Sam | 800.00000000 |

## D2

```sql
SELECT * FROM SEMANTIC_VIEW(sd_cust_first DIMENSIONS regions.manager METRICS avg_customer_rev) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_CUSTOMER_REV |
|---|---|
| Nina | 650.00000000 |
| Sam | 400.00000000 |

## D3

```sql
SELECT * FROM SEMANTIC_VIEW(sd_cust_first DIMENSIONS regions.manager METRICS avg_store_rev) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV |
|---|---|
| Nina | 800.00000000 |
| Sam | 600.00000000 |
| None | 350.00000000 |

## D4

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sd_cust_first DIMENSIONS regions.manager METRICS avg_store_rev_excl)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 4 | 4 | 4096 |
| 1 | 0 | None | Result | None | None | SYS_VW.MANAGER_0_7_0, SCALED_ROUND_INT_DIVIDE(SUM(SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))))) | None | None | None |
| 1 | 1 | [0] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))))], groupKeys: [SYS_VW.MANAGER_0_7_0] | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))), COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))], groupKeys: [SYS_VW.MANAGER_0_7_0] | None | None | None |
| 1 | 3 | [2] | LeftOuterJoin | None | None | joinKey: (SYS_VW.MANAGER_0_7_0 = REGIONS.MANAGER) | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 5 | [4] | TableScan | S_LAYER.SV_P8_FIX14D.REGIONS | REGIONS | MANAGER | 1 | 1 | 1024 |
| 1 | 6 | [3] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))), COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 7 | [6] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 8 | [7] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT)))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 9 | [8] | LeftOuterJoin | None | None | joinKey: (REGIONS.REGION = CUSTOMERS.REGION) | None | None | None |
| 1 | 10 | [9] | Filter | None | None | REGIONS.REGION IS NOT NULL | None | None | None |
| 1 | 11 | [10] | TableScan | S_LAYER.SV_P8_FIX14D.REGIONS | REGIONS | REGION, MANAGER | 1 | 1 | 1024 |
| 1 | 12 | [9] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT))], groupKeys: [CUSTOMERS.REGION] | None | None | None |
| 1 | 13 | [12] | LeftOuterJoin | None | None | joinKey: (CUSTOMERS.CUSTOMER_ID = ORDERS.CUSTOMER_ID) | None | None | None |
| 1 | 14 | [13] | Filter | None | None | CUSTOMERS.CUSTOMER_ID IS NOT NULL | None | None | None |
| 1 | 15 | [14] | TableScan | S_LAYER.SV_P8_FIX14D.CUSTOMERS | CUSTOMERS | CUSTOMER_ID, REGION | 1 | 1 | 1024 |
| 1 | 16 | [13] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDERS.CUSTOMER_ID] | None | None | None |
| 1 | 17 | [16] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | CUSTOMER_ID, AMOUNT | 1 | 1 | 1024 |

## V4

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_store_first
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (b_ord_cust AS orders(customer_id) REFERENCES customers, a_ord_store AS orders(store_id) REFERENCES stores,
    b_cust_reg AS customers(region) REFERENCES regions, a_store_reg AS stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    avg_store_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (stores.store_id)),
    avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id))
  )
```

| status |
|---|
| Semantic view SD_STORE_FIRST successfully created. |

## D5

```sql
SELECT * FROM SEMANTIC_VIEW(sd_store_first DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1000.00000000 |
| Sam | 1100.00000000 |

## D6

```sql
SELECT * FROM SEMANTIC_VIEW(sd_store_first DIMENSIONS regions.manager METRICS avg_customer_rev) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_CUSTOMER_REV |
|---|---|
| Nina | 400.00000000 |
| Sam | 300.00000000 |
| None | 233.33333333 |

## D7

```sql
SELECT * FROM SEMANTIC_VIEW(sd_store_first DIMENSIONS regions.manager METRICS avg_store_rev) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV |
|---|---|
| Nina | 1000.00000000 |
| Sam | 1100.00000000 |

## D8

```sql
EXPLAIN SELECT * FROM SEMANTIC_VIEW(sd_store_first DIMENSIONS regions.manager METRICS avg_store_rev_excl)
```

| step | id | parentOperators | operation | objects | alias | expressions | partitionsTotal | partitionsAssigned | bytesAssigned |
|---|---|---|---|---|---|---|---|---|---|
| None | None | None | GlobalStats | None | None | None | 4 | 4 | 4096 |
| 1 | 0 | None | Result | None | None | SYS_VW.MANAGER_0_7_0, SCALED_ROUND_INT_DIVIDE(SUM(SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))))) | None | None | None |
| 1 | 1 | [0] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))), COUNT(COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))))], groupKeys: [SYS_VW.MANAGER_0_7_0] | None | None | None |
| 1 | 2 | [1] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))), COUNT(COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))))], groupKeys: [SYS_VW.MANAGER_0_7_0] | None | None | None |
| 1 | 3 | [2] | LeftOuterJoin | None | None | joinKey: (SYS_VW.MANAGER_0_7_0 = REGIONS.MANAGER) | None | None | None |
| 1 | 4 | [3] | Aggregate | None | None | groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 5 | [4] | TableScan | S_LAYER.SV_P8_FIX14D.REGIONS | REGIONS | MANAGER | 1 | 1 | 1024 |
| 1 | 6 | [3] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))), COUNT(SUM(SUM(SUM(SUM(ORDERS.AMOUNT)))))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 7 | [6] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(SUM(ORDERS.AMOUNT))))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 8 | [7] | Aggregate | None | None | aggExprs: [SUM(SUM(SUM(ORDERS.AMOUNT)))], groupKeys: [REGIONS.MANAGER] | None | None | None |
| 1 | 9 | [8] | LeftOuterJoin | None | None | joinKey: (REGIONS.REGION = STORES.REGION) | None | None | None |
| 1 | 10 | [9] | Filter | None | None | REGIONS.REGION IS NOT NULL | None | None | None |
| 1 | 11 | [10] | TableScan | S_LAYER.SV_P8_FIX14D.REGIONS | REGIONS | REGION, MANAGER | 1 | 1 | 1024 |
| 1 | 12 | [9] | Aggregate | None | None | aggExprs: [SUM(SUM(ORDERS.AMOUNT))], groupKeys: [STORES.REGION] | None | None | None |
| 1 | 13 | [12] | LeftOuterJoin | None | None | joinKey: (STORES.STORE_ID = ORDERS.STORE_ID) | None | None | None |
| 1 | 14 | [13] | Filter | None | None | STORES.STORE_ID IS NOT NULL | None | None | None |
| 1 | 15 | [14] | TableScan | S_LAYER.SV_P8_FIX14D.STORES | STORES | STORE_ID, REGION | 1 | 1 | 1024 |
| 1 | 16 | [13] | Aggregate | None | None | aggExprs: [SUM(ORDERS.AMOUNT)], groupKeys: [ORDERS.STORE_ID] | None | None | None |
| 1 | 17 | [16] | TableScan | S_LAYER.SV_P8_FIX14D.ORDERS | ORDERS | STORE_ID, AMOUNT | 1 | 1 | 1024 |

## V5

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_cust_first_rev
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (b_store_reg AS stores(region) REFERENCES regions, a_cust_reg AS customers(region) REFERENCES regions,
    b_ord_store AS orders(store_id) REFERENCES stores, a_ord_cust AS orders(customer_id) REFERENCES customers)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)))
```

| status |
|---|
| Semantic view SD_CUST_FIRST_REV successfully created. |

## D9

```sql
SELECT * FROM SEMANTIC_VIEW(sd_cust_first_rev DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1300.00000000 |
| Sam | 800.00000000 |

## V6

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_unnamed
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (orders(customer_id) REFERENCES customers, orders(store_id) REFERENCES stores,
    customers(region) REFERENCES regions, stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)))
```

| status |
|---|
| Semantic view SD_UNNAMED successfully created. |

## D10

```sql
SELECT * FROM SEMANTIC_VIEW(sd_unnamed DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1000.00000000 |
| Sam | 1100.00000000 |

## V7

```sql
DESCRIBE SEMANTIC VIEW sd_unnamed
```

| object_kind | object_name | parent_entity | property | property_value |
|---|---|---|---|---|
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | EXPRESSION | AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | DATA_TYPE | NUMBER(38,8) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | ACCESS_MODIFIER | PUBLIC |
| TABLE | CUSTOMERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | CUSTOMERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | CUSTOMERS | None | BASE_TABLE_NAME | CUSTOMERS |
| TABLE | CUSTOMERS | None | PRIMARY_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_180178be-0e6a-411d-bc17-29d125abdf0d | CUSTOMERS | TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_180178be-0e6a-411d-bc17-29d125abdf0d | CUSTOMERS | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_180178be-0e6a-411d-bc17-29d125abdf0d | CUSTOMERS | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_180178be-0e6a-411d-bc17-29d125abdf0d | CUSTOMERS | REF_KEY | ["REGION"] |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | TABLE | CUSTOMERS |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | EXPRESSION | customer_id |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | ORDERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | ORDERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | ORDERS | None | BASE_TABLE_NAME | ORDERS |
| TABLE | ORDERS | None | PRIMARY_KEY | ["ORDER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_22ce6259-bf04-4166-9753-758981b733bf | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_22ce6259-bf04-4166-9753-758981b733bf | ORDERS | REF_TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_22ce6259-bf04-4166-9753-758981b733bf | ORDERS | FOREIGN_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_22ce6259-bf04-4166-9753-758981b733bf | ORDERS | REF_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_ab13aa4b-6efc-44be-93ed-bd9218da02f2 | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_ab13aa4b-6efc-44be-93ed-bd9218da02f2 | ORDERS | REF_TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_ab13aa4b-6efc-44be-93ed-bd9218da02f2 | ORDERS | FOREIGN_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_ab13aa4b-6efc-44be-93ed-bd9218da02f2 | ORDERS | REF_KEY | ["CUSTOMER_ID"] |
| FACT | ORDER_AMOUNT | ORDERS | TABLE | ORDERS |
| FACT | ORDER_AMOUNT | ORDERS | EXPRESSION | amount |
| FACT | ORDER_AMOUNT | ORDERS | DATA_TYPE | NUMBER(10,2) |
| FACT | ORDER_AMOUNT | ORDERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | REGIONS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | REGIONS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | REGIONS | None | BASE_TABLE_NAME | REGIONS |
| TABLE | REGIONS | None | PRIMARY_KEY | ["REGION"] |
| DIMENSION | MANAGER | REGIONS | TABLE | REGIONS |
| DIMENSION | MANAGER | REGIONS | EXPRESSION | manager |
| DIMENSION | MANAGER | REGIONS | DATA_TYPE | VARCHAR(16777216) |
| DIMENSION | MANAGER | REGIONS | ACCESS_MODIFIER | PUBLIC |
| TABLE | STORES | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | STORES | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | STORES | None | BASE_TABLE_NAME | STORES |
| TABLE | STORES | None | PRIMARY_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_c7ef5ae2-1b70-46e4-bc68-8f96b4905834 | STORES | TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_c7ef5ae2-1b70-46e4-bc68-8f96b4905834 | STORES | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_c7ef5ae2-1b70-46e4-bc68-8f96b4905834 | STORES | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_c7ef5ae2-1b70-46e4-bc68-8f96b4905834 | STORES | REF_KEY | ["REGION"] |
| DIMENSION | STORE_ID | STORES | TABLE | STORES |
| DIMENSION | STORE_ID | STORES | EXPRESSION | store_id |
| DIMENSION | STORE_ID | STORES | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | STORE_ID | STORES | ACCESS_MODIFIER | PUBLIC |

## V8

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_unnamed
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (orders(customer_id) REFERENCES customers, orders(store_id) REFERENCES stores,
    customers(region) REFERENCES regions, stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)))
```

| status |
|---|
| Semantic view SD_UNNAMED successfully created. |

## D11

```sql
SELECT * FROM SEMANTIC_VIEW(sd_unnamed DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1000.00000000 |
| Sam | 1100.00000000 |

## V9

```sql
DESCRIBE SEMANTIC VIEW sd_unnamed
```

| object_kind | object_name | parent_entity | property | property_value |
|---|---|---|---|---|
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | EXPRESSION | AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | DATA_TYPE | NUMBER(38,8) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | ACCESS_MODIFIER | PUBLIC |
| TABLE | CUSTOMERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | CUSTOMERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | CUSTOMERS | None | BASE_TABLE_NAME | CUSTOMERS |
| TABLE | CUSTOMERS | None | PRIMARY_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_40797c41-2664-46bd-9597-43398b3c83b2 | CUSTOMERS | TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_40797c41-2664-46bd-9597-43398b3c83b2 | CUSTOMERS | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_40797c41-2664-46bd-9597-43398b3c83b2 | CUSTOMERS | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_40797c41-2664-46bd-9597-43398b3c83b2 | CUSTOMERS | REF_KEY | ["REGION"] |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | TABLE | CUSTOMERS |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | EXPRESSION | customer_id |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | ORDERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | ORDERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | ORDERS | None | BASE_TABLE_NAME | ORDERS |
| TABLE | ORDERS | None | PRIMARY_KEY | ["ORDER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_397a579e-828d-4162-882c-bcfecfb076c6 | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_397a579e-828d-4162-882c-bcfecfb076c6 | ORDERS | REF_TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_397a579e-828d-4162-882c-bcfecfb076c6 | ORDERS | FOREIGN_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_397a579e-828d-4162-882c-bcfecfb076c6 | ORDERS | REF_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_e4cde97c-f545-4019-984d-87fd34d66100 | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_e4cde97c-f545-4019-984d-87fd34d66100 | ORDERS | REF_TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_e4cde97c-f545-4019-984d-87fd34d66100 | ORDERS | FOREIGN_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_e4cde97c-f545-4019-984d-87fd34d66100 | ORDERS | REF_KEY | ["CUSTOMER_ID"] |
| FACT | ORDER_AMOUNT | ORDERS | TABLE | ORDERS |
| FACT | ORDER_AMOUNT | ORDERS | EXPRESSION | amount |
| FACT | ORDER_AMOUNT | ORDERS | DATA_TYPE | NUMBER(10,2) |
| FACT | ORDER_AMOUNT | ORDERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | REGIONS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | REGIONS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | REGIONS | None | BASE_TABLE_NAME | REGIONS |
| TABLE | REGIONS | None | PRIMARY_KEY | ["REGION"] |
| DIMENSION | MANAGER | REGIONS | TABLE | REGIONS |
| DIMENSION | MANAGER | REGIONS | EXPRESSION | manager |
| DIMENSION | MANAGER | REGIONS | DATA_TYPE | VARCHAR(16777216) |
| DIMENSION | MANAGER | REGIONS | ACCESS_MODIFIER | PUBLIC |
| TABLE | STORES | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | STORES | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | STORES | None | BASE_TABLE_NAME | STORES |
| TABLE | STORES | None | PRIMARY_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_7e964d29-2c3f-4712-9d98-2c52090c26cc | STORES | TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_7e964d29-2c3f-4712-9d98-2c52090c26cc | STORES | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_7e964d29-2c3f-4712-9d98-2c52090c26cc | STORES | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_7e964d29-2c3f-4712-9d98-2c52090c26cc | STORES | REF_KEY | ["REGION"] |
| DIMENSION | STORE_ID | STORES | TABLE | STORES |
| DIMENSION | STORE_ID | STORES | EXPRESSION | store_id |
| DIMENSION | STORE_ID | STORES | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | STORE_ID | STORES | ACCESS_MODIFIER | PUBLIC |

## V10

```sql
CREATE OR REPLACE SEMANTIC VIEW sd_unnamed
  TABLES (customers AS customers PRIMARY KEY (customer_id), orders AS orders PRIMARY KEY (order_id),
    stores AS stores PRIMARY KEY (store_id), regions AS regions PRIMARY KEY (region))
  RELATIONSHIPS (orders(customer_id) REFERENCES customers, orders(store_id) REFERENCES stores,
    customers(region) REFERENCES regions, stores(region) REFERENCES regions)
  FACTS (orders.order_amount AS amount)
  DIMENSIONS (customers.customer_id AS customer_id, stores.store_id AS store_id, regions.manager AS manager)
  METRICS (avg_store_rev_excl AS AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)))
```

| status |
|---|
| Semantic view SD_UNNAMED successfully created. |

## D12

```sql
SELECT * FROM SEMANTIC_VIEW(sd_unnamed DIMENSIONS regions.manager METRICS avg_store_rev_excl) ORDER BY 1 NULLS LAST
```

| MANAGER | AVG_STORE_REV_EXCL |
|---|---|
| Nina | 1300.00000000 |
| Sam | 800.00000000 |

## V11

```sql
DESCRIBE SEMANTIC VIEW sd_unnamed
```

| object_kind | object_name | parent_entity | property | property_value |
|---|---|---|---|---|
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | EXPRESSION | AVG(SUM(orders.order_amount) EXCLUDE DIMENSIONS (stores.store_id)) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | DATA_TYPE | NUMBER(38,8) |
| DERIVED_METRIC | AVG_STORE_REV_EXCL | None | ACCESS_MODIFIER | PUBLIC |
| TABLE | CUSTOMERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | CUSTOMERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | CUSTOMERS | None | BASE_TABLE_NAME | CUSTOMERS |
| TABLE | CUSTOMERS | None | PRIMARY_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_c19a26df-055c-4a35-92e9-d08ccb9e4a6c | CUSTOMERS | TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_c19a26df-055c-4a35-92e9-d08ccb9e4a6c | CUSTOMERS | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_c19a26df-055c-4a35-92e9-d08ccb9e4a6c | CUSTOMERS | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_c19a26df-055c-4a35-92e9-d08ccb9e4a6c | CUSTOMERS | REF_KEY | ["REGION"] |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | TABLE | CUSTOMERS |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | EXPRESSION | customer_id |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | CUSTOMER_ID | CUSTOMERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | ORDERS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | ORDERS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | ORDERS | None | BASE_TABLE_NAME | ORDERS |
| TABLE | ORDERS | None | PRIMARY_KEY | ["ORDER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_545cc4f9-dcef-4922-90cb-63b0faaf86fe | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_545cc4f9-dcef-4922-90cb-63b0faaf86fe | ORDERS | REF_TABLE | CUSTOMERS |
| RELATIONSHIP | SYS_RELATIONSHIP_545cc4f9-dcef-4922-90cb-63b0faaf86fe | ORDERS | FOREIGN_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_545cc4f9-dcef-4922-90cb-63b0faaf86fe | ORDERS | REF_KEY | ["CUSTOMER_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_bfb7a117-149f-4243-9901-d249b11294fb | ORDERS | TABLE | ORDERS |
| RELATIONSHIP | SYS_RELATIONSHIP_bfb7a117-149f-4243-9901-d249b11294fb | ORDERS | REF_TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_bfb7a117-149f-4243-9901-d249b11294fb | ORDERS | FOREIGN_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_bfb7a117-149f-4243-9901-d249b11294fb | ORDERS | REF_KEY | ["STORE_ID"] |
| FACT | ORDER_AMOUNT | ORDERS | TABLE | ORDERS |
| FACT | ORDER_AMOUNT | ORDERS | EXPRESSION | amount |
| FACT | ORDER_AMOUNT | ORDERS | DATA_TYPE | NUMBER(10,2) |
| FACT | ORDER_AMOUNT | ORDERS | ACCESS_MODIFIER | PUBLIC |
| TABLE | REGIONS | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | REGIONS | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | REGIONS | None | BASE_TABLE_NAME | REGIONS |
| TABLE | REGIONS | None | PRIMARY_KEY | ["REGION"] |
| DIMENSION | MANAGER | REGIONS | TABLE | REGIONS |
| DIMENSION | MANAGER | REGIONS | EXPRESSION | manager |
| DIMENSION | MANAGER | REGIONS | DATA_TYPE | VARCHAR(16777216) |
| DIMENSION | MANAGER | REGIONS | ACCESS_MODIFIER | PUBLIC |
| TABLE | STORES | None | BASE_TABLE_DATABASE_NAME | S_LAYER |
| TABLE | STORES | None | BASE_TABLE_SCHEMA_NAME | SV_P8_FIX14D |
| TABLE | STORES | None | BASE_TABLE_NAME | STORES |
| TABLE | STORES | None | PRIMARY_KEY | ["STORE_ID"] |
| RELATIONSHIP | SYS_RELATIONSHIP_f1e4486f-d025-400d-9a45-3040f82b8e71 | STORES | TABLE | STORES |
| RELATIONSHIP | SYS_RELATIONSHIP_f1e4486f-d025-400d-9a45-3040f82b8e71 | STORES | REF_TABLE | REGIONS |
| RELATIONSHIP | SYS_RELATIONSHIP_f1e4486f-d025-400d-9a45-3040f82b8e71 | STORES | FOREIGN_KEY | ["REGION"] |
| RELATIONSHIP | SYS_RELATIONSHIP_f1e4486f-d025-400d-9a45-3040f82b8e71 | STORES | REF_KEY | ["REGION"] |
| DIMENSION | STORE_ID | STORES | TABLE | STORES |
| DIMENSION | STORE_ID | STORES | EXPRESSION | store_id |
| DIMENSION | STORE_ID | STORES | DATA_TYPE | NUMBER(38,0) |
| DIMENSION | STORE_ID | STORES | ACCESS_MODIFIER | PUBLIC |

## DROP SCHEMA S_LAYER.SV_P8_FIX14D

```sql
DROP SCHEMA S_LAYER.SV_P8_FIX14D
```

| status |
|---|
| SV_P8_FIX14D successfully dropped. |
