### Numbers compare by their exact value, and stores are re-keyed when they open

mongod compares a double with an Int64 or a Decimal128 by the double's exact
binary value. The Rust MongoDB server's matcher, and both servers' index keys,
compared it by its shortest decimal instead. So `find({a: 0.1})` matched
`Decimal128("0.1")`, `{a: {$lt: 0.1}}` missed it, and `_id: 2.0**62` was
accepted beside `_id: Int64(2**62)`. The index key also had a one-byte
exponent, so every number beyond 1e127, or below 1e-128 in magnitude, shared
one key per sign: with an index, `{a: {$lt: 1}}` lost `1e-300` and
`{a: {$gt: 1e200}}` lost `3e250`.

Numbers are now keyed by their exact value with a two-byte exponent (number-key
format 2), and the Rust server's matcher compares exactly. A store written
before is brought up to date the first time either server opens it. The `_id`
rows of documents with a numeric `_id` are re-keyed, the `_id` claims are taken
again, and every index without the new `numberKeys` stamp is rebuilt.

This change depends on a change to the Rust PostgreSQL server: its planner
lowered a `float8` compared with a `numeric` value on the old shortest-decimal
equality, so `where f = 0.1` over a `float8` column found nothing once numbers
compared exactly. That planner fix (casting the numeric side to `float8`, as
PostgreSQL does; RP-040) lands first.

#### Fixed

- `secantus-core`: a double compares with an Int64 or a Decimal128 by its exact
  value, and the int/double comparison is exact for every Int64, including
  ±2^63 (RM-152, `tests/backlog/test_rm_numeric_compare.py`).
- `sortkey` (both servers): every number is keyed in order, whatever its
  exponent; a double is keyed by its exact value.
- `$all: [null]` matches a missing field, and `$all: [[]]` an empty array:
  `$all` is an AND of equalities, as mongod parses it.
- `$mod` no longer matches a double whose truncation does not fit an Int64
  (`3e250`, `2^63`); it used to read such a double as `i64::MAX`.

#### Changed

- On-disk: number-key format 2. A store is re-keyed in place when it opens,
  recorded by an oplog-meta row (`numberKeys`) for the `_id` keys and claims,
  and a `numberKeys` stamp on every index (hidden from `listIndexes`).
- Two stored `_id`s that are equal by exact value but were keyed apart (the
  Int64 `2^62` and the double `2^62`): the first re-keyed keeps the key, the
  other stays readable by a scan, and the count is logged.
- A unique index whose stored values are equal by exact value cannot be
  rebuilt, and the store refuses to open, naming the index.
