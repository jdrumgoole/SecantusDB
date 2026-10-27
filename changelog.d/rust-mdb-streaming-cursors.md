### Rust MongoDB server: every find cursor reads on as mongod's does

A cursor on mongod saves its place between batches and each `getMore` picks
up from there on the collection as it is now, so a document inserted ahead of
the place is returned, one deleted ahead of it is not, and one whose key moved
ahead comes back again. The Rust MongoDB server did this for collection scans
and ascending indexes; every other read handed out the result as it was when
the command ran. Now a cursor is a small executor of mongod's plan over the
store, and a later batch returns what mongod's returns over descending,
multikey and collated indexes, `$or` and `SORT_MERGE` plans, `min` / `max`,
`returnKey` and `showRecordId`, a sort of index keys (whose documents are
fetched as each batch reaches them), the `_id` index over document `_id`s,
and an aggregate under `notablescan` or with `{$meta: "recordId"}`,
`"indexKey"` or `"sortKey"`. A large result is no longer held whole.

Two on-disk changes make the walk mongod's: an index's entries are now
mongod's keys in mongod's order, and so are `_id` keys. A store written
before opens as before: its indexes are rebuilt and its `_id` keys re-keyed
the first time this version opens it. Both Rust servers share the store, so
the PostgreSQL server links the change and opens older stores the same way.

#### Changed

- Entry format 6 (`secantus-storage` `ENTRY_FORMAT`): a descending index
  field's key is the ascending key inverted and then terminated
  (`sortkey::DESC_END`), so `"ab"` sorts before `"a"` on `{s: -1}` as on
  mongod; an empty leaf array is keyed `undefined` (after MinKey, before
  null) and an element on a dotted path that lacks the leaf null in an index
  that is not sparse (`secantus_core::index_keys`), as mongod keys them. An
  index of format 4 or 5 is rebuilt when the store opens.
- `_id` key format 2 (`sortkey::ID_KEY_FORMAT`): a document `_id` is keyed by
  value, not by its raw BSON, and BinData, JavaScript with a scope and MaxKey
  in mongod's order. A store is re-keyed once when it opens
  (`Storage::upgrade_id_keys`); a store whose data tables are not logged is
  checkpointed before the format is recorded.
- The stream (`explain/stream.rs`) runs the plan's COLLSCAN, IXSCAN, FETCH,
  SKIP, OR, SORT_MERGE and SORT-of-keys stages, each yielding at the end of a
  batch. A FETCH of an entry held over a yield drops it when the document no
  longer has that key, as mongod's does.
- The planner merges a rooted `$or` whose every branch gives the sort
  (`SORT_MERGE`, mongod 8.2.11), sorts the keys of an `$or` of scans below
  the FETCH, and walks an index backward between `min` and `max` for the
  reverse sort.

#### Fixed

- Fixes RM-080 (`tests/backlog/test_rm_057_084.py::test_descending_index_get_more_sees_writes_made_since_the_find`,
  `tests/backlog/test_rm_found_2026_10_25.py::test_a_cursor_over_a_collated_index_of_strings_reads_on`).
- Fixes RM-309: a `$group` read from a DISTINCT_SCAN down a descending index
  or along a collated one returns its groups in the scan's order
  (`tests/backlog/test_rm_found_2026_10_25.py::test_a_distinct_scan_group_this_server_does_not_stream_keeps_the_scans_order`).
- Fixes RM-345: `min` / `max` naming only a prefix of the hinted index's key
  are refused (51174), and every `min` / `max` refusal is worded as mongod's
  planner words it
  (`tests/backlog/test_rm_found_2026_10_27b.py::test_min_and_max_must_name_the_whole_index_key`).
- Fixes RM-346: the planner's refusal names a `find`'s `batchSize`
  (`tests/backlog/test_rm_found_2026_10_27b.py::test_the_planners_words_name_the_batch_size`).

#### Added

- `crates/secantusdb/tests/streaming_cursors.rs` and
  `tests/mongod_live/test_streaming_cursors.py` (31 write-during-iteration
  scenarios compared with a live mongod).
- New items for what still reads whole: `$near` / `$geoNear` (RM-347), a
  time-series collection (RM-348), a plan mongod picks by trial (RM-349).
