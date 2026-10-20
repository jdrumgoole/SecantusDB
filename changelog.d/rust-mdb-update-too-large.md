### Rust MongoDB server: an update past 16 MB fails as mongod words it

#### Fixed

- An update whose result passes the 16 MB BSON limit gave the insert
  path's "object to insert too large". It now gives mongod 8.2.11's errors,
  under its "Plan executor error during update" wrapper (or `findAndModify`):
  - "Resulting document after update is larger than 16777216", as 10334
    for one document and 17419 for a multi update;
  - a pipeline update's "Serializing Document failed" above 16 MiB + 16 KiB;
  - an upsert's 17420 "Document to upsert is larger than 16777216".

Fixes RM-223 (`tests/backlog/test_rm_found_2026_10_20b.py::test_update_past_16mb_message`).
