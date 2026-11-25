### A rewritten function row recompiles the function, as PostgreSQL's does

A PL/pgSQL function's CONTEXT label is now cached by the function's oid and
the version of its catalog row, as `plpgsql_compile` checks a cached
compile against the `pg_proc` row's `xmin`. Another session's identical
`CREATE OR REPLACE`, an owner change or an `ALTER FUNCTION ... VOLATILE`
now relabels the function at its next call. A COMMENT, an ALTER to the
owner it already has, or a rename of a type or schema it references keeps
the label, because none of them rewrites the row.

#### Fixed

- A function replaced by an identical `CREATE OR REPLACE` kept the label of
  its old compile. The label was cached by the CREATE text.
- `CREATE FUNCTION` now assigns the transaction an xid, as PostgreSQL
  15.19's catalog write does, and an `ALTER FUNCTION ... OWNER TO` the
  current owner writes nothing.

Fixes RP-1051 (tests/backlog/test_rp_found_rp833.py::test_an_identical_replace_relabels_the_function)
