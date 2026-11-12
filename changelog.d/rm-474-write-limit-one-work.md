### Rust MongoDB server: a write's LIMIT of one is done after one work

A write of one document read in its sort order (an update with a `sort`,
a findAndModify) now reports the works mongod 8.2.11 reports in its
profile entry's `execStats`: the LIMIT is at its end after the work that
returned its document, so the UPDATE and the LIMIT each do one work.

#### Fixed

- An update with a `sort` and a findAndModify reported `works: 2` for
  their UPDATE and LIMIT stages, and the UPDATE `needTime: 1`: the write's
  read counted the extra work `find`'s executor spends finding a LIMIT at
  its end, which a write's executor never does.

Fixes RM-474 (tests/backlog/test_rm_found_rm376.py::test_a_writes_limit_of_one_is_done_after_one_work)
