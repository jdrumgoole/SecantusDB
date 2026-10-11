### Change streams on the Rust MongoDB server, checked command by command against mongod

A comparison of 115 scripted change-stream scenarios with `mongod` 8.2.11
(`tools/probes/change_stream_commands.py`): what `$changeStream` accepts,
every event's shape, `getMore`, invalidation, resuming, the three scopes,
pre-images and transactions. The previous release answered 111 of them
differently; this one answers 5 differently.

#### Fixed

- **A write in a transaction is reported without its transaction.** Its
  events now lead with `txnNumber` and `lsid`, as `mongod`'s do.
- **A resume token that is not found resumed anyway.** `mongod` re-reads
  the token's event and refuses (280, `NonResumableChangeStreamError`)
  when it is not the first one seen: a token from another collection, or
  a pipeline that filters its event out. The server started after the
  token's position whatever it named.
- **`startAtOperationTime` in the future delivered everything**, and one
  far in the past delivered nothing when the oplog was long.
- **A stream on the target of `renameCollection ... dropTarget` saw
  nothing**; it sees the rename and is invalidated. The replaced target no
  longer gets a `drop` event of its own.
- **A cluster-wide stream reported writes to `admin`, `config` and
  `local`.**
- **After a stream ended, its cursor could still be read**, and a
  `getMore` with cursor id 0 found a cursor.
- **An insert that created its collection produced no `create` event**
  under `showExpandedEvents`. It does now, and so does an upsert.
- **`collMod`'s `modify` event listed options the command did not give**
  (`validationLevel`, `validationAction`); `createIndexes` and
  `dropIndexes` events left an index's options (`unique`) out and had
  `mongod`'s field order wrong.
- **A small event carried `splitEvent: {fragment: 1, of: 1}`** under
  `$changeStreamSplitLargeEvent`. `mongod` adds the field only to an event
  it split.

#### Changed

- **The `$changeStream` spec is checked.** An unknown field (40415), a
  wrong type (14), a bad `fullDocument` or `fullDocumentBeforeChange`
  value (2), `resumeAfter` with `startAfter` (50865), a token whose
  `_data` is not a string (40647) or not hex (9), `allChangesForCluster`
  anywhere but `admin` with no collection (72), and a stream on `admin`,
  `local`, `config` or a `system.*` collection (73) are refused with
  `mongod`'s messages. A stage `mongod` knows but does not allow after
  `$changeStream` is "not permitted" (20); `$changeStream` after the first
  stage is 40602; `$changeStreamSplitLargeEvent` must be last (7182802)
  and needs a change stream (20).
- **The cursor document's field order is `mongod`'s**:
  `postBatchResumeToken` before `id`, and `invalidated: true` on the batch
  that ends a stream. A 280 error leads with its `errorLabels`.
- **Resume tokens are upper-case hex**, as `mongod`'s are. A token from an
  earlier release still resumes.
