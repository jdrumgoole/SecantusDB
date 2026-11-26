### Rust PG server: a busy backend's held statistics wait until it is idle

A backend that could not flush its statistics yet (under a second since its
last flush) holds them for the idle timer, and PostgreSQL turns that timer
off when it reads the next command, so the counts go out when the backend is
next idle. `secantusd-pg` never turned it off. The wire layer's message hook
opened each message cycle first, so the cycle-start branch that stops the
timer never ran, and another session's report published a busy backend's
counts mid-statement: `n_tup_ins` read 2 during a `pg_sleep(13)` where
PostgreSQL 15.20 reads 1.

#### Fixed

- The message hook now starts the cycle itself: it drops a stale cancel,
  stops the idle timer and moves the cycle count that keys the per-cycle
  catalog cache. That count had not moved either, and its `cycle start`
  trace line had never been written.

Fixes RP-1116 (tests/backlog/test_rp_busy_backend_stats.py::test_a_busy_backends_held_counts_wait_until_it_is_idle)
