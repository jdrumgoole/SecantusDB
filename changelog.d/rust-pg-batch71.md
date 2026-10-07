### The Rust PostgreSQL server gives each connection its own thread

The Rust PostgreSQL server now runs every client connection on a thread of
its own, the way PostgreSQL gives every connection a backend process. A
statement runs directly on the thread that owns its client, instead of being
handed off from a shared runtime worker to a blocking thread first, and a
connection that waits (on a lock, in `pg_sleep`) holds up only itself.

Profiling eight inserting clients showed the server's time going to locks,
not work: the per-statement hand-off contended on the async runtime's
blocking-pool mutex, and every table lookup took one process-wide catalog
cache lock. With both gone, INSERT throughput at 8 clients (one table each,
no journal sync) rose from about 45k to about 61k statements a second, and
scaling from 2.8x to 3.6x of one client.

#### Changed

- `secantus-pgserver`: each accepted connection runs on its own OS thread with
  a one-worker runtime of its own (`server::accept_loop`). Synchronous statement
  work goes through `blocking_wait`, which runs it in place on a connection's
  runtime and uses `block_in_place` elsewhere. A notice sent mid-statement
  still hands the worker off so the socket keeps being driven.
- `secantus-pgserver`: the table-definition cache (`lookup_inner`) and the
  committed-catalog cache (`committed_cached`) keep a per-thread copy of what
  the shared cache holds as current, so a hit takes no shared lock.
