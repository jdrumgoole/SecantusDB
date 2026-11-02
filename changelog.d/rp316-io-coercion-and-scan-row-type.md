### Rust PG server: an I/O cast's folding and a scan's row type count their catalog reads

Two more of a new session's first catalog reads now match PostgreSQL
15.19's counts in `pg_stat_sys_indexes`. Both were traced in a PostgreSQL
backend.

#### Fixed

- A cast with no `pg_cast` entry between two typed values (a
  `CoerceViaIO`, such as `'now'::text::regproc`) now fetches the source
  type's output function and the target's input function, as the
  planner's `simplify_function` does. When a stable input function cannot
  fold over a constant, it also counts `cstring`'s type and `cstring_out`,
  which the planner looks up to price the rebuilt coercion.
- A lone table scanned beneath an aggregate with no `GROUP BY` (`select
  count(*) from pg_am`) returns its physical row, and now looks up every
  column's type for the executor's tuple descriptor.

Fixes part of RP-316 (tests/backlog/test_rp_catalog_reads.py::test_a_catalog_scans_types_and_an_io_coercions_functions); the rest of RP-316 stays open.

#### Changed

- A built-in relation's column list (`catalog_meta::system_relation_columns`)
  is now built once instead of being re-read from the dumps on every call,
  which takes about 15% off each `CREATE VIEW`. The rest of that cost is
  RP-336.
