### `$$ROOT` no longer shows a document's hidden sort key

Behind a `$sort` whose key a later stage reads, `$$ROOT` (in `$addFields`,
a `$lookup` `let`, or anywhere an expression reads the whole document) gave
the document with this server's internal `__secantus_meta_sortKey` field.
mongod 8.2.11 keeps that key as metadata, which `$$ROOT` never shows.

#### Fixed

- `$$ROOT` and `$$CURRENT` leave out the hidden metadata fields (sort key,
  `$geoNear` distance and point).

Fixes RM-620 (tests/backlog/test_rm_found_2026_11_13_rm520.py::test_root_does_not_show_the_sort_key)
