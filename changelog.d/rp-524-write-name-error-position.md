### A write's unknown or ambiguous column points at the name as written

An UPDATE or DELETE that names a column that does not exist, or an
ambiguous one, now reports PostgreSQL's position: the reference itself,
found in the order PostgreSQL reads the statement (WHERE, then RETURNING,
then SET). It used to point at the first word of the statement spelling
that name, usually the target table or a FROM item, and an ambiguous
bare column in an UPDATE ... FROM's or DELETE ... USING's RETURNING
carried no position at all.

#### Fixed

- `update fs x set c = fs::text` and `... where fs is null` are 42703 at
  the `fs` in the expression (21 / 31), not at the target (8);
  `update fs set c = 'q' from fe e ... returning fe` at the RETURNING's
  `fe` (60), not the FROM item (28); and `returning a`, ambiguous after
  an UPDATE ... FROM or DELETE ... USING, is 42702 at the `a` (59 / 53).

Fixes RP-524 (tests/backlog/test_rp_subquery_scope.py::test_write_name_error_position)
