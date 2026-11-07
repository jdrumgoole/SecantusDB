### Rust PG server: a VALUES list's references and an unread WITH item are analysed

PostgreSQL parse-analyses every row of a VALUES list and every item of a
WITH before it runs anything. Here a column reference in a VALUES row was
refused as a gap, and a WITH item that nothing read was dropped without being
looked at. So `with w as (select nosuch) select 1` answered `1`, and
`with w as (select nosuch) delete from t` deleted the rows, where PostgreSQL
refuses both with 42703.

#### Fixed

- A column reference in a VALUES row is `transformColumnRef`'s error at its
  own token: 42703 `column "nosuch" does not exist`, or 42P01 `missing
  FROM-clause entry` for a qualified one. It used to be 0A000 `ColumnRef is not
  supported yet`, or the error of a cell evaluated before it
  (`values (1/0, nosuch)` was 22012). Each row is analysed before its length is
  compared, and every row before any is evaluated.
- A correlated or LATERAL VALUES reads the outer row:
  `select (values (a)) from (select 1 a) t` and
  `select * from t, lateral (values (t.a)) v` answer their rows instead of
  0A000.
- Every item of a non-recursive WITH, up to the last one nothing reads, is
  planned in order before the statement, without being run. An error parse
  analysis raises is then the statement's: an unknown name, relation or
  function, or a type mismatch, on a SELECT and on an UPDATE, DELETE or INSERT
  ... VALUES. A write is refused before it runs. An unread item is still never
  evaluated, so `with w as (select 1/0) select 1` answers 1, as there.
- A WITH inside a FROM subquery sees the enclosing statement's CTEs from its
  own items' bodies, and its items hide the enclosing ones of the same name.
  `with w as (select 1 a) select * from (with w as (select 2 a) select * from
  w) s` used to answer 1 (the outer `w`), and an inner item that read an outer
  CTE was 42P01 `relation "o" does not exist`.

Fixes RP-501 (tests/backlog/test_rp_fromless_analysis.py::test_values_and_unread_cte_are_analysed,
tests/backlog/test_rp_fromless_analysis.py::test_values_and_with_items_read_their_scope,
tests/backlog/test_rp_fromless_analysis.py::test_unread_cte_of_a_write_is_analysed).

Filed while working it: RP-503 (a failed statement keeps the writes of its
data-modifying WITH items), RP-504 (what `transformWithClause` checks that is
still unchecked) and RP-505 (a subquery's bare name for a column of an outer
set-operation FROM item).
