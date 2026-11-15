### Rust MongoDB server: a time-series `aggregate` behind an inclusion `$project` reads a bucket at a time

A time-series `aggregate` of `$match` and an inclusion `$project` now unpacks
its buckets one at a time as the cursor asks, with no cursor-stage buffer, as
mongod 8.2.11's slot-based engine does; an inclusion `$project` alone reads
through the 32-bucket buffer like the other streamed stages. Both used to read
the whole collection at the command.

#### Fixed

- A time-series cursor behind an inclusion `$project` returned 3,999 more
  measurements after a delete where mongod returns 71 (with a `$match`) or
  2,303 (without one).

Files RM-702 (an insert racing the TTL monitor fails with 112).

Fixes RM-456 (tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_projection_reads_a_bucket_at_a_time)

Filed RM-702: an insert into a time-series bucket the TTL monitor removes fails with 112.
