# Architecture

SecantusDB is two servers, `secantusd-mdb` (MongoDB wire protocol) and
`secantusd-pg` (PostgreSQL wire protocol), built from one Cargo workspace
under `crates/`. Each is layered outermost-in: a TCP accept loop (with TLS) on
a wire-protocol codec, on a command layer, on pure operator engines, on a
WiredTiger-backed storage layer. The wire layer never knows about commands,
the command layer never knows about SQL, and the pure engines take documents
in and out with no I/O.

## The MongoDB server

| Crate | Layer |
| --- | --- |
| `secantus-wire` | MongoDB wire framing: message header, `OP_MSG` (2013) and legacy `OP_QUERY` / `OP_REPLY` for the handshake. |
| `secantus-server` | The TCP accept loop and connection handling over `secantus-wire` and `secantus-commands`, with TLS. |
| `secantus-commands` | Command dispatch: the dispatch table, the error contract (`{ok: 0, errmsg, code, codeName}`, `CommandNotFound` for an unknown command), cursors, and the command handlers, including the planner and `explain`. |
| `secantus-auth` | SCRAM-SHA-256 (and SCRAM-SHA-1) mechanics, X509 name matching and role resolution. |
| `secantus-core` | The pure operator engines: query matcher, update operators, projection, the aggregation pipeline and expression evaluator, sort keys, geo and collation. No I/O, no WiredTiger. |
| `secantus-icu` | The ICU collator `mongod` links, so collation compares as `mongod` does. |
| `secantus-storage` / `secantus-storage-adapter` | The WiredTiger-backed `Storage` (collections, documents, indexes, oplog and pre-image tables, transactions, backup and point-in-time recovery) and its adapter to the command layer. |
| `secantus-wt` / `secantus-wiredtiger-sys` | Safe Rust bindings over the vendored WiredTiger C library, and the build of WiredTiger itself (static, from `vendor/wiredtiger`). |
| `secantusdb` | The `secantusd-mdb` binary and the `secantus-mdb` library (`secantus_mdb::Server`). |

`secantus-wire`, `secantus-commands`, `secantus-server`, `secantus-auth` and
`secantus-core` do not link WiredTiger, so they build and test on their own;
the crates that do link it are built from their own directories.

## The PostgreSQL server

| Crate | Layer |
| --- | --- |
| `secantus-pgwire` | SecantusDB's fork of `pgwire`: the PostgreSQL v3 wire protocol as a library. |
| `secantus-pg-query` | SecantusDB's fork of `pg_query`: PostgreSQL's own parser (libpg_query). |
| `secantus-pgplan` | Lowers PostgreSQL parse trees to the MQL filters `secantus-core` evaluates. |
| `secantus-pgcatalog` | The SQL catalog documents (`pg_catalog` and friends). |
| `secantus-pg` | The server: executor, catalogs, roles and authentication, TLS, and the `secantusd-pg` binary and `secantus_pg::PgServer` library. |

Because SQL is lowered onto the same engines and the same storage layer, the
PostgreSQL server inherits their index acceleration and type handling.

## Storage

Both servers share one storage format on WiredTiger, the same engine `mongod`
ships:

- **Documents are opaque BSON blobs**, keyed by a byte-sortable encoding of
  `_id` (so walking the table gives MongoDB's natural cross-type order).
  Filtering, projection, sorting and updates happen in the engines after
  decoding; the storage layer never inspects a document. Any lossy
  intermediate form (JSON, native column types) would show a client the
  difference for `ObjectId`, `Decimal128`, int32 versus int64, dates,
  `Binary` and regexes.
- **Secondary indexes are WiredTiger indexes over typed sort-key columns**
  derived from BSON values, not JSON and not coerced numerics. The byte order
  of the keys matches BSON's cross-type sort order, so numbers of different
  types that compare equal collide and order correctly.
- **The oplog and pre-image tables** back change streams and point-in-time
  recovery.

A data directory written by `secantusd-pg` opens in `secantusd-mdb` (a table
reads back as a collection, with the `PRIMARY KEY` as `_id`), but not at the
same time: WiredTiger's exclusive file lock makes the second server exit. The
reverse direction does not work, because `secantusd-pg` resolves names through
a catalog that only `CREATE TABLE` writes.

## Concurrency

Each accepted connection is served on its own thread over WiredTiger
sessions, so reads proceed in parallel and write throughput is
bounded by WiredTiger's own write path and the durability setting
(`--sync-on-commit`, the logged or non-logged data tables). See
[Concurrency](concurrency.md) for what scales and what does not, and
[Benchmark](benchmark.md) for measured numbers against `mongod`.

Tests of the servers run against real on-disk WiredTiger, never a mock.

The deprecated Python server had a different structure (a thread per
connection, a global storage lock); its layer map is kept, as a record, on
[Deprecated: Python servers](python-servers.md).
