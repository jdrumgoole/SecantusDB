### The probe-store sweep asks the store, not the name, before deleting it

The sweep that reclaims abandoned probe WiredTiger homes deleted a **running**
mongod's data directory. The directory had been named by hand with a shell's
`$$` — the shell's pid, not the server it launched — so the name pointed at a
process that had exited seconds later. The sweep saw a dead pid, believed it,
and removed the files underneath a live database, which died on a fatal
WiredTiger assertion: `log pre-alloc server error ... the process must exit and
restart`.

The pid in a store's name is written by whoever created the directory, so it is
a hint and not proof of ownership. Nothing else was asked before deleting.

#### Fixed

- A store is reaped only when the **store itself** reports it is not in use.
  `_wt_home_in_use` takes `WiredTiger.lock` as the authority: a non-blocking
  exclusive `flock` on POSIX, and on Windows an attempted `os.remove`, which is
  what actually distinguishes the two states there — opening the lock with
  `r+b` succeeds while WiredTiger holds it and says nothing, measured on this
  box rather than assumed. Anything ambiguous answers "in use", because a store
  left behind costs disk and a store deleted too early costs data.
- `probe_store()` is documented as the only way to name one of these
  directories, since it fills in `os.getpid()` and cannot point at the wrong
  process.

The regression test stands up a real `SecantusDBServer` on a store named for a
dead pid and asserts the sweep leaves its files alone; it was confirmed to fail
("swept a live database") when the guard is removed.
