### The Rust MongoDB server holds DDL and admin commands to mongod's rules

`secantusd-rs` now refuses the database names, `create` options and command
arguments `mongod` 8.2.11 refuses, and reports what `mongod` reports from
`listCollections`, `dbStats`, `collStats` and `listDatabases`. A comparison
of 809 replies went from 268 different (of the first 670) to 3.

Several of the differences could leave a test passing on something `mongod`
would have stopped: any database name was accepted, `dropDatabase` dropped
the database whatever its argument, and `collMod` with `dryRun: true`
applied the change.

#### Fixed

- **Rust MongoDB server: an invalid database name is refused.** `a.b`,
  `a b`, the empty name and names with `/`, `\` or `"` were accepted, and a
  write created the database. A name with `$` is refused for writes, as on
  `mongod`.
- **`dropDatabase` takes `1`.** `0`, `2.5`, a string or null used to drop
  the database; they are refused (5255100, 14, 40414).
- **`renameCollection` checks both namespaces first**: an empty or dotless
  namespace, a rename onto itself (20), onto `system.x` (20) or onto a name
  past 255 bytes.
- **`create` parses its options before creating anything**:
  `validationLevel` and `validationAction` must be strings,
  `changeStreamPreAndPostImages` a document with a boolean `enabled`,
  `expireAfterSeconds` a number, `storageEngine` a known engine, `idIndex`
  a `{_id: 1}` key; `autoIndexId` is unknown; a namespace past 255 bytes is
  refused. A view keeps only what defines it.
- **`collMod`**: `dryRun: true` no longer applies the change; a null option
  is an option not given; `expireAfterSeconds` is refused on a collection
  that is not clustered; an index named by both `name` and `keyPattern` is
  refused; the reply ends with `ok`.
- **Converting an index to unique takes mongod's two steps.** `unique:
  true` is refused until `prepareUnique: true` has been set; the replies
  carry `prepareUnique_old` / `prepareUnique_new` and `unique_new`;
  `unique: false` is refused.
- **Dropping `system.views` drops every view.** It answered ok and left
  them in place.
- **`listCollections`**: no `ns` on `idIndex`, no `uuid` on a view, options
  in mongod's order, defaults left out; with `nameOnly` the filter sees the
  name and type only; an unknown operator in the filter is refused.

#### Changed

- **`dbStats`, `collStats` and `listDatabases` have mongod's fields, order
  and number types**: `views`, `avgObjSize`, `totalSize` and `scaleFactor`
  on `dbStats`; `numOrphanDocs`, `freeStorageSize`, `indexBuilds`,
  `totalSize` and `scaleFactor` on `collStats`; `totalSizeMb` on
  `listDatabases`. `scale` and `freeStorage` are honoured and checked. A
  database is `empty` when it holds no collection.
- **`dataSize`** is a command.
- An unknown field on `drop`, `dropDatabase`, `dbStats`, `collStats`,
  `listDatabases` or `renameCollection` is refused (40415).
- `listDatabases` outside `admin` is refused (13).
