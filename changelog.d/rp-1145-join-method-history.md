### The catalog join-method test's answer is PostgreSQL's after little DDL, and it runs where little DDL has run

RP-290's regression test for the catalog reads a join of two catalogs makes
failed on CI whenever `pytest-randomly` ran three or more of its module's
tests before it. In the nested loop with `pg_class` outer,
`pg_class_relname_nsp_index` fetched 3 heap rows where the test expected 2.
The server was right and the test was wrong. Run again and again in one
database, PostgreSQL 15.20 gives the expected answer on runs 1 to 3, and
from run 4 on it gives exactly the answer CI got, `[3, 3, 3]`. In a
database created for each run it gives the expected answer on every one of
5 runs (private cluster, 2026-11-27).

#### Fixed

- `tests/backlog/test_rp_catalog_reads.py::test_a_catalog_joins_method_decides_its_reads`
  already runs on a server of its own, since RP-1097's commit. Its
  recorded answer stays as it was, and the comment over `JOIN_METHOD_STEPS`
  now says where that answer holds and why the test must not share the
  module's server.
- Fixes RP-1145 (`tests/backlog/test_rp_catalog_reads.py::test_a_catalog_joins_method_decides_its_reads`).
