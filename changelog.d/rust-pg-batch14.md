### Rust PostgreSQL server: backlog batch 14

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **A join of three tables on one equivalence class returns its rows in
  PostgreSQL's order.** With `l JOIN r ON l.b = r.c JOIN m ON l.b = m.d`
  the three keys are one equivalence class, so PostgreSQL can join any
  two of the tables first, on the clause the class implies between them
  (`generate_join_implied_equalities`), and a merge join over a pair
  already merged on the class reads it unsorted, through a Material node.
  The server now weighs all three pairs, keeps each pair's cheapest path
  in key order beside its cheapest overall (`get_cheapest_path_for_pathkeys`),
  and runs the plan PostgreSQL picks, the ONs still deciding every row;
  for integer keys. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_three_tables_on_one_equivalence_class).
- **A join of four tables returns its rows in the order of PostgreSQL's
  plan.** PostgreSQL plans four tables by its join order search
  (`standard_join_search`): each level's relations built from the level
  below joined to each table a clause connects, and at four tables the
  two pairs joined too (a bushy plan, such as two merge joins whose
  outputs hash join). The server now runs that search for a chain of four
  stored tables joined INNER, each on one column equality to a table
  before it, every relation's paths in one `add_path` list, and builds
  the rows in the plan it picks. Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_four_table_chain_joins_in_the_searched_order).
- **IN, OR, IS NULL and LIMIT over an ANALYZEd table follow PostgreSQL.**
  An `IN` list sums its values' selectivities (`scalararraysel`), an `OR`
  combines its arms (`clauselist_selectivity_or`) and is priced as a
  BitmapOr where PostgreSQL could read it so, `IS [NOT] NULL` reads the
  null fraction (`nulltestsel`), and a WHERE under a LIMIT is weighed as
  PostgreSQL weighs it -- so the groups of such a query over an analysed
  table come out as 15.19 returns them. A non-unique integer index whose
  repeated keys B-tree deduplication fits onto one leaf is now sized so,
  which decides whether `select distinct g ... limit 3` reads it. Part of
  RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_or_over_an_analyzed_table_combines_its_arms).
