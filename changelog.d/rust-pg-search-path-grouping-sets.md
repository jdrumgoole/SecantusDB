### Rust PostgreSQL server: `public` off the `search_path` hides its tables, and grouping sets take expressions in any clause

A table in `public` is stored under its bare name, so a bare name went on
finding it after `public` was taken off the `search_path`. PostgreSQL answers
`42P01 relation "t" does not exist` there; this server read the table, and an
`INSERT`, `UPDATE` or `DELETE` wrote it. It is 42P01 now.

Under `ROLLUP`, `CUBE` and `GROUPING SETS`, a `HAVING` term or a select-list
expression computed over the keys was refused when it was more than a key
compared with a constant. Those queries run now.

#### Fixed

- A relation in `public` named without its schema is not found while `public`
  is off the `search_path`: a `SELECT`, `INSERT`, `UPDATE`, `DELETE`,
  `TRUNCATE`, `COPY`, `ALTER TABLE`, `CREATE INDEX`, `COMMENT` or `DROP` of it
  is 42P01, as PostgreSQL 15 has it. A name written with its schema
  (`public.t`) works, and a view created over such a table keeps reading it.
  Still found, and listed in the backlog: a name looked up from a string
  (`nextval('s')`, `'t'::regclass`) and `DROP ... IF EXISTS`.
- An unterminated string, quoted identifier, dollar-quoted string or comment
  reports where it opens (`select 'abc` is position 8). The error had no
  position.

#### Added

- Under `ROLLUP` / `CUBE` / `GROUPING SETS`: a `HAVING` term over a key
  (`having g || 'x' = 'ax'`, `having coalesce(g, z) = 'a'`), an expression
  over an expression key (`select upper(g) || 'x' ... group by rollup
  (upper(g))`), `GROUPING()` inside an expression (`case when grouping(g) = 1
  then 'total' else g end`), and `ORDER BY` over an expression of the keys or
  an aggregate. Each was `0A000`.
- `GROUP BY (a, b)` groups by `a` and `b`. It was a 42803 naming `a`.

#### Tests

- `tools/probes/pg_corpora/search_path_public.sql` (37 statements) and
  `grouping_set_exprs.sql` (45, was 25) against PostgreSQL 15.19. What still
  differs is listed at the end of each file and in `tasks/backlog.md`.
