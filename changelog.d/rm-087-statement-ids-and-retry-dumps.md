### Statement ids are checked when a write parses, and retry errors quote what mongod quotes

A write's statement ids are part of the command, and mongod checks them when
it parses one: an `insert`, `update` or `delete` whose `stmtIds` name more or
fewer ids than it has statements, or that has both `stmtId` and `stmtIds`, is
refused in or out of a session, before anything runs. The Rust MongoDB server
checked them only on a retryable write. Outside a session it ran the write;
inside a transaction it inserted the document of the `stmtId` / `stmtIds`
case, and refused the other only after starting the transaction. It now
refuses all of them at parse, as mongod does, and a transaction or a
retryable write's number is never touched by one.

The refusals now carry mongod's whole text. The statement-id errors quote the
ids and the command as mongod's parser serialises it again, with defaults
filled in (`bypassDocumentValidation`, `ordered`, `multi`, `upsert`, a write
concern's `w` and `wtimeout`), a named `hint` as `{ $hint: ... }`, and the
generic arguments in mongod's order. A `findAndModify` retry that does not fit
the write it retries (40606, 40607, 40608, 40611, 40612) quotes the request,
then the oplog entry of that write, an insert batch's one operation of its
`applyOps` included. An `update` whose later statement has a mistyped field
also applied the statements ahead of it before refusing the command; it now
refuses the command before any statement runs.

#### Fixed

- `secantus-commands` `crud.rs`: `write_parse_problem` runs mongod's
  `checkOpCountForCommand` on `insert`, `update` and `delete` after the
  command's own fields, with `stmtId` / `stmtIds` type-checked in field order
  (14). An `update`'s per-statement types (`q`, `u`, `multi`, `upsert`,
  `collation`, `arrayFilters`, `hint`, `c`) are checked at parse, not as each
  statement runs.
- `secantus-commands` `request_dump.rs` (new): mongod's `BSONObj::toString`
  (a long string cut at 150 bytes, a UUID as `UUID("...")`) and the IDL
  serialisation of `insert` / `update` / `delete` / `findAndModify`, measured
  against mongod 8.2.11.
- `secantus-commands` `session.rs`: a retryable write whose statement ids do
  not fit answers what its parse refuses first; a refused `findAndModify`
  retry reads the statement's oplog entry for its message.
- `secantus-commands` `util.rs`: the `stmtId` / `stmtIds` refusal (72) is a
  parse failure, so it starts no transaction.
- `tools/probes/transactions.py` compares these messages in full (4 of 267
  scenarios still differ, all filed or decided).

Fixes RM-087 in part (tests/backlog/test_rm_085_112.py::test_statement_id_count_error_names_the_request,
tests/backlog/test_rm_085_112.py::test_statement_ids_are_checked_outside_a_session,
tests/backlog/test_rm_085_112.py::test_statement_ids_that_do_not_parse_start_no_transaction,
tests/backlog/test_rm_085_112.py::test_a_find_and_modify_retry_quotes_the_request_and_its_oplog_entry).
Fixes RM-461 (tests/backlog/test_rm_found_2026_11_09_rm087.py::test_a_mistyped_update_statement_applies_none).
Filed RM-460: `insert`, `update` and `delete` take `originalQuery`,
`originalCollation` (and `update` a command-level `sampleId`), which mongod
refuses as unknown (tests/backlog/test_rm_found_2026_11_09_rm087.py::test_a_write_refuses_fields_mongod_does_not_know).
