### A collection created in a transaction is registered when it commits

On the Rust MongoDB server, two transactions that created one collection
conflicted at the second one's first write. mongod lets both go on and fails
whichever commits second, with 112 `Collection namespace '<ns>' is already in
use` and the `TransientTransactionError` label. So when the later
transaction committed first, the two servers kept different documents.

A collection a MongoDB transaction creates, by an insert or by `create`, is
now held by the transaction until it commits. Only that transaction sees it,
and the commit writes it into the catalog. A second transaction, or a plain
insert, that created the collection meanwhile makes the commit fail, as on
mongod (measured 8.2.11, 2026-10-14). The PostgreSQL server's `CREATE TABLE`
in a transaction is unchanged.

#### Fixed

- Fixes RM-086 (`tests/backlog/test_rm_085_112.py::test_two_transactions_creating_one_collection_conflict_at_commit`,
  now a `backlog_fixed` regression test).
- `crates/secantus-storage`: `UserTransactionHandle::set_defer_collection_creates`,
  the transaction's own view of a collection it created (`deferred_row`),
  and the write at commit (`flush_deferred_creates`). The adapter turns it
  on for every MongoDB transaction.
- The collection's `_id` index rows and unique claims are kept under a name
  of the transaction's own until the commit moves them (`key_coll`), so two
  transactions that store one `_id` in the collection they both create do
  not conflict before the commit either.
- `tools/probes/transactions.py` has ten scenarios for it, `_id` reads and
  writes inside the transaction among them. Nine match mongod; the tenth,
  with a default collation, behaves the same and differs only in the text
  of a duplicate-key message, which it does outside a transaction too
  (filed as RM-142).
