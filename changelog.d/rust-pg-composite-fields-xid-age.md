### The Rust PostgreSQL server: composite fields, and age() of a transaction id

A column of a composite type can be taken apart and assigned by field, as in
PostgreSQL: `(v).*`, `(t.v).*` and `((v).inner).*` expand to one column per
field, `UPDATE ... SET v.a = 1` changes one field and keeps the rest, and
`INSERT INTO t (v.a, v.b)` builds the value from its fields. Each was refused.

`age(xid)` answers what PostgreSQL's `xid_age` does: the current transaction
id less the argument. Transaction ids follow PostgreSQL's model: one counter
for the cluster that a restart does not reset, persisted in the store, and a
store that starts where `initdb` leaves a cluster -- `datfrozenxid` 722, the
first xid 723. So `age(datfrozenxid)`, which monitoring queries ask, grows
over the store's life as it does on PostgreSQL (no autovacuum advances it
here).

#### Fixed

- **A predicate in a select list over a table** (`n BETWEEN 0 AND 5`,
  `id IN (...)`, `LIKE`, `IS DISTINCT FROM`) was described as its left
  operand's type, so a client reading binary results read the boolean as
  an int4 and failed.
- **`(information_schema._pg_expandarray(a)).n` is an `integer`**, where it
  was a `bigint`; pgjdbc's primary-key queries compare it with one.
  `(_pg_expandarray(a)).*` expands to `x` and `n`.

#### Changed

- `txid_current()` and `pg_current_xact_id()` count from 723 on a new store
  and carry on across restarts, where each run used to start from a number
  derived from the clock. A store from before this release starts at 723
  too: a snapshot SAVED by an earlier run compares as later than the new
  xids.
