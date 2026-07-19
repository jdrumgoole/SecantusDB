### `$sum` / `$avg` / `$max` / `$min` expression operators on the Rust server

The expression-operator forms of `$sum`, `$avg`, `$max`, and `$min` (added to the
Python server in the previous release) now also compute on the embedded Rust
server, so both servers support the MongoDB 5.0+ feature. The Rust
implementation reuses the group-accumulator numeric-width logic (int32 → int64 →
double promotion) and BSON cross-type ordering, so its result — value *and* type
— is byte-for-byte identical to the Python server (pinned by the parity suite).

#### Added

- `$sum` / `$avg` / `$max` / `$min` as expression operators in the Rust core.
  An array argument reduces over its elements, a scalar is a single value, and a
  missing/absent argument contributes nothing; a `Decimal128` element or an
  extreme that isn't BSON-orderable still defers to Python.
