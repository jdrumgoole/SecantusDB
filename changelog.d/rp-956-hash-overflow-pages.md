### A hash index's full buckets chain overflow pages

A hash index kept only its bucket count, so a bucket holding more tuples than
a page took none of the overflow pages PostgreSQL chains to it: 1,000 rows of
one key made a 6-page index where PostgreSQL's has 12. The hash index model
now keeps each bucket's pages, 407 hash codes to a page. An insert takes the
first page with room or adds an overflow page; a split copies its tuples to
the new bucket and then cleans the old one, freeing its emptied overflow pages
for reuse; a VACUUM squeezes each bucket it deleted from. CREATE INDEX also
estimates the table before recording its size, as PostgreSQL's build does, so
a hash index built on a never-vacuumed table starts with PostgreSQL's bucket
count.

#### Fixed

- Rust PG server: a hash index's size counts its overflow pages, and a hash
  index built by CREATE INDEX starts from the heap's estimate, not its
  freshly recorded row count.
  Fixes RP-956 (tests/backlog/test_rp_found_rp929.py::test_duplicate_hash_keys_fill_overflow_pages)
