### `dropIndexes: "*"` and the catalog follow mongod's index order

An index a `collMod` changes is dropped last by `dropIndexes: "*"`, its spec
listed with the option `collMod` added after `name`, as `mongod` 8.2.11 does.

#### Fixed

- `mongod` keeps two orders. Its durable catalog (`_mdb_catalog`, and
  `$listCatalog`) holds a collection's indexes in build order, and each
  spec's fields in the order it wrote them: `v`, `unique`, `hidden`,
  `prepareUnique`, `key`, `name` and then the rest at creation, and after
  `name` for an option a `collMod` adds. Its in-memory catalog moves an index
  a `collMod` changed to the end, where `dropIndexes: "*"` (and its oplog
  and change-stream entries) take them from; a `collMod` that changes
  nothing does not move it, a restart puts them back in build order, and a
  rename keeps the order. This server wrote `unique` and `hidden` after
  `name`, put every option a `collMod` set before `key`, and dropped in build
  order.
- The index's registry row now records its spec's field order (`specOrder`),
  kept in `_mdb_catalog` as written, taken from the spec when a `mongod`
  store is opened, and written the same way into the `createIndexes` and
  `dropIndexes` entries. `collMod` writes `hidden: false` and
  `prepareUnique: false` as no field, as mongod does, writes a changed
  `expireAfterSeconds` last (mongod removes the field and writes it again),
  and applies it before `hidden` whatever order the command gave them.
  The order of the in-memory catalog is kept in memory, keyed by the
  collection's UUID.
- `$listCatalog` lists each spec in that order.
- A `mongod` store holding an index armed with `collMod`'s `prepareUnique`
  is served, where it was refused by name.
- The store format does not move: a store written before reads as it did, and
  has its catalog rows rewritten in the new order when it is opened.

Fixes RM-734 (tests/backlog/test_rm_found_2026_11_21_resume_tokens.py::test_dropping_every_index_lists_a_collmodded_index_last_with_hidden_after_name)
