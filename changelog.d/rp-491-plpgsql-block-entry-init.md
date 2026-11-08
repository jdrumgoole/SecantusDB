### Rust PG server: a PL/pgSQL block initialises its variables each time it is entered

A PL/pgSQL block now gives each variable it declares its default, or NULL,
every time the block is entered, as PostgreSQL's `exec_stmt_block` does.
Before, every default in the function was evaluated once, before the body
ran. So a block inside a loop kept the last pass's values, and a default
read the state at function start rather than at the block:
`declare x int := 1; begin x := 5; declare y int := x; begin return y;
end; end` returned 1, where PostgreSQL returns 5.

#### Fixed

- A block inside a loop starts afresh on each pass. A counter declared
  `c int := 0` and incremented once per pass reads 1 each time, not 1, 2,
  3; one declared without a default is NULL again; and a `record` it
  declares is unassigned again (all silent wrong answers before).
- A default can read a FOR loop's variable: `declare c int := i * 10`
  inside `for i in 1..2 loop` gives 10 then 20. It was 42703 `column "i"
  does not exist`.
- A default's error carries PostgreSQL's CONTEXT line `PL/pgSQL function
  f() line N during statement block local variable initialization`.

Fixes RP-491 (tests/backlog/test_rp_plpgsql_labels.py::test_block_initialises_its_variables_on_entry,
tests/backlog/test_rp_plpgsql_labels.py::test_default_error_names_the_block_initialisation).
