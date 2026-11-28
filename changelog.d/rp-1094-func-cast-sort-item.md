### pg_get_viewdef parenthesises a function cast in ORDER BY, as PostgreSQL does

PostgreSQL wraps every function call among a view's ORDER BY, PARTITION
BY, GROUP BY and DISTINCT ON items in parentheses, even outside pretty
mode. Some casts are function calls too. A cast that `pg_cast` runs through
a function, such as `name::text`, `int4::int8` or a length coercion like
`varchar(10)`, is one, so `order by c.relname::text` prints as
`ORDER BY ((c.relname)::text)`. The Rust PostgreSQL server knew only the
binary-coercible casts, so it printed such an item without the parentheses.

#### Fixed

- `secantus-pgplan`: `pg_get_viewdef` now asks which node PostgreSQL's
  `find_coercion_pathway` would build for each sort or group item that is
  a cast, using the built-in and user `pg_cast` rows. A function cast and a
  length coercion are parenthesised in both print modes. An I/O
  conversion, a binary-coercible cast and a cast to a domain are not. A
  same-type cast of a column, and a cast of a literal, now print bare in
  pretty mode too, as PostgreSQL 15.20 prints them.
  Fixes RP-1094 (tests/backlog/test_rp_found_rp782.py::test_pg_get_viewdef_parenthesises_a_function_cast_sort_item).
