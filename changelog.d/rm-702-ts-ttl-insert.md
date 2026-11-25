### A time-series insert no longer fails when the TTL monitor removes its bucket

An insert whose open bucket a TTL pass deleted while it was being written
returned write error 112 to the client; mongod 8.2.11 retries it into a new
bucket. The Rust MongoDB server now aborts that bucket and stages its
measurements again, so the insert succeeds.

#### Fixed

- Fixes RM-702 (tests/backlog/test_rm_found_rm456.py::test_an_insert_into_a_bucket_the_ttl_monitor_removes_succeeds)
