### A `dropDatabase` or `dbStats` line names the database

The Rust MongoDB server's `Slow query` line for a `dropDatabase` or a
`dbStats` now names the database alone (`ns: "<db>"`), as mongod 8.2.11's
does: both commands run on the database itself rather than on a collection
or the database's `$cmd` namespace. A log reader that groups lines by
namespace now files them where it files mongod's.

#### Fixed

- `dropDatabase` and `dbStats` (`dbstats`) `Slow query` lines named
  `<db>.$cmd`; they name `<db>` (`slowop::attributes`). Their profile
  entries already named the database.
- Fixes RM-383 (tests/backlog/test_rm_drop_reports.py::test_a_drop_database_line_names_the_database),
  with a crate test, `slowop::tests::a_database_command_names_the_database`.
- Filed RM-495: a `validate`, `dataSize` or `getLog` line names the
  command's string argument as a collection, where mongod's name
  `<db>.$cmd`.
