### `reIndex` builds the indexes again, and a mongod store with `background` specs is served

`reIndex` now drops and builds every index but `_id` from its options, as
`mongod` does, so a spec comes back in creation order; a store `mongod`
wrote whose index specs carry `background` opens.

#### Fixed

- `reIndex` rebuilt nothing and answered from the registry, so an index a
  `collMod` had changed kept the field order `collMod` gave it (`hidden`
  after `name`). `mongod` 8.2.11 writes each spec again as it does at
  creation (`unique`, `hidden`, `prepareUnique` before `key`, then `name` and
  the rest, a changed TTL last) and keeps the indexes' order. A `--repair`
  that salvages index files already did this; it is pinned by
  `tests/mongod_live/test_index_spec_repair.py`.
- A store `mongod` wrote with `background` in an index spec (kept in the
  catalog by `mongod`, and by this server since RM-788) was refused by name
  when opened.

Fixes RM-789 (tests/backlog/test_rm_index_spec_fidelity.py::test_reindex_writes_a_collmodded_spec_in_creation_order)
