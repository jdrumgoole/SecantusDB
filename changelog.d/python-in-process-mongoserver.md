### `pip install SecantusDB` runs the MongoDB server inside your Python process

`MongoServer()` from the PyPI package now runs the Rust MongoDB server in the
Python process, as SQLite does. There is no child process, but there is still
a real loopback TCP port, and pymongo connects to it as it would to `mongod`.
`MongoServer(embedded=False)` keeps the child-process launcher. The server
behaves the same either way: the daemon's single-node `secantus` replica set
(change streams and transactions work), test commands off, and the TTL
monitor on.

#### Added

- The `secantusdb-v*` release job builds the `_secantus_server` extension
  beside `secantusd-mdb` on Linux x86_64 and macOS arm64, from the same commit
  and the same WiredTiger build. MongoDB only (`--no-default-features`), LTO,
  no PGO yet. Before attaching it, the job imports it as
  `secantusdb._secantus_server` on Python 3.10 and 3.13, serves one document
  through pymongo, and checks its dynamic dependencies. It ships as
  `secantusdb-ext-<version>-<target>.tar.gz` with a `.sha256`.
- A `dry_run` input on the release workflow's manual dispatch: it builds and
  checks the archives and keeps them on the run, but attaches nothing to the
  Release.
- `publish-pypi.yml` builds two wheels per platform. The `cp310-abi3` wheel
  carries the extension. The `py3-none` fallback wheel has no extension and
  is what pip picks on free-threaded CPython, where `MongoServer()` raises
  `ServerStartError` naming `embedded=False` instead of starting a child
  process on its own. The smoke job installs from the wheels by name, as a
  user does, on CPython 3.12 and on free-threaded 3.13t, and asserts which
  wheel each one got.
- `secantusdb.ForkedServerError`. A forked child can reach the parent's
  in-process server through `uri`, and leaving the parent's `with` block in
  the child does nothing. An explicit `stop()` in the child raises, and the
  child never runs the native shutdown on the parent's WiredTiger.
- `RustServer(..., enable_test_commands=True)`: the in-process `MongoServer`
  passes `False`, mongod's default and the daemon's.
- `tests/test_pypi_inprocess_server.py` drives the launcher against the real
  extension in the storage-engine CI lane on every PR. It covers change
  streams, a transaction, test commands refused, stop and reopen in one
  process, two servers at once, the WiredTiger lock, a port in use, and a
  forked child that exits normally.

#### Changed

- `RustServer.stop()` now closes the store explicitly (WiredTiger's final
  checkpoint) and raises `RuntimeError` if the close fails, as the
  `secantusd-mdb` daemon makes a failed close its exit status (RM-189).
  Before, the store closed when its last reference dropped, and a failed
  checkpoint was only a line on stderr. The shutdown order is now the
  daemon's: the TTL/heartbeat thread, then the listener and connections,
  then the store. If a connection thread outlives the 10-second drain and
  still holds the store, `stop()` does not close WiredTiger under it. It
  raises instead, and the store closes when that thread finishes.
