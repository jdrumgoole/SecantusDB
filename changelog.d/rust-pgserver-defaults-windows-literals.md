### The Rust PostgreSQL server compares quoted literals correctly, and gains expression defaults

A quoted literal compared with a column is PostgreSQL's unknown-typed constant,
and it takes the column's type. The Rust PostgreSQL server left it a string, so
`WHERE n > '5'`, `WHERE ok = 't'` or `WHERE created > '2026-03-01'` compared a
string against a number, boolean or timestamp and matched **nothing** -- a
silent empty answer, on every quoted number, boolean, timestamp and interval
in a WHERE, an IN list or a BETWEEN. The literal is now coerced to the
column's type, and a timestamp comparison accounts for the sub-millisecond
remainder the storage keeps beside the millisecond value.

Column DEFAULTs may now be expressions -- `now()`, `CURRENT_DATE`,
`gen_random_uuid()`, `nextval('s')` -- evaluated for each inserted row (and
each existing row, for `ALTER TABLE ADD COLUMN`) rather than refused.
`INSERT ... DEFAULT VALUES`, `VALUES (DEFAULT, ...)` and `UPDATE ... SET c =
DEFAULT` work. Sequence functions work anywhere in an expression, not only as
a bare select-list target. Window functions work over an aggregate, over
`generate_series`, and inside an expression.

#### Added

- `secantus-pgplan` / `secantus-pgserver`: expression column defaults, stored in
  the Python server's `default_expr` catalog key; `DEFAULT VALUES`, the
  `DEFAULT` keyword in VALUES and SET.
- `nextval` / `currval` / `setval` / `lastval` inside any expression, through a
  sequence hook the executor installs; `lastval()`.
- `gen_random_uuid()` / `uuid_generate_v4()`, `random()`, and `CURRENT_DATE` /
  `CURRENT_TIME` / `LOCALTIME` / `LOCALTIMESTAMP` anywhere in an expression.
- A window function over an aggregate (`sum(sum(v)) OVER (...)`), over
  `generate_series`, and nested in an expression (`v - avg(v) OVER ()`).

#### Fixed

- A quoted literal compared with a non-text column matched nothing.
- A timestamp comparison ignored the stored microsecond remainder, so
  `t = '...123456'` and `t > '...123'` answered wrongly.
- A volatile SET value (`SET n = nextval('s')`) was evaluated once for every
  row instead of once per row.
- `now()::date` and the other timestamptz casts to a zone-less type failed in a
  constant expression.

#### Added (composite keys)

- A composite `PRIMARY KEY`, stored as the Python server's subdocument `_id`
  (its columns in table order), so either server reads and enforces the
  other's. A duplicate names the key's columns in its DETAIL.

#### Fixed (grouping)

- A QUALIFIED grouped column in the select list (`select c.a, count(*) from t
  c group by c.a`) was a 42803 naming the alias `c`: the target read the first
  name part rather than the column.
