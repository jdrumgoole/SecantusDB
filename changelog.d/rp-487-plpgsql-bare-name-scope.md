### Rust PG server: a PL/pgSQL variable goes out of scope when its block ends

A bare variable name in a PL/pgSQL function now resolves through the
namespace levels being run, innermost first, then the function's own, as
PostgreSQL's `plpgsql_ns_lookup` does. Before, it was the last variable of
that name the compiler made, so an inner block's variable outlived the
block: `<<o>> declare x int := 1; begin declare x int := 2; begin null;
end; return x; end` returned 2, where PostgreSQL returns 1.

#### Fixed

- After an inner block or a FOR loop ends, a bare name reads the outer
  variable again, not the inner one (a silent wrong answer before).
- A name only an ended block declared is no longer a variable: it is 42703
  `column "z" does not exist`, as on PostgreSQL.
- `SQLSTATE` and `SQLERRM` are a level of their own for each EXCEPTION
  section, so after a nested handler ends the outer handler's values are
  read again.
- A declaration's default reads only the names its own block declared
  before it: in `declare y int := x; x int := 3;` the `x` is the outer one.

Fixes RP-487 (tests/backlog/test_rp_plpgsql_labels.py::test_bare_name_after_inner_block).
Files RP-491: a block's variables are set once, when the function starts,
not each time the block is entered.
