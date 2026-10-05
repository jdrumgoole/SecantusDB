### Rust MongoDB server: decimal sort directions, write namespaces and arrayFilters

A decimal sort direction sorted the wrong way on the Rust MongoDB server.
Writes could also create collections mongod refuses. Both now match mongod
8.2.11, along with the rest of sort-spec, arrayFilters and write-reply
validation.

#### Fixed

- `find` with a `Decimal128` sort direction returned the wrong order:
  `sort: {a: Decimal128("-1")}` sorted ascending. Sort directions now follow
  mongod's rule in `find` and in the aggregation `$sort` stage. A double is
  truncated and a decimal is rounded half to even.
- An insert, update, delete, findAndModify or createIndexes into a name mongod
  refuses now fails with code 73 and mongod's message. That covers `a$b`,
  `system.foo`, `system.views`, `system.profile`, and a namespace over 255
  characters. An insert used to create such a collection.
- A bad `$sort` key, value or `$meta` spec now gets mongod's own error code,
  at the top level and inside `$facet`, `$lookup` and `$unionWith`. These used
  to report "stage not supported", and `{a: 1.5}` was refused although mongod
  accepts it.
- Invalid `arrayFilters` get mongod's code and message on `update` and
  `findAndModify`. A non-document filter fails the whole command, as on mongod.
- On a replica set, insert, update and delete replies now carry `electionId`
  and `opTime`. All write replies order their fields as mongod does.

#### Added

- `tools/probes/write_and_sort_validation.py` covers 84 shapes against mongod.
