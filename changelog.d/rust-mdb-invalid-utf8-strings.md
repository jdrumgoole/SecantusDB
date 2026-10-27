### Rust MongoDB server: strings that are not UTF-8 are kept, byte for byte

A BSON string is a length-prefixed run of bytes, and mongod keeps whatever
bytes it is sent: a document whose string holds `0xff 0xfe` is stored,
returned, compared, indexed and grouped like any other. The Rust server
refused it ("BSON error: utf-8 encoding error"), so data mongod keeps could
not be written, and a dump of it could not be restored. It now keeps those
strings exactly, and its operators read them as mongod does, byte by byte.

#### Fixed

- A string whose bytes are not UTF-8 is decoded into a carrier the `bson`
  crate cannot mistake for a string or write by accident (a one-field
  document keyed by a lone NUL, which no BSON key can be), and every encode
  in the server writes it back as the string's own bytes. The crate's own
  encoders refuse the carrier, so a path that missed the change fails loudly
  rather than storing other bytes; a new source check
  (`tests/test_rust_literal_bson_decode.py`) keeps the server on
  `bsondoc::write` / `to_vec`. A field name that is not UTF-8 is kept
  escaped (NUL plus two hex digits per invalid byte) and written back as its
  bytes.
- Such a string is a string everywhere it is looked at: `$type`, the
  canonical sort order (byte order, between the other strings), equality,
  `$in` and ranges, index keys (its bytes, so no index entry format changes
  and the PostgreSQL server's keys are untouched), `_id`, `$group`,
  `distinct`, `$jsonSchema` (`type`, `minLength` / `maxLength` counted as
  mongod counts), change streams, the oplog, `$out`, `$merge` and `$lookup`.
- A duplicate key that is not UTF-8 is reported as mongod reports it:
  `dup key: { s: "0xfffe" }`, `keyValue: {s: "fffe"}`, `hexEncoded: [true]`.
- The string operators follow mongod's byte rules: `$strLenCP` counts every
  byte that is not a continuation byte, `$indexOfCP` and `$substrCP` step by
  a lead byte's leading ones (40095, 34456-34459), `$toUpper` / `$toLower` /
  `$strcasecmp` are ASCII-only on the bytes, `$split`, `$replaceOne` /
  `$replaceAll`, `$trim` (5156304 / 5156305 for invalid `chars`),
  `$concat`, `$substrBytes`, `$binarySize`, `$toHashedIndexKey` and
  `$convert` work on the bytes, and inside `$group` the slot-based engine's
  own codes (5075307, 5073403) are given. A regular expression over such a
  string does not match in a query, and `$regexMatch` / `$regexFind` /
  `$regexFindAll` report PCRE2's verdict (51156); a pattern that is not
  UTF-8 is 51091 / 51111 with the same text.
- explain's index bounds show the string's bytes.
- `$setField` with a NUL in its field name is 9534700 and `$rename` to one is
  9867600, as on mongod, so a string cannot forge an escaped name.

Fixes RM-222 in part (tests/backlog/test_rm_found_2026_10_20b.py::test_insert_string_with_invalid_utf8,
tests/mongod_live/test_invalid_utf8.py). Left open in RM-222: collection
names, error messages that quote the bytes, and the other types that carry
text. Files RM-329 (a `$group` key that is an array sorts by its smallest
element).
