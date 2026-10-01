### Change events and error replies on the Rust MongoDB server match mongod

Three new probes compare the Rust MongoDB server with mongod 8.2.11: random
write sequences watched through a change stream, every update operator's
`updateDescription`, and the full reply to 58 real failures. Everything they
found is fixed, including several writes that stored nothing or the wrong
thing.

#### Fixed

- `$set` through an array index past the end (`b.0.c` on `[]`) creates the
  element instead of silently writing nothing.
- A positional update whose query names the array itself (`{a: 2}` with
  `$set: {"a.$": 9}`) works; it was refused.
- `updateMany` keeps the documents it updated before a failure, as mongod does.
- `multi` with a replacement document, a delete `limit` other than 0 or 1, a
  duplicate index under a new name, an index with an empty key or no name, and
  collection names mongod refuses are now refused instead of carried out.
- Change events: no `fullDocumentBeforeChange` on inserts; `updateDescription`
  reports what each update operator touched, as mongod does; pipeline updates
  use mongod's own diff and become `replace` events when mongod's would.
- Error replies: duplicate-key messages in mongod's form with `keyPattern` and
  `keyValue`, and mongod's code and message for unknown operators, invalid
  regexes, unmatched positional updates, unused array filters, index and
  collection DDL conflicts, bad sort specifications, negative `skip` / `limit`,
  `aggregate` without `cursor`, and `renameCollection` outside `admin`.

#### Added

- `tools/probes/change_stream_fuzz.py`, `tools/probes/update_description.py`
  and `tools/probes/write_error_replies.py`.
