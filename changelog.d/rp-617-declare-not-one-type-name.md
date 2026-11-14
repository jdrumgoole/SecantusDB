### A PL/pgSQL declaration whose type is not one type name is refused

A DECLARE such as `x int(3)` or `x varchar(nosuch)` used to be created, and
fail later or never. The Rust PostgreSQL server now refuses it at CREATE as
PostgreSQL does: `typeStringToTypeName` reads the text with the grammar, and
text that is not one type name is a 42601 syntax error at its token, with
the context `invalid type name "..."`.

#### Fixed

- A PL/pgSQL DECLARE whose type text is not one type name is 42601 at the
  token the grammar stops at, placed in the CREATE's text.
- Fixes RP-617 (tests/backlog/test_rp_found_rp573.py::test_declare_of_text_that_is_not_one_type_name)
