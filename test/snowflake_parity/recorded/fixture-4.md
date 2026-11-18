# Snowflake's results for phase7-fixture-4.sql

## CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX4

```sql
CREATE OR REPLACE SCHEMA S_LAYER.SV_P7_FIX4
```

| status |
|---|
| Schema SV_P7_FIX4 successfully created. |

## USE SCHEMA S_LAYER.SV_P7_FIX4

```sql
USE SCHEMA S_LAYER.SV_P7_FIX4
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

## CREATE TABLE book_authors (book_id INT, author_id INT)

```sql
CREATE TABLE book_authors (book_id INT, author_id INT)
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

## INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200

```sql
INSERT INTO books VALUES (10, 'Duo', 100), (11, 'Mixed', 200), (12, 'Solo', 300), (13, 'Orphan', 50)
```

| number of rows inserted |
|---|
| 4 |

## INSERT INTO book_authors VALUES (10, 1), (10, 2), (11, 1), (

```sql
INSERT INTO book_authors VALUES (10, 1), (10, 2), (11, 1), (11, 3), (12, 3)
```

| number of rows inserted |
|---|
| 5 |

## W0

```sql
CREATE OR REPLACE SEMANTIC VIEW wq
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
    b.avg_excl_title    AS AVG(total_pages) OVER (PARTITION BY EXCLUDING b.title),
    b.running_by_title  AS SUM(total_pages) OVER (ORDER BY b.title)
  )
```

| status |
|---|
| Semantic view WQ successfully created. |

## W1

```sql
CREATE OR REPLACE SEMANTIC VIEW wu
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
    b.avg_excl_title    AS AVG(total_pages) OVER (PARTITION BY EXCLUDING title),
    b.running_by_title  AS SUM(total_pages) OVER (ORDER BY title)
  )
```

| status |
|---|
| Semantic view WU successfully created. |

## W2

```sql
SELECT * FROM SEMANTIC_VIEW(wq DIMENSIONS b.title METRICS b.total_pages, b.running_by_title) ORDER BY ALL NULLS LAST
```

| TITLE | TOTAL_PAGES | RUNNING_BY_TITLE |
|---|---|---|
| Duo | 100 | 100 |
| Mixed | 200 | 300 |
| Orphan | 50 | 350 |
| Solo | 300 | 650 |

## W3

```sql
SELECT * FROM SEMANTIC_VIEW(wq DIMENSIONS b.title METRICS b.avg_excl_title) ORDER BY ALL NULLS LAST
```

| TITLE | AVG_EXCL_TITLE |
|---|---|
| Duo | 162.500 |
| Mixed | 162.500 |
| Orphan | 162.500 |
| Solo | 162.500 |

## W4

```sql
SELECT * FROM SEMANTIC_VIEW(wq DIMENSIONS a.country, b.title METRICS b.total_pages, b.avg_excl_title) ORDER BY ALL NULLS LAST
```

| COUNTRY | TITLE | TOTAL_PAGES | AVG_EXCL_TITLE |
|---|---|---|---|
| FR | Duo | 100 | 150.000 |
| FR | Mixed | 200 | 150.000 |
| UK | Mixed | 200 | 250.000 |
| UK | Solo | 300 | 250.000 |
| None | Orphan | 50 | 50.000 |

## W5

```sql
SELECT * FROM SEMANTIC_VIEW(wq DIMENSIONS a.country, b.title METRICS b.total_pages, b.running_by_title) ORDER BY ALL NULLS LAST
```

| COUNTRY | TITLE | TOTAL_PAGES | RUNNING_BY_TITLE |
|---|---|---|---|
| FR | Duo | 100 | 100 |
| FR | Mixed | 200 | 500 |
| UK | Mixed | 200 | 500 |
| UK | Solo | 300 | 850 |
| None | Orphan | 50 | 550 |

## W6

```sql
SELECT * FROM SEMANTIC_VIEW(wq DIMENSIONS b.title METRICS b.running_by_title WHERE a.country = 'FR') ORDER BY ALL NULLS LAST
```

| TITLE | RUNNING_BY_TITLE |
|---|---|
| Duo | 100 |
| Mixed | 300 |

## W7

```sql
SELECT * FROM SEMANTIC_VIEW(wu DIMENSIONS b.title METRICS b.total_pages, b.running_by_title) ORDER BY ALL NULLS LAST
```

| TITLE | TOTAL_PAGES | RUNNING_BY_TITLE |
|---|---|---|
| Duo | 100 | 100 |
| Mixed | 200 | 300 |
| Orphan | 50 | 350 |
| Solo | 300 | 650 |

## DROP SCHEMA S_LAYER.SV_P7_FIX4

```sql
DROP SCHEMA S_LAYER.SV_P7_FIX4
```

| status |
|---|
| SV_P7_FIX4 successfully dropped. |
