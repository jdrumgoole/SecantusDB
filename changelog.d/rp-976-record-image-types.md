### Record image comparisons cover intervals, times, names and network types

`row('1 day'::interval) *= row('24 hours'::interval)` is now `false` on the
Rust PostgreSQL server, as on PostgreSQL 15.19, where it used to be refused.
The `record_image` operators compare a record's fields by their binary
images, and the server now builds those images for interval (its 16 bytes:
time, day, month), time, timetz, name (NUL-padded to 64 bytes), `"char"`,
macaddr and macaddr8, inet and cidr, tid and pg_lsn, each laid out as
PostgreSQL holds the datum in memory.

#### Fixed

- `*=`, `*<>`, `*<`, `*<=`, `*>` and `*>=` over interval, time, timetz,
  name, `"char"`, macaddr, macaddr8, inet, cidr, tid and pg_lsn fields
  answer as PostgreSQL 15.19 does instead of 0A000.

Fixes part of RP-976 (tests/backlog/test_rp_found_rp590.py::test_record_image_over_remaining_field_types); money, arrays, jsonb, bit strings and ranges stay open under RP-976.
