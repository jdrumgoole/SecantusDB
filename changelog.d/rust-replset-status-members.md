### The C driver can finally exercise change streams

`replSetGetStatus` said this server was a standalone while `hello`, on the very
same connection, described a single-node replica set. Real MongoDB is never
both, and the disagreement had a cost: the C driver's test fixture reads the
member roster to decide whether replica-set behaviour is available, saw an
empty one, and skipped every change-stream test as inapplicable. The strictest
wire-protocol suite we run had no change-stream coverage at all.

`replSetGetStatus` now reports the same one-member primary that `hello` already
advertised. A server started without a replica-set name still answers as a
standalone, which is the honest reply for one.

Thirty-two change-stream tests run as a result, and four real defects came out
of them: the error for a pipeline that discards the resume token had the wrong
message, the error for a malformed pipeline stage had the wrong code and
message, and — the substantive one — a pipeline that *rewrote* the resume token
rather than removing it was accepted. MongoDB permits only transformations that
leave the token untouched, so a rewritten token now fails the same way a removed
one does, instead of reaching the client as a confusing driver-side error.

#### Fixed

- `replSetGetStatus` reports a one-member primary roster when a replica-set
  name is configured, agreeing with `hello`.
- A change-stream pipeline that modifies the resume token is rejected, not just
  one that removes it.
- The resume-token and pipeline-stage errors carry MongoDB's own codes and
  messages.
