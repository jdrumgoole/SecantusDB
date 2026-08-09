### The Rust server matches the Python one on the C and Ruby driver suites

Four more behaviours the Rust server was missing, found by regenerating the
gauges rather than reasoning about the code — every one of them was invisible
from the source and obvious from a single line of driver output.

`serverStatus` omitted its `connections` section entirely, so a driver asking
how many connections had been created got no answer. That is what the C
driver's exhaust-cursor tests were failing on all along: they open a cursor and
check that a connection was created, and the failure looked for all the world
like an exhaust-cursor bug. It took three passes to fix properly — the section
was missing, then present but the wrong integer width for a driver that
type-checks rather than coerces, then present and correctly typed but always
zero, which cannot satisfy a test asserting the count went up. It now reports
the server's real counters.

A capped collection's `$collStats` still didn't report its bounds, because the
values arrive as 32-bit integers and were read as 64-bit only. And
`listIndexes` accepted a negative `batchSize` instead of rejecting it, which is
the deliberate failure a Ruby session spec uses to check that errors surface.

#### Fixed

- `serverStatus` reports `connections` (with live counts), `opcounters` and
  `network`.
- `$collStats` reports `maxSize` / `max` for a capped collection regardless of
  the integer width the driver used.
- `listIndexes` rejects a negative `batchSize` rather than accepting it.
