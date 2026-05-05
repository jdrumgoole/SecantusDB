# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

- [ ] **`hostInfo`** / **`whatsmyuri`** / **`buildInfo`** — hardcoded values. `buildInfo.version` is literally `"7.0.0"`.
- [ ] **`getLog`** — empty log array.
- [ ] **`startSession`** / **`endSessions`** / **`refreshSessions`** — `startSession` returns a fresh UUID; the others are no-ops. **No session state is tracked**, so cross-session correlation isn't enforced.
- [ ] **`abortTransaction`** / **`commitTransaction`** — return `{ok: 1}` but **do not roll back**. Operations inside a transaction take effect immediately. Tests that depend on real transactional rollback need a real `mongod`.

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`$lookup` index acceleration: compound and multikey foreign indexes still hash-join.** The simple-form / pipeline-with-prefilter paths now drive the per-outer-doc lookup through `Storage.find_matching` when there's a non-multikey single-field index on `foreignField` (each request lands as IXSCAN, the foreign collection is never materialised). When the foreign field is array-valued (multikey index) or only covered by a compound index, the lookup falls back to the original O(N+M) in-memory hash-join — multikey would force a per-outer-doc scan and end up O(M*N), and the compound-leading-field equality probe is correct but the safe-perf rule today is "exact-shape match only". A future slice could opt into the compound-leading-field index when the exact-shape rule could be loosened safely.
- [ ] **`$dateFromString` / `$dateToString`** — format strings use Python's `strptime`/`strftime` codes plus the `%L` extension for milliseconds; `timezone` argument supports IANA names ("Europe/Dublin"), UTC offsets ("+05:30"), and "GMT"/"UTC". Still missing: full MongoDB format spec (`%G`/`%V` ISO-week, `%j` day-of-year edge cases) and the `format` option's MongoDB-specific tokens.
- [ ] **`renameCollection`** — atomic per the storage `RLock`, but no protection against concurrent writers across worktrees. Tests are single-process so this is fine.
- [ ] **`createIndexes` options that are accepted but not enforced**: `collation` (Python compares with default locale).
- [ ] **TTL is opt-in, not automatic** — `expireAfterSeconds` is honoured by `Storage.prune_ttl(db, coll, *, now=None)` which deletes expired docs and their index entries. Real MongoDB runs this on a 60s background sweeper; SecantusDB does not, so tests that depend on TTL behaviour must call `prune_ttl` explicitly.

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **More aggregation expressions**: `$mergeAll`, `$function` (JS — also out of scope).
- [ ] **More aggregation stages**: `$fill`. `$densify` is implemented for numeric ranges (`bounds: "full"` / `[min, max]`, `partitionByFields`); date densify (`unit: "day" | "hour" | ...`) is deferred — needs date-arithmetic step iteration that isn't a one-line addition.
- [ ] **`mapReduce`** — deprecated by MongoDB but still used by some legacy code. Not implemented.

### Authentication

SCRAM-SHA-256 is implemented end-to-end. The wire-protocol shape (saslStart/saslContinue, `hello.saslSupportedMechs`, per-connection auth state, `--auth` gating) is conformant for pymongo and mongo-go-driver. The remaining gaps are mostly orthogonal:

- [ ] **Custom roles** (`createRole` / `dropRole` / `updateRole` / `grantPrivilegesToRole` / `revokePrivilegesFromRole`) — built-in roles (`read`, `readWrite`, `dbAdmin`, `userAdmin`, the `*AnyDatabase` variants, and `root`) are enforced via `secantus.rbac` and the per-command action map in `secantus.commands`. Custom user-defined roles, `clusterAdmin` / `clusterMonitor` / `backup` / `restore`, and role-inheritance graphs aren't implemented yet.
- [ ] **SCRAM-SHA-1** — legacy mechanism. Modern drivers default to SCRAM-SHA-256 since pymongo 3.7 / driver-spec 2018; we only advertise the modern one. Add if a downstream tool insists on SHA-1.
- [ ] **SASLprep (RFC 4013)** password normalisation — passwords go to PBKDF2 as raw UTF-8 with no normalisation. ASCII passwords work byte-identically against real `mongod`; non-ASCII (combining marks, full-width digits, etc.) may diverge from a SASLprepping client's expectation.
- [ ] **Speculative authentication** — pymongo / mongo-go-driver can fold the first SCRAM round-trip into the `hello` reply via `speculativeAuthenticate`. Not implemented; the explicit two-round-trip path works fine but adds one RTT to connection setup.
- [ ] **x509 / LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** — alternative mechanisms. Out of scope for the first auth slice.
- [ ] **Internal cluster auth (keyfile / x509)** — only meaningful with replica sets / sharding, both out of scope.
- [ ] **`system.users` collection visibility** — credentials live in a dedicated WT table (`secantus_users`), not surfaced via `find` / `aggregate` on `admin.system.users`. Tools that poke at the system collection won't see them; use `usersInfo` instead.
- [ ] **`system.version` `authSchema`** — not maintained. Tools that read the auth-schema version will get nothing.

### Change-stream limitations

Single-node change streams are implemented and conformant for typical pymongo `watch()` flows, but the following are deferred or intentionally diverge from real `mongod`:

- [ ] **Multi-document transactions in change events** — `txnNumber` and `lsid` are never present on change events; SecantusDB has no real transaction state.
- [ ] **`splitLargeChangeStreamEvents`** — not implemented. Events are emitted whole.
- [ ] **`noop` heartbeat events** — real `mongod` writes periodic no-ops to advance cluster time even when no real ops happen; SecantusDB does not. Resume tokens advance only on real ops.
- [ ] **Read concern / write concern semantics** — accepted on the wire for compatibility, otherwise ignored.
- [ ] **`showExpandedEvents`** — accepted, ignored.
- [ ] **Resume-token cross-server identity** — tokens are opaque to pymongo and round-trip fine, but the inner layout is `{s, t, n, k}` (BSON-encoded, hex-stringed) rather than mongod's keystring format. Tokens minted by SecantusDB cannot be presented to a real `mongod`, and vice versa.
- [ ] **`updateDescription.truncatedArrays`** — emitted only when the post array is a strict head-prefix of the pre array. Other array reshapes produce a wholesale `updatedFields` entry rather than the in-place diff mongod would produce.

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Real replica sets / sharding** — depend on cluster topology and cross-node consistency. SecantusDB advertises `setName: "secantus"` to satisfy pymongo's change-stream topology check, but the topology is fictional — there are no other members, no elections, no cross-node oplog. Change streams are still in scope (single-node, oplog-backed); see `## 3. Deferred work / Change-stream limitations`.
- ~~Authentication (SCRAM-SHA-256)~~ — implemented. `--auth` (CLI) / `require_auth=True` (constructor) gates non-handshake commands behind a successful `saslStart`/`saslContinue` round-trip. Provision users via `createUser`; manage with `dropUser` / `usersInfo`. The remaining auth gaps are tracked under `## 3. Deferred work / Authentication` below.
- **TLS / SSL** — same reason.
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo — complete and shipped.** Operators (`$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`) + `$geoNear` aggregation stage (auto-infer `key`, `includeLocs`), `2dsphere` (S2 cell coverings + ancestors) and `2d` (bit-interleaved geohash) index acceleration, compound geo+scalar indexes (geo cell scan + verifier-step filter on trailing scalars), and write-time input validation (out-of-range coordinates / unparseable shapes reject with mongod's documented code 16572 across insert / update / upsert / createIndex). See `src/secantus/geo.py` + `src/secantus/geo_index.py`. **Validation surface**: 60+ in-tree pymongo tests in `tests/test_geo*.py`; 3 cross-driver smoke tests through mongosh, mongo-node-driver, and mongo-go-driver in `tests/test_geo_cross_driver.py` (all pass — wire-protocol geo path is clean across drivers); the pymongo conformance gauge keeps `test_collection.py`'s built-in geo tests at 100% pass. **Known optimisation deferred**: fine-grained 2d range covering for large query polygons (the single-coarse-bbox range over-scans Z-order space because bit-interleaved geohashes don't preserve row contiguity; tighter covering needs Tropf-Herzog LITMAX/BIGMIN-style range decomposition, ~100+ LOC). The verifier filters all false positives, so this is a perf optimisation not a correctness gap; defer until a workload actually hits it. **Out of scope**: Java cross-driver smoke (single-file equivalent doesn't exist; BSON-level GeoJSON serialization is exercised by the `:bson:test` Java gauge already), exact mongod error string matching (chase work without a clear payoff unless a driver test pins exact wording).
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **Capped collections** — fixed-size, FIFO collections. Implementable later if needed.
- **Profiling** (`setProfilingLevel`, `system.profile` collection) — real `mongod` self-profiles; we don't.
- ~~Tailable / awaitData cursors~~ — implemented for change streams (see "In scope" in `CLAUDE.md`). Outside change streams (e.g. capped-collection tailables) still depend on capped-collection support, which remains out of scope.

## 5. Known bugs and edge cases to watch

Subtler than the above; these may bite specific test suites.

- [ ] **`$sample`** — uses `random.sample` without a fixed seed. Deterministic only if test does `random.seed(...)` first.
- [ ] **`$type: "number"`** in queries — handles `int`, `float`, `Decimal128`, but the int32-vs-int64 distinction depends on Python value range, not the original BSON type tag (which we throw away on decode). A doc inserted as `Int64(5)` reads back as a small Python int and matches `$type: "int"`, not `"long"`.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.
