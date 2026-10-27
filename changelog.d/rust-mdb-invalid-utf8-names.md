### Rust MongoDB server: names, messages and the other text types that are not UTF-8

The last of RM-222. mongod takes a database or collection name as bytes.
It quotes a string's own bytes in an error message, and it stores
JavaScript, a symbol, a regex, a DBPointer or JavaScript with a scope
whose text is not UTF-8. The Rust server refused all three. It now
answers as mongod does.

#### Fixed

- **Database and collection names that are not UTF-8** can be created,
  written, read, listed (`listCollections`, its filter included, and
  `listDatabases`), indexed, renamed, dropped and explained. Replies,
  `ns` fields, error messages and the oplog carry the name's bytes.
  - Inside the server such a name is held escaped, as a field name
    already was.
  - `secantus-wt` writes an escaped name to WiredTiger as its own bytes
    and reads it back escaped. Valid names are unchanged, so no store
    changes and the PostgreSQL server is unaffected.
  - A client's NUL in a name is refused (73) before any escape exists, so
    a string cannot forge one.
- **Change streams over such names.** mongod refuses a change stream on
  such a collection or database with PCRE2's verdict on its namespace
  regex (51091), and a database stream shows no event for such a
  collection. The Rust server does the same.
- **Error messages quote the bytes.** A message that quotes a string that
  is not UTF-8 (`$convert`, `$toObjectId`, `$toDate`, `$dateFromString`,
  …) now carries that string's bytes, where it carried U+FFFD.
  `$dateFromString`'s diagnostics name each byte, and an ObjectId's length
  is counted in bytes.
- **The other text types.** JavaScript, a symbol, a regex, a DBPointer and
  JavaScript with a scope whose text is not UTF-8 are stored and returned
  byte for byte, with their own `$type`.
- **A NUL in a field name taken from a string is refused.** The affected
  places are `$count` (40159), `$lookup` / `$graphLookup` `as`,
  `$unwind`'s `includeArrayIndex`, `$densify`, `$fill`, a field path
  (16411, with mongod's message), `$unwind`'s path (16419) and `$unset`
  (9527900). These used to be accepted, and since the field-name escape
  their NUL and hex digits could be written as other bytes.
- **A `$sort` on `_id` alone compares the whole value**, as mongod does:
  an array `$group` key sorts as an array, after the documents (RM-329).
- **A build of a text or hashed index over documents is refused again**,
  as the one-step build refuses it. The build path from #2191 skipped the
  check, and recorded such an index as built.

Fixes RM-222 (tests/mongod_live/test_invalid_utf8.py,
tests/backlog/test_rm_222_invalid_utf8.py::test_collection_name_that_is_not_utf8,
::test_error_message_quotes_the_bytes, ::test_other_text_types_that_are_not_utf8)
and RM-329 (tests/backlog/test_rm_222_invalid_utf8.py::test_sort_of_group_ids_that_are_arrays).
Files RM-330, RM-331 and RM-332.
