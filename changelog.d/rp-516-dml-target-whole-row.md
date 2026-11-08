### Rust PG server: a write's target is its own whole row

`update fs set c = fs::text returning c`, `delete from fs where fs is null`
and `update fs set c = 'q' returning fs` were each 42703 `column "fs" does
not exist`. PostgreSQL's `setTargetTable` puts an UPDATE's or DELETE's
target in the statement's namespace, so a bare `fs` in its SET values,
WHERE or RETURNING (and an INSERT's RETURNING) is the target's row, of the
table's row type, as a SELECT's FROM item already was here. Valid SQL was
refused.

#### Fixed

- A write's target named alone, or as `fs.*` inside an expression, in its
  SET values, WHERE or RETURNING is `ROW(<columns>)::<table>`: `fs::text`,
  `(fs).c`, `row_to_json(fs)`, `fs = row(1,'y')::fs` and `returning fs`
  answer as on PostgreSQL 15.19. Under an alias only the alias names it,
  and a column of the table's name still wins.
- A subquery in those clauses reads the target's row too: `set c = (select
  fs::text)`, `returning (select fs)`, an INSERT's `returning (select
  fs::text)`.
- An UPDATE ... FROM's or DELETE ... USING's other table named bare in
  RETURNING (`returning fe`) is that table's row; it was 42703.

Fixes RP-516 (tests/backlog/test_rp_subquery_scope.py::test_dml_target_whole_row),
pinned in the crate by `tests::a_writes_target_is_its_own_whole_row`. Files
RP-521, RP-522, RP-523 and RP-524, found while working it.
