### A transaction's profile entries count mongod's locks

A statement of a multi-document transaction now takes its intent locks in
IX, reads too, as mongod 8.2 does, and its profile entry counts only the
locks its transaction took for it: the first command the transaction's
barrier, transition, global, database and collection locks, an update or
delete statement its database and collection locks, a later statement only a
collection it did not hold yet.

#### Fixed

- A `find` in a transaction took the global, database and collection locks in
  IS; mongod takes them in IX.
- A transaction statement's profile entry printed the transaction's running
  lock totals, a global IS for its storage statistics and the profile write's
  locks; mongod prints none of those.

Fixes RM-357, in part (tests/backlog/test_rm_355_364.py::test_a_transaction_statements_entry_counts_what_it_took, tests/backlog/test_rm_355_364.py::test_a_collectionless_aggregates_entry_counts_mongods_locks)
