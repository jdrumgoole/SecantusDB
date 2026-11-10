### Rust PG server: an INTERSECT or EXCEPT returns the row PostgreSQL's plan keeps for each group

Two rows of one group can look different to a client: `'xy '::bpchar` and
`'xy'::text`, or `1.0` and `1.000`. Which one a set operation returns is
part of its answer. PostgreSQL 15.19 puts an INTERSECT's input with fewer
estimated groups first (`generate_nonunion_paths`). It then hashes or sorts
as `choose_hashed_setop` prices the two, and returns each group's first row
from that order, once per output row. The server now makes the same plan.
It estimates each input's rows and groups for a constant, a VALUES list, a
user table's stored columns and a nested set operation. It prices hashing
against sorting with `cost_agg`'s spill model and `work_mem`, honours
`enable_hashagg` and the types that only sort or only hash, and sorts with
PostgreSQL's quicksort. Before, it always returned the left input's row.

#### Fixed

- `select a from t intersect select 'xy'::text` over a `char(3)` column
  returns `xy`, unpadded, as PostgreSQL does: the one-row constant is the
  smaller input.
- INTERSECT ALL and EXCEPT ALL repeat the group's first row: `select x from
  (values (1.0), (1.00)) v(x) intersect all select x from (values (1), (1))
  w(x)` is `1.0` twice, not `1.0, 1.00`.
- A sorted SetOp (`enable_hashagg` off, or a `money` or `bit` column)
  returns its groups in sorted order, each one's row the first among its
  ties.

Fixes RP-692 (tests/backlog/test_rp_found_rp672.py::test_intersect_keeps_the_smaller_inputs_value, tests/backlog/test_rp_found_rp672.py::test_a_set_operations_group_is_its_first_inputs_first_row)

Filed while working it: RP-841 (an INTERSECT over a join or another input
the server does not estimate keeps its left input first), RP-842 (a hashed
set operation's groups come out in its first input's order, not its hash
table's) and RP-843 (`INSERT ... SELECT 1.0 ... FROM` a FROM item is refused
with 42804).
