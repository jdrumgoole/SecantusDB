### The Rust server disables Nagle too

Mirror of the Python servers' `TCP_NODELAY` fix: the Rust server's
accept loop now calls `set_nodelay(true)` on every accepted connection,
closing the same ~40ms-per-round-trip delayed-ACK stall on Linux that
cost pgjdbc's chatty batch tests a 200x slowdown in CI against the
Python server. Best-effort (a failed setsockopt on a dying socket never
kills the accept loop), matching mongod's and PostgreSQL's own
unconditional NODELAY.
