Title: secantus-pg 0.1.0-beta.16: an UPDATE that changed nothing, and functions that disagreed with PostgreSQL
Date: 2026-10-14 18:00:00
Slug: secantus-pg-0-1-0-beta-16
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.16 fixes answers that were wrong without an error. A table whose composite primary key had a column named like "$date" or "$regex" could report an UPDATE as done and change nothing, return NULL for the key, or fail every read. It also fixes pg_lsn arithmetic and filters, ts_headline's options and its JSON form, and five more functions that disagreed with PostgreSQL 15.

Every fix in this release is to an answer that was wrong without an error.
Each was found by running the same statement on this server and on
PostgreSQL 15.19, and each is now checked against PostgreSQL's answer.

## Column names that looked like something else

The server stores a row as a BSON document. A composite primary key is a
sub-document keyed by the column names, so a key over columns called
`"$date"` and `"$x"` is stored as `{"$date": ..., "$x": ...}`.

The library we read documents with treats a sub-document with keys like
those as extended JSON, a text form of a typed value, and turns it into the
value it stands for. So a key named like a date came back as a date, or not
at all. Depending on the names:

- an `UPDATE` reported its row count and changed nothing;
- a `SELECT` returned NULL for the key columns;
- or every read of the table failed.

Foreign keys over the same columns went wrong the same way. Rows are now
decoded as the bytes they are, and the filters that find a row by its key
compare the names literally. A probe of 130 scenarios over such names went
from 60 that differed from PostgreSQL to 7, and those 7 are one feature
the server refuses with an error: selecting a field from a composite-typed
column, `(v).*`.

`json` and `jsonb` were never affected: they are stored as text, so a value
like `{"$numberLong": "1"}` comes back as written.

## pg_lsn

A `pg_lsn` column could not be compared with a quoted literal:
`where l = '0/16'` matched no rows, and `set l = l + 1` failed. Arithmetic
with a fractional number did not round as PostgreSQL does, and out-of-range
results, `NaN` and infinity were not refused with PostgreSQL's errors. All
of those agree now.

## ts_headline

`ts_headline`'s options were ignored, so `StartSel=<` still marked a match
with `<b>`. `StartSel` and `StopSel` now apply. The options that choose
which words to show (`MaxWords`, `MinWords`, `ShortWord`, `MaxFragments`,
`HighlightAll`) are refused with an error naming the option, rather than
ignored; doing them properly means porting PostgreSQL's word selection.

Over `json` and `jsonb`, `ts_headline` marked up a document's keys as well
as its values. It marks only the string values now, and answers a
document of the type it was given.

## Five more functions

These disagreed with PostgreSQL 15 in the function-coverage sweep and agree
now:

- `?#` and `#` between two line segments, and `<->` between a line and a
  point, are transcribed from PostgreSQL's geometry code: two segments on
  one line do not intersect, and the distance agrees to the last digit;
- `"char"` converts to and from `int4` as a byte (`"char"(65)` is `A`, and
  `200` is out of range), reads the first byte of non-ASCII text, and prints
  a byte above 127 as `\ooo`;
- `array_agg` over arrays refuses a NULL array, an empty first array and
  arrays of different shapes, where it failed or answered a ragged array;
- `pg_relation_is_publishable` is true only for a user table that is not
  temporary, and NULL for an oid that names no relation;
- a built-in called by its operator's name, such as `int4pl(1.5, 2)`,
  refuses an argument PostgreSQL would not convert, where it computed an
  answer.

## rustls 0.23.46, in both servers

Two rustls advisories were published on the day of this release,
RUSTSEC-2026-0346 and RUSTSEC-2026-0349. Both are in rustls's TLS client,
and neither of our servers makes a TLS client connection, so neither was
reachable through them. The released binaries are built from our committed
lockfiles, though, and those named the affected version. This release and
secantus-mdb 0.5.3-beta.195, cut alongside it with no other change, are
built with rustls 0.23.46.

## The checks that ran

- All 261 statement corpora against PostgreSQL, one at a time: the same 13
  statements differ as before this release, so nothing moved backwards.
- The function-coverage record, now 198 statements with PostgreSQL 15.19's
  answers beside them: none differs. CI runs it on every change.
- The release workflow's smoke test, through a restart, on both binaries
  before they were attached.
- `cargo install secantus-pg --version 0.1.0-beta.16` and `cargo install
  secantus-mdb --version 0.5.3-beta.195` from crates.io, each built with
  rustls 0.23.46, and a query against what each built, including the
  composite-key `UPDATE` above.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.16` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.16)
