### Rust PostgreSQL server: a heap kept across a restart

A table's dead tuples, HOT redirects and free space map survive a restart,
and each database's tables have a heap of their own.

#### Fixed

- A heap is kept across a restart (RP-193; decided by Joe: a checkpointer
  and a reconcile).
  - **Writing.** A table's pages and free space map are written to the
    store as they change: at most once a second from the end of a message
    cycle, and at a clean shutdown, outside any user transaction, as
    PostgreSQL's checkpointer writes dirty pages.
  - **Reading back.** The first touch after a start reads them back and
    reconciles them with the rows' stored ctids: a delete the save saw
    running is settled by whether its row is still there, a reused line
    pointer is the row's again, a place whose row went after the save is
    dead.
  - **Before**, a restart derived the heap from the live rows alone: a
    dead tuple's space was free before any VACUUM, a HOT chain's dead root
    was reused, and the free space map was empty.
  - A clean restart is exact. A crash can lose the last second's dead
    tuples and free space, which PostgreSQL's WAL keeps.

  Fixes RP-193 (tests/backlog/test_rp_batch48.py::test_a_dead_tuple_outlives_a_restart).
- Two databases' tables no longer share a heap model. The model was kept
  per table oid, and each database draws its oids from a counter of its
  own, so the first table of every database shared one: its rows' ctids
  continued another database's. A dropped database's heaps are forgotten,
  so one made again under its name starts afresh. Fixes RP-233
  (tests/backlog/test_rp_batch58.py::test_two_databases_tables_have_their_own_heaps).
