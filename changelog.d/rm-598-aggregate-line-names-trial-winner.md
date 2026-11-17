### A contested `aggregate`'s line names the plan its trial chose

The Rust MongoDB server's `Slow query` line and profile entry for an
`aggregate` whose plan was chosen by trial now name the plan that ran, as
mongod 8.2.11 does, rather than the plan the catalog alone would put first.

#### Fixed

- An `aggregate` of `{$match: {x: {$lt: 30}, y: 1}}` over indexes on `x` and
  `y` ran and cached `IXSCAN { x: 1 }` but its line said `IXSCAN { y: 1 }`.
  Fixes RM-598 (tests/backlog/test_rm_found_rm363.py::test_a_contested_read_names_the_plan_its_trial_chose)
