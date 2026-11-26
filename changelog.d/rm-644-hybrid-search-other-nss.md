### `$queryStats` names the collection a hybrid search unions

A `$rankFusion` or `$scoreFusion` reads its own collection again for every
input pipeline after the first; its query stats key now lists that
collection under `otherNss`, and hashes with it, as mongod 8.2 does.

#### Fixed

- The query stats key of a `$rankFusion` / `$scoreFusion` aggregate with
  more than one input pipeline printed no `otherNss`, so its `keyHash`
  differed from mongod's.

Fixes RM-644 (tests/backlog/test_rm_found_2026_11_14_rm532.py::test_a_hybrid_search_names_the_collection_it_unions)
