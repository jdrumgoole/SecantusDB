### Rust PG server: a multi-statement query's block statements answer as PostgreSQL's do

A simple Query of several statements runs in PostgreSQL's implicit
transaction block (`TBLOCK_IMPLICIT_INPROGRESS`), and the Rust PostgreSQL
server now models it for every block statement, as PostgreSQL 15.19 does.
The common case was `begin; ...; commit` sent as one string, which drew a
WARNING 25001 `there is already a transaction in progress` on every run.
The rarer one was more serious: `select 1; commit and chain` left the
session in a transaction where PostgreSQL refuses the statement.

#### Fixed

- A BEGIN or START TRANSACTION in a multi-statement query turns the
  implicit block into a real one with no WARNING (`BeginTransactionBlock`);
  a BEGIN inside an explicit block still warns 25001.
- A COMMIT, ROLLBACK or PREPARE TRANSACTION in the implicit block ends it
  with PostgreSQL's WARNING 25P01 `there is no transaction in progress`
  (`EndTransactionBlock`), where it ended it silently.
- COMMIT AND CHAIN and ROLLBACK AND CHAIN in the implicit block are 25P01
  `... can only be used in transaction blocks`, where they committed and
  left the session in a new transaction.
- SAVEPOINT, RELEASE SAVEPOINT and ROLLBACK TO SAVEPOINT in the implicit
  block are 25P01 `... can only be used in transaction blocks`, where a
  SAVEPOINT was accepted.
- A BEGIN that continues an open transaction (nested, or in the implicit
  block) keeps the transaction's isolation level, read-only and deferrable
  modes and sets only the ones it names: `set transaction read only;
  begin; show transaction_read_only` is `on`, where every BEGIN reset them
  to the session defaults.

Fixes RP-829 (tests/backlog/test_rp_found_rp823.py::test_a_begin_in_a_multi_statement_query_does_not_warn,
tests/backlog/test_rp_found_rp823.py::test_the_implicit_blocks_transaction_statements,
tests/backlog/test_rp_found_rp823.py::test_a_prepare_transaction_in_the_implicit_block_prepares_it)

#### Filed

- RP-831: a transaction's isolation, read-write and deferrable modes can be
  changed after it ran a query, where PostgreSQL refuses with 25001.
