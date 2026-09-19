### `varchar` and `char(n)` columns pointed at a type that did not exist

Both spellings fold to the `text` storage tag, but a column still records the
declared oid — 1043 for `varchar`, 1042 for `bpchar`. `pg_type` is built from a
tag-keyed table, which can name only one of the three, so every `varchar` and
`char(n)` column in the catalog referenced an oid with **no `pg_type` row**. A
client joining `pg_attribute` to `pg_type` — which is what a JDBC or psycopg
metadata call does — resolved those columns to nothing.

Function AND procedure parameters had the same distinction missing one level
further in: they carried no equivalent of a column's `decl_oid`, so `CREATE
FUNCTION f(int, varchar)` recorded `proargtypes = '23 25'` where PostgreSQL 14
records `'23 1043'`, and a client reading the argument back was told it was
`text`. For a procedure the same gap reached the **wire**: an `INOUT varchar`
parameter's `CALL` result column was described as oid 25 where PostgreSQL 14
sends 1043.

Found from pgjdbc's `DatabaseMetaDataTest::functionColumns`, which asserts a
bare `varchar` parameter reports as `varchar`. All values here were measured
against PostgreSQL 14 on 2026-09-19.

#### Fixed

- `pg_type` now carries rows for `varchar` (1043) and `bpchar` (1042), so a
  declared-char column resolves to a real type row instead of dangling.
- `pg_proc.proargtypes` records the declared oid for a `varchar` / `char(n)`
  parameter, in both the named and the unnamed signature form, for functions
  and procedures alike.
- A `CALL`'s `RowDescription` reports the declared oid for an `OUT` / `INOUT`
  `varchar` / `char(n)` parameter — the wire-visible half of the same gap.
- A bare `bpchar` parameter recorded `2278` (**void**) — it has no storage tag
  at all, so it was not merely the wrong string type but an oid this catalog
  does not define. It now records 1042.
- A function's RETURN type kept the declaration too: `RETURNS varchar` had
  recorded `prorettype` 25, and `pg_get_function_result` /
  `information_schema.routines.data_type` both said `text`.
- `information_schema.columns.data_type` names the declared type for a
  `varchar` / `char(n)` column instead of reporting the `text` tag for every
  string column.
- `pg_get_function_arguments` and `information_schema.parameters.data_type`
  render `character varying` / `character` for those parameters — the SQL
  spelling PostgreSQL uses there, which is deliberately *not* the
  `pg_type.typname` (`varchar` / `bpchar`) the same type reports elsewhere.

The Rust Postgres server was checked for the same defect and does not carry it:
its `BUILTIN_TYPES` already lists `varchar` (1043) and `bpchar` (1042) with the
same array oids measured here, and it has no `pg_proc` function reflection at
all. This one is Python-server-only.
