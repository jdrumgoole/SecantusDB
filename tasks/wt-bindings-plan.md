# Plan: replace WiredTiger's SWIG Python bindings (production write throughput)

Branch: `wt-bindings` · Worktree: `../SecantusDB-wt-bindings`

## Why this exists

The `wt-concurrency` exploration (merged into main as `cad49c1`) decomposed `Storage._lock` into per-collection locks + an `_oplog_seq_lock` for sequence minting. It didn't lift the write-concurrency ceiling. The `bench/profile_insert.py` cProfile then showed where the wall time actually goes:

- **30.5%** in `wiredtiger/packing.py:unpack` — pure-Python format-string packing in WT's SWIG bindings
- **23.9%** in `wiredtiger.swig_wiredtiger:get_keys` — SWIG cursor result unpacking
- **20.9%** in `wiredtiger.swig_wiredtiger:get_values` — same
- **26.5%** in `bson decode` — already C, fast in absolute terms
- **~6%** in our own sortkey encoding
- **NOT VISIBLE**: `_pack_entry`, `_index_key_variants` (the original C-rewrite target — too fast to register)

WT's SWIG bindings hold the GIL across every cursor op. That makes the *bindings themselves* the GIL bottleneck, not our code on top. Two server threads can't actually run `cursor.insert()` in parallel because only one can be inside the SWIG layer at a time.

The fix is to replace WT's SWIG-generated Python bindings with Cython (or cffi) bindings that:
1. Call WT's C API directly — `wiredtiger_open`, `WT_CONNECTION->open_session`, `WT_SESSION->open_cursor`, `WT_CURSOR->set_key`/`set_value`/`insert`/`update`/`remove`/`search`/`next`/`reset`/`close`, etc.
2. Release the GIL across every blocking call.
3. Match the existing SWIG API surface so `Storage` can switch over without a sweeping refactor.

## Goal

A single SecantusDB process serving N concurrent writer connections delivers:
- 2-writer aggregate throughput ≥ **1.5×** the 1-writer baseline (the regression test threshold from `tests/test_concurrency.py`)
- 4-writer aggregate throughput ≥ **2.5×**
- 8-writer aggregate throughput across distinct collections ≥ **3.5×**

These are the same numbers the `wt-concurrency` plan set; that plan failed to reach them because the SWIG layer was the real ceiling. This plan attacks the actual ceiling.

## Non-goals

- Multi-process / sharded SecantusDB.
- Reimplementing WT itself; we still link against the vendored C library.
- API-source compatibility with `import wiredtiger` from outside callers — only what `secantus.storage` uses needs to keep working.
- Migrating the existing on-disk format. Bytes on disk are WT's; bindings change is invisible to existing data.

## Architecture

Two layers:

```
secantus.storage         (Python, no change to its public API)
       │
       ▼
secantus._wt             ← NEW Cython package (the rebound bindings)
       │  ⟂ releases GIL on every cursor op
       ▼
libwiredtiger.a          (the vendored static library, no change)
```

The new `secantus._wt` package exposes a Pythonic API that mirrors the bits of `wiredtiger.Session` / `wiredtiger.Cursor` we use, but every method:
- Translates Python → raw C buffers up front (under the GIL)
- Calls the WT C function inside a `with nogil:` block
- Translates C result → Python values (under the GIL)

The crucial property: **two threads can be inside `with nogil:` simultaneously**. WT's own MVCC handles the actual concurrency at the C level (per-thread sessions, per-table B-tree page locks); the GIL was the artificial ceiling.

## Phases

Each phase is independently mergeable. Full test suite must stay green between phases.

### Phase 3.1 — GIL-release proof of concept (1 day) — **gate criterion for the whole plan**

Build the smallest possible Cython binding: a `_wt_poc.pyx` module exposing `open_connection`, `open_session`, `open_cursor`, `cursor_insert(key, value)`, `cursor_close`, `session_close`, `connection_close`. Just one cursor type, one key format (`u`, raw bytes), one value format (`u`).

Threaded benchmark:
- N threads (1, 2, 4, 8) each in a tight loop calling `cursor_insert(random_key, random_value)`.
- Compare against the existing SWIG `cursor.insert()` in the same shape.
- Two configurations: same-table (forces WT page contention) and per-thread-table (no WT contention).

**Gate:** if Cython scales ≥1.5× at N=2 and ≥2.5× at N=4 (per-table mode), the rewrite is justified — proceed to Phase 3.2. If both Cython and SWIG flatten the same way, the bottleneck is below the bindings (WT B-tree page locks, log-file fsync ordering, kernel-level lock contention) and the entire plan should be parked.

### Phase 3.2 — Mirror the SWIG cursor API (3-4 days)

Build out `secantus._wt.Cursor` covering every method `secantus.storage` calls:
- `set_key(*args)` for `key_format=SS`, `SSS`, `SSu`, `SSSu`, `q`, `S`
- `set_value(value)` for `value_format=u`
- `insert()`, `update()`, `remove()`
- `search()`, `search_near()`, `next()`, `prev()`
- `get_key()`, `get_value()`
- `reset()`, `close()`
- The `__getitem__` / `__setitem__` shortcuts (`c[k] = v` and `c[k]`)
- `overwrite=` in cursor open

Each method releases the GIL across its WT call. Format packing is done in Cython without re-entering Python (port the `wiredtiger/packing.py` logic to typed Cython that operates on `bytes` buffers directly).

### Phase 3.3 — Connection + Session (2 days)

`secantus._wt.Connection`, `secantus._wt.Session`. The connection is opened once per process via `wiredtiger_open`; sessions are per-thread (matches our existing `threading.local()` model in `Storage`).

Includes:
- Config string passing
- Error code translation (a `WiredTigerError` exception class with the same `.errno` etc. callers expect)
- `WT_DUPLICATE_KEY` recognition for unique-index conflicts
- Transaction control: `begin_transaction()`, `commit_transaction()`, `rollback_transaction()`, `checkpoint()`

### Phase 3.4 — Switch `Storage` to the new bindings (1 day)

Single import-line change: `from secantus._wt import wiredtiger_open, ...` instead of `from wiredtiger import ...`. The Cython module exposes the same surface so no `Storage` code changes.

If we shake out parity bugs here, the test suite catches them.

### Phase 3.5 — Concurrency benchmark + regression test (1 day)

Re-run `bench/concurrency.py` and `tests/test_concurrency.py`. The slow-marked regression test should flip green.

### Phase 3.6 — Drop the SWIG dependency (2 days)

Once `secantus._wt` is fully on, remove `wiredtiger_python` from the CMake build (saves wheel size + build time) and the SWIG patches in `cmake/patch_wt_python.py`. Keep just the static C library build.

### Phase 3.7 — Wider conformance pass (3 days)

Run the four driver gauges (pymongo / mongo-go / mongo-node / mongo-java / mongo-ruby) and the chaos benchmark. Verify no regressions. Update `docs/concurrency.md` with the new ceiling numbers.

## Schedule

| Phase | Days |
|---|---|
| 3.1 — GIL-release POC (gate criterion) | 1 |
| 3.2 — Cursor API parity | 3-4 |
| 3.3 — Connection + Session + transactions | 2 |
| 3.4 — Switch Storage | 1 |
| 3.5 — Concurrency validation | 1 |
| 3.6 — Drop SWIG dependency | 2 |
| 3.7 — Conformance + docs | 3 |
| **Total** | **~13-14 days** |

## Risks

1. **Phase 3.1 fails its gate.** This is the killer. If the Cython binding scales no better than SWIG, the bottleneck is in WT's C library (page-level locks, log-file fsync, etc.) and Python-side rebinding cannot help. Plan parks until WT has a story for higher write concurrency, or we move to a sharded multi-process model.
2. **Format-packing edge cases.** WT's packing.py handles unusual cases (BSON binary, signed/unsigned int variants, padding). Missing one corrupts a key on disk. Mitigation: test parity against the existing SWIG bindings on every WT format we use, byte-for-byte.
3. **Cursor lifecycle in Cython.** WT cursors are session-affine; closing the session frees them. The Cython wrapper has to match that lifecycle without leaking. Mitigation: `__dealloc__` on the cursor type calls `close()` if not already closed.
4. **Build matrix complexity.** Adding a Cython extension to scikit-build-core's CMake setup. Mitigation: do this carefully in Phase 3.1 — if the build doesn't work cleanly across macOS arm64 / Linux x86_64+aarch64 / Windows, that's a deal-breaker that has to be solved early.
5. **Maintainability handoff.** Cython is more cryptic than Python; introduces a new debugging dimension. Mitigation: keep `_wt.pyx` deliberately small (the goal is "thin layer over WT C API"), don't put any Python-domain logic in there.

## Decision points

- **After Phase 3.1**: gate criterion. Numbers in hand.
- **After Phase 3.2**: do we fork WT's `wiredtiger/packing.py` logic byte-for-byte (safer but more code) or use cffi format-string parsing (less code, more risk)?
- **After Phase 3.5**: if benchmarks show real scaling, push to main. If gate barely cleared and benchmarks underwhelm at scale, weigh the SWIG-removal cost (Phase 3.6) — keeping both bindings around is also viable.

## Spike-first discipline

Same as the previous plan: do Phase 3.1 first, in isolation, before committing to anything else. The cProfile-driven discipline already saved 12 days of wasted work on the wrong target. Phase 3.1 must produce an actual measurement before any of 3.2+ starts.

---

## Phase 3.1 result (2026-05-10) — gate criterion FAILED

`bench/wt_poc/wt_pthread_bench.c` is a pure-C pthread program: N threads, each opening its own session on a shared `WT_CONNECTION`, each writing `count` rows to its own table. Identical workload to `bench/wt_poc/wt_swig_bench.py` (Python threads through SWIG). Same WT config as `Storage`. Total work scaled to 50,000 rows total at every N so the comparisons are apples-to-apples.

**Pure C + pthread (no GIL, no Python overhead) — `log=(enabled=true)`:**

| N | Aggregate rows/s | Scaling |
|---|---|---|
| 1 | 276,449 | 1.00× |
| 2 | 340,106 | 1.23× |
| 4 | 352,731 | 1.28× |
| 8 | 285,146 | 1.03× |

**Pure C + pthread — logging disabled (`log=(enabled=false)`):**

| N | Aggregate rows/s | Scaling |
|---|---|---|
| 1 | 1,007,557 | 1.00× |
| 2 | 1,156,150 | 1.15× |
| 4 | 700,035 | 0.69× |
| 8 | 347,176 | 0.34× |

**Python + SWIG + threads (the existing path):**

| N | Aggregate rows/s | Scaling |
|---|---|---|
| 1 | 116,578 | 1.00× |
| 2 | 87,010 | 0.75× |
| 4 | 67,660 | 0.58× |
| 8 | 58,751 | 0.50× |

### What this proves

1. **WiredTiger itself does not scale write-concurrency past N≈2 within a single `WT_CONNECTION`.** This is at the C level, with no GIL, no Python anywhere on the hot path. Logging on or off, the ceiling is roughly the same — at best ~1.3× of single-thread aggregate throughput. Beyond N=4 the C path either stays flat (with logging) or actively regresses (without). The bottleneck is in the WT C library: B-tree page locks, log-write serialisation, cache eviction lock, internal scheduler — pick any of them, the result is the same. Re-binding above this layer cannot deliver scaling that the layer below doesn't have.

2. **Disabling logging is not a winning trade.** Single-thread without logging is ~4× single-thread with logging (1M vs 276k r/s). That looks attractive, but multi-thread without logging *collapses* at N=4/8. So the "fast path" is only fast for one writer; under any concurrency it degrades worse than the logged path. And you've lost durability across SIGKILL.

3. **The Cython rebind plan would deliver ≤ 1.3× over single-writer at best.** Even a perfect zero-overhead binding can't break the WT-level ceiling. After the cost of mature Cython implementation + format-pack parity work + maintenance burden, a 30% throughput bump is not the right ROI.

### Decision

**Phase 3.2+ does not start.** The wt-bindings rewrite is parked.

Real production write-throughput concurrency on WT requires either:
1. **Multiple `WT_CONNECTION`s on different data directories** — sharded multi-process coordination layer above. Architecturally large; defeats the "single embeddable process" goal.
2. **Use a different storage engine** — RocksDB, FoundationDB, etc. have different concurrency models. Wholesale replacement.
3. **Mongod-style C++ above WT** — `mongod` doesn't get its write throughput from clever Python bindings; it gets it by being a careful C++ scheduler over WT's lower-level primitives. Reimplementing that stack above WT is not a SecantusDB project.

For SecantusDB's stated niche — **single-process embeddable test surrogate** — the right answer is: document the WT-level concurrency ceiling clearly, mark `tests/test_concurrency.py` as `xfail` (or remove it as misleading), and stop. Concurrent-write throughput is not a goal SecantusDB can credibly compete on with WT as the storage backend.

### Spike artefacts kept

- `bench/wt_poc/wt_pthread_bench.c` — pure-C pthread benchmark. Default log-on; `-DWT_NOLOG_VARIANT=1` to test no-log.
- `bench/wt_poc/wt_swig_bench.py` — SWIG equivalent.
- `bench/wt_poc/run.py` — orchestrator that runs both at N=1,2,4,8 and prints the scaling table.

These exist as reproducible evidence so the next person to ask "can we just rewrite the bindings?" can re-run the benchmark, see the same numbers, and not re-walk this path.
