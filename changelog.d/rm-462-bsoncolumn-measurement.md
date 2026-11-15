### Rust MongoDB server: a time-series measurement holding BSONColumn data is refused

A measurement whose data field is, or holds at any depth, a binary of
subtype 7 is now refused as mongod 8.2.11 refuses it, instead of stored.

#### Fixed

- Inserting such a measurement answers `12506300`, `time-series insert
  failed: <ns> :: caused by :: Timeseries collections cannot store
  BSONColumn data`, and stores nothing. The meta field is not checked, as
  mongod appends only data fields to a column.

Fixes RM-462 (tests/backlog/test_rm_462.py::test_a_measurement_cannot_hold_a_bsoncolumn)
