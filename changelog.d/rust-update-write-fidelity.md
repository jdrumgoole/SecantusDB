### The Rust server stores the zero you asked it to store

`{$set: {a: -0.0}}` over a stored `0.0` was silently dropped by the Rust
server. Its storage write guard asked `new != doc`, and Rust's `f64 ==` calls
the two zeros equal, so the write was skipped: `nModified` came back 0, no
oplog entry was emitted, no change-stream event fired, and reading the document
back gave the old zero. The value the caller asked to store was never stored,
and nothing reported a problem. The Python server was fixed for this a release
ago; the Rust storage layer kept the bare comparison, and nothing covered it —
the parity suites pin the pure operator engines, not storage.

The same batch closes the two ways the Rust server misdescribed an update it
refused. An `$inc` or `$mul` past int64 could only defer, and a defer has no
Python engine behind it on the Rust server, so five real overflow shapes told
the client `query uses a construct the Rust server does not support` — which
says the server cannot do `$inc`, when it can and it was the result that did
not fit. Separately, every execution-time update error came back bare: mongod
reports the failures that depend on the stored document under
`Plan executor error during <command> :: caused by ::` and leaves the parse
errors readable from the update spec alone unwrapped, and the Rust server had
the message bodies right and the wrapper on none of them.

All three were measured against mongod 8.2.11, and the Rust server now matches
it on every shape in the sweep that the Python server matches.

#### Fixed

- `secantus-storage`: an update whose only difference is the sign of a zero is
  stored and counted, instead of being silently skipped. The write guard is now
  `secantus_core::diff::doc_changed`, which falls back to the encoded BSON when
  `==` says the documents match — the same rule `storage._doc_changed` applies
  on the Python server, rather than a third copy of it.
- `secantus-core`: an `$inc` / `$mul` that overflows int64 reports mongod's
  `Failed to apply $inc operations to current value ((NumberLong)…) for
  document {_id: …}` (code 2) instead of a generic "not supported".
- `secantus-core` / `secantus-storage` / `secantus-commands`: execution-time
  update errors carry mongod's `Plan executor error during <command> :: caused
  by ::` wrapper, with the command name interpolated (`update` and
  `findAndModify` report their own), and parse-time errors stay bare. Ten
  wrapped and seven bare shapes probed against 8.2.11.
