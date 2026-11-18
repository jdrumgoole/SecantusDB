### Rust MongoDB server: `{$meta: "sortKey"}` reads in a `$facet` pipeline

A `$facet` pipeline now continues from its documents' sort-key metadata, as
mongod 8.2.11 does: the query layer's sort key reads there, and with none
`$addFields`, `$set` and `$group` give `[]` (the facet's own `$sort` sets no
key) and `$match` sees it missing. The server used to refuse the `$meta`.

#### Fixed

- `{$meta: "sortKey"}` in a `$facet` pipeline is answered, not refused with code 2.
  Fixes RM-625 (tests/backlog/test_rm_found_2026_11_13_rm521.py::test_a_facet_sort_key_is_empty)
