### Rust MongoDB server: a `$facet` refuses `$documents` as mongod does

A `$facet` whose pipeline held a `$documents` was answered with the wrong
refusal (73, "can only be run with {aggregate: 1}"), and one whose
`$unionWith` or `$lookup` made its documents with a `$documents` simply
ran. mongod 8.2.11 refuses all of them as it parses the pipeline, with
40600: "$documents is not allowed to be used within a $facet stage", or
"$documents inside of $unionWith ..." / "... inside of $lookup ..." for the
collectionless stage that holds one, however deep. The Rust MongoDB server
now does the same, `explain` included.

#### Fixed

- `secantus-commands` (`aggregate.rs`): the checks mongod makes as it
  parses the pipelines a stage holds are one pass, `nested_stage_problem`,
  in mongod's order: a `$facet`'s whole spec shape (40169 / 40170 / 40171),
  then every facet's own sub-pipelines, then the first stage of each facet
  that may not run in one (40600: `$out`, `$merge`, `$geoNear`, `$facet`,
  `$listCatalog`, `$documents`, and a `$documents` inside a `$lookup` or
  `$unionWith` reading no collection). A `$lookup` or `$unionWith` now
  refuses what its own sub-pipelines hold before its writing stage
  (51047 / 31441), and the pass runs before the outer pipeline's
  `$out`-is-last (40601) and `$geoNear`-is-first (40603) checks, where
  mongod runs it.
- Fixes RM-530 (tests/backlog/test_rm_found_2026_11_10_lookup.py::test_documents_is_refused_in_a_facet).
