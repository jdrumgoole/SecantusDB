### Change streams: two fatal errors that shared one wrong code, and events whose fields came out in the wrong order

Change streams had never been compared against a real MongoDB, because mongod
refuses them on a standalone server and the differential harness spawns a
standalone. Run against a single-node replica set, 14 of 41 cases disagreed.

#### Fixed

- A change stream configured with `fullDocument: "required"` or
  `fullDocumentBeforeChange: "required"` failed with code 280
  `ChangeStreamFatalError` when the image was not available. MongoDB answers
  that with code **47 `NoMatchingDocument`** and no error labels — a different
  condition, and a different code, from the 280 it uses when a pipeline strips
  the resume token. Both now match, message included. The two used to share one
  exception class, which is why one wrong code covered both; 280 is still
  correct for the stripped-token case and is asserted there by the driver spec
  suite.
- The stripped-token error itself was missing MongoDB's `Executor error during
  getMore :: caused by ::` prefix and the trailing `Expected: … but found: …`,
  which names the resume token that was expected and what the pipeline left
  behind. Dropping the token, rewriting it with `$literal`, and rewriting it
  with `$addFields` all now produce MongoDB's message exactly.
- Change events emitted their fields in the wrong order: `wallTime` came after
  `documentKey` instead of directly after `clusterTime`, and `fullDocument` was
  hoisted ahead of `_id` — so `_id` was not the first field. Field order is
  invisible to a document comparison, which is how nine event-construction
  sites drifted out of it without any test noticing. Events are now assembled in
  MongoDB's order in one place rather than nine, which took the ordering
  divergences from 28 cases to 5 — and the five that remain are all explained by
  a missing field that is recorded in the backlog, not by ordering.

All three are fixed on **both** servers, and both now diverge from MongoDB on
exactly the same remaining cases.

#### Known, recorded, not fixed

`updateDescription` still reports array edits in a shape MongoDB never
produces: we emit `truncatedArrays` where MongoDB reports either the whole new
array or the positional path of an appended element. This is measured across
eight mutations and four array sizes in the backlog. It is not fixed here
because the diff walk is mirrored in both engines and roughly nineteen
assertions pin the current behaviour — that is its own change, not a tail-end
addition to this one.

#### Added

- `tools/probes/change_streams.py`, the probe behind all of the above. It
  compares event contents *and* field order, and documents the replica-set
  requirement that kept this area unprobed.
