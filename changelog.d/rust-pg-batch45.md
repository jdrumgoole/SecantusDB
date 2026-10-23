### Rust PostgreSQL server: more of `pg_depend`, and an index predicate's typed constants

A range type's constructor functions are catalogued, `pg_depend` records
more of what PostgreSQL 15.19 records, and an index predicate prints its
constants with their types.

#### Fixed

- Fixes most of RP-105
  (`tests/backlog/test_rp_batch26.py::test_scenario[dependencies on a constant's type and a range's functions]`,
  `tests/backlog/test_rp_batch45.py::test_scenario[a range's constructors, a collation's columns and an extension's type]`):
  - **A range's constructors.** `CREATE TYPE r AS RANGE` now lists its
    five constructors in `pg_proc`: `r(sub, sub)`, `r(sub, sub, text)`,
    `r_multirange()`, `r_multirange(r)` and `r_multirange(VARIADIC r[])`.
    They are internal and immutable, at consecutive oids, and `\df` and
    `regprocedure` name them. Each depends on its type (`i`), and the
    range's cast to its multirange is in `pg_cast`.
  - **A constant's type.** A predicate's or expression's cast now records
    the type it names. The walk had skipped a cast's type name.
  - **More rows.** A column depends on a collation someone made. An
    extension depends on its schema, and its types are its members (`e`).
- Fixes RP-191 (`tests/backlog/test_rp_batch45.py::test_scenario[an index predicate's constants are typed]`):
  an index predicate printed a string literal untyped unless its column
  was text, and an `IN` list as written. It now prints them as ruleutils
  does:
  - **Literals:** `int4` and `numeric` bare, booleans as `true`, anything
    else as its output text cast to the type (`'2020-01-01'::date`,
    `'ab'::bpchar`, `'a'::mood`).
  - **Lists:** `IN` as `= ANY (ARRAY[...])` and `NOT IN` as `<> ALL`, one
    value as a plain comparison.

#### Known

- RP-105, what is left: an extension's functions, operators, casts and
  operator classes have no catalog rows of their own here, so their
  membership is not recorded
  (`tests/backlog/test_rp_batch45.py::test_scenario[an extension's members]`).
