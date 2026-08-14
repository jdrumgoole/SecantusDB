### Pipelined statements run in one implicit transaction, like Postgres

Statements a client pipelines before a single Sync now run in ONE
implicit transaction, exactly as PostgreSQL treats them: a mid-pipeline
error rolls back the earlier statements' effects, a clean Sync commits
them, and an explicit BEGIN inside the pipeline takes the transaction
over. pgjdbc's batch semantics depend on this — a failed batch must not
leave its earlier inserts behind — and its 184-variant BatchFailureTest
and 140-variant BatchExecuteTest both now pass in full. The first
statement of a pipeline retries internally on write-write races, so
single-statement autocommit behaves exactly as before.

Two describe/planner gaps closed alongside: SELECTs joining derived
VALUES tables (no real table anywhere) now Describe their shape instead
of answering NoData before emitting rows (a protocol violation pgjdbc
rejects), and CrystalReports' `{oj ((( … )))}` grouping-paren join
chains plan correctly. pgjdbc's OuterJoinSyntaxTest passes in full.

#### Fixed
- Extended protocol: implicit transaction from first pipelined statement
  to Sync (commit / rollback-on-error at Sync; BEGIN takeover;
  transaction-control and VACUUM-class statements exempt).
- Describe over joins of derived VALUES tables returns the row shape.
- Grouping parens around join chains unwrap through multiple layers, and
  an aliased VALUES parsed as a Table-wrapped node normalizes.
