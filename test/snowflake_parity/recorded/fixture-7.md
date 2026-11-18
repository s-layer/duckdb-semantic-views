# Snowflake's results for phase7-fixture-7.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX7

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX7
```

| status |
|---|
| Schema SV_P7_FIX7 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX7

```sql
USE SCHEMA S_LAYER.SV_P7_FIX7
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

## CREATE TABLE reviews (review_id INT, book_id INT, stars INT)

```sql
CREATE TABLE reviews (review_id INT, book_id INT, stars INT)
```

| status |
|---|
| Table REVIEWS successfully created. |

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

## INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4

```sql
INSERT INTO reviews VALUES (1, 10, 5), (2, 10, 3), (3, 12, 4), (4, 13, 2), (5, NULL, 1), (6, 11, 4)
```

| number of rows inserted |
|---|
| 6 |

## D1

```sql
CREATE OR REPLACE SEMANTIC VIEW d1
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, a.name AS name, b.title AS title, rv.review_id AS review_id, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id))
```

| status |
|---|
| Semantic view D1 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d1 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d1 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D2

```sql
CREATE OR REPLACE SEMANTIC VIEW d2
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, a.name AS name, b.title AS title, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id))
```

| status |
|---|
| Semantic view D2 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d2 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d2 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D3

```sql
CREATE OR REPLACE SEMANTIC VIEW d3
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, b.title AS title, rv.review_id AS review_id, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id))
```

| status |
|---|
| Semantic view D3 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d3 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d3 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D4

```sql
CREATE OR REPLACE SEMANTIC VIEW d4
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, a.name AS name, b.title AS title, rv.review_id AS review_id, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id), b.total_pages AS SUM(pages))
```

| status |
|---|
| Semantic view D4 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d4 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d4 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

| STARS | COUNTRY |
|---|---|
| 3 | FR |
| 4 | FR |
| 4 | UK |
| 5 | FR |

## D5

```sql
CREATE OR REPLACE SEMANTIC VIEW d5
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, a.name AS name, b.title AS title, rv.review_id AS review_id, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id), rv.total_stars AS SUM(stars))
```

| status |
|---|
| Semantic view D5 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d5 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d5 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D6

```sql
CREATE OR REPLACE SEMANTIC VIEW d6
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, b.title AS title, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id), rv.total_stars AS SUM(stars), b.total_pages AS SUM(pages))
```

| status |
|---|
| Semantic view D6 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d6 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d6 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

| STARS | COUNTRY |
|---|---|
| 3 | FR |
| 4 | FR |
| 4 | UK |
| 5 | FR |

## D7

```sql
CREATE OR REPLACE SEMANTIC VIEW d7
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, b.title AS title, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id), rv.total_stars AS SUM(stars))
```

| status |
|---|
| Semantic view D7 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d7 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d7 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D8

```sql
CREATE OR REPLACE SEMANTIC VIEW d8
  TABLES (a AS authors PRIMARY KEY (author_id), b AS books PRIMARY KEY (book_id), ba AS book_authors PRIMARY KEY (book_id, author_id), rv AS reviews PRIMARY KEY (review_id))
  RELATIONSHIPS (ba_author AS ba (author_id) REFERENCES a, ba_book AS ba (book_id) REFERENCES b, rv_book AS rv (book_id) REFERENCES b)
  DIMENSIONS (a.country AS country, rv.stars AS stars)
  METRICS (rv.review_count AS COUNT(review_id))
```

| status |
|---|
| Semantic view D8 successfully created. |

## SELECT * FROM SEMANTIC_VIEW(d8 DIMENSIONS rv.stars, a.countr

```sql
SELECT * FROM SEMANTIC_VIEW(d8 DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D9

```sql
SELECT * FROM SEMANTIC_VIEW(d1 DIMENSIONS rv.review_id, a.name) ORDER BY ALL NULLS LAST
```

**Error:**

```
010959 (42601): SQL compilation error:
The entities 'RV' and 'A' are not related. Consider selecting calculations from entities that are related and have appropriate levels of granularity.
```

## D10

```sql
SELECT * FROM SEMANTIC_VIEW(d6 DIMENSIONS rv.stars WHERE a.country = 'FR') ORDER BY ALL NULLS LAST
```

| STARS |
|---|
| 3 |
| 4 |
| 5 |

## CREATE TABLE orders (order_id INT, region VARCHAR)

```sql
CREATE TABLE orders (order_id INT, region VARCHAR)
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

## CREATE TABLE carriers (carrier_id INT, carrier_name VARCHAR)

```sql
CREATE TABLE carriers (carrier_id INT, carrier_name VARCHAR)
```

| status |
|---|
| Table CARRIERS successfully created. |

## CREATE TABLE shipments (shipment_id INT, order_id INT, carri

```sql
CREATE TABLE shipments (shipment_id INT, order_id INT, carrier_id INT, mode VARCHAR)
```

| status |
|---|
| Table SHIPMENTS successfully created. |

## CREATE TABLE notes (note_id INT, order_id INT, kind VARCHAR)

```sql
CREATE TABLE notes (note_id INT, order_id INT, kind VARCHAR)
```

| status |
|---|
| Table NOTES successfully created. |

## CREATE TABLE note_tags (tag_id INT, note_id INT, tag VARCHAR

```sql
CREATE TABLE note_tags (tag_id INT, note_id INT, tag VARCHAR)
```

| status |
|---|
| Table NOTE_TAGS successfully created. |

## CREATE TABLE line_items_nopk (item_id INT, order_id INT, qty

```sql
CREATE TABLE line_items_nopk (item_id INT, order_id INT, qty INT)
```

| status |
|---|
| Table LINE_ITEMS_NOPK successfully created. |

## INSERT INTO orders VALUES (1, 'EU'), (2, 'EU'), (3, 'US')

```sql
INSERT INTO orders VALUES (1, 'EU'), (2, 'EU'), (3, 'US')
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

## INSERT INTO line_items_nopk VALUES (1, 1, 5), (2, 1, 1), (3,

```sql
INSERT INTO line_items_nopk VALUES (1, 1, 5), (2, 1, 1), (3, 2, 2), (4, 3, 7)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO carriers VALUES (10, 'Fast'), (20, 'Slow')

```sql
INSERT INTO carriers VALUES (10, 'Fast'), (20, 'Slow')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO shipments VALUES (1, 1, 10, 'air'), (2, 1, 20, '

```sql
INSERT INTO shipments VALUES (1, 1, 10, 'air'), (2, 1, 20, 'sea'), (3, 2, 10, 'air')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO notes VALUES (1, 1, 'gift'), (2, 1, 'rush'), (3,

```sql
INSERT INTO notes VALUES (1, 1, 'gift'), (2, 1, 'rush'), (3, 2, 'gift')
```

| number of rows inserted |
|---|
| 3 |

## INSERT INTO note_tags VALUES (1, 1, 'red'), (2, 1, 'blue'),

```sql
INSERT INTO note_tags VALUES (1, 1, 'red'), (2, 1, 'blue'), (3, 2, 'red')
```

| number of rows inserted |
|---|
| 3 |

## N0

```sql
CREATE OR REPLACE SEMANTIC VIEW nv
  TABLES (o AS orders PRIMARY KEY (order_id), li AS line_items PRIMARY KEY (item_id), c AS carriers PRIMARY KEY (carrier_id), sh AS shipments PRIMARY KEY (shipment_id), n AS notes PRIMARY KEY (note_id), t AS note_tags PRIMARY KEY (tag_id))
  RELATIONSHIPS (li_order AS li (order_id) REFERENCES o, sh_order AS sh (order_id) REFERENCES o, sh_carrier AS sh (carrier_id) REFERENCES c, n_order AS n (order_id) REFERENCES o, t_note AS t (note_id) REFERENCES n)
  DIMENSIONS (o.region AS region, li.qty AS qty, c.carrier_name AS carrier_name, sh.mode AS mode, n.kind AS kind, t.tag AS tag)
  METRICS (o.order_count AS COUNT(order_id), li.item_count AS COUNT(item_id), li.total_qty AS SUM(qty), n.note_count AS COUNT(note_id))
```

| status |
|---|
| Semantic view NV successfully created. |

## N1

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS n.kind METRICS o.order_count) ORDER BY ALL NULLS LAST
```

| KIND | ORDER_COUNT |
|---|---|
| gift | 2 |
| rush | 1 |
| None | 1 |

## N2

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS n.kind, sh.mode METRICS li.item_count) ORDER BY ALL NULLS LAST
```

| KIND | MODE | ITEM_COUNT |
|---|---|---|
| gift | air | 3 |
| gift | sea | 2 |
| rush | air | 2 |
| rush | sea | 2 |
| None | None | 1 |

## N3

```sql
SELECT * FROM SEMANTIC_VIEW(nv METRICS li.item_count, li.total_qty WHERE n.kind = 'gift')
```

| ITEM_COUNT | TOTAL_QTY |
|---|---|
| 3 | 8 |

## N4

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS t.tag METRICS li.item_count) ORDER BY ALL NULLS LAST
```

| TAG | ITEM_COUNT |
|---|---|
| blue | 2 |
| red | 2 |
| None | 2 |

## N5

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS li.qty METRICS n.note_count) ORDER BY ALL NULLS LAST
```

| QTY | NOTE_COUNT |
|---|---|
| 1 | 2 |
| 2 | 1 |
| 5 | 2 |

## N6

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS n.kind METRICS li.item_count, o.order_count) ORDER BY ALL NULLS LAST
```

| KIND | ITEM_COUNT | ORDER_COUNT |
|---|---|---|
| gift | 3 | 2 |
| rush | 2 | 1 |
| None | 1 | 1 |

## N7

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS o.region, n.kind METRICS li.item_count) ORDER BY ALL NULLS LAST
```

| REGION | KIND | ITEM_COUNT |
|---|---|---|
| EU | gift | 3 |
| EU | rush | 2 |
| US | None | 1 |

## N8

```sql
SELECT * FROM SEMANTIC_VIEW(nv DIMENSIONS li.qty, n.kind) ORDER BY ALL NULLS LAST
```

| QTY | KIND |
|---|---|
| 1 | gift |
| 1 | rush |
| 2 | gift |
| 5 | gift |
| 5 | rush |

## N9

```sql
CREATE OR REPLACE SEMANTIC VIEW nv_nopk
  TABLES (o AS orders PRIMARY KEY (order_id), li AS line_items_nopk, n AS notes PRIMARY KEY (note_id))
  RELATIONSHIPS (li_order AS li (order_id) REFERENCES o, n_order AS n (order_id) REFERENCES o)
  DIMENSIONS (n.kind AS kind)
  METRICS (li.total_qty AS SUM(qty))
```

| status |
|---|
| Semantic view NV_NOPK successfully created. |

## SELECT * FROM SEMANTIC_VIEW(nv_nopk DIMENSIONS n.kind METRIC

```sql
SELECT * FROM SEMANTIC_VIEW(nv_nopk DIMENSIONS n.kind METRICS li.total_qty) ORDER BY ALL NULLS LAST
```

**Error:**

```
010933 (42601): SQL compilation error:
Metric entity 'LI' does not have a primary or unique key defined, which is required by certain types of queries (such as LOD expression and many-to-many relationship).
```

## DROP SCHEMA S_LAYER.SV_P7_FIX7

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX7
```

| status |
|---|
| SV_P7_FIX7 successfully dropped. |
