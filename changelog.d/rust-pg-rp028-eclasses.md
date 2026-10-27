### The Rust PostgreSQL server: joins on several equivalence classes, and ORDER BY with LIMIT over two tables

A join whose ON clauses make more than one equivalence class now returns
its rows in the order PostgreSQL 15.19 does. For example, `a JOIN b ON a.b
= b.a JOIN c ON a.c = c.a JOIN d ON a.b = d.a JOIN e ON a.c = e.a` makes
two classes. PostgreSQL's join search merges on each class and reads a
side already merged on a class without sorting it again. The search here
modelled one class and declined the rest, so such joins ran as written.
It now keeps every class, as `equivclass.c` does.

An outer join's key now finds the class it belongs to. That can be the
class an inner join's equality made, or the one-member class an earlier
outer join's clause on the same column made. A merge on such a key reads
a side already in that order unsorted. An ORDER BY under a LIMIT is now
planned when the search holds outer joins too. An ORDER BY of a column an
outer join can NULL-extend is sorted, because no join above keeps its
order.

An ascending ORDER BY under a LIMIT over an inner or LEFT join of two
stored tables now goes to the join search instead of the narrow two-table
path. The search weighs the paths already in the ORDER BY's order, such
as a merge join on its column, against a bounded Sort of the cheapest
path, as `create_ordered_paths` does. The narrow path joined every row and
sorted the result, so ties came out in another order.

The join-order matrix was rebuilt and is now committed as
`tools/probes/pg_join_order_matrix.py`. It runs 1,665 shapes over tables of
20, 300 and 3,000 rows, and every answer is measured on PostgreSQL 15.19.
It now matches on:

- 1,140 of 1,140 shapes of three to five tables under a LIMIT (1,031
  before);
- 285 of 285 shapes of three to five tables with no LIMIT (247 before);
- 228 of 240 ORDER BY and LIMIT shapes over two tables (168 before). The
  12 left are LEFT joins of 3,000-row tables with an index on the
  nullable side's key, which PostgreSQL reads by an Index Only Scan under
  a Merge Right Join.

#### Added

- `tools/probes/pg_join_order_matrix.py`: the join-order matrix, run
  against a private PostgreSQL 15 cluster and `secantusd-pg`.

#### Fixed

- `secantus-pg` (`join_order`): `process_equivalence` keeps every class
  of three or more members, in `root->eq_classes` order. Two relations
  join on each class with members on both sides
  (`have_relevant_eclass_joinclause`), and each class has its own merge
  order (`Rel::keyed`). Its members' widths count while a member is
  outside the relation, and a base table's scans are parameterized by
  every other member of each class it is in.
- `secantus-pg` (`join_order`): a merge clause's key finds its class
  through `get_eclass_for_sort_expr` (`Search::key_ec`). An outer join's
  clause still to come keeps that class's order of use above a relation
  (`pathkeys_useful_for_merging`).
- `secantus-pg` (`join_order`): an ORDER BY under a LIMIT is planned with
  outer joins in the search. A preserved key's order is its class's, and a
  nullable key's order is its own.
- `secantus-pgplan`: the narrow two-table join path refuses an ascending
  ORDER BY (NULLS LAST) under a LIMIT over two stored user tables, inner
  or LEFT. The general planner's search plans it.

Part of RP-028
(`tests/backlog/test_rust_postgres_gaps.py::test_a_join_on_two_equivalence_classes_is_searched`,
now a regression test, with
`test_a_left_join_on_a_key_an_inner_join_shares_reads_its_order`,
`test_left_joins_on_one_preserved_key_merge_in_its_order` and
`test_an_order_by_under_a_limit_over_two_tables_reads_a_merge_join`). The
open gap test is now
`tests/backlog/test_rust_postgres_gaps.py::test_a_descending_order_by_under_a_limit_over_two_tables`.
