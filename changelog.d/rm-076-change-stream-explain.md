### A change stream's explain shows the oplog read mongod runs

Asking the Rust MongoDB server to explain a `$changeStream` used to describe a plan over the watched collection, which is not what a change stream reads. It now answers as mongod 8.2 does: a `$cursor` over `local.oplog.rs` whose filter is the one mongod's change-stream filter builders make for the stream's scope and options, normalised and sorted the way mongod prints it, with the same plan and plan-cache hashes, followed by the internal stages the stream expands into and the user's own stages in the order mongod moves them.

The comparison covers collection, database and cluster streams, expanded, system and migration events, pre- and post-images, resumed and started-after streams and a start time, at every verbosity, in `tests/mongod_live/test_change_stream_explain.py`. Three parts are still mongod's alone and are filed: a `$match` mongod rewrites into the oplog filter, the counts of a read that starts before the end of the oplog, and the change stream's `queryShapeHash`.

#### Fixed

- `secantus-commands` (`explain/change_stream.rs`): `explain` of an aggregate that starts with `$changeStream` reports mongod's oplog read -- `namespace: local.oplog.rs`, the oplog filter as `parsedQuery`, `queryHash` / `planCacheKey`, `PROJECTION_SIMPLE` over a `COLLSCAN` from the start time's record -- then `internalUnwindTransaction`, `internalTransform`, `$_internalChangeStreamCheckInvalidate`, `$_internalChangeStreamCheckResumability`, the `$match` on the event types, the pre- and post-image stages, the user's stages and `$_internalChangeStreamEnsureResumeTokenPresent`. Fixes RM-076 (tests/backlog/test_rm_057_084.py::test_explain_of_a_change_stream_scans_the_oplog).
- `tools/probes/change_stream_commands.py`: an `explain` reply is compared without the host, the echoed command, the scan's start record and the timings.
- Filed RM-452 (a `$match` rewritten into the oplog filter), RM-453 (`executionStats` of a read that finds entries) and RM-454 (the change stream's `queryShapeHash`), each with a gap test in `tests/backlog/test_rm_found_2026_11_09_rm076.py`.
