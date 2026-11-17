### Rust MongoDB server: the Atlas Search stages and commands are refused as mongod refuses them

`$search`, `$searchMeta`, `$vectorSearch` and `$listSearchIndexes`, and the
search-index commands, now answer with mongod 8.2.11's SearchNotEnabled (31082)
and its two messages, after the rest of the pipeline has parsed.

#### Fixed

- The Atlas Search stages answered 115 (CommandNotSupported) with an older
  mongod's wording, before a later stage was checked; they now answer 31082
  in mongod 8.2.11's messages, and a later unknown stage is still 40324.
  `{$search: 1}` is refused with 10065, as on mongod.
- `createSearchIndexes`, `updateSearchIndex` and `dropSearchIndex` answer
  31082 too, and `listSearchIndexes` is a command (it answered
  CommandNotFound).

Fixes RM-605 (tests/backlog/test_rm_found_rm386.py::test_an_atlas_stage_is_refused_as_mongod_refuses_it)
