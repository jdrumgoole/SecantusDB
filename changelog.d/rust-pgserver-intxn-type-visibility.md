### User types are visible in the transaction that creates them

A user type created on the Rust PostgreSQL server is now visible to later
statements in the same transaction, before it is committed. Previously
`CREATE TYPE t AS (...); SELECT 't'::regtype` in one transaction failed with
`42704 type "t" does not exist`, because planning resolves type names against
the catalog and reads it OUTSIDE the open transaction, so an uncommitted
`CREATE TYPE` was invisible to the read that needed it.

This is the transaction-visibility gap that tables already closed, now closed
for types. It mattered far beyond one statement: psycopg's composite, enum, and
range test fixtures run on a non-autocommit connection and create a type then
immediately use it — `CompositeInfo.fetch`, `EnumInfo.fetch`, and
`RangeInfo.fetch` all query the catalog in that same transaction — so the
invisible type returned `None` and cascaded into `TypeError: no info passed` and
`FeatureNotSupported` across the whole composite/enum/range gauge surface. A
per-connection overlay of the transaction's uncommitted type creates and drops,
consulted before the committed catalog exactly as the table one is, makes the
type resolve, cast, and fetch inside its own transaction; `ROLLBACK` discards
it, `COMMIT` persists it, and `ROLLBACK TO SAVEPOINT` undoes a type created
after the savepoint. Wrapping the catalog read in the transaction was rejected
for the same reason the table fix rejected it — it deadlocks `COPY`, which opens
its own transaction context.

#### Fixed

- A `CREATE TYPE` (composite, enum, or range) issued inside a transaction is now
  visible to later statements in that transaction: `to_regtype`, a value cast to
  the type, and psycopg's `CompositeInfo` / `EnumInfo` / `RangeInfo` `.fetch`
  helpers all resolve it before it is committed.
- `DROP TYPE` inside a transaction hides the type from later statements in the
  same transaction, before the drop is committed.
- `ROLLBACK` and `ROLLBACK TO SAVEPOINT` correctly discard a type created in the
  rolled-back span, and `COMMIT` persists one; the type-catalog collections are
  captured by savepoint pre-images so a rolled-back create cannot survive a later
  commit.
