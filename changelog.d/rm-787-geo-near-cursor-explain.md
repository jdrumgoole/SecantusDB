### `explain` of a leading `$geoNear` is mongod's `$geoNearCursor`

`explain` of an `aggregate` whose first stage is `$geoNear` now lists the
`$geoNearCursor` stage mongod 8.2 lists: its `parsedQuery` is the stage's
`query` with the near leaf on its key, and its winning plan the near
search over the geo index, with the stages after it as the pipeline's.

#### Fixed

- `secantusd-mdb` listed a `$cursor` with an empty `parsedQuery` over a
  COLLSCAN, then `$geoNear` as a pipeline stage. Fixes RM-787
  (tests/backlog/test_rm_geo_wildcard_plans.py::test_an_explain_of_a_geo_near_stage_is_the_geo_near_cursor_with_its_query)
