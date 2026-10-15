### The Rust MongoDB server speaks wire compression

`secantusd-rs` accepts `OP_COMPRESSED` with `snappy`, `zstd` and `zlib`,
mongod's default set, so a driver configured with `compressors=`
compresses its traffic as it does against mongod 8.2.11. A pymongo run
through each compressor gave the same results, and the same counts of
compressed frames in each direction, as mongod.

#### Added

- `hello` / `isMaster` answer a client's `compression` list with the ones
  this server speaks, in the client's order, first in the reply. The list
  belongs to the connection and is replaced by the next one sent.
- A compressed request is answered under its compressor, on every frame
  of an exhaust stream too. A frame whose declared size is out of bounds
  or does not match its data, or that names a compressor not enabled,
  closes the connection, and the declared size is checked before anything
  is allocated.

#### Fixed

- An empty `speculativeAuthenticate` in `hello` is refused after
  `saslSupportedMechs` and the awaitable checks, in mongod's order.
- Fixes RM-039 (`tests/backlog/test_rm_029_056.py::test_hello_names_compressors`;
  `crates/secantusdb/tests/compression.rs`).
