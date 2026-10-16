### Rust MongoDB server: views, deletes, update pipelines and the TTL monitor answer as mongod does

A batch of smaller differences from mongod 8.2.11 in the Rust MongoDB
server's commands, each measured against mongod. A hint on a view now names
an index of the collection beneath it, as on mongod, so a hint that names no
index is refused and one that does sets the order. An unbounded delete
removes its documents in mongod's batches, ten at a time with each batch
last-read first, which is the order a change stream on a replica set sees.
`$$CLUSTER_TIME` works in pipelines and pipeline updates. `getParameter`
reports the TTL monitor cadence the server was started with.

A server started with the TTL sweep off now has a monitor that is switched
off, as mongod's is with `ttlMonitorEnabled=false`, and `setParameter
ttlMonitorEnabled: true` turns it on. The embedded Python `RustServer`
handle has a monitor too, off by default (`ttl_sweep_seconds=` sets a
cadence).

#### Fixed

- Views: `find`, `count` and `distinct` on a view refuse a hint that names
  no index of the base collection (2) and read through the one named;
  `$indexStats` through a view with a pipeline is 40602, and through one
  without it reports the base collection's indexes. Fixes RM-108 in part
  (tests/backlog/test_rm_085_112.py::test_hint_on_a_view_is_refused,
  ::test_index_stats_on_a_view_is_refused,
  ::test_hint_on_a_view_count_distinct_and_order).
- A clustered collection's clustered `_id` key is no longer counted by
  `createIndexes` (`numIndexesBefore: 0`), `dropIndexes` or `drop`. Fixes
  RM-100 (tests/backlog/test_rm_085_112.py::test_clustered_collection_has_no_separate_id_index).
- A pipeline update checks the `_id` it leaves before its immutability: an
  upsert's array `_id` is 54, and on a stored document an array or regex is
  53 and a `$`-prefixed field inside it 52, where all were 53 or 66. Fixes
  RM-101 (tests/backlog/test_rm_085_112.py::test_pipeline_upsert_with_array_id).
- `getParameter ttlMonitorSleepSecs` / `ttlMonitorEnabled` report the
  monitor's own settings, the started cadence included. Fixes RM-103
  (tests/backlog/test_rm_085_112.py::test_ttl_monitor_sleep_secs_reports_the_started_cadence,
  tests/test_rust_server_smoke.py::test_ttl_monitor_starts_off_and_can_be_switched_on).
- A `collMod` of `cappedSize` / `cappedMax` is recorded in the oplog and its
  `modify` event as the command named them (int64, first); it was recorded
  with neither, so a replay of it changed nothing. Replay maps them to
  `size` / `max`. Fixes RM-110
  (tests/backlog/test_rm_085_112.py::test_coll_mod_capped_size_event_names_fields_as_sent).
- An unbounded delete outside a transaction removes its matches in
  mongod's batches: ten at a time in its plan's order, each batch
  last-read first. Fixes RM-077
  (tests/backlog/test_rm_057_084.py::test_delete_order_after_a_multi_update).
- `$text` with no text index is mongod's 27 from `update` statements,
  `findAndModify`, `find` and a leading `$match` of `aggregate` (it was a
  generic 2). Fixes RM-061
  (tests/backlog/test_rm_057_084.py::test_text_in_an_update_query_without_a_text_index).
- `$$CLUSTER_TIME` is the operation's cluster time on a replica-set member
  (update, `findAndModify`, `aggregate`), and mongod's 10071200 on a
  standalone. Fixes RM-062
  (tests/backlog/test_rm_057_084.py::test_cluster_time_in_a_pipeline_update).
- An update pipeline's `$replaceWith` of a non-document names the whole
  input document in its 40228, as mongod does (an `aggregate` still names
  the fields it read). Fixes RM-063
  (tests/backlog/test_rm_057_084.py::test_replace_with_a_non_document_in_an_update).
- A `mapReduce` whose map / reduce is not the one count shape this server
  recognises is refused as a mongod with no JavaScript engine refuses it
  (31264, measured against `mongod --noscripting` 8.2.11). It answered
  `ok: 1` with no results, a silent wrong answer
  (crates/secantus-storage-adapter/tests/phase5_commands_wt.rs::map_reduce_without_javascript_is_refused).
