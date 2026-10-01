### Rust PostgreSQL server: correlated subqueries over equalities run once

A correlated subquery used to be planned and run again for every distinct outer value. When every outer reference is an equality (`WHERE t.x = o.a AND t.y = o.b`), the inner query now runs once per statement and each outer row is answered by a hash lookup, the semi-join PostgreSQL itself plans.

#### Changed

- Timings at 2,000 × 2,000 rows (debug build):
  - `EXISTS` over two equalities: 7.3 s → 36 ms.
  - `NOT EXISTS` over two equalities: 7.4 s → 47 ms.
  - A correlated `count(*)`: 1.1 s → 227 ms.
  - A correlated `LIMIT 1` subquery: 7.6 s → 36 ms.
- Answers are unchanged. A shape the lookup cannot reproduce exactly falls back to the per-row path. That includes a non-equality correlation, a numeric key, a nondeterministic collation, and an inner `ORDER BY`, `DISTINCT` or function.
- New corpus `correlated_hash` (23 edge cases) matches PostgreSQL. Every corpus still matches.
