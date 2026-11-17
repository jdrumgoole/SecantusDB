### A LIMIT over a join with a joined inner side stops early

A nested loop whose inner side is itself a join, such as the bushy plan of
`e join e e3 ... join zs ... join e e4 ... limit 7 offset 3`, built a whole
batch of outer rows against every inner row before the LIMIT saw its first
one. On a thousand-row table that was millions of rows and 9 GB of server
memory, enough to end a CI runner. Joined rows now go on to the consumer
4,096 at a time, so a LIMIT stops the join as soon as it has its rows, the
way PostgreSQL's executor does.

#### Fixed

- `join_rows_core` hands its joined rows on in bounded chunks and stops when
  the consumer has enough (`tests/test_rust_pgserver_slice.py::test_a_limit_over_a_join_with_a_joined_right_side_stops_early`).
