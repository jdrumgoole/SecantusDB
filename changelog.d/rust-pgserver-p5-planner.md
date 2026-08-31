### The Rust PostgreSQL server learns ORDER BY, aggregates, UPDATE, DELETE and SQL's three-valued logic

The Rust PostgreSQL server now handles `ORDER BY` (with `LIMIT` and `OFFSET`),
`UPDATE`, `DELETE`, and the predicates that make SQL SQL: `IS NULL`, `IN`,
`NOT IN`, `BETWEEN` and `NOT`. All of it is checked against a real PostgreSQL
rather than against our own idea of what PostgreSQL does — a new differential
suite runs 83 identical statements against both and compares the answers.

That comparison earned its keep immediately, because SQL and MongoDB disagree
about NULL in ways that are easy to miss and produce wrong rows rather than
errors. PostgreSQL sorts NULLs last when ascending and first when descending,
while MongoDB sorts them low; pushing a sort down to the storage engine would
have quietly reordered every nullable column. `n <> 1` must not return a row
whose `n` is NULL, because comparing anything to NULL yields NULL rather than
true — but MongoDB's equivalent operator matches it. `x NOT IN (1, NULL)`
returns nothing at all, for the same reason. Each of those is now handled
explicitly, and each has a test that would have caught it.

`NOT` is pushed down into the individual comparisons rather than wrapped around
them, since MongoDB has no operator that means what SQL's `NOT` means. De
Morgan's laws hold in SQL's three-valued logic, so the transformation is exact.
Anything the server still cannot express — joins, aggregates, `LIKE`, sorting
by an expression — continues to answer PostgreSQL's "feature not supported"
rather than guessing at an answer.

Aggregates arrived with the same care. `count(*)` counts rows including those
whose columns are all NULL, while `count(col)`, `sum`, `min` and `max` skip
NULLs — and over an empty result every one of them except `count` returns NULL
rather than zero. A NULL forms its own `GROUP BY` group. `avg` is deliberately
absent: PostgreSQL returns it as `numeric` with particular scale rules, and a
close-enough answer would be worse than an honest refusal.

#### Added

- `count(*)`, `count`, `sum`, `min` and `max`, with or without `GROUP BY`,
  including PostgreSQL's result types (`count` and `sum` are `bigint`; `min`
  and `max` take the column's own type).
- `ORDER BY` with per-column direction and `NULLS FIRST` / `NULLS LAST`,
  including PostgreSQL's direction-dependent defaults; `LIMIT` and `OFFSET`.
- `UPDATE` and `DELETE`, with PostgreSQL's row counts (`UPDATE` reports rows
  matched, not rows whose value changed).
- `IS NULL`, `IS NOT NULL`, `IN`, `NOT IN`, `BETWEEN`, `NOT BETWEEN` and `NOT`.
- A differential test suite comparing 109 statements against a live PostgreSQL.

#### Fixed

- `<>` returned rows whose column was NULL, where PostgreSQL excludes them.
- Selecting the same aggregate name twice (`SELECT count(*), count(n)`) reported
  the second result in both columns.
