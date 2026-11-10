### Rust PG server: a record filled by FETCH keeps its columns' domains

A PL/pgSQL record filled by `FETCH c INTO r` could take a value its column's
domain refuses. After `open c` over `select * from t` and `fetch c into r`,
`r.a := -5` on a column of a domain with `check (value > 0)` was stored, where
PostgreSQL 15.19 raises 23514, and `pg_typeof(r.a)` answered the domain's
base type. The cursor kept only its wire description, which names a domain
column by its base type, as RowDescription does.

In PostgreSQL a portal's row type is its query's tuple descriptor, which keeps
each column's domain. A cursor now keeps each column's domain beside its
description, whether a PL/pgSQL `OPEN` (a bound cursor, `OPEN ... FOR` a
query or `FOR EXECUTE`) or a SQL `DECLARE` made it, and a PL/pgSQL `FETCH`
types the record's fields by it. A SQL `DECLARE` fetched by name from
PL/pgSQL keeps them too.

#### Fixed

- `secantus-pg` (`lib.rs`, `portal_stream.rs`, `plpgsql_portals.rs`,
  `plpgsql_fn/open_cursor.rs`): `CursorState` keeps the domain of each
  column, and `QueryOut` carries it from a `FETCH` to `assign_row`.
- `secantus-pgplan` (`domain_scope.rs`): `select_output_domains` reads a
  `DECLARE ... CURSOR FOR`'s query.
- Fixes RP-807 (tests/backlog/test_rp_found_rp670.py::test_fetch_into_record_fields_keep_their_domains)

#### Known gaps

- A bound cursor's arguments (`c cursor (k int) for ...; open c(5)`) are not
  bound: the OPEN is 42703 (RP-809).
