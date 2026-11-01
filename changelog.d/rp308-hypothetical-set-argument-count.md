### Rust PG server: a WITHIN GROUP call with the wrong argument count is named as PostgreSQL names it

`rank(2) WITHIN GROUP (ORDER BY a, b)`, and any ordered-set or
hypothetical-set call whose direct arguments do not fit its ordering
columns, was refused as `function rank(unknown) does not exist` with the
generic hint. It now names every argument's type, the direct ones and then
the ordering columns' (`function rank(integer, integer, text) does not
exist`), and a hypothetical-set aggregate carries PostgreSQL 15.19's hint
giving both counts.

#### Fixed

- The 42883 message of a mismatched WITHIN GROUP call names the argument
  types, and `rank` / `dense_rank` / `percent_rank` / `cume_dist` give the
  "number of hypothetical direct arguments" hint.
  Fixes RP-308 (tests/backlog/test_rp_top10_found.py::test_a_hypothetical_set_aggregate_with_too_few_direct_arguments);
  regression test tests/test_rust_pgserver_slice.py::test_within_group_argument_count_mismatch.
