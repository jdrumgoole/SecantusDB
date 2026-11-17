### A CHECK may name its own table by regclass

`CREATE TABLE t (x int, CHECK (tableoid = 't'::regclass))` is now accepted.
PostgreSQL cooks a table's CHECK constraints after `heap_create_with_catalog`
has made the relation, so the expression can name it; the server judged the
expression before the table existed and refused it with `relation "t" does
not exist`. The new relation is now visible while its constraints are
cooked, as its row type already was.

#### Fixed

- Rust PG server: a CHECK naming its own table by `regclass` is taken at
  CREATE TABLE.
  Fixes RP-974 (tests/backlog/test_rp_found_rp816.py::test_a_checks_regclass_of_its_own_table_resolves_at_create)
