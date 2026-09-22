### The probe-store sweep no longer deletes a database that is still in use

The sweep that reclaims abandoned probe WiredTiger homes deleted a **running**
mongod's data directory. The directory had been named by hand with a shell's
`$$` — the shell's pid, not the server it launched — so the name pointed at a
process that had exited seconds later. The sweep saw a dead pid, believed it,
and removed the files underneath a live database, which died on a fatal
WiredTiger assertion: `log pre-alloc server error ... the process must exit and
restart`.

The pid in a store's name is written by whoever created the directory. It is a
hint, not proof of ownership, and nothing else was asked before deleting.

#### Fixed

- A dead pid is no longer sufficient grounds. A store is reaped only once it
  has also sat **untouched for half an hour** — a store being served is written
  to constantly, so a recent mtime means hands off, whatever the name claims.
  An abandoned store is still reclaimed on the next run after that, which is
  ample for a problem measured in days.
- `probe_store()` is documented as the only way to name one of these
  directories, since it fills in `os.getpid()` and cannot name the wrong
  process.

`WiredTiger.lock` looks like the better authority and is not, which CI proved
before this merged: POSIX advisory locks are held per **process**, so a check
made from the process that opened the store reports the file as free. That
passed on Windows — which locks mandatorily at the handle — and failed on
macOS, where the sweep then deleted a live store and took `WT_PANIC` through
the worker. Worse, merely opening and closing a descriptor to a file the
process holds an `fcntl` lock on *releases* that lock, so the "safe" probe can
break the database it is inspecting. The Windows `os.remove` probe is kept as
an extra gate there, where it is genuinely decisive.

The regression tests build stores directly rather than running a server, so a
future regression fails an assertion instead of panicking WiredTiger inside the
test worker. They were confirmed to fail with the guard removed.
