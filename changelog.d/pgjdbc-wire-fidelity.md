### pgjdbc wire fidelity: DISCARD tags, implicit-txn commit, OUT-param procedures

Four wire-protocol fidelity fixes surfaced by CockroachDB's pgtest `pgjdbc`
corpus (which greens as a result). `DISCARD` now echoes its target in the
command tag (`DISCARD ALL` / `DISCARD PLANS` / `DISCARD SEQUENCES` /
`DISCARD TEMP`) instead of a bare `DISCARD`. A simple `Query` message received
mid-pipeline now commits any pending extended-protocol **implicit** transaction
before it runs — pgjdbc's autosave pattern interleaves a simple query between
extended `Execute`s and relies on the earlier statement committing (so
re-executing a unique insert then conflicts), and the reported transaction
status becomes idle; an explicit `BEGIN` block is left open. And stored
procedures now support **OUT parameters**: a procedure is keyed by its total
parameter count (CALL supplies a placeholder for every parameter, OUT included),
`CALL` returns the OUT / INOUT parameters as its result row, and an
extended-protocol `Describe` of a CALL portal reports that shape — a
`RowDescription` when the procedure has output parameters, `NoData` when it
doesn't — without running the body (so a procedure that `COMMIT`s internally
emits no stray `RowDescription`s).

#### Fixed

- `engine.py`: `DISCARD <target>` command tags; procedures keyed by total
  parameter count with `OUT` parameters forming the CALL result row and the
  extended `Describe`-portal shape (`describe_statement` CALL branch).
- `pgserver.py`: a simple `Query` mid-pipeline commits a pending
  extended-protocol implicit transaction (explicit blocks untouched).
