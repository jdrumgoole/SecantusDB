### A `$near` cursor reads on ring by ring, as mongod's does

mongod answers a `$near` by searching rings around the centre, each wider
than the last, and it searches a ring only when the cursor reaches it. A
`getMore` therefore sees the collection as it is then: a document written
into a ring the search has not reached yet comes back, and one deleted from
it does not, while the documents an earlier ring already buffered are handed
out as they were. The Rust MongoDB server replayed the same rings, but all of
them when the command ran, so every later batch was the result as it stood at
the start.

The search now keeps its state between batches -- its rings, its queue and
its ring width -- and reads each ring from the store when it gets there. A
`find` with `$near` or `$nearSphere` and an `aggregate` that opens with
`$geoNear` both read this way; the aggregate's documents go through the
cursor stage's buffer (32 first, twice as many at each refill), which gives
each one its distance as it buffers it, as mongod 8.2.11 does.

#### Fixed

- `secantus-core` `geoorder::near`: the ring search is a resumable
  `NearSearch` over a `Source` read at each ring; `near_order` (explain, the
  read made whole) is that search over a collection that does not change.
- `secantus-commands`: `explain::stream` gains a `$near` stage
  (`Stream::of_near`), which `find` streams through
  (`explain::order::near_stream`); `agg_stream` streams a leading `$geoNear`
  through the cursor stage, the stages after it run as documents arrive.
- Fixes RM-347 (tests/backlog/test_rm_found_2026_10_27b.py::test_a_near_read_goes_ring_by_ring;
  regression test `secantus-core` `geoorder::tests::a_near_search_reads_each_ring_as_the_collection_is_then`).
