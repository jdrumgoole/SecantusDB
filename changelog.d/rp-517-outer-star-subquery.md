### Rust PG server: `v.*` in a subquery reads an outer FROM item's columns

`select (select v.*) from (values (1)) v(a)` answers 1 on PostgreSQL, and
here it was 42P01 `missing FROM-clause entry for table "v"`. The same error
came back for every star a subquery wrote over a table, a derived table, a
function, a join or a CTE of an enclosing query, in a scalar subquery, `IN`
or `ARRAY(...)`. So a valid query was refused, and a star too wide for its
subquery got the wrong error. PostgreSQL expands such a star with the
enclosing level's range-table entry (`ExpandColumnRefStar`), and so does this
server now. Separately, a scalar subquery whose first target is a star was
named after the qualifier (`v`) or `?column?` rather than its first column.

#### Fixed

- `q.*` in a subquery's select list, where `q` is an enclosing query's FROM
  item at any depth, is that item's columns, read as outer references. One
  column answers, and a wider one is the sublink's 42601 (`subquery must
  return only one column`, `subquery has too many columns`). A star the
  subquery's own FROM binds stays its own, and an aliased table named by its
  own name is still the 42P01 invalid reference.
- A scalar subquery whose first target is a star is named by the star's
  first column, as `FigureColname` reads the analysed subquery: `(select
  v.*)` over `v(a)` is `a`, and `(select * from (values (3)) w(z))` is `z`,
  through a cast and through a nested subquery too.

Fixes RP-517 (tests/backlog/test_rp_subquery_scope.py::test_outer_star_in_subquery,
tests/backlog/test_rp_subquery_scope.py::test_star_subquery_is_named_by_its_first_column).

#### Filed

- RP-697: a row compared with a correlated subquery (`(a, c) in (select
  fs.a, fs.c)`) is 0A000.
- RP-698: a correlated set operation (`(select v.a intersect select 5)`) is
  42P01.
- RP-699: a correlated subquery that passes an outer FROM function's column
  or a whole row's field through unchanged answers it as text, and `(select
  (v).*)` is 0A000.
- RP-700: a GROUP BY naming a correlated subquery target (`group by 1`) is
  42803.
- RP-701: a LATERAL subquery's outer column is named by its type (`int4`),
  and a star over an outer item there is 42P01.
