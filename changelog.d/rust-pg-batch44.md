### Rust PostgreSQL server: citext's byte-order operators, and its operators named by schema

The rest of citext's operator surface now works as in PostgreSQL 15.19.

#### Fixed

- Fixes RP-178, its remainder
  (`tests/backlog/test_rp_batch38.py::test_scenario[citext's pattern operators and an operator named by its schema]`,
  `tests/backlog/test_rp_batch44.py::test_scenario[citext's byte-order and schema-named operators]`).
  - **Byte-order operators.** `'a'::citext ~<~ 'B'::citext` (and `~<=~`,
    `~>=~`, `~>~`) was 42804 `collations are not supported by type
    citext`. Over citext casts and columns they now compare the lowered
    strings bytewise, as `citext_pattern_lt` does.
  - **Operators named by schema.** `OPERATOR(s.=)` named with the
    extension's schema is now citext's comparison whether or not the path
    reaches `s`. Named with `pg_catalog`, it is text's.
