### `WHERE id IN %s` is a syntax error, not zero rows

`IN` takes a parenthesised list or a subquery. Given a bare right-hand side —
`WHERE id IN %s`, which is a common slip in psycopg where the working spelling
is `= ANY(%s)` — SecantusDB compared against it and quietly matched nothing.
PostgreSQL rejects it outright.

Returning no rows is the worst available answer for this: it looks like data
rather than a mistake, so the query silently reports that nothing matched.

#### Fixed

- `x IN <expr>` without parentheses reports `42601 syntax error`, as
  PostgreSQL does. Lists, single-element lists, `NOT IN`, subqueries and
  `= ANY(…)` are unaffected.
