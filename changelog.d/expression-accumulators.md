### `$sum` / `$avg` / `$max` / `$min` as expression operators

MongoDB 5.0 made `$sum`, `$avg`, `$max`, and `$min` usable as ordinary
expression operators — over an array or a single value, anywhere an expression
is accepted (e.g. inside `$project`/`$addFields`), not only as `$group`
accumulators. SecantusDB previously rejected these as unknown expression
operators (code 168); they now compute, matching real mongod 7.0.12.

#### Added

- `$sum` / `$avg` / `$max` / `$min` as expression operators. An array argument
  reduces over its elements; a scalar is a single value; a missing/absent
  argument contributes nothing. `$sum`/`$avg` ignore non-numeric elements
  (`$sum` of an empty/all-non-numeric input is `0`, `$avg` is `null`);
  `$max`/`$min` order by BSON cross-type order and ignore `null` (empty →
  `null`).

#### Notes

- Implemented on the Python server (the pymongo conformance target). The Rust
  core defers these to Python, so the embedded Rust server does not yet compute
  them — tracked in `tasks/backlog.md`.
