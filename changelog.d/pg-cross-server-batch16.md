### The Python PostgreSQL server on a store the Rust server shares: honest refusals, faithful indexes

The two PostgreSQL servers share one on-disk format. The Python server
silently mishandled three things the Rust server stores in it, each a
wrong answer rather than an error. Batch 16 makes the Python server either
handle them as PostgreSQL does or refuse them with PostgreSQL's `0A000`.

#### Fixed

- **Triggers it cannot run were skipped.** The Python server runs only
  BEFORE INSERT FOR EACH ROW triggers, and it silently ignored every other
  kind the Rust server stores, so a write bypassed an audit trigger or an
  enforced invariant.
  - A write that would fire such a trigger is now refused. This covers
    INSERT, UPDATE, DELETE, TRUNCATE, MERGE, ON CONFLICT, UPDATE FROM and
    DELETE USING.
  - A multi-event trigger is now read from all of its events, not only the
    first.
- **Partitions read as empty.** The Rust server keeps a partitioned table's
  rows in the root's collection, so the Python server read a partition as
  having no rows and accepted root writes no partition covers.
  - Reading the root is allowed, since every row is there.
  - Anything else touching a partition or writing the root is refused.
- **ALTER TABLE erased what the Rust server recorded.** A Python catalog
  rewrite dropped every key it does not model: `partition_by`, `owner`, a
  column's `collation`. Those keys now round-trip verbatim.
- **CREATE INDEX** matches PostgreSQL on all 48 lines of `indexes.sql`, up
  from 35:
  - a unique index over colliding rows, and an UPDATE into one, are `23505`,
    not `XX000`;
  - default names follow PostgreSQL's `<table>_<cols>_idx`;
  - an index name collides with table and view names in both directions;
  - dropping a constraint's index is `2BP01`;
  - `DROP INDEX a, b` works;
  - `USING hash` and `INCLUDE` render in `pg_indexes`.
