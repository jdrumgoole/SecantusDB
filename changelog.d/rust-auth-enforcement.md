### The Rust MongoDB server enforces privileges as mongod does

Under `--auth`, what a user may run was decided by a short hand-written list of
roles and a check that named the database and never the collection, so a role
with `insert` on `ae.c1` could insert into every collection of `ae`, and a
command missing from the list -- `explain`, `bulkWrite`, `validate`, `collMod`,
`fsync`, `killAllSessions` and more -- needed no privilege at all. Measured
against mongod 8.2.11 with 66 users holding one role each, 3,869 of 12,053
answers differed on 0.5.3-beta.192. 388 differ now.

The check is now mongod's: a user's effective privileges are the union of
every role it holds and every role those inherit, built-in roles from the
generated table and stored roles from storage, matched against the resource
each command works on by mongod's own pattern rules (an exact namespace, a
database, a collection name on every database, every normal collection of
every database but `local` and `config`, the cluster, any resource). Every
command the server has says what it needs, arguments included: an upsert needs
`insert`, `findAndModify` needs `find`, a pipeline needs what its stages need,
`explain` needs what it explains, `bulkWrite` is checked per op, a view needs
`createCollection`, a user may view and change its own record, and a cursor is
read and killed only by the user that opened it. `listCollections` with
`authorizedCollections` and `listDatabases` filter their answer instead of
refusing. A command the table does not name is refused. Of the 388 answers that
still differ, none is about who may run a command: 324 are five commands the
server does not have (`dbHash`, `logRotate`, `connPoolStats`,
`setDefaultRWConcern`, `setFeatureCompatibilityVersion`), and the rest are
commands both servers allow whose handlers answer differently.

#### Changed

- `crates/secantus-commands`: `rbac.rs` is the privilege model (`Authz`,
  mongod's resource search list, a per-connection cache invalidated by every
  user and role command); `authz.rs` and `authz_pipeline.rs` are what each
  command needs. The hand-written role sets are gone. `getMore` and
  `killCursors` know the cursor's user; `listCollections` and `listDatabases`
  filter by privilege.
- `tools/probes/auth_enforcement.py` is the measure, and
  `crates/secantus-storage-adapter/tests/auth_enforcement_wt.rs` pins its
  answers.
- `tests/test_rbac_pipeline.py` holds the Rust server to mongod's answers: a
  `$lookup` needs `find` on the collection it reads, and `configureFailPoint`
  needs no privilege.
