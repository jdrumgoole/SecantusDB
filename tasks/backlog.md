# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

- [ ] **`serverStatus`** — returns version + zeroed metrics (uptime, connections). No real metrics tracking.
- [ ] **`connectionStatus`** — empty `authInfo` (no auth implemented).
- [ ] **`dbStats`** / **`collStats`** — counts are real, sizes are zeroed (we don't track storage size). If a tool depends on `dataSize`/`storageSize`/`avgObjSize`, it'll see 0.
- [ ] **`hostInfo`** / **`whatsmyuri`** / **`buildInfo`** — hardcoded values. `buildInfo.version` is literally `"7.0.0"`.
- [ ] **`getLog`** — empty log array.
- [ ] **`startSession`** / **`endSessions`** / **`refreshSessions`** — `startSession` returns a fresh UUID; the others are no-ops. **No session state is tracked**, so cross-session correlation isn't enforced.
- [ ] **`abortTransaction`** / **`commitTransaction`** — return `{ok: 1}` but **do not roll back**. Operations inside a transaction take effect immediately. Tests that depend on real transactional rollback need a real `mongod`.
- [ ] **`$comment`** query operator — accepted and ignored.

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`$lookup` does not use storage indexes** — joins are O(N+M) via an in-memory hash table built once over the foreign collection (covers array-valued local/foreign fields correctly via element expansion). Both simple (`localField`/`foreignField`) and `let`/`pipeline` forms are accelerated; if both are specified, the simple-form hash-join pre-filters the candidates fed to the pipeline. Storage indexes on the foreign field are NOT consulted; a true index-driven join would skip materialising the foreign collection but needs multikey-index support to stay correct for array-valued foreign fields.
- [ ] **`$merge` whenMatched: "merge"** — shallow `{**existing, **new}` merge with new winning per-key. MongoDB has deeper semantics for nested docs (recursive merge for sub-documents); we do not.
- [ ] **`$dateFromString`** — uses Python's `strptime` codes (or `fromisoformat` if no format). No full MongoDB format spec, no `timezone` argument, no `%G`/`%V` ISO-week support.
- [ ] **`$dateToString`** — Python `strftime` + `%L` for millisecond extension. No `timezone` argument.
- [ ] **`renameCollection`** — atomic per the storage `RLock`, but no protection against concurrent writers across worktrees. Tests are single-process so this is fine.
- [ ] **`createIndexes` options that are accepted but not enforced**: `expireAfterSeconds` (no TTL), `partialFilterExpression` (full collection participates), `collation` (Python compares with default locale).

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **More aggregation expressions**: `$mergeAll`, `$function` (JS — also out of scope).
- [ ] **More aggregation stages**: `$densify`, `$fill`.
- [ ] **`mapReduce`** — deprecated by MongoDB but still used by some legacy code. Not implemented.

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

- [ ] **Iteration order of `find()` without sort** is WT B-tree order on `id_key`, which is `id_key` byte-lex order. For consecutive integer `_id` values this happens to match insertion order. For `_id` types with large or unordered byte representations (e.g. `ObjectId` with embedded timestamps + counter) it'll differ from a real `mongod`'s natural order. Tests that assert order without an explicit `sort` may be brittle.
- [ ] **`_id` uniqueness check via canonical bytes** — for non-numeric BSON values the check is byte-equality on `bson.encode({"_": value})`. Two different Python values that happen to encode to the same bytes (rare) would falsely collide.
- [ ] **`$sample`** — uses `random.sample` without a fixed seed. Deterministic only if test does `random.seed(...)` first.
- [ ] **`update_matching` with `multi=False`** stops at the first WT-key-ordered match. Real MongoDB stops at the first natural-order match. Same for most cases (see iteration-order note above) but not for all `_id` types.
- [ ] **`$type: "number"`** in queries — handles `int`, `float`, `Decimal128`, but the int32-vs-int64 distinction depends on Python value range, not the original BSON type tag (which we throw away on decode). A doc inserted as `Int64(5)` reads back as a small Python int and matches `$type: "int"`, not `"long"`.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.
