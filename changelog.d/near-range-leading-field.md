### A `$near` with a range on a compound index's leading field returns every match

`{pts: {$near: ...}, t: {$gte: 2}}` over `{t: 1, pts: "2dsphere"}` returned 28 of mongod's 30 documents, and a `$near` or `$geoNear` over a multikey trailing field returned 45 of 60: documents were silently dropped from the result.

#### Fixed

- The near search's index scan read the cells of `t: 3` from where `t: 2` left off, past the cells of the first interval; each value of a field now starts the later fields' intervals again, as mongod's scan does.
- A multikey trailing field is bounded by one predicate, the first in canonical order, as mongod bounds it, not by the intersection of two that different elements of an array may meet. A `2d` or `2dsphere` `$near`, `$nearSphere` and `$geoNear` all take it.

Fixes RM-779 (tests/backlog/test_rm_geo_wildcard_plans.py::test_a_near_with_a_range_on_the_leading_field_returns_every_match, tests/backlog/test_rm_geo_wildcard_plans.py::test_a_near_over_a_multikey_trailing_field_bounds_it_by_one_predicate)
