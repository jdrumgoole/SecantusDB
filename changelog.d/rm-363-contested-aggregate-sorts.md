### Rust MongoDB server: a contested aggregate that sorts counts what it examined

An `aggregate` whose `$match` two indexes contest, followed by a `$sort`, now runs mongod's trial through the plan cache, reads the
winner's trial results first and goes on from there, so its slow-query line
counts the keys and documents the winner examined (133 and 133 for the
measured case), as mongod 8.2.11's does.

#### Fixed

- RM-363 in part: a contested sorting `aggregate`'s line now
  carries `keysExamined` and `docsExamined`. The grouping (SBE), time-series,
  geo, `count`, `distinct` and write cases stay open.

Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_contested_aggregate_that_does_not_stream_counts_what_it_examined[stage0])
