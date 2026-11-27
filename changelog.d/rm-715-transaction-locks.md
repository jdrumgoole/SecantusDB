### A transaction's statement counts the transaction's locks

On the Rust MongoDB server, the slow-query line and profile entry for a statement in a multi-document transaction now count the locks the way mongod 8.2.11's stashed transaction locker does.

#### Fixed

- The first statement of a transaction counts `MultiDocumentTransactionsBarrier` (w 1), three `ReplicationStateTransition` (w) and one flow control ticket.
- Each later statement's line counts on from the earlier ones (its profile write included) and does not count again a lock the transaction already holds.
- A write in a transaction that a failed statement refuses (an `update` of a view) prints the transaction's locks on its command's line, as mongod does when the failure aborts the transaction.

Fixes RM-715 (tests/backlog/test_rm_view_namespaces.py::test_a_transaction_statement_counts_the_transactions_locks)
