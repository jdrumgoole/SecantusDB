### A `rename` change event now leads with `to`, as MongoDB's does

MongoDB puts `to` first on a `rename` change event — before `_id`, the only
event where the resume token is not the leading field. SecantusDB put it after
`ns`.

This was measured earlier and deliberately left alone, on the reasoning that
leading with `to` looked like an artifact of how MongoDB assembles that event
and might not survive a version change. Re-measured against MongoDB 8.2.11, the
version SecantusDB targets, it is identical — with and without
`showExpandedEvents`. Stable across two major versions is a contract, not an
artifact, so it is now replicated.

With this, the change-stream differential sweep is at **zero divergences across
all 41 cases and zero field-order differences**, on both servers, against a live
MongoDB 8.2.11.
