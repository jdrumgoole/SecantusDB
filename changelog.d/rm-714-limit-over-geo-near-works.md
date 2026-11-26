### A LIMIT over a `$near` search reports its work counts

An `explain` of a `find` with a `limit` over a GEO_NEAR stage now prints the
LIMIT's `works`, `advanced` and `needTime` as mongod 8.2.11 does: each work
of the search below it, and one more for the EOF it answers once it has
returned its limit.

#### Fixed

- The LIMIT over GEO_NEAR_2D / GEO_NEAR_2DSPHERE printed no work accounting.
  Fixes RM-714 (tests/backlog/test_rm_found_rm707.py::test_a_limit_over_a_geo_near_reports_its_works)
