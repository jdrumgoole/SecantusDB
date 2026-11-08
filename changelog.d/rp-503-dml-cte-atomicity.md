### Rust PG server: a failed statement takes back its data-modifying WITH item's write

A statement such as `with x as (delete from t returning a) select nosuch
from x` failed with the right error, but when it ran in autocommit over the
simple protocol (how psycopg sends a statement with no parameters), the
rows were already deleted. The WITH item runs while the statement is
planned, and a lone statement outside a block had no transaction of its own,
so the item's write committed before the rest of the statement failed.
PostgreSQL runs each statement as one unit and undoes all of its writes when
any part fails.

#### Fixed

- A statement with a data-modifying WITH item (INSERT, UPDATE, DELETE or
  MERGE, under a SELECT, INSERT, UPDATE, DELETE or MERGE) now runs in an
  implicit transaction of its own, like a statement that can run user code.
  When it fails, its items' writes are rolled back with its own: an error in
  the main query, an unread item's analysis error, and a failing outer
  INSERT or UPDATE all leave both tables as they were, the same as
  PostgreSQL 15.19. Explicit blocks and prepared statements already got
  this right.

Fixes RP-503 (tests/backlog/test_rp_dml_cte_atomicity.py::test_failed_statement_keeps_no_write)
