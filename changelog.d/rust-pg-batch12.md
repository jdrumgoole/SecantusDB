### Rust PostgreSQL server: backlog batch 12

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A join under a WHERE returns its rows in PostgreSQL's order.** A
  WHERE conjunct over one side of a join is a qual of that side's scan,
  so a merge join sorts, and a hash join hashes, only the rows the scan
  keeps, and their ties come out in the order of that filtered input. The
  server now filters each side first, then puts it in the plan's order,
  and prices the plan with each side's own WHERE: a filtered side is
  often the cheaper outer, so a join with the right side outer (a hash or
  merge join, or a nested loop) runs with the sides swapped. The choice
  follows `add_path`, startup costs included, and under a LIMIT the plan
  cheapest for the rows it reads (`get_cheapest_fractional_path`), which
  is often a nested loop over a materialised inner. A conjunct naming an
  unqualified column is pushed to the one table that has it, and a WHERE
  that no NULL-extended row can pass makes a LEFT join an inner one
  (`reduce_outer_joins`). Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_merge_join_under_a_where_sorts_the_rows_it_keeps).
- **A join on an expression returns its rows in PostgreSQL's order.**
  `ON a.x = b.y + 3` is a merge or hash join keyed by each side's value
  of its expression, which PostgreSQL sorts or hashes by. The server now
  evaluates each side's key once per row, matches rows through a hash on
  it rather than testing every pair, and orders both sides as the plan
  reads them; the plan is priced as PostgreSQL prices an expression with
  no statistics, its operators added to the join clause's cost
  (`cost_qual_eval`). A LIMIT straight over such a join, or any general
  join, picks the plan cheapest for the rows it reads. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_merge_join_on_an_expression_sorts_by_its_value).
