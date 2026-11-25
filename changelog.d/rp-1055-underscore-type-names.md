### Rust PG server: a `_`-prefixed type name names the type of that name

A type whose own name starts with `_` is found by that name, and an array
type is found by the name PostgreSQL gives it. After `CREATE TYPE _custom`
and `CREATE TYPE custom`, `'_custom'::regtype` used to answer `custom[]`.
It now answers the enum `_custom`, as PostgreSQL 15.19 does.

#### Fixed

- `'_name'::regtype` and `to_regtype('_name')` look up a type named
  `_name` before they look for an array of `name`.
- An array type's own name is the one PostgreSQL's `makeArrayTypeName`
  gives it. That is `_` and the element's name, with another `_` in front
  while the name is taken, in creation order. With `custom` created
  before `_custom`, `__custom` is now `custom[]` and `___custom` is
  `_custom[]` (both had been `_custom`'s array or no type), in `public`
  and in any schema on the search path.

Fixes RP-1055 (tests/backlog/test_rp_found_rp970.py::test_an_underscore_type_name_resolves_to_that_type,
tests/backlog/test_rp_found_rp970.py::test_every_underscore_spelling_names_the_type_postgresql_names)
