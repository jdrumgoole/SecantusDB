### Rust PG server: a join's alias names the join

PostgreSQL lets a query name a join: `(fs join fe using (a)) as j` makes the
join one FROM entry, `j`, whose columns are those `*` lists (renamed in order
by `as j(x, y)`), and hides the names of the tables inside it. PostgreSQL 14
added a second kind: `fs join fe using (a) as u` names the merged USING column
`u.a`, beside `fs` and `fe`. This server refused the first with `0A000 an
aliased JOIN is not supported yet`. The second was `42P01 missing FROM-clause
entry for table "u"`, or, inside a subquery, a `42883` that named the
correlated rewrite's internal function. Both now resolve as on PostgreSQL
15.19.

#### Fixed

- A join's alias names the join's columns, renamed by its column list. More
  names than columns is `42P10`, and a name both sides have is `42702` through
  the alias. A table the alias hides is `42P01 invalid reference to
  FROM-clause entry`, with PostgreSQL's hint, or the enclosing query's table
  of that name when there is one.
- A USING join's alias names its merged columns, `u.*` included. `u.c` is
  `42703` with PostgreSQL's hint, and a USING alias that repeats a table's
  name is `42712`.
- The operator type check sees through either alias: `where j.b = 1` over a
  text column is `42883`, where it answered no rows.
- A nested USING join's merged column is visible in an enclosing join's ON,
  as in PostgreSQL. It was dropped from the ON's scope, so `j.a` there was
  `42703` and a bare `a` read only the other side's column, where PostgreSQL
  answers `42702`.
- A table name that an aliased join hides, reused by another FROM item, is
  refused by name (`0A000`) until RP-469 gives the two tables separate keys.
  PostgreSQL accepts it.

Fixes RP-454 (tests/backlog/test_rp_subquery_scope.py::test_join_alias).
Filed while working it: RP-467 to RP-471, each with a gap test in the same
file.
