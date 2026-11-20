### Change-stream resume tokens are mongod's own

A change-stream resume token from the Rust MongoDB server is now the token
mongod 8.2.11 makes: `_data` is the hex KeyString (V1) of `{clusterTime,
version: 2, tokenType, txnOpIndex, fromInvalidate, uuid, eventIdentifier,
fragmentNum?}`, with `_typeBits` beside it when mongod includes one. Built from
the same inputs, our token is mongod's bytes; a high-water mark
(`postBatchResumeToken` of a batch that ended on no event) is mongod's
`tokenType: 0` token. The server reads such tokens whichever server made
them, so a stream resumes from a token mongod issued (and, once the server
opens mongod's store, mongod from ours). An oplog position is a timestamp, so
the token's `clusterTime` is the position and the old private fields (`s`, `n`,
`k`, `x`, `f`) are gone. A token written by an earlier build is refused with
mongod's answer for a token it cannot decode; no token is stored anywhere.

A resume finds the event the way mongod does: from the token's time, the event
with its `txnOpIndex`, collection and identifier must be the first the stream
reads (a fragment of a split event is found by its `fragmentNum`), else the
stream fails with `cannot resume stream; the resume token was not found`. A
high-water mark names no event: the stream reads the entries at or after its
time. A new stream's mark is the time after the oplog's last entry, and one
that has read on is the last entry it read; a resume whose token is the last
entry read returns that token.

#### Added

- `tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json`
  (`tools/probes/resume_tokens_fixture.py`): tokens of every kind of event,
  captured from mongod 8.2.11 (`int`, `long` and `double` `_id`s and their type
  bits, `txnOpIndex` in a batch and a transaction, every DDL event, both
  `invalidate`s, split fragments, high-water marks, collection, database and
  cluster scope), with what each holds. `crates/secantus-storage/tests/resume_tokens.rs`
  decodes them with this repo's KeyString decoder and rebuilds them byte for
  byte.
- `tests/mongod_live/test_resume_tokens.py`: the same writes on both servers
  make equal tokens (the cluster time and UUIDs masked), and a resume from each
  token, or from one that names no event of the stream, ends the same.

#### Changed

- `secantus-storage`: `ResumeTokenData`, `make_resume_token`,
  `parse_resume_token` are mongod's form; the `invalidate` event's token is the
  triggering event's with `fromInvalidate`; a token holds the collection UUID
  of its entry and, for a command, its `operationDescription` whether or not
  the stream shows it.
- `secantus-commands`: a resume reads from the token's time and expects its
  event; a `startAtOperationTime` and a high-water mark read from their time.
  A high-water-mark token resumes a single-collection stream whatever its
  `uuid` (mongod, 8.2.11); an event token without one is refused with 260.

Closes the "resume-token cross-server identity" scope note of the change-stream
section of the backlog. Files RM-733 (`collMod` of an index: its event) and
RM-734 (`dropIndexes: "*"`: order and spec), found by comparing the identifier
of the tokens.
