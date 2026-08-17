### The ltree type

`ltree` columns and casts now work: stored as a validated dotted label path
(alphanumeric/underscore labels), reported on the wire at oid 90010 — the
stable placeholder CockroachDB uses for the extension type, mirroring
citext's 90008 — with ParameterDescription inference for INSERT targets and
for unknown parameters compared against an ltree column, and the binary
parameter/result format PostgreSQL's extension uses (a version byte
followed by the text). The pgtest `ltree` corpus file pins the whole
exchange byte-for-byte and is now green.

#### Added
- The `ltree` type: text storage with label validation, oid 90010, binary
  codec, parameter inference.
