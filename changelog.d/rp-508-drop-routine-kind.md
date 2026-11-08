### DROP and ALTER of a routine named without arguments look for the statement's kind

The Rust PostgreSQL server now resolves `DROP FUNCTION gq`, `DROP
PROCEDURE gq`, `ALTER FUNCTION gq ...` and the rest with no argument list
the way PostgreSQL's `LookupFuncWithArgs` does: among routines of the
statement's kind only. With a procedure `gq(int)`, `drop function if
exists gq` now skips with the notice `function gq() does not exist,
skipping` instead of failing with 42809 `gq(integer) is not a function`,
which broke an idempotent migration script. `drop function gq` is 42883
`could not find a function named "gq"`, as on PostgreSQL. Over a function
`gm(int)` and a procedure `gm(text)`, `drop function gm` drops the function
and keeps the procedure, where it was refused as ambiguous. With an argument
list the lookup still covers every routine, so the wrong kind is still
42809, as on PostgreSQL.

#### Fixed

- `secantus-pg`: DROP and ALTER of a FUNCTION, PROCEDURE or ROUTINE named
  without arguments take only the statement's kind (`routines_of_kind`).
  A FUNCTION skips procedures, a PROCEDURE takes nothing else, and a
  ROUTINE takes either.
- `secantus-pg`: the 42725 for two routines of one name says the
  statement's kind (`procedure name "gp" is not unique`, `routine name
  ...`) with its hint, and ALTER's carries the hint too. DROP ROUTINE's IF
  EXISTS notice says `routine`, not `function`.
- Fixes RP-508 (tests/backlog/test_rp_found_rp500.py::test_drop_without_arguments_over_the_other_kind,
  tests/backlog/test_rp_found_rp500.py::test_routine_lookup_by_kind,
  tests/backlog/test_rp_found_rp500.py::test_drop_function_keeps_the_procedure_of_its_name).
