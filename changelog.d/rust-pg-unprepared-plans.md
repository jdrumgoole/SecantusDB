### Rust PostgreSQL server: an unprepared statement reuses a plan

A statement sent with its values written into the text (`select v from t
where k = 5`, then `... k = 6`) was a new text each time, so the server
parsed and planned every one from the start. It now reads such a statement
as the same statement with the values taken out, and after the second of a
shape it reuses the plan with the new values put in, as it already did for
a prepared statement. On the development machine a primary-key read with a
different literal each time went from 118 to 43 microseconds, against 42 for
PostgreSQL 15 on the same machine and 42 for the same text repeated.

#### Changed

- A plan is kept for the shape of an unprepared `SELECT`, `INSERT`, `UPDATE`
  or `DELETE` whose integer and plain string literals stand where a
  prepared statement's parameters may: compared with a column in the
  `WHERE`, a value of an `INSERT`, the right-hand side of a `SET`. It is
  learned the way a prepared statement's is, by planning the text twice
  with stand-in literals and keeping the plan only if it follows them, so a
  literal the planner reads (a `LIMIT`, a cast, arithmetic, a value for a
  `varchar(n)` or a `date` column) is planned every time as before.
- A plan template is learned the second time a statement is planned, not
  the first. Learning plans the statement twice more, which a statement
  sent once never earned back.
- A single command is no longer parsed once to split it from its
  neighbours and again to plan it. Its syntax error is unchanged: the
  position counts from the start of what the client sent.

#### Fixed

- A string literal compared with a `bytea` column (`x = 'abc'`,
  `x = '\x616263'`) matched no row. It is read as `bytea` now, in either
  input format.
- Under `GROUPING SETS`, `ROLLUP` or `CUBE`, a select-list expression with
  no aggregate in it (a constant, `g || 'x'`) was refused. So was an
  aggregate over such a query in `FROM`
  (`select count(*) from (select ... group by cube (g, z)) q`), because the
  outer query needs none of the inner columns. Both work, and a column that
  is not a grouping key is PostgreSQL's 42803.

#### Still open

- Under grouping sets, a `HAVING` term over a key (`having g || 'x' =
  'ax'`) and an expression over an expression key (`rollup (upper(g))`) are
  still refused.
- A decimal or exponent literal, and a negative number written against its
  operator (`k =-5`), are not taken out of the text; such a statement is
  planned every time.
