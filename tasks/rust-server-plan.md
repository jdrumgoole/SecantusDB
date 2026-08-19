# Rust server plan — two separate servers, not one selectable engine

**Status: active. This document supersedes the in-process selectable-engine
model** described in `tasks/rust-rewrite-plan.md` §2 / §10, the "dual-engine at
`Storage` granularity" decision in `tasks/rust-rewrite-phase3-scoping.md` §3 and
`tasks/rust-rewrite-phase4-scoping.md`, and the §5e "Python `Storage` adapter +
`secantus.engine` selection" cutover. Where those docs say "select the engine
process-wide via `secantus.engine` / `SECANTUS_ENGINE`" or "two `Storage`
implementations behind one interface," read **this** instead.

The *engine-porting* history in those docs (the pure-Rust crates, the WT-FFI
decisions, the storage sub-phases 2a–5e, the geo slices) is still valid and
still the foundation — only the **integration model** changes.

---

## 1. The decision

There are **two completely separate servers**, and a user runs **one or the
other** — never a mix, never a per-operator/per-`Storage` selection inside one
request path:

- **The Python server** — the *original* `SecantusDBServer`: `server.py` +
  `wire.py` + `commands.py` + the pure-Python `Storage` and the pure-Python
  operator engines (`query` / `update` / `expressions` / `projection` / `diff` /
  `sortkey` / `aggregate`). **No Rust in the request path.** This is the
  reference implementation and stays first-class and permanent.

- **The Rust server** — a *whole, self-contained* server written in Rust: its
  own wire layer, command dispatch, cursor registry, accept loop, and the
  already-ported pure-Rust `secantus-core` + `secantus-storage` + `secantus-wt`
  crates underneath. **No Python in the request path, no PyO3 in the hot path,
  no fallback into Python operators.**

### Why this replaces the old model

The old plan made the two implementations co-resident in one process, selected
per-component (`secantus.engine`) with a graceful per-call `Fallback` to Python.
For *pure operator engines* that was safe and it shipped (Phases 1–2). But
extending it to **storage and the server** forced the "5e adapter": a Python
`Storage` shell routing every data op across the PyO3 seam into `RustStorage`,
with `EngineFallback` bouncing unsupported ops *back* into Python operators
running over Rust-scanned docs. That is a half-Python/half-Rust request path —
the worst of both: the marshalling tax the byte-seam was meant to kill, two code
paths entangled in one process, and no clean "this is the Rust server" artifact.

Two separate servers removes the entanglement. Each server is internally
coherent and independently testable, and the Rust server is exactly the
standalone artifact the crate split was always aiming at (`secantus-core` /
`secantus-storage` are deliberately PyO3-free precisely so a standalone server
can reuse them).

## 2. The Python interface is a thin layer over the Rust server

The CLAUDE.md ergonomic — *"starting a server in a test should be one or two
lines, with no external processes to manage"* — is preserved for the Rust
server by a **thin embedded lifecycle handle**, not by putting Python in the
request path:

- A small PyO3 crate (`crates/secantus-server-py`) exposes a `#[pyclass]` with
  only **`start(storage_path, port=0, …) -> address`**, **`stop()`**, `.address`
  / `.uri`, and the context-manager protocol. That is the *entire* Python
  surface — lifecycle, not operators.
- On `start()`, the Rust server's TCP accept loop runs on a **Rust thread inside
  the Python process** (GIL released via `Python::allow_threads`), exactly as
  today's pure-Python `SecantusDBServer` runs its accept loop on a daemon
  thread. The user's `pymongo` client connects over **real TCP** to that
  in-process listener. `port=0` (ephemeral) and a `tmp_path` storage dir work
  unchanged; shutdown is deterministic.

This keeps the byte path closed (`socket bytes → Rust wire → Rust dispatch →
Rust storage → Rust BSON reply → socket bytes`) — Python is only ever the
*launcher*. It is **not** the rejected selectable engine: there is no Python
operator code in the path and no `EngineFallback`.

Two ways a user reaches the Rust server, both driving the *same* Rust core:

1. **From Python** — `from secantus import …` a thin wrapper that boots the
   embedded handle (the test/dev ergonomic, what pytest uses).
2. **Standalone** — a `secantusdb` Rust binary (`main` over the same crates) for
   non-Python users. Free once the server is a library crate.

*(Subprocess model — Python spawns the standalone binary and hands back a URI —
was considered and rejected: it reintroduces an external process to manage,
against the design constraint. Embedded handle is the chosen path.)*

## 3. What is kept, what is dropped

**Kept (the foundation — unchanged):**
- The pure-Rust crates: `secantus-core` (engines + geo + aggregation), `secantus-
  storage` (the full WT-backed `Storage`: CRUD, indexes, geo, oplog/change-stream
  storage, users/roles/profiling, lifecycle, stats), `secantus-wt` (the WT FFI).
  All of the 2a–5e porting work lands directly under the Rust server.
- **The leaf-engine parity suites** (`tests/test_rust_*_parity.py`) — Rust
  engines stay pinned **byte-for-byte** to the pure-Python engines as the
  fine-grained correctness oracle (the maintainer chose to keep these). They
  import `_secantus_core` to run the Rust engine and diff it against the Python
  one; that is their *only* remaining reason to exist, and it is sufficient.
- The WT-FFI engine choice (option A), faithful on-disk encodings (`sortkey` /
  geo golden-pinned), and the 1:1 concurrency port.
- The decision to **bundle** the compiled Rust artifact into the `secantus`
  wheel behind an off-by-default CMake flag, reusing the wheel's existing
  vendored-WiredTiger build (see `rust-rewrite-phase4-scoping.md` "wheel-matrix
  gate"). That mechanism now ships the Rust *server* extension, not a storage
  adapter.

**Dropped / retired (the selectable-engine machinery):**
- **`secantus.engine` process-wide selection in the Python request path** — the
  per-component shims (`query.py` / `update.py` / … delegating to `_secantus_core`
  when `enabled()`), `SECANTUS_ENGINE` / `SECANTUS_RUST_<COMPONENT>` routing, and
  `SecantusDBServer(engine=…)`. The Python server reverts to **pure Python**. The
  `_secantus_core` PyO3 bindings are **retained only as the parity-test vehicle**,
  not wired into any server's request path.
- **The "5e adapter"** — the Python `Storage` over `RustStorage`, `EngineFallback`
  routing, and the *fat* `secantus-storage-py` surface (the whole `Storage`
  interface exposed to Python). The Rust server calls `secantus-storage`
  **natively in Rust**; the only Python-facing surface is the thin lifecycle
  handle (§2). `secantus-storage-py` collapses accordingly (kept, if useful, only
  for storage-crate unit smokes — not as a production path).

> **Note / confirm:** retiring the in-process per-operator accelerator means the
> Python server no longer gets the Rust speedup — that path is now the Rust
> server's job. This follows from "two completely separate servers, original
> server based on Python." Flagged here in case the intent was to *also* keep the
> per-operator accelerator on the Python server; the plan assumes not.

## 4. Rust-server build-out (the new phase sequence)

The engines + storage are done (pure-Rust crates, parity-pinned). What remains is
the **server above the storage**, all net-new Rust. Each slice gates on running
the relevant pymongo-driven suite **against the Rust server** (via the embedded
handle, `port=0`, `tmp_path`) in CI / on a WT-capable machine.

- **R1 — Wire layer** (`crates/secantus-wire`). ✅ **DONE.** Pure-Rust, PyO3-free
  port of `wire.py`: the 16-byte little-endian `Header` (pack/unpack +
  `body_len` bounds), `OP_MSG` (2013) kind-0 body + kind-1 document-sequence
  parsing, legacy `OP_QUERY` (2004) parsing, and the `OP_MSG` / `OP_REPLY`
  builders. Parsing is **zero-copy framing** — `OpMsg` / `OpQuery` borrow byte
  slices (kind-0 body, each kind-1 doc) out of the caller's buffer rather than
  decoding to owned `Document`s; builders take already-encoded BSON bytes. The
  `_check_doc_len` bounds discipline and the recoverable-vs-fatal split are
  preserved via `WireError::{MalformedBody, Protocol}` + `is_recoverable()` (the
  connection loop pairs a recoverable error with the header it already read). BSON
  content is deep-validated at parse time (`Document::from_reader`) to match
  `bson.decode`'s accept/reject, incl. the handcrafted malformed-body case from
  `tests/test_wire_malformed.py`. Added to the workspace `members` (no native
  deps beyond `bson`, so the `rust` CI job builds it); 17 unit tests, `clippy
  -D warnings` + `fmt` clean. **Follow-up:** dispatch (R2) currently re-decodes
  the borrowed body — a later optimisation can return the validated owned doc.

- **R2 — Command dispatch** (`crates/secantus-commands`). Port `commands.py`: the
  dispatch table keyed on the first doc key, the handshake family
  (`hello`/`isMaster`/`ping`/`buildInfo`/…), CRUD
  (`insert`/`find`/`update`/`delete`/`count`/`drop`/`aggregate`/`findAndModify`/
  `listCollections`/…), the error contract (handler error →
  `{ok:0, errmsg, code, codeName}`; unknown command → `59 CommandNotFound` so the
  connection survives). The widest slice; **sub-sliced by command family**:
  - **R2a ✅ DONE** — dispatch framework + handshake family. `command_name`
    (first key), a `Handler` registry (`lookup`), the [`CommandError`] triple +
    `into_reply` (`{ok:0, errmsg, code, codeName}`; unknown → `59
    CommandNotFound`), and the cross-cutting validation `dispatch` runs first:
    `readConcern.level` (`FailedToParse` 9 / `SnapshotUnavailable` 246) and
    `apiVersion` / `apiStrict` (`APIVersionError` 322 / `APIStrictError` 323,
    `distinct` name gate). Handlers: `hello`/`isMaster`/`ismaster` (standalone +
    single-node `secantus` replica-set block via `ctx.cluster_time`,
    `accessControlEnabled`, int64 `connectionId`/`counter`), `ping`,
    `buildInfo`/`buildinfo`. Handlers return `Result<Document, CommandError>` —
    no Python-style `try/except` (typed errors carry their own code). 13 unit
    tests, `clippy -D warnings` + `fmt` clean; added to the workspace. **Deferred
    to later slices:** metrics / session-TTL touch / `--auth` gating / RBAC /
    failpoints / profiling / `writeConcernError` attachment (land with their
    families); `hello`'s `saslSupportedMechs` / `speculativeAuthenticate` /
    client-metadata stash (R5 auth); the `apiStrict` aggregation-stage gate
    (aggregate family).
  - **R2b ✅ DONE** — CRUD write/count family (`insert` / `delete` / `count`) +
    the storage seam. `secantus-commands` stays **WT-free** via a `Storage`
    **trait** (`src/storage.rs`: `insert` / `update_matching` / `delete_matching`
    / `count_matching`, bytes at the seam, a boxed `StorageError` the adapter
    pre-classifies into per-op `writeError` codes); the real
    `secantus-storage::Storage` satisfies it through an adapter in the server
    crate (R4). `CommandContext` gained an `Option<Arc<dyn Storage>>` + a
    `storage()` accessor (missing backend → `InternalError`). Handlers (ports of
    `_insert`/`_delete`/`_count`): empty-`documents` → `InvalidLength` (4); `_id`
    `$`-prefix per-doc rejection (2); ordered/unordered semantics; `writeErrors`
    index remap from the surviving subset back to original positions; E11000
    `keyPattern`/`keyValue`; `delete` `{q, limit}` batch; `count` skip/limit
    clamp. 9 handler tests over an in-memory fake `Storage` (22 crate tests
    total), `clippy -D warnings` + `fmt` clean. **Deferred (tracked in backlog
    §7, and **both have since shipped** — `crud::update` and `find::find` are
    registered in the dispatch table; this list is the state as of R2b):**
    `update` (R2c — pipeline-form `u` / `arrayFilters` / `let` /
    `collation` / `validator` need storage-signature work); `find` (with R3
    cursors + projection); `writeConcern`, collection `validator`,
    `_reject_oplog_rs_write`, `let`/`collation` on delete, view-collection count.
  - **`find` ✅ DONE** (`secantus-commands::find`) — the keystone read command
    and the producer of the cursors R3a manages. Port of `_find`'s non-tailable
    path: the `Storage` trait gained `find` (= `find_matching_with`; returns the
    full ordered match set), and the handler does `skip` → `limit` → `projection`
    (via `secantus_core::projection`; a `Fallback` ⇒ `BadValue`) →
    `_split_into_cursor` (firstBatch + register the remainder, `batchSize` 0 / +
    / `singleBatch` all handled). Hints pass through as raw `Bson` (the adapter
    converts to `secantus_storage::Hint`). The full **`find → getMore →
    killCursors`** read path now works end-to-end in the Rust dispatch. Helpers
    refactored into a shared `util` module. 8 find tests (41 crate total),
    `clippy -D warnings` + `fmt` clean. **Deferred:** up-front empty-collection
    filter validation (needs the query engine's parse-vs-`Fallback` distinction);
    `tailable: true` capped-poll; `let` / `collation`.
  - **`update` ✅ DONE** (`secantus-commands::crud::update`) — port of `_update`'s
    document-form path: per-spec `sort` rejection (FailedToParse 9, pre-8.0),
    pipeline-form shape validation (malformed → command-level FailedToParse 9 /
    InvalidPipelineOperator 168), `update_matching` per spec with `multi` /
    `upsert`, error mapping (DuplicateKey → 11000, adapter-classified
    WriteError → per-op writeError, Internal → command-level), and the
    `n` / `nModified` / `upserted` / `writeErrors` reply.
    - **Pipeline-form `u` ✅ DONE** — `secantus-storage` gained
      `update_matching_pipeline` (each matched doc rewritten by running the
      aggregation pipeline over it; shares the match/write/oplog/index path with
      `update_matching` via a transform closure; **always diff-style oplog** so
      change streams report `operationType: "update"`, not `"replace"` — the
      array-truncation spec). Threaded through the command `Storage` trait
      (default rejects → adapter forwards) + handler; malformed stages still
      surface 9 / 168 at command level; pipeline upsert seeds from the filter.
    - **Positional operators + `arrayFilters` ✅ DONE** —
      `secantus-core::update::apply_update_with` resolves `$` (from the query
      filter via `find_positional_matches`), `$[]` (all elements), and
      `$[ident]` (via `index_array_filters` + the query matcher). `secantus-storage::
      update_matching` always computes positional matches and takes an
      `array_filters` slice; threaded through the command trait's
      `update_matching_array_filters` (default forwards, adapter routes) + handler
      (parses per-statement `arrayFilters`). Parity suite extended (the
      `apply_update_with` binding + 11 arrayFilters cases stay byte-pinned to
      Python).
    - **`let` ✅ DONE** (update + delete + **find / aggregate / findAndModify**) —
      `resolve_let_vars` (in `util`) seeds `$$NOW` + evaluates each `let` value
      (mirrors `commands._resolve_let_vars`), threaded as query vars through the
      storage matcher: writes via `update_matching_array_filters` /
      `update_matching_pipeline` / `delete_matching_with_let`; reads via
      `find_matching_with`'s new `vars` arg + the trait's `find_collated`
      `let_vars`. (Also fixed `aggregate`, which had passed the raw `let` doc.)
    - **`collation` ✅ DONE (server-wide, COLLSCAN-correct)** — a command
      `collation` threads through `find` / `count` / `distinct` / `aggregate`
      (`$match` + `$sort` + lifted fetch) / `update` / `delete`. The storage
      query methods take a collation and **force a COLLSCAN** when one is active
      (the byte-sortable indexes are collation-naive) + a collation-folded
      in-memory sort. Trait seam: additive `find_collated` / `count_collated`
      (default → uncollated) + collation params on the update/delete
      option-methods. Non-ASCII / `numericOrdering` collation → `BadValue`.
      Deferred: collection-default collation, `$elemMatch` sub-query collation,
      per-index-collation IXSCAN.
    - **`validator` ✅ DONE (insert-time)** — `create` / `collMod` persist
      `validator` + `validationLevel` / `validationAction` (and the other stored
      collection options) via `set_collection_options`; `insert` enforces the
      `validator` (code 121 `DocumentValidationFailure`) unless
      `bypassDocumentValidation` or `validationAction` is `warn` / `off`. Deferred:
      `update` / replace-time enforcement (needs the post-apply doc in storage).
    - **`writeConcern` ✅ DONE (writeConcernError attachment)** — `dispatch`
      attaches `{code:100, codeName:"CannotSatisfyWriteConcern"}` as a
      `writeConcernError` when a request carries `writeConcern.w > 1` and the reply
      is `ok:1` (single-node can't satisfy a multi-node concern).
    - **`collMod` ✅ DONE** — merges the stored collection-option subset into an
      existing collection (`NamespaceNotFound` 26 if missing).
    - **`explain` ✅ DONE** — ports `commands._explain`: lifts a leading `$match`
      for aggregate, rejects journaled / `w:"majority"` writeConcern (72), validates
      `verbosity` (2), shapes `queryPlanner.winningPlan` (`FETCH`+`IXSCAN` or
      `COLLSCAN`) via the trait's `explain_plan` + an `executionStats` block, and
      adds the aggregate `stages: [{$cursor: …}]` wrapper. Collation / collectionless
      forces COLLSCAN.
  - **`aggregate` ✅ DONE** (`secantus-commands::aggregate`, post-merge of PR #31)
    — port of `_aggregate`'s storage-independent path: fetch input via the
    `Storage` trait's `find` (lifting a leading `$match` into the fetch filter +
    dropping that stage), run `secantus_core::aggregate::apply_pipeline` (all the
    storage-free stages), split into a cursor (`cursor.batchSize`). `$changeStream`
    standalone-rejection (40573) honoured; collectionless `aggregate: 1` handled.
    A pipeline the Rust engine can't reproduce (`Fallback`) → `BadValue`. 6 unit
    tests; the pymongo smoke test now exercises `count_documents` + a direct
    `$match`→`$group` pipeline.
    - **Storage-backed stages ✅ DONE** — a `run_segmented` executor in
      `secantus-commands::aggregate` interleaves the storage-free core engine
      with command-layer storage-backed stages: `$lookup` (simple
      `localField`/`foreignField` + `let`/`pipeline` forms, array-aware
      `lookup_match`, `let`-expression evaluation, recursive sub-pipeline via the
      same executor), `$sample` (`rand`, `SECANTUS_SAMPLE_SEED` for determinism),
      `$collStats` / `$indexStats` (first-stage, via `count_matching` /
      `list_indexes` / `collection_is_capped`), `$out` (drop+create+insert), and
      `$merge` (deep-merge default + `replace`/`keepExisting`/`delete`/`fail`
      modes, `whenNotMatched` insert/discard/fail). 9 unit tests over a stateful
      `FakeStorage` + a pymongo→Rust→WiredTiger e2e. **+14 on the R8 rust-server
      gauge (809 → 823; `test_crud_unified` 217 → 229), zero regressions.**
    - **`$geoNear` ✅ DONE** — brute-force COLLSCAN at the command layer
      (`secantus_core::geo::point_distance`: haversine metres for spherical /
      planar otherwise): per-doc distance from `key` to `near`, `query`
      pre-filter, min/max-distance filter (on the raw distance), ascending sort,
      `distanceField` (× `distanceMultiplier`) + `includeLocs` attach. GeoJSON
      `near` ⇒ spherical, legacy `[x,y]` ⇒ planar unless `spherical:true`.
      Gauge-flat (the curated pymongo set doesn't exercise `$geoNear`) but
      e2e-verified + 2 unit tests; closes the aggregate-stage surface.
    - **Still deferred:** `$graphLookup`; `$geoNear` `key`-inference from a geo
      index (explicit `key` required); `$lookup` nested in `$facet` (facet
      sub-pipelines run inside the storage-free core); `$merge` pipeline-form
      `whenMatched` + `on`-field unique-index validation.
  - **`distinct` ✅ DONE** — fetch matching docs via `find`, resolve the dotted
    `key` (flattening one array level), dedup by BSON equality. Collation
    deferred. 5 tests.
  - **DDL + introspection ✅ DONE** (`secantus-commands::admin`) — `create` /
    `drop` / `listCollections` / `listIndexes` / `createIndexes` / `dropIndexes`.
    Extended the `Storage` trait with `list_collections` / `create_collection` /
    `drop_collection` / `list_indexes` / `create_index` / `drop_index` /
    `drop_all_indexes` (default-impl'd so existing fakes compile; the R4b adapter
    forwards each to real WT storage). `NamespaceExists` (48) / `NamespaceNotFound`
    (26) / `IndexNotFound` (27); auto-derived index names; `createIndexes`
    auto-creates the collection. 4 tests. **Deferred:** `create` option/capped/
    view validation; `listCollections` filter; `listIndexes` NamespaceNotFound;
    `dropIndexes` by key spec.
  - **`findAndModify` ✅ DONE** — composed at the command layer (find limit-1 +
    sort → update/remove → re-find for the new image → projection); old/new
    image, upsert, remove, E11000 preserved. Not atomic across the find+modify
    calls (caveat). 7 tests.
  - **db-admin ✅ DONE** (`secantus-commands::admin`) — `dropDatabase` /
    `renameCollection` / `collStats` / `dbStats` / `serverStatus` (the `Storage`
    trait gained `drop_database` / `rename_collection` / `collection_is_capped` /
    `collection_data_size` / `index_sizes`; the R4b adapter forwards them).
    `serverStatus` is a minimal subset; `collStats`/`dbStats` use `dataSize` for
    `storageSize`. 5 tests.
  - **sessions + diagnostics ✅ DONE** (`secantus-commands::diagnostics`) —
    `startSession` (mints a UUID lsid) / `endSessions` / `refreshSessions` /
    `killSessions` / `killAllSessions[ByPattern]` (no-op bookkeeping) /
    `commitTransaction` / `abortTransaction` (no-op) / `getParameter` /
    `getCmdLineOpts` (reflects `--auth`) / `connectionStatus` / `whatsmyuri` /
    `hostInfo` / `getLog`. Storage-light, mostly static — removes
    `CommandNotFound` noise on driver connect/teardown/admin probes. 5 tests.
    **Deferred:** real session/cursor affinity; live `connectionStatus` auth info
    (R5); peer-address `whatsmyuri`.
  - **Next** — **R5 auth + TLS**:
    - **R5a ✅ DONE** — the SCRAM-SHA-256 mechanism (`crates/secantus-auth`,
      pure Rust): `derive_credentials` (PBKDF2-HMAC-SHA-256 → client/stored/server
      keys), `begin_scram` / `continue_scram` server handshake, constant-time
      proof check, unknown-user-fabrication timing. Verified with a full
      client↔server round-trip (6 tests) incl. wrong-password / unknown-user /
      nonce-mismatch / malformed rejections. SCRAM-SHA-1 (MD5 prepass) +
      MONGODB-X509 + non-ASCII SASLprep deferred.
    - **R5b-1 ✅ DONE** — wired SCRAM-SHA-256 into the command layer:
      `saslStart` / `saslContinue` (`secantus-commands::auth`) drive
      `begin_scram` / `continue_scram` against a per-connection
      `ConnectionAuth` (`scram` conversation + authenticated principals),
      threaded one-per-socket through the server's `handle_connection` →
      `make_context`. User management — `createUser` (derives + stores the
      `{credentials: {SCRAM-SHA-256}}` record, mongod-identical shape so both
      servers share the `secantus_users` table), `dropUser`, `usersInfo`
      (`showCredentials` gating) — over four new `Storage` trait methods
      (`add_user` / `get_user` / `drop_user` / `list_users`, default-impl'd,
      forwarded by the WT adapter to `secantus_storage`). 6 command-level unit
      tests (full SCRAM client↔server round-trip, wrong-password, unknown-user,
      unsupported-mechanism, usersInfo cred-hiding, dropUser) + a pymongo TCP
      auth round-trip in `test_rust_server_smoke.py`.
    - **R5b-2 ✅ DONE** — dispatch-level `--auth` gating + RBAC privilege checks.
      New `secantus-commands::rbac` ports the built-in role catalogue (`read` /
      `readWrite` / `dbAdmin` / `userAdmin` / `dbOwner`, the `*AnyDatabase`
      variants, `clusterMonitor` / `clusterAdmin` / `backup` / `restore`,
      `root`) + `check_privilege`. `dispatch`'s `authorize` rejects
      non-handshake commands from unauthenticated connections (`Unauthorized`,
      13) and checks the principal's effective roles against a per-command
      `(action, scope)` table; `createUser` validates roles against the
      catalogue (`RoleNotFound`, 31); a successful `saslContinue` loads the
      user's role bindings into `ConnectionAuth::effective_roles`. 11 unit tests
      (rbac matrix + gating + cross-db / cluster denial + role validation).
    - **R5b-3 ✅ DONE** — custom user-defined roles. New `secantus-commands::roles`
      (`createRole` / `updateRole` / `dropRole` / `dropAllRolesFromDatabase` /
      `rolesInfo`) over four new role-storage trait methods (`add_role` /
      `get_role` / `drop_role` / `list_roles`, adapter-forwarded to
      `secantus-storage`). `rbac::check_privilege_resolved` expands custom roles
      through a `Storage::get_role`-backed resolver (privilege match +
      inheritance walk with cycle detection), and `createUser` now accepts a
      custom role that exists in storage. 6 unit tests (role lifecycle, built-in
      collision, updateRole, resolver privilege + inheritance) + a pymongo WT
      round-trip.
    - **R5b-4 ✅ DONE** — auth/RBAC completion: the role `grant`/`revoke` quartet
      (`grantPrivilegesToRole` / `revokePrivilegesFromRole` / `grantRolesToRole`
      / `revokeRolesFromRole` — merge/dedup privileges by resource, drop
      emptied privileges, dedup inherited roles), `updateUser` (rotate password
      / replace roles, with a live `effective_roles` refresh on the calling
      connection), `dropAllUsersFromDatabase`, and `hello`'s `saslSupportedMechs`
      advertisement. 8 new unit tests + a pymongo WT round-trip. This closes the
      auth/RBAC surface bar SCRAM-SHA-1 / X509 (R5c).
    - **R5c-1 ✅ DONE** — TLS / mTLS transport in the accept loop (`rustls`, ring
      backend — no cmake/nasm). `ServerConfig.tls: Option<TlsOptions>`
      (`cert_file` / `key_file` / `ca_file` / `require_client_cert`); `bind`
      builds the rustls config up front (bad cert → `bind` fails). The accept
      loop drives the handshake under the shutdown-poll timeout, extracts the
      verified client cert's subject DN (`x509-parser`, RFC 4514) into
      `CommandContext::peer_cert_dn`, and the request loop (`serve`) is generic
      over the transport (`TcpStream` | rustls `StreamOwned`). The `RustServer`
      Python handle gains `tls_cert_file` / `tls_key_file` / `tls_ca_file` /
      `tls_require_client_cert`. Validated by a Rust integration test
      (`tests/tls.rs`: self-signed cert via `rcgen` → rustls client → `hello`)
      + an openssl-guarded pymongo TLS smoke test.
    - **R5c-2 ✅ DONE** — the `MONGODB-X509` mechanism. `createUser` provisions
      X509-capable users (`mechanisms: ["MONGODB-X509"]`, no password, X509
      credential marker); `saslStart` with `mechanism: "MONGODB-X509"` and the
      legacy `authenticate` command read `ctx.peer_cert_dn` (the verified client
      cert DN from R5c-1's mTLS handshake), enforce an optional payload-username
      match, look the user up by DN on `$external` (falling back to `admin`),
      require an X509 credential entry, and authenticate without a password.
      `hello`/`getParameter` advertise `MONGODB-X509` alongside SCRAM-SHA-256.
      4 unit tests (X509-only createUser, cert auth, SCRAM-only + DN-mismatch
      rejection, legacy authenticate). **This closes R5 (auth) bar SCRAM-SHA-1.**
      SCRAM-SHA-1 (legacy MD5 prepass) remains deferred — low priority, no modern
      driver defaults to it.
    Then the tailable change-stream getMore + storage-backed aggregation stages,
    and **R7/R8** (standalone `secantusdb` binary + the full pymongo conformance
    gate against the Rust server).

- **R3 — Cursor registry + change-stream tailable plumbing** (in the server
  crate). Port `cursors.CursorRegistry` (int64 id → remaining batch, idle-TTL
  pruning, injectable clock) to a Rust struct owned by the server, and the
  tailable change-stream producer (oplog tail → `changestreams::project`, already
  in `secantus-storage`) driving `getMore`/`killCursors`. The blocking
  `awaitData` wait uses the storage `wait_for_oplog` / `notify_oplog_waiters`
  condvar primitive (landed in 5e-gap-c). Gate: `test_change_streams.py`.
  - **R3a ✅ DONE** — `CursorRegistry` (`secantus-commands::cursors`) + the
    non-tailable `getMore` / `killCursors` handlers. Byte-seam native (pending
    docs are `Vec<Vec<u8>>`), thread-safe behind one `Mutex`, opportunistic
    idle-TTL pruning with an **injectable clock**, 63-bit random ids (ordinary
    odd; tailable `> 2**32`), `max_cursors` cap. `register` / `register_tailable`
    (producer closure via a `CursorProducer` trait) / `info` / `next_batch`
    (tailable persists on empty) / `kill` / `invalidate` / `len` / `snapshot`.
    `getMore` (non-tailable: namespace-ownership check → `CursorNotFound` 43,
    `nextBatch` + `id` 0-on-exhaustion) and `killCursors` wired into
    `CommandContext` via `Option<Arc<CursorRegistry>>`. 13 tests (33 crate
    total), `clippy -D warnings` + `fmt` clean.
  - **R3b-a ✅ DONE** — change streams work end-to-end. `aggregate` with a
    leading `$changeStream` opens a tailable cursor (`changestream::open_change_
    stream`); tailable `getMore` drains buffered events, polls the producer
    (`CursorProducer` extended with `position` / `invalidated`), and returns the
    batch + `postBatchResumeToken`. The projector runs behind a WT-free `Storage`
    trait seam — `change_stream_poll` (read oplog + `changestreams::project` +
    encode), `wait_for_oplog`, `notify_oplog_waiters`, `oplog_tail_seq`,
    `oplog_floor_seq`, `seq_for_timestamp` — implemented in the WT-linked adapter,
    so `secantus-commands` stays WiredTiger-free. insert / update / replace /
    delete + `updateLookup` + pre-images all project correctly. **+58 on the R8
    rust-server gauge (936 → 994, 52 change-stream, zero regressions).**
  - **R3b-b ✅ DONE** — change-stream blocking + resume. `awaitData` blocking
    (the tailable `getMore` loop in `cursors::get_more` blocks on the storage
    oplog condvar via `Storage::wait_for_oplog` until an event arrives or
    `maxTimeMS`/1s elapses, instead of busy-polling); resume positioning in
    `changestream::open_change_stream` — `resumeAfter` / `startAfter` (token →
    `Storage::resume_token_seq`) and `startAtOperationTime` (`seq_for_timestamp`
    - 1), with a `ChangeStreamHistoryLost` (286) guard when the resume point has
    fallen below `oplog_floor_seq`; the synthesized terminal `invalidate` event
    after a drop / rename / dropDatabase (the adapter's `change_stream_poll` now
    appends `changestreams::invalidate_event`, mirroring `commands.py`'s
    producer) so the cursor closes only after delivering it; and empty-batch
    `postBatchResumeToken` advancement via a high-water-mark token
    (`Storage::high_water_mark_token`). Two supporting fixes the resume path
    depended on: `hello`'s `lastWrite.opTime.ts` is now minted from
    `Storage::current_cluster_time` (was a hard-zero R4a stub — `find_seq_for_ts`
    matched everything, breaking `startAtOperationTime`); and `killCursors` wakes
    a blocked tailable getMore via `notify_oplog_waiters`. **+15 on the R8
    rust-server change-stream gauge (55 → 70 / 155) and +6 on `test_custom_types`
    (40 → 46), zero regressions across all server categories.** **Still deferred:**
    noop-heartbeat-driven token advancement on a fully quiet stream (no oplog
    rows at all — the HWM token only advances once a scanned row moves the
    position).

- **R4 — Accept loop + connection threads + TLS** (`crates/secantus-server`). Port
  `server.py`: TCP accept on a daemon thread, thread-per-connection (1:1 with the
  Python model), per-request `CommandContext`, graceful shutdown. TLS via
  `rustls` (+ `rustls-pemfile`); mTLS peer-cert subject-DN extraction reproduced
  for X509 auth. Gate: `test_server.py` / `test_tls*.py`.
  - **R4a ✅ DONE** — the accept loop + connection handling, **generic over the
    command `Storage` trait** (so the crate is WT-free and runs over real TCP in
    the WT-less CI job / this sandbox). `bind(addr, config, storage, cursors) ->
    RunningServer` (accept loop on a background thread, one thread per
    connection, `address()` / `uri()` / `stop()` + Drop-shutdown). Per
    connection: read header → `body_len` bounds → read body → `parse_body`;
    `OP_MSG` (merge kind-1 sequences into the body via `_merge_op_msg_body`,
    honour `moreToCome` = no reply) and legacy `OP_QUERY` (handshake → `OP_REPLY`)
    both dispatch through `secantus_commands::dispatch`; recoverable wire errors →
    a `BadValue` reply that keeps the connection (matching
    `test_wire_malformed.py`), fatal → drop. Read-timeout polling so idle
    connection threads are reaped on `stop`. **Two WT-free integration tests over
    real TCP** (`tests/roundtrip.rs`, in-memory `Storage`, hand-rolled wire
    client): hello / ping / insert / count / find / delete / unknown-command
    survival / legacy `isMaster`; and `find → getMore → killCursors`. `clippy
    -D warnings` + `fmt` clean. **`RunningServer` is the exact core R6's embedded
    Python handle wraps.** **Deferred:** TLS / mTLS (R4 tail); `peer_cert_dn` +
    auth state (R5); metrics / sessions / failpoints / connreg (their slices);
    sourcing `cluster_time` from storage (`hello`'s `lastWrite` uses a zero ts
    until the `Storage` trait exposes `current_cluster_time`).
  - **R4b ⚠️ WRITTEN, CI-VALIDATED-ONLY** — the WiredTiger adapter
    (`crates/secantus-storage-adapter`): `StorageAdapter(Arc<secantus_storage::
    Storage>)` implementing `secantus_commands::Storage`. A near-identity over the
    matching signatures plus two translations — `RawHint` (`Bson`) →
    `secantus_storage::Hint` (`String`⇒`Name`, doc⇒`KeySpec`), and
    `secantus_storage::StorageError` → the command `StorageError` (`DuplicateKey`
    keeps `keyPattern`/`keyValue`; `BadHint` / `QueryUnsupported` / unsupported
    id/value → `WriteError{2}` BadValue; engine/IO → `Internal`). **Links
    WiredTiger, so it's excluded from the clean workspace and could NOT be
    compiled or tested in this sandbox** — it builds + runs only where WT is
    available (the `rust-storage` CI job / a WT machine). Written against the
    confirmed `secantus-storage` signatures; first CI run is its validation.
    (One risk CI will settle: whether `secantus_storage::Storage` is `Send +
    Sync` as the trait's supertrait requires.)

- **R5 — Auth** (in the server crate or `crates/secantus-auth`). SCRAM-SHA-1/256
  (`hmac` / `sha2` / `pbkdf2`), MONGODB-X509, and the RBAC checks. Port `auth.py`
  / `rbac.py`. Pin against `test_auth.py` / `test_x509_auth.py` (tight suites).
  Uses the already-ported users/roles storage (5e-gap-a).

- **R6 ⚠️ WRITTEN, CI-VALIDATED-ONLY** — the thin Python embed handle
  (`crates/secantus-server-py`, the `_secantus_server` extension). A
  `#[pyclass] RustServer` whose constructor opens a WiredTiger
  `secantus_storage::Storage`, wraps it in the R4b `StorageAdapter`, and
  `secantus_server::bind`s it; exposes `address` / `uri` / `stop` + the
  context-manager protocol. The accept loop runs on a GIL-released Rust thread
  in-process; `pymongo` connects over TCP. Lifecycle only — no operators, no
  Python in the request path. Mirrors `secantus-storage-py`'s build (own
  `[workspace]`, `build.rs` macOS `dynamic_lookup`, `pyproject.toml` →
  `maturin`); **links WiredTiger, so excluded from the clean workspace and NOT
  built in this sandbox** — built by the wheel CMake under
  `SECANTUS_BUILD_STORAGE_ENGINE` or local `maturin` with WT present. A pymongo
  smoke test (`tests/test_rust_server_smoke.py`, `importorskip`'d) drives CRUD +
  handshake against it — the **first pymongo → Rust → WiredTiger** test, the
  embryonic R8 gate. **Still a Python wrapper in `secantus` for ergonomic parity
  with `SecantusDBServer` is a follow-up.**

- **R7 — Standalone `secantusdb` binary. ✅ DONE.** `crates/secantusdb`, a
  `main` over the same crates the embedded handle (R6) uses: parse args → open
  `secantus_storage::Storage` → `StorageAdapter` → `bind` → print the bound
  address (the smoke test / a wrapping launcher reads it) → block until
  SIGINT/SIGTERM (`ctrlc`, termination feature) → clean `stop()`. Arg parsing
  is a WT-free module in `secantus-server` (`args.rs`: `--host` / `--port` /
  `--storage-path` / `--auth` / `--standalone` / the four `--tls-*` flags;
  hand-rolled, both `--flag value` and `--flag=value`; TLS pairing rules
  enforced; 11 unit tests in the clean workspace). The bin crate links
  WiredTiger → own `[workspace]`, excluded from the clean workspace, built +
  smoked in the `storage-engine` CI job (Linux/macOS; Windows deferred —
  `build.rs` probes `libwiredtiger.a/.so`, which MSVC doesn't produce).
  Smoke: `tests/test_rust_binary_smoke.py` (launch on port 0, pymongo
  handshake + CRUD round-trip, `--standalone` hello shape, bad-args exit 2,
  clean SIGTERM exit 0); `invoke rust-binary-test` builds + runs it.
  **Update (2026-08-18): the deferred tail has since shipped** — `args.rs` now
  parses `--config` (TOML), `--log-level`, `--log-file-max`, `--cache-size`,
  `--session-max`, `--sync-on-commit`, `--checkpoint-seconds`, `--oplog-async`,
  `--oplog-nonlogged`, `--oplog-archive-dir`, `--oplog-max-entries`,
  `--oplog-retention-seconds`, `--noop-heartbeat-seconds` and `--data-nonlogged`
  alongside the original set.

- **R8 — Conformance gate (go/no-go).** Run the **unchanged** pymongo-driven
  suites (`test_crud.py` / `test_storage.py` / `test_indexes.py` /
  `test_geo_index.py` / `test_change_streams.py` / `test_aggregate.py` / …) and
  the **pymongo + Go/Node/Java/Ruby driver gauges** against the Rust server. The
  headline "MongoDB compatibility" number must not regress vs the Python server.
  This is the definition of "the Rust server is correct."

  **Status: MET as measured 2026-08-11 — all THIRTEEN gauges run against both
  servers.** `pymongo_validation/plugin.py` selects the server via
  `SECANTUS_GAUGE_SERVER` (`python` default / `rust` → the
  `_secantus_server.RustServer` embedded handle) and `gauge_common.py` does the
  same for the other-language gauges (via the standalone `secantusd-rs` binary),
  so `invoke validate-all-servers` runs the whole fleet twice and each Rust pass
  writes a `-rust-server` report. The weekly `.github/workflows/validate.yml`
  matrix has a `pymongo-rust-server` entry.

  **The measured gate (report pairs, `Overall` rows):** the Rust server ties the
  Python server on nine gauges (pymongo 1020 / 99.5% on both, plus
  pymongo-async, go, node, ruby, kotlin, dotnet, php-ext, rust), **beats** it on
  two (c: 749/99.1% vs 739/98.5%; php-lib: 3051/98.7% vs 3049/98.6%), and the
  cxx pair has no parseable `Overall` row (its report format differs — worth a
  look, not a known regression).

  The one genuine regression was **java: 445/99.6% vs 446/99.8%**, and the
  failure sets were *disjoint* — the Rust server PASSES the `ClientMetadataTest`
  the Python server fails, and failed two `mapReduce` tests instead
  (`MongoCollectionTest#testMapReduceWithGenerics`,
  `UnifiedWriteConcernTest#default-write-concern-3.4`) because `mapReduce` was
  simply not in the Rust dispatch table (→ `59 CommandNotFound`).
  `secantus-commands::mapreduce` closes it: the same no-JS-engine port the
  Python server ships (canonical `emit(this.<field>, 1)` + `values.length` →
  `$group` count, double-typed `value`, `{out: {inline: 1}}` gate, empty-but-ok
  otherwise). **Not yet re-measured against the Java gauge** — that needs a JVM
  + Gradle run against a freshly built `secantusd-rs`; the port is unit-tested
  case-for-case against the Python implementation that passes those two tests.

**Leftover storage work folded in** (was Phase-4 tail): re-home `$lookup` /
`$geoNear` / `$out` / `$merge` storage-backed aggregation into `secantus-storage`
(geo-4 + the lookup/merge acceleration), and the remaining 5e gaps
(`checkpoint` / `close` / `create_archive`) as the Rust server needs them.

## 5. Testing & CI strategy

- **Two green servers.** The pymongo + 6-driver gauges run **twice** — once
  against the Python server (the existing CI matrix, unchanged) and once against
  the Rust server (the R8 gate). Neither may regress.
- **Parity suites stay** as the operator-level oracle (§3): Rust engines pinned
  byte-for-byte to Python, independent of either server. They catch divergences
  finer-grained than the wire gauges can.
- **Golden vectors** for the on-disk/byte contracts (`sortkey`, geo cells, resume
  tokens) remain checked-in and asserted in `cargo test`.
- CI lanes: the existing `rust` (core parity) + `rust-storage` (WT-backed storage
  crate) jobs stay; add a **`rust-server`** job that builds the server extension
  (reusing the wheel's vendored WT) and runs the pymongo gauge against it. The
  `storage-engine` bundling job generalises to bundle the *server* extension.

## 6. Open items to confirm

1. **Fate of the in-process per-operator accelerator** — the plan retires it from
   the Python server (Python server = pure Python; all Rust = the Rust server),
   keeping `_secantus_core` only as the parity-test vehicle. Confirm this is the
   intent and not "keep the per-operator speedup on the Python server too." (§3
   note.)
2. **Naming** — how the two servers are surfaced in the public Python API (e.g.
   `SecantusDBServer` stays pure-Python and a new `RustSecantusDBServer` / a
   `secantusdb --rust` flag boots the Rust one), so "pick one" is explicit and
   discoverable. To be settled at R6.
3. **Whether the Python server stays at feature parity going forward**, or becomes
   a frozen reference once the Rust server passes the gate. (The product can keep
   both permanently; this is about where *new* feature work lands.)
