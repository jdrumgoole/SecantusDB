### Composite field selection and NULL-blind composite equality on the Rust PostgreSQL server

The Rust PostgreSQL server (`secantusd-pg`) now understands `(expr).field` — the
field-selection syntax psycopg emits when a composite is registered — and it now
compares composite VALUES the way PostgreSQL does, which is not the same as
comparing two row constructors. A composite with a NULL field, compared for
equality against another composite, is equal when their NULL fields line up;
only a bare `ROW(...) = ROW(...)` keeps SQL's three-valued rule where a NULL
makes the result NULL. Composite fields that are themselves composites compare
by the same rule, recursively.

Together these close the composite-planner half of psycopg's `test_composite.py`
gauge: field selection (`(mycol).foo`, `(ROW(1,2)).f1`), composite `=`/`<>`/`<`
with NULL fields, and comparison of nested records all now match a real
PostgreSQL 16 oracle. What remains — binary anonymous-record result encoding and
reserved-keyword quoting in `regtype::text` output — is tracked in the backlog;
neither is a planner-comparison gap.

#### Added

- `secantus-pgplan`: `(expr).field` field selection (`AIndirection`) in the
  scalar expression evaluator and the const-column projection allow-list. A
  named composite resolves the field name through its declared field list (and
  reports the field's declared type in the row description); an anonymous record
  names its fields `f1`, `f2`, … by position. A field of a NULL composite is
  NULL; an unknown field is `42703`, with PostgreSQL's own message
  (`column "x" not found in data type t` / `could not identify column "x" in
  record data type`).

#### Fixed

- `secantus-pgplan`: `record_compare` now takes the operand kind into account.
  Comparing composite VALUES treats two NULL fields as equal and a NULL as
  sorting larger than any non-NULL (so the result is always true/false), while a
  bare row constructor keeps the three-valued NULL rule. The row-constructor case
  is detected from the AST (`N::RowExpr` on both sides) in the expression
  evaluator; every other record comparison is a value comparison.
- `secantus-pgplan`: `record_compare` recurses into a field that is itself a
  record, so comparing composites whose fields are composites (`ce2 = ce2`) no
  longer fails with "comparing document with document using =".
