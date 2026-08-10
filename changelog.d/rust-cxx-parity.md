### A change stream stops forgetting where it got to

Reading to the end of a change stream threw away the position it had reached.
While events were arriving, the stream reported each one's position faithfully;
the moment a read came back empty it replaced that with a bare positional
marker — one that named neither the collection nor the document last seen. A
client that then reconnected resumed from something less precise than it had
already been told, and the token it had been carefully tracking went backwards.

The position now only ever moves forward. An idle stream still advances as the
server's clock does, so a quiet collection doesn't strand a reader behind the
oplog window, but it never rewinds past an event already delivered.

Separately, `$currentOp` did not report which application a connection belonged
to, so tools that look up their own operation — by the `appName` given in the
connection string — found nothing to inspect.

Both were invisible until now: the C++ driver's suite is the one that covers
them, and it had never been run against this server because its tests bind a
fixed port.

#### Fixed

- A change stream's resume position no longer regresses to a positional marker
  when a read returns no events.
- `$currentOp` reports `appName` and the connection's driver metadata.
