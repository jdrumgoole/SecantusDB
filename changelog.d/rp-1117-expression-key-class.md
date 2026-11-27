### Rust PG server: EXPLAIN joins on an expression key's equivalence class

A join whose expression key shares a member with another ON clause
(`e.a = k.a - 1 and e.a = k.a`) is now planned and priced as PostgreSQL
plans it: the members make one equivalence class, the join hashes on one
clause per class, and the side holding two members is filtered by their
equality. Before, both clauses printed as the `Hash Cond` (or a `Join
Filter` of an unpriced Nested Loop) and every cost was zero.

#### Fixed

- `explain select e.b from e join k on e.a = k.a - 1 and e.a = k.a` prints
  PostgreSQL's plan: `Hash Join (cost=20.06..37.70 rows=5 width=3)`,
  `Hash Cond: (e.a = k.a)`, and `k` read by a `Seq Scan` with `Filter: ((a
  - 1) = a)`, 5 rows by `eqsel`'s default selectivity for an equality with
  no constant side. Members join the class in the order the ON writes
  them (`process_equivalence`). The join clause is the first left and
  right members that score best, a plain column on each side first
  (`generate_join_implied_equalities_normal`), so `e.a = e2.a + 1 and e.a
  = e2.a * 2` hashes on `(e.a = (e2.a + 1))` where it was an unpriced
  Nested Loop. A side with several restrictions orders them cheapest first
  (`order_qual_clauses`), and `EXPLAIN VERBOSE` prefixes them.
- A side scanned with such a restriction is priced beside the index
  leading with its join key: that index's full and parameterized paths
  now charge the scan's expression comparisons per row fetched, and an
  index-only scan is considered only when the index holds the columns
  those comparisons read.
- Still unpriced, filed as RP-1171: the same class beside a WHERE, beside
  an ON inequality, under an aggregate, in a comma join, or restricting
  both sides.

Fixes RP-1117 (tests/backlog/test_rp_explain_costs.py::test_plan[an expression key sharing a column with a column pair joins its class])
