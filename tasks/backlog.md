# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

- ~~`getLog`~~ — real now. Backed by `secantus.logbuf.LogBuffer` (5000-line ring buffer) on `SecantusDBServer.logs`. The accept loop logs connect events; expand to other log sites as needed.
- [ ] **`abortTransaction`** / **`commitTransaction`** — return `{ok: 1}` but **do not roll back**. Operations inside a transaction take effect immediately. Tests that depend on real transactional rollback need a real `mongod`. (Logical sessions ARE tracked end-to-end via ``secantus.sessions.SessionRegistry``; transactions are the next layer up that doesn't yet correlate with session state.)

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`renameCollection`** — atomic per the storage `RLock`, but no protection against concurrent writers across worktrees. Tests are single-process so this is fine.
- [ ] **`createIndexes` options that are accepted but not enforced**: `collation` (Python compares with default locale).
- [ ] **Crash durability — `writeConcern: {j: true}` not yet honoured.** WT logging is now on (`log=(enabled=true)`) and `Storage.close()` forces a checkpoint before releasing the connection, so `bench/chaos.py` (3 min, 13 SIGKILLs) now reports **121,734 inserts acked / 121,709 persisted** (99.98%) — up from 1/432,881 before. See the regression test `tests/test_storage.py::test_inserts_survive_simulated_crash`. Remaining: the connection runs with `transaction_sync=(enabled=false,method=fsync)`, which matches mongod's default `w:1 / j:false` (durable across SIGKILL of the process, vulnerable to power-loss between commits and the next OS flush). A `writeConcern: {j: true}` request from the client is currently ignored — we should either plumb it down to a per-commit `commit_transaction("sync=on")` or document it as out of scope. Throughput cost will be substantial (current chaos shows ~675 inserts/s with logging-only; per-commit fsync would drop another order of magnitude).

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **`local.oplog.rs` synthetic collection**: real mongod exposes the oplog as a queryable collection at `local.oplog.rs`; the admin UI's deferred `/oplog` page (window inspector + paged entries) wants this. Today, `Storage.read_oplog` / `oplog_floor_seq` / `oplog_tail_seq` exist as Python methods but no wire surface lets a pymongo client see them. Either synthesize the collection in `find_matching` / `count_matching` / `list_collections` for the `(local, oplog.rs)` pair, or add narrowly-scoped `secantusAdmin.oplogStats` + `secantusAdmin.oplogRead` commands. The synthetic-collection path is the more honest dogfooding choice. Slice 6 of the admin UI ships change-stream tail only; the oplog page is parked here.
- [ ] **`killOp` / connection-close command**: real mongod exposes `db.killOp(opid)` to abort an in-flight op (which also reaps the connection's TCP socket). SecantusDB has no equivalent — the admin UI's `/connections` page (Slice 8) is read-only as a result. Implementation needs interruptible commands at the dispatch layer (a per-op cancel flag the long-running paths poll) plus a wire command that takes `opid` and either signals the flag or closes the per-connection socket directly. Until then, "kill this connection" is a TODO on the connections page.
- [ ] **Admin UI saved-connections / settings page**: Slice 11 of the admin UI shipped schema sampler / logs viewer / geo viewer but skipped the planned `/settings` page with saved Mongo URIs and a manual dark/light toggle. The CLI today takes a single `--uri` per launch, so saved connections are bookmark-only (you can't switch targets after start). When the launcher gains hot-swap support, revisit this page — it's likely a small SQLite-backed list reusing the existing `~/.secantus/admin.db` store.
- [ ] **Admin UI native WT-checkpoint backup**: Slice 12 ships a mongodump/mongorestore-driven `/backup` page; the originally-planned "WT checkpoint → tar" path is parked here. It would (1) call `Storage.checkpoint()` to flush, (2) tar the storage directory, (3) deliver the archive. The hard part is that the admin app talks to SecantusDB only over the wire — it doesn't know the server's `storage_path`. The cleanest implementation is a `secantusAdmin.backupArchive` wire command that takes a server-side output path, performs the checkpoint, and writes the tarball. Skipped from Slice 12 to keep that slice focused on the tools-on-PATH happy path.
- [ ] **More aggregation expressions**: `$mergeAll`, `$function` (JS — also out of scope).
- [ ] **More aggregation stages**: `$fill`. `$densify` is implemented for both numeric ranges and date ranges with fixed-duration units (`week` / `day` / `hour` / `minute` / `second` / `millisecond`); the variable-length units (`month` / `quarter` / `year`) are rejected with a clear error — supporting them needs `relativedelta`-style arithmetic which isn't worth a new dependency yet.
- [ ] **`mapReduce`** — deprecated by MongoDB but still used by some legacy code. Not implemented.

### Authentication

SCRAM-SHA-256 is implemented end-to-end. The wire-protocol shape (saslStart/saslContinue, `hello.saslSupportedMechs`, per-connection auth state, `--auth` gating) is conformant for pymongo and mongo-go-driver. The remaining gaps are mostly orthogonal:

- [ ] **x509 / LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** — alternative mechanisms. Out of scope for the first auth slice.
- [ ] **Internal cluster auth (keyfile / x509)** — only meaningful with replica sets / sharding, both out of scope.
- [ ] **`system.users` collection visibility** — credentials live in a dedicated WT table (`secantus_users`), not surfaced via `find` / `aggregate` on `admin.system.users`. Tools that poke at the system collection won't see them; use `usersInfo` instead.
- [ ] **`system.version` `authSchema`** — not maintained. Tools that read the auth-schema version will get nothing.

### Change-stream limitations

Single-node change streams are implemented and conformant for typical pymongo `watch()` flows, but the following are deferred or intentionally diverge from real `mongod`:

- [ ] **Multi-document transactions in change events** — `txnNumber` and `lsid` are never present on change events; SecantusDB has no real transaction state.
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
- ~~Capped collections~~ — implemented. `create capped: true, size, max` accepted; `Storage.insert` and `Storage.update_matching` enforce FIFO eviction by walking the doc table in natural order and evicting oldest non-fresh docs while bounds are exceeded. `listCollections` surfaces `options.{capped,size,max}`. Eviction emits oplog `op:"d"` entries (and pre-images when enabled) so change streams observe the deletes. **Known limitation**: eviction order is `_id_key` natural order, which equals insertion order only when `_id` is monotonic (the default `ObjectId`). With user-supplied non-monotonic `_id` values, eviction does not match strict insertion order — capped users with custom `_id` should not rely on FIFO semantics.
- ~~Profiling~~ — implemented. `profile` command (-1 / 0 / 1 / 2 with `slowms` + `sampleRate`) sets per-database state in `secantus_profile_settings`. Dispatch wraps each non-skip command in `time.monotonic_ns` timing; if the per-DB level matches, an entry is inserted into `<db>.system.profile` (auto-created capped 10 MB). Recursion guard skips ops against `system.profile` itself + handshake / cursor-continuation / profile-itself commands. Entry shape mirrors mongod (`ts`, `op`, `ns`, `command`, `millis`, `ok`, `client`, optional `user`, `errMsg` / `errCode` on failure). Out of scope today: `planSummary` / `keysExamined` / `docsExamined` / `nreturned` (would need post-handler stats plumbing).
- ~~Tailable / awaitData cursors~~ — implemented for change streams (see "In scope" in `CLAUDE.md`). Outside change streams (e.g. capped-collection tailables) still depend on capped-collection support, which remains out of scope.

## 5. Known bugs and edge cases to watch

Subtler than the above; these may bite specific test suites.

- [ ] **`$sample`** — uses `random.sample` without a fixed seed. Deterministic only if test does `random.seed(...)` first.
- [ ] **`$type: "number"`** in queries — handles `int`, `float`, `Decimal128`, but the int32-vs-int64 distinction depends on Python value range, not the original BSON type tag (which we throw away on decode). A doc inserted as `Int64(5)` reads back as a small Python int and matches `$type: "int"`, not `"long"`.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).
- [ ] **Ruby gauge: `applies the write concern passed in as an option` expected-fail under single-node topology** — mongo-ruby-driver's `Mongo::Collection#create ... when write concern passed in as an option` test (`spec/mongo/collection_ddl_spec.rb:211`) explicitly passes `w: 2` to `collection.create` and expects success — it assumes the canonical multi-node replica-set test cluster the Ruby driver's CI runs against. SecantusDB advertises as a single-node `secantus` replica set, so `w: 2` produces a `writeConcernError` (code 100, `CannotSatisfyWriteConcern`) — added in `commands.py:_unsatisfiable_wc_error` + dispatch wire-up. This is the correct mongod emulation; the test is structurally incompatible with our topology. **Net trade-off was +7 Ruby gauge passes**: seven `applies the write concern` tests that pass `INVALID_WRITE_CONCERN = {w: 4000}` and expect `OperationFailure` now pass because of the wce, this one test now fails. If the test cluster ever grows past 1 advertised member, this test will start passing organically.
- [ ] **`createCollection` option handling gaps** — three mongo-go-driver tests in `internal/integration::TestDatabase/create_collection` regressed when the parallel session's collation rollout in `b7ad85c` made them reachable (prior they were gated out by missing feature support). Each is a specific `createCollection` option round-trip that our handler doesn't accept / surface back via `listCollections`:
  - `options/all_options_except_collation_and_csppi` — accepting the full "all standard options minus collation and changeStreamPreAndPostImages" bundle in a single `create` (likely missing one or more of `validator`, `validationLevel`, `validationAction`, `expireAfterSeconds`, `clusteredIndex`, `pipeline`/`viewOn`, `writeConcern` echo).
  - `options/changeStreamPreAndPostImages` — `create` accepts `changeStreamPreAndPostImages: {enabled: true}` per the `collMod` path, but `listCollections` doesn't surface it back under `options` for verification.
  - `collation` — `create` with a `collation` option needs to round-trip back through `listCollections.options.collation` matching the spec the driver supplied. The collation work in `b7ad85c` made the feature reachable but didn't wire the option-echo path.
  Look at `_create` and `_list_collections` in `src/secantus/commands.py` and `Storage.get_collection_options` to see what's persisted vs surfaced.

## 6. Admin UI review punch list

End-to-end review of the secantus-admin web UI on `main` (May 2026, before the `admin-ui` branch lands its next slice). Severity tiers: P0 broken/silently-wrong, P1 inconsistency or significant usability gap, P2 polish. File refs are absolute under `src/secantus/admin/` unless noted.

### P0 — broken / silently wrong

- [ ] **Doc tour describes non-existent `/console` page** — `docs/admin.md:201-214` walks the user through "the Console" with three Alpine-toggled tabs (find / aggregate / runCommand). The page was renamed to `/query` mid-refactor; `templates/pages/query.html` has the actual implementation, and `/insert` is a separate top-level route. Replace the Console subsection with `### Query (/query)` + `### Insert (/insert)` matching the sidebar. Also add the missing `### Server (/server)` subsection — current target switching, recently-used targets, and embedded-server start/stop are documented nowhere.

- [ ] **Maintenance "Drop collection" form references a dead route** — `templates/pages/maintenance.html:68-71` has `hx-get="/maintenance/drop-collection-redirect"` but no such route exists in `routers/maintenance.py`. Currently masked because `onsubmit` line 71 intercepts and calls `openDropCollModal()`; with JS disabled or a HTMX/JS race, the user gets a 404. Drop the dead `hx-get` attribute and rely on the JS handler, or wire a real `/maintenance/drop-collection/{db}/{coll}/confirm` endpoint and use Alpine `:hx-get` to assemble it.

- [ ] **Dead dashboard partial + endpoint** — `routers/dashboard.py:91-106` exposes `GET /_partials/dashboard-tiles` rendering `templates/partials/dashboard_tiles.html`, but `pages/dashboard.html` is now fully WebSocket-driven via Alpine (lines 4-55) and never includes the partial. The HTMX-polling fallback the docstring at `dashboard.py:1-7` describes is dead code. Delete both, or repurpose the partial as a `<noscript>` fallback inside `dashboard.html`.

- [ ] **Profiler page accepts `flash` kwarg but never renders it** — `routers/profiler.py:31-84` passes `flash` to the template; `templates/pages/profiler.html` has no flash banner block. POST → `HX-Redirect` (line 109) means HTMX submits get zero confirmation feedback. Add a flash banner block to `profiler.html` and stop using `HX-Redirect`, or accept that the form-value change is the only feedback.

- [ ] **Profiler swallows every exception reading `system.profile`** — `routers/profiler.py:52` has a bare `except Exception:` that masks server-down errors as "no entries yet — run an operation to see one appear here." Narrow to `except (PyMongoError, OperationFailure):` so target-disconnect surfaces.

### P1 — significant inconsistency / usability

- [ ] **`/roles` is reachable but invisible in sidebar, and highlights wrong nav item** — `templates/base.html:19-34` lists 13 nav items; Roles isn't one. The route is real (`routers/users.py:254-278`), only discoverable via the breadcrumb on `/users`. Worse, `roles.html` sets `active="users"` so the wrong sidebar item highlights when on `/roles`. Either add a Roles entry under Users in the sidebar with `active="roles"`, or commit to Users-as-parent and add an explicit Users-page sub-nav.

- [ ] **Sidebar visual grouping is incomplete** — `static/css/admin.css:124-128` has one separator above "Server"; the next logical group break (after Users, separating "per-target operations" from "operational state": Change stream / Connections / Cursors / Profiler / Maintenance / Logs / Backup) has no visual cue. Add a second `.nav-separator` rule and apply it on the first item of the operational-state group.

- [ ] **Modal patterns drift across the 8 confirmation modals** — `templates/partials/{index_drop,user_drop,user_password,user_roles,cursor_kill,doc_edit,doc_delete,maintenance_drop_db,maintenance_drop_coll}_modal.html`. Five sub-issues:
  - Destructive button copy: Drop / Delete / Kill / "Drop database" / "Drop collection" — pick one convention (always restate action+noun).
  - Typed-confirm targets: doc-delete asks for the **collection name** (shared by every doc on the page), not the doc `_id`; cursor-kill asks for the giant int cursor id. Use `_id` (or literal "delete") for documents; use `ns` for cursors.
  - Escape key doesn't dismiss any modal. Add `@keydown.escape.window` handler in the overlay div.
  - Focus is not restored to the trigger element after close; Tab key escapes the modal back into the page (no focus trap).
  - `aria-label="Close"` is on `doc_delete_modal` and `doc_edit_modal` only — apply to all 8.

- [ ] **No `hx-indicator` anywhere — long-running ops look frozen** — grep for `hx-indicator` in `templates/` returns zero hits. HTMX adds the `htmx-request` class during in-flight requests; without an indicator element the user has no signal when (a) opening a modal, (b) submitting a form, (c) deleting an index/document/cursor, (d) running an aggregate with `$lookup`. On a slow target this looks like the UI froze. Add a small inline spinner pattern in `base.html` and reference it from the long-running endpoints.

- [ ] **`/backup/dump` and `/backup/restore` block the request thread** — `routers/backup.py:91, 110` call `backup_lib.run_mongodump` / `run_mongorestore` synchronously. A 500MB collection dump hangs the page for tens of seconds with no spinner, no progress, no async wrapper. At minimum disable the button + show a spinner; ideally wrap in a background task with poll status.

- [ ] **`/connections` and `/cursors` have no auto-refresh and no manual refresh** — sister pages to the dashboard (which polls 1Hz over WS), but both are static snapshots requiring F5. Add `hx-trigger="every 5s"` on the table tbody, or at minimum a "Refresh" button.

- [ ] **`data-table` empty-state row hardcodes `colspan` in 8 templates** — `pages/{databases,collections,connections,cursors,users,indexes,maintenance,backup}.html`. Future "add a column" edits silently break alignment in every other empty state. Pull into a shared partial (or use an out-of-table `.empty-state` div).

- [ ] **Collection viewer pager is one-directional** — `pages/collection.html:58-63` has only "Next page →". Skip-by-`_id` pagination makes Previous expensive but a "← First page" reset link is free; without it a user who paged 12 times has no way back to page 1 except deep-link or sidebar bounce.

- [ ] **`/connections` / `/cursors` table-shape asymmetry** — `pages/connections.html` has 6 columns + no Actions column (because `killOp` isn't implemented); `pages/cursors.html` has 4 columns + Actions. Sister pages should match. Add a disabled, tooltip-explained Actions column to Connections so the layouts mirror, and link the "kill not implemented" tooltip to the `killOp` deferred entry in this backlog.

- [ ] **Form-validation feedback is inconsistent or missing** — three drift cases:
  - `/users` create-user POST returns 400 on missing fields (`routers/users.py:103-107`); the client `hx-on::after-request` (`pages/users.html:32`) only reloads on `event.detail.successful`, so 400s never surface. Use the form-validation handler in `app.py:162-200` or render the error inline.
  - `/changestream` Watch button is inside a `<form method="get">` with no submit handler (`pages/changestream.html:7-30`); pressing Enter in any field does a GET form submit and reloads the page, losing in-flight events. Drop the `<form>` wrapper or add an Alpine Enter handler.
  - Server-switch URI form (`pages/server.html:66-85`) does no client-side validation; user discovers `htt://typo` only after submit. Add `pattern="mongodb(\\+srv)?://.*"` and `inputmode="url"`.

- [ ] **Maintenance "Drop collection" form has no db/coll datalist** — `pages/maintenance.html:73-76`. Forces the user to remember exact names; `/insert` and `/query` already populate datalists from `listDatabases`. Reuse the same pattern.

- [ ] **`HX-Redirect` on indexes POST forces a full reload that loses scroll position** — `routers/indexes.py:179-186`. Long index lists drop the user back to the top. Switch to `HX-Trigger: indexes-changed` plus `hx-trigger="indexes-changed from:body"` on the table tbody to swap rows only.

### P2 — polish

- [ ] **Admin UI polish bundle** — small fixes that don't deserve individual entries; address opportunistically when touching nearby code:
  - **Dead CSS**: `.loading` (`admin.css:233-235`), `.nav-disabled` (`admin.css:130-133, 297-300`) — neither is referenced in any template.
  - **Undefined-but-used class**: `.filter-label` is used in 17 places but never defined in `admin.css`. Either define it (currently `.filter-form label` is the de-facto rule) or replace usages.
  - **Inline error styling** in `pages/profiler.html:67` and `pages/backup.html:62` (`style="border-color: var(--error); color: var(--error)"`) — promote to `.badge-error` and use a class.
  - **Brand-mark glyph reuse**: `●` is used as the brand glyph, the server-badge dot, and the embedded-server "running" indicator — three semantic uses of one character. Different glyphs or sizes for state-indicator vs brand-mark would help.
  - **Opposite ordering conventions**: change-stream events are newest-first (`pages/changestream.html:114` `unshift`); logs are oldest-first (`partials/logs_lines.html:4`). Pick one.
  - **Empty-state copy is uneven**: "No databases.", "No documents match.", "No fields seen — collection may be empty." Compute "0 of N documents match filter" vs "0 documents in <coll>" so users can tell empty-collection from no-match.
  - **No favicon**: `<head>` in `base.html` has no `<link rel="icon">`; `/favicon.ico` requests will 401 through the auth middleware.
  - **Logs page** has no last-updated timestamp despite polling every 2s — user can't tell if the poll is alive (`pages/logs.html:9`, `partials/logs_lines.html:4`).
  - **Schema page** sample size silently caps at 1000 (`extras.py:35-42`); show "(max 1000)" in the label.
  - **Geo page** always picks the first `2dsphere`/`2d` field (`extras.py:139-143`); collections with two geo fields can't see the second. Add a `<select>` populated from `geo_indexes`.
  - **Index/user create forms** are wrapped in `<details><summary>Create X</summary>` with the same label on both summary and submit button; rename summary to "+ New X".

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.
