### An `update` or `delete` statement takes its locks as it runs

The Rust MongoDB server took every statement's lock round before an
`update` or `delete` ran and held the last to the end, so each statement's
`Slow query` line read its statistics without a fresh execution ticket.
Each statement now takes its own round and lets it go before its line is
written, as mongod 8.2.11's does: the lines count mongod's admissions.

#### Fixed

- Fixes RM-355 in part (tests/backlog/test_rm_355_364.py::test_an_update_statement_takes_mongods_execution_tickets)
