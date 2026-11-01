### Rust PG server: the built-in catalogs' initdb heaps are recorded (RP-291, heap slice 1)

The PostgreSQL 15.19 dump now records where `initdb` left every built-in
catalog's rows: each live tuple's `ctid` and each heap's length in blocks,
read from a fresh database. The server loads them (`catalog_heap.rs`) as the
base the next RP-291 slices place later catalog rows on and count heap pages
from. RP-291 stays open.

#### Added

- `pg15_catalog_heaps.txt`, written by `tools/probes/pg15_system_catalog_dump.py`,
  and crate tests checking it against the rows file (one row per tuple, in
  `ctid` order) and against every recorded catalog index (as many entries on
  each heap block; an entry names a HOT chain's root, which is on its live
  tuple's page).
