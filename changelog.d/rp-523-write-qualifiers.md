### Rust PG server: a write's qualified column names are looked up in its own namespace

An UPDATE, DELETE or INSERT read any qualified column in its SET, WHERE or
RETURNING as the target's column, whatever the qualifier said. Under an
alias, `update fs x set c = fs.c` and `delete from fs x where fs.a = 1`
wrote rows, and `update fs set c = zz.c` did too, with no table `zz`
anywhere. Each is now PostgreSQL's 42P01 at the reference.

#### Fixed

- A write's own qualified references are resolved as PostgreSQL's
  `transformColumnRef` resolves them: an aliased target answers to its
  alias alone (`invalid reference to FROM-clause entry for table "fs"`,
  HINT `Perhaps you meant to reference the table alias "x".`), and a
  qualifier nothing in the statement has is `missing FROM-clause entry for
  table "zz"`, in an UPDATE's SET, WHERE and RETURNING (with or without a
  FROM), a DELETE's WHERE and RETURNING (with or without USING), and an
  INSERT's ON CONFLICT and RETURNING, where `excluded` is visible only to
  the ON CONFLICT clause. Fixes RP-523
  (tests/backlog/test_rp_subquery_scope.py::test_aliased_write_target_hides_its_name;
  crates/secantus-pgplan/src/tests.rs::a_writes_qualifiers_are_looked_up_in_its_namespace).
  Files RP-527: a three-name reference's schema is not read.
