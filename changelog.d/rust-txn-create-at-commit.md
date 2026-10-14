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

- `crates/secantus-storage`: `UserTransactionHandle::set_defer_collection_creates`,
  the transaction's own view of a collection it created (`deferred_row`),
  and the write at commit (`flush_deferred_creates`). The adapter turns it
  on for every MongoDB transaction.
- `tools/probes/transactions.py` has eight scenarios for it. One still
  differs: two transactions that insert the same `_id` into the collection
  they both create conflict at the second insert, because the `_id` index is
  keyed by the collection's name.
