### Rust MongoDB server: an SBE sort spills rows of values, as mongod's does

A sort mongod runs in its slot-based engine (one a `$group` is pushed down
behind) spills each record as two rows of SBE values, the key's and the
kept fields', in `MaterializedRow::serializeForSorter`'s format. This
server wrote the key as a classic `Value` and the fields as a BSON
document, so `sortSpilledBytes` and the spill file's size differed from
mongod's.

#### Fixed

- An SBE sort's spill records are now mongod 8.2's rows: a `size_t`
  count, then each value's `TypeTags` byte and value. A string of up to
  seven bytes is a `StringSmall`, a longer one a `StringBig`, a missing
  field `Nothing`, and the fields' slots go in name order. Over 300
  documents of a 1000-byte string under a 50000-byte budget, the spill is
  now mongod 8.2.11's 927900 bytes over 900 records (1031 a record, was
  1025). A compound key, a string key and every BSON type match mongod's
  `sortSpilledBytes` and `sortSpilledDataStorageSize` to the byte.
  Documents read back from a spill keep their fields' order.
  Fixes RM-506 (tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_spilled_bytes).
- Filed while working: RM-615 (a whole-document row leaves out the fields
  read beside it) and RM-616 (a missing field's slot counts no memory).
