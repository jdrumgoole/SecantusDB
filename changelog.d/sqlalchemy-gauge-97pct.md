### The SQLAlchemy compliance gauge climbs from 77% to 97% — and takes a pile of SQL fixes with it

One day after the SQLAlchemy dialect-compliance gauge landed at 77.5%, a
sweep through its failure clusters brought the PostgreSQL server to **713 of
735 executed suite tests passing (97.0%), with zero errors**. As with the
gauge's first landing, the score is a by-product: each cluster traced to a
real server gap, and each fix is ordinary engine behavior any client
benefits from.

The catalog now tells the truth about more things: temp tables carry
`relpersistence 't'`, are visible only to their creating session
(`pg_table_is_visible` is session-aware), and are dropped when that
session's connection closes — real Postgres temp-table lifecycle. Declared
type modifiers survive into reflection (`varchar(52)` reports its length and
`character varying(52)` from `format_type`; numeric precision/scale
likewise), `pg_get_expr` returns stored default expressions (so a SERIAL
column reflects its `nextval` default and `autoincrement`), plain views
expose their output columns through `pg_attribute`, constraint comments
reflect through `pg_description`, `pg_get_constraintdef` quotes identifiers
the way `quote_ident` does (fixing every "bizarro character" reflection
case), and a composite primary key reflects its declared column order.

The expression engine grew `LIKE … ESCAPE` (with PG's `22025` invalid-escape
error and `ESCAPE ''` disabling escaping), computed LIKE patterns over the
extended protocol (Describe no longer fails on a WHERE that will be
evaluated per-row), `IS [NOT] DISTINCT FROM` in per-row evaluation, exact
numeric division for int-to-numeric casts (`CAST(15 AS NUMERIC) / 10` is
`1.5`, while `15 / 10` stays integer division), float⊕numeric operand
harmonization, constant expressions in `LIMIT` / `OFFSET`, `INSERT …
DEFAULT VALUES`, and `CREATE SEQUENCE … NO MINVALUE NO MAXVALUE`.

#### Added

- `sql/planner.py` + `sql/scalar.py`: `LIKE … ESCAPE` (pushdown + per-row),
  `IS [NOT] DISTINCT FROM` (per-row), constant expressions in
  LIMIT/OFFSET, `INSERT … DEFAULT VALUES`, `CREATE SEQUENCE NO
  MINVALUE / NO MAXVALUE`.
- `sql/virtual.py`: plain-view columns in `pg_attribute`; constraint
  comments in `pg_description`; `quote_ident` semantics in
  `pg_get_constraintdef`; temp tables report `relpersistence 't'` /
  `pg_temp_1`.
- `sql/session.py` + `sql/engine.py` + `sql/pgserver.py`: session-scoped
  temp-table lifecycle — visibility limited to the creating session, drop at
  connection teardown.
- `sqlalchemy_validation/requirements.py`: temp-table, constraint-index,
  and include-columns capabilities declared.

#### Fixed

- `sql/scalar.py`: `CAST(<int> AS NUMERIC)` now yields numeric, so division
  is exact instead of silently truncating; mixed float/Decimal arithmetic
  no longer raises `TypeError` (float8 wins, as in PG); `pg_get_expr`
  returned NULL for every stored default, hiding SERIAL defaults and
  `autoincrement` from reflection; `format_type` ignored type modifiers.
- `sql/engine.py`: extended-protocol Describe failed outright on a WHERE
  clause that Execute would evaluate per-row (computed LIKE patterns over
  bound parameters errored under psycopg); `CREATE SEQUENCE … NO MINVALUE`
  crashed with an internal error.
- `sql/planner.py`: a composite PK's declared column order
  (`PRIMARY KEY (name, id, attr)`) was lost in reflection.
