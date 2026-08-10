### Sequences allocate in batches — bulk SERIAL ingest is 3x faster

Every `nextval` used to pay a full read plus durable-update transaction
against the sequence's stored document, which dominated bulk-ingest
profiles: a 100k-row `COPY` into a SERIAL table spent roughly three
quarters of its time advancing the sequence, capping ingest around
5,000 rows/s. `nextval` now pre-allocates a batch of 128 values with a
single persisted write and hands the rest out from memory under the
same statement-write lock that already serialized it — PostgreSQL's own
`CACHE` mechanism applied server-side. The same `COPY` now runs at
13,000–15,800 rows/s, and per-statement SERIAL inserts gain about 20%.

Values remain gapless while the server runs (the cache is server-wide,
not per-backend). The stored document carries the batch's high-water
mark, so a restart resumes past the unhanded values — the identical gap
PostgreSQL's `CACHE` and crash semantics produce. `setval`,
`ALTER SEQUENCE`, `DROP`, and re-`CREATE` all discard the prefetched
run, so their effects stay immediate.

#### Changed

- `Catalog.sequence_nextval` allocates `SEQUENCE_ALLOC_BATCH` (128)
  values per persisted write; `tests/test_sql_sequences.py` pins the
  gapless run, the high-water persistence and reopen gap, and the
  invalidation on `setval` / `ALTER … RESTART` / re-create paths.
