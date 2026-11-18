# Snowflake's results for phase7-fixture-5.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX5

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX5
```

| status |
|---|
| Schema SV_P7_FIX5 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX5

```sql
USE SCHEMA S_LAYER.SV_P7_FIX5
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE emp (emp_id INT, dept_id INT, name VARCHAR)

```sql
CREATE TABLE emp (emp_id INT, dept_id INT, name VARCHAR)
```

| status |
|---|
| Table EMP successfully created. |

## CREATE TABLE dept (dept_id INT, head_emp_id INT, dname VARCH

```sql
CREATE TABLE dept (dept_id INT, head_emp_id INT, dname VARCHAR)
```

| status |
|---|
| Table DEPT successfully created. |

## CREATE TABLE ca (ca_id INT, cb_id INT)

```sql
CREATE TABLE ca (ca_id INT, cb_id INT)
```

| status |
|---|
| Table CA successfully created. |

## CREATE TABLE cb (cb_id INT, cc_id INT)

```sql
CREATE TABLE cb (cb_id INT, cc_id INT)
```

| status |
|---|
| Table CB successfully created. |

## CREATE TABLE cc (cc_id INT, ca_id INT)

```sql
CREATE TABLE cc (cc_id INT, ca_id INT)
```

| status |
|---|
| Table CC successfully created. |

## INSERT INTO emp VALUES (1, 10, 'Ana'), (2, 10, 'Bo'), (3, 20

```sql
INSERT INTO emp VALUES (1, 10, 'Ana'), (2, 10, 'Bo'), (3, 20, 'Cy')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO dept VALUES (10, 1, 'Eng'), (20, 3, 'Ops')

```sql
INSERT INTO dept VALUES (10, 1, 'Eng'), (20, 3, 'Ops')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO ca VALUES (1, 1)

```sql
INSERT INTO ca VALUES (1, 1)
```

| number of rows inserted |
|---|
| 1 |

## INSERT INTO cb VALUES (1, 1)

```sql
INSERT INTO cb VALUES (1, 1)
```

| number of rows inserted |
|---|
| 1 |

## INSERT INTO cc VALUES (1, 1)

```sql
INSERT INTO cc VALUES (1, 1)
```

| number of rows inserted |
|---|
| 1 |

## C1

```sql
CREATE OR REPLACE SEMANTIC VIEW cyc2
  TABLES (
    e AS emp  PRIMARY KEY (emp_id),
    d AS dept PRIMARY KEY (dept_id)
  )
  RELATIONSHIPS (
    e_dept AS e (dept_id)     REFERENCES d,
    d_head AS d (head_emp_id) REFERENCES e
  )
  DIMENSIONS (e.name AS name, d.dname AS dname)
  METRICS (e.emp_count AS COUNT(emp_id), d.dept_count AS COUNT(dept_id))
```

**Error:**

```
010210 (42601): SQL compilation error:
Cyclic relationships exist between entity 'E' and 'D'.
```

## C2

```sql
SELECT * FROM SEMANTIC_VIEW(cyc2 DIMENSIONS d.dname METRICS e.emp_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX5.CYC2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX5.CYC2.
```

## C3

```sql
SELECT * FROM SEMANTIC_VIEW(cyc2 DIMENSIONS e.name METRICS d.dept_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX5.CYC2' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX5.CYC2.
```

## C4

```sql
CREATE OR REPLACE SEMANTIC VIEW cyc3
  TABLES (
    a AS ca PRIMARY KEY (ca_id),
    b AS cb PRIMARY KEY (cb_id),
    c AS cc PRIMARY KEY (cc_id)
  )
  RELATIONSHIPS (
    a_b AS a (cb_id) REFERENCES b,
    b_c AS b (cc_id) REFERENCES c,
    c_a AS c (ca_id) REFERENCES a
  )
  DIMENSIONS (a.ca_id AS ca_id, c.cc_id AS cc_id)
  METRICS (a.a_count AS COUNT(ca_id))
```

**Error:**

```
010232 (42601): SQL compilation error:
Cycle in the relationship graph is not allowed. Cycle detected: [CC,CA,CB]
```

## CREATE TABLE regions (region_id INT, region_name VARCHAR)

```sql
CREATE TABLE regions (region_id INT, region_name VARCHAR)
```

| status |
|---|
| Table REGIONS successfully created. |

## CREATE TABLE customers (cust_id INT, cust_name VARCHAR, regi

```sql
CREATE TABLE customers (cust_id INT, cust_name VARCHAR, region_id INT)
```

| status |
|---|
| Table CUSTOMERS successfully created. |

## CREATE TABLE stores (store_id INT, store_name VARCHAR, regio

```sql
CREATE TABLE stores (store_id INT, store_name VARCHAR, region_id INT)
```

| status |
|---|
| Table STORES successfully created. |

## CREATE TABLE orders (order_id INT, cust_id INT, store_id INT

```sql
CREATE TABLE orders (order_id INT, cust_id INT, store_id INT, amount INT)
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

## INSERT INTO customers VALUES (1, 'Ana', 1), (2, 'Bo', 2)

```sql
INSERT INTO customers VALUES (1, 'Ana', 1), (2, 'Bo', 2)
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO stores VALUES (1, 'S-US', 2), (2, 'S-EU', 1)

```sql
INSERT INTO stores VALUES (1, 'S-US', 2), (2, 'S-EU', 1)
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO orders VALUES (1, 1, 1, 10), (2, 2, 1, 20), (3,

```sql
INSERT INTO orders VALUES (1, 1, 1, 10), (2, 2, 1, 20), (3, 1, 2, 40)
```

| number of rows inserted |
|---|
| 3 |

## CREATE OR REPLACE SEMANTIC VIEW xv

```sql
CREATE OR REPLACE SEMANTIC VIEW xv
  TABLES (
    o AS orders    PRIMARY KEY (order_id),
    c AS customers PRIMARY KEY (cust_id),
    s AS stores    PRIMARY KEY (store_id),
    r AS regions   PRIMARY KEY (region_id)
  )
  RELATIONSHIPS (
    o_cust   AS o (cust_id)   REFERENCES c,
    o_store  AS o (store_id)  REFERENCES s,
    c_region AS c (region_id) REFERENCES r,
    s_region AS s (region_id) REFERENCES r
  )
  DIMENSIONS (
    o.order_id    AS order_id,
    c.cust_name   AS cust_name,
    s.store_name  AS store_name,
    r.region_name AS region_name
  )
  METRICS (
    o.revenue        AS SUM(amount),
    c.customer_count AS COUNT(cust_id),
    s.store_count    AS COUNT(store_id),
    r.region_count   AS COUNT(region_id)
  )
```

| status |
|---|
| Semantic view XV successfully created. |

## X1

```sql
SELECT * FROM SEMANTIC_VIEW(xv DIMENSIONS s.store_name METRICS c.customer_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## X2

```sql
SELECT * FROM SEMANTIC_VIEW(xv DIMENSIONS c.cust_name METRICS s.store_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## X3

```sql
SELECT * FROM SEMANTIC_VIEW(xv DIMENSIONS c.cust_name, s.store_name) ORDER BY ALL NULLS LAST
```

| CUST_NAME | STORE_NAME |
|---|---|
| Ana | S-EU |
| Ana | S-US |
| Bo | S-US |

## X4

```sql
SELECT * FROM SEMANTIC_VIEW(xv METRICS c.customer_count WHERE s.store_name = 'S-US')
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## X5

```sql
SELECT * FROM SEMANTIC_VIEW(xv DIMENSIONS c.cust_name METRICS r.region_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## X6

```sql
SELECT * FROM SEMANTIC_VIEW(xv DIMENSIONS o.order_id METRICS r.region_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## CREATE TABLE authors (author_id INT, name VARCHAR, country V

```sql
CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR)
```

| status |
|---|
| Table AUTHORS successfully created. |

## CREATE TABLE books (book_id INT, title VARCHAR, pages INT, l

```sql
CREATE TABLE books (book_id INT, title VARCHAR, pages INT, lead_id INT)
```

| status |
|---|
| Table BOOKS successfully created. |

## CREATE TABLE book_authors (book_id INT, author_id INT, role

```sql
CREATE TABLE book_authors (book_id INT, author_id INT, role VARCHAR)
```

| status |
|---|
| Table BOOK_AUTHORS successfully created. |

## INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'

```sql
INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'), (4, 'Dan', 'DE')
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO books VALUES (10, 'Duo', 100, 1), (11, 'Mixed',

```sql
INSERT INTO books VALUES (10, 'Duo', 100, 1), (11, 'Mixed', 200, 3), (12, 'Solo', 300, 3), (13, 'Orphan', 50, NULL)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co

```sql
INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co'), (11, 1, 'co'), (11, 3, 'lead'), (12, 3, 'lead')
```

| number of rows inserted |
|---|
| 5 |

## P0

```sql
CREATE OR REPLACE SEMANTIC VIEW pv
  TABLES (
    b  AS books        PRIMARY KEY (book_id),
    a  AS authors      PRIMARY KEY (author_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id)
  )
  RELATIONSHIPS (
    b_lead    AS b (lead_id)    REFERENCES a,
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title, ba.role AS role)
  METRICS (
    b.total_pages   AS SUM(pages),
    b.pages_by_lead USING (b_lead) AS SUM(pages),
    a.author_count  AS COUNT(author_id),
    ba.credits      AS COUNT(*)
  )
```

| status |
|---|
| Semantic view PV successfully created. |

## P1

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS a.country METRICS b.total_pages) ORDER BY ALL NULLS LAST
```

| COUNTRY | TOTAL_PAGES |
|---|---|
| FR | 100 |
| UK | 500 |
| None | 50 |

## P2

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS a.country METRICS b.pages_by_lead) ORDER BY ALL NULLS LAST
```

| COUNTRY | PAGES_BY_LEAD |
|---|---|
| FR | 100 |
| UK | 500 |
| None | 50 |

## P3

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS ba.role METRICS b.total_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## P4

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS b.title METRICS a.author_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## P5

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS b.title, a.country) ORDER BY ALL NULLS LAST
```

| TITLE | COUNTRY |
|---|---|
| Duo | FR |
| Mixed | UK |
| Orphan | None |
| Solo | UK |

## P6

```sql
SELECT * FROM SEMANTIC_VIEW(pv DIMENSIONS a.country METRICS ba.credits) ORDER BY ALL NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'A' and the base metric or dimension entity 'BA' is not supported.
```

## P7

```sql
SELECT * FROM SEMANTIC_VIEW(pv METRICS b.total_pages WHERE a.country = 'FR')
```

| TOTAL_PAGES |
|---|
| 100 |

## CREATE TABLE reviews (review_id INT, book_id INT, stars INT)

```sql
CREATE TABLE reviews (review_id INT, book_id INT, stars INT)
```

| status |
|---|
| Table REVIEWS successfully created. |

## INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4

```sql
INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4), (4, 13, 2), (5, NULL, 1)
```

| number of rows inserted |
|---|
| 5 |

## CREATE TABLE h_books (book_id INT, title VARCHAR, pages INT)

```sql
CREATE TABLE h_books (book_id INT, title VARCHAR, pages INT)
```

| status |
|---|
| Table H_BOOKS successfully created. |

## CREATE TABLE h_book_authors (book_id INT, author_id INT)

```sql
CREATE TABLE h_book_authors (book_id INT, author_id INT)
```

| status |
|---|
| Table H_BOOK_AUTHORS successfully created. |

## INSERT INTO h_books VALUES (10, 'Duo', 100), (11, 'Mixed', 2

```sql
INSERT INTO h_books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300), (13, 'Orphan', 50)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO h_book_authors VALUES (10, 1), (10, 2), (11, 1),

```sql
INSERT INTO h_book_authors VALUES (10, 1), (10, 2), (11, 1), (11, 3), (12, 3)
```

| number of rows inserted |
|---|
| 5 |

## CREATE OR REPLACE SEMANTIC VIEW hv

```sql
CREATE OR REPLACE SEMANTIC VIEW hv
  TABLES (
    a  AS authors        PRIMARY KEY (author_id),
    b  AS h_books        PRIMARY KEY (book_id),
    ba AS h_book_authors PRIMARY KEY (book_id, author_id),
    rv AS reviews        PRIMARY KEY (review_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    rv_book   AS rv (book_id)   REFERENCES b
  )
  DIMENSIONS (
    a.country    AS country,
    a.name       AS name,
    b.title      AS title,
    rv.review_id AS review_id,
    rv.stars     AS stars
  )
  METRICS (rv.review_count AS COUNT(review_id))
```

| status |
|---|
| Semantic view HV successfully created. |

## H7

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.review_id, a.name) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## H8

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.review_id, b.title, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## H9

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS b.title, a.country) ORDER BY ALL NULLS LAST
```

| TITLE | COUNTRY |
|---|---|
| Duo | FR |
| Mixed | FR |
| Mixed | UK |
| Solo | UK |

## H10

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.stars WHERE a.country = 'FR') ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## H11

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.review_id, rv.stars WHERE a.name = 'Cleo') ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## H12

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## H13

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS a.country, rv.review_id METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_ID | REVIEW_COUNT |
|---|---|---|
| FR | 1 | 1 |
| FR | 2 | 1 |
| UK | 3 | 1 |
| None | 4 | 1 |
| None | 5 | 1 |

## DROP SCHEMA S_LAYER.SV_P7_FIX5

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX5
```

| status |
|---|
| SV_P7_FIX5 successfully dropped. |
