### The Rust server validates command arguments

A wrong-typed command argument was accepted without complaint, and the server
then did the wrong thing and reported success. `createIndexes` with a non-array
`indexes` answered `ok: 1` and created no index — a driver was told an index
existed that did not. An `update` whose `multi` was a document updated a single
document instead of all matches. A `findAndModify` whose `upsert` was an array
skipped the upsert. A `find` with a non-numeric `limit` returned everything.

Measured against a real MongoDB across 87 argument shapes, the server diverged
on 78 of them; it now matches on all 87, as the Python server already did.

A second group answered a generic `BadValue` where MongoDB has a specific code
for the stage in question — `$lookup`, `$group`, `$match`, `$sort`, `$limit`,
`$skip`, `$count` and `$unwind` each report their own code and message now. That
one had a structural cause worth naming: the shared engine signals "this
construct needs the Python implementation", which the Python server honours by
running it, and this server — having no Python — was reporting as a generic
error. Stages it can identify are now named before that point is reached.

Both servers are now at 87 of 87 on this sweep.
