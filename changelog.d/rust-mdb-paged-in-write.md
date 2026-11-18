### A write of many through an `$in` writes as it reads

The Rust MongoDB server's updateMany / deleteMany on a clustered collection
whose filter is one field's `$in` (or `$eq`) of plain scalars now reads that
field's index a page at a time and writes each page before reading the next,
as mongod 8.2 writes each document its plan returns. Out of `maxTimeMS`
part-way it keeps what it wrote, where it wrote nothing before.

#### Fixed

- Fixes part of RM-390 (tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_write_of_many_through_an_in_out_of_time_keeps_what_it_wrote). The store crate is shared: the PostgreSQL server links the change.
