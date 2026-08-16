### Multi-name DROP TABLE and Bind format-code validation

`DROP TABLE a, b, c` now behaves as the single statement it is in
PostgreSQL: one CommandComplete tag instead of one per table, and — without
IF EXISTS — every name must resolve before anything is dropped, so a
missing table aborts the whole statement with 42P01 and leaves the others
intact. Separately, Bind now rejects parameter/result format codes other
than 0 (text) and 1 (binary) with PG's 08P01 protocol violation before
BindComplete. Both shapes are pinned by the pgtest `errors` corpus file,
now green (its foreign-key ConstraintName error-field expectations already
passed unchanged).

#### Fixed
- Multi-name DROP TABLE emitted one tag per table and dropped
  left-to-right before failing on a missing name.
- Invalid Bind format codes were silently accepted.
