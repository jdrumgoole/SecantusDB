### Rust PostgreSQL server: backlog batch 15

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A LEFT join chain of four tables returns its rows in the order of
  PostgreSQL's plan.** PostgreSQL's join order search may reorder outer
  joins only as their `SpecialJoinInfo`s allow (`join_is_legal`): a LEFT
  join commutes with an inner join whose clause reads only its preserved
  side, and with a LEFT join below whose nullable table its strict
  clause reads, and an inner join above makes the LEFT joins below it
  whose nullable table its clause reads inner (`reduce_outer_joins`).
  The four-table search now applies those rules, and runs a LEFT join's
  right join with a join's output on either side: a hash right join
  with its unmatched rows in its hash table's order, a merge right join
  with them in their key's place (`ExecMergeJoin`'s inner fill), which
  three-table plans use too. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_four_table_left_join_chain_joins_in_the_searched_order).
