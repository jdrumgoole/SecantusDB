### Eleven driver gauges re-measured against the Rust server

Step 5 of the driver-conformance plan, run against `secantusd-rs` built from
`d971ef9e` — the first sweep since the transaction fixes landed.

| gauge | passed | failed | skipped | rate |
| --- | ---: | ---: | ---: | ---: |
| C++, Kotlin, .NET | 892 / 340 / 228 | 0 | 9 / 198 / 0 | **100.0%** |
| php-lib | 3,101 | 1 | 27 | 99.9% |
| php-ext | 679 | 1 | 35 | 99.8% |
| Node | 357 | 1 | 6 | 99.7% |
| Ruby | 293 | 1 | 24 | 99.6% |
| pymongo | 1,205 | 5 | 290 | 99.5% |
| Java | 493 | 3 | 404 | 99.3% |
| mongo-rust-driver | 100 | 1 | 0 | 99.0% |
| Go | 439 | 0 | 37 | withheld — truncated run |

**pymongo hit the predicted 1,205 / 5 exactly**, and its five remaining failures
are precisely the declared non-goals: hashed indexes, text indexes, `$where`
twice, and `test_to_list_csot_applied`. The Rust server went 15 failures to 5 on
that gauge and is now level with the Python server.

#### Changed

- Every `-rust-server` report regenerated from a run of its own. Seven of them
  previously carried a September date over raw artifacts from 10–30 August.
- The two Go reports now carry the truncation banner added in #1613, rather
  than a bare 100.0% over a run that stopped at 476 of 481 tests.

#### Found

Three server-side defects, all filed in `tasks/backlog.md` — and none of them in
pymongo, which is the argument for running the other-language gauges at all:
`$geoIntersects` returning nothing, change-stream resume losing the original
read preference, and eight C-driver failures clustered in the connection-string
and server-selection surface.
