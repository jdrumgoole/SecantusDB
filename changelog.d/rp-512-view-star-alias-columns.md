### Rust PG server: a view's `*` over an aliased table takes the alias's column names

`create view v as select * from fs f(x, y)` now reads back as columns `x,
y`, as on PostgreSQL. CREATE VIEW expands a `*` into the columns it covers
when the view is created (`view_deps::expand_stars`). It read a subquery's
alias column list, but for a table it wrote the table's own names (`f.a,
f.c`). The view was created, and every read of it was then 42703 `column
f.a does not exist`.

#### Fixed

- A `*` or `f.*` over a table with an alias column list expands to the
  names the list gives, and the table's own names past the end of a
  shorter list (`fs f(x)` reads `x, c`), as `buildRelationAliases` names
  them.
  Fixes RP-512 (tests/backlog/test_rp_found_rp467.py::test_view_star_over_an_aliased_column_list)
  Regression test: `view_deps::tests::a_star_over_an_aliased_table_takes_the_alias_names`.
