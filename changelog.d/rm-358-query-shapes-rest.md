### Almost every read names its query shape

The Rust MongoDB server's `queryShapeHash` now covers nearly everything
mongod 8.2 gives a query shape. Geo predicates, `$text`, `$where` and
`$sampleRate` are shaped, along with the aggregation operators and
accumulators that take arguments of their own: the date operators,
`$convert` and the `$toX` family, `$let`, `$map`, `$filter`, `$reduce`,
`$switch`, the regex and trim operators, `$getField`, `$setField`,
`$sortArray`, `$zip`, `$topN`, `$median`, `$accumulator` and more. So are
the stages beyond the common ones, change streams, a collectionless
`aggregate: 1`, and a DBPointer literal. The `Slow query` line, the profile
entry and `explain` print the same hash mongod prints for the same command.

Each node is shaped by its own mongod serializer, transcribed from mongod
and checked against mongod 8.2.11. A constant under an operator with named
arguments is wrapped as `{$const: ...}`, while an n-ary operator's constant
is not. A geo literal becomes a valid stand-in shape, such as a `[1, 1]`
point or a unit triangle, so the representative query still parses. The
stages mongod rewrites into others are shaped as the stages they become:
`$bucket` as a `$group` over a `$switch`, `$setWindowFields` as a `$sort`
and `$_internalSetWindowFields`, and `$documents` as the four stages mongod
runs it as. Two existing shapes were wrong and now match mongod: a
`$project` mixing included and computed fields, and a `$lookup` on fields
with an empty pipeline. `$jsonSchema`, `$rankFusion` and `$scoreFusion`
still print no hash.

#### Fixed

- `secantus-commands` `query_shape.rs`: mongod 8.2's `queryShapeHash` for
  geo (`$geoWithin`, `$within`, `$geoIntersects`, `$near`, `$nearSphere`),
  `$text`, `$where` and `$sampleRate` predicates; the date operators,
  `$convert` and `$toX` (`$toUUID` included), `$let`, `$map`, `$filter`,
  `$reduce`, `$switch`, `$cond`'s document form, the regex, trim and
  replace operators, `$getField`, `$setField`, `$unsetField`,
  `$sortArray`, `$zip`, `$meta`, `$rand`, `$function`, `$sigmoid`, the
  `$similarity*` operators, `$maxN` and its kin, `$median`,
  `$percentile`, the trigonometric and bitwise operators, and `$substr`;
  the `$top` / `$bottom` / `N` accumulators, `$mergeObjects`,
  `$concatArrays`, `$setUnion` and `$accumulator`; `$facet`, `$lookup`
  with a pipeline, `$bucket`, `$bucketAuto`, `$graphLookup`, `$unionWith`,
  `$redact`, `$setWindowFields` (window functions and bounds), `$fill`,
  `$densify`, `$out`, `$merge`, `$geoNear`, `$collStats`, `$indexStats`,
  `$planCacheStats`, `$documents`, `$score`, `$changeStream` and
  `$changeStreamSplitLargeEvent`; `aggregate: 1`; a DBPointer literal; a
  `$meta` sort key (named `$computed<position>`) and a find's `$meta`
  projection.
- A `$project` mixing included and computed fields hashes as mongod's does
  (`_id`, then the included fields, then the computed ones and the
  sub-objects), and so does a `$lookup` on `localField` / `foreignField`
  with an empty `pipeline`.
- Fixes RM-358 in part (tests/mongod_live/test_query_shape_hash.py, and
  the `query_shape.rs` unit tests for what this server refuses to run:
  `$text`, `$where`, `$function`); `$jsonSchema`, `$rankFusion` and
  `$scoreFusion` stay open on
  tests/backlog/test_rm_355_364.py::test_a_query_of_every_kind_names_its_query_shape.
  Files RM-466 (an `explain` of `$planCacheStats` fails).
- Fixes RM-454
  (tests/backlog/test_rm_found_2026_11_09_rm076.py::test_a_change_stream_has_a_query_shape_hash):
  the `explain` of a collection or database change stream carries
  mongod's `queryShapeHash`. `tools/probes/change_stream_commands.py` now
  finds no difference from mongod 8.2.11 in its 115 scenarios.
