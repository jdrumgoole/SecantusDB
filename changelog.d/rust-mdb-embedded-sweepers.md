### The embedded Rust server expires TTL documents

`secantus_mdb::Server`, the MongoDB server you start from a Rust test, now runs
the same background sweepers as the `secantusd-rs` daemon. A TTL index expires
documents every 60 seconds, as on mongod. Before this, documents in an embedded
server never expired. The noop oplog heartbeat, which keeps a quiet change
stream's resume token advancing, is available and off by default, as on the
daemon.

#### Added

- `Builder::ttl_sweep(Option<Duration>)` sets the TTL sweep interval (default
  60 seconds, `None` to disable). Set a shorter one in a test that waits for
  an expiry.
- `Builder::noop_heartbeat(Option<Duration>)` sets the noop oplog heartbeat,
  which also prunes the oplog (default off).
