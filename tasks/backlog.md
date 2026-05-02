# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

- [ ] **`serverStatus`** — returns version + zeroed metrics (uptime, connections). No real metrics tracking.
- [ ] **`connectionStatus`** — empty `authInfo` (no auth implemented).
- [ ] **`hostInfo`** / **`whatsmyuri`** / **`buildInfo`** — hardcoded values. `buildInfo.version` is literally `"7.0.0"`.
- [ ] **`getLog`** — empty log array.
- [ ] **`startSession`** / **`endSessions`** / **`refreshSessions`** — `startSession` returns a fresh UUID; the others are no-ops. **No session state is tracked**, so cross-session correlation isn't enforced.
- [ ] **`abortTransaction`** / **`commitTransaction`** — return `{ok: 1}` but **do not roll back**. Operations inside a transaction take effect immediately. Tests that depend on real transactional rollback need a real `mongod`.

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`$lookup` does not use storage indexes** — joins are O(N+M) via an in-memory hash table built once over the foreign collection (covers array-valued local/foreign fields correctly via element expansion). Both simple (`localField`/`foreignField`) and `let`/`pipeline` forms are accelerated; if both are specified, the simple-form hash-join pre-filters the candidates fed to the pipeline. Storage indexes on the foreign field are NOT consulted; a true index-driven join would skip materialising the foreign collection but needs multikey-index support to stay correct for array-valued foreign fields.
- [ ] **`$dateFromString` / `$dateToString`** — format strings use Python's `strptime`/`strftime` codes plus the `%L` extension for milliseconds; `timezone` argument supports IANA names ("Europe/Dublin"), UTC offsets ("+05:30"), and "GMT"/"UTC". Still missing: full MongoDB format spec (`%G`/`%V` ISO-week, `%j` day-of-year edge cases) and the `format` option's MongoDB-specific tokens.
- [ ] **`renameCollection`** — atomic per the storage `RLock`, but no protection against concurrent writers across worktrees. Tests are single-process so this is fine.
- [ ] **`createIndexes` options that are accepted but not enforced**: `collation` (Python compares with default locale).
- [ ] **TTL is opt-in, not automatic** — `expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)` which deletes expired docs and their index entries. Real MongoDB runs this on a 60s background sweeper; SecantusDB does not, so tests that depend on TTL behaviour must call `prune_ttl` explicitly.

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **More aggregation expressions**: `$mergeAll`, `$function` (JS — also out of scope).
- [ ] **More aggregation stages**: `$fill`. `$densify` is implemented for numeric ranges (`bounds: "full"` / `[min, max]`, `partitionByFields`); date densify (`unit: "day" | "hour" | ...`) is deferred — needs date-arithmetic step iteration that isn't a one-line addition.
- [ ] **`mapReduce`** — deprecated by MongoDB but still used by some legacy code. Not implemented.
- [ ] **WiredTiger binary wheels** — `pip install secantus` triggers a from-source build of `wiredtiger==11.3.1`. The README now lists the `cmake` / `ninja` / `swig` prerequisites for macOS and Linux, which unblocks most users. The full fix (binary wheels) is bigger than a single slice: either land a cibuildwheel pipeline upstream in `apache/wiredtiger` (or wherever the `wiredtiger` PyPI package is built from), or vendor WiredTiger's source into `secantus` and ship `secantus` itself as a per-platform binary wheel. Both need hands-on iteration against real CI runners on macOS x86_64/arm64, manylinux x86_64/arm64, and Windows x86_64.

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Replica sets / sharding / change streams** — depend on cluster topology or oplog. SecantusDB is single-process; not the target use.
- **Authentication** (SCRAM-SHA-256, x509, LDAP, Kerberos) — production auth is not the test-harness concern. `connectionStatus` returns an empty `authInfo` so clients that probe see "no auth required."
- **TLS / SSL** — same reason.
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo** (`$near`, `$nearSphere`, `$geoWithin`, `$geoIntersects`, 2d / 2dsphere indexes) — would need geometric primitives.
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **Capped collections** — fixed-size, FIFO collections. Implementable later if needed.
- **Profiling** (`setProfilingLevel`, `system.profile` collection) — real `mongod` self-profiles; we don't.
- **Tailable / awaitData cursors** — depend on oplog or capped collections.

## 5. Known bugs and edge cases to watch

Subtler than the above; these may bite specific test suites.

- [ ] **`$sample`** — uses `random.sample` without a fixed seed. Deterministic only if test does `random.seed(...)` first.
- [ ] **`$type: "number"`** in queries — handles `int`, `float`, `Decimal128`, but the int32-vs-int64 distinction depends on Python value range, not the original BSON type tag (which we throw away on decode). A doc inserted as `Int64(5)` reads back as a small Python int and matches `$type: "int"`, not `"long"`.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.
