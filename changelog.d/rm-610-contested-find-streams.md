### A contested find, or a write of one, tries its plans over what they reach

A `find` or a single-document `update` / `delete` whose query two indexes
contest read every document the query matched before running mongod's plan
trial, so its cost grew with the collection where mongod's stops at what the
read needs. The trial now runs the filters itself over only what each
candidate's scan reaches, as a contested write of many has since RM-390.

#### Fixed

- A contested `find(...).limit(n)` and `updateOne` on the Rust MongoDB server
  cost the same over 10,000 documents as over 100,000, as on mongod 8.2.11.

Fixes part of RM-610 (tests/backlog/test_rm_found_2026_11_13_contested_reads.py::test_a_contested_find_and_write_of_one_cost_what_they_return); RM-610 stays open for `explain`.
