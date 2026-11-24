### A store mongod wrote with a `2d`, `2dsphere` or wildcard index is adopted, not refused

`secantusd-mdb` opens a store `mongod` 8.2.11 wrote that holds geo and
wildcard indexes, serves them, and hands the store back to a `mongod` whose
`validate({full: true})` passes after this server's own writes.

#### Added

- Adoption of `2d`, version 3 `2dsphere` (compound, array-of-documents paths,
  partial, `finestIndexedLevel` / `coarsestIndexedLevel`) and wildcard
  indexes (whole, subtree, `wildcardProjection` as an inclusion and as an
  exclusion, partial) from a `mongod` store. A `2dsphere` of version 1 or 2
  is still refused by name.

#### Fixed

- A `2dsphere` or `2d` index over a path that runs through an array of
  documents (`places.loc`) keyed no document, so `$near` and `$geoWithin`
  on it returned nothing where `mongod` returns the matches; the keys and
  the near search now take every value along the path, the nearest
  measuring the document.
- A partial `2d` or `2dsphere` index kept a key for a document an update
  moved out of its filter; `validate` reported it as an extra index entry.
- Fixes RM-775 (`tests/backlog/test_rm_storage_model.py::test_secantus_serves_mongods_geo_and_wildcard_indexes`,
  `test_secantus_refuses_a_2dsphere_version_it_does_not_key_by_name`).
  Files RM-777: `$geoWithin` / `$geoIntersects` and wildcard predicates are
  answered by a collection scan where `mongod` reads the index.
