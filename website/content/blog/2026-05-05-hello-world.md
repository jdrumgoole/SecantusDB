Title: Hello from SecantusDB
Date: 2026-05-05
Slug: hello-world
Author: Joe Drumgoole
Category: Updates
Summary: Welcome to the SecantusDB blog — what the project is, what it isn't, and where it's heading next.

Welcome to the **SecantusDB** blog. SecantusDB is a real MongoDB server
written in Python, backed by the same WiredTiger engine MongoDB ships
with. It speaks the MongoDB wire protocol on a real TCP socket, so
`pymongo`, `mongo-go-driver`, `mongosh`, and `mongodump` all connect
unchanged. Within single-node scope, your driver can't tell us apart
from `mongod`.

## What it's for

- **Tests.** Spin up an embedded server in a fixture, run your suite,
  tear it down. No Docker, no ports to manage, parallel-test friendly.
- **Dev loops.** Local development without standing up a `mongod`
  process. Default storage is on-disk WiredTiger, so your data
  survives a restart.
- **Single-node prototypes.** Small services that don't need a real
  cluster but want a real document database with real durability.

## What it isn't

SecantusDB is **not** a replacement for production MongoDB. It does
single-node only by design. Replica sets, sharding, real cluster
topology, and anything that depends on multi-node behaviour are out of
scope. We advertise ourselves as a single-node `secantus` replica-set
primary in the `hello` reply so `pymongo`'s topology machinery accepts
change streams, but the topology is fictional &mdash; there is no other
member.

## Status: alpha

We're in early development. The Python API surface (CLI flags, public
class signatures) is still settling and may shift between point
releases. The on-disk format is WiredTiger's, and our schema on top of
it (collection / index / oplog tables) has been stable across
releases. There's no migration tool yet, so don't put production data
here.

## What's next

More driver-conformance coverage, performance work on the aggregation
pipeline, and richer index acceleration (multi-field sort, collation).
Follow along on [GitHub](https://github.com/jdrumgoole/SecantusDB) or
read the [docs](https://secantusdb.readthedocs.io/).
