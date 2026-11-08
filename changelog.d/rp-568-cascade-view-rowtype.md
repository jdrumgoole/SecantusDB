### Rust PG server: a view a DROP reaches takes what names its row type with it

A view's row type is a type like a table's, and a routine over it depends on
it. `DROP TABLE t` with a view `v` over `t` and a function `f(v)` named only
the view, and `DROP TABLE t CASCADE` dropped the view and left `f` behind,
over a type that no longer existed. The same held for a view or materialized
view reached through another one, by DROP TABLE, DROP VIEW or DROP
MATERIALIZED VIEW.

#### Fixed

- A DROP's 2BP01 DETAIL and its CASCADE notice name what depends on each
  view's or materialized view's row type (or its array) right after that
  view, ahead of the view's own readers, as `findDependentObjects` reaches
  them; CASCADE drops those routines, casts and transforms too.
- An object two of the dropped relations' row types reach (`f(v1, v2)`,
  `f(t) returns v`) is named once, under the newer relation, as PostgreSQL
  15.19 names it.

Fixes RP-568 (tests/backlog/test_rp_found_rp546.py::test_cascaded_view_takes_its_row_type_dependants,
tests/backlog/test_rp_found_rp546.py::test_row_type_dependants_through_nested_and_shared_views)
