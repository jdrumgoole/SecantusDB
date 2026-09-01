### `maxTimeMS` actually times operations out

`maxTimeMS` was parsed and validated exactly the way MongoDB validates it — and
then ignored. The operation ran to completion and answered `ok` where MongoDB
aborts it with `MaxTimeMSExpired`. That is invisible on a fast operation, which
is why it survived: a sweep at a one-second budget shows nothing, because
nothing takes a second. At a two-millisecond budget it shows up everywhere.

A deadline cannot be a parse-time check, because the thing being bounded is
elapsed time inside the handler. It is now a thread-local budget armed around
the command and polled from the loops whose length tracks the data: the storage
scan and its predicate pass, `count`'s own scan, the write commands' candidate
selection, the index build, and the aggregation pipeline between stages. An
expired budget answers `50 MaxTimeMSExpired`, and `createIndexes` wraps it in
MongoDB's index-build envelope, both reproduced from a probe of 8.2.11.

Enforcement is cooperative and polls once every 64 documents, so an operation
can overrun by up to that many — MongoDB's own enforcement is interrupt-point
based and has the same property. `getMore` is deliberately excluded: there
`maxTimeMS` is the `awaitData` wait budget, and arming a deadline would make
every tailable poll report a timeout the moment it waited out its budget.

The write path polls candidate *selection* only, which happens before anything
is written, so an expired budget leaves nothing half-applied.

#### Added

- `secantus/deadline.py`: the thread-local budget, armed by `dispatch`.

#### Fixed

- `commands.py` / `storage.py` / `aggregate.py`: `maxTimeMS` is enforced on
  `find`, `count`, `distinct`, `aggregate`, `update`, `delete` and
  `createIndexes`.
