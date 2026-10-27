### The Rust PostgreSQL server: joins parameterized by an outer table, and index sizes as built

A LIMIT over LEFT joins with an index on a join key now returns the rows
PostgreSQL 15.19 does. PostgreSQL reads the nullable side of such a join
through a join relation parameterized by the outer table: for each outer
row, a hash join of the next tables over an index scan of the key equal to
the outer row's. The join order search now builds those paths, prices them
by PostgreSQL's rules and runs them per outer row. It also reads the sizes
of the indexes it prices from the server's B-tree model, and it reproduces
the order of a hash table that outgrows its buckets.

On the matrix of 843 shapes of three to five tables under a LIMIT, all 843
match (812 before). On 288 shapes of three to five tables with no LIMIT, 283
match (282 before); the five left join on two equivalence classes. A LIMIT
with an ORDER BY over two tables, which the narrow join path plans, is
unchanged (84 of 108).

#### Fixed

- `secantus-pgplan` (`joincost`): `add_paths_to_joinrel` builds a path
  that still needs outer tables where `param_source_rels` allows it (an
  outer join keeping this relation from joining that table directly). Hash
  joins are tried over every pair of the two sides'
  `cheapest_parameterized_paths`. A parameterized path's rows are its
  parameterization's (`get_joinrel_parampathinfo`), sized by the first pair
  of input paths to make it.
- `secantus-pg`: an outer join's clause parameterizes a scan of its
  nullable side only (`join_clause_is_movable_to`). A nested loop over a
  parameterized join relation reads that relation again for each outer row
  with the row bound, as `ExecReScan` does.
- `secantus-pg`: with no LIMIT, the join order search now keeps whole
  pathlists too, so a nested loop over an index scan parameterized by the
  outer row is weighed. When that search declines, the old one runs.
- `secantus-pg`: the planner prices an index by its pages and root level as
  the B-tree model holds them (`RelationGetNumberOfBlocks`, the metapage's
  level), rather than by an estimate.
- `secantus-pg`: a hash join whose table outgrows the buckets its estimate
  sized is rebuilt as `ExecHashIncreaseNumBuckets` rebuilds it. The
  dense-allocation chunks are walked newest first and each tuple is pushed
  onto its bucket's head, which decides the order of a key's matches and of
  a right or full join's unmatched rows.
- `secantus-pg` (RP-298, found here): CREATE INDEX's B-tree model limits a
  posting list to a tenth of a page, as `_bt_load` does (812 bytes, not the
  1,352 an insert's deduplication allows). It finishes a leaf counting the
  posting list its high key sheds and the heap TID it may gain. Index sizes
  now match 15.19's in 60 of 60 builds of 300 to 30,000 rows; 10,000 or
  more rows with repeated keys were 1 to 5 pages too many
  (`tests/backlog/test_rust_postgres_gaps.py::test_create_index_packs_posting_lists_as_postgres_builds_them`).

Part of RP-028
(`tests/backlog/test_rust_postgres_gaps.py::test_a_limit_over_left_joins_reads_a_parameterized_join`).
RP-299 is filed: an index created in the open transaction is not yet a
relation `regclass` finds
(`tests/backlog/test_rust_postgres_gaps.py::test_an_index_made_in_the_open_transaction_is_a_relation`).
