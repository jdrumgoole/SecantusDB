### Rust PG server: a PL/pgSQL label qualifies its block's variables

`<<blk>> declare x int := 3; begin return blk.x + p; end` was 42P01
`missing FROM-clause entry for table "blk"`: the interpreter keeps a
function's variables in one flat list, and only the function's own name was
known as a label. It now rebuilds PostgreSQL's namespace levels once per
function: the names each DECLARE section declares, read off the body and
matched to the datums in the order the PL/pgSQL compiler creates them. A
`label.name` is then looked up from the innermost running level outwards, as
`plpgsql_ns_lookup` does.

#### Fixed

- A block's label reaches the variables and records its DECLARE section
  declares (`blk.x`, `blk.r`, `blk.r.f`) and its `ALIAS FOR` names, inside
  the block, in its EXCEPTION handlers and in its own declarations' defaults
  (`y int := blk.x + 1`). Where two levels share a label, a name the inner
  one lacks is found in the outer one.
- A FOR loop's label reaches its loop variable (`<<l>> for i in 1..3 loop
  ... l.i`). A block's label does not reach a loop's variable or a handler's
  `sqlstate`, which have levels of their own; both stay 42P01, as on
  PostgreSQL.

Fixes RP-463 (tests/backlog/test_rp_function_bodies.py::test_block_label_qualifies_a_variable,
tests/backlog/test_rp_plpgsql_labels.py::test_label_qualifies_a_variable).

#### Found

- RP-486: a DO block with a label is a 42601 syntax error.
- RP-487: after an inner block ends, a bare name still reads the inner
  block's variable.
- RP-488: an expression's error in a PL/pgSQL statement carries no
  `internal_query` or internal position.
