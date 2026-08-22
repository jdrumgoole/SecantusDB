### `top` works on the Rust server

`mongotop` failed outright against the Rust server: `top` answered code 59
CommandNotFound, so the tool errored instead of rendering a table. The backlog
entry describing this said "counters are always zero", which read as though it
covered both servers and hid the fact that one of them did not implement the
command at all.

It does now, ported from `commands.py::_top` — one `totals` entry per namespace,
the `note` key mongo-tools skips, `total`/`readLock`/`writeLock` plus the per-op
sections each `{time, count}`, and the same code-13 refusal outside the `admin`
database.

The counters themselves are still zero on both servers: nothing instruments
per-namespace operation timing, so `mongotop` renders an idle server. That half
stays open and is recorded as such.

#### Fixed

- `top` on the Rust server returns the mongod-shaped reply instead of
  CommandNotFound, so `mongotop` runs against it. Covered by a
  `secantus-commands` unit test pinning the non-admin refusal.
