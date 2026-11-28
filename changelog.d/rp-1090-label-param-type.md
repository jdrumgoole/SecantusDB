### A PL/pgSQL CONTEXT line qualifies a parameter type off the search path

The `PL/pgSQL function ... line N at ...` frame names the function as
PostgreSQL's `format_procedure` does, and that prints each argument type by
`format_type_be`: a type whose schema is not on the search path keeps its
schema. `s1100.f(x s1100.ty)` called with `s1100` off the path is now
`s1100.f(s1100.ty)`, as on PostgreSQL, where it was `s1100.f(ty)`.

#### Fixed

- Rust PostgreSQL server: a PL/pgSQL CONTEXT label renders each parameter
  type through the type it resolves to, qualified and quoted per part when
  it is not visible.

Fixes RP-1090 (tests/backlog/test_rp_found_rp833.py::test_a_label_qualifies_a_parameter_type_off_the_path)
