# Plan: connection-handling thread pool

> **NOT STARTED — check the scope decision before picking this up (audited
> 2026-08-20).** No pool exists; `SecantusDBServer` still runs one daemon thread per
> connection (CLAUDE.md describes it that way, and `src/secantus/server.py` has no
> `ThreadPool`). Unlike most stale plans here this one was never delivered under
> another name — but its motivation is Python-server scalability, and
> `tasks/backlog.md` records that **the Python server is not a perf target;
> throughput and latency work goes to the Rust server.** Treat as an open design
> document, not queued work.

Branch: `claude/server-thread-pool-arch-dwbeR`

## Goal

Replace the unbounded "fresh `threading.Thread` per accepted connection" model in
`SecantusDBServer` with a **bounded, reused pool** of connection-handling threads,
without changing any wire-protocol behaviour observable by `pymongo`. The concrete
payoff is eliminating per-connection thread *creation/teardown* cost under connection
churn (the canonical case: driver spec-runners — mongo-rust-driver especially — open
and close on the order of a thousand short-lived connections per run).

## Non-goals

- **Higher request throughput.** `Storage` serialises every public method behind a
  single global `RLock`, so request *processing* has essentially no parallelism today
  and a thread pool cannot add any. This plan is about resource management and
  connection-handling cost, not speed. (Write concurrency is a separate effort —
  see `tasks/wt-concurrency-plan.md`.)
- **Flat thread count for many idle connections.** That requires a reactor
  (`selectors`) + request-dispatch pool — see "Option B (rejected)" below. Out of
  scope here.
- **Changing the `max_connections` reject-at-cap contract.** A client beyond the cap
  must still see a clean refusal (closed socket / EOF), never an indefinite queue/hang.
- **TLS / mTLS changes.** The handshake stays on the accept thread, exactly as today.

## Architecture today

`src/secantus/server.py`:

- `start()` spawns one daemon accept thread (`secantus-accept`) running
  `_serve_forever` (server.py:208-211).
- `_serve_forever` loops on `self._socket.accept()`. Per accepted socket it:
  1. Checks `_active_conns` under `_active_conns_lock`; if `>= max_connections`,
     closes the socket and continues (reject-at-cap, server.py:239-254).
  2. Otherwise increments `_active_conns`, does the TLS handshake (when configured)
     on *this* accept thread (server.py:260-269), sets the idle timeout, and
  3. **Creates a brand-new `threading.Thread(target=self._handle_client, ...)` and
     starts it** (server.py:275-280). The thread is discarded when the connection
     closes.
- `_handle_client` runs a blocking read loop (`read_message(conn)`) for the whole
  connection lifetime, dispatching each request and writing the reply. On exit its
  `finally` decrements `_active_conns` and **resets the thread-local WiredTiger
  session** (`self.storage._reset_thread_session()`, server.py:446-447) so WT's
  session pool (default 1024) isn't leaked under churn.

Two properties constrain the design:

1. **Storage is RLock-serialised** → no processing parallelism to gain.
2. **Handlers are long-lived and mostly blocked** on `read_message`, and
   change-stream `getMore` deliberately blocks on `Storage._oplog_cv` for up to ~1s.
   A handler therefore *occupies its thread for the connection's lifetime*, not just
   for one request.

## Chosen approach: Option A — bounded reuse pool (one connection per worker)

Keep the thread-per-connection *shape* (a worker owns a connection for its lifetime,
blocking I/O unchanged), but **draw the worker from a reused pool instead of
`Thread(...).start()`**.

### Mechanism

- Add a pool to `SecantusDBServer`, created in `start()` and shut down in `stop()`:

  ```python
  from concurrent.futures import ThreadPoolExecutor
  self._pool = ThreadPoolExecutor(
      max_workers=self.max_connections,
      thread_name_prefix="secantus-conn",
  )
  ```

- In `_serve_forever`, replace the `Thread(...).start()` block with:

  ```python
  self._pool.submit(self._handle_client, conn, addr)
  ```

  Everything above it (cap check, TLS wrap, idle timeout) is unchanged.

### Why this is correct and bounded

- The existing **reject-at-cap check guarantees we never have more than
  `max_connections` submissions in flight at once.** Because `max_workers ==
  max_connections`, the executor's internal queue is therefore *never* used —
  every `submit` gets an immediately-available (new or reused) worker thread. No
  request ever sits queued behind a long-lived handler, so the starvation failure
  mode of a too-small fixed pool cannot occur.
- Under churn, `ThreadPoolExecutor` **reuses** idle worker threads instead of
  creating a fresh OS thread per connection — the concrete win. Threads are created
  lazily up to the high-water mark and then retained for reuse (the executor does
  not reap idle threads; acceptable, even desirable, for a churny test workload).
- The WT-session reset in `_handle_client`'s `finally` (server.py:446) becomes
  *load-bearing rather than merely hygienic*: a pooled thread now serves many
  connections in succession, so resetting the thread-local WT session between
  connections is what prevents one connection's WT read snapshot / cursors from
  leaking into the next connection that reuses the same thread. This call already
  exists and already runs on every exit path — no change needed, but the test plan
  must lock the behaviour in (see below).

### Shutdown ordering (the one subtlety)

`stop()` currently: sets `_stop_event`, closes the listen socket (unblocks
`accept()`), joins the accept thread, then `storage.close()`. With a pool we must
also tear the pool down, and ordering matters because handler threads call into
`storage`:

1. `self._stop_event.set()`
2. Close the listen socket + join the accept thread (no new work can be submitted).
3. `self._pool.shutdown(wait=False, cancel_futures=True)` — don't block on
   handler threads. Handler loops already poll `_stop_event` and have a bounded
   socket idle timeout, but a handler parked in a tailable `getMore` can sit on
   `_oplog_cv` for up to the per-call wait (~1s); we must not deadlock `stop()`
   behind it. Handlers are daemon-equivalent (the pool threads must be daemon — see
   risks), so process exit isn't blocked, and `stop()` stays responsive.
4. `self.storage.close()` as today.

Open question to resolve in implementation: whether to additionally signal
`_oplog_cv` on stop so parked `getMore` handlers wake promptly instead of waiting
out their ~1s timeout. Today they wait it out (the accept-thread join already
tolerates this via its 2.0s timeout). Probably leave as-is for parity, but note it.

### Risks / things to verify

- **Daemon-ness.** Current handler threads are `daemon=True`, so a hung handler never
  blocks interpreter exit. `ThreadPoolExecutor` worker threads are **non-daemon** by
  default. To preserve today's "server threads never block process exit" property,
  either (a) accept that `_pool.shutdown(wait=False)` plus explicit `stop()` in tests
  is sufficient, or (b) construct the pool with a custom daemon thread factory. Python
  3.12 (the pinned version) does not expose a public daemon knob on
  `ThreadPoolExecutor`; option (b) means subclassing or setting
  `_threads`/`initializer` tricks, which is fragile. **Recommendation:** keep workers
  non-daemon but ensure `stop()` always runs (it does, via `__exit__` and test
  fixtures) and that handler loops always terminate (they do: `_stop_event` poll +
  socket idle timeout). Document the one residual: a process that `del`s the server
  without calling `stop()` and leaves a client connected could keep a non-daemon
  worker alive. The `with SecantusDBServer(...)` context manager (the documented
  usage) always calls `stop()`, so this is a misuse-only edge.
- **Cap accounting under reject.** `_active_conns` is incremented *before* `submit`
  and decremented in the handler's `finally`. If `submit` itself raised (e.g. pool
  already shut down during a race with `stop()`), the increment would leak. Guard the
  `submit` so a failed submit decrements `_active_conns` and closes the socket,
  mirroring the existing TLS-handshake-failure cleanup (server.py:267-268).
- **No semantic change for pymongo.** One connection still maps to one dedicated
  thread for its lifetime; request ordering, `moreToCome`, OP_QUERY handshake, change
  streams — all unchanged. Conformance gauges should be unaffected.

### Blast radius

- `src/secantus/server.py` only (pool field, `start`/`stop`, the `submit` swap, the
  failed-submit guard). ~1 file, ~25 lines net.
- No storage, wire, command, or cursor changes.
- Docs: a one-line update where the thread-per-connection model is described
  (CLAUDE.md "Architecture" bullet for `server.py`; `docs/` if it mentions the model).

### Test plan (for when implementation is greenlit)

New `tests/test_server_pool.py` (pymongo-driven where possible):

1. **Thread reuse across churn.** Open and close N (> small pool warm-up) connections
   serially; assert the set of distinct handler thread names observed is bounded
   (reuse happened) rather than ~N distinct names. Capture thread identity via a
   test hook or `threading.enumerate()` name prefix `secantus-conn`.
2. **Cap still rejects.** With `max_connections` small, open that many concurrent
   connections, assert the next one is refused (current behaviour preserved).
3. **WT session does not leak across reused threads.** Churn many short-lived
   connections (well past `session_max`) on a server with a small `session_max`;
   assert no `WT_ERROR: out of sessions`. This is the exact failure the reset guards
   against and is the regression most worth pinning now that threads are reused.
4. **Clean shutdown with a parked change-stream getMore.** Open a change stream
   (handler parked in tailable `getMore`), call `stop()`, assert it returns within a
   couple of seconds and `storage.close()` ran.
5. Full existing suite green (`invoke test`), plus a spot pymongo CRUD smoke.

## Option B (rejected for this branch): reactor + request-dispatch pool

A small `selectors`-based I/O thread set watches all sockets; when a complete wire
message is framed, the *request* (not the connection) is submitted to a worker pool
that runs `dispatch()`, and the reply is handed back to the I/O layer to write. This
is the "real" thread-pool-serves-requests design and gives a flat thread count
regardless of connection count.

Rejected here because the cost/benefit is wrong for a single-node test surrogate:

- **No throughput upside.** `Storage`'s global `RLock` serialises dispatch anyway, so
  the only gain is thread/memory count for many *idle* connections — not a documented
  pain point for the test-fixture audience.
- **Non-blocking TLS is fussy.** `ssl` sockets in a selector loop must handle
  `SSLWantRead`/`SSLWantWrite` re-arm; today the handshake is a simple blocking call
  on the accept thread.
- **Blocking `awaitData` getMore.** Change-stream `getMore` blocks on `_oplog_cv` for
  up to ~1s by design. In a request-pool model that parks a worker per active change
  stream, partially recreating the per-connection-thread cost it was meant to avoid,
  and needs a redesign (e.g. a timer/condition-driven re-submit) to do properly.
- **In-order per connection.** Must guarantee at most one in-flight request per
  connection (pymongo is one-request-at-a-time per socket, so tractable, but it's
  extra bookkeeping the current model gets for free).

If a future need arises to hold tens of thousands of mostly-idle connections in one
process, revisit Option B — but that is not the test-surrogate workload today.

## Summary

| | Today | Option A (chosen) | Option B (rejected) |
|---|---|---|---|
| Thread per connection | yes (new each time) | yes (reused from pool) | no (per request) |
| Bounded | by `max_connections` | by `max_connections` (== pool size) | by pool size |
| Connection-churn cost | high (create/teardown) | low (reuse) | low |
| Throughput vs today | — | unchanged (RLock-bound) | unchanged (RLock-bound) |
| Thread count for many idle conns | high | high | flat |
| Blast radius | — | ~1 file | wire + I/O rewrite |
| TLS / getMore complexity | simple | simple (unchanged) | significant |
