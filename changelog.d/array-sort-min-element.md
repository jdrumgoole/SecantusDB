### Sorting on an array field no longer depends on whether an index exists

MongoDB sorts an array-valued field by one representative element: its minimum
ascending, its maximum descending. SecantusDB compared whole arrays, which placed
every array after every scalar.

That was wrong against mongod, but the sharper problem was closer to home. A
multikey index writes one entry per element, so an index scan already produced
mongod's ordering — meaning the same query returned a different order depending on
whether an index happened to exist. An index is supposed to change speed, never
results.

The in-memory sort now uses the same representative element the index path does,
and an empty array sorts between MinKey and null as mongod places it.

#### Fixed

- `sort` on an array-valued field orders by the array's minimum element ascending
  and its maximum descending, so indexed and unindexed sorts agree with each other
  and with mongod in both directions. `tests/test_array_sort_order.py`.
- The index scan no longer lets a multikey index's whole-array entry decide sort
  position. Those entries exist to answer equality against a whole array; they were
  being hit first on a backward walk and steering the descending order.

- The Rust server matches, on all four paths. Its `find` sort builds a byte key
  through the same encoder that writes index entries, so the empty-array case is
  handled locally in the sort key rather than by renumbering the persisted ranks.
