### Rust MongoDB server: a replica-set member creates mongod's step-up `config` collections

A one-member mongod 8.2.11 set creates, as it steps up,
`config.system.indexBuilds` and the query sampler's
`config.analyzeShardKeySplitPoints`, `sampledQueries` and
`sampledQueriesDiff`, each of the three with a TTL index on `expireAt`
(`expireAfterSeconds: 0`). This server created only `transactions` and
`image_collection`, so a fresh store's `config` differed from mongod's.
`listCollections` also now gives every `config` collection mongod's
`info.configDebugDump: false`, after its `uuid`.

Part of RM-366
(`tests/backlog/test_rm_found_2026_10_28.py::test_a_set_member_creates_its_step_up_collections`).
What is left of it is `config.system.preimages`, where mongod keeps change
stream pre-images; Joe decided (2026-10-28) to move this server's pre-images
there faithfully, as its own change.

#### Fixed

- A replica-set member creates `config.system.indexBuilds`,
  `analyzeShardKeySplitPoints`, `sampledQueries` and `sampledQueriesDiff`
  with mongod's options and indexes.
- `listCollections` on `config` reports `info.configDebugDump: false`.

#### Tests

- RM-219's `autoCompact` regression test sends a step that WiredTiger
  refused as busy (314, the previous command not yet taken up) again for up
  to ten seconds, on both servers alike, instead of relying on a fixed
  0.3 s pause, which lost the race once in a full parallel run.
