### `replSetStepDown`, and a hello that waits when it is asked to

SecantusDB advertises itself as a single-node replica-set primary and already
answered `replSetGetStatus` with a full status, but `replSetStepDown` was
`CommandNotFound` — so a driver could read the topology and not act on it. It is
implemented now on both servers, reproducing what a real single-node replica set
does: the command returns immediately, the node reports itself a secondary for
the requested period with `primary` and `electionId` dropped from `hello`,
writes are refused `10107 "not primary"` while reads keep working, and then it
is primary again.

Two halves of that were not guessable from the command's description. The
refusal has to carry `topologyVersion`, or the driver marks the server Unknown
and the very next *read* fails server selection. And the monitoring `hello`
stream has to push the change the moment it happens rather than waiting out
`maxAwaitTimeMS`, or the driver does not learn about it until its next heartbeat
and the refusal then looks newer than its whole view of the server.

Alongside it, a plain awaitable `hello` — `topologyVersion` plus
`maxAwaitTimeMS`, without `exhaustAllowed` — now holds its reply for the
requested budget instead of answering in a fraction of a millisecond. The
streaming `exhaustAllowed` form was already implemented; this is the other half
of the same protocol, and a driver that polls it was spinning.

Election *timing* is deliberately not reproduced: mongod's return to primary is
driven by its election machinery rather than the step-down period alone, and
modelling that means modelling election timeouts — the multi-node machinery this
project puts out of scope. The window here is exactly the period requested.

#### Added
- `replSetStepDown` on both servers, with mongod's refusals: `2 BadValue` for a period under `secondaryCatchUpPeriodSecs` or a negative one, `262` when a non-forced step-down has no electable secondary to hand over to, `10107` when already a secondary, and `76 NoReplicationEnabled` on a server not advertising a set.
- `hello` reports `secondary`, which mongod always includes in a replica-set reply and SecantusDB never emitted.

#### Fixed
- A plain awaitable `hello` holds its reply for `maxAwaitTimeMS` rather than returning at once.
- `maxAwaitTimeMS` without a `topologyVersion` is refused `31368`, as mongod refuses it; both servers used to accept it.
- `topologyVersion.counter` advances when the topology changes. It was pinned at 0 on the grounds that the topology never changes, which stopped being true here — and a frozen counter makes a step-down look stale to a driver.
