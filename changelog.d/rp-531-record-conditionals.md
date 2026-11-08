### Rust PG server: NULLIF, GREATEST and LEAST over a whole row answer as PostgreSQL does

With `fs` a table `(a int, c text)` holding `(1, 'y')`, `nullif(fs, (1,
'y'::text))`, `least(fs, row(0, 'a')::fs)` and `greatest(fs, (2,
'a'::text))` each answered `(1,y)`. This server had no comparison of two
records at the top level, so NULLIF never found them equal, and GREATEST and
LEAST kept whichever argument came first. PostgreSQL 15.19 answers NULL,
`(0,a)` and 42804 `GREATEST types fs and record cannot be matched`. NULLIF
compares through `record_eq`, GREATEST and LEAST order by `record_cmp`, and
`select_common_type` refuses to mix a table's row type with `record`.

#### Fixed

- NULLIF over two records compares them with `record_eq`, its field-type
  checks included: `nullif(fs, (1, 'y'))` is 42804 over the bare `'y'`.
- GREATEST and LEAST order records by `record_cmp`, current value against
  the next as `ExecEvalMinMax` calls it. NULL fields sort last, and two
  fields of different types, or records of different widths, are 42804.
- GREATEST, LEAST, COALESCE and CASE now type a whole row as
  `select_common_type` does:
  - A table's or a view's whole row (`fs`, `fs.*`, an alias of it) is of
    its own composite type, category C.
  - A `ROW(...)`, or a subquery's or CTE's whole row, is of `record`, a
    pseudo-type, category P.
  - Mixing the two, or a row with a scalar, is 42804 at the later argument
    (`coalesce(fs, (2, 'a'::text))`, `greatest(fs, 1)`).
  - Two different composite types are 42846 `could not convert type ft to
    fs`, raised after the category pass over every argument, as in
    PostgreSQL.
  - A `ROW(...)` cast to a composite is reported at its `::`.

Fixes RP-531 (tests/backlog/test_rp_found_rp522.py::test_record_conditionals,
tests/backlog/test_rp_found_rp522.py::test_record_conditionals_more).
