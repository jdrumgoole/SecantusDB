### Tailable cursors wait, rewritten resume tokens are refused, and `_id` leads again

Three unrelated fidelity gaps, each found by mongo-php-library's suite
asking a question no unit test had thought to ask.

A `TAILABLE_AWAIT` cursor is supposed to park on the server until data
arrives or `maxAwaitTimeMS` expires. SecantusDB's capped-collection
tailables returned in about a fifth of a millisecond, because the wait's
wake condition was keyed on a change-stream position counter that plain
tailables never maintain — leaving it permanently satisfied. Clients
polling a capped collection were spinning instead of waiting.

A change-stream pipeline may not tamper with an event's `_id`: that field
is the resume token, and an altered one silently breaks resumption. The
server already rejected a pipeline that *removed* it, but a pipeline that
*rewrote* it passed straight through, and the error surfaced client-side
in the driver rather than from the server. Both are fatal now, as they are
in mongod.

Finally, a replacement-style update put the preserved `_id` at the end of
the stored document rather than the front. BSON keeps field order on the
wire, so the bytes a client got back differed from mongod's for the same
operation — invisible until something compared raw documents, which is
exactly what the PHP codec tests do.

#### Fixed

- `getMore` on a capped-collection tailable cursor with `awaitData` now
  blocks for up to `maxTimeMS` instead of returning immediately.
- A change-stream pipeline that modifies (not only removes) an event's
  `_id` now fails server-side with `ChangeStreamFatalError`, matching the
  Rust server, which already did this.
- Replacement updates place `_id` first in the resulting document, on both
  the Python and Rust engines.

The mongo-php-library gauge goes from 42 failures to 1 — and the one that
remains is a text-index test, a feature that is explicitly out of scope.
