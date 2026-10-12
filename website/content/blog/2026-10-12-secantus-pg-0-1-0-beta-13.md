Title: secantus-pg 0.1.0-beta.13: every built-in function, checked against PostgreSQL
Date: 2026-10-12 23:00:00
Slug: secantus-pg-0-1-0-beta-13
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-pg 0.1.0-beta.13 fixes wrong answers that no test had asked for. A new probe calls every built-in function in PostgreSQL 15's catalog on PostgreSQL and on the Rust PostgreSQL server. Of the 1,935 functions it can call, 619 answered the same before this release and 1,439 answer the same now. `sum` over an interval, `substr` over a `bytea` and `mod` over a numeric were among those that did not.

The Rust PostgreSQL server is checked against PostgreSQL with files of SQL,
each statement run on both and the answers compared. Before this release
those files held 9,584 checks and the server differed on 13. That reads as
nearly done, and it measured less than it looks: a file of SQL holds the
statements somebody thought to write.

## Asking the catalog instead

PostgreSQL lists its own functions in `pg_proc`. The new probe reads that
list on PostgreSQL 15.19, builds one call for each function from a sample
value for each argument type, and runs the call on both servers. It calls
only functions PostgreSQL marks immutable or stable, so nothing it runs can
change anything.

| | Functions called | The same as PostgreSQL |
| --- | --- | --- |
| 0.1.0-beta.12 | 1,935 | 619 |
| 0.1.0-beta.13 | 1,935 | 1,439 |

One sample per type is a thin test of any one function. It is a good test
of whether the function is there at all, and it found answers that were
wrong.

## What was wrong

Each of these returned an answer, with no error, so a test that did not
compare with PostgreSQL would not have seen it.

| Statement | Before | PostgreSQL, and now |
| --- | --- | --- |
| `sum(i)` over two intervals, `1 day` and `2 days` | `00:00:00` | `3 days` |
| `avg(i)` over the same | NULL | `1 day 12:00:00` |
| `substr('\x0102030405'::bytea, 2, 2)` | two bytes of the value's debug text | `\x0203` |
| `'\x616263'::bytea LIKE '\x6125'::bytea` | NULL | true |
| `mod(7.5, 2)` | 1 | 1.5 |
| `div(9.9, 0.3)` | division by zero | 33 |
| `32767::smallint + 1::smallint` | 32768 | error 22003, `smallint out of range` |
| `to_tsvector('{"title": "cats", "n": 5}'::jsonb)` | `'5':4 'cat':2 'n':3 'titl':1` | `'cat':1` |

`md5`, `overlay`, `btrim`, `ltrim`, `rtrim` and `trim` over a `bytea` had the
same fault as `substr`: the text function ran over a rendering of the
bytes. `smallint` arithmetic was also typed `integer`, and `array_agg` over
an array column was typed `varchar`.

## What was missing

- `regexp_match`.
- `regexp_replace` with a start position and with the number of the match
  to replace.
- `OVERLAPS`, over dates, times and timestamps.
- `bit_xor`, `bit_and` and `bit_or` over bit strings, and `string_agg` over
  `bytea`.
- `date + time`.
- `get_bit`, `set_bit` and `bit_count` over `bytea`.
- `num_nulls`, `num_nonnulls`, `parse_ident`, `convert_to`, `convert`,
  `multirange(range)`, `trunc(macaddr)`.

Most of the move from 619 to 1,439 is one mechanism. Every PostgreSQL
operator is a function too: `1 + 2` runs `int4pl(1, 2)`, and a statement may
call it by that name. The server had the operators and not the names. A
call by such a name is now rewritten into its operator before planning, for
738 functions read from PostgreSQL 15's `pg_operator`.

## Two changes you can meet

Both are PostgreSQL's behaviour, and both turn an answer into an error.

- **A regular-expression option that is no option is refused.**
  `regexp_replace(s, 'a', 'b', 'z')` ran before, ignoring the `z`. It is
  now error 22023, `invalid regular expression option: "z"`. The same holds
  for `regexp_like`, `regexp_count`, `regexp_instr`, `regexp_substr`,
  `regexp_matches`, `regexp_split_to_array` and `regexp_split_to_table`.
- **`smallint` arithmetic overflows.** A result outside -32768 to 32767 is
  error 22003 where it used to be a number.

## What is still not the same

The probe's count after this release, of the 1,935 functions it calls:

| | Functions |
| --- | --- |
| The same as PostgreSQL | 1,439 |
| Refused by this server | 428 |
| A different error, or an answer where PostgreSQL has an error | 60 |
| A different value | 8 |

Of the 428 refused, 57 are functions a statement calls by name, among them
`unnest` over a multirange, `ts_debug`, `table_to_xml` and
`json_populate_record` in a select list. The other 371 sit behind an
operator, a type's input and output, an index or an aggregate. Some of
those operators are themselves missing: the multirange positional
operators, bitwise operators on `macaddr`, and the `~<~` family of text
pattern comparisons.

The eight different values are known and not fixed in this release. They
include `pg_lsn + numeric`, which answers a number where PostgreSQL answers
a `pg_lsn`, and `ts_headline` over a JSON document.

The probe cannot call another 722 functions, because they take a type it
has no sample for, and it leaves out 325 that report the server's own
state. Nothing here is a statement about those.

## The checks that ran

- The corpora: 261 files, 9,737 checks, 13 differences. They are the same
  13 as before the release, and the 153 checks added for this release have
  none.
- psycopg 3.3.4's own test suite, unmodified, on macOS: 5,544 pass and none
  fail of 5,731, in the clear and again over TLS.

A new CI lane runs psycopg's suite on Linux, in the clear and over TLS,
each week. On its first runs the suite's NOTIFY timing tests failed on the
hosted Linux runner: each expects a notification within 25 to 50 ms and
saw it 55 to 80 ms late. They pass on macOS. We have not yet established
whether that is the shared runner or the server on Linux.

This is a server for tests. It is single-node, and it is a beta.

`cargo install secantus-pg --version 0.1.0-beta.13` builds it from crates.io.
Binaries for Linux x86_64 and macOS arm64 are on the release.

[Rust PostgreSQL server](https://secantusdb.com/rust-pg.html) ·
[secantus-pg on crates.io](https://crates.io/crates/secantus-pg) ·
[PostgreSQL binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusd-pg-v0.1.0-beta.13)
