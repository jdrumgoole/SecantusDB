### Timestamp comparisons are microsecond-exact

A `timestamp` or `timestamptz` holding sub-millisecond precision could not be
found by a query for it. Comparisons looked only at the stored millisecond, not
the microsecond remainder kept alongside it, and the results were wrong in both
directions: a row storing `12:00:00.123456` did **not** match
`WHERE t = '12:00:00.123456'` — an equality on its own stored value — while it
**did** match `WHERE t = '12:00:00.123'`, a value it is not equal to. Range
comparisons inside the same millisecond were wrong the same way.

Reads were always precise, so a value could be inserted, selected back
correctly, and still be unfindable by a predicate on the value just returned.

Comparisons now consider the remainder: the millisecond is compared first and
the remainder only within it. Every shape — `=`, `<>`, `<`, `<=`, `>`, `>=` —
was checked against a live PostgreSQL across 42 predicate/literal combinations
with no divergence, and that comparison now runs as a test wherever a
PostgreSQL server is reachable.

`ORDER BY` within a single millisecond is still millisecond-granular; sorting
needs the remainder as a tiebreaker and is not part of this change.

#### Fixed

- `WHERE` comparisons on `timestamp` / `timestamptz` account for
  sub-millisecond precision, so a row matches an equality on its own stored
  value and no longer matches a truncated one it differs from.
