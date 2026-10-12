Title: secantus-mdb 0.5.3-beta.185 refuses the database names and DDL arguments mongod refuses
Date: 2026-10-12 18:00:00
Slug: secantus-mdb-0-5-3-beta-185
Author: Joe Drumgoole
Category: Releases
Tags: release

Summary: secantus-mdb 0.5.3-beta.185 refuses invalid database names, makes dropDatabase take only 1, stops a collMod dry run from applying its change, and gives dbStats, collStats and listDatabases mongod's fields.

If your tests create and drop databases or collections on `secantusd-rs`,
upgrade. Several things in this release could leave a test passing on
something `mongod` would have stopped.

- **Any database name was accepted.** `a.b`, `a b`, a name with a slash,
  the empty name: a write created the database. `mongod` refuses all of
  them, and this server does now, in `mongod`'s words.
- **`dropDatabase` dropped the database whatever it was given.**
  `{dropDatabase: 0}`, a string or null all dropped it. Only `1` does now.
- **`collMod` with `dryRun: true` applied the change.** A dry run is now
  refused unless it is the index conversion `mongod` allows one for, and it
  changes nothing.
- **Dropping `system.views` left every view in place** and answered ok. It
  drops them, as on `mongod`.
- **`renameCollection` accepted namespaces it should not**: an empty one,
  one with no database, a rename of a collection onto itself, and a rename
  onto `system.x`.

We found these by sending the same DDL and admin commands to the server and
to `mongod` 8.2.11 and comparing every reply: `create` with every option it
takes, `listCollections`, `collMod`, `renameCollection`, `drop`,
`dropDatabase`, `dbStats`, `collStats`, `dataSize` and `listDatabases`.
There are 809 replies. The previous release differed on 268 of the first
670. This one differs on 2.

**`create` checks its options before it creates anything.**
`validationLevel` and `validationAction` must be strings.
`changeStreamPreAndPostImages` must be a document with a boolean `enabled`.
An unknown storage engine, `autoIndexId`, and a namespace longer than 255
bytes are refused. A view keeps only what defines it: `capped` or a
validator given with `viewOn` is not stored.

**Converting an index to unique takes two steps**, as on `mongod`.
`collMod` with `index: {unique: true}` is refused until `prepareUnique:
true` has been set on the index. The replies say what changed
(`prepareUnique_old`, `prepareUnique_new`, `unique_new`), and `unique:
false` is refused.

**The catalog reads as `mongod`'s does.** `listCollections` no longer puts
`ns` on `idIndex` or a `uuid` on a view, lists options in `mongod`'s order,
and leaves out options at their defaults. With `nameOnly` its filter sees
only the name and the type.

**The stats commands have `mongod`'s fields.** `dbStats` reports `views`,
`avgObjSize`, `totalSize` and `scaleFactor`. `collStats` reports
`numOrphanDocs`, `freeStorageSize`, `indexBuilds`, `totalSize` and
`scaleFactor`, and is refused on a view. `listDatabases` reports
`totalSizeMb` and runs only against `admin`. `scale` is honoured and
checked. `dataSize` is a command. The byte counts themselves are this
server's own: it stores documents differently from `mongod`.

What this release does not do: `listDatabases` lists `admin` and `config`
only once something has been written to them; `dbStats` reports the
filesystem sizes as zero; and a clustered collection's `expireAfterSeconds`
is stored and listed, but nothing expires.

We ran the pymongo, Go and Node driver suites against this code. Each gave
the same result as the release before.

This is a server for tests. It is single-node and it is a beta.

`cargo install secantus-mdb --version 0.5.3-beta.185` builds it from crates.io.
Binaries for Linux x86_64, macOS arm64 and Windows x86_64 are on the release.

[Rust MongoDB server](https://secantusdb.com/rust-db.html) ·
[secantus-mdb on crates.io](https://crates.io/crates/secantus-mdb) ·
[MongoDB binaries](https://github.com/jdrumgoole/SecantusDB/releases/tag/secantusdb-v0.5.3-beta.185)
