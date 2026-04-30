# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

- [ ] **`explain`** — always returns a `COLLSCAN` plan; never actually plans or executes. Fine because we don't have lookup acceleration yet, but the day we add real indexes we should report `IXSCAN` for indexed queries.
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

- [ ] **Mixed-direction compound indexes** — single-field ASC and DESC indexes both work end-to-end (equality, `$in`, range, sort-by-indexed-field, `hint`). Pure-ASC compound indexes also work for prefix lookup, prefix + trailing operator, and sort. What's still missing: compound indexes with mixed directions like `{a: 1, b: -1}` — the encoder already supports per-field direction via `encode_value_directed`, but the lookup planners only consider all-ASC compound indexes. Wiring work, no encoding work.
- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`$lookup`** — full-scan of the foreign collection per outer doc; no use of indexes for the join. Both simple (`localField`/`foreignField`) and `let`/`pipeline` forms are supported. If both are specified, both are applied (simple-form pre-filter, then pipeline). MongoDB's actual behavior with both is more nuanced.
- [ ] **`$merge` whenMatched: "merge"** — shallow `{**existing, **new}` merge with new winning per-key. MongoDB has deeper semantics for nested docs (recursive merge for sub-documents); we do not.
- [ ] **Cursors never expire** — `CursorRegistry` keeps cursors until `killCursors` or batch-exhaustion. Real MongoDB has a 10-minute idle TTL.
- [ ] **`$dateFromString`** — uses Python's `strptime` codes (or `fromisoformat` if no format). No full MongoDB format spec, no `timezone` argument, no `%G`/`%V` ISO-week support.
- [ ] **`$dateToString`** — Python `strftime` + `%L` for millisecond extension. No `timezone` argument.
- [ ] **`renameCollection`** — atomic per the storage `RLock`, but no protection against concurrent writers across worktrees. Tests are single-process so this is fine.
- [ ] **`createIndexes` options that are accepted but not enforced**: `expireAfterSeconds` (no TTL), `partialFilterExpression` (full collection participates), `collation` (Python compares with default locale).

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **More aggregation expressions**: `$mergeAll`, `$function` (JS — also out of scope).
- [ ] **More aggregation stages**: `$densify`, `$fill`.
- [ ] **`mapReduce`** — deprecated by MongoDB but still used by some legacy code. Not implemented.
- [ ] **WiredTiger binary wheels** — `pip install secantus` currently triggers a from-source build of `wiredtiger==11.3.1` which needs `cmake`, `ninja`, and `swig` on `PATH`. The fix is `cibuildwheel` jobs in CI that produce `wiredtiger` wheels for macOS x86_64/arm64, manylinux x86_64/arm64, and Windows x86_64, then host them on GitHub releases or PyPI under the secantus namespace. Until then, document the prerequisites in the README.

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
