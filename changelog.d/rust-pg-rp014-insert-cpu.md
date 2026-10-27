### The Rust PostgreSQL server: less CPU per INSERT

A run of single-row INSERTs costs the server less CPU, with every answer
and every page model (heap, B-tree, SSI, statistics) unchanged. On
`test_type_error_shadow`'s workload from psycopg's suite (three
connections, 50,000 INSERTs each, debug build), the server retires about
29% fewer instructions than before (RP-014, which keeps that test near its
20 s limit on the Linux CI lane).

#### Changed

- `secantus-storage` (linked by the MongoDB server too): an insert decodes
  its documents, mints a missing `_id` and builds the `_id` key and the
  oplog's `o2` before taking the collection's lock. `insert_documents`
  takes documents the caller holds decoded beside their encoding, so they
  are not decoded again. `list_indexes_shared` hands out the cached index
  list instead of a copy. The catalog caches are asked by borrowed names.
  An insert's oplog entries skip the per-entry `local` check when their
  namespace answers it; the user write block is checked as before.
- `secantus-pg`: a table's cached catalog entry is shared, not copied. The
  parameters Bind decodes are reused by the portal's Execute while the
  settings, the catalog and the session's uncommitted catalog are as they
  were. A lookup of a user table skips the virtual relations, all of
  which are `pg_` or `information_schema.` names (a unit test holds them
  to it). The session's zone and DateStyle are parsed once per settings
  generation, a built-in type's tuple layout once per thread. A backend
  with no SERIALIZABLE transaction answers "not tracked" without the SSI
  lock. A unique B-tree insert reuses its uniqueness check's leaf for the
  page SSI checks. Rows are moved, not copied, through stamping, and
  several per-statement checks read settings and names in place.
- `secantus-pgplan`: `max_param_number` remembers the last statement it
  was asked about on each thread.
