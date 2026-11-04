### Rust PG server: EXPLAIN of a LEFT join whose WHERE fixes its outer key

`EXPLAIN` of a LEFT join whose WHERE sets the outer join key to a constant,
such as `select e.a from e left join k on e.b = k.b where e.b = 'v1'`, now
prints PostgreSQL 15.19's plan. The constant filters the nullable side too.
The ON stays as a join clause the equivalence class already enforces, so it
selects every pair, and the `Hash Left Join` or `Hash Right Join` is priced
as PostgreSQL prices it. Before this the nullable side's scan had no filter
and every number was zero.

#### Fixed

- A narrow-path LEFT join whose outer key a WHERE fixes follows
  `reconsider_outer_join_clauses`. The nullable side's scan gets the
  constant as a `Filter`. The kept clause's selectivity is 1, as
  `clause_selectivity` treats a redundant clause.
- On both sides, the class's equality comes after the scan's own quals
  in one `Filter`, as PostgreSQL re-derives it:
  `((a < 900) AND (b = 'v3'::text))`.
- Which side a hash join hashes follows `add_path`'s
  `compare_path_costs_fuzzily`. A total within 1% of the other path's is
  decided by the startup cost, not by the cheaper total (RP-359 compared
  totals only). Then, between fuzzily equal paths, a 1.0000000001 fuzz
  decides.
- The hashed side's bucket skew (`estimate_hash_bucket_stats`) divides by
  the relation's raw distinct count, not the one scaled to the scan's
  restriction (RP-330 scaled it first). This only changes a hashed side
  that is filtered.
- `cost_hashjoin`'s `run_cost += a * b` sums are fused multiply-adds, as
  PostgreSQL's arm64 build compiles them, which decides the last printed
  digit.
- A LEFT join reduced by a WHERE on its nullable side is still unpriced
  (RP-365). A join PostgreSQL merges still prints a priced Hash Join
  (RP-366).
  Fixes RP-364 (tests/backlog/test_rp_explain_costs.py::test_plan[a projected LEFT join whose WHERE fixes the outer key])
