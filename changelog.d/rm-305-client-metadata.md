### A connection's handshake is in the log as mongod writes it

The Rust MongoDB server's log now records a connection's handshake the way
mongod 8.2 does. A driver's first `hello` writes `client metadata`, with the
driver's document, the address it came from and the compressors the two
sides settled on. The first command after the handshake writes `Received
first command on ingress connection`, with the milliseconds since the
connection opened or since its last `hello` or authentication step. Both
lines are on the connection's own thread, with mongod's ids, so a log reader
or a `getLog` check that knows mongod's lines finds them here too.

#### Fixed

- `hello` / `isMaster` with a `client` document writes mongod's
  `client metadata` line (NETWORK 51800), after compression is negotiated
  and before a `saslSupportedMechs` refusal, as mongod does.
- The first command after the handshake writes mongod's 6788700 line once
  per connection. `hello`, `isMaster` and the authentication commands
  restart its clock, and a command neither server knows is passed over.
  Fixes RM-305 in part (tests/backlog/test_rm_001_028.py::test_client_metadata_is_logged);
  the startup lines stay open under RM-305, and the parse-failure case
  is filed as RM-386.
