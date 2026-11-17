### A write of many through a clustered collection's index writes as it reads

An updateMany or deleteMany on a clustered collection whose plan reads one
field's index between bounds read the index's every RecordId before it
wrote a document, so one that ran out of `maxTimeMS` there wrote nothing,
where mongod 8.2 has written part of the collection. The store now reads
that index a page at a time and writes each page before reading the next.
Both servers link `secantus-storage`.

#### Fixed

- A write of many through a single-field index of a clustered collection,
  out of `maxTimeMS`, keeps what it wrote, as on mongod.

Fixes RM-390 in part (tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_write_of_many_out_of_time_keeps_what_it_wrote_whatever_its_plan); a compound index's read stays open (test_a_write_of_many_through_a_compound_index_out_of_time_keeps_what_it_wrote).
