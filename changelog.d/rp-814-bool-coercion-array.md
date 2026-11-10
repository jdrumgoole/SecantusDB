### The Rust PostgreSQL server refuses an array where a boolean is required

`select * from t where a` over an `int[]` column answered no rows, and
`create table ... (a int[] check (a))` created the table. PostgreSQL refuses
both: `coerce_to_boolean` accepts only `boolean`, so an array in a WHERE,
under AND / OR / NOT, under IS TRUE and its kin, or in a CHECK is 42804
`argument of WHERE must be type boolean, not type integer[]`. The Rust
PostgreSQL server now gives the same error. When the operand is a domain,
the error names the domain (`not type di811`), as PostgreSQL's does,
instead of the domain's base type. A domain over `boolean` is still
accepted.

#### Fixed

- `secantus-pgplan` (`optype`): the WHERE / HAVING / CHECK clause check,
  the AND / OR / NOT argument check and the boolean test's argument check
  share one `not_boolean` helper. It follows a domain to its innermost base
  type to decide, refuses every array, and names the operand's declared
  type, reading a column's domain from the catalog's `domain_type`.

Fixes RP-814 (tests/backlog/test_rp_found_rp811.py::test_a_boolean_coercion_refuses_an_array_and_names_a_domain)
