### The Rust server gains the MongoDB 8.0 features and an honest version

The Python server advertised MongoDB 8.2.11 and supported `bulkWrite`, `sort` on
`updateOne` / `replaceOne`, and change-event `nsType`. The Rust server had none
of them and still advertised 7.0, so which features you got depended on which
server you started. It now has all three and advertises the same version.

The sequencing is the same as the Python side's, and it is the point: the
advertised version is a capability contract, so it moved only once the features
it promises existed. Raising it first would have made drivers send `bulkWrite`
and receive `CommandNotFound`.

Each operation in a `bulkWrite` runs through the ordinary insert / update /
delete handler with the database rebound to that operation's namespace, so bulk
semantics cannot drift from single-write semantics — the same structure the
Python implementation uses.

Verified against a live MongoDB 8.2.11: the twelve-shape `bulkWrite`
differential agrees exactly, as it does for the Python server, and the pymongo
conformance gauge run against the Rust server sits at 99.4% with the same five
out-of-scope failures the Python server has.
