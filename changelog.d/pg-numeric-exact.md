### `numeric` is exact at any width on the Python PostgreSQL server

The Python PostgreSQL server stored every `numeric` as a BSON Decimal128, which
holds 34 significant digits. A wider value was silently rounded, and a very
small one was changed to a different number entirely: `1E-7000` was stored as
`1E-6176`. That was documented as a permanent limit. It is gone: `numeric` now
holds any value PostgreSQL does, exactly, with its display scale.

It uses the same representation the Rust server adopted, so both servers store
a numeric the same way. A value that fits a Decimal128 exactly is stored as one,
so a Mongo client reading the same collection sees exactly what it saw before
for ordinary numbers. Anything wider is stored in the column as a small document
holding PostgreSQL's text for the value and a sort key, and every part of the
server that compares, sorts or adds numbers understands both forms.

Getting there turned up several older bugs, now fixed. `+`, `-` and `*` on a
`numeric` rounded at 28 digits, below even Decimal128's 34, and `%` converted
its operands to floating point. `ORDER BY` over a numeric column holding `NaN`
failed with an internal error, and so did `UPDATE` of a numeric primary key.
`numrange` bounds printed large values in exponent notation.

#### Fixed

- `sql/numeric.py` (new): the two-form representation, PostgreSQL's canonical
  text, a byte-sortable key with `NaN` above everything, and exact `WHERE`
  filters that work for either form. Ported from
  `crates/secantus-pgplan/src/numeric.rs`, with the Decimal128 neighbour
  calculation corrected for values near the smallest exponent (recorded for the
  Rust side in `tasks/backlog.md`).
- `sql/typemap.py`: writes, literals, typmod enforcement, rendering and sort
  keys understand the wide form; unary minus no longer rounds.
- `sql/planner.py`: `=`, `<>`, `<`, `<=`, `>`, `>=`, `IN`, `BETWEEN` and
  `= ANY(ARRAY[...])` on a numeric column are exact; `sum` / `min` / `max` /
  `avg` over a numeric are computed exactly after the pipeline; an `ORDER BY` on
  a numeric in a grouped, distinct or joined query is done after the pipeline.
- `sql/scalar.py`: `+`, `-`, `*`, `abs`, `round` and `%` are exact.
- `sql/executor.py`: a numeric primary key rejects a duplicate by value
  (`1e40` and `1e40.0`), `ON CONFLICT` finds it by value, and an `UPDATE` that
  changes a numeric key no longer fails.
- `sql/window.py`: a window `ORDER BY` over a wide numeric no longer orders it
  as JSON.
- `sql/ranges.py`: `numrange` bounds accept the wide form and render in plain
  notation.

#### Testing

- `tests/test_sql_numeric_wide.py`: the Rust server's own wide-numeric tests,
  whose expectations PostgreSQL 16 produced, running against the Python
  server: round trips in text and binary, arithmetic, every comparison
  operator, `NaN` placement, exact aggregates, and numeric primary keys.
