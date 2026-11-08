### Rust PG server: GROUP BY a whole row the select list does not name

`select count(*) from fs group by fs` was 42703 `column "fs" does not exist`:
a table's whole row was only rewritten into its row in the select list, the
WHERE and ORDER BY. The same query with `fs` in the select list only worked
because the GROUP BY name matched the output column. A GROUP BY item and
HAVING now read the whole row the way every other clause does.

#### Fixed

- `group by fs`, `group by fs.*` and `group by x` under `from fs x` group by
  the row whether or not the select list names it, alone or beside other
  keys, and over a FROM-subquery's row too.
- HAVING accepts a FROM item's name as its whole row. PostgreSQL's
  `transformColumnRef` falls back to the relation once no column has the
  name, and so does this check now. Before, `... group by fs having fs is
  not null` was 42703.
- `(fs).a` is the column `a` itself, as PostgreSQL's `ParseComplexProjection`
  makes it, not a field of the grouped row. So `select (fs).a, count(*) from
  fs group by fs` is now 42803 `column "fs.a" must appear in the GROUP BY
  clause ...` at `fs`, where it used to answer as though `a` were grouped.

Fixes RP-532 (tests/backlog/test_rp_found_rp522.py::test_group_by_whole_row).

#### Found

- RP-534: a composite value's IS [NOT] NULL in a WHERE or HAVING tests the
  value, not its fields
  (tests/backlog/test_rp_found_rp532.py::test_composite_null_test_in_a_clause).
- RP-535: ORDER BY a row DESC sorts it ascending
  (tests/backlog/test_rp_found_rp532.py::test_row_order_desc).
- RP-536: an ungrouped whole row's 42803 names its first column, not `fs.*`,
  with no position (tests/backlog/test_rp_found_rp532.py::test_ungrouped_whole_row).
- RP-537: a relation name its alias hides is reported at the FROM item
  (tests/backlog/test_rp_found_rp532.py::test_alias_hides_relation_name).
