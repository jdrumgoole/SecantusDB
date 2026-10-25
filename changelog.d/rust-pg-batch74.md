### Rust PostgreSQL server: a B-tree page takes a tuple where PostgreSQL's does

The modelled B-tree pages split where PostgreSQL 15.19's do, through a long
run of updates.

#### Fixed

- A B-tree leaf or internal page takes a new tuple while its free space
  (`PageGetFreeSpace`, which already sets one line pointer aside) is at
  least the tuple's size, as `_bt_insertonpg` decides. It had also asked
  for a second line pointer's 4 bytes, so a page with exactly room left
  split. Deletion and deduplication before a split are tried by the same
  rule. A unique index's bottom-up pass also starts from the executor's
  "unchanged" hint, as `_bt_findinsertloc` sets `uniquedup`. In eight
  updates of every row of a 2,000-row table, the index's pages now match
  PostgreSQL 15.19's at every step (13, 14, 14, 16, 17, 18, 18, 18).
  Fixes part of RP-196 (tests/backlog/test_rp_batch71.py::test_scenario[every step of eight updates of a primary key]).
- A cancel during the planning of a very large statement is answered
  sooner. With a 200,000-item `IN` list it was up to 0.15 s late here,
  and up to 0.57 s on a CI runner, which failed RP-215's regression test.
  It is now at most 0.09 s late. The copy of the memoised parse tree and
  the scan of the text for its parameters now run on a thread of their own
  under the cancel hook. The typing of the `IN` list and each of the
  statement rewrite passes now have a cancellation point. Part of RP-202,
  which keeps the 0.12 s walks that are left.
