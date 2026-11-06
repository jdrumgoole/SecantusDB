### EXPLAIN of a join plans in time linear in its WHERE conjuncts

On the Rust PostgreSQL server, `EXPLAIN` of a two-table join whose WHERE
held many conjuncts took time quadratic in their number: `FROM a, b WHERE
a.x = b.x AND b.y = 1 AND a.x = b.x AND b.y = 3 AND ...` took 8.3 s for
1,000 conjuncts in a debug build, so one ordinary statement could hold its
connection's thread for as long as the client liked (security issue #2346).
It now takes 0.22 s, and its time doubles when the conjuncts double, up to
32,000 and for every shape measured. PostgreSQL has no limit on the number
of conjuncts, and neither does this server: the planning is linear, not
refused.

#### Fixed

- `secantus-pgplan` (`ruleutils.rs`, `comma_join_conds`): a comma join's
  conjuncts are classified in one pass and split in another, where they
  were removed one at a time, looked up per conjunct and reordered by
  position searches. It is a cancellation point, and the operator count it
  sorts the join filter by no longer drops a cancel.
- `secantus-pgplan` (`joins.rs`, `pushdown_filters`): the conjuncts the
  equivalence classes absorb and those moved into a table's scan are
  marked once, where each conjunct searched both lists.
- `secantus-pg` (`scan_estimate.rs`): a join side's scan filter is built
  once per table, where it was rebuilt for every further join equality;
  whether a conjunct is already in the scan's filter is a hash lookup
  that agrees with `Bson`'s `==`, where it was a scan of the filter's
  `$and` per conjunct; the join's equalities are matched through a set;
  and a hash join with more further clauses than it prices is refused
  before its sides are priced.

Fixes RP-388 (tests/backlog/test_rp_explain_bounds.py::test_explain_comma_join_time_is_linear_in_conjuncts); the crate test `scan_estimate::pushed_conjuncts_tests` pins the lookup to `==`.
