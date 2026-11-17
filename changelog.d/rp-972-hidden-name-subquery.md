### A subquery or function may reuse a name an aliased JOIN hides

An aliased JOIN hides the names of the items inside it, so PostgreSQL lets
another FROM item take one of those names:
`select * from (fs join fe using (a)) as j, (select 2 as z) fs` reads both.
The Rust PostgreSQL server already allowed this for a table, but refused
it with 0A000 when the item reusing the name was a subquery or a set-returning
function. Every kind of FROM item of such a name is now kept apart, and the
statement answers as on PostgreSQL.

#### Fixed

- A FROM subquery or function named as a table an aliased JOIN hides is
  planned, not refused.
  Fixes RP-972 (tests/backlog/test_rp_subquery_scope.py::test_hidden_join_name_reused_by_subquery)
