### The record_image operators compare dates, timestamps and uuids

`*=` .. `*>=` now compare a date field by its day number, a timestamp or
timestamptz by its microseconds from 2000, and a uuid by its sixteen bytes,
each read as PostgreSQL's `record_image_cmp` reads the datum, so a date
before 2000 sorts after one from 2000 on, as it does there.

#### Fixed

- The `record_image` operators accept date, timestamp, timestamptz and uuid
  fields instead of refusing them with 0A000.
  Fixes RP-976, in part (tests/backlog/test_rp_found_rp590.py::test_record_image_over_other_field_types)
