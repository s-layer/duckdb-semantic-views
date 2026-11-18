# Snowflake's results for phase8-fixture-14g.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14G

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P8_FIX14G
```

| status |
|---|
| Schema SV_P8_FIX14G successfully created. |

## USE SCHEMA S_LAYER.SV_P8_FIX14G

```sql
USE SCHEMA S_LAYER.SV_P8_FIX14G
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

## INSERT INTO order_returns VALUES (8, 999, 'lost')

```sql
INSERT INTO order_returns VALUES (8, 999, 'lost')
```

| number of rows inserted |
|---|
| 1 |

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
    orders.category AS category, orders.order_id AS order_id, order_returns.reason AS reason)
  METRICS (
    orders.total_revenue AS SUM(amount),
    avg_customer_rev AS AVG(SUM(orders.order_amount) INCLUDE DIMENSIONS (customers.customer_id)),
    avg_orders_fixed AS AVG(COUNT(orders.order_id) FIXED DIMENSIONS (customers.customer_id))
  )
```

| status |
|---|
| Semantic view SALES_CHILD successfully created. |

## J1

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS order_returns.reason METRICS avg_orders_fixed) ORDER BY 1 NULLS LAST
```

| REASON | AVG_ORDERS_FIXED |
|---|---|
| damaged | 3.500000 |
| late | 4.000000 |
| None | 2.333333 |

## J2

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region, order_returns.reason METRICS avg_orders_fixed) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| REGION | REASON | AVG_ORDERS_FIXED |
|---|---|---|
| North | damaged | 4.000000 |
| North | late | 4.000000 |
| North | None | 4.000000 |
| South | damaged | 3.000000 |
| None | None | 1.500000 |

## J3

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region, order_returns.reason METRICS orders.total_revenue, avg_orders_fixed) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| REGION | REASON | TOTAL_REVENUE | AVG_ORDERS_FIXED |
|---|---|---|---|
| North | damaged | 400.00 | 4.000000 |
| North | late | 900.00 | 4.000000 |
| North | None | 400.00 | 4.000000 |
| South | damaged | 300.00 | 3.000000 |
| South | None | 500.00 | None |
| None | None | None | 1.500000 |

## J4

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS orders.category, order_returns.reason METRICS avg_orders_fixed) ORDER BY 1 NULLS LAST, 2 NULLS LAST
```

| CATEGORY | REASON | AVG_ORDERS_FIXED |
|---|---|---|
| Clothing | late | 4.000000 |
| Clothing | None | 4.000000 |
| Electronics | damaged | 3.500000 |
| Electronics | late | 4.000000 |
| None | None | 1.500000 |

## J5

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS avg_orders_fixed WHERE order_returns.reason IS NULL) ORDER BY 1 NULLS LAST
```

| REGION | AVG_ORDERS_FIXED |
|---|---|
| North | 4.000000 |
| None | 1.500000 |

## J6

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS avg_orders_fixed WHERE order_returns.reason IS NULL OR order_returns.reason = 'damaged') ORDER BY 1 NULLS LAST
```

| REGION | AVG_ORDERS_FIXED |
|---|---|
| North | 4.000000 |
| South | 3.000000 |
| None | 1.500000 |

## J7

```sql
SELECT * FROM SEMANTIC_VIEW(sales_child DIMENSIONS customers.region METRICS avg_orders_fixed WHERE order_returns.reason = 'late') ORDER BY 1 NULLS LAST
```

| REGION | AVG_ORDERS_FIXED |
|---|---|
| North | 4.000000 |

## DROP SCHEMA S_LAYER.SV_P8_FIX14G

```sql
DROP SCHEMA S_LAYER.SV_P8_FIX14G
```

| status |
|---|
| SV_P8_FIX14G successfully dropped. |
