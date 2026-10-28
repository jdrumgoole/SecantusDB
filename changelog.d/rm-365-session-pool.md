### Rust MongoDB server: reads cost less than before operation counting

Outside a transaction, every storage call an operation made opened a
WiredTiger session and closed it, and since RM-023 each close also read the
session's statistics: a `find_one` makes several such calls. The storage
now keeps idle sessions in a pool (closed with the store, before its
connection), as mongod keeps a session cache. A session returns to the pool
only with no transaction and no checkpoint read on it, after giving the
running operation what it counted; one parked while nothing counted is
cleared before an operation that counts uses it.

Release builds, alternated round by round against the build before RM-023
(#2208): `find_one` by `_id` -12.8%, an index-range `find` -38.5%,
`count_documents` -11.4%, `update_one` +1.8%. Fixes RM-365 (a latency
budget, no gap test: the A/B in the PR).

#### Changed

- `secantus-storage`: autocommit operations take their WiredTiger session
  from a pool of up to 64 idle sessions instead of opening one per call.
- `secantus-wt`: `Session::reusable` says whether a session can be handed
  on.
