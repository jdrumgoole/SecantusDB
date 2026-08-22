### `replSetGetStatus` now agrees with `hello` on the Python server

SecantusDB advertises itself as a single-node `secantus` replica-set primary so
that drivers accept change streams. The Python server said so in `hello` — and
then, asked `replSetGetStatus`, replied "not running with --replSet". Real mongod
is never both, and drivers notice: libmongoc's test framework counts the member
roster to decide what kind of server it is talking to, saw nothing, concluded
standalone, and ran standalone-only tests against a server presenting itself as a
replica set.

The Rust server fixed this months ago; the Python server never got the port. It
does now, gated the same way — with a set name configured, `replSetGetStatus`
reports the one-member PRIMARY roster matching `hello`; started with
`replica_set_name=None` it is a genuine standalone and the honest
`NoReplicationEnabled` error stands.

#### Fixed

- `replSetGetStatus` on the Python server reports a one-member PRIMARY roster
  consistent with `hello`, instead of the standalone `NoReplicationEnabled`
  error, whenever a replica-set name is configured. Ported from
  `crates/secantus-commands/src/handshake.rs`.
- Two mongo-c-driver gauge failures on the Python server —
  `/Client/last_write_date_absent` and its pooled variant — now report `skip`
  rather than failing. They are standalone-only tests that libmongoc should never
  have run against us, and it only did because the roster was empty.
