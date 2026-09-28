### A `hint` on a collection that does not exist was rejected

mongod validates a hint during query **planning**, and there is nothing to plan
against when the namespace does not exist — so it accepts any hint there and
returns an empty result. This server refused it, on every command that takes a
hint.

Measured against mongod 8.2.11 (2026-09-28), across three collection states:

| collection | mongod | before |
|---|---|---|
| does not exist | `ok` | **BadValue (2)** |
| exists, empty | BadValue (2) | BadValue (2) |
| has documents | BadValue (2) | BadValue (2) |

#### Fixed

- Two sites, because reads and writes validate in different places: the storage
  layer's `resolve_hint` covers `find` / `count` / `aggregate` / `distinct`, and
  `validate_write_hint` covers `findAndModify` / `update` / `delete`. Fixing
  only the first left the three write commands still diverging, which is the
  sort of half-fix a single-command test would have missed.

#### Found by

mongo-c-driver's `/find_and_modify/hint`, which runs against a collection it
never creates. One failing driver test; **seven** diverging commands once the
siblings were probed.

#### Also

- The C gauge now spawns its daemon with `--standalone`, as the Java gauge
  already did. libmongoc's `MONGOC_TEST_URI` carries no `replicaSet=`, so its
  tests assert standalone semantics — four `/Client/select_server*` tests select
  with a SECONDARY read preference and assert
  `standalone_or_rs_secondary_or_mongos`, which our single-node replica-set
  persona answered with `RSPrimary`. With the flag they pass; without it they
  fail.
