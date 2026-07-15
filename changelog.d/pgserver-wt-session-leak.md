### PG server: connection teardown releases the thread's WiredTiger session

Every PG connection thread that wrote data leaked its cached WT session on
disconnect (the Mongo server's teardown has always released it;
`_handle_client`'s never did). Dead threads' positioned cursors kept cache
pages pinned, and after a few hundred connections WiredTiger's eviction
livelocked — an application thread wedged in `__wt_cache_eviction_worker`
while holding the storage lock, queueing every other connection forever. The
full psycopg gauge's single-daemon run hung at ~test 420 three times out of
three; with the fix it completes in ~125s (faster than the ~550s baseline,
since sessions no longer pile up). Verified by an 8-writer-connection leak
probe (unfixed: 2 → 10 sessions; fixed: flat) pinned as a regression test.
Also: a binary/garbage COPY payload now raises SQLSTATE 22021 (invalid byte
sequence) instead of escaping as an internal error.

#### Fixed

- `pgserver.py`: `_handle_client`'s finally releases the thread's WT session
  and cached cursors via `Storage._reset_thread_session()`, mirroring the
  Mongo server; `_copy_in` guards `decode_text` with a faithful 22021.
- psycopg gauge headline after the day's slices (COPY transactionality,
  CREATE SCHEMA, server-side cursors, this fix), on the standard
  single-daemon protocol: **2554 passed / 61.9%**, up from 2465 / 59.8% —
  report refreshed.
