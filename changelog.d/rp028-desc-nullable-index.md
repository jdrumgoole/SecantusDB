### Rust PG server: a DESC ORDER BY of a nullable or unread key with an index keeps PostgreSQL's ties

`t0 LEFT JOIN t1 ON t0.b = t1.a ORDER BY t1.a DESC LIMIT 5` with an index on
`t1.a` is now planned by the join search instead of being joined whole and
sorted. For a key an outer join NULL-extends, or one no clause reads, the
index gives only its backward scan under a descending ORDER BY: the forward
scan's ascending pathkey is the wrong direction for merging
(`right_merge_direction`) and of no use for ordering, so
`truncate_useless_pathkeys` leaves it none and the backward scan replaces it.
A SELECT of an ORDER BY of one column under a LIMIT over two tables the search
plans is also no longer streamed, since the streamed merge join was another
plan's and left ties in another order. On the join-order matrix (private
PostgreSQL 15.19) every two-table ORDER BY + LIMIT shape now matches, ascending
and descending (240 of 240; 228 and 226 before).

#### Fixed

- The tied NULL-extended rows of that query come back as PostgreSQL 15.19's
  (1722, 1274, 1288, 1281, 1729) rather than 28, 7, 14, 21, 35.

Fixes RP-028 (part) (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_nullable_key_with_an_index_keeps_its_ties)
