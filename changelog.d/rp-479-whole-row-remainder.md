### Rust PG server: a subquery reads an outer whole row, and ORDER BY positions count a star's columns

`select ob.*, -a from ob order by 2` sorted by `-a`, not by `ob`'s second
column, and `select *, -a from ob order by 3` did not sort at all: an ORDER
BY position was counted against the select list before its `*`, `t.*` or
`(t).*` was expanded, so every position past a star named the wrong column.
The rows came back in the wrong order with no error. PostgreSQL expands the
stars first (`transformTargetList`), and so does this server now. Also, a
subquery could not read an outer FROM item's whole row: `select (select fs)
from fs` was 42703 `column "fs" does not exist`, where PostgreSQL tries a
relation after every level's columns (`transformColumnRef`).

#### Fixed

- An ORDER BY position counts each select-list star as its columns, over one
  FROM item or a join: `order by 2` after `t.*` is `t`'s second column.
- `(t).*` over a join is `t.*`, its columns, as PostgreSQL's
  `ExpandRowReference` expands a whole-row Var: `select (fs).* from fs, fe
  order by 1` was 0A000 `this field selection is not supported yet`.
- A subquery's bare name that no level has as a column reads an outer FROM
  item's whole row: bare (`(select fs)`), as a field selection's argument
  (`(select (fs).c)`), in a call (`(select row_to_json(fs))`), and two levels
  down. It is of the table's row type (`pg_typeof` is `fs`), or a record for a
  FROM subquery's row.

Fixes RP-479 (tests/backlog/test_rp_subquery_scope.py::test_whole_row_remainder,
tests/backlog/test_rp_subquery_scope.py::test_subquery_reads_an_outer_whole_row,
tests/backlog/test_rp_subquery_scope.py::test_order_by_position_counts_star_columns).

#### Filed

- RP-516: an UPDATE's or DELETE's target table is not a whole row to its own
  SET, WHERE or RETURNING (`update fs set c = fs::text` is 42703).
- RP-517: `v.*` in a subquery's select list over an outer FROM item is 42P01
  (`select (select v.*) from (values (1)) v(a)` answers 1 on PostgreSQL).
