### Rust MongoDB server: geo order in `$or` branches and under a trailing range

#### Fixed

- **An `$or` with a geo branch returns mongod's order** (fixes RM-151).
  mongod scans each branch's index and merges the scans in the catalog
  order of those indexes, each document once. A geo branch replays its
  scan of the geo index, and any other branch comes in its plan's order,
  so an `_id` branch comes first and two geo branches come in the order
  written. They came back in storage order.
- **A compound `2dsphere` index with a range on its trailing field.** The
  range bounds the scan, as mongod's does (`k: {$gte: "a"}` scans
  `["a", {})`), and the documents come in that scan's order.
- **Line against line is S2's `S2Polyline::Intersects`** (RM-148 part).
  Two edges meet when they cross or share a vertex, settled by S2's exact
  predicates, so a line ending exactly on another line's edge does not
  meet it, as on mongod. It was a floating-point test with a tolerance
  (`tests/backlog/test_rm_geo_residuals.py::test_line_ending_exactly_on_another_line`).
- **`$near` in a planner error and `parsedQuery`.** mongod's planner text
  prints a `$near` as `GEONEAR  field=loc maxdist=... isNearSphere=...`,
  and `parsedQuery` keeps a legacy `$near` together with its
  `$maxDistance`. Both now match. `tools/probes/geo_queries.py` is at none
  of 1,278 replies against a replica-set mongod 8.2.11.
- Tests: `tests/backlog/test_rm_geo_order_residuals.py::test_geo_predicates_in_or_branches`,
  `::test_geo_predicate_or_id` and
  `::test_compound_geo_index_with_a_range_on_its_trailing_field`.
