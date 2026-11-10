### Rust PG server: a PL/pgSQL record filled by a query keeps its columns' domains and modifiers

A PL/pgSQL record filled by `SELECT ... INTO r` could take a value its
column's type refuses. After `select * into r from t`, `r.a := -5` on a
column of a domain with `check (value > 0)` and `r.v := 'abcdef'` on a
`varchar(3)` column were both stored, where PostgreSQL 15.19 raises 23514 and
22001 at the assignment. `pg_typeof(r.a)` also answered the domain's base
type. The record's fields were typed from the result's wire description,
which gives a domain column its base type and no length.

In PostgreSQL a record's fields are the query's columns, each of its declared
type (`exprType` / `exprTypmod`). The fields now take the column's domain, or
the base type at its modifier, for a `SELECT ... INTO`, a `FOR` over a query
and an `EXECUTE ... INTO`. An assignment to a field that is not of a domain
now follows the same assignment rule a variable does (`exec_assign_value`):
an overlong value is 22001 instead of being truncated, a `char(3)` is padded,
and a `numeric(2,1)` out of range is 22003.

#### Fixed

- `secantus-pg` (`plpgsql_fn.rs`, `lib.rs`, `plpgsql_portals.rs`): `QueryOut`
  carries each column's type modifier. `assign_row` types a record's fields
  by the column's domain or its modifier. `assign_field` gives a field that
  is not of a domain its value by the assignment rule.
- Fixes RP-688 (tests/backlog/test_rp_found_rp670.py::test_select_into_record_fields_keep_their_declared_types)

#### Known gaps

- A record filled by `FETCH` still types a domain column by its base type:
  the portal keeps only its wire description (RP-807).
