### Unqualified SQL names resolve through `search_path`

The PostgreSQL front end resolved an unqualified relation name to the
`public` schema and nowhere else. `SET search_path TO reporting` followed by
`SELECT * FROM orders` raised `relation "orders" does not exist` even though
`reporting.orders` was right there — the schema was addressable only by
spelling it out on every reference. Unqualified names now walk `search_path`
in order and bind to the first schema that holds them, which is what every
tool that sets a search path and then writes plain SQL expects.

Resolution only consults the path when the bare name misses, so a relation
that already resolved is never redirected, and the rewrite happens on the
statement itself — a read and a write of the same unqualified name are
guaranteed to address the same schema. `CREATE TABLE` is deliberately exempt:
Postgres creates into the path's first schema rather than binding to a
same-named relation further along it.

Separately, a fixed wrong answer: a nested `SELECT` inside a `FROM`-less one
had its aggregates folded against the outer statement's single implicit row,
so `SELECT (SELECT count(*) FROM t)` reported `1` for any table regardless of
its contents, and the other aggregates raised `column … does not exist`. The
subquery now aggregates over its own rows.

#### Added

- `Session.search_path`, the resolution-ordered schema list (`"$user"`
  collapsed to `public`, repeats dropped). `Session.current_schema` is now
  its first entry.
- `planner.qualify_from_search_path`, which binds unqualified table
  references to a `search_path` schema in place, skipping CTE names and
  `CREATE TABLE` / `CREATE VIEW` targets.

#### Fixed

- Unqualified relation names now resolve through every `search_path` entry
  instead of only `public`.
- Aggregates in a subquery nested inside a `FROM`-less `SELECT` are no longer
  folded against the outer implicit row.
