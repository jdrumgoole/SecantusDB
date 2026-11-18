### A write of many through a compound index pages its read into the write

On a clustered collection, an updateMany or deleteMany whose filter is
several operator clauses no index covers whole (`{a: {$gte: 0}, b: {$gte:
0}}` over index `a_1_b_1`) read every RecordId of the chosen index before
writing; out of `maxTimeMS` there it wrote nothing, where mongod 8.2 writes
part-way. The store now reads that field's index a page at a time, writing
each page first.

#### Fixed

- Fixes part of RM-390 (tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_write_of_many_through_a_compound_index_out_of_time_keeps_what_it_wrote).
