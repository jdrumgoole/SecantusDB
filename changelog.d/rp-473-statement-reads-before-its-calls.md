### Rust PG server: a statement does not see what the functions it calls write

A statement saw the writes of the volatile functions it called: `select *
from t, f()` over a body that inserts into `t` returned the inserted row
too, a row the body updated came back at its new value, and one it deleted
was missing. On PostgreSQL the statement reads at its own snapshot and
command id, and the function's statements run at later command ids, so the
statement sees `t` as it was before the call (`HeapTupleSatisfiesMVCC`'s
`cmin >= curcid` test). Here the transaction's own writes were visible to
all its reads at once. Now each running statement keeps, from its first
call of a volatile user function, every row its calls write as it was
before them, and lays those rows over its own reads. The function's own
statements still see what the body wrote before them, the next statement
sees everything, and a savepoint's rollback is unchanged.

The view lives in `secantus-storage` (`StatementViews`, beside the
savepoint undo journal, recorded at the same write sites), which the Rust
MongoDB server links; that server opens no view, so it reads as before.

#### Fixed

- A statement reads its tables as they were before the volatile functions
  it calls wrote them: in FROM, LATERAL, an uncorrelated subquery or the
  select list, from a SQL or a PL/pgSQL body, in or out of a transaction
  block, over inserts, updates and deletes, as on PostgreSQL 15.19.
  Fixes RP-473 (tests/backlog/test_rp_from_function_scan.py::test_scan_does_not_see_the_functions_write,
  tests/backlog/test_rp_from_function_scan.py::test_statement_reads_rows_as_before_its_calls;
  crates/secantus-storage/tests/statement_views.rs)
- Filed, found while working it: RP-496, a volatile function in a FROM
  subquery or a CTE joined to a table runs twice
  (tests/backlog/test_rp_from_function_scan.py::test_joined_subquery_runs_its_function_once).
