### `$out` to a time-series collection checks each measurement's time field

A `$out` into a time-series collection, new or existing, now fails with
mongod 8.2.11's BadValue (2) and message when a document it would write has
no date in the `timeField`, and leaves the target as it was. It used to write
the document.

#### Fixed

- Fixes RM-718 (tests/backlog/test_rm_found_2026_11_17_rm358.py::test_out_to_time_series_needs_the_time_field)
