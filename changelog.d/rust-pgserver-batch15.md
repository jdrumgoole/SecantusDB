### The Rust PostgreSQL server: expression indexes that work, numeric and NOT IN at index speed, error positions

Batch 15 makes several common shapes fast instead of quadratic or
full-scanning. It also corrects error positions and static type checks.
Each change was probed against PostgreSQL 15, and the corpora run against
PostgreSQL 14.

#### Fixed

- **A UNIQUE expression index (`lower(email)`) cost a full table evaluation
  per write.** An INSERT took 1.6 s at 20,000 rows; it now takes 0.9 ms at
  any size.
  - Each row keeps the expression's value in a hidden field, and the storage
    index on that field enforces UNIQUE.
  - Rows another writer left (the Python server knows no expression indexes)
    are recomputed when the server opens the store.
- An error raised while rows stream, or for a record without the named
  field, now carries PostgreSQL's position.
- `integer + boolean`, string arithmetic and `jsonb` arithmetic on a
  composite field are rejected at plan time (`42883`), as on PostgreSQL,
  even over an empty table.

#### Added

- `ON CONFLICT (expression)` arbitrates on a unique expression index.

#### Performance (debug builds)

- A WHERE over an indexed expression uses the index: `lower(t) = 'x'` at
  20,000 rows went from 2.4 s to 3.7 ms.
- A `numeric` predicate uses the column's index: `n = 5` went from 820 ms to
  1.2 ms, and `BETWEEN` from 1.9 s to 17 ms.
  - Storage answers an `$or` whose every branch has an index as the union of
    the branches' lookups (mongod's OR plan).
  - Each indexable `$or` in an AND is intersected with the others.
- `NOT IN (subquery)` and `NOT EXISTS` went from 7.3 s to 34 ms over
  2,000 x 2,000 rows.
  - `NOT (x = ANY (...))` now lowers to an index-aware filter.
  - A scan hashes a large `$in` / `$nin` list once instead of comparing every
    row against every element.

#### The MongoDB server

The storage and matcher changes above apply to the Rust MongoDB server too:

- an indexed `$or` uses its indexes;
- a long `$in` / `$nin` list is hashed;
- `explain` reports the OR plan as mongod 8.2.11 does: `SUBPLAN`, `FETCH`,
  `OR`, and an `IXSCAN` per branch.

New corpora: `not_in_large`, `numeric_index`, `error_positions`,
`expr_unique`, `expr_where`. All are at 0 divergences.
