# Python launcher API

`pip install SecantusDB` installs the `secantusdb` package: a small launcher
that starts the bundled `secantusd-mdb` / `secantusd-pg` binaries as
subprocesses. It is the only Python API the wheel provides; the servers
themselves are Rust.

```python
from secantusdb import MongoServer, PostgresServer, ServerStartError, binary_path
```

## `MongoServer` and `PostgresServer`

```python
MongoServer(storage_path=None, *, port=0, startup_timeout=60.0)
PostgresServer(storage_path=None, *, port=0, startup_timeout=60.0)
```

| Argument | Meaning |
| --- | --- |
| `storage_path` | The WiredTiger data directory, created if absent and kept after `stop()`. With `None`, a temporary directory is used and removed on `stop()`. |
| `port` | TCP port; `0` (the default) picks a free one. |
| `startup_timeout` | Seconds to wait for the server to report that it is listening. |

Both bind `127.0.0.1`, are context managers (`with MongoServer() as server:`),
and have the same members:

| Member | Meaning |
| --- | --- |
| `start()` | Start the subprocess and wait for it to listen; returns the server. Raises `ServerStartError` if it exits or stays silent past `startup_timeout`. |
| `stop()` | Terminate the subprocess (killing it if it does not exit) and remove a temporary store. |
| `host`, `port` | The address the server reported once started. |
| `storage_path` | The data directory in use. |
| `output` | The server's most recent output lines, for diagnosing a failure. |
| `uri` (`MongoServer`) | A `mongodb://` connection string for `pymongo` or any MongoDB driver. |
| `dsn` (`PostgresServer`) | A `postgresql://postgres@host:port/postgres` string for `psycopg` or any PostgreSQL driver. |

The launcher passes only the port and the storage path. For anything else
(authentication, TLS, the WiredTiger cache, ...) run the daemon yourself with
the flags in [Configuration](configuration.md) and the
[Rust server docs](https://secantusdb.com/docs/rust/running.html).

## `binary_path(name)`

Returns the path of a bundled binary, `"secantusd-mdb"` or `"secantusd-pg"`;
raises `ValueError` for another name and `FileNotFoundError` if the install has
no binary for the platform.
