### Rust PG server: the Sort above a HashAggregate prints its Sort Key

`EXPLAIN` of a grouped query with an ORDER BY prints the Sort's `Sort Key`
above the `HashAggregate`, as PostgreSQL 15 does.

#### Fixed

- `explain (costs off) select e.b, count(*) from e group by e.b order by
  e.b` printed a `Sort` with no `Sort Key` line. Each key is now printed as
  PostgreSQL's `show_sort_keys` prints it: the group key's column (an output
  alias printed as the column it names), its direction and NULLS placement,
  a computed key parenthesised (`(length(b))`), and qualified (`e.b`) when a
  removed join leaves its relation in the range table.
- Fixes RP-1108 (`tests/backlog/test_rp_explain_costs.py::test_plan[a sort above a hash aggregate prints its key]`,
  `[a sort above a hash aggregate prints each key's direction]`,
  `[a sort above a hash aggregate over a removed join qualifies its key]`).
  Files RP-1162 (a computed GROUP BY key prints its column name as its
  Group Key), RP-1163 (a Sort prints a key the ORDER BY repeats), RP-1164
  (JSON EXPLAIN of an aggregate prints no Strategy) and RP-1165 (a GROUP BY
  key a WHERE constant fixes is hashed, not a GroupAggregate).
