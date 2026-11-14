### CREATE FUNCTION reads a constant its operator, target column or COALESCE types

PostgreSQL coerces every untyped string constant in a SQL function's body
while it analyses the body, so `create function gx(p int) returns int
language sql as $$ select p + 'a' $$` fails at CREATE with `invalid input
syntax for type integer: "a"`, at the constant. The Rust PostgreSQL server
read only a cast's constant there; now each constant an operator over a
column or a parameter, an INSERT or UPDATE target or a COALESCE gives its
type is read by that type's input function too. An undeclared parameter's
inferred type also follows a declared parameter, a cast or a numeric
constant beside it and COALESCE's typed argument, as
`pg_prepared_statements.parameter_types` reports on PostgreSQL.

#### Fixed

- Fixes RP-507 (tests/backlog/test_rp_found_rp500.py::test_operand_constant_read_at_create)
