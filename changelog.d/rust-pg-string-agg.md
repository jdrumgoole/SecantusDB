### The Rust PostgreSQL server supports string_agg, and ORDER BY inside an aggregate

`string_agg(s, ',')` was refused because it takes TWO arguments and the
aggregate planner rejected anything but one. It now joins the non-NULL values
in group order, answers NULL over an empty input, treats a NULL separator as
joining with nothing between the values, and — under `DISTINCT` — dedups and
sorts, which is how PostgreSQL implements a DISTINCT aggregate. A non-text
argument is reported as the missing FUNCTION PostgreSQL calls it (42883),
not as an unsupported feature.

`ORDER BY` written inside an aggregate now works too: `array_agg(s ORDER BY id
DESC)` and `string_agg(s, ',' ORDER BY s)` sort the group's rows before the
values are collected, which is the only thing that gives either aggregate a
defined order. An ORDER BY over an expression rather than a column is still
refused, rather than quietly answered in a different order.

#### Added

- `crates/secantus-pgplan`: `AggFunc::StringAgg` with its separator, and
  `plan_aggregate_order` for the in-call ORDER BY.
- `crates/secantus-pgserver`: both, over the group's rows.

#### Testing

- `tests/test_rust_pgserver_slice.py`: grouped and ungrouped, empty input, a
  NULL separator, DISTINCT, FILTER, the 42883 for a non-text argument, and
  the ordering cases — all measured against PostgreSQL 14.24.
