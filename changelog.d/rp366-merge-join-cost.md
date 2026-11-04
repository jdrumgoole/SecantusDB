### Rust PG server: EXPLAIN prints the Merge Join PostgreSQL picks

`EXPLAIN` of a two-table join over analysed tables now weighs PostgreSQL
15.19's merge paths against its hash paths. Where the merge is cheaper, it
prints the `Merge Join` (or `Merge Left Join` / `Merge Right Join`) with its
`Merge Cond`, and each input under a `Sort` on its key, all priced. A
skewed key is a typical case: its hash buckets grow long. Before this it
printed a priced `Hash Join` whose numbers PostgreSQL never prints.

#### Fixed

- `mergejoinscansel`: the fraction of each input a merge skips before the
  first pair and reads up to the last. Each comes from `scalarineqsel`
  against the other input's least or greatest value
  (`get_variable_range`: the histogram's ends and the most common values).
- `initial_cost_mergejoin` and `final_cost_mergejoin`: each input
  `cost_sort`ed from its scan, the inner rescanned or read through a
  `Materialize` when that is cheaper, and the merge qual over the rows
  read. An outer join reads its preserved input whole.
- The four paths (merge, then hash, with each table outer in turn) go
  through `add_path` in `add_paths_to_joinrel`'s order, and `set_cheapest`
  picks among what survives.
- Not weighed: a merge reading an index-presorted input, or a side an
  index scan could serve. Those still print zeros (RP-368).
  Fixes RP-366 (tests/backlog/test_rp_explain_costs.py::test_plan[a projected join PostgreSQL merges],
  with `[a LEFT join PostgreSQL merges]`)
