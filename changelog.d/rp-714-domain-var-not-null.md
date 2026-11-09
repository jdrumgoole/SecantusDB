### Rust PG server: a PL/pgSQL variable of a NOT NULL domain with no default is refused

A PL/pgSQL variable declared with a domain type and no DEFAULT started out
NULL, whatever the domain allowed. `do $$ declare x dnn as ...; begin end $$`,
with `dnn` a domain `int not null`, ran, and a later `select 5 into x`
succeeded. PostgreSQL 15.19 refuses that block when it is entered: 23502 `domain
dnn does not allow null values`, under the CONTEXT `PL/pgSQL function
inline_code_block line 1 during statement block local variable
initialization`.

`exec_stmt_block` initialises a domain-typed variable that has no DEFAULT by
assigning it NULL through `exec_assign_value`, claimed to be of type
`unknown` so the coercion to the domain runs. The interpreter now does the
same on every entry to a block. So the domain's NOT NULL, including one
inherited from a domain it is defined over, and its CHECKs judge the NULL: a
domain `check (value is not null)` is 23514. A domain whose CHECK passes on
NULL, and an array of the domain, still start NULL.

#### Fixed

- Fixes RP-714 (tests/backlog/test_rp_found_rp694.py::test_a_not_null_domain_variable_without_a_default_is_refused),
  with the error's CONTEXT pinned by
  tests/backlog/test_rp_found_rp694.py::test_a_not_null_domain_variables_refusal_names_the_block_initialization.
