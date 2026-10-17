### Rust MongoDB server: change events over 16 MB split as mongod splits them

#### Fixed

- **`$changeStreamSplitLargeEvent` splits as mongod 8.2 does.** Every
  field but `_id` goes into exactly one fragment, taken in order of the
  field's BSON size and then its name, and packed until the next field
  would take the fragment past 16 MB less the resume token. Each fragment
  has its own resume token, carrying its number. The fragments used to
  copy every small field into each one and give each large field a
  fragment of its own, so for a 7 MB `$set` on a 7 MB document with
  `fullDocument: "updateLookup"`, the first fragment carried `fullDocument`
  where mongod's carries `updateDescription`.
- **A stream resumes from a fragment's token**, with the fragments after
  it and then the events that follow, as on mongod.

Fixes RM-079 (tests/backlog/test_rm_057_084.py::test_an_event_over_16mb_is_split,
tests/backlog/test_rm_057_084.py::test_resume_from_a_fragment).
