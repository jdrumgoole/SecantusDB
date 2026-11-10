### Rust PG server: a PL/pgSQL variable declared `%TYPE` has the type it names

A PL/pgSQL variable declared with `%TYPE`, such as `k t.v%type` or
`k a%type`, could not be assigned. Its default and every later assignment
failed, either with 3F000 `schema "t" does not exist` (the text read as a
qualified type name) or with 42601 `syntax error at or near "$1"`. The
variable's type was the text exactly as written, because libpg_query stubs
out the two lookups PostgreSQL makes while it compiles the declaration.

The server now makes those lookups, following `plpgsql_parse_wordtype` and
`plpgsql_parse_cwordtype` in PostgreSQL 15.19:

- `word%TYPE` takes the type of the variable the word names in the
  namespace at the point of declaration. That is a variable declared earlier
  in the same block, one in an enclosing block, or a parameter, named by
  name or by `$n`. A record or row variable is not a type. A word that names
  no variable is looked up as a type name (`int4%type`, `text%type`), and a
  composite type is not one.
- `a.b%TYPE` is first a variable qualified by a block's label, or by the
  function's name for a parameter. Otherwise it is a relation's column,
  found through the search path. `a.b.c%TYPE` is a schema, a relation and a
  column. The variable gets the column's type and modifier: `t.v%type` over
  a `varchar(3)` column refuses a four-character value with 22001.
- A reference nothing answers is 42601 `syntax error at or near "%"`,
  reported at the `%` with the CONTEXT `invalid type name "..."`. This
  happens at CREATE FUNCTION and before a DO block runs.

#### Fixed

- `secantus-pgplan` (`plpgsql_pct_type.rs`): reads a `%TYPE` reference's
  words and answers the lookups outside the function's namespace: a type
  name, or a relation's column with its type and modifier.
- `secantus-pg` (`plpgsql_fn/percent_type.rs`, `plpgsql_fn.rs`, `lib.rs`):
  resolves each `%TYPE` declaration in creation order against the block
  levels `block_scope` rebuilds. The interpreter gives the variable that
  type, and the declaration check refuses a reference that nothing answers.
  Fixes RP-818 (tests/backlog/test_rp_found_rp809.py::test_a_percent_type_variable_is_assigned).

#### Found while working it

- RP-822: a routine's parameter or result declared `%TYPE` is not resolved.
  The parameter is stored as a type named `"t.v"` and a call sees a
  `record`.
