### Each collection in a table of its own, listed in mongod's catalog

Every collection's documents now live in a WiredTiger table of their own,
named as mongod names them (`collection-<UUID>`) and keyed by RecordId, and
every collection is listed in `_mdb_catalog`, mongod's catalog, in mongod's
own format. Until now all collections shared sixteen tables. Dropping a
collection drops its table instead of deleting its documents one at a time,
and renaming one moves its table rather than copying it.

**This changes the on-disk format again** (store format 2), within the
release freeze the first step started: a store written before it is refused
at open.

#### Changed

- `secantus-storage`: one table per collection, made when the collection is
  created (by a transaction that creates it, when it commits) and dropped
  once a drop commits and nothing has it open; an open drops any table no
  collection names. A read in a transaction whose snapshot predates a drop
  is refused with mongod's `SnapshotUnavailable` message rather than read
  as empty.
- `_mdb_catalog` is written in the same transaction as every catalog change
  (options, indexes, multikey paths), from this server's own registry.
- `$listCatalog`'s `ident` and `pg_relation_filepath` name the collection's
  own table.

#### Removed

- The migrations for stores written before document sharding and before
  RecordId keys: such stores are refused before they could run.
