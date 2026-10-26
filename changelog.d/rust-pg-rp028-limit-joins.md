### The Rust PostgreSQL server: a LIMIT over a join of three or more tables is planned for its first rows

`a JOIN b ON ... JOIN c ON ... LIMIT 5` now returns the rows PostgreSQL
15.19 does. Under a LIMIT, PostgreSQL plans the join for the rows the LIMIT
reads rather than for every row, which usually makes it a nested loop that
returns its first rows at once, in its outer table's order. The join order
search planned three or more tables for every row, so the LIMIT took the
first rows of a different order. Over a matrix of 843 shapes of three to
five tables (20, 300 and 3,000 rows; LIMIT 1, 5 and 50; with no index, a
primary key, plain indexes on the join keys or on other columns; with a
WHERE, an ORDER BY, an OFFSET, LEFT, FULL, bushy and shared-key joins), 812
match 15.19 against 514 before; none that matched before differs now.

#### Fixed

- `secantus-pgplan` (`joincost`): a join relation's paths as PostgreSQL
  keeps them under a LIMIT. `add_path` counts startup costs, rows and the
  outer tables a parameterized path needs, and keeps the list in total cost
  order. `add_paths_to_joinrel` reads its inputs' whole pathlists: a nested
  loop over every outer path, merge joins over presorted outer paths, and
  hash joins over the cheapest-startup and cheapest-total outer paths.
  `set_cheapest`, `create_limit_path` (`limit_choice`) and
  `create_ordered_paths` (`ordered_limit_choice`, with `cost_sort`'s
  bounded heap and external sort) pick the plan.
- `secantus-pgplan` (`joincost`): `final_cost_nestloop` for an inner side
  that is unique (`inner_unique`, scans that stop at their first match),
  `cost_material` and `cost_rescan` with their spill past `work_mem`, a hash
  join's rescan that keeps its table, Memoize (`cost_memoize_rescan`), and
  a merge join that must materialise a presorted join input that cannot
  mark and restore.
- `secantus-pgplan` (`costsize`): index and bitmap heap scans repeated once
  per outer row of a nested loop (`cost_index` and `compute_bitmap_pages`
  with a `loop_count`).
- `secantus-pg`: the join order search takes a LIMIT, over three to eight
  tables and over two whose WHERE holds a join filter. Each base table's
  paths are its scans: sequential, index and bitmap through its WHERE, the
  whole index in a join key's order, and index and bitmap scans
  parameterized by each clause's other table, a class's members included.
  A WHERE whose every conjunct the join evaluates no longer turns the LIMIT
  off, and its conjuncts over several tables are join filters of the first
  join holding their tables. An ORDER BY of one integer column, ascending,
  is weighed against paths already in its order, and when the plan returns
  its rows in that order the select runs no Sort of its own.

Declined, keeping the order the join had before: an outer join beside a
parameterized scan (PostgreSQL then parameterizes a join relation), an
index on a key or WHERE column whose deduplicated size is not modelled, an
ORDER BY of something else, and a join of two tables (planned by the
narrow join path). Part of RP-028
(`tests/backlog/test_rust_postgres_gaps.py::test_a_limit_over_three_tables_is_planned`).

RP-028 stays open. A LIMIT over LEFT joins whose inner side PostgreSQL
reads through a join parameterized by the outer table is its new gap test
(`tests/backlog/test_rust_postgres_gaps.py::test_a_limit_over_left_joins_reads_a_parameterized_join`).
