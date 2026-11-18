# Snowflake's results for phase7-fixture-6.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX6

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX6
```

| status |
|---|
| Schema SV_P7_FIX6 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX6

```sql
USE SCHEMA S_LAYER.SV_P7_FIX6
```

| status |
|---|
| Statement executed successfully. |

## CREATE TABLE agencies (agency_id INT, agency VARCHAR)

```sql
CREATE TABLE agencies (agency_id INT, agency VARCHAR)
```

| status |
|---|
| Table AGENCIES successfully created. |

## CREATE TABLE authors (author_id INT, name VARCHAR, country V

```sql
CREATE TABLE authors (author_id INT, name VARCHAR, country VARCHAR, agency_id INT)
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

## CREATE TABLE votes (vote_id INT, review_id INT)

```sql
CREATE TABLE votes (vote_id INT, review_id INT)
```

| status |
|---|
| Table VOTES successfully created. |

## CREATE TABLE reviews_nopk (review_id INT, book_id INT, stars

```sql
CREATE TABLE reviews_nopk (review_id INT, book_id INT, stars INT)
```

| status |
|---|
| Table REVIEWS_NOPK successfully created. |

## INSERT INTO agencies VALUES (100, 'Lit FR'), (200, 'Lit UK')

```sql
INSERT INTO agencies VALUES (100, 'Lit FR'), (200, 'Lit UK')
```

| number of rows inserted |
|---|
| 2 |

## INSERT INTO authors VALUES (1, 'Ana', 'FR', 100), (2, 'Ben',

```sql
INSERT INTO authors VALUES (1, 'Ana', 'FR', 100), (2, 'Ben', 'FR', 100), (3, 'Cleo', 'UK', 200), (4, 'Dan', 'DE', NULL)
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

## INSERT INTO votes VALUES (1, 1), (2, 1), (3, 3), (4, 6), (5,

```sql
INSERT INTO votes VALUES (1, 1), (2, 1), (3, 3), (4, 6), (5, NULL)
```

| number of rows inserted |
|---|
| 5 |

## INSERT INTO reviews_nopk VALUES (1, 10, 5), (2, 10, 3), (2,

```sql
INSERT INTO reviews_nopk VALUES (1, 10, 5), (2, 10, 3), (2, 10, 3), (3, 12, 4), (4, 13, 2), (5, NULL, 1), (6, 11, 4)
```

| number of rows inserted |
|---|
| 7 |

## K0

```sql
CREATE OR REPLACE SEMANTIC VIEW kv
  TABLES (
    ag AS agencies     PRIMARY KEY (agency_id),
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id),
    rv AS reviews      PRIMARY KEY (review_id),
    v  AS votes        PRIMARY KEY (vote_id)
  )
  RELATIONSHIPS (
    a_agency  AS a (agency_id)  REFERENCES ag,
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    rv_book   AS rv (book_id)   REFERENCES b,
    v_review  AS v (review_id)  REFERENCES rv
  )
  DIMENSIONS (
    ag.agency  AS agency,
    a.country  AS country,
    a.name     AS name,
    b.title    AS title,
    ba.role    AS role,
    rv.stars   AS stars
  )
  METRICS (
    rv.review_count AS COUNT(review_id),
    rv.total_stars  AS SUM(stars),
    rv.avg_stars    AS rv.total_stars / rv.review_count,
    v.vote_count    AS COUNT(vote_id),
    b.total_pages   AS SUM(pages)
  )
```

| status |
|---|
| Semantic view KV successfully created. |

## K1

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS rv.review_count, rv.total_stars) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT | TOTAL_STARS |
|---|---|---|
| FR | 3 | 12 |
| UK | 2 | 8 |
| None | 2 | 3 |

## K2

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS ba.role METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| ROLE | REVIEW_COUNT |
|---|---|
| co | 3 |
| lead | 4 |
| None | 2 |

## K3

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS v.vote_count) ORDER BY ALL NULLS LAST
```

| COUNTRY | VOTE_COUNT |
|---|---|
| FR | 3 |
| UK | 2 |
| None | 1 |

## K4

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS ag.agency METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| AGENCY | REVIEW_COUNT |
|---|---|
| Lit FR | 3 |
| Lit UK | 2 |
| None | 2 |

## K5

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS b.title, a.country METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| TITLE | COUNTRY | REVIEW_COUNT |
|---|---|---|
| Duo | FR | 2 |
| Mixed | FR | 1 |
| Mixed | UK | 1 |
| Orphan | None | 1 |
| Solo | UK | 1 |
| None | None | 1 |

## K6

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS rv.review_count, v.vote_count) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT | VOTE_COUNT |
|---|---|---|
| FR | 3 | 3 |
| UK | 2 | 2 |
| None | 2 | 1 |

## K7

```sql
SELECT * FROM SEMANTIC_VIEW(kv METRICS rv.review_count, rv.total_stars WHERE ba.role = 'co')
```

| REVIEW_COUNT | TOTAL_STARS |
|---|---|
| 3 | 12 |

## K8

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS rv.review_count WHERE b.title <> 'Duo') ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT |
|---|---|
| FR | 1 |
| UK | 2 |
| None | 1 |

## K9

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS rv.avg_stars) ORDER BY ALL NULLS LAST
```

| COUNTRY | AVG_STARS |
|---|---|
| FR | 4.000000 |
| UK | 4.000000 |
| None | 1.500000 |

## K10

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country, rv.stars METRICS rv.review_count) ORDER BY ALL NULLS LAST
```

| COUNTRY | STARS | REVIEW_COUNT |
|---|---|---|
| FR | 3 | 1 |
| FR | 4 | 1 |
| FR | 5 | 1 |
| UK | 4 | 2 |
| None | 1 | 1 |
| None | 2 | 1 |

## K11

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS rv.stars, a.country) ORDER BY ALL NULLS LAST
```

| STARS | COUNTRY |
|---|---|
| 3 | FR |
| 4 | FR |
| 4 | UK |
| 5 | FR |

## K12

```sql
SELECT * FROM SEMANTIC_VIEW(kv DIMENSIONS a.country METRICS rv.review_count, b.total_pages) ORDER BY ALL NULLS LAST
```

| COUNTRY | REVIEW_COUNT | TOTAL_PAGES |
|---|---|---|
| FR | 3 | 300 |
| UK | 2 | 500 |
| None | 2 | 50 |

## K13

```sql
CREATE OR REPLACE SEMANTIC VIEW kv_nopk
  TABLES (
    a  AS authors      PRIMARY KEY (author_id),
    b  AS books        PRIMARY KEY (book_id),
    ba AS book_authors PRIMARY KEY (book_id, author_id),
    rv AS reviews_nopk
  )
  RELATIONSHIPS (
    ba_author AS ba (author_id) REFERENCES a,
    ba_book   AS ba (book_id)   REFERENCES b,
    rv_book   AS rv (book_id)   REFERENCES b
  )
  DIMENSIONS (a.country AS country)
  METRICS (rv.review_count AS COUNT(review_id), rv.row_count AS COUNT(*))
```

| status |
|---|
| Semantic view KV_NOPK successfully created. |

## K14

```sql
SELECT * FROM SEMANTIC_VIEW(kv_nopk DIMENSIONS a.country METRICS rv.review_count, rv.row_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
010933 (42601): SQL compilation error:
Metric entity 'RV' does not have a primary or unique key defined, which is required by certain types of queries (such as LOD expression and many-to-many relationship).
```

## K15

```sql
SELECT * FROM SEMANTIC_VIEW(kv_nopk METRICS rv.review_count, rv.row_count)
```

| REVIEW_COUNT | ROW_COUNT |
|---|---|
| 7 | 7 |

## CREATE TABLE orders (order_id INT)

```sql
CREATE TABLE orders (order_id INT)
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

## INSERT INTO orders VALUES (1), (2), (3)

```sql
INSERT INTO orders VALUES (1), (2), (3)
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

## K16

```sql
CREATE OR REPLACE SEMANTIC VIEW sv_ship
  TABLES (
    o  AS orders     PRIMARY KEY (order_id),
    li AS line_items PRIMARY KEY (item_id),
    c  AS carriers   PRIMARY KEY (carrier_id),
    sh AS shipments  PRIMARY KEY (shipment_id),
    n  AS notes      PRIMARY KEY (note_id)
  )
  RELATIONSHIPS (
    li_order   AS li (order_id)   REFERENCES o,
    sh_order   AS sh (order_id)   REFERENCES o,
    sh_carrier AS sh (carrier_id) REFERENCES c,
    n_order    AS n (order_id)    REFERENCES o
  )
  DIMENSIONS (c.carrier_name AS carrier_name, sh.mode AS mode, n.kind AS kind)
  METRICS (li.item_count AS COUNT(item_id), li.total_qty AS SUM(qty))
```

| status |
|---|
| Semantic view SV_SHIP successfully created. |

## K17

```sql
SELECT * FROM SEMANTIC_VIEW(sv_ship DIMENSIONS c.carrier_name METRICS li.item_count, li.total_qty) ORDER BY ALL NULLS LAST
```

| CARRIER_NAME | ITEM_COUNT | TOTAL_QTY |
|---|---|---|
| Fast | 3 | 8 |
| Slow | 2 | 6 |
| None | 1 | 7 |

## K18

```sql
SELECT * FROM SEMANTIC_VIEW(sv_ship DIMENSIONS sh.mode METRICS li.item_count) ORDER BY ALL NULLS LAST
```

| MODE | ITEM_COUNT |
|---|---|
| air | 3 |
| sea | 2 |
| None | 1 |

## K19

```sql
SELECT * FROM SEMANTIC_VIEW(sv_ship DIMENSIONS n.kind METRICS li.item_count) ORDER BY ALL NULLS LAST
```

| KIND | ITEM_COUNT |
|---|---|
| gift | 3 |
| rush | 2 |
| None | 1 |

## CREATE TABLE airports (code VARCHAR, city VARCHAR)

```sql
CREATE TABLE airports (code VARCHAR, city VARCHAR)
```

| status |
|---|
| Table AIRPORTS successfully created. |

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

## INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'Londo

```sql
INSERT INTO airports VALUES ('CDG', 'Paris'), ('LHR', 'London'), ('JFK', 'New York')
```

| number of rows inserted |
|---|
| 3 |

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

## R0

```sql
CREATE OR REPLACE SEMANTIC VIEW air
  TABLES (
    f  AS flights         PRIMARY KEY (flight_id),
    ap AS airports        PRIMARY KEY (code),
    d  AS airport_details PRIMARY KEY (code)
  )
  RELATIONSHIPS (
    dep_airport AS f (dep)   REFERENCES ap,
    arr_airport AS f (arr)   REFERENCES ap,
    d_airport   AS d (code)  REFERENCES ap
  )
  DIMENSIONS (ap.city AS city, d.terminal AS terminal)
  METRICS (
    f.flight_count AS COUNT(flight_id),
    f.arrivals   USING (arr_airport) AS COUNT(flight_id),
    f.departures USING (dep_airport) AS COUNT(flight_id)
  )
```

| status |
|---|
| Semantic view AIR successfully created. |

## R1

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS f.arrivals) ORDER BY ALL NULLS LAST
```

| TERMINAL | ARRIVALS |
|---|---|
| T2 | 1 |
| T4 | 1 |
| T5 | 1 |

## R2

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS f.departures) ORDER BY ALL NULLS LAST
```

| TERMINAL | DEPARTURES |
|---|---|
| T2 | 2 |
| T5 | 1 |

## R3

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS f.flight_count) ORDER BY ALL NULLS LAST
```

**Error:**

```
010246 (42601): SQL compilation error:
Invalid dimension specified: Multi-path relationship between the dimension entity 'D' and the base metric or dimension entity 'F' is not supported.
```

## R4

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal METRICS f.arrivals, f.departures) ORDER BY ALL NULLS LAST
```

| TERMINAL | ARRIVALS | DEPARTURES |
|---|---|---|
| T2 | 1 | 2 |
| T4 | 1 | None |
| T5 | 1 | 1 |

## R5

```sql
SELECT * FROM SEMANTIC_VIEW(air METRICS f.arrivals WHERE d.terminal = 'T2')
```

| ARRIVALS |
|---|
| 1 |

## R6

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS ap.city, d.terminal METRICS f.arrivals) ORDER BY ALL NULLS LAST
```

| CITY | TERMINAL | ARRIVALS |
|---|---|---|
| London | T5 | 1 |
| New York | T4 | 1 |
| Paris | T2 | 1 |

## R7

```sql
SELECT * FROM SEMANTIC_VIEW(air DIMENSIONS d.terminal) ORDER BY ALL NULLS LAST
```

| TERMINAL |
|---|
| T2 |
| T4 |
| T5 |

## DROP SCHEMA S_LAYER.SV_P7_FIX6

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX6
```

| status |
|---|
| SV_P7_FIX6 successfully dropped. |
