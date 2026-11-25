### An index's `configString` builds its WiredTiger table, as on mongod

`createIndexes` with `storageEngine: {wiredTiger: {configString: "..."}}`
now makes the index's table from that string, and so does a collection's
`indexOptionDefaults` for its `_id_` index and every index without a string
of its own. Until now a valid string was checked and kept and the table was
made from this server's defaults. `collStats` lists each table's
`creationString`, and a store `mongod` wrote with such an index is adopted
rather than refused.

#### Added

- The table's configuration is `mongod` 8.2's: the defaults, then the
  collection's `indexOptionDefaults` string, then the index's own, so each
  overrides the one before it key by key (`block_compressor`,
  `prefix_compression`, `leaf_page_max`, `allocation_size` and the rest), and
  then `key_format`, `value_format`, `app_metadata` and `log`, which no string
  changes. A string WiredTiger will not build the table from fails the build
  as `mongod` fails it: `BadValue` for `EINVAL` and `UnknownError` for
  the rest, naming WiredTiger's code, and for a parse error the text it
  gives.
- `collStats` has a `wiredTiger` section and an `indexDetails` section, each
  table's `metadata`, `creationString`, `type`, `uri` and `version`. The
  WiredTiger statistics categories `mongod` adds are not listed.
- `create` checks its `storageEngine` and `indexOptionDefaults` options as
  `mongod` does (the engine named, an embedded document, a string
  `configString`), and an empty `storageEngine` is not listed by
  `listCollections`.

#### Fixed

- `configString`s that `mongod` refuses by name are refused as it refuses
  them: any `type=lsm` (6627201), an `encryption=(...)` group (20) and a NUL
  byte (9), in that order, before WiredTiger's own check.
- Creating an index again under another name with only a TTL, `hidden`,
  `prepareUnique` or `storageEngine` different built a second index on the
  same key; it is 85 as on `mongod`. The same name with another
  `storageEngine` is 85 too, where it was 86.
- Adopting a store whose index spec carries a `storageEngine` is no longer
  refused by name.
- Fixes RM-796 (`tests/backlog/test_rm_index_config_string.py`,
  `tests/backlog/test_rm_storage_model.py::test_mongod_opens_the_config_string_indexes_secantus_wrote`,
  `test_secantus_serves_the_config_string_indexes_mongod_wrote`). Files
  RM-802: a collection's own `storageEngine` `configString` is still not
  applied to its table.
