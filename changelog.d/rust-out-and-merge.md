### Rust MongoDB server: `$out` and `$merge` checked against mongod

`$merge` on the Rust MongoDB server stored a different document from
mongod's. With `whenMatched: "merge"`, the default, it merged sub-documents
field by field: `{o: {p: 1}}` merged into `{o: {q: 2}}` gave `{o: {q: 2, p:
1}}`. mongod sets each top-level field whole and stores `{o: {p: 1}}`. The
stage is now written with the `update` command, as mongod writes it, and
stores what mongod stores.

Found by sending 376 `$out` and `$merge` pipelines to the server and to
mongod 8.2.11 and comparing each reply and what the source and target held
afterwards. 191 of the first 327 differed; 5 of 376 do now
(`tasks/backlog.md` 7.079).

#### Fixed

- `whenMatched: "merge"` sets each top-level field whole. A field name
  with a dot in it is a path, and new fields are stored in the order
  `$set` stores them.
- `$out` and `$merge` inside `$facet`, `$lookup` or `$unionWith` are
  refused. They ran, and wrote to the target.
- A write the target refuses no longer stops the others: every document of
  the batch is attempted and the first failure is reported.
- The target's validator is asked about each document as it is written.
  One failing document used to stop all of them from being written.
- A `whenMatched` pipeline no longer inserts a document the validator
  refuses, and sees the stage's `let` variables.
- `whenMatched: "delete"` is refused; mongod has no such mode. So are the
  pairs of modes mongod does not have.
- The `on` fields need a unique index that is not partial and has the
  pipeline's collation.
- A target that does not exist is not created by a pipeline that writes
  nothing.
- `$out` refuses a target name no collection can have, a system
  collection, and a document an insert would refuse, and leaves the target
  as it was.
- A field set to `$$variable.path` that leads nowhere is removed. It was
  set to null, in `aggregate` and in a pipeline update.
- A replacement cannot change an `_id` to one a collation holds equal.
- `{$set: {$c: 2}}` over a document whose `$c` is already 2 is accepted.
- `listIndexes` shows a clustered index's `expireAfterSeconds`.
- A duplicate key reported for a whole command carries `keyPattern` and
  `keyValue`.
