### Binary int2vector results encode as int2 arrays

Requesting an `int2vector` column (pg_index's indkey/indoption) in binary
result format now yields PostgreSQL's wire form — an int2 array with
element oid 21, 2-byte elements, and lower bound 1 — where previously the
text rendering leaked through the binary format. Binary pgwire clients
decoding index metadata get well-formed arrays. The pgtest `int2vector`
corpus file pins the encoding byte-for-byte (its expected indoption VALUE
is CockroachDB's NULLS-FIRST 2 where PostgreSQL — and SecantusDB — report
0; recorded as an expected divergence).

#### Fixed
- Binary-format int2vector results carried text bytes.
