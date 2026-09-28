### `INSERT ... ON CONFLICT` works on the Rust PostgreSQL server

The clause used to be parsed and then silently discarded, so
`insert ... on conflict do nothing` raised `23505` where PostgreSQL inserts
nothing and succeeds, and `do update` never upserted. A dropped clause is worse
than an unimplemented one — the client gets a confident wrong answer instead of
an honest `0A000` — and it was one of two such cases the 2026-09-28 survey found.

#### Added

- `ON CONFLICT DO NOTHING` and `ON CONFLICT ... DO UPDATE SET ... [WHERE ...]`,
  with `excluded.*` reading the proposed row, assignments that mix the existing
  row and the proposed one (`set v = t.v * 100 + excluded.v`), a column-list /
  `ON CONSTRAINT` / bare arbiter, `RETURNING` over exactly the rows affected,
  and PostgreSQL's row counts (a skipped row counts 0).
- An arbiter that matches no unique constraint is a PLAN-time error, as in
  PostgreSQL: `42P10` for a column list, `42704` for an unknown constraint name.
  The two codes differ, measured against PostgreSQL 14.13 rather than assumed.

#### Known limitation

- A partial-index arbiter (`ON CONFLICT (a) WHERE ...`) is refused `0A000`.
  Inferring it needs partial indexes, which this server does not have; widening
  it to the unconditional index would absorb a conflict the predicate excludes,
  which is the silent divergence this change exists to remove.
