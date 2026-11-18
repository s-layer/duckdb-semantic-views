# Snowflake's results for phase7-fixture-3.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX3

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX3
```

| status |
|---|
| Schema SV_P7_FIX3 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX3

```sql
USE SCHEMA S_LAYER.SV_P7_FIX3
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE authors (author_id INT, name VARCHAR, country V

```sql
CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR)
```

| status |
|---|
| Table AUTHORS successfully created. |

## CREATE TABLE books (book_id INT, title VARCHAR, pages INT)

```sql
CREATE TABLE books (book_id INT, title VARCHAR, pages INT)
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

## CREATE TABLE book_editors (book_id INT, author_id INT)

```sql
CREATE TABLE book_editors (book_id INT, author_id INT)
```

| status |
|---|
| Table BOOK_EDITORS successfully created. |

## INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'

```sql
INSERT INTO authors VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK'), (4, 'Dan', 'DE')
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200

```sql
INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300), (13, 'Orphan', 50)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co

```sql
INSERT INTO book_authors VALUES (10, 1, 'lead'), (10, 2, 'co'), (11, 1, 'lead'), (11, 3, 'co'), (12, 3, 'lead')
```

| number of rows inserted |
|---|
| 5 |

## INSERT INTO book_editors VALUES (12, 2)

```sql
INSERT INTO book_editors VALUES (12, 2)
```

| number of rows inserted |
|---|
| 1 |

## Q1b

```sql
CREATE OR REPLACE SEMANTIC VIEW qv_b
  TABLES (
    b  AS books        PRIMARY KEY (book_id),
    a  AS authors      PRIMARY KEY (author_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id),
    be AS book_editors PRIMARY KEY (book_id, author_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    be_author AS be (author_id) REFERENCES a,
    be_book   AS be (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title, ba.role AS role)
  METRICS (
    b.total_pages  AS SUM(pages),
    a.author_count AS COUNT(author_id),
    ba.credits     AS COUNT(*),
    be.edits       AS COUNT(*)
  )
```

| status |
|---|
| Semantic view QV_B successfully created. |

## Q1a

```sql
CREATE OR REPLACE SEMANTIC VIEW qv_ba
  TABLES (
    ba AS book_authors PRIMARY KEY (book_id, author_id),
    be AS book_editors PRIMARY KEY (book_id, author_id),
    b  AS books        PRIMARY KEY (book_id),
    a  AS authors      PRIMARY KEY (author_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    be_author AS be (author_id) REFERENCES a,
    be_book   AS be (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title, ba.role AS role)
  METRICS (
    b.total_pages  AS SUM(pages),
    a.author_count AS COUNT(author_id),
    ba.credits     AS COUNT(*),
    be.edits       AS COUNT(*)
  )
```

| status |
|---|
| Semantic view QV_BA successfully created. |

## Q2

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS a.country METRICS b.total_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## Q3

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS b.title METRICS a.author_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## Q4

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b METRICS b.total_pages)
```

| TOTAL_PAGES |
|---|
| 650 |

## Q5

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS a.country METRICS ba.credits) ORDER BY ALL NULLS LAST
```

| COUNTRY | CREDITS |
|---|---|
| FR | 3 |
| UK | 2 |

## Q6

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS a.country METRICS be.edits) ORDER BY ALL NULLS LAST
```

| COUNTRY | EDITS |
|---|---|
| FR | 1 |

## Q7

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS ba.role METRICS b.total_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## Q8

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS b.title, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## Q9

```sql
SELECT * FROM SEMANTIC_VIEW(qv_b DIMENSIONS a.country) ORDER BY ALL NULLS LAST
```

| COUNTRY |
|---|
| DE |
| FR |
| UK |

## Q10

```sql
SELECT * FROM SEMANTIC_VIEW(qv_ba DIMENSIONS a.country METRICS b.total_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Multi-path in many-to-many relationship'.
```

## Q11

```sql
SELECT * FROM SEMANTIC_VIEW(qv_ba DIMENSIONS a.country METRICS ba.credits) ORDER BY ALL NULLS LAST
```

| COUNTRY | CREDITS |
|---|---|
| FR | 3 |
| UK | 2 |

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

## D1o

```sql
CREATE OR REPLACE SEMANTIC VIEW dv_o
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
    o.revenue          AS SUM(amount),
    o.rev_by_cust      USING (o_cust)  AS SUM(amount),
    o.rev_by_store     USING (o_store) AS SUM(amount),
    c.customer_count   AS COUNT(cust_id)
  )
```

| status |
|---|
| Semantic view DV_O successfully created. |

## D1r

```sql
CREATE OR REPLACE SEMANTIC VIEW dv_r
  TABLES (
    r AS regions   PRIMARY KEY (region_id),
    c AS customers PRIMARY KEY (cust_id),
    s AS stores    PRIMARY KEY (store_id),
    o AS orders    PRIMARY KEY (order_id)
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
    o.revenue          AS SUM(amount),
    o.rev_by_cust      USING (o_cust)  AS SUM(amount),
    o.rev_by_store     USING (o_store) AS SUM(amount),
    c.customer_count   AS COUNT(cust_id)
  )
```

| status |
|---|
| Semantic view DV_R successfully created. |

## D2

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name METRICS o.revenue) ORDER BY ALL NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'R' and the base metric or dimension entity 'O' is not supported.
```

## D3

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name METRICS o.rev_by_cust) ORDER BY ALL NULLS LAST
```

| REGION_NAME | REV_BY_CUST |
|---|---|
| EU | 50 |
| US | 20 |

## D4

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name METRICS o.rev_by_store) ORDER BY ALL NULLS LAST
```

| REGION_NAME | REV_BY_STORE |
|---|---|
| EU | 40 |
| US | 30 |

## D5

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name METRICS o.rev_by_cust, o.rev_by_store) ORDER BY ALL NULLS LAST
```

| REGION_NAME | REV_BY_CUST | REV_BY_STORE |
|---|---|---|
| EU | 50 | 40 |
| US | 20 | 30 |

## D6

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS c.cust_name METRICS o.revenue) ORDER BY ALL NULLS LAST
```

| CUST_NAME | REVENUE |
|---|---|
| Ana | 50 |
| Bo | 20 |

## D7

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name METRICS c.customer_count) ORDER BY ALL NULLS LAST
```

| REGION_NAME | CUSTOMER_COUNT |
|---|---|
| EU | 1 |
| US | 1 |

## D8

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS r.region_name) ORDER BY ALL NULLS LAST
```

| REGION_NAME |
|---|
| EU |
| US |

## D9

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS c.cust_name, r.region_name) ORDER BY ALL NULLS LAST
```

| CUST_NAME | REGION_NAME |
|---|---|
| Ana | EU |
| Bo | US |

## D10

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o DIMENSIONS o.order_id, r.region_name) ORDER BY ALL NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'R' and the base metric or dimension entity 'O' is not supported.
```

## D11

```sql
SELECT * FROM SEMANTIC_VIEW(dv_o METRICS o.revenue WHERE r.region_name = 'EU')
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'R' and the base metric or dimension entity 'O' is not supported.
```

## D12

```sql
SELECT * FROM SEMANTIC_VIEW(dv_r DIMENSIONS r.region_name METRICS o.revenue) ORDER BY ALL NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'R' and the base metric or dimension entity 'O' is not supported.
```

## D13

```sql
SELECT * FROM SEMANTIC_VIEW(dv_r DIMENSIONS r.region_name METRICS o.rev_by_cust) ORDER BY ALL NULLS LAST
```

| REGION_NAME | REV_BY_CUST |
|---|---|
| EU | 50 |
| US | 20 |

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

## CREATE OR REPLACE SEMANTIC VIEW hv

```sql
CREATE OR REPLACE SEMANTIC VIEW hv
  TABLES (
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id),
    rv AS reviews      PRIMARY KEY (review_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    rv_book   AS rv (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title, rv.stars AS stars)
  METRICS (
    rv.review_count AS COUNT(review_id),
    rv.total_stars  AS SUM(stars),
    b.total_pages   AS SUM(pages)
  )
```

| status |
|---|
| Semantic view HV successfully created. |

## H1

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS a.country METRICS rv.review_count, rv.total_stars) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT | TOTAL_STARS |
|---|---|---|
| FR | 2 | 8 |
| UK | 1 | 4 |
| None | 2 | 3 |

## H2

```sql
SELECT * FROM SEMANTIC_VIEW(hv METRICS rv.review_count, rv.total_stars)
```

| REVIEW_COUNT | TOTAL_STARS |
|---|---|
| 5 | 15 |

## H3

```sql
SELECT * FROM SEMANTIC_VIEW(hv METRICS rv.review_count, rv.total_stars WHERE a.country = 'FR')
```

| REVIEW_COUNT | TOTAL_STARS |
|---|---|
| 2 | 8 |

## H4

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS a.country METRICS rv.review_count, b.total_pages) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT | TOTAL_PAGES |
|---|---|---|
| FR | 2 | 300 |
| UK | 1 | 500 |
| None | 2 | 50 |

## H5

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS b.title METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| TITLE | REVIEW_COUNT |
|---|---|
| Duo | 2 |
| Orphan | 1 |
| Solo | 1 |
| None | 1 |

## H6

```sql
SELECT * FROM SEMANTIC_VIEW(hv DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

| STARS | COUNTRY |
|---|---|
| 3 | FR |
| 4 | UK |
| 5 | FR |

## S1

```sql
CREATE TABLE owners (owner_id INT, owner_name VARCHAR, country VARCHAR)
```

| status |
|---|
| Table OWNERS successfully created. |

## CREATE TABLE positions (pos_id INT, acct VARCHAR, snap_date

```sql
CREATE TABLE positions (pos_id INT, acct VARCHAR, snap_date DATE, qty INT)
```

| status |
|---|
| Table POSITIONS successfully created. |

## CREATE TABLE position_owners (pos_id INT, owner_id INT)

```sql
CREATE TABLE position_owners (pos_id INT, owner_id INT)
```

| status |
|---|
| Table POSITION_OWNERS successfully created. |

## INSERT INTO owners VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR')

```sql
INSERT INTO owners VALUES (1, 'Ana', 'FR'), (2, 'Ben', 'FR'), (3, 'Cleo', 'UK')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO positions VALUES (1, 'A', '2026-01-01', 10), (2,

```sql
INSERT INTO positions VALUES (1, 'A', '2026-01-01', 10), (2, 'A', '2026-02-01', 20), (3, 'B', '2026-02-01', 5), (4, 'B', '2026-01-01', 7)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO position_owners VALUES (1, 1), (2, 1), (2, 2), (

```sql
INSERT INTO position_owners VALUES (1, 1), (2, 1), (2, 2), (3, 2), (4, 3)
```

| number of rows inserted |
|---|
| 5 |

## S0

```sql
CREATE OR REPLACE SEMANTIC VIEW sv_pos
  TABLES (
    ow AS owners          PRIMARY KEY (owner_id),
    p  AS positions       PRIMARY KEY (pos_id),
    po AS position_owners PRIMARY KEY (pos_id, owner_id)
  )
  RELATIONSHIPS (
    po_owner AS po (owner_id) REFERENCES ow,
    po_pos   AS po (pos_id)   REFERENCES p
  )
  DIMENSIONS (ow.country AS country, ow.owner_name AS owner_name, p.acct AS acct, p.snap_date AS snap_date)
  METRICS (
    p.total_qty AS SUM(qty),
    p.last_qty NON ADDITIVE BY (p.snap_date) AS SUM(qty)
  )
```

| status |
|---|
| Semantic view SV_POS successfully created. |

## S0b

```sql
CREATE SEMANTIC VIEW IF NOT EXISTS sv_pos
  TABLES (
    ow AS owners          PRIMARY KEY (owner_id),
    p  AS positions       PRIMARY KEY (pos_id),
    po AS position_owners PRIMARY KEY (pos_id, owner_id)
  )
  RELATIONSHIPS (
    po_owner AS po (owner_id) REFERENCES ow,
    po_pos   AS po (pos_id)   REFERENCES p
  )
  DIMENSIONS (ow.country AS country, ow.owner_name AS owner_name, p.acct AS acct, p.snap_date AS snap_date)
  METRICS (
    p.total_qty AS SUM(qty),
    p.last_qty NON ADDITIVE BY (snap_date) AS SUM(qty)
  )
```

| status |
|---|
| SV_POS already exists, statement succeeded. |

## S1

```sql
SELECT * FROM SEMANTIC_VIEW(sv_pos DIMENSIONS ow.country METRICS p.last_qty, p.total_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Semi-additive metrics are not supported with many-to-many relationships'.
```

## S2

```sql
SELECT * FROM SEMANTIC_VIEW(sv_pos METRICS p.last_qty, p.total_qty)
```

| LAST_QTY | TOTAL_QTY |
|---|---|
| 25 | 42 |

## S3

```sql
SELECT * FROM SEMANTIC_VIEW(sv_pos DIMENSIONS ow.owner_name METRICS p.last_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Semi-additive metrics are not supported with many-to-many relationships'.
```

## S4

```sql
SELECT * FROM SEMANTIC_VIEW(sv_pos METRICS p.last_qty WHERE ow.country = 'UK')
```

**Error:**

```
000002 (0A000): Unsupported feature 'Semi-additive metrics are not supported with many-to-many relationships'.
```

## S5

```sql
SELECT * FROM SEMANTIC_VIEW(sv_pos DIMENSIONS ow.country, p.acct METRICS p.last_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Semi-additive metrics are not supported with many-to-many relationships'.
```

## S6

```sql
CREATE TABLE people (person_id INT, person_name VARCHAR)
```

| status |
|---|
| Table PEOPLE successfully created. |

## CREATE TABLE accounts (acct_id VARCHAR, acct_name VARCHAR)

```sql
CREATE TABLE accounts (acct_id VARCHAR, acct_name VARCHAR)
```

| status |
|---|
| Table ACCOUNTS successfully created. |

## CREATE TABLE holders (acct_id VARCHAR, person_id INT)

```sql
CREATE TABLE holders (acct_id VARCHAR, person_id INT)
```

| status |
|---|
| Table HOLDERS successfully created. |

## CREATE TABLE balances (bal_id INT, acct_id VARCHAR, snap_dat

```sql
CREATE TABLE balances (bal_id INT, acct_id VARCHAR, snap_date DATE, balance INT)
```

| status |
|---|
| Table BALANCES successfully created. |

## INSERT INTO people VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo'

```sql
INSERT INTO people VALUES (1, 'Ana'), (2, 'Ben'), (3, 'Cleo')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO accounts VALUES ('A', 'Joint'), ('B', 'Solo')

```sql
INSERT INTO accounts VALUES ('A', 'Joint'), ('B', 'Solo')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO holders VALUES ('A', 1), ('A', 2), ('B', 2)

```sql
INSERT INTO holders VALUES ('A', 1), ('A', 2), ('B', 2)
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO balances VALUES (1, 'A', '2026-01-01', 100), (2,

```sql
INSERT INTO balances VALUES (1, 'A', '2026-01-01', 100), (2, 'A', '2026-02-01', 150), (3, 'B', '2026-01-01', 30), (4, 'B', '2026-02-01', 40)
```

| number of rows inserted |
|---|
| 4 |

## S6

```sql
CREATE OR REPLACE SEMANTIC VIEW sv_bal
  TABLES (
    pe AS people   PRIMARY KEY (person_id),
    ac AS accounts PRIMARY KEY (acct_id),
    h  AS holders  PRIMARY KEY (acct_id, person_id),
    bl AS balances PRIMARY KEY (bal_id)
  )
  RELATIONSHIPS (
    h_person AS h (person_id) REFERENCES pe,
    h_acct   AS h (acct_id)   REFERENCES ac,
    bl_acct  AS bl (acct_id)  REFERENCES ac
  )
  DIMENSIONS (pe.person_name AS person_name, ac.acct_name AS acct_name, bl.snap_date AS snap_date)
  METRICS (
    bl.total_balance AS SUM(balance),
    bl.last_balance NON ADDITIVE BY (bl.snap_date) AS SUM(balance)
  )
```

| status |
|---|
| Semantic view SV_BAL successfully created. |

## S6b

```sql
CREATE SEMANTIC VIEW IF NOT EXISTS sv_bal
  TABLES (
    pe AS people   PRIMARY KEY (person_id),
    ac AS accounts PRIMARY KEY (acct_id),
    h  AS holders  PRIMARY KEY (acct_id, person_id),
    bl AS balances PRIMARY KEY (bal_id)
  )
  RELATIONSHIPS (
    h_person AS h (person_id) REFERENCES pe,
    h_acct   AS h (acct_id)   REFERENCES ac,
    bl_acct  AS bl (acct_id)  REFERENCES ac
  )
  DIMENSIONS (pe.person_name AS person_name, ac.acct_name AS acct_name, bl.snap_date AS snap_date)
  METRICS (
    bl.total_balance AS SUM(balance),
    bl.last_balance NON ADDITIVE BY (snap_date) AS SUM(balance)
  )
```

| status |
|---|
| SV_BAL already exists, statement succeeded. |

## S7

```sql
SELECT * FROM SEMANTIC_VIEW(sv_bal DIMENSIONS pe.person_name METRICS bl.last_balance, bl.total_balance) ORDER BY ALL NULLS LAST
```

**Error:**

```
000002 (0A000): Unsupported feature 'Semi-additive metrics are not supported with many-to-many relationships'.
```

## S8

```sql
SELECT * FROM SEMANTIC_VIEW(sv_bal DIMENSIONS ac.acct_name METRICS bl.last_balance) ORDER BY ALL NULLS LAST
```

| ACCT_NAME | LAST_BALANCE |
|---|---|
| Joint | 150 |
| Solo | 40 |

## S9

```sql
SELECT * FROM SEMANTIC_VIEW(sv_bal METRICS bl.last_balance)
```

| LAST_BALANCE |
|---|
| 190 |

## N0

```sql
CREATE OR REPLACE SEMANTIC VIEW nv
  TABLES (
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title)
  METRICS (
    b.total_pages       AS SUM(pages),
    b.avg_country_pages AS AVG(total_pages) OVER (PARTITION BY EXCLUDING b.title),
    b.running_pages     AS SUM(total_pages) OVER (ORDER BY a.country),
    b.running_by_title  AS SUM(total_pages) OVER (ORDER BY b.title)
  )
```

**Error:**

```
010211 (42601): SQL compilation error:
Invalid metric definition for 'B.RUNNING_PAGES': A metric cannot refer to another dimension from an unrelated entity.
```

## N0b

```sql
CREATE SEMANTIC VIEW IF NOT EXISTS nv
  TABLES (
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id)
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country, b.title AS title)
  METRICS (
    b.total_pages       AS SUM(pages),
    b.avg_country_pages AS AVG(total_pages) OVER (PARTITION BY EXCLUDING title),
    b.running_pages     AS SUM(total_pages) OVER (ORDER BY country),
    b.running_by_title  AS SUM(total_pages) OVER (ORDER BY title)
  )
```

**Error:**

```
000904 (42000): SQL compilation error: error line 15 at position 59
invalid identifier 'COUNTRY'
```

## N1

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS a.country, b.title METRICS b.total_pages, b.avg_country_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX3.NV' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX3.NV.
```

## N2

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS a.country METRICS b.total_pages, b.running_pages) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX3.NV' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX3.NV.
```

## N3

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS a.country METRICS b.running_pages WHERE a.country IN ('FR', 'UK')) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX3.NV' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX3.NV.
```

## N4

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS b.title METRICS b.running_by_title WHERE a.country = 'FR') ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX3.NV' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX3.NV.
```

## N5

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS b.title METRICS b.running_by_title) ORDER BY ALL NULLS LAST
```

**Error:**

```
002003 (02000): SQL compilation error:
Semantic View 'S_LAYER.SV_P7_FIX3.NV' does not exist or not authorized. Your primary role DBT_S_LAYER or one of your secondary roles must have at least one privilege granted on SEMANTIC VIEW S_LAYER.SV_P7_FIX3.NV.
```

## DROP SCHEMA S_LAYER.SV_P7_FIX3

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX3
```

| status |
|---|
| SV_P7_FIX3 successfully dropped. |
