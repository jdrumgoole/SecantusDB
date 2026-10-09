### One log sync now covers every commit already written

When several clients commit at once and each commit is synced (`fdatasync` on
Linux), WiredTiger recorded a sync as covering only the commit of the thread
that made it. The sync had flushed every commit written before it began, but
the next thread synced again all the same. A sync is now credited with
everything written before it started, as PostgreSQL credits a WAL flush.

#### Changed

- A new WiredTiger patch, `cmake/patch_wt_fsync_group.py`, applied by the
  wheel's build and the `secantus-wiredtiger-sys` crate. It changes
  `method=fsync` only: the Rust PostgreSQL server on Linux, and either
  MongoDB server when it is run with a sync per commit. Measured on Linux
  with the Rust PostgreSQL server, durable writes at eight and sixteen
  clients are 10 to 18 percent faster; one to four clients are unchanged.
- A commit is still acknowledged only after a sync that began after its
  bytes were written. Checked by hard-rebooting a server under sixteen
  remote writers six times: no acknowledged row was missing.
