### Parameterised queries work against the Rust PostgreSQL server

Client libraries switch to PostgreSQL's extended query protocol the moment a
query carries a parameter — `WHERE n > %s` rather than `WHERE n > 5`. The Rust
PostgreSQL server only implemented the simple protocol, so those queries were
answered with a success status and **no rows at all**. That is worse than an
unimplemented feature: an application would have seen an empty result for a
query that should have returned data, with nothing to indicate anything had gone
wrong. Prepared statements, portals, parameter binding and `Describe` are now
implemented, so parameterised queries, prepared statements and the row limits
that clients set on execution all behave.

Adding them immediately exposed a second, quieter bug that had nothing to do
with parameters. Comparing anything to NULL in SQL is never true — `WHERE n =
NULL` returns no rows, even for rows where `n` really is NULL, because only `IS
NULL` tests for it. The server was treating that comparison as a match. It went
unnoticed because every existing test wrote its comparisons as literals, and it
took binding a NULL parameter to make the case obvious enough to write down.

Both protocols now run through one shared execution path, so they cannot drift
apart, and the differential suite that compares the server against a real
PostgreSQL grew to 141 statements — 28 of them over the extended protocol.

#### Added

- The PostgreSQL extended query protocol: `Parse`, `Bind`, `Describe`,
  `Execute` and `Close`, with parameters in both text and binary form, and
  support for the row limit a client can set on execution.

#### Fixed

- Comparing a column to NULL (`= NULL`, `<> NULL`, `> NULL`) returned rows
  where PostgreSQL returns none. Affected literal SQL as well as parameters.
