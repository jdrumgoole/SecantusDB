### Four probes were only ever asking half the question

Every differential probe here asks "does SecantusDB match mongod?" — and SecantusDB is two servers. Five probes only ever asked the Python one. That is how the aggregation stage corpus came to hide 219 Rust divergences until a `PROBE_SERVER` column was added to it last week.

`tools/probes/_servers.py` is now the shared `probe_targets()` helper, and `update_operators`, `arg_types_documents`, `update_path_conflicts` and `findandmodify_shapes` all compare both servers. It found **21 divergences on the Rust server, 0 on the Python one**.

#### Fixed — the Rust server accepted malformed writes and reported success

- **A non-document `q`, or a non-document/array `u`, on `update` / `delete`** fell through every match arm: the statement applied nothing and answered `ok`. mongod refuses the command (14 for a bad filter, 9 for a bad update, 40414 for an absent one, and 14 for a non-document element inside a pipeline-form `u`). 12 shapes.
- **`findAndModify` with `remove` alongside `new` or `upsert`**, or with an `update` that is neither a document nor a pipeline, **ran the delete** where mongod refuses the command outright.

#### Fixed — reply shape

- A remove's `lastErrorObject` carried **`updatedExisting`**, which mongod reports only for an update. Drivers read that field by field.
- `findAndModify`'s remove+update message said "both update and remove=true"; mongod says "both an update and remove=true".

#### Fixed — a defer that should have been an answer

- `$rename` with the same source and target deferred, which on the standalone Rust server reports "a construct the Rust server does not support" for an ordinary bad argument. It now names it, as mongod does.

Two shapes remain, recorded in `tasks/backlog.md`: the Rust update path has no parse-time / execution-time distinction, so it omits mongod's `Plan executor error during update :: caused by ::` wrapper. Code and message body already match.
