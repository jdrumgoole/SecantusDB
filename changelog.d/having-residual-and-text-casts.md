### HAVING accepts any predicate, and a cast to text produces text

Postgres evaluates `HAVING` over the grouped rows, so anything that yields a
boolean is legal there. SecantusDB lowered what it could into the aggregation
engine's `$match` and rejected the rest outright: `HAVING NOT (count(*) > 1)`,
`HAVING count(*) BETWEEN 2 AND 5`, `HAVING count(*) * 2 > 3`, a `CASE`, a call
to `abs()` or `coalesce()` — all of them a flat "not supported". Of ten common
shapes surveyed, nine were refused. They now fall back to the same
per-grouped-row evaluation the `HAVING` subquery case already used, and all ten
return exactly what PostgreSQL 14 returns. A genuine mistake still fails
properly: a bare column that is neither grouped nor aggregated is still the
error Postgres raises, not a silently deferred predicate.

Fixing that surfaced something worse underneath. A cast to `text` was not
producing text — it passed the value through unchanged — so `count(*)::text =
'2'` compared the *number* 2 against the *string* `'2'` and quietly matched
nothing. Rendering had hidden the bug for years, because a number and its text
form go onto the wire as identical bytes; only a comparison could see the
difference, and when it did, the answer was silently wrong rather than an
error. Casts to text now convert, using Postgres' own spellings: `2.0::float8`
renders `2`, `2.50::numeric` keeps its scale, and `true::text` is `true` — not
the `t` that appears in a result row.

#### Fixed

- `HAVING` predicates outside the lowerable set (`NOT`, `BETWEEN`, arithmetic
  on aggregates, `CASE`, function calls) no longer raise `0A000`.
- A cast to `text` converts numbers, decimals and booleans to their Postgres
  text spellings, so comparing a cast result against a string literal works.
