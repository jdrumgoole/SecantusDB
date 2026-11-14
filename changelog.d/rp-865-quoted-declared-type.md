### A PL/pgSQL variable of a quoted type name keeps its case

`declare x "Over845"` used to look up the type `over845`, and fail with
42704. The Rust PostgreSQL server now reads a declaration's type as the
grammar does, keeping a quoted name's case, so a variable of a mixed-case
domain is created and its constraints are checked when it is assigned.

#### Fixed

- A PL/pgSQL declaration of a quoted, mixed-case type name keeps its case.
- Fixes RP-865 (tests/backlog/test_rp_found_rp839.py::test_a_plpgsql_declaration_keeps_a_quoted_types_case)
