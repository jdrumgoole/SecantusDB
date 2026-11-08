### GRANT and COMMENT ON a routine find it as PostgreSQL does

The Rust PostgreSQL server now resolves the routine a `GRANT ... ON
FUNCTION | PROCEDURE | ROUTINE` or a `COMMENT ON FUNCTION | PROCEDURE |
ROUTINE` names the way PostgreSQL's `LookupFuncWithArgs` does, as DROP and
ALTER have since RP-508. With no argument list only the statement's kind is
searched: with a procedure `gq(int)`, `comment on function gq is 'x'` is now
42883 `could not find a function named "gq"`, where it put the comment on
the procedure. Over a function `gm(int)` and a procedure `gm(text)`, `grant
execute on function gm` and `comment on function gm` now succeed instead of
42725, and ROUTINE's 42725 says `routine name "gm" is not unique`.

COMMENT ON a routine used to drop its argument list and its schema and
match on the last name alone, so `comment on function go(text) is 'y'` was
42725 over two overloads of `go`, `comment on function nos.gq` commented
`public.gq`, and `comment on procedure gq(text)` commented `gq(int)`. It now
names the routine by its signature, as GRANT does: the overload is found by
its types, a signature of the other kind is 42809 (`gq(integer) is not a
function`), and a missing one is 42883 `procedure gq(text) does not exist`.
`COMMENT ON ROUTINE` is accepted where it was 0A000.

#### Fixed

- `secantus-pg`: `function_acl_key` takes the statement's kind and looks a
  bare name up through `routines_of_kind` / `routine_lookup_error`. A
  missing signature's 42883 names a procedure for PROCEDURE.
- `secantus-pgplan`: COMMENT ON FUNCTION, PROCEDURE and ROUTINE carry the
  routine's signature (`routine_signature`, shared with GRANT).
- `secantus-pg`: COMMENT ON a routine resolves it through
  `function_acl_key` and `check_routine_kind`. COMMENT ON a built-in
  routine is refused by name (0A000) instead of reported missing (RP-519).
- Fixes RP-514 (tests/backlog/test_rp_found_rp508.py::test_grant_and_comment_look_up_by_kind,
  tests/backlog/test_rp_found_rp508.py::test_comment_names_a_routine_by_its_signature,
  tests/backlog/test_rp_found_rp508.py::test_comment_lands_on_the_named_overload).

Filed along the way: RP-518 (a COMMENT ON FUNCTION is not seen by its own
transaction), RP-519 (a built-in routine is not looked up among its
overloads or its kind), RP-520 (GRANT's 42883 for a missing signature
carries a position).
