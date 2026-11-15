### COALESCE, CASE, GREATEST and LEAST over one domain keep the domain

`select pg_typeof(coalesce(a, a))` over a column of domain `d` is now `d`,
as on PostgreSQL, and so are a CASE with an ELSE, GREATEST and LEAST whose
every result is a column of that one domain: `select_common_type` keeps a
type all its inputs share exactly. They were typed by the domain's base. A
CASE without an ELSE stays the base type, as there, because its implicit
NULL is of unknown type.

#### Fixed

- The common type of CASE / COALESCE / GREATEST / LEAST over columns of a
  single domain.
  Fixes RP-764 (tests/backlog/test_rp_found_rp665.py::test_common_type_of_domain_columns_alone_is_the_domain)
