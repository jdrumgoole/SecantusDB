### A GROUP BY position inside a grouping set is a position

`GROUP BY rollup(1)`, `GROUP BY grouping sets ((1), ())` and `GROUP BY (1, 2)` now read each integer as a select-list position, as PostgreSQL's `transformGroupClause` does for every item it flattens. They grouped by the constant itself and failed with 42803; a position past the select list inside a set (`rollup(9)`) is now 42P10 `GROUP BY position 9 is not in select list` at the number, not 0A000.

#### Fixed

- A GROUP BY position inside ROLLUP, CUBE, GROUPING SETS or a parenthesised list resolves to its select-list item.

Fixes RP-873 (tests/backlog/test_rp_found_rp854.py::test_a_group_by_position_inside_a_set_or_list_is_a_position)
