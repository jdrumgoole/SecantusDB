### A subquery's unread WITH item and an EXISTS select list are analysed

An unread WITH item inside a subquery expression under a FROM now raises the
missing name PostgreSQL raises, unless an enclosing level supplies it, and a
cast literal in an EXISTS subquery's select list is read as its type before
the list is dropped, so `select exists (select 'x'::int)` is 22P02.

#### Fixed

- `select (with w as (select nosuch) select 1) from (values (1)) v(a)` is
  42703; `select exists (select 'x'::int)` is 22P02 at the literal.
  Fixes RP-504 (tests/backlog/test_rp_fromless_analysis.py::test_with_under_from_and_exists_literal)
