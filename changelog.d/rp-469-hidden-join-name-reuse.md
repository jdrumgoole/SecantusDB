### A table an aliased JOIN hides can be named again beside it

An aliased JOIN hides the names of the tables inside it, so PostgreSQL lets
another FROM item reuse one: `select * from (fs join fe using (a)) as j, fs`
reads `fs` twice, once inside `j` and once on its own. The Rust PostgreSQL
server refused this with "not supported", because a joined row's columns
were keyed by the table's name and the two `fs` would have shared keys. Each
table leaf of such a name is now keyed apart, so the query answers as
PostgreSQL does, whole-row references (`select fs from ...`) included.

#### Fixed

- A FROM item named as a table an aliased JOIN hides is no longer 0A000.
  Fixes RP-469 (tests/backlog/test_rp_subquery_scope.py::test_hidden_join_name_reused)
- Filed RP-972: a subquery or function reusing such a name is still refused.
