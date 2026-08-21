# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those live on the branch and PR doing the fixing. (This line used to point at `tasks/todo.md`; that file is the *delivered* Admin UI plan, not a general in-flight list.)

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

**Markers are load-bearing — `- [ ]` means open work, `- [x]` means a finished
record kept for its detail.** When a slice lands, flip its box in the same commit.
An audit on 2026-08-20 found 107 of 178 open-marked items were finished work whose
box was never flipped, which made the board unreadable: a session scanning for
`- [ ]` could not tell the real tasks from the archive.

**Correct a superseded entry rather than adding a newer one beside it.** The same
audit found entries asserting limitations the engine had not had for months —
"Foreign keys … NOT enforced" sat a few hundred lines above "FOREIGN KEY
enforcement on write landed". **Reproduce an item before working it** (a probe, the
failing test, the query); several "open" items were already fixed, and the wrong
ones were wrong in the direction that costs most — they described work as missing
when it existed.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

(None currently — `commitTransaction` / `abortTransaction` were the last true stubs; real multi-document transactions shipped via `secantus.transactions` + WT-native per-transaction sessions. Remaining transaction divergences are in §3.4.)

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [x] **`$bucketAuto` `granularity` rounding — SHIPPED, hex-exact on both servers (2026-07-19).** The prior "1-ULP blocker" was a wrong-constants artifact: mongod stores each preferred-number series as **integer-valued doubles** (R5 = `{10,16,25,40,63}`, not normalised `0.63`-style literals) and computes `series_element * multiplier` (multiplier a power of 10), which reproduces its non-standard ULPs (`63 * 0.1 = 6.300000000000001`) bit-for-bit in Python and Rust f64. Ported `roundUp` / `roundDown` (double path) verbatim from `granularity_rounder_preferred_numbers.cpp` + the `populateNextBucket` boundary walk (first min = `roundDown(dataMin)`, every other boundary = `roundUp(chunkMax)` with the absorb-below-boundary loop; `std::round(nDocs/nBuckets)` bucket size), plus the POWERSOF2 rounder. Both engines: `secantus.aggregate._bucket_auto_granular` (`_BUCKET_AUTO_SERIES` + `_round_up/down_series` + `_round_up/down_pow2`) and `secantus-core` `group::bucket_auto_granular`. **Verified hex-exact vs a live mongod 7.0.12 oracle** (1200+ cases across all 13 granularities × scales × bucket counts, incl. edges: zeros, exact-boundary values, +inf, single bucket); Rust pinned to Python by a 400-case parity fuzz. Value validation reproduces mongod's codes on the Python server — non-numeric 40258, NaN 40259, negative 40260 (name errors 40261/40257 already shipped) — and defers on the Rust server (BadValue, the standing error-code gap). **Remaining sub-limitation:** a **Decimal128**-valued groupBy defers/rejects (code 2) rather than running mongod's separate Decimal128 rounder — the standing Decimal128 precision deferral; the double/int path (the common case) is complete.
- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`top`: zero counters on the Python server, and the command is MISSING on the Rust server** (re-probed 2026-08-20). Python returns the mongod shape (one `totals` entry per namespace, `total`/`readLock`/`writeLock`/per-op sections) and mongotop renders it like an idle server, but nothing instruments per-namespace operation timing, so every `{time, count}` is `0`. Real counters would need per-ns accounting in `Metrics` threaded through dispatch. **The Rust server answers `top` with code 59 CommandNotFound** — this entry previously read as a both-servers stopgap and hid that gap. `mongotop` against the Rust server therefore fails outright rather than rendering an idle server; the zero-counter shape is the cheap fix, real counters the honest one.
- ~~**`renameCollection` cross-process safety**~~ structurally guaranteed by WiredTiger (b34). Within-process atomicity is the storage `RLock`. Cross-process exclusion is `WiredTiger.lock` — a second `wiredtiger_open` on the same path fails with ``WT_ERROR Resource busy`` before any state is touched, so concurrent writers across processes / worktrees can't exist in the first place. See `tests/test_storage_exclusion.py`.
- ~~**`createIndexes` collation**~~ shipped (single-field b25 + compound b27). `sortkey.encode_value_directed` takes a `collation` kwarg; index entries are written under the index's stored collation; single-field equality / range / `$in` (`_find_leading_field_index`), compound bare-equality (`_pick_compound_eq_index`), and compound prefix + trailing-operator (`_pick_compound_range_index`) all thread collation through and gate by exact match. Unique-probe path reads each index's stored collation too. Strength 1/2/3 + `caseLevel` work uniformly across single- and compound-field indexes; `numericOrdering` still falls back to COLLSCAN at every level (would need a length-prefixed digit-run encoding to stay byte-sortable). See `docs/indexes.md` "Per-index collation".

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **find/aggregate firstBatch is count-capped but not byte-capped**: the getMore-default-batch slice gave `getMore` mongod's 16MB byte budget (both servers), but a FIRST batch is still capped only at its document count — `find` with `batchSize: 101` over 1MB documents assembles a ~101MB reply where mongod stops at 16MB and keeps the cursor. Apply the same byte budget in `find.rs::split_into_cursor` / `split_docs_into_cursor` (blob lengths are free) and `commands.py::_split_into_cursor` (needs encode-to-measure). Low urgency: drivers rarely combine a large explicit batchSize with megabyte documents, and the default 101-doc first batch only overflows with >160KB average docs.
- [ ] **Three-droplet DigitalOcean benchmark: two paths still unexercised**: `crates/secantus-bench` has now been run end-to-end against a live DigitalOcean account (2026-08-21) — provisioning, VPC + firewall, SSH, cloud-init, release-binary deploy, the on-droplet agent build, the timed run, result collection, and `destroy` teardown all work; two consecutive 120s runs agreed to 0.16% (6,220 and 6,230 ops/s aggregate). That first live run found four real bugs (curl body vs stdin, hopeless-SSH-retry, agent built from the server's release tag, sampler collection race), all fixed. **Still unexercised**: `--server-build source` (the on-droplet WiredTiger + cargo build, ~20 min) and the `snapshot` / `power-off` teardown modes — `destroy` is the default and is verified. See `bench/DO_CLUSTER.md`.
- [ ] **Admin UI saved-connections / settings page**: Slice 11 of the admin UI shipped schema sampler / logs viewer / geo viewer but skipped the planned `/settings` page with saved Mongo URIs and a manual dark/light toggle. The CLI today takes a single `--uri` per launch, so saved connections are bookmark-only (you can't switch targets after start). When the launcher gains hot-swap support, revisit this page — it's likely a small SQLite-backed list reusing the existing `~/.secantus/admin.db` store.

### 3.1 Authentication

SCRAM-SHA-256 is implemented end-to-end. The wire-protocol shape (saslStart/saslContinue, `hello.saslSupportedMechs`, per-connection auth state, `--auth` gating) is conformant for pymongo and mongo-go-driver. The remaining gaps are mostly orthogonal:

- ~~**MONGODB-X509**~~ — shipped on top of the b22 mTLS slice. Users with `mechanisms: ["MONGODB-X509"]` on `$external` (or `admin`) auth via cert-subject-DN-as-username; no password. The legacy `authenticate` command path (what pymongo / Java / Go / Node use for X509) is wired up alongside `saslStart`. See `docs/authentication.md` "MONGODB-X509" section.
- [ ] **LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** — remaining alternative auth mechanisms. Out of scope for now (each one is its own slice with its own external-system dependency).
- [ ] **Internal cluster auth (keyfile / x509)** — only meaningful with replica sets / sharding, both out of scope.
- ~~**`system.users` collection visibility**~~ shipped (b31). `admin.system.users` is a synthetic read-only view onto the `secantus_users` table; `find` / `aggregate` / `count` surface the stored records with their existing mongod-shaped fields (`_id`, `user`, `db`, `credentials`, `roles`, `mechanisms`). Writes are rejected with code 13 Unauthorized — mutate via `createUser` / `updateUser` / `dropUser`. Other dbs' `system.users` returns empty (matches mongod). See `storage._find_system_users` + `_count_system_users`.
- ~~**`system.version` `authSchema`**~~ shipped (b33). `admin.system.version` is a synthetic read-only view returning a single hard-coded doc: `{_id: "authSchema", currentVersion: 5}` (SCRAM-SHA-256 baseline, MongoDB 4.0+). Tools that read the auth-schema version on startup now get an honest answer. Writes rejected (code 13). See `storage._find_system_version`.

### 3.2 Change-stream limitations

Single-node change streams are implemented and conformant for typical pymongo `watch()` flows, but the following are deferred or intentionally diverge from real `mongod`:

- [x] **Rust server: DDL change-stream events (`showExpandedEvents`)** — DONE
  (0.5.3-beta.22). `create_index` / `drop_index` now emit an oplog `op: "c"`
  entry (`{createIndexes, indexes:[{v,key,name}]}` / `{dropIndexes, index}`), and
  `collMod` routes through a new `Storage::coll_mod` that writes the options AND
  emits `{collMod, …}` (plain `set_collection_options` stays oplog-silent so
  `create`'s internal option writes don't fire a spurious `modify`). The
  storage-side projector already gated `createIndexes` / `dropIndexes` behind
  `showExpandedEvents`; added the `modify` branch. A `showExpandedEvents` watch
  now sees `createIndexes` / `dropIndexes` / `modify`, and a default watch
  suppresses them.
- [ ] **Read concern / write concern semantics** — read concern is accepted and ignored. **Write concern is NOT merely ignored** (re-probed 2026-08-20): an unsatisfiable `w` is rejected exactly as mongod rejects it, `{code: 100, codeName: "CannotSatisfyWriteConcern", errmsg: "Not enough data-bearing nodes"}`, on **both** servers — `w: 2` against a one-member set raises rather than silently succeeding. That is the faithful behaviour, and the ruby-gauge `w:2` expected-fail below is a consequence of it. What remains genuinely accept-and-ignore is `j` / `wtimeout` / `w: "majority"` durability semantics, which a single node satisfies trivially.
- [x] **C-driver (`libmongoc`) change-stream gauge tests — NOW RUNNING.** The
  `/change_stream` + `/change_streams` suites were excluded because the C
  fixture bootstraps them through `test_framework_replset_member_count()`,
  which counts `replSetGetStatus.members`; SecantusDB answered the standalone
  `NoReplicationEnabled` error, so the helper saw 0 members and skipped every
  one. `replSetGetStatus` now reports the same one-member PRIMARY roster its
  own `hello` already advertised (gated on a set name being configured — a
  genuine standalone still gets the honest error), which resolves an
  inconsistency as well as enabling the suites: `hello` claimed replica-set
  primary while `replSetGetStatus` claimed standalone, and real mongod is
  never both. **32 change-stream tests now execute; 28 pass.** Four defects
  surfaced and were fixed: the resume-token error message and the
  pipeline-stage error code/message did not match mongod's, and the
  resume-token validity check caught only a *removed* `_id`, not a *modified*
  one (mongod allows "only transformations that retain the unmodified `_id`").
  Net C gauge 802 tests/10 fails -> 815/10 — same failures, 13 more tests.
  **All three follow-up gaps are now CLOSED** (`failGetMoreAfterCursorCheckout`
  + the `ResumableChangeStreamError` label): change-stream failures 3 -> 0, C
  gauge 10 -> 7 fails / 99.1%, 31 change-stream tests passing. The remaining 7
  failures are the long-standing ones the **Python** server shares, so the C
  gauge is back to zero rust-only failures WITH change streams covered. **Skipped as inherent:**
  `/change_stream/live/read_prefs`, `/Client/command_secondary` and
  `/Collection/aggregate/secondary` need a real SECONDARY, which a single-node
  surrogate has not got (multi-node is out of scope) — rationale recorded in
  `c_validation/include_paths.py::SKIP_TESTS`.
  **Follow-up 2026-08-21: the `replSetGetStatus` half of this was Rust-only until
  now.** The entry above reads as a server-wide fix, but only
  `handshake.rs::repl_set_get_status` reported the roster; `commands.py` kept
  answering the standalone error, so the Python server went on claiming
  `setName: secantus` in `hello` while telling `replSetGetStatus` it was "not
  running with --replSet". libmongoc counts `members` to classify the topology, so
  it treated the Python server as standalone and ran standalone-only tests against
  it — `/Client/last_write_date_absent{,/pooled}` asserted no `lastWriteDate` and
  found the replica-set-shaped one our `hello` supplies. Porting the roster to
  Python drops those two (now correctly `skip`), which is why the Python list was
  10 and the Rust list 7. **Triage of the remainder, reproduced 2026-08-21:**
  `/Client/select_server` ×4 is *inherent* — it asserts the server selected for
  reads is `standalone_or_rs_secondary_or_mongos` and we can only offer the
  primary, the same class as the pymongo/ruby secondary-readPreference
  expected-reds. Still open and genuinely fixable: `/Client/ipv6` ×2 (we bind
  `127.0.0.1` only, so the test's `::1` ping fails) and
  `/command_monitoring/unified/writeConcernError`, which unlike the "flaky, not
  deterministic" note above **reproduces every time when run alone** — worth
  re-triaging rather than trusting either description.
  **`/BulkOperation/OP_MSG/max_msg_size` is NOT in that list, and this audit
  briefly claimed it was.** Run by its exact name it PASSES, both forked and
  `--no-fork`, exactly as the 2026-08-11 re-diagnosis above says. It appeared to
  fail here only because the reproducer appended a `*` to the pattern, pulling in
  its `/BulkOperation/OP_MSG/*` siblings and reproducing the accumulated-state
  interference that entry already documents. The lesson is the entry's own: the
  framing "the server doesn't split" is wrong and chasing it wastes time.
- [ ] **Resume-token cross-server identity** — tokens are opaque to pymongo and round-trip fine, but the inner layout is `{s, t, n, k}` (BSON-encoded, hex-stringed) rather than mongod's keystring format. Tokens minted by SecantusDB cannot be presented to a real `mongod`, and vice versa.

### 3.3 MongoDB CLI / tool conformance tests

Every connectable tool in the MongoDB toolchain is now covered by tests in the
default suite (each starts an embedded `SecantusDBServer` and drives the real
binary; all skip gracefully when the tool isn't on PATH):

- `mongosh` — `tests/test_mongosh.py` (two-direction round-trip).
- `mongodump` / `mongorestore` / `bsondump` — `tests/test_mongodump_restore.py`
  (dump-restore round-trip incl. indexes; bsondump pins the extended-JSON dump
  format).
- `mongoimport` / `mongoexport` — `tests/test_mongoimport_export.py` (NDJSON +
  CSV round-trips, `--query`/`--fields`, `--drop`, canonical-extended-JSON type
  fidelity for ObjectId / datetime / Decimal128 / Int64 / Binary).
- `mongostat` / `mongotop` — `tests/test_mongostat_mongotop.py` (single
  iteration each; mongostat needed `serverStatus.mem`, mongotop needed the
  `top` command — both fixed on the cli-tools slice).
- `mongofiles` (GridFS) — `tests/test_mongofiles.py` (put/get/list/delete,
  cross-checked against pymongo's gridfs).

CI installs mongosh + Database Tools on the Linux and macOS `test.yml` cells
(apt repo / mongodb/brew tap) so the tool tests run continuously there.
Windows is intentionally uncovered: the mongosh tests skip on win32 by design
(PowerShell `--eval` quoting), and a choco database-tools install is slow and
flake-prone for the marginal remainder. Per-tool caveats live in the test
files' docstrings.

Compass is covered headlessly in `tests/test_compass_commands.py`: the full
command surface the GUI issues (instance probes, `$collStats` / `$sample` /
`$indexStats`, explain at both verbosities, performance-tab polls,
`atlasVersion` → CommandNotFound) is pinned without driving Electron. Driving
the actual GUI stays out of scope.

### 3.4 Multi-document transaction limitations

Real transactions shipped (`secantus.transactions` registry + per-transaction
WT sessions in `Storage`; statements run with the transaction's session swapped
into the thread-local, oplog entries buffered until commit). Conformance:
`tests/test_transactions.py` (pymongo-driven), `tests/test_transaction_registry.py`,
`tests/test_storage_user_txn.py`. Known divergences, all deliberate:

- [x] **Non-transactional writers DO block until the transaction ends — entry
  was wrong; re-measured 2026-08-11.** The claim below (bounded ~5s backoff,
  then 112 `WriteConflict`) does not reproduce on either server. Measured over
  the wire: a holder thread opens a transaction, updates `{_id: 1}`, sleeps,
  commits, while an outside client updates the same doc. The outside writer
  parks for the full life of the transaction and then **succeeds** —
  Rust `hold=2s → waited 1.73s`, `hold=8s → waited 7.72s`,
  `hold=20s → waited 19.72s`; Python `hold=8s → waited 7.71s`. All four end
  `{'txn': 'committed', 'outside': 'ok'}` with the final value reflecting both
  writes. 20s is well past the claimed ~5s deadline, so there is no deadline to
  hit. This matches mongod. Nothing to fix.
- [x] **Cross-transaction unique-index enforcement can leak — FIXED (#809).**
  Index-entry keys embed the RecordId, so two different docs violating the same
  unique constraint never collided on a WT key, and the check was a probe read
  through the caller's own snapshot — blind to a value an open transaction held
  uncommitted. A transaction plus a concurrent writer could both commit;
  reproduced 10/10 over the wire, silently, with both writers told they had
  succeeded. Fixed by porting the Python server's #775 design: a
  `secantus_unique_keys` table keyed on the VALUE alone (not value+RecordId),
  claimed with `overwrite=false` so WiredTiger refuses the second claim itself.
  Claims release only to their owning RecordId and are purged on collection /
  database / index drop (#808's false-rejection lesson). `_id_` is excluded —
  its uniqueness is already enforced by the `_id` index, and claiming it too
  double-wrote every insert. Verified both directions: commit ⇒ the outside
  writer gets DuplicateKey, abort ⇒ it succeeds; 0/25 plain-concurrency leaks
  and 0/10 drop/recreate false rejections.
- [x] **Failpoint-injected errors on in-transaction statements DO abort the
  transaction — entry was wrong; re-measured 2026-08-11.** Measured on both
  servers: `failCommand` injects `errorCode: 2` on an in-transaction `insert`,
  then a second statement and a commit are attempted. Both servers give
  identical, mongod-shaped results — injected statement `code=2`; the *next*
  statement is refused with `code=251` `NoSuchTransaction` +
  `errorLabels: ['TransientTransactionError']`; the commit likewise 251 +
  `TransientTransactionError`; and the collection ends holding only the seed
  doc, so no transaction write leaked. The label drivers key whole-transaction
  retry off is therefore already present and does **not** need to come from the
  failpoint's own `errorLabels` data. Note when probing this: use a non-SDAM
  `errorCode`. `11600` (`InterruptedAtShutdown`) makes the driver mark the
  topology unknown, so the follow-up fails on server selection and hides the
  transaction state you're trying to observe.
- [ ] **No `recoveryToken` / mongos pinning, no prepared transactions, no
  `maxCommitTimeMS`, no `serverStatus.transactions` metrics, no
  `afterClusterTime` enforcement** — multi-node machinery; out of scope.
  (`$clusterTime` / `operationTime` ARE gossiped on every reply so drivers
  can send `afterClusterTime`; the value is accepted and ignored.)
- [ ] **Expected-red in the pymongo transactions gauge**: the three
  secondary-read-preference unified tests
  (`TestUnifiedRunCommand::test_run_command_fails_with_*secondary*`,
  `TestUnifiedReadPref::test_secondary_readPreference`) fail with a
  client-side 30s `ServerSelectionTimeoutError` — the fictional
  single-node replica set has no secondary to select, and the asserted
  server error ("read preference in a transaction must be primary")
  can only come from a server that received the command. Unfixable on
  a single node.
- [ ] **readConcern levels inside transactions are accept-and-ignore** — every
  in-transaction read runs against the transaction's pinned WT snapshot
  regardless of level (`snapshot` is exactly that; `local`/`majority` are
  indistinguishable on a single node).

### 3.4 Honest-gauge remainder (post-94.8% triage)

The 2026-06-13 gauge-gaps slice fixed projection `_id`/array semantics,
`maxBsonObjectSize` enforcement, snapshot readConcern + `$$NOW`, and most of
the change-stream batch. Still open, precisely characterized:

- [ ] **Cursor/collection misc from the 64-list** (task #14 of the slice).
  Fixed in the gauge-misc slice (2026-06-13): embedded-document equality is
  now order-sensitive + exact (real matcher correctness bug — `query_embedded`
  / `query_array` examples), the `validate` command is implemented
  (clean mongod-shaped result + full/background rejection), and upsert with a
  `None` `_id` reports `did_upsert` correctly (real bug — `None` was the
  "no upsert" sentinel). Still open:
  - timeseries `insertMany` bulk path (`test_collection_management` timeseries).
  - the 3 `test_dbref.py` execnet failures: gauge-harness artifact (xdist
    can't serialize ObjectId in subtest reports — runner-side, not server).
  - `test_maxtime_ms_message` / `test_to_list_csot_applied`: pymongo CSOT
    client-side timeout formatting — not server-side.
  Out-of-scope rejections to keep: `$where`, text/hashed indexes (CLAUDE.md).
  Expected-red (single-node topology): the 3 `test_transactions_unified`
  secondary-readPreference tests.

### 3.5 Point-in-time recovery (PITR) — v2 and limitations

PITR shipped for the Python server: `secantus.oplog_replay` replays an oplog
source (a backup archive or a stopped data dir) into a fresh store, stopping at a
target `ts` / wall time; surfaced as `secantusdb restore` (CLI) and
`secantusAdmin.restoreToTimestamp` (wire). **v2 also shipped** (`secantus.pitr_archive`):
with `oplog_archive_dir` set, `prune_oplog` archives soon-to-be-dropped oplog rows
to durable segment files; `Storage.archive_base_snapshot` / `secantusAdmin.archiveBaseSnapshot`
take base snapshots; and `restore_from_archive_dir` (a directory `source` on the
CLI/wire) picks the newest base ≤ T and stitches archived oplog forward onto it,
lifting the genesis-intact restriction. `--preserve-oplog` carries the replayed
oplog for change-stream resume continuity, and collection options replay too.
Native Rust server PITR (Phase R) is **complete** (branch `rust-pitr`):
`secantusAdmin.backupArchive` + `secantusdb restore` (v1 + v2 archive-dir
sources, `--preserve-oplog`), `secantusAdmin.archiveBaseSnapshot`,
`--oplog-archive-dir` server flag, the `secantus_storage::{replay,pitr_archive}`
modules, the reverse `apply_update_description` in `secantus-core`, collection-
options carry, and `set_index_expiry`. Cross-server restore is byte-faithful both
ways (`tests/test_rust_pitr_cross_server.py`, `tests/test_rust_binary_pitr.py`).
Plan/history: `tasks/rust-pitr-phase-r-plan.md`.

### 3.6 Test-cycle performance (deferred levers)

Full analysis + the measured scaling curve live in
`tasks/test-performance-plan.md`. **Shipped** from that plan: I1 (stress test →
`@pytest.mark.slow` + a CI `slow` lane), I2a (opt-in fast test storage
`durable=False`, ~9% off the inner-loop wall, with a `SECANTUS_FORCE_DURABLE`
full-suite CI lane so checkpoint-durability stays covered), the storage
close-path use-after-free / double-close fix (`_session` /
`_reset_thread_session` / oplog readers now fenced against `_closed` under the
lock), and the `secantus-conn-*` stop-drain stack-dump diagnostic.
**Won't-do:** I4 (xdist balancing) — measured: the suite is I/O-bound with a
~177 s serial floor (fsync-bound) that no rebalancing can touch. **Already
done:** V1 (gauge build caching) — `validate.yml` already caches every
from-source build via `actions/cache`.

**Shipped — CI disk headroom (2026-07-10).** The `test.yml` durable-storage lane
(ubuntu only) runs the whole suite under `SECANTUS_FORCE_DURABLE=1`, and
`tests/conftest.py` pins `tmp_path_retention_policy="all"` (deleting a passed
test's WT home mid-session races WT's background threads → `WT_PANIC`), so
thousands of on-disk WiredTiger homes accumulate for the whole session. Each is
already minimised to ~10 MB (`log=(prealloc=false)` + `file_max=10MB` in
`Storage`), but the total approaches the runner's ~21 GB free and intermittently
hit "No space left on device" (grows with the test count). Fixed with a
Linux-only "Free up disk space" step in `test.yml` that reclaims ~15-20 GB of
unused preinstalled toolchains (android/dotnet/ghc/boost/swift/powershell)
before the build. If the durable lane keeps growing, the next lever is a smaller
per-instance footprint (not a mid-session delete — that reintroduces the panic).

**Shipped — oplog prune is O(deleted), not O(oplog size) (2026-07-24).**
`_prune_oplog_locked` used to decode every oplog entry on every run (once per
1000 writes) to find the few to drop; once a workload filled the oplog to the
100k cap (one big bulk insert does it) each prune wasted ~0.9s decoding rows it
then kept, and the tax landed on every later write in the session. Now it keeps
an in-memory live-row count (seeded by a key-only walk on open) and streams only
the oldest entries, stopping at the first the retention/cap criteria spare — a
prune that drops D rows reads ~D+1. One prune over a full oplog: 0.86s → 0.002s;
~27s off the serial pymongo gauge. See PR (branch `oplog-prune-scaling`).

Deferred, low value / risk — revisit only if inner-loop wall becomes a real
pain point again:

- **Python-server raw insert throughput (WT pure-Python packing).** After the
  oplog-prune fix above, `test_bulk::test_numerous_inserts` (200k inserts) is
  ~14.9s and now bottlenecked on the storage insert itself: profiling 100k
  inserts (6.5s) shows `wiredtiger/packing.py` + `intpacking.py` — the
  pure-Python `SSu`/`SSq` cursor-key packing, run several times per insert (doc
  table + nat + nat-seq + oplog) — dominating at ~2.9s cumulative. This is the
  known "use the Rust server for throughput" gap (§7); shaving it means touching
  the per-op cursor-key path for every write, which is high-risk for a modest
  win on a server whose throughput story is the Rust server. Not worth it now.
- **I2b — module-scoped server fixtures.** Convert the ~30 function-scoped
  `server(tmp_path)` fixtures to module scope (unique db/collection per test) to
  cut the *number* of WT open/close cycles. Modest upside (scaling curve shows
  the suite near its floor) and real risk: shared oplog / cluster-time across a
  module, so oplog / change-stream / reopen / capped / TTL-clock tests must stay
  function-scoped. Not worth it now.
- **V3 — `run-tests.php -j N` for the PHP-ext gauge.** Parallelise the ~712
  process-spawn `.phpt` runs. Blocked on flake risk: all `.phpt` hit one shared
  SecantusDB daemon — the contention CLAUDE.md flags at high concurrency. Needs
  a local PHP toolchain + a flake-diff-vs-serial run before adopting.
- **V2 — toolchain-daemon reuse in `validate-all`** (shared Gradle daemon across
  Java+Kotlin; dotnet incremental build; skip the gpg libmongocrypt re-verify
  when unchanged). Modest, and only affects the weekly CI wall.

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Real replica sets / sharding** — depend on cluster topology and cross-node consistency. SecantusDB advertises `setName: "secantus"` to satisfy pymongo's change-stream topology check, but the topology is fictional — there are no other members, no elections, no cross-node oplog. Change streams are still in scope (single-node, oplog-backed); see `## 3. Deferred work / Change-stream limitations`.
- ~~Authentication (SCRAM-SHA-256)~~ — implemented. `--auth` (CLI) / `require_auth=True` (constructor) gates non-handshake commands behind a successful `saslStart`/`saslContinue` round-trip. Provision users via `createUser`; manage with `dropUser` / `usersInfo`. The remaining auth gaps are tracked under `## 3. Deferred work / Authentication` below.
- ~~TLS / SSL~~ — implemented in b21+b22. `[tls] cert_file` + `[tls] key_file` enable server-side TLS; `[tls] ca_file` + `[tls] require_client_cert` add mTLS (transport-layer client verification). MONGODB-X509 cert-as-username auth landed in a later slice (see `docs/authentication.md`).
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo — complete and shipped.** Operators (`$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`) + `$geoNear` aggregation stage (auto-infer `key`, `includeLocs`), `2dsphere` (S2 cell coverings + ancestors) and `2d` (bit-interleaved geohash with quadtree-decomposed Z-order range covering) index acceleration, compound geo+scalar indexes (geo cell scan + verifier-step filter on trailing scalars), legacy mongod sibling-form `$maxDistance` / `$minDistance` for `$near` / `$nearSphere`, and write-time input validation (out-of-range coordinates / unparseable shapes reject with mongod's documented code 16572 across insert / update / upsert / createIndex). See `src/secantus/geo.py` + `src/secantus/geo_index.py`. **Validation surface**: 79 in-tree pymongo tests in `tests/test_geo*.py`; 3 cross-driver smoke tests through mongosh, mongo-node-driver, and mongo-go-driver in `tests/test_geo_cross_driver.py` (all pass — wire-protocol geo path is clean across drivers); the pymongo conformance gauge keeps `test_collection.py`'s built-in geo tests at 100%; mongo-java-driver's `GeoJsonFiltersFunctionalSpecification` + `GeoFiltersFunctionalSpecification` upstream specs both pass 10/10 in the java gauge's `:driver-core:test` module. **Out of scope**: exact mongod error-string matching (chase work without a clear payoff unless a driver test pins exact wording).
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library — `$cond` / `$switch` / `$let` / `$map` / `$reduce` / `$filter` / `$regexFind` / `$concat` / `$substrCP` / `$dateToString` / etc. — which is more expressive than people often realise. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (CLAUDE.md "pymongo cannot tell SecantusDB apart from mongod" — pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is actually worse than embedded JS engines (no per-context isolation primitives, no reliable cross-platform CPU-time interrupt, `RestrictedPython` is an AST rewriter not a true sandbox). If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`. Also explicitly deprecated by MongoDB (removed from the Stable API in 5.0; recommended migration path is aggregation pipelines). `commands._map_reduce` recognises the canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern (the shape mongo-java-driver's `testMapReduceWithGenerics` test exercises) and translates it to an equivalent `$group` aggregation. Non-canonical map / reduce bodies return `{results: [], ok: 1}` so wire-shape probes pass, and `out: "<coll>"` (non-inline) is rejected with FailedToParse. Anything that genuinely needs JS evaluation needs a real `mongod`.
- ~~Capped collections~~ — implemented. `create capped: true, size, max` accepted; `Storage.insert` and `Storage.update_matching` enforce FIFO eviction by walking the doc table in natural order and evicting oldest non-fresh docs while bounds are exceeded. `listCollections` surfaces `options.{capped,size,max}`. Eviction emits oplog `op:"d"` entries (and pre-images when enabled) so change streams observe the deletes. Both servers now do **true FIFO**: `_enforce_capped_bounds_locked` walks the natural-order index (`_scan_docs_natural`), so the oldest-inserted doc is evicted first regardless of `_id` monotonicity (matching mongod). Rust: beta.92, `scan_docs_natural` — see §7.3.
- ~~Profiling~~ — implemented. `profile` command (-1 / 0 / 1 / 2 with `slowms` + `sampleRate`) sets per-database state in `secantus_profile_settings`. Dispatch wraps each non-skip command in `time.monotonic_ns` timing; if the per-DB level matches, an entry is inserted into `<db>.system.profile` (auto-created capped 10 MB). Recursion guard skips ops against `system.profile` itself + handshake / cursor-continuation / profile-itself commands. Entry shape mirrors mongod (`ts`, `op`, `ns`, `command`, `millis`, `ok`, `client`, optional `user`, `errMsg` / `errCode` on failure). Out of scope today: `planSummary` / `keysExamined` / `docsExamined` / `nreturned` (would need post-handler stats plumbing).
- ~~Tailable / awaitData cursors~~ — implemented for change streams (see "In scope" in `CLAUDE.md`) **and** for plain capped collections + `local.oplog.rs` (`commands._find_tailable` / `_find_tailable_oplog`, blocking `getMore` on the oplog condition variable). The producer re-applies the find filter (with `let` vars + collation) to follow-up inserts, advances its watermark by **RecordId** (insertion order — the same order capped eviction uses; an `id_key` watermark dropped and redelivered docs when `_id`s weren't monotonic), and raises `CappedPositionLost` (136) on rollover.

## 5. Known bugs and edge cases to watch

- [x] **DATA CORRUPTION: retryable writes were not idempotent — FIXED on BOTH
  servers (#844 Python, #850 Rust).** Found 2026-08-13 while
  triaging the C gauge's `/command_monitoring/unified/writeConcernError`.

  mongod persists the outcome of every retryable write keyed by
  `(lsid, txnNumber)` (its `config.transactions` collection). When the same
  `lsid` + `txnNumber` arrives again — which every official driver sends
  automatically after a network blip, a `writeConcernError`, or a stepdown —
  mongod recognises the repeat and returns the **stored** result without
  re-applying the write. SecantusDB does neither: `_txn_envelope` parses the
  envelope and `TransactionRegistry.on_retryable_write` merely consumes the
  txnNumber sequence. Nothing caches the result; nothing detects a repeat. The
  write simply runs again.

  Reproduced over the wire, same command sent twice with one `lsid` /
  `txnNumber` (`{"$inc": {"n": 1}}` against `{_id: 1, n: 0}`):

  ```
  first  reply nModified: 1
  retry  reply nModified: 1
  final stored value: 2        <-- mongod stores 1
  ```

  **Identical on the Python and Rust servers.** The insert case is loud (the
  retry hits `E11000`, which is what the C test sees). The update case is
  SILENT: the value is doubled and both replies claim `nModified: 1`, so the
  client is told exactly one update happened. Any non-idempotent operator —
  `$inc`, `$push`, `$addToSet`-on-array-growth, `$mul` — corrupts data with no
  error surfaced to anyone. This is the "never ignore an error — this is a
  database" category: a user loses data integrity and gets a success reply.

  **Fixing it properly** means implementing mongod's retryable-write record:
  persist `(lsid, txnNumber) -> {reply, opTime}` per statement (mongod keys
  per statement id within a batch, so a partially-applied `insert` batch
  retries only its missing documents), return the cached reply verbatim on a
  repeat, and expire records the way mongod's 30-minute
  `transactionLifetimeLimitSeconds` sweep does. It must land on **both**
  engines together plus storage, so it is a slice of its own, not a
  conformance-gauge cleanup. Deliberately deferred here rather than
  half-implemented — a partial version that caches some operations and not
  others would be worse than the current honest-but-wrong behaviour, because
  it would look fixed.

  **Status (2026-08-13).** The Python server now keeps the record: dispatch
  looks up `(lsid, txnNumber)` plus a digest of the command body before
  executing a retryable write, replays the stored reply on a match, and
  records the outcome on the way out (`TransactionRegistry.retryable_reply` /
  `record_retryable`). Verified over the wire: a retried `$inc` now leaves 1,
  a retried insert no longer self-collides with `E11000`, and a retried
  `$push` appends once.

  Three deliberate limits, none of which make it *look* fixed while being
  broken:

  * **Only fully-successful writes are recorded.** A failed or partially-failed
    write (`writeErrors` present) is not, so its retry genuinely re-executes —
    caching a failure would make a transient error permanent. A
    `writeConcernError` IS recorded: the write applied, only its replication
    did not confirm, so a retry must not apply it twice.
  * **No per-statement-id granularity.** mongod records each statement in a
    batch, so a partially-applied `insert` batch retries only the missing
    documents. We record whole-command outcomes, so a partially-failed batch
    re-runs entirely — exactly as it does today, not worse.
  * **The command identity is part of the key.** If the same
    `(lsid, txnNumber)` is presented with a *different* command we execute
    rather than replay, because serving one write's reply for another would be
    worse than the double-apply this prevents. (Found the hard way: pymongo
    OVERRIDES an explicit `lsid` on `db.command()` with its own implicit
    session, so hand-built probes collide on one session.)

  **The Rust server is fixed too** (#850, 2026-08-14): the same record, keyed
  on `(lsid, txnNumber, identity)`, lives in `TransactionRegistry`
  (`crates/secantus-commands/src/transactions.rs`) and is consulted from
  `run_with_txn_envelope`. Verified at the WIRE level against a release
  `secantusd-rs` — a retried `$inc` leaves 1 (was 2) and a retried insert no
  longer self-collides with `E11000`. The digest algorithm deliberately
  differs from Python's (SHA-256-truncated vs SHA-1): records are per-process
  and never shared, so only "same command => same identity" matters.

  The two servers agree again and the three limits above apply to both. The C
  gauge's `writeConcernError` test should now pass on both — worth confirming
  on the next gauge refresh rather than assuming.

- [ ] **`invoke concurrency-refresh` needs a genuinely quiet box AND a spread
  audit before its output is trusted.** Two consecutive refresh runs
  (2026-08-02) were silently poisoned by a parallel session's gauge load —
  medians-of-3 didn't protect (whole interleaved passes degraded 10-30x,
  mongod itself read 10.5k vs its healthy 104k). A load<3 gate at START is
  insufficient; contention arriving mid-run wrecks all arms. Before accepting
  a refresh: check every `runs_docs_per_sec` cell for >2.5x intra-cell spread
  and re-run (or keep the previously committed clean numbers) when flagged.
  Worth upstreaming the audit into the task itself.

- [x] **RESOLVED (fired for real in the 0.6.0b8 release gate, 2026-08-02): the 25-min watchdog hard-exited five healthy workers when a shared-box suite run took 25:11 — local default now 90 min, CI pins 1500 explicitly (test.yml).** Original entry: the 25-min hang watchdog's own dump can wedge a
  worker (observed live 2026-07-31).** On a box loaded enough that a session
  crosses `SECANTUS_HANG_SECONDS` (1500s), `faulthandler.dump_traceback_later`
  fires while the worker is still healthy; the dump thread walks live threads'
  frames without the GIL and was sampled hot-spinning in
  `_Py_DumpTracebackThreads → _PyCode_CheckLineNumber` (CPython race, not ours).
  It then never releases faulthandler's `running` lock, so the session-fixture
  teardown's `cancel_dump_traceback_later()` blocks forever — the worker looks
  "wedged after its last test". Mitigations landed: the nested-run wedge
  tolerance in `test_crash_stall_watchdog.py` now actually matches `-q` output,
  and the hard-kill harness kill is ack-driven. The dump-spin itself is
  upstream; if it recurs, consider raising `SECANTUS_HANG_SECONDS` locally or
  `exit=True`-only dumps to a file (never a pipe).

- [x] **RESOLVED (bisected 2026-07-30): the small-doc one-shot insert
  "regression" (0.8× → 1.2× mongod between #666 and #703) is a COST SHIFT
  from PR #680's lazy shard creation, not a hot-path regression.** Bisect
  (insert-only micro-harness mirroring `compare_servers`, PGO disabled at
  every point for comparability, 5 reps/point, medians): #664 62.5ms →
  #674 68.6 → **#679 61.7 (clean) → #680+#681 86.9 (+41%)** → #696 84.0 →
  #700 88.5 → #702 83.9 → current 83.4. Mechanism confirmed by a warmup
  probe: a single 1-doc insert before the timed 10k-doc run drops current
  code from 84.6ms back to 64.9ms — the benchmark times a FRESH store's
  first-ever writes, which post-#680 absorb the one-time lazy table
  creates (~10.6ms each: doc shard, oplog shard) plus first-WAL
  allocation that eager creation used to pay at open. That shift is
  #680's entire point (eager creation's ~500ms/37-table opens saturated
  disk under the parallel suite); steady-state insert cost is
  unregressed, and sustained-throughput measurements improved throughout
  (Findings 12-13). `docs/benchmark.md` now explains the number instead
  of calling it a regression. Optional future nicety (small): pre-create
  the two route-default oplog shards on a background thread at open to
  hide ~10ms of first-write latency — cosmetic, not queued.

- [x] **ws-changes flake — THREE causes, all now fixed (2026-07-22 / 2026-07-26).
  Two lived in the test and the admin router; the third was silent event loss in
  the server itself.** Confirmed by a controlled repro (`crash-repro.yml`,
  `mode=ws-single`, run 29877050313, attempt 2/3), which caught the
  `+++ Timeout +++` stack the previous seven occurrences had all lost. The
  chain, verified end to end:

  1. `test_ws_changes_quiet_stream_closes_cleanly_and_app_stays_healthy` calls
     starlette `TestClient`'s `ws.receive_json()`, which takes **no timeout** and
     blocks forever. The dump has MainThread parked in
     `testclient.py:receive → portal.call(self._send_rx.receive)` at
     `tests/test_admin_skeleton.py:973`, waiting for an event that never came.
  2. At 120 s, `--timeout-method=thread` (what every lane uses) fires. That
     method writes its stack to the worker's **stderr** and then calls
     `os._exit()`.
  3. xdist **discards a dead worker's stderr**, so the stack vanished; `os._exit`
     is not a signal, so faulthandler wrote nothing; no memory was involved, so
     `dmesg` was clean. Net symptom: `node down: Not properly terminated`, no
     `FAILED` line, no stack, no OOM — for seven occurrences.

  **Second (deeper) cause — a real product bug, found because the fix above made
  the hang visible.** Once the receive was bounded, PR #603's CI reported
  `no websocket frame within 30.0s` instead of an anonymous death — and that named
  a genuine event-loss race: the admin ws tail
  (`admin/routers/changestream.py`) sent its `{"type":"open"}` frame **before**
  calling `target.watch()`. The frame promises the client that writes from here on
  are observed, but the stream's start point isn't fixed until `watch()` returns,
  so any write in that window is lost — and the admin UI has the same race (open
  the change-stream page, write immediately, miss the event). The tests do exactly
  that (`open` → insert → await), so under CI load the insert fell in the window
  and the awaited event never came. **Fixed (#603, second commit): establish the
  stream first, then send `open`.** So the causality is the reverse of the whole
  investigation's assumption — the test wasn't hanging for no reason; there was a
  real race, and the unbounded receive is what turned it into an anonymous crash
  instead of a visible failure.

  **Fix:** `tests/test_admin_skeleton.py` now wraps every event-awaiting receive
  in `_recv_json(ws, timeout=30)` (a daemon-thread pull with a deadline), so a
  stream that produces nothing fails as a normal assertion instead of vanishing a
  worker. Receives inside `pytest.raises(WebSocketDisconnect)` keep blocking
  semantics deliberately — their frame is a close, which arrives promptly. Plus
  the `open`/`watch()` reorder above.

  **Same anonymous signature, a THIRD independent trigger — cross-driver build
  timeouts (#605).** 49 tests in `test_cross_driver_features.py` /
  `test_geo_cross_driver.py` wrap toolchain builds (`npm install`,
  `cargo build --release`, a Maven/Gradle jar, `bundle`/`composer install`) whose
  budgets are 300–600 s, under the lanes' `--timeout=120`. On a cold cache the
  test that draws the build is killed → `os._exit()` → the identical anonymous
  worker death, naming a *random smoke test that is not a bystander but the one
  that drew the build*. Fixed with a module-level `pytest.mark.timeout(600)`. The
  general rule (`--timeout-method=thread` + xdist discarding worker stderr turns
  any over-long wait into an anonymous native-looking crash) is now the lead entry
  in the `/ci-check` pattern catalog.

  **Two hypotheses this kills, both of which had real evidence behind them:**
  * **OOM / the admin-fixture memory leak.** Refuted by measurement: zero `dmesg`
    OOM lines, peak RSS 1218–1333 MB against a 3 GB cap that never bit. The leak
    is real (+206 MB per 113 tests, halved in #597) and worth fixing on its own
    merits, but it is **not** this.
  * **A WiredTiger / server-teardown race** (this item's prime suspect for
    months). The storage layer is exonerated: the connection thread in the dump
    is sitting in `_get_more`'s **bounded** `_oplog_cv.wait_for`, behaving
    correctly on a genuinely quiet stream.

  **THIRD cause — the server was throwing the event away (fixed 2026-07-26,
  #667).** After the two fixes above the test still failed intermittently, now on
  Windows lanes, with the honest message the bounded receive gives us: `no
  websocket frame within 30.0s`. The traceback names the *second* receive, so
  `open` had arrived — the stream was established and the insert still never
  came. Not a websocket bug at all.

  `commands.py`'s change-stream producer skips the cursor to the oplog tail when
  a poll finds nothing for the watched namespace (so a quiet collection's PBRT
  can move past unrelated activity). That skip was bounded by
  `storage.oplog_tail_seq()`, read *after* the scan, which loses writes two ways:
  one committing between the scan and the tail read is counted by the tail but
  was never examined; and the tail is the highest seq **minted**, which a writer
  takes before committing its batch without holding `_lock`, so it can name an
  entry no reader can see yet. `position_seq` only moves forward, so either way
  the event is gone permanently — no later poll returns it.

  Fixed by bounding the skip with what the poll actually examined:
  `_merge_oplog_on_session` reports `scan_high` (highest seq walked, including
  filter-rejected entries), exposed as `Storage.read_oplog_scan`. Note that
  snapshotting the tail *before* the scan is NOT sufficient — it closes the first
  gap but not the minted-uncommitted one. **The Rust server never had this**: its
  `change_stream_poll` reads unfiltered and advances only over scanned rows.
  Regression: `test_change_streams.py::test_empty_poll_cannot_skip_a_write_that_
  lands_mid_scan`, which commits a write from inside the producer's own storage
  calls and fails (times out) on pre-fix code.

  **Lesson for the next one of these:** two plausible root causes had already
  been found and fixed, and both were real — which made it easy to read the next
  recurrence as "the same flake again". It wasn't. A test that keeps failing
  after a correct fix deserves the same suspicion the first failure got.

  **Method note worth keeping:** the diagnostic that finally worked was removing
  xdist (`-n0`), not adding more instrumentation — the evidence existed all along
  and was being thrown away by the harness that collected it. When a failure is
  persistently anonymous, suspect the collector before the code.

- [ ] ~~**ws-changes xdist worker crash (Linux CI, recurring).**~~ (historical
  investigation record for the item above; kept for the reasoning trail)
  `tests/test_admin_skeleton.py::test_ws_changes_streams_collection_event`
  intermittently hard-crashes its Linux xdist worker ("Not properly
  terminated", ~11-12 min into the lane) and the REMAINING workers' progress
  freezes at the same moment until the 25-min conftest watchdog kills them,
  blaming bystander tests. Hit twice on 2026-07-17 (PR #451 durable lane, PR
  #456 test lane — unrelated branches); reruns pass. Same suspect as the
  macOS 6-hour-hang entry below: the admin UI websocket change-stream tail
  (a previous occurrence pinned a core for 15+ minutes). Needs a root-cause
  slice: reproduce under CPU starvation (the local box under parallel-session
  load may do it), get the faulthandler dump from a first-attempt log, and
  find both the crash cause AND why sibling workers stall (shared resource?
  admin server port? WT cache pressure?). Do not deselect the test — this is
  a real defect signal in the ws tail. Pattern catalogued in /ci-check.
  **Hit a THIRD time 2026-07-17 (PR #468 durable lane).** Mechanism candidate
  (from reading `src/secantus/admin/routers/changestream.py`): the tail loop is
  `while True: await asyncio.to_thread(stream.try_next)`. On websocket
  disconnect the awaiting coroutine is cancelled, but a thread-pool thread
  already blocked *inside* `stream.try_next()` (a blocking getMore on the
  tailable cursor) cannot be interrupted — it returns only when the getMore
  does (the server's 1s default awaitData wait, or longer under CI load). If
  the sibling SecantusDB server thread's parked `wait_for_oplog` isn't woken,
  that getMore — and its pool thread — can hang until the executor is torn down
  at worker shutdown, crossing the 25-min watchdog. Candidate fix (needs a
  repro to validate, do NOT blind-apply): bound the getMore wait
  (`max_await_time_ms` on `watch()`) so `try_next` always returns promptly, and
  ensure the `finally` `stream.close()` interrupts a parked cursor. A
  first-attempt faulthandler dump would confirm whether the wedged thread is
  this `try_next` pool thread.
  **Hit a FOURTH time 2026-07-20 (PR #586, `test (3.10, ubuntu-latest, 3)`).**
  Same signature: worker `gw2` "Not properly terminated" at ~97%, sibling
  workers froze, the controller's post-crash-overrun watchdog fired `os._exit(70)`
  ~5 min later. The first-attempt faulthandler dump was captured but **did not**
  confirm the `try_next` hypothesis — the crashed worker's own stack died with
  it (`pytest_testnodedown` only carries bystander controller/execnet threads),
  so the dump names no culprit. Rerun of the single failed shard passed clean.
  Takeaway: `pytest_testnodedown`'s crash-reason capture doesn't recover the
  dead worker's Python stack on Linux either — to actually pin this, the next
  slice needs the crash reproduced under a worker running with `faulthandler`
  armed *inside* the worker (not just the controller), e.g. via a low
  `--max-worker-restart` + core-dump capture, or by running the ws-changes test
  file alone under CPU starvation.
  **Investigation 2026-07-21 (findings, no fix — the code guidance to not
  blind-apply stands).** Read the tail loop (`admin/routers/changestream.py:99-121`):
  `stream = await asyncio.to_thread(target.watch)` then a loop of
  `await asyncio.to_thread(stream.try_next)`, with `finally: await
  asyncio.to_thread(stream.close)`. Two real defects confirmed by code reading:
  (1) **no `max_await_time_ms` on `watch()`** — each `try_next` blocks on the
  server's ~1s default tailable await, and `asyncio.to_thread` CANNOT interrupt
  the running pool thread on cancel, so a disconnect waits ~1s+ for the in-flight
  `try_next` to unwind (measured: a single disconnect adds seconds of latency;
  100 sequential iterations do not finish in 5 min). (2) **`close()` can run
  concurrently with a still-in-flight `try_next`** on the same pymongo
  `ChangeStream`, which pymongo documents as not thread-safe.
  **Could NOT reproduce the crash on macOS**, which is the key negative result:
  a direct two-thread harness (`scratchpad/repro_concurrent.py`: `try_next` loop
  on one thread + `close()` on another against one `ChangeStream`) survived
  **500 rounds even under 12 CPU-hog processes on a 12-core box** — zero crashes,
  thread count flat at 10 (no leak). So the crash is **not** a macOS-reproducible
  pymongo client-side concurrency segfault; it is Linux+load specific. Refined
  prime suspect given WT is involved and it's Linux-only: the WT-level race
  between the test's `SecantusDBServer` (the `server` fixture) tearing down and a
  change-stream `getMore` still parked on that server's oplog — a server-side
  analogue of the client `stop()` use-after-free fixed in 0.5.3b5, reachable via
  the admin ws tail's separate `MongoClient`. **The documented candidate fix
  (bound `max_await_time_ms`, serialize close vs try_next via a per-connection
  single-worker executor) is correct on its own merits and would remove both
  defects — but per the "do NOT blind-apply, needs a repro" note it is NOT
  applied here, since I can't verify it kills the Linux crash and the change
  touches a live path.** Concrete unblock: the fix needs a Linux repro (this box
  is macOS). Best next step is a CI-only diagnostic — arm `faulthandler.enable()`
  writing to a per-worker file uploaded as a CI artifact (pytest's faulthandler
  writes to worker stderr, which xdist drops on "node down") — so the next
  Linux occurrence dumps the faulting thread's C stack and finally names the
  culprit. (The macOS repro harness that stayed clean: a two-thread loop —
  `coll.watch()`, then one thread calling `stream.try_next()` repeatedly while
  another calls `stream.close()` after a 50 ms head start — 500 rounds under 12
  background CPU burners; trivial to re-create on a Linux box to attempt the
  repro there.)
  **MITIGATION MERGED — PR #588 (commit `f7c953b7`).** The two defects the
  2026-07-21 investigation confirmed by code reading (no `max_await_time_ms`,
  and `close()` racing an in-flight `try_next` on a non-thread-safe pymongo
  `ChangeStream`) are both fixed in `changestream.py`: (a)
  `watch(max_await_time_ms=500)` bounds every poll so an orphaned `to_thread`
  frees within ~0.5s; (b) each `try_next` poll races a `_wait_for_disconnect`
  watcher so a *quiet* stream notices a gone client immediately (the old loop
  only detected disconnect on the next `send`, so a quiet-then-closed stream
  looped until app shutdown); and (c) on disconnect the in-flight poll is
  **awaited to completion** before the `finally` closes the stream, so
  `try_next` and `close` never overlap (removing the concurrency race). These
  are genuine correctness fixes — but they are **NOT the crash's cause.**

  **DECISIVE NEW EVIDENCE (2026-07-21): the crash RECURRED on #588's own CI
  with the full mitigation in place** — `test-durable (3.10, ubuntu-latest, 1)`
  had *two* workers (`gw0`, `gw3`) go down "Not properly terminated", exit 70,
  no test-failure names (rerun passed clean). So the admin-side defects #588
  fixed are conclusively **ruled out** as the cause. Combined with the crash
  recurring on branches that never touch `changestream.py` (#451/#456/#468/#586),
  the real cause is server-/harness-side, not the admin ws tail. **Item stays
  open; root-cause work below.**

  ### REPRODUCED 2026-07-21 (run 29873889113) — OOM ruled OUT, culprit named

  The dispatchable `crash-repro.yml` job reproduced it on **attempt 3 of 3**
  (`[gw3] node down: Not properly terminated`), under `SECANTUS_FORCE_DURABLE=1`,
  8 CPU burners and a 3 GB `systemd-run` MemoryMax. Four hard results:

  1. **NOT an OOM kill.** `dmesg` carried **zero** OOM lines, and peak combined
     RSS was **1218–1333 MB against the 3 GB cap** — the ceiling never bit. The
     OOM hypothesis (from the measured admin-fixture leak + the absent
     faulthandler dumps) is **refuted**. The leak is real and worth fixing on its
     own merits, but it is **not** this crash.
  2. **NOT a fatal signal.** All five faulthandler dumps were **0 bytes**,
     including the dead worker's. So not SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL,
     and (per 1) not SIGKILL either.
  3. **The culprit is named, for the first time in seven occurrences:**
     `tests/test_admin_skeleton.py::test_ws_changes_quiet_stream_closes_cleanly_and_app_stays_healthy`
     (`worker 'gw3' crashed while running ...`). The admin/ws attribution was
     right; it is now a single test rather than a whole file.
  4. **Leading mechanism — a HANG, not a crash (inferred, not yet observed).**
     The lanes run `--timeout=120 --timeout-method=thread`. pytest-timeout's
     thread method writes its stack to the worker's **stderr** — which xdist
     discards on "node down" — and then calls `os._exit()`. That is abrupt with
     **no signal** (so faulthandler stays silent) and **no memory pressure** (so
     dmesg stays clean): precisely the evidence pattern above, and the reason
     every occurrence has been anonymous. The attempt ran 191 s, leaving room
     for a 120 s timeout. **The `+++ Timeout +++` marker was NOT directly
     observed** — confirm before treating as fact.

  **Confirmation available:** dispatch `crash-repro.yml` with `mode=ws-single`,
  which runs the admin file alone with `-p no:xdist` so nothing discards stderr
  (optionally `timeout_method=signal`, which raises inside the test instead). If
  the timeout stack appears, this stops being "a native worker crash" and becomes
  "a hang in the ws quiet-stream close path" — a tractable bug in a known place.

  ### Root-cause work — the real remaining fix (OPEN)

  Signature is always the same: one or more Linux xdist workers "node down: Not
  properly terminated" (a hard native process death — segfault / abort /
  WT_PANIC whose stderr xdist discards), late in the lane, siblings then stall.
  It does **not** reproduce on macOS (this dev box): 6× parallel ws-test runs +
  a two-thread `try_next`/`close` harness under 12 CPU burners all stay clean.
  Concrete next steps, in order:

  1. **Get the faulting stack — the diagnostic is now armed (#590).** Per-worker
     `faulthandler` output is captured as a CI artifact (pytest/xdist drop the
     crashed worker's stderr on "node down", which is why every prior dump —
     `pytest_testnodedown` — only carried bystander controller/execnet threads
     and named no culprit). The next Linux occurrence should upload the *faulting
     worker's own* C/Python stack. **Do not attempt another speculative fix
     before a real stack exists** — the "do NOT blind-apply, needs a repro" note
     has now been vindicated twice.
  2. **Prime suspect — server-side WT teardown racing a parked `getMore`.** Same
     "node down" signature and late timing as two already-fixed crashes: the
     unclosed-`Storage` fd/RSS leak (#563, the resolved macOS 6-hour hang below)
     and the 0.5.3b5 `SecantusDBServer.stop()` use-after-free. Both are fixed and
     this recurs *after* them, so it is a **distinct residual**. Hypothesis: the
     ws test's `server` fixture (a `SecantusDBServer`) tears down while the admin
     UI's *separate* `MongoClient` still has a change-stream `getMore` parked on
     that server's oplog condvar; if `stop()`'s drain doesn't wake/await *that*
     parked getMore under Linux load, a connection thread touches WiredTiger
     after close → native crash. Audit `SecantusDBServer.stop()` /
     `Storage.signal_shutdown` against the admin ws teardown path; check whether
     the admin `MongoClient` is closed *before* the server fixture stops (the
     `app` fixture's `app.state.mongo.close()` ordering vs the `server` fixture
     teardown).
  3. **Also re-audit for the #563 leak class specifically on the ws/admin path.**
     #563 fixed three fixtures; confirm no admin/ws fixture (or the embedded
     `SecantusDBServer` the ws tests spin up) leaks a `Storage`/client/fd under
     the durable lane, which is where the recurrence landed.
  4. **Reproduce on Linux.** Run `tests/test_admin_skeleton.py` alone, `-n auto`,
     under 12+ CPU burners on a Linux host, `SECANTUS_FORCE_DURABLE=1`, low
     `--max-worker-restart`, with the #590 faulthandler artifact armed. A crash
     there with a real stack closes the investigation; only then apply a fix.

  ### Measurement session 2026-07-21 — step 3 answered NO, and a real
  ### unbounded memory leak found in the admin fixtures (not yet proven to be
  ### the crash cause)

  Hit a **fifth** time on PR #595 (`test (3.10, ubuntu-latest, 1)`, worker `gw0`
  "Not properly terminated" at ~78%, controller `os._exit(70)` 5m44s later,
  rerun clean). Ran the step-3 audit with real instrumentation rather than code
  reading. Results, all on macOS with `-n0` (the *leak* is measurable even where
  the *crash* is not):

  - **fds are FLAT — step 3 is answered, negative.** `tests/test_admin_skeleton.py`
    (113 tests): fd count at each test's setup is 12 → 12, net **+0**; threads net
    +2 (the asyncio default executor pool). So the #563 fd-exhaustion class is
    **ruled out** for the admin/ws path. (Measure at *setup*, not teardown —
    fixture finalizers run during the teardown phase, so a teardown-time sample
    still counts the fixtures' own fds and shows a fake "+61 per test".)
  - **RSS grows without bound: +206 MB over those 113 tests (+1.8 MB/test),
    linear, no plateau** (130 MB at test #10 → 256 MB at #110). Control:
    `tests/test_change_streams.py` (server fixtures, sync, no admin app)
    **plateaus** at 84 MB (+0.38 MB/test, flat after warm-up). So the growth is
    specific to the admin file, not to `SecantusDBServer` generally.
  - **What is retained** (gc-reachable instance counts over the run):
    `FastAPI` 0→94, `MongoFacade` 0→94, `AsyncClient` 0→94,
    `_UnixSelectorEventLoop` 0→100, `Task` 0→294, `Future` 0→200,
    `StopAsyncIteration` 0→94 — i.e. **one event loop leaked per test**, pinned
    by never-completed awaitables, pinning the async-generator fixture frames
    (`dict(keys=['app']) <- frame <- traceback <- StopAsyncIteration <- Future`),
    which pin the app + facade. `Storage`/`SecantusDBServer`/`CursorRegistry`
    leak only 6 each, so whole servers are *not* the bulk. 94 apps × ~2 MB
    accounts for essentially all of the +206 MB.
  - **Deterministic 60-line reproducer** (bisected; kept in the session
    scratchpad, trivial to recreate): fixtures `server` (real `SecantusDBServer`)
    → `app` (real `create_app`) → `http` (async `AsyncClient` over
    `ASGITransport`), 30 parametrised `GET /healthz` tests. Retains **28 loops +
    28 apps out of 30**. Two controls isolate it: a *trivial* ASGI payload with
    the identical async-fixture shape retains **0** (so the async-fixture /
    anyio pattern is innocent), and `create_app` **without** a real server
    retains **0** (so it needs the actual mongo round-trip through the sync
    endpoint's threadpool hop).
  - The cycles are **collectable but never collected** during a normal run —
    adding a `gc.collect()` at every test *setup* drops retention to 0 while RSS
    still grows +1.51 MB/test. A single `gc.collect()` in the fixture teardown is
    NOT enough (24/30 still retained) because the fixture frame still holds the
    app at that moment.
  - **Partial fix APPLIED (leak only, not the crash):** the `app` fixture now
    releases the payload at teardown (`routes.clear()`,
    `user_middleware.clear()`, `middleware_stack = None`,
    `state._state.clear()` after `mongo.close()`). Measured on the real file:
    **+206 MB → +91 MB** (1.82 → 0.80 MB/test), 113/113 still passing; on the
    reproducer, 28→5 retained. This is landed **on its own merits as a memory-leak
    fix** — it is explicitly **NOT** claimed to fix the worker crash, and this
    item stays OPEN. The standing rule (no speculative *crash* fix without a
    repro of the crash) is intact: nothing in the product changed, only test
    fixture hygiene. The residual +0.80 MB/test is still unbounded and still
    wants the loop-pinning root cause.
  - **Why this is a strong candidate anyway:** an OOM-killed worker (SIGKILL)
    leaves **no faulthandler dump**, which explains the one fact every prior
    occurrence shares — the #590 diagnostic is armed for *fatal signals*, so a
    SIGKILL would still name no culprit. It also explains Linux-only (the OOM
    killer), late-in-lane (accumulation), intermittent (which files a worker
    draws), siblings stalling (box-wide memory pressure), rerun-passes, and
    recurrence on branches that never touch `changestream.py` — the leak is in
    the *fixtures*, not the router. **Not proven.** Next step is unchanged
    (Linux repro), but now with a specific thing to watch: worker RSS over time,
    and `dmesg`/`Killed process` evidence of an OOM kill.
  - **Also confirmed clean while here:** the ws handler's `finally` correctly
    cancels+awaits `disconnect_task` and closes the stream, and `read_oplog`
    holds the storage lock across its body with the `_closed` fence inside it,
    so `stop()`'s "close storage anyway" path is serialised against it as its
    comment claims. Neither is the leak.
  - **THE CRASH FOLLOWS `test_admin_skeleton.py` ACROSS LANES — 3 for 3.**
    (This corrects an earlier claim in this file that the ws/admin attribution
    was wrong. That claim was computed with `-p no:randomly` and was **wrong**:
    group membership is **not** fixed, because `pytest-randomly` reshuffles
    collection per seed and `pytest-split` partitions that *ordered* list, and CI
    pins `--randomly-seed=${{ github.run_id }}`. You must reproduce the split
    with the run's **own** seed.) Recomputed per run:

    | occurrence | crashed lane | admin file's group under that run's seed |
    | --- | --- | --- |
    | #595 (run 29849354713) | `test (3.10, ubuntu, 1)` | group **1** |
    | #597 run 1 (29857900416) | `test (3.10, ubuntu, 1)` | group **1** |
    | #597 run 2 (29858996357) | `test-durable (3.10, ubuntu, 4)` | group **4** |

    The file moved from group 1 to group 4 between runs and **the crash moved
    with it**. So the original attribution was right, and the mechanism is most
    likely the *file's memory cost* rather than the ws test's logic — which fits
    the measured leak (94 retained apps, ~206 MB over the file pre-fix) and the
    no-fatal-signal evidence below. Correct command, with the seed:
    `pytest --splits 4 --group N --durations-path=ci/durations/linux.json
    --randomly-seed=<run_id> --collect-only -q | grep -c <file>`.
    (The earlier #586-group-3 claim was computed the same wrong way and is
    **retracted** — unverified either way.)
  - **NO faulthandler dump is produced, and the diagnostic IS correctly wired.**
    `test-durable` sets `SECANTUS_FAULTHANDLER_DIR` (workflow line ~768) and has
    the prune+upload steps (~1087/1092), yet run 29858996357 uploaded
    **0 artifacts**. `faulthandler` catches SIGSEGV/SIGABRT/SIGBUS/SIGFPE/SIGILL
    — so the worker is **not** dying of any of those. The remaining ways to die
    with no dump are **SIGKILL (the OOM killer)** or an abrupt `_exit`. This is
    the first *positive* discriminator the investigation has had, and it points
    the same way as the measured leak. Still not proof — that needs `dmesg` /
    "Killed process" or a worker-RSS trace — but the #590 diagnostic has now
    returned a meaningful negative rather than nothing.
  - **SIXTH occurrence, on this very PR (#597), WITH the partial leak fix in
    place** — `test (3.10, ubuntu-latest, 1)` again (same shard as #595), worker
    `gw2` "Not properly terminated", rerun clean. The branch changes *only* test
    fixtures, so this is another data point that the crash is not branch-related
    — and it shows halving the leak (1.82 → 0.80 MB/test) does **not** on its own
    stop the crash. That neither confirms nor refutes OOM: the residual growth is
    still unbounded, so a long lane can still get there. To actually decide it,
    the Linux run needs worker-RSS sampling and `dmesg` / "Killed process"
    evidence — a single occurrence can't distinguish "OOM, still leaking enough"
    from "not OOM at all".


Subtler than the above; these may bite specific test suites.

- [x] **RESOLVED 2026-08-17 — `test_crash_stall_watchdog.py`'s two nested-run
  tests (`test_faulthandler_dir_arms_a_file_per_worker` /
  `…_captures_a_worker_crash`) wedged in pytest's SHARED-temp-root `atexit`
  cleanup, not in execnet/fd inheritance.** A pytest process that is not given
  `--basetemp` picks its base dir through `make_numbered_dir_with_cleanup`, which
  **registers an `atexit` hook** that `rmtree`s every stale
  `$TMPDIR/pytest-of-<user>/pytest-NNNN` except the newest three
  (`_pytest/pathlib.py`). This dev box had **240** such leftovers, each a full
  suite's worth of WiredTiger databases, so the nested session's *exit* ground
  through millions of `unlink()`s long after its last test. Measured on the exact
  nested command: nested tests **0.55s**, process wall clock **252.96s**
  (`user 1.08s / sys 18.11s` — I/O, not CPU), and `sample` of the wedged pid put
  **1479 of 1559** samples in `Py_FinalizeEx → atexit_callfuncs → os.unlink`.
  Intermittent because the first run to pay the cost drains the backlog: the same
  command three times in a row took 252.96s, 11.95s, 0.60s. Controlled proof: with
  a synthetic 30-dir stale root the nested run took 1.6s and deleted 26 of the
  dirs, and with `--basetemp` it took 0.6s and deleted none (150-dir / 240k-file
  variant: SIGKILLed at a 3s budget with all tests passed, vs 0.6s clean). Fix:
  both nested helpers now pass `--basetemp <tmp_path>/basetemp`, which skips that
  registration entirely (`TempPathFactory.getbasetemp`), plus the timeout path
  `killpg`s the nested tree (a Popen `start_new_session=True`) so an orphaned
  nested worker can't hold the inherited pipes open and starve the recovery read
  of EOF. Pinned by
  `test_nested_sessions_stay_out_of_the_shared_pytest_temp_root` (sentinel stale
  dirs in a private `TMPDIR`: it fails, deleting `pytest-1..3`, if the
  `--basetemp` isolation is removed). Note the earlier CPU-starvation theory is
  refuted by the `user 1.08s` figure, and the fd/execnet-inheritance hypothesis
  was not needed. **Residual, unexplained:** one captured occurrence showed
  stdout ending at `[100%]` with no summary line, i.e. a wedge *before* the
  summary; the flush timeline of a healthy nested run shows `.`, `[100%]` and
  `\n1 passed in Xs\n` as three separate flushes ~20-40ms apart with the summary
  gated on xdist's `teardown_nodes`, so that one capture points at a second
  (much rarer) pre-summary teardown wedge. `_run_tolerating_teardown_wedge`
  already tolerates that shape.

- [x] **macOS CI test job hangs to the 6-hour kill — RESOLVED + CONFIRMED 2026-07-19: an fd/memory leak from unclosed `Storage` test fixtures (fixed #563); macOS re-enabled on continuous CI and confirmed green (#567). Investigation detail below.** The `test (macos-latest, 3.10)` job hit GitHub's 6-hour job timeout repeatedly (≥3× across unrelated PRs, including a *tooling-only* one), while passing in ~4 min on Linux/Windows and locally on macOS. Mitigated, not fixed: `9e16e49` dropped macOS from the continuous (push/PR) matrix, so it now runs **only at release time** (`publish.yml` / scheduled sweep) — where the hang can still bite. The per-test `timeout` (pytest-timeout, 600s) does **not** cover the wedge because it only guards a test's own body, not collection / session-scoped fixtures / xdist worker *shutdown*. **Prime suspect:** a daemon/thread not reaped on macOS keeping the worker process alive after its tests "finish" — most likely the Rust server's `stop()` / accept-thread join or a parked change-stream tailable `getMore` (the same shape as the *Python* `SecantusDBServer.stop()` use-after-free fixed in 0.5.3b5, below; `test_rust_server_stress.py` / `test_rust_pitr_cross_server.py` exercise the Rust lifecycle). **Diagnostics added** (`ci-macos-hang-guard`): a `timeout-minutes: 30` cap on the test job + a `faulthandler` session watchdog in `tests/conftest.py` that dumps every thread's stack and exits at 25 min — so the next occurrence names the wedged thread/test instead of dying silent. Close once a stack identifies the culprit and the lifecycle leak is fixed. **Reframed 2026-07-19 (investigation):** it is NOT a thread leak — it is an early *worker-process crash*. The 2026-07-18 full-matrix dispatch run reproduced it: `test (3.12, macos-14, core)` ran 90 min (its dispatch-event `timeout-minutes` cap is 90, not 30) while every other macOS job finished in 4–6 min, and its worker `gw0` went **"node down: Not properly terminated" at ~3.5 min** — a subprocess crash (WT_PANIC / segfault / abort), not a lingering thread at shutdown. The prime suspect (Rust `stop()`) is **ruled out**: it has been bounded (10s drain, `RunningServer::Drop`→`stop()`) since beta.48 (2026-06-21), which *predates* the macOS-drop (9e16e49, 2026-06-29) by 8 days; all Python server threads are daemon. Local audits (single-process AND 3-worker xdist, thread + native `faulthandler` dumps) of every suspect file leave zero leaked threads / no crash — the crash is macOS-timing-specific and does not reproduce on a newer local macOS. The 25-min watchdog did not fire in that run because after the crash the surviving workers kept reporting, so the idle-based check never tripped and the shard crawled to the cap. **Fixed the diagnostic gap so the next occurrence self-diagnoses:** `pytest_testnodedown` now dumps the crash reason (`error`, carrying the dead worker's last frames) the moment a worker goes down, and the controller watchdog gained a *post-crash overrun* trigger (`_stall_trigger`) that fails fast with a stack dump if the run is still going `_POST_CRASH_DEADLINE_SECONDS` (20 min, `SECANTUS_POST_CRASH_SECONDS`-overridable) after the first crash — the idle check alone can't see a crawl. **ROOT CAUSE FOUND + FIXED 2026-07-19:** it is a **file-descriptor / memory leak from test fixtures that create a `Storage` but never `close()` it.** A returned-but-never-closed `Storage` abandons its WiredTiger connection — measured at **~2.5 MB and ~17 fds each** (150 leaks → 419 MB, 2571 fds, both linear/unbounded). Three offenders: `tests/test_indexes.py`'s `storage` fixture (`return Storage(...)`, backing **161 tests**), `tests/test_storage.py`'s `storage` fixture, and `tests/test_aggregate.py`'s `_setup_lookup_storage` helper (6 tests). A worker running many of these exhausts its fd limit (`ulimit -n`, ~1024 on CI) or RSS late in the run → the process dies hard with no clean error → xdist reports "node down: Not properly terminated" (the crashed worker's stderr, incl. any WT_PANIC, is lost). Intermittent because it depends on how many leaky tests a worker draws under the random split; late (~92%) because the leak accumulates. Fixed all three to `try: yield s finally: s.close()` / `request.addfinalizer(s.close)` — verified fds stay flat (12→12 over 221 tests, was ~17/test) and a broad sweep (471 tests) shows zero remaining fd growth. **Confirmed closed (#567):** macOS was re-enabled on the push/PR matrix for the `test` and `test-durable` lanes via a matrix `include` of `macos-14` + the dev interpreter (3.12) for each shard group (the include merges with the existing cron cross-product, so no extra weekly-cron cells). On that PR's own CI run **all 8 macOS shards** (`test` ×4 + `test-durable` ×4 — the exact lanes that used to hang) passed green, proving the fix holds on macOS. Any future regression is bounded to a fast, self-diagnosing failure by the #555 crash-capture + 30-min job cap. (`storage-engine` stays cron-only — heaviest source build, and it covers the Rust engine, not the fd-leak surface.)
- ~~**Intermittent pytest-xdist worker crash at ~97% of full suite (post-b18).**~~ Fixed (0.5.3b5). Root cause: `SecantusDBServer.stop()` joined only the accept thread, then closed WiredTiger while per-connection daemon threads could still be mid-WT-operation (e.g. a change-stream tailable `getMore` reading the oplog) — a use-after-free that surfaced as the native worker crash ("node down: Not properly terminated"). `stop()` now closes connection sockets, wakes parked tailable getMores (`Storage.signal_shutdown`), and waits for the active-connection count to drain to zero before `storage.close()`. Reproduced deterministically (a connection thread in a tight WT-read loop vs `storage.close()` raised `Cursor_reset ... is None`, the Python-surfaced form of the same use-after-close); a 200-iteration stress now runs clean. Regression guard: `tests/test_server_shutdown.py`.
- ~~**Rust server `WT_PANIC` under concurrent start/stop (cross-server PITR flake).**~~ Fixed (Rust 0.5.3-beta.48). Root cause was the Rust analogue of the Python shutdown bug above: `RunningServer::stop()` signalled the flag and joined only the accept loop — the detached per-connection threads (each holding an `Arc<Storage>`) weren't waited for, so the WiredTiger connection didn't close until one of them later exited, and that connection's final close-checkpoint then raced the caller removing / reopening the data dir (`WiredTigerHS.wt: stat: No such file` → "the checkpoint failed, the system must restart: WT_PANIC"). `stop()` now drains a live-connection counter (`Shared.active`, an independent `Arc<AtomicUsize>` so a thread releases its storage ref *before* decrementing) to zero — bounded by a 10s deadline — before returning, making teardown synchronous and the data dir quiescent. Reproduced deterministically with the new `bench/wt_stress.py` (`invoke rust-stress` — 24 of 64 concurrent cycles panicked before, 0 after). Regression guard: `tests/test_rust_server_stress.py`; the previously-deselected `tests/test_rust_pitr_cross_server.py` cross-server tests now pass under `-n auto`.
- ~~**`$type: "int"` / `"long"`**~~ fixed (b29). `_TYPE_PREDS` keys on `isinstance(v, bson.Int64)` rather than Python value range — pymongo's BSON decoder preserves the int32/int64 distinction by class (int32 → plain `int`, int64 → `Int64`), so a doc inserted as `Int64(5)` now matches `$type: "long"` (not `"int"`). `$convert: {to: "long"}` returns `Int64` so its output round-trips correctly through the type predicate.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).
- ~~**`apiStrict: true` enforcement Java pool-clear cascade**~~ resolved (0.5.2b3) by narrowing the gate instead of the broad-whitelist invert. A focused `_API_V1_REJECTED_BY_NAME = {"distinct"}` rejects only the canary command the spec's unified runners actively probe (mongo-java-driver `crud-api-version-1-strict.yml` `distinct appends declared API version`). Empirical Java-gauge run: +1 pass for the canary, **zero** new failures and zero pool-clear symptoms across the 900-test suite. The previous cascade theory (broad whitelist would invalidate the pool through SDAM) is correct for the broad path but doesn't trigger from a single command rejection — the broad invert also rejected `count` (used internally by `estimatedDocumentCount`) and other handshake-adjacent admin commands, which is the actual mechanism for the 6 cascade failures, not pool-clear semantics. The narrow gate sidesteps that entirely.
- [ ] **Java gauge: 5 remaining failures are all driver-internal / out-of-scope (triaged 2026-06-30, Python server, HEAD d8e75ff).** A broad multi-gauge survey left java the only gauge with fresh failures (5; node/ruby's single fails are the known text-index + single-node-`w:2` artifacts; rust/kotlin/dotnet/cxx are 100%). All 5 were triaged and proven **not** to be server divergences — driving the same operations via pymongo against an on-disk daemon produced exactly the spec-expected wire replies, and the pymongo gauge passes the *identical* upstream command-monitoring / versioned-api spec files (which assert the same event counts → the server induces no extra round-trips). Recorded in `validation_summary/expected_failures.py` (`JAVA` list):
  - `ClientMetadataTest … metadata append does not create new connections …` — client-side `appendMetadata` crosses no wire; driver connection/handshake logic. Not server-fixable.
  - `VersionedApiTest … find and getMore append API version` — asserts the *driver* decorates outbound find/getMore with `apiVersion:"1"`; SecantusDB already accepts the serverApi fields. pymongo passes the identical `crud-api-version-1` spec.
  - `CommandMonitoringTest … A successful deleteMany` — server reply is spec-correct (`delete` → `{ok:1, n:2}`); pymongo passes the identical spec. Java-driver event accounting vs standalone topology.
  - `CommandMonitoringTest … A successful find with a getMore` — server emits exactly the spec find→getMore wire sequence (firstBatch 3 + Int64 id, then nextBatch 2 + id:0, no extra round-trip); pymongo passes the identical spec.
  - `ConnectionPoolLoggingTest … Create a client, run a command, and close the client` — asserts the Java driver's CMAP connection-pool *log messages*; the server emits no log lines over the wire. Out of scope.
- [ ] **Go gauge flake: `TestIndexView/drop_one` + `drop_all` server-selection timeouts** — **mitigated at the runner (2026-06-26), not server-fixable.** Root cause (not a server bug): under `validate-all`'s multi-gauge CPU / socket-buffer contention the daemon briefly misses a heartbeat and the Go driver's 30s server-selection deadline lapses mid-test (`context deadline exceeded`, topology `Type: Unknown`). Mitigation: `go_validation/runner.py` points the gauge `MONGODB_URI` at `serverSelectionTimeoutMS=60000`, so the transient blip is ridden out; a genuinely unreachable daemon still fails inside the 30m package timeout. **Unverified in its actual failure condition** (it only fires under the full `validate-all` fan-out, which wasn't re-run); the bump is a low-risk, well-motivated mitigation, not a confirmed fix. (History: surfaced 2026-05-14; the per-collection-lock-deadlock hypothesis was ruled out 2026-06-15 by a clean 12-thread DDL+CRUD stress — the cause was always cross-gauge resource exhaustion, not a SecantusDB fault.)
- [ ] **Go gauge flake: `TestChangeStream_ReplicaSet/try_next/one_getMore_sent`** — **NOT server-fixable; confirmed go-harness / load-timing artifact (2026-06-26, with new hard evidence).** Fails intermittently (~1 in 3 full gauge runs) with `TryNext returned true on iteration 1` (elapsed ~0.3s instead of 1.0s): the first `getMore` on a freshly-opened, supposed-empty collection-scoped stream returns an event. **Two runner-side fix attempts both FAILED** (proving it isn't what their hypotheses assumed): (a) isolating change-stream tests from the rest of the suite still flaked 1/3; (b) running each change-stream top-level function in its own serial `go test` process *still* flaked 1/2. **The server is provably correct under load** (the decisive 2026-06-26 evidence — all on-disk, matching the gauge): a collection-scoped stream on an untouched collection saw **0** events across 400 polls while 8 threads did 2074 concurrent writes to other collections; and **0** events across 300 fresh stream-opens under heavy create/drop/insert churn (4110 ops). The flake also does **not** reproduce in any isolated loop (try_next alone 15/15; whole `TestChangeStream_ReplicaSet` group, on-disk, `-parallel=1`, 12/12; faithful Standalone-then-ReplicaSet on-disk with the gauge skip list, fresh daemon per iter, 10/10). It only manifests under the *full-gauge* concurrent load. Net: the change-stream wire/scope behaviour is correct; the flake is a timing/scheduling property of the shared-daemon mongo-go-driver mtest harness that the runner cannot suppress without per-test isolation that even then doesn't hold, nor without editing the vendored tests (forbidden). **Accepted.** The §5 verdict below has the harness-mechanism analysis.

  **Repro session 2026-06-15 — verdict: test-harness artifact, not a SecantusDB scope-filter bug; accepted.** Two things were established. (1) **The change-stream scope filter does not leak.** A direct stress — a collection-scoped `watch()` open on `db.A` while a writer hammered `db.B` on the same shared `:memory:` daemon — polled 1650 times and saw **0** cross-collection events. The projection layer (`changestreams.project`) and `_ns_filter` correctly confine a collection-scoped stream to its own namespace. So the `TryNext returned true on iteration 1` is *not* SecantusDB surfacing a foreign write through a mis-scoped filter. (2) **The Go mtest harness shares one namespace across tests and truncates collection names to a colliding suffix.** `dbName` defaults to a single shared constant `TestDB` for every test (`mongotest.go:117-118`), and `collName = t.Name()` is then truncated to its *trailing* bytes to fit the 120-byte namespace cap (`sanitizeCollectionName`, `mongotest.go:591-602`: `coll = coll[len(coll)-remaining:]`). Two different long subtest names can therefore collide on the same `TestDB.<suffix>` collection. Combined with the parallel top-level `Test*` functions that call `t.Parallel()` (encryption-prose, `TestClient_BSONOptions`) writing during the change-stream's await window, a genuinely same-namespace write from a *concurrent test* can legitimately wake the stream early — which is correct server behaviour, not a leak. This reproduces only under the one-shared-daemon gauge (each test gets its own server in normal CI), and is invisible running `TestChangeStream_ReplicaSet` alone (30/30 pass). Not patchable on the SecantusDB side without breaking conformance; the honest fix is harness-side namespace isolation, which would mean editing the vendored submodule (forbidden — defeats the gauge). Left documented and accepted.
- [ ] **Ruby gauge: `Index::View#create_one with session` test client-side-stripped** — mongo-ruby-driver's `Mongo::Index::View#create_one when provided a session behaves like a failed operation using a session raises an error` test passes `view.create_one(spec, invalid: true)` and expects an `OperationFailure` to come back from the server. But the Ruby driver's `Options::Mapper.transform` filters the model hash against its `OPTIONS` whitelist (`lib/mongo/index/view.rb:61`) **before** the command is built, so `invalid: true` never reaches the wire. We added unknown-spec-option rejection on `createIndexes` (`commands.py:_INDEX_SPEC_KNOWN_OPTIONS` + the `Location40415` gate in `_create_indexes`) which DOES fire when the option arrives, so this is a working server-side guard — the test is just structurally broken against modern Ruby drivers. Real mongod has the same problem; the test would need the driver to keep `invalid: true` in the spec for the server-side rejection path to be reachable. Documented and accepted.
- [ ] **Ruby gauge: `applies the write concern passed in as an option` expected-fail under single-node topology** — mongo-ruby-driver's `Mongo::Collection#create ... when write concern passed in as an option` test (`spec/mongo/collection_ddl_spec.rb:211`) explicitly passes `w: 2` to `collection.create` and expects success — it assumes the canonical multi-node replica-set test cluster the Ruby driver's CI runs against. SecantusDB advertises as a single-node `secantus` replica set, so `w: 2` produces a `writeConcernError` (code 100, `CannotSatisfyWriteConcern`) — added in `commands.py:_unsatisfiable_wc_error` + dispatch wire-up. This is the correct mongod emulation; the test is structurally incompatible with our topology. **Net trade-off was +7 Ruby gauge passes**: seven `applies the write concern` tests that pass `INVALID_WRITE_CONCERN = {w: 4000}` and expect `OperationFailure` now pass because of the wce, this one test now fails. If the test cluster ever grows past 1 advertised member, this test will start passing organically.
- [ ] **PHP gauges landed (2026-06-15) — conformance gaps surfaced, not yet fixed.** Two new gauges: `php_ext_validation` (mongo-php-driver `.phpt`, the low-level C extension that wraps libmongoc — strictest wire-protocol gauge alongside Go) at **99.9%** (670/671 ran, 41 skipped — climbing: dup-key `errmsg` fix 0.5.3b9, cursor open-count 0.5.3b11, insertion-order `find` 0.5.3b13) and `php_lib_validation` (mongo-php-library PHPUnit, the high-level `mongodb/mongodb` package) at **98.8%** (3051/3091 ran, 39 skipped — climbing: `explain` 0.5.3b8, `collMod` TTL 0.5.3b10, `count` hint + 2dsphere 0.5.3b12, insertion-order `find` 0.5.3b13). Submodules pinned to the installed extension version (driver `2.3.1`, library `2.3.0`); the ext gauge runs against the already-installed extension via `run-tests.php` (no rebuild). Real divergences to chase (none block the gauge feature):
  - ~~**capped-collection tailable cursors** — php-ext `cursor-tailable_error-001`~~ **FIXED (0.5.4b17)** — the test opens a `tailable` query on a capped collection, iterates with awaitData polling, and expects a "collection dropped" error when the coll is dropped mid-iteration. Capped tailables ship (`e187fb7` + the 0.5.4b16 filter fix); dropping the collection now **tombstones** open tailable cursors (`CursorRegistry.kill_namespace` sets a `dropped` flag instead of removing them) so the next `getMore` returns `QueryPlanKilled` (175) "collection dropped: <ns>" — the message the php-ext test asserts. Non-tailable cursors are still removed (→ `CursorNotFound` 43, per mongo-c-driver's `error_document/getmore`). Regression: `tests/test_crud.py::test_tailable_drop_returns_collection_dropped`. (The sibling `cursor-destruct-001` — killCursors-on-destruct via the live `serverStatus.metrics.cursor.open.total` count — shipped in 0.5.3b11.)
  - Transaction-gated cases skip/fail under single-node topology (expected, same class as the Ruby `w:2` note above).
- [ ] **mongo-c-driver (`libmongoc`) gauge landed (2026-06-19) — conformance gaps surfaced, not yet fixed.** New `c_validation` gauge builds the vendored driver's `test-libmongoc` from source (CMake) and runs a curated set of wire-protocol suites against an embedded daemon over `MONGOC_TEST_URI`. Driver pinned to `1.30.8`. Current (after the 0.5.4b9 fixes below, gauge-verified): **712 pass / 21 fail / 69 skip**; was **707 / 26 / 69** at landing — +5 from the five fixes below, zero regressions. The failure set is byte-identical across repeated full runs (deterministic, not flaky). 8 of the 26 are documented in `validation_summary/expected_failures.py` (`C` list — the RS-primary `hello` advertisement makes libmongoc's standalone/secondary server-type and `lastWriteDate`-absent assertions fail; IPv6 needs a v6 listener). The remaining ~18 are real divergences to chase (none block the gauge):
  - **`maxMessageSizeBytes` split boundary** (`/BulkOperation/OP_MSG/max_msg_size`, "2 == 1") — **NOT a splitting bug; re-diagnosed 2026-08-11.** The framing below ("the server doesn't split") is wrong, and chasing it wastes time. Measured: all three advertised limits are byte-identical to mongod on BOTH servers (`maxBsonObjectSize` 16777216, `maxMessageSizeBytes` 48000000, `maxWriteBatchSize` 100000), and the driver — which decides splitting itself, before sending — therefore has correct inputs. The test **PASSES standalone** (0.72s) and **passes alongside its `/BulkOperation/OP_MSG/*` sibling**; it fails only inside the full ~800-test run, and it fails on the **Python server too**, so it is not Rust-specific. That profile (isolation-clean, full-run-only, both servers) is test interference or accumulated state, the same class as the accepted go-gauge `TestChangeStream_ReplicaSet` harness race below — not a wire-limit divergence. Pinning it down means bisecting a ~10-minute full-gauge run; worth doing only if it ever blocks something. Original note:
  - ~~**`bypassDocumentValidation` on aggregate `$out`**~~ **FIXED (0.5.4b9)** — `$out` / `$merge` now enforce the destination collection's `validator` (when `validationAction: "error"`) unless the command set `bypassDocumentValidation`, raising `DocumentValidationFailure` (121). See `aggregate._enforce_target_validator` (`bypass_validation` threaded through `PipelineContext`).
  - ~~**getMore error-document shape**~~ **FIXED (0.5.4b9)** — dropping (or renaming) a collection now kills its open cursors (`CursorRegistry.kill_namespace`, called from `_drop` / `_rename_collection`), so a later `getMore` fails with `CursorNotFound` (43) instead of serving stale snapshot rows.
  - ~~**Decimal128 `batchSize`**~~ **FIXED (0.5.4b9)** — `find` / `aggregate` coerce a numeric `batchSize` of any BSON type via `_coerce_command_int` (Decimal128 → underlying Decimal → int).
  - ~~**Capped-collection tailable timeout** (`/Collection/tailable/timeout/single`)~~ **FIXED (0.5.4b16)** — not a capped-tailable gap (capped tailables already shipped, `e187fb7`): the tailable producer was returning follow-up inserts *unfiltered*, so a cursor watching `{a:1}` surfaced the unrelated `{}` doc and `mongoc_cursor_next` returned a doc instead of nothing. `commands._find_tailable` now re-applies the find filter to scanned rows. Regression: `tests/test_crud.py::test_tailable_await_filter_applies_to_follow_up_inserts`.
  - ~~**Namespace-length enforcement**~~ **FIXED (0.5.4b9)** — `dispatch` rejects any command whose database component exceeds 63 bytes with `InvalidNamespace` (73).
  - ~~**`collMod` error response** (`/collection-management/modifyCollection-errorResponse`)~~ **FIXED (0.5.4b9)** — `collMod {index: {prepareUnique: true}}` arms an index (new dup writes → 11000 via `prepareUnique` honoured in `storage._unique_conflict`) and `collMod {index: {unique: true}}` over existing duplicates is refused with `CannotConvertIndexToUnique` (359) + a `violations: [{ids:[...]}]` array (`storage.find_index_duplicates` / `set_index_options`).
  - ~~**`writeConcernError` reporting** (`/Client/command_w_write_concern`, `/Database/create_with_write_concern`, `/command_monitoring/unified/writeConcernError`)~~ **FIXED (0.5.4b13)** — per-test triage showed these were the `w: 99` case, resolved by the `w > 50` parse-error fix above. (The grouping was imprecise: the remaining two failures in the original list are unrelated and split out below.)
  - ~~**`/Collection/index_w_write_concern`**~~ **FIXED (0.5.4b14)** — not a write-concern issue at all: after the `w > 50` fix this test fails on its *invalid-index* assertion — it creates `{abc: "hallo thar"}` and expects the server to reject it. `storage.create_index` now rejects an index-key string that isn't a recognised plugin (`2d` / `2dsphere` accepted; `text` / `hashed` out-of-scope) with `CannotCreateIndex` (67) "Unknown index plugin '<value>'". Regression: `tests/test_driver_gaps.py::test_unknown_index_plugin_rejected`.
  - **`/command_monitoring/unified/writeConcernError`** — **flaky, not deterministic** (0.5.4b15 triage): a fresh full-run repro of the curated C include set has it **passing** (only `/Client/ipv6/single` + `/Collection/tailable/timeout` fail there now), so the earlier gauge report that listed it was a flaky red, not a reproducible state leak. The failpoint registry (`secantus.failpoints`) *is* per-server with no `appName` scoping, so a leaked `failCommand` from a prior test is a plausible mechanism, but it couldn't be reproduced in any isolable subset. Left as a watch item; if it recurs, add per-`appName` failCommand scoping (the mongod-faithful isolation) + audit failpoint teardown. **A real, separate bug found during this triage was fixed in 0.5.4b15**: a malformed `$and`/`$or`/`$nor` (non-array / empty / non-doc element) crashed the query engine into a generic `InternalError` instead of `BadValue` — see `query._match_clause` + the changelog.
  - ~~**State-ordering-dependent drop/rename/create** (`/Collection/drop`, `/Collection/rename`, `/Collection/index`, `/Database/drop`)~~ **FIXED (0.5.4b13)** — not state-ordering at all (misdiagnosis): each test ends with a DDL op carrying `writeConcern: {w: 99}` and asserts `assert_wc_oob_error` — for a server >= 4.3.3 (we advertise 7.0) that's `FailedToParse` (9) "w has to be a non-negative number and not greater than 50", because mongod caps numeric `w` at 50. SecantusDB was returning a `CannotSatisfyWriteConcern` (100) writeConcernError on a success instead. `commands._validate_write_concern` now rejects `w` outside `[0, 50]` with code 9 before the command runs (and `_drop_database` / `_rename_collection` now call it). Regression: `tests/test_crud.py::test_write_concern_w_above_50_is_parse_error`.
  - ~~**Atlas Search index management** (`/index-management/{list,drop,update,create}SearchIndex`)~~ **FIXED (0.5.4b18)** — `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` commands and the `$listSearchIndexes` aggregation stage (+ `$search` / `$searchMeta` / `$vectorSearch`) are Atlas-only; a non-Atlas mongod fails them with a message naming Atlas. Now rejected with `CommandNotSupported` (115) + the shared `aggregate.SEARCH_INDEX_ATLAS_MSG` (the tests assert `errorContains: "Atlas"`). Regression: `tests/test_crud.py::test_atlas_search_index_commands_rejected` + `tests/test_aggregate.py::test_atlas_only_stage_rejected_with_atlas_message`.
  - ~~**Change streams excluded**~~ **NOW RUNNING (#802, #804)** — `replSetGetStatus` reports the one-member roster `hello` already advertised, so the C fixture no longer skips the suites as standalone. 31 change-stream tests pass; the four defects that surfaced (resume-token error message, pipeline-stage code/message, resume-token *modification* not just removal, and `failGetMoreAfterCursorCheckout` + the `ResumableChangeStreamError` label) are all fixed.
  - **Fresh gauge number (0.5.4b18):** 720 pass / 13 fail / 69 skip. The 13 remaining are all documented-expected (ipv6 ×2, `last_write_date_absent` ×2, `select_server` ×4 — all RS-primary `hello` artifacts in `expected_failures.py`) or known/deferred (`BulkOperation/max_msg_size` driver-side split; `command_monitoring/writeConcernError` flaky). No clean actionable server divergences remain in this gauge.
- [x] **mongo-cxx-driver (`mongocxx`) gauge — ALL GAPS CLOSED (#815); 890 pass / 0 fail / 9 skip = 100.0%, identical to the Python server.** First run ever made against the RUST server (2026-08-11): its tests bind port 27017 with no env override, so it needed the local mongod stopped, and the report on disk until then was a Python-server artifact. Three rust-only failures surfaced and were fixed — the change-stream resume token rewound to a positional high-water mark on an empty batch (losing the `ns` / `documentKey` a real token carries), and `$currentOp` reported neither `appName` nor `clientMetadata`, so the handshake test's scan matched nothing. Original landing note follows. New `cxx_validation` gauge builds the vendored libmongoc (installed to a prefix) + the mongocxx `test_driver` Catch2 binary from source and runs it against an embedded daemon. Drivers pinned: mongocxx `r3.11.0` on libmongoc `1.30.8`. mongocxx's core tests hard-wire `mongodb://localhost:27017` (no `MONGOC_TEST_URI`-style override), so the gauge binds the daemon on **27017** (refuses to run if occupied). At landing: **880 pass / 9 fail / 9 skip (99.0%)** (Catch2 expands SECTIONs, so the testcase total > TEST_CASE count). After the 0.5.4b8 dup-index fix + the 0.5.4b9 fixes below + the 0.5.4b11 resume-token fix + the 0.5.4b12 `$currentOp` clientMetadata fix, the cxx gauge's real failures are **closed** (the prior `client metadata handshake` ×2 are fixed below). The remaining divergences to chase (none block the gauge):
  - ~~**Duplicate-index conflict not rejected** (3 tests)~~ **FIXED (0.5.4b8)** — `createIndexes` now rejects same-name-different-key with `IndexKeySpecsConflict` (86) and same-name-different-options with `IndexOptionsConflict` (85), and returns `note: "all indexes already exist"` on an identical re-create so drivers see the no-op (`storage.create_index` + `commands._create_indexes`).
  - ~~**`$out` not last in pipeline not rejected**~~ **FIXED (0.5.4b9)** — `apply_pipeline` rejects a non-terminal `$out` / `$merge` with `Location40601` (40601) before executing any stage.
  - ~~**Change-stream invalid-pipeline error timing**~~ **FIXED (0.5.4b9)** — `validate_stage_names` now also validates `$match` filter syntax (unknown query operators → `QueryError`) up-front, so a change stream opened with `{$match: {$foo: -1}}` errors at aggregate (`.begin()`) time rather than at the first `getMore`.
  - ~~**Change-stream resume-token continuity** (`Spec Prose Tests/1. ChangeStream must continuously track the last seen resumeToken`)~~ **FIXED (0.5.4b11)** — `postBatchResumeToken` now reflects the resume token of the last event *actually returned* in each batch (not the producer's prefetch tail), so per-batch tokens advance under any `batchSize`; and an empty `getMore` only re-mints the token when the oplog tail genuinely moved, so an exhausted quiet stream reports the same token as its last event. `commands._change_stream_cursor_doc` (PBRT = `batch[-1]["_id"]`) + the producer's guarded empty-batch advance; regression `tests/test_change_streams.py::test_resume_token_tracks_per_event_with_batch_size_one`. (Distinct from the Rust-server change-stream resume fixes in 0.5.3-beta.53/54: the open now polls once so already-available events ride the **firstBatch** — pymongo's `_has_next()` only inspects the client buffer, so a resumed stream with an empty firstBatch read as "no changes" — and `resumeAfter` on an **invalidate** token is rejected with `InvalidResumeToken` (260) via a `from_invalidate` marker in the token, while `startAfter` is allowed. Closes pymongo `test_start_after` ×2 + `test_resumetoken_uniterated_nonempty_batch_*` ×2.)
  - ~~**Client-metadata handshake** (`integration tests for client metadata handshake feature/with client`, `/with pool`)~~ **FIXED (0.5.4b12)** — the test connects with `?appName=xyz` and scans `db.aggregate([{$currentOp: {}}])` for an op whose `appName` matches, then reads its `clientMetadata.{application,driver,os}`. The `$currentOp` *aggregation stage* (`aggregate._stage_current_op`) was a bare stub; it now surfaces the connection's `clientMetadata` + a top-level `appName` (threaded via `PipelineContext.client_metadata` from the connection registry), like the `currentOp` command already did. Regression: `tests/test_hello_client_metadata.py::test_aggregation_currentop_surfaces_appname_and_metadata`.
  - Out-of-scope tags excluded in `cxx_validation/include_paths.py` (CSFLE, Atlas, search indexes, transactions, sessions, SDAM monitoring, and `[uri_options]` which needs the `URI_OPTIONS_TESTS_PATH` spec-data dir).
- [ ] **mongo-csharp-driver (C# / .NET) gauge landed (2026-06-19) — one conformance gap surfaced.** New `dotnet_validation` gauge runs the vendored driver's xUnit `MongoDB.Driver.Tests` via `dotnet test` against an embedded daemon over `MONGODB_URI`. Driver pinned `v3.9.0`; scoped to the **CRUD specification suite** (`MongoDB.Driver.Tests.Specifications.crud`) via `--filter` — `MongoDB.Driver.Tests` as a whole is enormous and dominated by non-server unit tests (LINQ/serialization) plus external-service suites (CSFLE/KMS, auth, Atlas Search, load balancing) and multi-node features (transactions, sessions, SDAM, retryable). At landing: **201 pass / 1 fail / 26 skip (99.5%)** (the 26 are `[RequireServer]`/CSFLE-gated skips). After the 0.5.4b9 validation-detail fix below, gauge-verified at **202 pass / 0 fail / 26 skip**. The one (now-fixed) divergence:
  - ~~**Document-validation error detail** (`CrudProseTests.WriteError_details_should_expose_writeErrors_errInfo`)~~ **FIXED (0.5.4b9)** — a failed query-expression validator now synthesises mongod's per-operator `errInfo.details` (`operatorName` / `specifiedAs` / `reason` / `consideredValue` / `consideredType`) via `commands._validation_failure_details` + `query.bson_type_name`, used by both the insert path and `_validate_doc_against_collection`. ($jsonSchema validators still report a minimal `{operatorName: "$jsonSchema"}` — their schema-rules detail is unsynthesised.)
  - The CRUD-only scope is deliberate and expandable — broaden the `--filter` in `dotnet_validation/include_paths.py` to add more spec families (e.g. `read_write_concern`, `change-streams`) as they're validated. Build note: the driver's `MongoDB.Driver.Encryption` project verifies a downloaded libmongocrypt with **gpg** at build time, so `gpg` (and network for the libmongocrypt download) are build prerequisites even though CSFLE itself is out of scope.

> **Rust-server follow-up — RESOLVED (Rust 0.5.3-beta.73).** The 0.5.4b8/b9
> Python-server fixes that the Rust server didn't yet mirror have now all been
> ported into `crates/secantus-{core,commands,storage,storage-adapter}`, each
> with a real-WiredTiger integration test in `crates/secantus-storage-adapter/
> tests/`: `$out`/`$merge`-not-last (`Location40601`), change-stream `$match`
> validation at open (unknown operator → `BadValue` at `.begin()` not first
> getMore), document-validation `errInfo.details` (`consideredValue` /
> `consideredType` via a new `secantus_core::query::bson_type_name`),
> index-conflict codes `IndexKeySpecsConflict` (86) / `IndexOptionsConflict`
> (85) + the `note: "all indexes already exist"` no-op, and (earlier) Decimal128
> `batchSize`, over-long db-name rejection, `$out`/`$merge` target-validator
> enforcement, `collMod prepareUnique`→`unique` 359 violations, and cursor-kill
> on drop/rename. Driver gauges pointed at the Rust server no longer regress on
> these.

## 6. Admin UI review punch list

End-to-end review of the secantus-admin web UI on `main` (May 2026, before the `admin-ui` branch lands its next slice). Severity tiers: P0 broken/silently-wrong, P1 inconsistency or significant usability gap, P2 polish. File refs are absolute under `src/secantus/admin/` unless noted.

### P0 — broken / silently wrong

(None at present.)

### P1 — significant inconsistency / usability

- [ ] **`/backup/dump` and `/backup/restore` long-task UX** — calls `backup_lib.run_mongodump` / `run_mongorestore` synchronously. Spinner + disabled button covers the visible UX gap for normal-sized dumps; the ideal version is a real background-task wrapper with poll status so the user can navigate away during multi-minute dumps of large collections. Not load-bearing — defer until someone actually hits a multi-minute dump.

### P2 — polish

- [ ] **Admin UI polish bundle** — small fixes that don't deserve individual entries; address opportunistically when touching nearby code. (Currently no entries — the bundle was cleared in `admin-ui-rest`, May 2026. Drop new ones here as they show up.)
- [x] **Admin surface for `collMod` / `createCollection` / validators / `renameCollection` / custom-role creation — SHIPPED.** `/db/{db}` grew a create-collection form plus per-row Modify (`collMod`) and Rename (`renameCollection`, including a cross-database `otherdb.name` target and `dropTarget`); `/roles` grew a create form (`createRole`) and a Drop button on custom roles only. Options are one Extended-JSON document rather than a field per option, so a server-version-dependent option set (validators, capped sizing, pre/post images) can't be capped by a stale form. All are PRG with 303 so a refresh can't replay the DDL.
- [x] **Change-stream page resume-token / `fullDocument` controls — SHIPPED.** `/changestream` now carries `fullDocument`, `fullDocumentBeforeChange`, `resumeAfter`, `startAfter`, `startAtOperationTime` and a pipeline filter, in a collapsed options panel; the `open` frame echoes which options took effect, and each event grew a **Resume from here** button that closes the copy-token loop (the page previously offered "Copy resume token" with nowhere to paste it). Options round-trip through the URL so a shared link reproduces the same stream.
- [x] **`validationLevel` is honoured on BOTH servers — the 2026-08-11 entry was
  wrong; re-probed 2026-08-20.** The claim was that the level is stored but never
  consulted. It is consulted, and correctly. Probed over the wire on both servers:
  create with `validator: {n: {$gte: 0}}`, `validationAction: "error"`,
  `validationLevel: "moderate"`; insert a valid doc; sneak an invalid one in under
  `validationLevel: "off"`; switch back to `moderate`. Updating the **already-valid**
  doc to an invalid value is rejected with 121, and updating the **already-invalid**
  doc is accepted — which is exactly mongod's `moderate` rule and is
  indistinguishable from `strict` only if the level were being ignored. Identical
  results from the Python server and the Rust binary. `"off"` demonstrably disables
  validation too (that is how the invalid doc got in). Nothing to fix.
- [ ] **The SQL / PostgreSQL-wire server has no admin UI at all**, and this is now an explicit scoping decision rather than an accident: `client.check_supported_uri` rejects a `postgresql://` target with a message saying so. If a SQL admin surface is ever wanted it needs its own console — every page here is pymongo-driven.
- [ ] **`StarletteDeprecationWarning` from fastapi's testclient import** — the six
  admin websocket tests each emit "Using `httpx` with `starlette.testclient` is
  deprecated; install `httpx2`" from fastapi's own import shim. The last warnings
  in the default suite. Fix is a dev-dependency bump (fastapi/starlette/httpx2)
  in its own PR — no runtime code involved.

## 7. Python → Rust rewrite (in progress)

### 7.0 Cache-pressure rollback — CI exercises it now

`WT_ROLLBACK` is two conditions wearing one code: a write-write conflict
(retryable → `WriteConflict`) and WiredTiger abandoning a transaction whose own
dirty content it cannot evict (NOT retryable → `TransactionTooLargeForCache`).
`Session::rollback_reason()` separates them at all three sites that mint the
error (#981).

This entry recorded that the cache-pressure branch had no end-to-end coverage,
because it could not be reproduced on a fast machine (80 runs under 8-way
concurrency plus 10 CPU burners did not trigger it). **It has since fired for
real on a loaded CI runner**, which both confirmed the classification works and
exposed the last piece: the condition can surface at EITHER level — our own
post-statement guard returns it as the outer `Err`, while an engine-side
rollback during the statement returns it as the INNER one. `txn_budget.rs`
accepted only the outer, so it panicked on the inner. It now accepts both.

So the branch is exercised, just not deterministically: CI is where it happens.
A `StorageOptions` override for the dirty budget would still let a test force
the engine to lose the race on demand, which is worth having if this area
changes again.


### 7.1 Rust server performance and security review (2026-06-16) — CLOSED

Items identified during a security/performance audit of the Rust server. Fixed in
0.5.3-beta.19: SCRAM timing oracle (`subtle::ConstantTimeEq`), mutex poisoning
panics (graceful error returns), double BSON decode in wire layer (length-only
check), response buffer pre-allocation, compound multikey cap (10k).

The remaining eight items (two security, six performance) were resolved in
0.5.3-beta.22:

- **SASLprep for non-ASCII passwords** — `secantus-auth` now applies RFC 4013
  SASLprep (via the `stringprep` crate) in `derive_credentials`, matching the
  Python server's `auth.saslprep` and what a compliant driver sends. `createUser`
  / `updateUser` surface a prohibited-character / bidi failure as `BadValue`.
- **Concurrent message allocation budget** — `secantus-server` carries a global
  `AllocBudget` (512 MB cap); each connection reserves `body_len` before
  allocating its body buffer and releases it once the message is answered, so a
  flood of concurrent large messages can't exhaust the heap.
- **Global storage lock during oplog scans** — the cross-thread readers
  (`read_oplog` / `read_preimage` / `oplog_floor_seq` / `oplog_tail_seq` /
  `find_seq_for_ts`) no longer take the global `Mutex` (their fresh WT session's
  MVCC snapshot is consistent on its own); `prune_oplog` scans lock-free and takes
  the lock only for the delete phase.
- **Document cloning in aggregation stages** — `$addFields`/`$set`/`$unset` mutate
  the owned doc in place (no clone), `$unwind` clones array elements one at a time
  instead of the whole array, and `$facet` reuses the input for its last
  sub-pipeline.
- **Oplog clones every written document** — insert / batch-insert / replacement
  update / upsert move the document into the oplog `o` field instead of cloning it.
- **Nested lock for timestamp minting** — `current_cluster_time` mints and
  snapshots the recovery meta under a single oplog-mutex hold. (The seq counter is
  deliberately NOT a bare `AtomicI64`: `wait_for_oplog` pairs `next_seq` with
  `oplog_cv`, whose lost-wakeup guarantee requires reading the counter under the
  same mutex as the wait.)
- **Query path resolution clones** — `query::resolve_path` (and the `query` /
  `geo` field-operator helpers it feeds) now return/accept borrowed `&Bson` rather
  than cloning every value at every path component.
- **Encode/decode round-trips in find→cursor→client** — `find` (with projection)
  and `aggregate` send the `firstBatch` straight to the wire as `Bson` and encode
  only the cursor *remainder* (`split_docs_into_cursor`), dropping the
  encode-then-decode of the docs the client receives immediately.

---

> ⚠️ **Direction changed — authoritative plan is now `tasks/rust-server-plan.md`.**
> The end-state is **two completely separate servers** (a pure-Python server and a
> self-contained Rust server with a thin embedded Python lifecycle handle), **not**
> the in-process `secantus.engine` selectable model the items below assume. The
> *porting* work recorded here (the pure-Rust crates) is still valid and is the
> Rust server's foundation; the `SECANTUS_ENGINE` selection / Python `Storage`
> adapter / `EngineFallback` items are **retired**. Next work is the Rust server
> itself (`rust-server-plan.md` §4, R1–R8), not more in-process selection surface.

Tracking the incremental rewrite (plan: `tasks/rust-server-plan.md` — north star;
`tasks/rust-rewrite-plan.md`; Phase 0 spike results: `tasks/rust-rewrite-spike-
findings.md`; Phase 3+4 scoping: `tasks/rust-rewrite-phase3-scoping.md`). The Rust
side is a Cargo workspace under `crates/`: the pure-Rust engine lib crate
`crates/secantus-core` plus the PyO3 bindings crate `crates/secantus-core-py`,
which builds the abi3 extension `_secantus_core` via maturin (`invoke rust-build`
/ `rust-test` / `rust-parity`).

**Both implementations are permanent (not a replacement).** The pure-Python
engines power the **Python server**; the Rust engines power the separate **Rust
server**. The in-process `secantus.engine` selection was first made inert
(0.5.3b3) and is now **fully removed** (0.5.3b14): the `secantus.engine` module,
the `--engine` CLI flag, the `SecantusDBServer(engine=...)` parameter, and the
`SECANTUS_ENGINE=rust` CI full-suite step are all gone. The
`tests/test_rust_*_parity.py` oracle still imports `_secantus_core` directly to
pin the Rust engines against the pure-Python ones.

- [x] **Rust numeric type preservation** — DONE (0.5.3-beta.22). `secantus-core`
  `numeric::int_promoted_to_bson` + `is_int64` encode `$inc` / `$mul` (update
  engine) and `$sum` (group accumulator — `Num::Int` now carries a `wide` flag)
  with mongod's promotion (int64 if any operand is int64 or a 32-bit result
  overflows), matching `secantus.numerics`. The `_norm_int_width` normaliser is
  dropped from `tests/test_rust_update_parity.py` (it now compares BSON subtypes
  directly). The expression-language `$add`/`$subtract`/`$multiply` deliberately
  still narrow to match the pure-Python `expressions` fold (which doesn't promote
  either) — making *both* engines promote there is a Python-server behaviour
  change, out of scope for this parity fix.

**Rust server build-out (`tasks/rust-server-plan.md` §4).** Done: R1
(`secantus-wire`), R2a (dispatch framework + handshake family), R2b (`insert` /
`delete` / `count` + the `Storage` trait seam), R3a (`CursorRegistry` +
non-tailable `getMore` / `killCursors`), `find` (read path: skip/limit/projection
/ cursor split → the full `find → getMore → killCursors` path works in dispatch),
`update` (document-form, multi/upsert, pipeline-shape validation), R4a
(`secantus-server`: accept loop + connection handling, generic over the command
`Storage` trait — runs over real TCP, two WT-free roundtrip integration tests),
**R4b + R6 (merged in PR #31, CI-green across Linux/macOS/Windows): the WT adapter
and the `_secantus_server` embedded handle — pymongo → Rust → WiredTiger works.**
`aggregate` (storage-independent pipeline via `secantus_core::apply_pipeline`;
`count_documents` + direct pipelines pass through pymongo).
**R7 (the standalone `secantusdb` binary): `crates/secantusdb`** — args module in
`secantus-server` (WT-free, 11 unit tests) + a WT-linked bin (open Storage →
adapter → bind → print address → SIGINT/SIGTERM → clean stop), smoked by
`tests/test_rust_binary_smoke.py` / `invoke rust-binary-test` and the
`storage-engine` CI job (Linux/macOS).
**Distribution:** the binary ships two ways. (1) Prebuilt static-WiredTiger
archives on a GitHub Release via `.github/workflows/release-binaries.yml`
(tag `secantusdb-v<crate-version>`; x86_64-linux-gnu + aarch64-apple-darwin;
the file inside each tarball is named `secantusd-rs`). (2) Bundled INTO the
`secantus` wheel as the `secantusd-rs` command (non-Windows): `CMakeLists.txt`
installs it into `SKBUILD_SCRIPTS_DIR` under the `SECANTUS_BUILD_STORAGE_ENGINE`
flag, so a flag-on wheel puts `secantusd-rs` on PATH (distinct from the
pure-Python `secantus.cli:main` `secantusd-py` console script). The
`storage-engine` CI job asserts the bundled `secantusd-rs` runs.
**Shipping wheels now flag-ON (0.5.4b1):** `wheels.yml` + `publish.yml` build
with `SECANTUS_BUILD_STORAGE_ENGINE=ON`, so `pip install secantus` bundles the
`_secantus_storage` / `_secantus_server` extensions **and** `secantusd-rs` on
**Linux (manylinux_2_28 + musllinux_1_2, x86_64 + aarch64), macOS arm64, and
Windows AMD64** — every shipped wheel except Intel macOS (no wheel target).
Toolchain in `[tool.cibuildwheel].before-build`: Linux swig+clang-libs+rustup
with libclang symlinked to `/opt/libclang`; macOS swig (Xcode's libclang —
Homebrew's breaks the WT bindings); Windows choco swig+llvm with `libclang.dll`
copied to a space-free `C:/libclang`. `RUSTFLAGS=-Ctarget-feature=-crt-static`
for musl cdylibs. Build perf: `SECANTUS_CARGO_TARGET` shares one cargo target dir
across all wheels in a job (and rides the ccache mount/cache on Linux/macOS) so
the Rust deps compile once, not per-wheel — cut wall-clock ~72min → ~35min.
Windows paths are forward-slashed so CMake doesn't eat the `\c`. Verified: built
manylinux + Windows wheels contain `secantusd-rs`(`.exe`) under
`*.data/scripts/` plus the two extensions. **Remaining:**
- [ ] **macOS x86_64 / Intel** stays pure-Python (no wheel target — runner-pool
  scarcity), so Intel-Mac pip users don't get the Rust bits.
- [ ] **Release fragility:** every wheel build now does cargo crates.io
  downloads in-container; a transient network failure (seen once on macOS) can
  fail a `publish.yml` release. The shared `SECANTUS_CARGO_TARGET` registry cache
  reduces re-downloads within a job; cargo vendoring / a registry mirror would
  remove the risk entirely.
**Deferred / not yet ported:**
- [~] **R7 tail — Windows lane added; PGO on MSVC still open**
  (2026-07-17, updated 2026-07-30). `secantus-wt`'s `build.rs` probes MSVC's
  `wiredtiger.lib` (and `.dylib`), so the standalone binary resolves WT on
  Windows. The `x86_64-pc-windows-msvc` lane is now in the `secantusdb-v*`
  release-binaries matrix, shipping a `.zip` + a `.tar.gz`. Two Windows-specific
  details worth remembering:
    * `+crt-static` — WT's `wiredtiger.lib` uses the static CRT while Rust's
      MSVC target defaults to the dynamic one. That mismatch is what
      `LNK4098: defaultlib 'LIBCMT' conflicts` was reporting, and it means two C
      runtimes with two heaps in one process — a real corruption risk for a
      database, not a cosmetic warning. Bonus: no VC++ redist needed to run it.
    * The binary smoke test can't use SIGTERM on Windows (`send_signal` maps it
      to `TerminateProcess`: immediate kill, exit 1, no handler, so the clean
      shutdown it asserts could never happen). It sends `CTRL_BREAK_EVENT` to a
      `CREATE_NEW_PROCESS_GROUP` child instead, which the `ctrlc` crate's
      console handler turns into the same graceful path SIGTERM takes on Unix.
  `test.yml`'s `storage-engine` job now runs that smoke test on Windows too, so
  the release binary is exercised on every push rather than only at tag time.
  Still open: **PGO on MSVC** (the lane builds `no_pgo`), which needs an
  instrumented MSVC build plus an `llvm-profdata` merge. The entry's second half
  was stale: the CLI's TOML config layer + tuning flags shipped in §7.6
  (beta.96, `config.rs` + `wt_config` knobs).
- [~] **R8 tail — ALL THIRTEEN driver gauges now run against the Rust server**
  (2026-07-17 sweep; reports committed as `docs/validation-report-*-rust-server.md`).
  The Rust server is at effective conformance parity with the Python server —
  two perfect scores, nothing below 98%, and every failure is either a known
  out-of-scope gap (text/hashed indexes, `$where`, transactions/sessions on a
  single node, Atlas search-index management, IPv6) or a documented driver-side /
  harness artifact — **no new Rust-specific divergence surfaced**:

  | gauge | passed/failed/skipped | pass % | failures |
  |---|---|---|---|
  | rust-driver | 101 / 0 / 0 | **100.0%** | — |
  | dotnet | 202 / 0 / 26 | **100.0%** | — |
  | kotlin | 294 / 0 / 244 | **100.0%** | — |
  | php-ext | 670 / 1 / 41 | 99.9% | tailable-collection-dropped edge |
  | node | 358 / 1 / 5 | 99.7% | text-search sort (out of scope) |
  | java | 445 / 2 / 453 | 99.6% | mapReduce (legacy) ×2 |
  | pymongo | 1019 / 6 / 475 | 99.4% | known set (text/hashed/$where/CSOT) |
  | go | 398 / 3 / 52 | 99.3% | `try_next` harness artifact (accepted) |
  | php-lib | 3048 / 43 / 39 | 98.6% | ~37 txn/session (out of scope) + 4 to triage |
  | pymongo-async | 919 / 13 / 491 | 98.6% | sync set + 6 read_concern harness-isolation |
  | ruby | 289 / 5 / 24 | 98.3% | documented ruby artifacts + session cases |
  | c | 718 / 15 / 69 | 98.0% | documented C set (ipv6/lastWriteDate/select_server/search) |
  | cxx | 885 / 3 / 9 | 99.7% | change-stream resume-token tracking, client-metadata handshake ×2 |

  **The php-lib row's "~37 txn/session (out of scope)" reading was WRONG —
  corrected 2026-08-12.** Those transaction failures were not an out-of-scope
  gap at all: `serverStatus` omitted the `storageEngine` sub-document, so the
  suite's `skipIfTransactionsNotSupported` helper threw "Could not determine
  server storage engine" and errored ~27 tests before any transaction ran.
  Fixed in #829; php-lib then went 42 failures -> 4, and #830 took it to 1
  (a text-index test, genuinely out of scope). The lesson worth keeping: a
  cluster of failures sharing one *symptom* is not evidence of a shared
  *cause*, and "out of scope" is the most expensive label to get wrong —
  it stops anyone looking again.

  Follow-up triage (not blockers, no data risk): the php-lib "4 real" assertion
  failures (change-stream resume-token type, session-freed, findOneAndReplace
  BSON-type-map field order, 2dsphere index-version) and the pymongo-async
  6× `test_read_concern` (shared `CollectionInvalid: collection already exists`
  — likely async-harness test-isolation) should be diffed against the Python-server
  runs of the same gauges to confirm none is Rust-specific. ~~The remaining CI
  wiring~~ — DONE (rust-gauge-lanes slice): `validate.yml` now carries a
  `*-rust-server` lane for every MongoDB-wire gauge (pymongo-async / go / node
  / java / kotlin / ruby / rust / php-lib / php-ext / c / cxx / dotnet, joining
  the existing pymongo-rust-server + java-rust-server), each reusing its
  language-toolchain steps plus the storage-engine sync, with `SECANTUSDB_BIN`
  pointing the daemon gauges at the staged `secantusd-rs`. All thirteen gauges
  now run weekly against BOTH servers.
  Original: only the pymongo gauge runs against the Rust server
  (`invoke validate --server rust` / the `pymongo-rust-server` entry in
  `validate.yml`). The Go/Node/Java/Ruby/Rust-driver gauges still gauge the
  Python server only: their runners spawn `python -m secantus` as the daemon,
  and pointing them at the Rust server needs a `secantusdb`-binary launch path
  in each gauge job — deferred until the Rust server's command surface is wide
  enough for those suites to be informative.
- [x] **Timeseries `_id` non-uniqueness in the Rust storage layer** — DONE
  (0.5.3-beta.22). `crates/secantus-storage` now mirrors the Python `Storage`:
  the command layer persists the `timeseries` option, `Storage::is_timeseries`
  detects it, and `timeseries_doc_suffix` (nanosecond timestamp + 16-bit counter)
  is appended to the doc-table key on insert / upsert so duplicate `_id`s coexist.
  `write_index_entries` / `delete_index_entries` gained an `id_key_override` so
  secondary-index entries point at the suffixed row (insert/update/delete/prune
  pass the actual key), and the `_id` point-lookup fast path (in both
  `try_index_id_keys` and the `explain` planner) is gated off for timeseries so a
  `{_id: x}` query scans and content-filters rather than reconstructing the bare
  key. Non-timeseries `_id` uniqueness is unchanged.
- [ ] **Gauge E11000 cluster — remaining tails (2026-06-12).** The
  order-dependent drop-then-reinsert E11000s are FIXED (stale WT read
  snapshot in the mutating scanners; see changelog Unreleased / the
  snapshot-refresh fix in `drop_collection` et al. — gauge went 93.5% →
  94.5%, E11000 failures 7 → 1). Arithmetic type errors are also FIXED
  (`$add`/`$subtract`/`$multiply`/`$divide`/`$mod` now raise mongod's
  errors on non-numeric operands, divide/mod-by-zero, and date misuse;
  Rust engine defers those cases — parity corpus extended first).
  Pipeline-update replace-vs-update misclassification FIXED, and
  timeseries `_id` uniqueness FIXED (suffixed doc keys; the one surviving
  E11000 is gone) — the 2026-06-12 E11000 triage is fully closed. The
  remaining ShowExpandedEvents / disambiguatedPaths introspection failures
  are separate unimplemented features. (`clusteredIndex` introspection is
  DONE in 0.5.3-beta.49 — `create` validates+stores it, `listCollections`
  surfaces `options.clusteredIndex` and omits `idIndex`, `listIndexes`
  reports the single `clustered: true` entry; mirrors commands.py. Closes the
  two `TestCollectionManagementClusteredIndexes` pymongo gauge tests.)
  Timeseries
  update restriction (mongod 7.0: an update may modify only the metaField)
  is now ENFORCED in the Rust server's `update` handler — non-meta /
  replacement / pipeline updates on a timeseries collection are rejected.
- [ ] **Go gauge: CI runs ~1/5 of the local set** — CI weekly artifacts have
  always reported ~450 tests (e.g. 401/453 on 2026-06-08, 447/900 on
  2026-06-12) while local `invoke validate-go` runs ~4700 (the numbers the
  pre-2026-06-12 committed report carried). Suspects: the 30-minute
  `go test -timeout`, cold caches on runners (GitHub cache service 400s), or
  package discovery under `./internal/integration/...` differing on CI.
  Diagnose before trusting week-over-week go comparisons.
- [~] **`aggregate` storage-backed stages** — DONE: `$lookup` (simple +
  `let`/`pipeline` forms), `$sample`, `$collStats`, `$indexStats`, `$out`,
  `$merge` (deep-merge default + replace/keepExisting/delete/fail modes),
  `$geoNear` (brute-force COLLSCAN via `secantus_core::geo::point_distance` —
  near/distanceField/key/query/min+maxDistance/distanceMultiplier/includeLocs/
  spherical; GeoJSON near ⇒ spherical, legacy `[x,y]` ⇒ planar). A
  `run_segmented` executor in `secantus-commands::aggregate` interleaves the
  storage-free core engine with these command-layer stages; `$lookup` `let`
  expressions are evaluated; `collation` threads through `$match`/`$sort`.
  **Source stages DONE:** `$currentOp` / `$listLocalSessions` / `$listSessions`
  emit one synthetic "op" row (port of `aggregate._stage_current_op`, with the
  `command` field echoing the request + `$db`/`cursor` defaulted), handled in
  `run_segmented` via `is_source_stage` / `apply_source_stage`. This is what
  makes a database-level `aggregate: 1` pipeline (no source collection) work —
  the `test_database.py` `$listLocalSessions` shape and the unified db-aggregate
  / versioned-API db-aggregate tests. **`$graphLookup` DONE** (0.5.3-beta.22+):
  command-layer BFS over the foreign collection (`startWith` / `connectFromField`
  / `connectToField`, `maxDepth` default 100, `depthField` as `NumberLong`,
  `restrictSearchWithMatch`), `_id`-dedup, value-match array-aware. **`$lookup` /
  `$graphLookup` nested inside `$facet` DONE**: `$facet` now runs each sub-pipeline
  through `run_segmented` (the command layer) instead of the storage-free core, so
  storage-backed stages work inside a facet. **`$geoNear` `key`-inference DONE**:
  when `key` is omitted it's inferred from the collection's lone `2d`/`2dsphere`
  index (ambiguous when there's more than one → error). **`$merge` pipeline-form
  `whenMatched` DONE** (inline pipeline applied to each matched doc with `$$new`
  bound to the incoming doc) plus **`on`-field unique-index validation** (a
  non-`_id` `on` requires a matching unique index on the target, else code
  51183). The command-layer storage-backed aggregate surface is now complete.
  **`$lookup` index acceleration DONE** (0.5.3-beta.123): the simple form drives a
  per-outer-doc `Storage::find` index probe when the foreign collection has a
  leading-field index on `foreignField` (single-field / compound-prefix / multikey
  all IXSCAN), falling back to the hash-join otherwise — matching the Python
  server's result *and* `as`-array order (index order), which fixes a prior
  two-server order divergence.
- [x] **`distinct` + DDL/introspection** — `distinct`, `create`, `drop`,
  `listCollections`, `listIndexes`, `createIndexes`, `dropIndexes` (the `Storage`
  trait gained the list/DDL methods; the R4b adapter forwards them).
- [x] **`findAndModify`** — composed at the command layer (find limit-1 + sort →
  update/remove → re-find for the new image → projection). **Caveat:** not atomic
  across the find+modify calls (a find-and-modify storage primitive would close
  that); `arrayFilters` / `let` / `collation` / `validator` deferred.
- [x] **db-admin commands** — `dropDatabase`, `renameCollection`, `collStats`,
  `dbStats`, `serverStatus` (trait gained drop_database / rename_collection /
  collection_is_capped / collection_data_size / index_sizes; adapter forwards).
  `serverStatus` minimal; `collStats`/`dbStats` use dataSize for storageSize.
- [x] **sessions + diagnostics** — `startSession` / `endSessions` /
  `refreshSessions` / `kill*Sessions` / `commit`+`abortTransaction` /
  `getParameter` / `getCmdLineOpts` / `connectionStatus` / `whatsmyuri` /
  `hostInfo` / `getLog` (storage-light; session/txn are no-ops). Removes
  CommandNotFound on driver connect/teardown.
- [~] **R5 — auth + TLS** (SCRAM-SHA-1/256, MONGODB-X509, rustls). **R5a DONE**:
  the SCRAM-SHA-256 mechanism (`crates/secantus-auth`, pure Rust, 6 tests, full
  client↔server round-trip). **R5b-1 DONE:** wired into the command layer —
  `saslStart`/`saslContinue` (`secantus-commands::auth`) over a per-connection
  `ConnectionAuth` (threaded one-per-socket through the server), plus
  `createUser`/`dropUser`/`usersInfo` over four new `Storage` trait methods
  (`add_user`/`get_user`/`drop_user`/`list_users`, adapter-forwarded to
  `secantus-storage`). Stored record is mongod-shape so both servers share the
  `secantus_users` table. 6 command unit tests + a pymongo TCP auth round-trip.
  **R5b-2 DONE:** dispatch-level `--auth` gating + RBAC. `secantus-commands::rbac`
  ports the built-in role catalogue + `check_privilege`; `dispatch`'s `authorize`
  rejects unauthenticated non-handshake commands (`Unauthorized` 13) and checks
  the principal's effective roles against a per-command `(action, scope)` table;
  `createUser` validates roles (`RoleNotFound` 31); `saslContinue` loads role
  bindings into `ConnectionAuth::effective_roles`. 11 unit tests. **R5b-3 DONE:**
  custom user-defined roles — `secantus-commands::roles` (`createRole`/`updateRole`/
  `dropRole`/`dropAllRolesFromDatabase`/`rolesInfo`) over four new role-storage
  trait methods (`add_role`/`get_role`/`drop_role`/`list_roles`, adapter-forwarded);
  `rbac::check_privilege_resolved` expands them via a `get_role`-backed resolver
  (privilege match + inheritance walk, cycle detection); `createUser` accepts
  storage-resident custom roles. 6 unit tests + a pymongo WT round-trip. **R5b-4
  DONE:** auth/RBAC completion — the role `grant`/`revoke` quartet
  (`grantPrivilegesToRole`/`revokePrivilegesFromRole`/`grantRolesToRole`/
  `revokeRolesFromRole`), `updateUser` (password rotation / role replacement +
  live `effective_roles` refresh), `dropAllUsersFromDatabase`, and `hello`'s
  `saslSupportedMechs`. 8 unit tests + a pymongo WT round-trip. **R5c-1 DONE:**
  TLS/mTLS transport in the accept loop (rustls, ring backend) — `ServerConfig.tls`
  (cert/key/ca/require_client_cert), handshake under the shutdown-poll timeout,
  client-cert subject DN (x509-parser, RFC 4514) → `CommandContext::peer_cert_dn`,
  `serve` generic over `TcpStream`|rustls `StreamOwned`, `RustServer` TLS params.
  Rust integration test (rcgen self-signed → rustls client → hello) + openssl-guarded
  pymongo TLS smoke. **R5c-2 DONE:** the MONGODB-X509 mechanism — createUser
  provisions X509-capable users (no password), saslStart / legacy authenticate
  read `peer_cert_dn`, match optional payload username, look up by DN on
  `$external`/admin, require an X509 credential, and auth without a password;
  hello/getParameter advertise MONGODB-X509. 4 unit tests. **This closes R5 (auth)
  bar SCRAM-SHA-1** (legacy MD5 prepass — deferred, low priority). Deferred:
  non-ASCII SASLprep.
- [x] **R4b — WiredTiger storage adapter** (`crates/secantus-storage-adapter`,
  `StorageAdapter`): CI-green (rust-storage builds it against vendored WT;
  `Send + Sync` confirmed). Bytes at the seam, `Hint` from `RawHint`, `map_err`.
- [x] **R6 — embedded Python handle** (`crates/secantus-server-py`, the
  `_secantus_server` extension / `RustServer`): CI-green — bundled into the wheel
  by CMake and smoke-tested via pymongo across Linux/macOS/Windows. `RustServer`
  auto-creates the storage dir. **Follow-ups:** a Python `secantus`-package
  wrapper for `SecantusDBServer`-style ergonomics; an `invoke rust-server-py` task.
- [x] **R4 tail — TLS / mTLS + MONGODB-X509: SHIPPED and now verified end-to-end
  (2026-07-17).** Server-side TLS, mTLS client-cert verification, and
  `peer_cert_dn` threading were already implemented; the new Rust-server
  e2e test (`test_rust_server_smoke.py::test_tls_and_x509_auth_end_to_end`,
  the Python suite's two-stage bootstrap flow) exposed that
  `cert_subject_dn` used x509-parser's raw `Display` — least-specific-first
  with `", "` separators — so the extracted DN NEVER matched a provisioned
  user record and X509 auth always failed with AuthenticationFailed. It now
  emits the mongod-style RFC 4514 form (most-specific-first, bare commas,
  short OID names, value escaping) byte-identical to
  `secantus.auth.subject_dn_from_peercert`, so a user provisioned against
  either server authenticates on the other.
- [~] **`update` options** — DONE: pipeline-form `u` (`[...]`) via
  `update_matching_pipeline` (diff-style oplog → change streams see
  `operationType: "update"`); **positional operators (`$` / `$[]` / `$[ident]`)
  + `arrayFilters`** — `secantus-core::update::apply_update_with` resolves
  positional paths (`find_positional_matches` for `$` from the query filter,
  `index_array_filters` + the query matcher for `$[ident]`); `secantus-storage::
  update_matching` always computes positional matches and takes `array_filters`,
  threaded through the command trait's `update_matching_array_filters` (default
  forwards) + adapter + handler (parses per-statement `arrayFilters`). Parity
  suite extended (`apply_update_with` binding + 11 arrayFilters cases).
  **`let` DONE** (update + delete): `resolve_let_vars` (handler) seeds `$$NOW`
  and evaluates each `let` value, threaded as query vars through
  `update_matching_array_filters` / `update_matching_pipeline` /
  `delete_matching_with_let` → the storage query matcher. **Still deferred:**
  `validator`.
- [x] **Collation server-wide (COLLSCAN-correct).** A command `collation` is
  parsed (`util::collation_of` → `secantus_core::collation::parse`) and threaded
  through `find` / `count` / `distinct` / `aggregate` (`$match` + `$sort` + lifted
  fetch) / `update` / `delete`. `secantus-storage::find_matching_with` /
  `count_matching` / `update_matching` / `delete_matching` take a collation and
  **force a COLLSCAN** when one is active (the byte-sortable indexes are
  collation-naive) + a collation-folded in-memory sort (`sort_key` →
  `encode_value_directed` with collation); the query matcher already honoured
  collation. Trait seam: additive `find_collated` / `count_collated` (default →
  uncollated) + collation params on the update/delete option-methods (defaults
  ignore → fakes unaffected); WT adapter routes. **Conservative:** any collation
  forces COLLSCAN (per-index-collation IXSCAN is a later optimisation — explain
  reports COLLSCAN under collation). Non-ASCII / `numericOrdering` collation →
  `BadValue` (the core engine defers; no Python fallback on the Rust server).
  **Deferred:** collection-*default* collation (needs `get_collection_options`);
  `$elemMatch` sub-query collation (matcher passes `None`); per-index-collation
  IXSCAN.
- [x] **`let` on reads (find / aggregate / findAndModify / distinct path).**
  `find_matching_with` gained a `vars` arg → threaded into the query matcher;
  the trait's `find_collated` gained a `let_vars` param (default ignores → fakes
  unaffected), routed by the WT adapter. Handlers resolve `let` via the shared
  `util::resolve_let_vars` (seeds `$$NOW`, evaluates each value) — this also
  fixed `aggregate`, which had passed the *raw* (un-evaluated) `let` doc as vars.
  findAndModify threads `let`+`collation` into its match (upsert-no-match `let`
  still deferred). **+7 gauge (`test_crud_unified` 275→282).**
- [ ] **`find` edges** — empty-collection/empty-result filter validation DONE:
  when nothing matched, the filter is re-run once against an empty document so an
  invalid / unsupported filter surfaces `BadValue` (consistent with the
  non-empty storage-scan path) instead of an empty cursor. The `tailable: true`
  capped-collection poll SHIPPED since (find.rs's tailable producer, mirroring
  `_find_tailable`) — verified live and pinned by
  `test_rust_server_smoke.py::test_tailable_cursor_on_capped_collection`
  (2026-07-17 audit).
- [x] **R2c — `update` command.** Document-, replacement-, and pipeline-form `u`
  all apply; positional operators + `arrayFilters` + `let` + `collation` done;
  sort-rejection (9) + pipeline-stage validation (9 / 168) pre-checks done.
  `validator` still deferred (see "update options" above).
- [x] **`find` command — SHIPPED** (R3 landed long since: first-batch +
  `getMore`/`killCursors`, cursor registry, `secantus-core` projection; the
  Rust-server smoke suite and the pymongo gauge exercise all of it). Entry was
  stale (2026-07-17 audit).
- [x] **`collMod` + collection options + `validator` (insert).** `collMod` is now
  a registered command (`secantus-commands::admin::coll_mod`): merges recognised
  options (`validator` / `validationLevel` / `validationAction` /
  `changeStreamPreAndPostImages` / `capped`+`size`+`max`) into the collection
  (`NamespaceNotFound` 26 if missing); `create` persists the same set. Trait seam:
  `get_collection_options` / `set_collection_options` (defaults no-op/empty →
  fakes unaffected; WT adapter forwards to the existing storage methods). The
  `insert` handler enforces `validator` (code 121) unless
  `bypassDocumentValidation` / `validationAction: warn|off`. **+8 gauge
  (`test_change_stream` +7 from collMod enabling `changeStreamPreAndPostImages`,
  `test_collection` +1).** **Deferred:** `validator` on update/replace (needs the
  post-apply doc in storage); `collMod` TTL-index `index:{expireAfterSeconds}`
  modify; capped-size enforcement.
- [x] **`writeConcernError` for unsatisfiable `w > 1`.** `dispatch` attaches a
  `writeConcernError` (code 100, `CannotSatisfyWriteConcern`) when a request
  carries `writeConcern: {w: int > 1}` and the command succeeded — the single-node
  `secantus` RS can't satisfy it, but the write still happens (mirrors mongod +
  the Python server's `_unsatisfiable_wc_error`). One central place covers every
  write command. **+7 gauge (`test_collection`).**
- [x] **`explain` command.** `secantus-commands::admin::explain` ports
  `commands._explain`: parses the wrapped `find` / `aggregate` / `count` inner
  command, lifts a leading `$match` for aggregate, rejects a journaled /
  `w:"majority"` writeConcern (72 `InvalidOptions`), validates `verbosity`
  (`queryPlanner` / `executionStats` / `allPlansExecution`, else 2 `BadValue`),
  shapes `queryPlanner.winningPlan` (`FETCH` wrapping an `IXSCAN` inputStage with
  `indexName` / `keyPattern` / `direction`, or a bare `COLLSCAN`) via the trait's
  new `explain_plan` method (default → COLLSCAN; WT adapter converts the storage
  `ExplainPlan`), plus an `executionStats` block (runs `find_collated` to count
  `nReturned`) above `queryPlanner` verbosity, and the aggregate
  `stages: [{$cursor: {queryPlanner, …}}, …]` wrapper drivers look for. Collation /
  collectionless explain forces COLLSCAN.
- [x] **Query `$regex` / `$options` + bare BSON regex (Rust query engine).**
  `secantus-core::query` matches regex with the `regex` crate instead of deferring
  to Python: `field_matches` intercepts `$regex` (reading its sibling `$options`)
  and a bare `Bson::RegularExpression`; `op_regex` does `re.search`-style
  unanchored `is_match` over string values + string elements of arrays; flags
  `i`/`m`/`s`/`x` map to `RegexBuilder` (other flag chars ignored, mirroring
  Python's `_re_flags`). Patterns the linear `regex` crate can't compile
  (backreferences, lookaround) now fall back to the backtracking **`fancy-regex`**
  engine (flags ride an inline `(?ims x)` prefix) — closing the
  `test_list_collection_names` gauge gap (pymongo's `^(?!system\.)` negative
  lookahead). Only patterns neither engine compiles, or over the 1000-char cap,
  → `Fallback` (defer). Parity suite extended: curated regex cases (incl.
  lookahead/lookbehind/backref) + a 4000-iteration `test_regex_fuzz_parity`
  (safe-subset patterns/options/subjects; Rust ≡ Python `re`). **Known divergence
  (accepted):** the `regex` crate's `$` matches only end-of-haystack, not before a
  trailing `\n` like Python/PCRE — so `{x:{$regex:"foo$"}}` against `"foo\n"`
  matches on the Python server but not the Rust server. Rare; documented in
  `query.rs` module docs. Fuzz subjects are newline-free to avoid spurious parity
  failures from this gap.
- [x] **View-collection reads — DONE on both servers.** `find` / `aggregate` /
  `count` on a view resolve the view's `viewOn` + pipeline against the base
  collection (recursively for a view-on-a-view): Python `commands._resolve_view`
  (0.5.4b124), Rust `aggregate::resolve_view` (0.5.3-beta.126); a `find` on a view
  is translated into the equivalent aggregate on both. **Rust CRUD cross-cutting
  all done:** `writeConcern` value validation (codes 9/79/14 — 0.5.3-beta.124),
  `validator` on update/replace (post-apply doc via `Storage::update_matching`),
  and `_reject_oplog_rs_write` — direct writes to `local.oplog.rs` /
  `admin.system.users` rejected with code 13 (0.5.3-beta.125), all matching the
  Python server.

- [x] **Engine selection** — `secantus.engine` is the single source of truth
  (`available()` / `selected()` / `set_engine()` / `enabled(component)`); all six
  shims consult it; `SecantusDBServer(engine=)` + `--engine` set it. Unit-tested
  by `tests/test_engine.py` (WT-independent).
- [x] **CI validates the Rust core** — `.github/workflows/test.yml` has a `rust`
  job (Linux, py3.12) that builds `_secantus_core`, runs `cargo fmt`/`clippy
  -D warnings`/`test`, the engine-parity suites, **and the full pytest suite
  under `SECANTUS_ENGINE=rust`** (the real differential check through pymongo /
  WiredTiger). Before this, CI built neither the extension nor selected the Rust
  engine, so the parity suites `importorskip`'d and the rewrite was effectively
  un-validated by CI. Note: the workflow triggers only on push/PR to `main`, so
  this job first runs when the rewrite branch opens a PR / merges — watch its
  first run (the YAML couldn't be exercised in the WT-less dev sandbox).
- [x] **Phase 0 spikes** — BSON fidelity, WiredTiger FFI, sortkey golden
  vectors all green (`rust/`, `rust/run_spikes.sh`).
- [x] **GIL release + benchmark** — every `#[pyfunction]` now wraps its pure-Rust
  compute in `Python::allow_threads` (BSON decode stays GIL-held since it borrows
  the Python buffers; the work runs GIL-free). `benchmarks/engine_bench.py` +
  `benchmarks/RESULTS.md` quantify it. **Findings:** (1) the byte seam does *not*
  eat the win — even paying `bson.encode`/`decode` per call, the Rust path is
  ~2× faster on the leaf ops and ~8× on the pipeline single-threaded; (2) GIL
  release parallelises only *coarse* calls — the pipeline scales ~1.5× on 2
  threads, but cheap per-op leaf calls regress under concurrency (per-call
  GIL release/re-acquire + GIL-held encode/decode dominate the tiny compute and
  ping-pong the GIL). **Implication:** the next real throughput lever for CRUD is
  **coarsening the seam** — batch the per-doc hot loops into one Rust call
  (`query_matches_batch`, `apply_update_batch`) so one GIL release covers many
  docs, the way `apply_pipeline` already does — not more operator coverage. Track
  with the benchmark; validate under a real concurrent server load (needs WT).
- [x] **Batched seam — prototyped and proven across all three CRUD engines.**
  `query_matches_batch` / `apply_update_batch` / `apply_projection_batch` (Rust)
  + `query.matches_batch` / `update.apply_update_batch` /
  `projection.apply_projection_batch` (shims) each do a whole list in one seam
  crossing (one GIL release for all N), whole-batch fallback to per-doc Python if
  any doc defers. Benchmark: each is faster single-threaded (1.06–1.23×) *and*
  fixes the multi-thread regression — per-doc anti-scales (~0.10–0.15× at 4
  threads) while the batch path *scales* (1.11–1.44×), **~10–12× more docs/s
  under 4-thread concurrency**. Parity: `test_batch_matches_parity` /
  `test_batch_apply_parity` / `test_batch_projection_parity` (batch results equal
  per-doc; the whole batch defers iff any doc would). Remaining (needs WT to land
  + measure): wire storage's scan / multi-update / projection paths to the
  `*_batch` shims; then the bytes-in/bytes-out variant (§4 step 2) that skips the
  per-doc Python decode entirely.
- [x] **Phase 1, leaf engine #1: `sortkey`** — ported to Rust behind the fat
  byte seam; pure-Python `secantus.sortkey` delegates when
  `SECANTUS_RUST_SORTKEY=1`. Parity pinned by `tests/test_rust_sortkey_parity.py`
  (curated + 2000-case fuzz, byte-identical).
- [x] **Packaging decision: ship the Rust core as a separate optional package**
  (not bundled into the `secantus` wheel). `crates/secantus-core` is a real
  `secantus-core` package (proper metadata, version **lockstep** with SecantusDB);
  `pip install "secantus[rust]"` pulls it via the new `rust` extra
  (`secantus-core==<this version>`); `.github/workflows/rust-wheels.yml` builds
  the abi3 wheels (maturin-action) across the macOS-arm64 / linux-x86_64 /
  linux-aarch64 / windows matrix + an sdist, and publishes on a release tag via
  trusted publishing. Keeping it separate (vs. merging the two native build
  systems into one wheel) is the bridge to the longer-term goal: the Rust side as
  a **first-class standalone Rust package** — a publishable crate and eventually a
  standalone `secantusdb` server binary.
- [ ] **One-time: configure a PyPI Trusted Publisher for `secantus-core`.**
  On PyPI, add a Trusted Publisher for the (new) `secantus-core` project pointing
  at `.github/workflows/rust-wheels.yml` (environment `pypi`), mirroring the
  `secantus` setup. Until this exists the tag-gated `publish` job fails auth; the
  `build`/`sdist` jobs run on every push/PR and are already CI-validated, so this
  is the only thing blocking the first published `secantus-core` wheel.
- [ ] **Release-process: bump & publish `secantus-core` in lockstep.** The
  `secantus[rust]` extra pins `secantus-core` to the exact SecantusDB version, so
  every release must (a) bump BOTH versions — root `pyproject.toml` and
  `crates/secantus-core/pyproject.toml` — and the `rust` extra pin in the root
  pyproject, and (b) publish both wheels. Wire this into the `/secantusdb-release`
  skill so the two can never drift (the `secantus[rust]` install breaks if the
  matching `secantus-core` version isn't on PyPI).
- [x] **Lib/bindings split (step 1 toward a standalone Rust package).** The Rust
  side is now a Cargo workspace (`crates/Cargo.toml`): `crates/secantus-core` is
  the **pure-Rust engine lib crate** (no PyO3 — engines over `bson`, public API =
  the 8 engines, internal helpers crate-private) and `crates/secantus-core-py` is
  the **thin PyO3 bindings crate** (byte seam + `#[pyfunction]` glue) that builds
  the `_secantus_core` extension / `secantus-core` wheel via maturin. All 62
  engine unit tests, clippy (`-D warnings`), and the maturin wheel build pass; the
  wheel is byte-for-byte the same `secantus_core-<ver>` distribution.
- [x] **Phase 4 sub-phase 0 — WiredTiger FFI foundation (`crates/secantus-wt`).**
  Safe Rust bindings over the vendored WiredTiger C lib (bindgen + `build.rs`):
  `Connection`/`Session`/`Cursor`, the key formats SecantusDB uses
  (`SS`/`SSu`/`SSS`/`SSSu`/`q`/`S`/`u`), `WT_NOTFOUND`/`WT_DUPLICATE_KEY`/
  `WT_ROLLBACK` translation, transactions. Verified against real WiredTiger
  (insert / byte-order scan / point search / NOTFOUND / update / remove / numeric
  `q` ordering / commit+rollback / on-disk reopen); `cargo fmt` + `clippy -D
  warnings` clean (`invoke rust-wt-test`). Excluded from the `crates` workspace so
  the green `secantus-core` CI is untouched. Scoping/status:
  `tasks/rust-rewrite-phase4-scoping.md`.
- [x] **Phase 4 sub-phase 1 — CRUD core (`crates/secantus-storage`).** A `Storage`
  over `secantus-wt` + `secantus-core`'s `sortkey`: `secantus_collections` +
  `secantus_documents` tables, `insert_one` (auto-`ObjectId`, duplicate-`_id`
  rejection), `find_by_id`, `scan_collection` (natural order), `replace_by_id`,
  `delete_by_id`, collection registry, the coarse serialize-everything lock;
  `id_key = sortkey.encode_value(_id)`. 7 integration tests vs real WiredTiger
  (cross-type natural order, db/coll isolation, reopen persistence); `cargo fmt` +
  `clippy -D warnings` clean (`invoke rust-storage-test`). Standalone crate
  (excluded from the `crates` workspace). Also fixed a latent use-after-free in
  `secantus-wt` (the `Cursor` now owns its `S`/`u` key/value buffers).
- [x] **Phase 4 — PyO3 exposure of the Rust storage (`crates/secantus-storage-py`).**
  A `RustStorage` PyO3 class over the BSON byte seam (`_id` wrapped as
  `{"v": id}`), exposing the CRUD core. Crucially this proves the
  **WiredTiger-linking extension builds (maturin → abi3 wheel) and imports**, and
  drives the Rust storage end-to-end from Python (`tests/test_rust_storage_smoke.py`,
  2 tests; `invoke rust-storage-py`) — the core risk of the wheel-matrix gate,
  de-risked on Linux. `cargo fmt` + `clippy -D warnings` clean. Standalone crate
  (excluded from the `crates` workspace).
- [x] **Phase 4 sub-phase 5a — write path in `secantus-storage`.** `update_matching`
  (operator + replacement, `multi`, `upsert`-with-seed, unique enforcement,
  index-entry / multikey upkeep, and the `$v:2` diff vs full-doc oplog split — closes
  the sub-phase-3b deferral), `delete_matching` (filter-routed, `limit`, op `"d"` +
  pre-images), `count_matching`, the shared materialised `candidate_docs` router, and
  public `UpdateOutcome`. 14 WT-backed tests in `tests/write.rs`; `clippy -D warnings`
  + `fmt` clean. **Deferred to the future engine-selection adapter (route to Python):**
  `array_filters`, positional update operators (`$`/`$[]`), `let`/`collation`,
  document `validator`, and geo-index validation on update — the Rust signatures
  don't accept these, so such ops stay on pure-Python `Storage`. (Capped-collection
  eviction now lands natively in `Storage::insert` via `enforce_capped_bounds` —
  oldest non-fresh docs evicted to stay within `size`/`max`, with `op:"d"` oplog +
  pre-images; mirrors Python's `_enforce_capped_bounds_locked`.)
- [x] **Phase 4 sub-phase 5b — collection/database lifecycle in `secantus-storage`.**
  `create_collection` / `drop_collection` / `drop_database` / `rename_collection`
  (move-by-rekey, `drop_target`, source/target guards) / `list_databases`, with the
  `op:"c"` command oplog entries (`create` w/ `idIndex`, `drop`, `dropDatabase`,
  `renameCollection`). Shared `purge_collection_tables` / `colls_of` /
  `collect_idx_rows` / `collect_entry_rows`. 10 WT-backed tests in
  `tests/lifecycle.rs`; clippy + fmt clean.
- [x] **Phase 4 sub-phase 5c — collection stats/introspection in `secantus-storage`.**
  `get_collection_options` (synthetic `local.oplog.rs` shape; `uuid` stays Binary),
  `collection_is_capped`, `collection_data_size`, `index_sizes` (`_id_` + per-index
  packed bytes), `scan_docs_after_id_key`. 7 WT-backed tests in `tests/stats.rs`;
  clippy + fmt clean.
- [x] **Phase 4 sub-phase 5d — full PyO3 surface (`crates/secantus-storage-py`).**
  `RustStorage` now exposes the whole `Storage` interface over the BSON byte seam
  (query/write/count, indexes+TTL, lifecycle, options+stats, oplog+cluster time,
  config setters) plus the exported `EngineFallback` exception (the
  `QueryUnsupported` "defer to Python" signal). Smoke-tested end-to-end against the
  built wheel (`tests/test_rust_storage_smoke.py`, 5 tests; `invoke rust-storage-py`).
  **Deferred to the 5e adapter:** rich E11000 `keyPattern`/`keyValue` propagation
  (DuplicateKey → `KeyError` w/ index name for now) and `BadHint` code mapping.
- [x] **Phase 4 sub-phase 5e-gap-a — users/roles/profiling in `secantus-storage`.**
  `add_user`/`get_user`/`drop_user`/`list_users` + role equivalents (opaque BSON
  record blobs over the `secantus_users`/`secantus_roles` `SS` tables, paginated
  `db`-filtered list), `get_profile`/`set_profile` (validated) over the
  `secantus_profile_settings` `S` table, and `ensure_profile_collection`. PyO3
  bindings + 6 WT-backed tests (`tests/auth.rs`) + smoke coverage. First of the
  5e gap-closure slices (port the 17 server-needed `Storage` methods missing from
  the Rust binding, then the engine adapter).
- [x] **Phase 4 sub-phase 5e-gap-b — batch insert + prune-all in `secantus-storage`.**
  `insert(db, coll, docs, ordered) -> (inserted, errors)` (auto-`_id`, unique +
  dup-`_id` write-errors, ordered/unordered, batched `op:"i"` oplog) and
  `prune_ttl_all_collections`. PyO3 bindings + 7 WT-backed tests
  (`tests/batch_insert.rs`) + smoke. **Deferred (no Rust capped support):**
  capped-collection eviction inside `insert` + geo-index validation on insert —
  capped collections under `SECANTUS_ENGINE=rust` won't enforce bounds, and a bad
  geometry won't be rejected (just indexed as no geo entry). Close when capped /
  geo-validation land in the Rust storage.
- [x] **Phase 4 sub-phase 5e-gap-c — change-stream tailable-wait in `secantus-storage`.**
  `wait_for_oplog(after_seq, timeout_ms)` (Condvar paired with the oplog mutex;
  one bounded wait, no lost-wakeup) + `notify_oplog_waiters()` (for killCursors) +
  `emit_oplog` notify. PyO3 `wait_for_oplog` releases the GIL while blocking. The
  Rust equivalent of `storage._oplog_cv`; `oplog_tail_seq_nolock` subsumed. 4
  cross-thread WT-backed tests (`tests/condvar.rs`) + threaded smoke. `commands.py`
  refactor off the raw `_oplog_cv` onto this method pair lands with the adapter.
- [x] **Phase 4 sub-phase 5e — SUPERSEDED by the two-server model**
  (`tasks/rust-server-plan.md`; 2026-07-17 audit). The `secantus.engine`
  storage-selection + Python-`Storage`-adapter-over-`RustStorage` +
  `SECANTUS_ENGINE=rust` gauge this entry planned is exactly the retired
  in-process model; its goal — the full Rust storage validated by the pymongo
  gauge — is delivered by the Rust *server* (99.4%, identical failure set to
  the Python server). Original scope for reference: Remaining gaps:
  `checkpoint`/`close`/`create_archive` (admin/`fsync`/backup — none block the core
  conformance suites; `close` handled adapter-side). Then the `secantus.engine`
  storage-selection + Python `Storage` adapter over `RustStorage` (BSON seam,
  `EngineFallback` → Python-operators-over-Rust-docs, E11000/`BadHint` translation,
  `commands.py` getMore refactored onto `wait_for_oplog`/`notify_oplog_waiters`),
  then `test_storage.py`/`test_crud.py` + pymongo gauge under `SECANTUS_ENGINE=rust`.
- [x] **Phase 4 — storage keystone: (a) SUPERSEDED, (b) DONE** (2026-07-17
  audit). (a) `secantus.engine` selection is the retired in-process model —
  the Rust storage serves the Rust server instead. (b) the shipping
  `wheels.yml` cibuildwheel matrix already builds with
  `SECANTUS_BUILD_STORAGE_ENGINE=ON` (lines ~113-130), so the flag-flip this
  entry was waiting on happened. Original scope: (a) wire
  `secantus.engine` storage selection so `SecantusDBServer` can use the Rust
  `Storage` under `SECANTUS_ENGINE=rust` — **gated on porting the rest of the
  `Storage` surface** (the server needs `find_matching`/indexes/oplog/etc., not
  just CRUD), i.e. sub-phases 2-4 (indexes, geo, oplog/change-streams) plus the
  remaining higher-level methods (users/roles/profile/`checkpoint`/
  `create_archive`); then the conformance gate
  (`test_storage.py` / `test_crud.py` under `SECANTUS_ENGINE=rust`).
  (b) **The wheel-matrix gate — bundled behind an off-by-default build flag.**
  Decided (after rejecting a separate companion wheel — see scoping doc): the Rust
  `_secantus_storage` extension is built INTO the `secantus` wheel by the main
  wheel's CMake, against the WiredTiger that wheel already builds, gated behind the
  `SECANTUS_BUILD_STORAGE_ENGINE` CMake option (default **OFF** — wheel unchanged,
  no Rust/clang needed when off). The `storage-engine` job in `test.yml` is a 3-OS
  matrix (Linux / macOS / Windows) validating the flag-on build + import + smoke;
  the WT system-link flags are cfg-gated per target OS in `secantus-wt/build.rs`
  (Linux `pthread`+`rt`+`dl`; macOS `pthread`+`dl`; Windows none). Remaining: only
  once engine-selection makes the Rust storage engine selectable, flip the flag on
  in the shipping `wheels.yml` / cibuildwheel matrix (and add the abi3 storage
  extension to the wheel's repair/tag handling there).
- [ ] **Toward a standalone Rust package — (b) DONE, (a) needs a crates.io
  decision** (2026-07-17 audit). (b) the `secantusdb` binary crate exists and
  ships (`secantusd-rs`, the `secantusdb-v*` release-binaries track). (a)
  flipping `publish = false` and publishing `secantus-core` to crates.io needs
  Joe's crates.io account + a public-API freeze decision — flagged. Original: With the lib/bindings
  split done, the remaining steps to "ultimately a Rust package": (a) settle the
  `secantus-core` lib's public API and flip `publish = false` → publish to
  crates.io; (b) add a `secantusdb` **binary crate** (a thin `main` over the
  engines + storage) — gated on the storage keystone (Phase 4 above), since a
  standalone server also needs storage in Rust, not just the operator engines.
- [ ] **Make Rust the *recommended* default — a product/docs decision for
  Joe** (2026-07-17 audit). The byte-seam overhead rationale below is moot
  under the two-server model (the Rust server has no per-call seam); what
  remains is the positioning call (docs/README recommending `secantusd-rs` /
  the embedded Rust handle as the default) plus the R8 gauge evidence.
  Original:
  Currently every component defaults to Python; `SECANTUS_ENGINE=rust` opts in.
  With the optional package now shipping (above), recommending Rust by default
  for installs that have the extension still wants: a decision on the byte seam's
  per-call `bson.encode({"v": value})` overhead vs passing values without
  re-encoding (see the batching work in `benchmarks/`). The Python engine remains
  a permanent, selectable mode regardless.
- [x] **Collation (query matcher).** `crates/secantus-core/src/collation.rs`
  implements the ASCII-safe, Unicode-version-independent cases (case-insensitive
  ASCII = ASCII-lowercase; accent-strip is a no-op on ASCII; strength-3 identity
  handles any string); threaded through the Rust query matcher's string `$eq`/
  `$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$nin`. `query.matches` now delegates to
  Rust *with* a collation, which defers to Python for non-ASCII-under-transform
  and `numericOrdering`. Parity: `tests/test_rust_query_parity.py` collation
  cases + 4000-case fuzz.
- [x] **Collation-aware sortkey in Rust.** `sortkey.encode_value` /
  `encode_value_directed` thread a collation through to the Rust encoder
  (`collation::normalize_index_bytes`, where `numericOrdering` is the raw-bytes
  identity — matching Python's `_encode_string` skipping normalisation when
  `supports_index_encoding` is false). ASCII normalisation is reproduced;
  non-ASCII transforms defer to Python. Parity:
  `tests/test_rust_sortkey_parity.py` collation cases.
- [x] **Phase 1, leaf engine #2: `query.matches`** — common operators ported
  to Rust (`crates/secantus-core/src/query.rs` + `numeric.rs`) behind the byte
  seam; `secantus.query.matches` delegates when `SECANTUS_RUST_QUERY=1` (now
  including collation — see the collation item above). The Rust matcher returns
  `None` (→ pure-Python fallback) for anything not reproduced faithfully:
  `$jsonSchema`, geo,
  **any regex**, `$all`, structural/compound equality (array/doc operands),
  bool-as-int comparison, exotic BSON types. Parity pinned by
  `tests/test_rust_query_parity.py` (curated + 6000-case fuzz).
- [ ] **Widen the Rust query matcher (Rust server)** — the matcher backs the
  Rust server directly; there is no Python fallback in that path, so an unported
  construct surfaces as `BadValue`. **Done:** `$all` (element equality via
  `expressions::py_eq`, **regex elements via `op_regex`**, and — fixed 2026-07-17
  — a **scalar** field value matched like a one-element array, plus `$all: []`
  matching nothing; this was a real dual-server correctness bug found by the R8
  gauge triage: `{tags: {$all: ["red"]}}` silently missed scalar `tags: "red"`
  on *both* servers, verified against mongod 7.0.12 and fixed in `query._op_all`
  + `query::op_all` with three-way parity); structural array/doc *equality*
  (`array_eq` / `doc_eq`); bool-as-int `$gt`/`$lt`/`$gte`/`$lte` comparison
  (0.5.3-beta.119 — numeric vs int/long/double, no-match vs any other type,
  matching Python's `<`). **Structural ordering under `$gt`/`$lt` now done:**
  array-vs-array lexicographic range (2026-07-13) and — fixed 2026-07-17 —
  **doc-vs-doc ordering** (field-by-field: key string compare, else recurse,
  else shorter-first), another dual-server bug found by the R8 triage:
  `{a: {$gt: {x: 1}}}` returned nothing on *both* servers (Python's
  `operator.gt` raises on dicts → swallowed no-match), where mongod orders
  embedded documents. Fixed in `query._try_cmp` (via `ordering._bson_lt`) and
  `query::compare_values` (via `order::bson_lt`), three-way mongod-verified; the
  document-vs-scalar type bracket still no-matches correctly. **Array-vs-array
  cross-type element ordering also done** (2026-07-17, same R8 triage): array
  elements order by *full* BSON order (type rank first), so `{a: {$gt: [1,2]}}`
  matches `a: [1,"x"]` (string element outranks number) — both servers returned
  nothing before (Python's list `<` raises str-vs-int). Fixed via `_bson_lt` /
  `order::bson_lt` element-wise; the stale
  `array_vs_array_cross_type_element_no_match` unit test that pinned the bug is
  corrected. **`$mod` fidelity fixed** (2026-07-17, same R8 triage): both
  servers now truncate the value AND divisor toward zero to integers, exclude
  bool, and use C-style truncated modulo — the Rust server previously *errored*
  (`BadValue`) on a double-valued field and both servers wrongly matched a bool
  field; three-way mongod-verified, with the zero-divisor / malformed-spec
  errors reproduced. **Exotic-type comparison is handled natively now (verified
  2026-07-19, NOT deferred — the earlier "still deferred" wording was stale):**
  `query::compare_values` compares JS code / symbol / with-scope code as text and
  no-matches DBPointer / undefined itself (the 2026-07-13 cross-type range slice),
  so a top-level exotic scalar under a range op resolves on the Rust server without
  a Python round-trip. **Still deferred where faithful:** an exotic-text value
  *under a collation* (ICU folding), a **DBPointer nested inside an array/document**
  being range-compared (`order::bson_lt` → `None`, Python's type-*name* tiebreak),
  and a **Decimal128-valued `$mod` field on the Rust server** (`int(Decimal)` is
  exact to 34 digits, which an `f64` truncation can't reproduce — the standing
  Decimal128 precision-parity deferral; the Python engine handles it).
  **`$size` argument validation fixed** (2026-07-17, same R8 triage): a
  negative `$size` now errors (was a silent no-match), a bool is rejected (was
  accepted as 1), and an integer-valued float `2.0` is accepted as `2` (was
  rejected) — mongod's three parse-error stems ("Expected a number" / "an
  integer" / "a non-negative number", code 2) reproduced on the Python server
  and mapped to BadValue on the Rust server; three-way mongod-verified. (The
  retired in-process "flip `query.matches` default to Rust" item is gone — the
  two-server model has no per-call engine selection; `_secantus_core` is only
  the parity-test vehicle now.)
- [x] **Phase 1, leaf engine #3: `update.apply_update`** — the common
  deterministic operators ported to Rust (`crates/secantus-core/src/update.rs`,
  with the `secantus.paths` dotted-path helpers): replacement-style, `$set`,
  `$setOnInsert`, `$unset`, `$inc`, `$mul`, `$push`, `$pop`, `$rename`, plus
  `_id` immutability. `secantus.update.apply_update` delegates when
  `SECANTUS_RUST_UPDATE=1`. Returns `None` (→ pure-Python fallback) for pipeline
  (array) updates, positional ops / array filters, `$currentDate`,
  `$min`/`$max`/`$pull`/`$addToSet`/`$bit`, Decimal128/non-numeric arithmetic,
  and every error condition (so the exact `UpdateError`/`PathError` is raised by
  Python). Parity pinned by `tests/test_rust_update_parity.py` (curated +
  6000-case fuzz).
- [ ] **Widen the Rust update operators (Rust server)** — remaining defers where
  faithful. **`$inc`/`$mul` non-number operand fixed** (2026-07-18, found by the
  R8 update-op triage): both servers wrongly COMPUTED with a bool operand
  (`5 + True = 6`, `5 * False = 0` — the recurring `bool`-is-`int` root cause)
  and Python raw-raised `ValueError`/`TypeError` on a string/null operand.
  Both now reject a non-number operand: the Python server raises mongod's exact
  `code 14 "Cannot increment/multiply with non-numeric argument: {field: value}"`;
  the Rust server surfaces `BadValue` (the standing update error-code gap — the
  pure core returns a code-less `Fallback`, same as null-field `$inc`), but the
  correctness contract (reject, don't compute) now holds on both, three-way
  mongod-verified. **`$pop`/`$position`/`$slice`/`$bit` bool-argument cluster
  fixed** (2026-07-18, same triage sweep): `$pop: true` computed (True as 1, pop
  last) on *both* servers, and `$push` `$position`/`$slice: true` computed on the
  Rust server — all now reject a bool argument. The Python server reports
  mongod's exact codes (9 for `$pop`, 2 for `$position`/`$slice`/`$bit`) and
  messages; the Rust server surfaces `BadValue` (code 2 for all — matching mongod
  for `$position`/`$slice`/`$bit`, the error-code gap only for `$pop`). `$pop`
  also now errors on a non-±1 number (was a silent no-op). Four of the seven
  R8-triage bugs share the root cause: Python's `bool` being an `int` subclass
  sneaking through `isinstance(int)` / `as_int_like` numeric checks.
  **Aggregation-expression bool-argument cluster fixed** (2026-07-18, same
  root cause): `$round`/`$trunc` (place), `$arrayElemAt`/`$slice`/`$indexOfArray`/
  `$substrCP` (index), and `$sortArray` (sortBy) all *computed* a bool argument
  (`as_int_like(Boolean) → 0/1`) instead of rejecting it. Both servers now reject:
  the Python server carries mongod's exact per-op codes (16004 / 28690 /
  28725 / 28727 / 2942507 / 34450 / 34452 / 40096), the Rust core defers →
  `BadValue`. `$range` (34443/34445/34447) and `$sortArray` already deferred on the
  Rust side (`range_int` / the `_ => Fallback` sortBy arm); three-way mongod
  7.0.12-verified. **`$substrBytes` / `$substr` bool-argument cluster fixed**
  (2026-07-18): `$substrBytes` rejected a bool with mongod's codes (16034 start /
  16035 length, note the verbatim double-space message); and `$substr` — which
  mongod treats as a deprecated *byte-based* alias of `$substrBytes` — was
  mis-aliased to code-point `$substrCP` on both servers, so it diverged on
  multi-byte strings and reported the wrong bool code (34450 vs 16034). `$substr`
  now aliases `$substrBytes` on both servers; three-way mongod-verified.
  **Follow-ups still open (NOT fixed — separate slices):** (1)
  **whole-number-double index** — these operators reject *all* float indices, but
  mongod *accepts a whole-number double* (`$arrayElemAt: [[..], 2.0]` → element 2;
  `-1.0` → last) and *rejects a fractional* one with a per-op code, NOT truncation.
  Fully probed against mongod 7.0.12 (2026-07-18): `$arrayElemAt` frac → **28691**,
  `$slice` → **28726**, `$substrCP` → **34451**, `$indexOfArray` → **40096** (same as
  bool), `$range` → **34446**, `$round` place → **51082** ("precision argument to
  $round must be a integral value", double-space verbatim). **Exception:**
  `$substrBytes` (and thus `$substr`) *accepts any double and truncates* (`1.7` →
  same as `1.0`) — no fractional rejection. Fix = accept int-or-whole-double
  (coerce to int), reject fractional with the per-op code (except substrBytes
  truncates); needs a shared `_as_int_index` helper on both engines. Best split by
  operator family (array indices first — the common case). **Array operators done
  (2026-07-18):** `$arrayElemAt` / `$slice` / `$indexOfArray` accept a whole-number
  double (coerce to int, compute on *both* servers) and reject a fractional one
  with the per-op code (28691 / 28726 / 28728 / 40096) — via `_int_index` /
  `_FractionalIndex` (Python) + the `IdxCoerce` enum (Rust). **String/range/round
  operators done (2026-07-18):** `$substrCP` (start 34451 / length 34453), `$range`
  (start 34444 / end 34446 / step 34448), and `$round`/`$trunc` precision (51082)
  accept a whole-number double and reject a fractional one — same helpers, three-way
  mongod 7.0.12-verified. (`$substrBytes`/`$substr` already accept any double and
  truncate — mongod-correct, left as-is.) So the whole-double-index sweep is
  **complete** across the aggregation surface. Remaining edge: a *huge* whole double
  (> 2^31) is accepted here but mongod rejects it (32-bit-representable check) — a
  narrow case; both SecantusDB engines stay mutually consistent (both null/whole).
  (2) **`$substrBytes` / `$substr` mid-UTF8-character — FIXED (2026-07-18).**
  A byte range that splits a UTF-8 char now raises mongod's codes on the Python
  server (28656 start-on-continuation-byte / 28657 end-mid-character, verbatim
  double-space messages) and defers → `BadValue` on the Rust server. A fuzz run
  surfaced the subtlety: mongod rejects a continuation-byte start *even for an
  empty (length 0) range*, which the Rust core's `std::str::from_utf8` slice check
  missed (an empty slice is valid UTF-8) — so **both** engines got an explicit
  boundary check (a negative start keeps legacy slice semantics on both).
  Three-way mongod 7.0.12-verified. **`$substr*` negative-index rejection — FIXED
  (2026-07-18):** a negative start (`$substrBytes`/`$substr` → 50752;
  `$substrCP` → 34455) and a negative `$substrCP` length (34454) now raise mongod's
  codes on the Python server and defer → `BadValue` on the Rust server, instead of
  a Python-style negative-index slice; a negative `$substrBytes` length is still
  "to end" (mongod-correct). **`$substrBytes` float truncation — FIXED
  (2026-07-18):** `$substrBytes` now accepts any double start/length and truncates
  toward zero (`1.7`→1, `2.9`→2, `0.9`→0), matching mongod (unlike `$substrCP`,
  which rejects fractional). Both engines *compute* the truncated result — the Rust
  core via a new `trunc_index` helper (`d.trunc() as i64`), not defer — so a valid
  double substring agrees on both servers; a truncated-negative start still hits
  50752. **With this, `$substrBytes` / `$substrCP` numeric-argument handling fully
  matches mongod** (bool, byte-vs-codepoint aliasing, whole-double / fractional /
  truncation, UTF-8-split, negative indices). **Aggregation-stage numeric-arg
  fidelity (`$limit` / `$skip`) — FIXED (2026-07-18):** both stages coerced their
  arg with a naive `int(spec)`, so `$limit: 0` returned nothing (mongod: 15958
  "the limit must be positive"), `$limit: -1` negative-sliced, and bool/fractional
  were silently coerced; the Rust server had the mirror bug — it *rejected a valid
  whole-double `$limit: 2.0`* (its `positive_int` used `as_int_like`, which returns
  None for a double) while coercing bool→1. Now both accept a whole double and
  reject bool/fractional/negative (`$limit` 5107201, `$skip` 5107200; zero-`$limit`
  15958) — Python raises the code, the Rust core defers via the new
  `stage_nonneg_int`. **`$sample` size — FIXED (2026-07-18):** the Python server
  now rejects a bool size (28746) and a negative size (28747) and truncates a
  fractional double; the Rust server already validated (its `$sample` lives in the
  server crate, not the deferring core), so this was a Python-only fix. **With this
  the aggregation numeric-argument trio (`$limit` / `$skip` / `$sample`) matches
  mongod on both servers.** **Done
  (0.5.3-beta.118):** `$min`/`$max` (Python `<` for numeric /
  string / date pairs, bool-as-int; a cross-type comparison Python would raise on
  defers), `$pull`/`$addToSet` (Python `==` membership incl. bool-as-int and
  structural equality via `expressions::py_eq`); `$bit`, `$each` (for `$push` /
  `$addToSet`, incl. `$push` `$position` / `$slice`; `$sort` defers) were already
  native. **Positional operators (`$` / `$[]` / `$[id]`) and `arrayFilters` work
  on the Rust *server*** via the storage layer (`update_matching_array_filters`),
  even though the pure `secantus-core` engine defers them. **Done since this entry
  (verified 2026-07-19 against the running Rust server — the three items below were
  stale, all now work):** (1) **pipeline (array) updates** run on the Rust *server*
  via `crud.rs` → `Storage::update_matching_pipeline` → `aggregate::apply_pipeline`
  (allowed-stage set `$set`/`$addFields`/`$unset`/`$project`/`$replaceRoot`/
  `$replaceWith`, `_id`-preservation; smoke-verified: a computed `$set`+`$unset`, a
  `$replaceWith` that reroots a sub-doc while preserving `_id`, and a disallowed
  `$match` stage rejected with mongod's "stage $match not allowed in pipeline
  updates"); (2) **`$push` `$sort`** sorts whole-elements (`1`/`-1`) or `{field:dir}`
  in BSON order (`update::push_sort` via `order::cmp`); (3) **`$min`/`$max`** compute
  the full cross-type order via `order::bson_lt`. **Still deferred where faithful:**
  a `$push` `$sort` over an element outside `is_sortable`'s transitive subset
  (bool / NaN / Binary / Timestamp / Regex / Min-MaxKey / Decimal128 / exotic — the
  deliberate Timsort-parity defer, since a non-transitive comparator would diverge
  from Python's stable sort), and a `$min`/`$max` **DBPointer** operand (`bson_lt`
  → `None`, type-name tiebreak). **`$inc` / `$mul` on a Decimal128 field**
  (verified: Python computes `5.5 + 1.5 = 7.0`; the Rust server errors) is a
  **parity-risk deferral, not a coverage oversight** — the Python oracle does the
  arithmetic in `decimal.Decimal`'s 28-significant-digit `ROUND_HALF_EVEN`
  context (`numerics._combine`), which no Rust decimal library reproduces exactly
  (`rust_decimal` caps at ~28–29 digits with different rounding, native
  decimal128 uses 34 digits). Since the Rust server has no runtime Python oracle,
  shipping an approximation would be a silent divergence — same class as the
  named-IANA-timezone deferral. Leave deferred unless a decimal path with proven
  bit-for-bit parity to Python's `decimal` context appears. Field-order on `$set`
  of an existing
  key is **verified correct** (0.5.3-beta.22+): `bson::Document::insert` preserves
  an existing field's position and appends new keys — matching mongod. (The
  retired "flip `update` default to Rust" framing is dropped — no in-process
  default exists in the two-server model.) **`$currentDate` is done** (0.5.3-beta.47):
  the core engine still defers it (non-deterministic), but the Rust *storage* layer
  resolves it to a concrete clock value before `apply_update_with` (date / timestamp,
  one clock read per op), via `resolve_current_date` — mirrors `update.py`. Closes
  the pymongo gauge `test_update_and_replace`.
- [x] **Phase 1, leaf engine #4: `expressions.evaluate`** — a high-value core of
  the aggregation expression language ported to Rust
  (`crates/secantus-core/src/expressions.rs`): field paths / `$$var` / `$$ROOT` /
  `$literal`; comparison (`$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`); logic
  (`$and`/`$or`/`$not`); control flow (`$cond`/`$ifNull`/`$switch`); arithmetic
  (`$add`/`$subtract`/`$multiply`/`$divide`/`$mod`); and common array ops
  (`$size`/`$arrayElemAt`/`$first`/`$last`/`$concatArrays`/`$reverseArray`/`$in`).
  `secantus.expressions.evaluate` delegates when `SECANTUS_RUST_EXPR=1`. Because
  expressions are recursive, the evaluator returns `None` (whole-call fallback)
  if ANY operator/value in the tree isn't ported. Parity pinned by
  `tests/test_rust_expressions_parity.py` (curated + 8000-case nested fuzz).
  **This also unlocked `$expr` in the Rust query matcher** (it now calls the Rust
  evaluator directly, Rust->Rust) — `query_matches` gained a `vars` arg threaded
  through `$expr`.
- [x] **Date arithmetic** — `$dateAdd`/`$dateSubtract`/`$dateDiff`/`$dateTrunc`
  ported via dependency-free civil-date math (`days_from_civil` inverse +
  `days_in_month`), UTC, bounded to Python's datetime range (out-of-range
  defers). The 8000-case date fuzz caught three real Python-semantics quirks:
  `$dateDiff second` truncates toward zero (`int(total_seconds())`) while
  hour/minute floor (`// n`); `$dateTrunc` truncates the *field* (keeping
  higher fields) not the total-since-midnight; and the sub-day `$dateDiff`
  units route through Python's lossy `timedelta.total_seconds()`
  (`total_microseconds / 10**6`, an int/int correctly-rounded divide) — Rust
  reproduces the same single-rounding float path, guarded to `|total_us| <=
  2**53` (where the `as f64` conversion is exact) and deferring extreme dates
  to Python so the double-rounding can't diverge.
- [ ] **Remaining expression operators are principled defers** (cannot be
  reproduced without a fidelity risk; all run pure-Python): regex
  (`$regexMatch`/`$regexFind`/`$regexFindAll` — Python `re`);
  `$dateToString`/`$dateFromString` (`strftime`/`strptime` + timezones);
  `$convert`/`$toDecimal` and float-`str()` / string→number / Decimal128
  conversion edges; `$round`/`$pow`/`$trunc` (rounding mode) and the
  transcendentals `$exp`/`$ln`/`$log`/`$log10` (last-ULP vs Python's libm);
  `$sortArray` (Python `sorted()` ordering/stability, raises on mixed types);
  non-ASCII `$toLower`/`$toUpper` and default-whitespace `$trim`. Each is a
  deliberate fallback, not a gap. (`$rand` is the lone exception that is
  *evaluated* in Rust rather than deferred — see the "now also handled" item —
  because deferring would error the standalone server, which has no Python.)
- [x] **Widen the Rust expression evaluator** — now also handled:
  `$slice`/`$indexOfArray`; ASCII `$concat`/`$toLower`/`$toUpper`/`$strLenCP`/
  `$split`/`$substrCP`; `$mergeObjects`/`$objectToArray`; the scope-introducing
  `$let`/`$map`/`$filter`/`$reduce`; and UTC date component extractors
  (`$year`/`$month`/`$dayOfMonth`/`$hour`/`$minute`/`$second`/`$dayOfWeek` +
  `$dateToParts`, via a dependency-free civil-date algorithm); a safe subset of
  conversions (`$toInt`/`$toDouble`/`$toBool`/`$toString` for numbers/bools/
  strings); exactly-deterministic math (`$abs`/`$floor`/`$ceil`/`$sqrt`); and
  `$range`/`$strLenBytes`/`$arrayToObject`; and the date-arithmetic ops
  `$dateAdd`/`$dateSubtract`/`$dateDiff`/`$dateTrunc` (UTC, civil-date math —
  see the "Date arithmetic" item); and `$rand` (uniform double in [0, 1) via the
  `rand` crate — evaluated directly, not byte-pinned to Python, since the
  standalone server has no Python to defer to). Remaining whole-call fallbacks to widen
  where faithful: `$dateToString`/`$dateFromString` (timezones, `strftime`/
  `strptime`); `$round`/`$pow`/`$trunc` (rounding mode) and transcendental math
  (`$exp`/`$ln`/`$log`/`$log10` — last-ULP divergence risk vs Python's libm);
  the conversion edges deferred above (Decimal128, string→number parsing, float
  `str()`, `$convert`/`$toDecimal`); `$sortArray` (uses Python `sorted()`
  ordering/stability + raises on mixed types); and non-ASCII case /
  default-whitespace `$trim` (defer for Unicode-fidelity safety). Regex ops
  (`$regexMatch`/…) need Python `re`. Done recently: string index/byte ops,
  `$getField`/`$setField`, `$zip`.
- [x] **Phase 1, leaf engine #5: `projection.apply_projection`** — inclusion /
  exclusion / `$slice` / `$elemMatch` projection shapes ported to Rust
  (`crates/secantus-core/src/projection.rs`). `secantus.projection` delegates
  when `SECANTUS_RUST_PROJECTION=1`; returns `None` for mixed inclusion/exclusion
  (Python raises), nested-document specs, unusual `$slice` arg types, and
  `$elemMatch` sub-filters the matcher defers. Parity:
  `tests/test_rust_projection_parity.py` (curated + 6000-case fuzz).
- [x] **Phase 1, leaf engine #6: `diff.compute_update_description`** — the
  change-stream `$v: 2` update diff ported to Rust
  (`crates/secantus-core/src/diff.rs`), reusing the expression engine's Python-`==`
  semantics. `secantus.diff` delegates when `SECANTUS_RUST_DIFF=1`; defers on
  Decimal128 / exotic values. Parity: `tests/test_rust_diff_parity.py` (curated +
  6000-case fuzz).
- [ ] **Shared path write helpers** (`set_path`/`unset_path`) now live in
  `paths.rs` alongside the read helpers; `update.rs` keeps a thin `Fallback`-
  mapping wrapper. (Note for the eventual default-flip: same `bson::Document`
  field-order question as the update engine applies to projection/diff outputs.)
- [x] **All six leaf engines are ported** (`collation` landed — see the
  collation items above). Remaining Phase-1 work is *widening* the
  already-ported engines (see the per-engine "widen" items above).
- [x] **Phase 2, aggregation pipeline — first slice.** `apply_pipeline` ported
  to Rust (`crates/secantus-core/src/aggregate.rs`) behind a list-of-docs byte
  seam (`{"d": [...]}` / `{"p": [...]}`), reusing the ported leaf engines
  (`query::matches`, `expressions::evaluate`, the `paths` helpers) so a pure
  pipeline runs end to end in Rust without re-entering Python per stage / per
  doc. Stages handled: `$match`, `$limit`, `$skip`, `$count`, `$project`
  (inclusion / exclusion / computed, mirroring `_project_one`'s mapping-only
  `_path_present`), `$addFields`/`$set`, `$unset`, `$replaceRoot`/`$replaceWith`.
  `secantus.aggregate.apply_pipeline` delegates when `SECANTUS_RUST_AGGREGATE=1`.
  **Graceful whole-pipeline fallback:** any unported stage or any deferred inner
  expression makes `apply_pipeline` return `None` and the pure-Python pipeline
  runs. Parity: `tests/test_rust_aggregate_parity.py` (curated + 4000-case fuzz).
- [x] **Widen the pipeline: `$sort` + `$unwind`.** `$sort` ported via a faithful
  cross-type BSON comparator (`crates/secantus-core/src/order.rs`, mirroring
  `_bson_lt` / `_bson_type_rank` — type ranks, doc/array recursion, the unified
  numeric type incl. NaN-is-equal). **Subtlety that drives the fidelity gate:**
  `sort_docs` wraps keys in `_SortKey` whose `__eq__` is Python `==` (so
  `False == 0`, `True == 1`, `1 == 1.0`) but whose `__lt__` is rank-based
  `_bson_lt`; a tuple sort advances to the next field only when `==` is True, so
  the comparator is *non-transitive* whenever `bool`/`NaN` mix with numbers (or
  the `==`-False-but-`<`-both-False types — Binary-with-subtype / Timestamp /
  Regex / Min/MaxKey appear). `order::is_sortable` therefore only green-lights
  the types where Python `==` agrees with `cmp == Equal` (null / non-NaN numbers
  / string / datetime / ObjectId / docs+arrays thereof) and defers the rest
  (bool, NaN, Decimal128, Binary, Timestamp, Regex, Min/MaxKey, exotic) to
  Python's Timsort. Single + multi-field, both directions, stable. `$unwind`
  ported (string + doc spec, `includeArrayIndex`, `preserveNullAndEmptyArrays`,
  missing/null/non-array/empty edges). **Refactor:** the pure comparator moved
  out of `storage.py` into a new I/O-free `secantus.ordering` module
  (`sort_docs`/`_bson_lt`/`_bson_type_rank`/`_SortKey`/`_to_decimal`; `storage`
  re-exports them) so `sort_docs` is importable without the WiredTiger
  extension — matching the pure-operator-engine layering. Parity:
  `tests/test_rust_aggregate_parity.py` (mixed-type sort corpus + fuzz with
  arrays / mixed-type fields).
- [x] **Widen the pipeline: `$group` + `$sortByCount`.** Ported in
  `crates/secantus-core/src/group.rs`. **Group-key bucketing** matches Python's
  dict semantics — each `_id` value is canonicalised into a hashable `GKey`
  (numbers + bool normalised through `numeric::NumVal`, so `1 == 1.0 == True`
  collapse into one bucket; docs/arrays recurse via `_hashable`'s key-sorted
  tuple) preserving first-seen `_id` and insertion order; key types we can't
  canonicalise faithfully (Decimal128, NaN, Binary/Timestamp/Regex/Min/MaxKey,
  exotic) defer. **Accumulators** reproduce Python's exact semantics:
  `$sum`/`$count`/`$avg` (running int-vs-float via a `Num` enum; `$avg` is
  always a double and the field stays *absent* when no non-null value is seen —
  matching the pure code that never creates the bucket key; non-numeric operands
  `TypeError` → defer); `$min`/`$max` (native `<`/`>` via `expressions::py_order`
  — cross-type pairs that Python would raise on → defer, not guess; null is a
  no-op that never "unsets"); `$first`/`$last`/`$push`; `$addToSet` (membership
  via Python `==` / `expressions::py_eq`). `$sortByCount` = group + stable count-
  descending sort. Validated across the in-repo curated + 5-seed fuzz **and** 8
  extra local seeds (~1,650–1,730 group/sortByCount pipelines handled per 4000,
  zero mismatches). `numeric::NumVal` gained `Eq + Hash`; `expressions::py_order`
  /`py_eq` are now `pub`.
- [x] **Widen the pipeline: `$bucket` + `$facet`.** `$bucket`
  (`group::bucket_stage`) places each doc into the half-open boundary range
  `boundaries[i] <= value < boundaries[i+1]` using `expressions::py_order`
  (Python's native `<=`/`<`, so cross-type / Decimal128 / array-doc boundaries
  defer rather than guess; NaN / TypeError fall through to `default`), then runs
  the `output` accumulators per bucket (reusing the `$group` accumulator
  machinery via `accumulate_into`). Reproduces the pure quirks: empty buckets
  emit only `{_id}` (accumulator fields are never created), an explicit `null`
  default counts as absent, missing/empty `output` falls back to
  `{count: {$sum: 1}}`, and pathological Python-equal bucket keys defer (the dict
  would collapse them). `$facet` (`aggregate::facet_stage`) runs each named
  sub-pipeline over a clone of the input via the recursive `apply_pipeline` and
  collects the results — any sub-pipeline that defers defers the whole stage.
  Validated across curated cases + the 5-seed in-repo fuzz **and** 8 extra local
  seeds (~500–580 `$bucket` and ~370–410 `$facet` pipelines handled per 5000,
  zero mismatches).
- [x] **Widen the pipeline: `$densify` (numeric path).** `densify::densify_stage`
  ports the numeric densify — partition the docs (Python dict semantics via the
  shared `group::GKey`), sort each partition by the numeric field, and fill every
  multiple of `step` strictly between the bounds (`"full"`/`"partition"` = the
  partition's observed min/max; explicit `[lo, hi]`). The cursor arithmetic
  mirrors Python exactly (a `Num{Int,Float}` enum so `int + int` stays int and
  widens to f64 once a float enters; `_densify_canon` collapses an
  integer-valued float filler back to an int), and the `existing_values`
  membership reproduces the set's `1 == 1.0 == True` collision via
  `numeric::NumVal`. The no-input-docs-with-explicit-bounds case and the
  "originals at/beyond `hi`" tail are reproduced. **Defers** to Python: any
  `range.unit` (date densify — fixed-duration `timedelta` *and* variable-length
  `relativedelta` month/quarter/year), non-numeric field values / bounds /
  partition keys (Python's `sorted` / comparisons would raise), and explicit
  bounds that would emit > 1M fillers (Python raises). `numeric::from_int` /
  `from_f64` and `group::gkey`/`GKey` are now exposed. Validated across curated
  cases + a dedicated 4000-case densify fuzz in-repo **and** 8 extra local seeds
  (5000 densify pipelines each, all handled, zero mismatches).
- [ ] **Pipeline: only the storage-backed stages remain.** Every pipeline stage
  that doesn't touch `Storage` is now ported. Still deferring to Python:
  `$lookup`/`$graphLookup`/`$geoNear`/`$out`/`$merge` (read/write collections via
  `ctx.storage`), non-deterministic `$sample`, and date-unit `$densify`. These
  wait for Phase 3+ (storage / wire / dispatch into Rust) per
  tasks/rust-rewrite-plan.md. Also still open: `$sort` defers on bool / NaN sort
  keys (the non-transitive `_SortKey` cases above) — reproducing Python's exact
  Timsort comparison sequence would widen it, but the risk/reward is poor.

### Two latent `sortkey` bugs fixed while porting (now Python == Rust == mongod)

Found by the parity test; both changed the **on-disk index-key bytes** for the
affected values (immaterial for ephemeral test data, but note it):

- **Date keys** used `int(total_seconds() * 1000)`, a float path that rounded
  sub-second values off by up to 1ms vs the integer millis BSON actually stores
  (and mongod sorts by). Now integer-exact.
- **Regex keys** did `bytes(r.flags)`, which — because a BSON-round-tripped
  `Regex.flags` is an *int* — produced N NUL bytes instead of the option string
  (e.g. flags=10 → ten NULs instead of `"im"`). Now reconstructs the option
  chars in pymongo's on-wire order (`ilmsux`).

### 7.2 Command/server tests moved onto real WiredTiger (2026-06-24) — CLOSED

The Rust command/server crates were unit-tested against hand-rolled in-memory
storage doubles, so the command×storage and wire×storage paths were only ever
exercised on real WiredTiger end-to-end via the driver gauges — a fake could
pass while real WT diverged. Both doubles are now scrapped (commits `a11328a`,
`e13d526`):

- **`FakeStorage` (six `secantus-commands` modules) → gone.** All 82
  storage-backed command tests (find / crud / findandmodify / admin / aggregate
  / distinct) were re-homed as real-WT integration tests in the WT-linked
  `secantus-storage-adapter` crate (`tests/command_*_wt.rs` + a shared
  `tests/common/mod.rs` `with_wt()` helper), each driving the real `dispatch`
  path over `WtStorage` via `StorageAdapter`. Fake-specific setup/verification
  was redone in real-WT terms (validators via real `create`; `$out`/`$merge`
  checked by reading the target back; the collMod unique-conversion over real
  duplicate docs instead of injected dup-groups).
- **`MemStorage` (`secantus-server` roundtrip tests) → gone.** The five
  wire/TCP roundtrip tests moved to `secantus-storage-adapter/tests/
  server_roundtrip_wt.rs`, binding the real server over real `WtStorage`
  (`secantus-server` added as a *dev-dependency* of the adapter crate only).
- **Why the adapter crate is the home:** `secantus-commands` and
  `secantus-server` are deliberately WiredTiger-free (clean-workspace members,
  so the fast `rust` CI job + manylinux wheels build with no libclang/WT). They
  *can't* link WT, so the tests had to relocate to a WT-linked, gate-covered
  crate (`rust-adapter-test` runs its `cargo test`). Those two crates keep only
  their pure-unit tests (lib/cursors/util) and no storage doubles; `clippy -D
  warnings` confirms the removal left no dead code.
- **Teardown race found + fixed while porting:** the awaitable-hello server test
  signalled completion after `stop()` but before the server finished dropping,
  letting the temp dir be removed out from under WiredTiger's final
  close-checkpoint (`WT_PANIC: WiredTigerHS.wt No such file`). Fixed by fully
  dropping the server before signalling; other tests rely on
  `RunningServer::Drop` → `stop()` draining connection threads first. 5/5 repeat
  runs clean.
- **Remaining storage doubles (intentional, not scrapped):** `ClockStorage`
  (`secantus-commands` lib test — injects a deterministic `Timestamp(555,9)`
  real WT can't reproduce) and `NoStorage` (`secantus-server` `tls.rs` — a
  hello-only TLS handshake never touches storage). Neither is a storage
  stand-in.

### 7.3 Natural-order (insertion) index ported to Rust storage (2026-06-25) — CLOSED

The Rust storage previously had no insertion-order index — its doc table is keyed
by `id_key = encode_value(_id)`, so an unsorted `find()` walked `_id`-sort order,
not insertion order. This only diverged from mongod / the Python server for
**mixed `_id` types inserted out of `_id` order** (monotonic ids hide it), which
php-lib `BulkWriteFunctionalTest::testInserts` exposed. Ported Python's design to
`crates/secantus-storage`:

- Two WT tables (`secantus_natural` `SSq→u` = `(db,coll,seq)→id_key` and the
  reverse `secantus_natural_seq` `SSu→q`) + a persisted monotonic `next_nat_seq`
  counter (in the oplog-meta blob; recovered on reopen, or by scanning the max
  nat seq when absent — so it survives oplog-disabled / legacy DBs).
- Maintenance on **every** doc mutation: insert / `insert_one` / upsert write a
  nat entry; `delete_by_id` / `delete_matching` / `prune_ttl` / capped eviction /
  `purge_collection_tables` (drop + dropDatabase + rename's dropTarget) remove it.
  rename drops the source's entries; the destination is re-keyed directly (no nat
  entries) so it falls back to `id_key` order (a minor, documented degradation).
- The collscan find paths + the `$natural` hint walk `scan_blobs_natural`
  (seq-ascending → fetch by `id_key`, with a legacy `id_key`-order fallback when a
  collection has no nat entries); the `_id_` hint stays `id_key` order.
- New `SSq` key / `q` value cursor accessors in `secantus-wt`.
- Tests: `crates/secantus-storage/tests/natural_order.rs` (insertion order for
  mixed `_id`s, reopen recovery, delete+reinsert no-doubling, drop+recreate reset,
  `$natural` hint) + an end-to-end adapter test.

Capped-collection eviction now also walks insertion order: `enforce_capped_bounds`
uses `scan_docs_natural` (beta.92), so FIFO holds for non-monotonic custom `_id`s.
This matches mongod. The Python server evicts in insertion order too
(`_enforce_capped_bounds_locked` walks `_scan_docs_natural`), so both servers are
FIFO-correct for non-monotonic custom `_id`s.

### 7.4 Verified rust-only gauge tail (2026-06-25)

Authoritative `invoke validate-all-servers --jobs 4` run (every gauge on **both**
Python and Rust, same day, JDK 17 for java) — so each "rust-only" item below is a
failure the Rust server has that the **Python server does not**, not a stale-baseline
artifact. **Clean (0 rust-only): c, cxx, dotnet, kotlin, node, mongo-rust-driver.**
All four actionable themes are now fixed and merged: the geo `$center` / `$near` /
`$nearSphere` query operators (#66), php-ext write-reply shapes + cursor metrics +
Code `_id` (#67), ruby index-option validation/echo (#69), and
`splitLargeChangeStreamEvents` (#64). What remains is **only** the out-of-scope
session tests + the go harness race below.

- **Out of scope (session plumbing, do not chase):** ruby "behaves like a failed
  operation using a session raises an error" (×3, `Collection#create` / `#indexes` /
  `Index::View#create_one`) and php-lib `WatchFunctionalTest::testSessionFreed` (×1,
  `resumeCallable` unset on invalidate via reflection). Same class as the deferred
  change-stream session items — driver-internal session lifecycle, not a wire divergence.
- **go `TestChangeStream_ReplicaSet/try_next/one getMore sent` (3 entries, 1 test) — NOT a server bug; §5 harness race (verdict 2026-06-26).**
  The Rust change-stream behaviour was verified **correct** by two direct probes, so the
  earlier "confirmed real Rust bug / scope-leak" escalation was wrong and is retracted:
  (1) a pymongo repro against the standalone `secantusdb` binary (`/tmp/cs_repro.py`) —
  collection-scoped watch on an empty collection returns `try_next()==None` (start
  position correct), writes to a sibling collection / other DB do **not** surface (no
  scope leak), and an own-collection write **does** surface (control); (2) the exact go
  test run **in isolation** (`go test -run TestChangeStream_ReplicaSet/try_next
  -count=10` against the rust binary) passes **10/10**. The full-suite failure is the
  documented §5 artifact: other `t.Parallel()` top-level tests write the shared `TestDB`
  namespace (with collection-name-truncation collisions) during the `try_next` await
  window, and that genuinely-same-namespace write *correctly* wakes the stream — the test
  only assumes an empty stream because the shared-daemon gauge doesn't give it the
  per-test namespace isolation real `mongod`s would. Rust appears more susceptible than
  Python in the full suite (getMore/await response timing makes it catch the concurrent
  write more often — Python passed the same-day run, Rust failed), but that's scheduling
  sensitivity to a harness race, not a wire/correctness divergence. **Runner-side fixes do
  NOT work (2026-06-26):** two attempts — isolating change-stream tests from the rest of
  the suite, then running each change-stream top-level function in its own serial `go test`
  process — both still flaked (1/3 and 1/2). And the server is provably correct under
  on-disk load (collection-scoped stream on an untouched collection: 0 events / 400 polls
  while other collections took 2074 concurrent writes; 0 events / 300 fresh stream-opens
  under create/drop/insert churn). So the flake is a timing property of the shared-daemon
  mtest harness under full-gauge load, not suppressible at the runner and not a server bug.
  **Accepted**, same as the Python-server verdict in §5. See the top-section entry for the
  full 2026-06-26 evidence.
  - [ ] *Minor, separate — the CAUSE has changed; re-probed 2026-08-20.* The go gauge
    still runs the Rust server without periodic noop heartbeats, but no longer because
    the binary lacks the flag: `secantusd-rs --help` lists `--noop-heartbeat-seconds S`,
    `crates/secantus-server/src/args.rs:268` parses it, and the daemon starts cleanly
    with it. What still drops it is `gauge_common._PYTHON_ONLY_FLAGS` (line 45), which
    strips the flag for the Rust server. **The fix is now deleting one line from that
    set**, not adding a flag to the binary — and then confirming the go gauge still
    passes with heartbeats on.

### 7.5 Remaining Rust-server feature gaps (defer audit, 2026-06-26)

From the `Fallback`-site audit (#2): constructs the Python server supports but the
Rust core defers (so they error on the Rust server). None are *correctness* bugs —
the actionable gauge tail is closed; these are feature completeness. **Shipped since
the audit:** `$exp`/`$ln`/`$log`/`$pow`/`$round`/`$trunc` (#74), `$sortArray` (#76),
`$unionWith` (#77), `$fill` (#123), `$toDecimal` + `$convert` (#132),
`$setWindowFields` (rank funcs + the **full time-series operator set** `$shift` /
`$expMovingAvg` / `$locf` / `$linearFill` / `$derivative` / `$integral` + `$group`
accumulators over document-based **and value-based `range`** windows — both
servers, single ascending numeric sortBy; only date-`unit` x-axes defer), the
regex family `$regexMatch` /
`$regexFind` / `$regexFindAll` (shared `regexutil`; all three served by both the
linear and backtracking `fancy-regex` engines — lookaround / backreference finds
compute, matching Python `re`),
`$jsonSchema` (`bsonType` / `type` / `enum` / numeric bounds / string length +
`pattern` / array + object counts + `items` / `required` / `properties` /
`additionalProperties` / `patternProperties` / `dependencies` + the `allOf` /
`anyOf` / `oneOf` / `not` combinators — both servers; only exotic keywords still
ignored, unreproducible shapes defer), `$dateFromString`
(canonical ISO — `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`, optional trailing `Z` or
fixed `±HH:MM` offset → UTC instant — plus `null`→`onNull`; the parity harness now
compares the *bson-normalised* stored form so tz-aware results compare cleanly.
`format` / separate `timezone` field / fractional seconds / non-canonical strings
defer), and `$dateToString` (strftime-style formatting: `%Y`/`%m`/`%d`/`%H`/`%M`/
`%S`/`%L`/`%j`/`%w`/`%u`/`%%` + literals; a `timezone`, any other directive
(`%z`/`%Z`/`%G`/`%V`/`%U`/locale names), or a non-4-digit year defer to Python
`strftime`). **With `$dateToString`, the pure expression-operator surface is
complete on both servers** (only date *formatting/parsing* edges below remain).
**Remaining:**

- [ ] **Expression operators (Rust server):** the remaining date-op defers to the
  Python oracle (the Rust server errors on them): *named IANA* `timezone` zones on
  `$dateFromString` (naive-local→instant is DST-ambiguous across a gap/overlap, so
  it stays deferred — unlike `$dateToString`'s unambiguous instant→wall-clock);
  `$dateFromString` `format` directives *outside* the numeric subset
  (`%z`/`%Z`/`%a`/`%b`/`%p`/… — need locale/text/offset handling), a `%j` combined
  with `%m`/`%d`, and any input Python would reject; `$dateToString` `%z`/`%Z`/
  ISO-week/locale directives; and the `timezone` form of **`$dateTrunc` /
  `$dateDiff`** — a gap in *both* servers, but **deliberately deferred** (not a
  clean instant→local like the extractors / `$dateToParts`): `$dateTrunc` truncates
  to a *local* boundary that must convert back to a UTC instant (local→instant,
  DST-ambiguous across a gap/overlap — same class as `$dateFromString`), and
  `$dateDiff`'s `day`/`week` already count elapsed 24h/7d periods rather than
  local-calendar boundaries (a pre-existing mongod divergence, independent of
  timezone), so honouring `timezone` there would first require fixing that calendar
  semantics. **Now native on the Rust server:** fixed-offset /
  `UTC` / `GMT` timezones on both ops (0.5.3-beta.116); `$dateFromString`
  `format` (strptime) for the numeric-directive subset `%Y`/`%y`/`%m`/`%d`/`%H`/
  `%M`/`%S`/`%j`/`%%` + literals + whitespace (0.5.3-beta.117, regex built from
  CPython `_strptime` fragments); **named IANA `timezone` zones on
  `$dateToString`** (0.5.3-beta.131, via `chrono-tz` — DST-correct instant→
  wall-clock, matching Python `zoneinfo`; parity corpus curated to post-2007 dates
  in decade-stable major zones to avoid tzdb release skew); and the
  **`{date, timezone}` object form of the seven date component extractors**
  (`$year`/`$month`/`$dayOfMonth`/`$dayOfWeek`/`$hour`/`$minute`/`$second`) —
  fixed-offset + named IANA zones, on **both** servers (0.5.3-beta.133 / 0.5.4b161;
  previously both ignored `timezone` here and returned null for the object form);
  and **`$dateToParts` `timezone`** (fixed-offset + named IANA, both servers,
  0.5.3-beta.134 / 0.5.4b162 — instant→wall-clock via the shared
  `timezone_offset_ms` helper; previously both ignored it). Fractional seconds stay
  deferred (BSON is millisecond-only). The Python server already supports the
  remaining `$dateFromString`/`$dateToString` directive edges.
- [x] **Log-family domain errors — FIXED (2026-07-17, probed against mongod
  7.0.12).** Out-of-domain args now raise mongod's Location codes on the Python
  engine (`$ln` ≤ 0 → 28766, `$log10` ≤ 0 → 28761, `$log` argument → 28758 /
  base → 28759, `$sqrt` < 0 → 28714, verbatim messages incl. ", but is X");
  the Rust engine defers those cases so both servers surface the same errors
  (the arithmetic-error precedent). NaN now propagates as nan (IEEE, matching
  mongod) instead of null; null/missing still yield null. Parity-corpus
  comments updated; pinned by `test_expressions.py::
  test_log_family_domain_errors`.
- [ ] **`$group` accumulator gaps — `$median`/`$percentile` SHIPPED on both
  servers (2026-07-17).** Both the group-accumulator and expression forms now
  run on Python and Rust, pinned by a live mongod **7.0.12** probe: the
  "approximate" method on bounded data is mongod's discrete percentile
  (`sorted[max(0, ceil(p*n) - 1)]`, doubles out; bool/NaN excluded, Decimal128
  included; empty → null / per-p nulls), so no t-digest is needed and the two
  engines agree exactly (curated parity cases). Spec validation carries
  mongod's verbatim codes (40414 missing `method`/`input`/`p`; 2 non-approximate
  method; 7750301 non-array `p`; 7750303 out-of-range `p`). Still absent from
  both: the hashing family (`$toHashedIndexKey` — mongod-specific hash), and
  **`$bitAnd`/`$bitOr`/`$bitXor` as `$group` accumulators** (their
  *expression* forms shipped — see below). NOT validatable locally: a
  2026-07-17 probe against the mongod 7.0.12 tarball
  (`/usr/local/mongodb-macos-aarch64-7.0.12/bin/mongod`) rejects
  `{$group: {a: {$bitAnd: ...}}}` with `15952 unknown group operator` — the
  accumulator form needs a newer mongod (docs say 6.3, reality says newer);
  implement only once a mongod that accepts it is available to probe.
  **Sort-layer edge (pre-existing, not $topN-specific):** SecantusDB's sort treats a
  *missing* field as equal to explicit `null`, whereas mongod sorts missing just
  *above* null — so `$top`/`$bottom`/`$topN`/`$bottomN` (and `$sort`) can order docs
  with equal sort keys differently from mongod only when explicit-null and
  missing-field docs tie. Python and Rust agree with each other. **Now native on
  both servers:** the sort-key accumulators **`$top` / `$bottom` / `$topN` /
  `$bottomN`** (0.5.3-beta.140 / 0.5.4b178 — three-way verified: multi-key `sortBy`,
  array `output`, integral-double `n`, mongod validation codes); the
  **`$dateFromParts`** expression (0.5.3-beta.141 / 0.5.4b201 — three-way verified:
  calendar rollover, defaults, null-propagation, fixed-offset tz, mongod codes
  40515/40516/40523; Python also resolves named zones via `zoneinfo`, Rust defers
  them; **ISO-week form** `isoWeekYear`/`isoWeek`/`isoDayOfWeek` added 0.5.3-beta.142
  / 0.5.4b202 via `chrono`'s ISO calendar); the **`$tsSecond` / `$tsIncrement`,
  `$type`, `$isNumber`, `$isArray`, `$strcasecmp`, `$replaceOne` / `$replaceAll`**
  expression operators (0.5.3-beta.142 / 0.5.4b202 — three-way verified: values +
  mongod error codes 5687301/2, 51745; `$type` reports `"missing"` for an absent
  field; `$strcasecmp`/`$replace*` defer non-ASCII case to the Python oracle); the
  **set-expression family** `$setUnion` / `$setIntersection` / `$setDifference` /
  `$setEquals` / `$setIsSubset` / `$allElementsTrue` / `$anyElementTrue` plus the
  utilities `$cmp`, `$binarySize`, `$bsonSize`, `$degreesToRadians` /
  `$radiansToDegrees` (0.5.3-beta.143 / 0.5.4b206 — three-way verified, zero value
  divergences: union/intersection return BSON-sorted, difference preserves
  first-array order, all set ops dedup by BSON-order equality; a non-array arg or a
  cross-type-unsortable element defers to the Python oracle on the Rust side); the
  **trigonometric family** `$sin` / `$cos` / `$tan` / `$asin` / `$acos` / `$atan` /
  `$atan2` / `$sinh` / `$cosh` / `$tanh` / `$asinh` / `$acosh` / `$atanh`
  (0.5.3-beta.144 / 0.5.4b207 — three-way verified, zero value divergences: both
  servers compute through the platform libm so Rust `f64` and CPython `math` agree
  bit-for-bit; domain violations raise mongod's `Location50989` — `$asin`/`$acos`/
  `$atanh` need [-1,1], `$acosh` needs [1,∞), `$sin`/`$cos`/`$tan` reject ±inf/NaN;
  `$atanh(±1)` → ±inf; non-numeric raises `Location28765`, `$atan2` `Location51044`;
  Decimal128 is float-cast on Python and defers on Rust); the **array-update
  operators** `$pull` (query semantics — element-value predicate / sub-document
  match / BSON-aware equality via `query::matches`, replacing the old literal-`==`
  path that silently ignored predicates and wrongly conflated `1`/`true`),
  `$pullAll` (literal-equality removal, previously unimplemented on *both* servers —
  rejected as an unknown modifier), and the Rust-server `$push` `$sort` modifier
  (`1`/`-1` whole-element or `{field: dir}`, BSON order via `order::cmp`;
  0.5.3-beta.145 / 0.5.4b208 — found by a three-way update differential vs mongod
  6.0, all four fixes verified with zero divergences); the **query match
  operators** `$in` / `$nin` with a **regex candidate** (matches string values by
  pattern, not by literal equality — the old path silently matched nothing / errored
  on Rust) and `$all` with **`$elemMatch` clauses** (each clause requires some array
  element to satisfy its sub-query; 0.5.3-beta.146 / 0.5.4b209 — found by a
  three-way query differential vs mongod 6.0, both fixes verified Rust==Python); the
  **`$push` / `$addToSet` skip-missing** accumulator fix (a missing accumulator
  field is not added — an explicit null still is — matching mongod; via the
  missing-aware `evaluate_or_missing` / `eval_or_missing`; 0.5.3-beta.147 / 0.5.4b215
  — found by a three-way aggregate differential vs mongod 6.0, verified
  Rust==Python==mongod); the
  bitwise **expression** operators `$bitAnd` / `$bitOr` / `$bitXor` / `$bitNot`
  (0.5.3-beta.136 / 0.5.4b164 — int/long operands, int32/int64 result width,
  empty-list identity, null propagation; a non-integer operand raises), and the
  **N-element array expressions** `$firstN` / `$lastN` / `$maxN` / `$minN`
  (0.5.3-beta.13{7,8} / 0.5.4b16{5,6}; the `{n,input}` validation is matched to
  **real mongod 6.0** via a three-way probe — integral-double `n` accepted, and a
  null/missing/non-array `input` **raises** `Location5788200`, not null; `$maxN`/
  `$minN` sort via the `$sortArray` `order::cmp`/`is_sortable` contract, deferring
  bool/Decimal128 elements to Python's `_SortKey`), **plus their `$group` /
  `$setWindowFields` accumulator forms** (0.5.3-beta.139 / 0.5.4b173 — three-way
  verified: `$firstN`/`$lastN` keep null values, `$maxN`/`$minN` drop them; shared
  `nelem_parse_n` validation). **Error-code gap (both these
  operator families and, generally, any operator whose error path defers):** the
  Python server reproduces mongod's exact Location codes, but the **Rust server**
  raises a generic `BadValue` (2) on these error paths because the Rust core signals
  `Fallback` rather than a coded error — same class as the unrecognized-operator
  nit. A faithful fix needs the `Fallback` type to carry an optional mongod code (or
  per-operator error emission in the command layer). **Now native on the Rust
  server:** `$stdDevPop` /
  `$stdDevSamp` accumulators (0.5.3-beta.135 / 0.5.4b163 — Python already had them;
  both engines aligned to a naive-fold + multiply + `sqrt` computation so they agree
  bit-for-bit despite CPython 3.12's compensated `sum()`).
- [ ] **Cross-type range comparison — FIXED 2026-07-13 on both servers; only a
  DBPointer operand nested in an array/document still defers (`$gt`/`$gte`/`$lt`/
  `$lte`). All four sub-items below are done (verified 2026-07-19).** mongod's range operators are
  **type-bracketed** (verified with a three-way probe against real `mongod` 6.0):
  a scalar bound only matches values in the same BSON type bracket — `{a: {$gt: 2}}`
  does **not** match a document- or string-valued `a`, only numbers (plus array
  elements that are numbers). So the earlier premise here (that mongod uses full
  cross-type BSON order for range, à la sort) was wrong; the Python matcher's
  no-match on cross-type scalars was already correct. Two real divergences were
  fixed:
  - **Document/array operand → Rust server error.** `compare_values` returned
    `Fallback` for a document operand, so the Rust *server* raised `BadValue` on an
    otherwise-fine query (`{a: {$gt: 2}}` against a document-valued `a`, or
    `{items: {$elemMatch: {$gt: n}}}` over an array of sub-documents — found by the
    three-way differential 2026-07-10). Now a **document** operand returns
    `Ok(None)` (clean no-match, mirroring Python + mongod).
  - **Bool matched numeric bounds on both engines.** Python's `bool` is an `int`
    subclass, so `True < 2` matched; the Rust `compare_values` bool block compared
    bool numerically against int/long/double. mongod brackets bool separately —
    a bool field never matches a numeric bound and vice versa, but `bool`-vs-`bool`
    compares. Fixed in **both** engines (Python `_try_cmp` bool-bracket guard; Rust
    `compare_values` bool compares only with bool). Parity fuzz + a three-way probe
    (collection-scan **and** index-scan paths) confirm all three agree.
  - **Array-vs-array bound — FIXED 2026-07-13 (Rust server).** An **array-vs-array**
    bound (`{a: {$gt: [1,2]}}`) is now compared **whole-array lexicographically** in
    the Rust matcher (`compare_values` recurses element-wise, mirroring Python's
    native `list < list` and mongod — verified against real mongod 6.0), and
    `cmp_op` also does the whole-value compare so an array field vs an array bound
    matches. A cross-type element pair (where Python's `<` would raise) returns a
    clean no-match, not a `Fallback`. Pinned by curated parity cases +
    `array_vs_array_lexicographic_range` / `array_vs_array_cross_type_element_no_match`
    unit tests.
  - **Exotic-type range operands — FIXED (Rust matcher).** JS code / symbol
    compare as text (mirroring pymongo's decode: Symbol → `str`, `Code` is a
    str subclass, with-scope Code compares by its code string); a DBPointer or
    undefined operand is a clean no-match. Under a collation the exotic-text
    combination still defers to Python. Pinned by curated query-parity cases.
  (The **`$min`/`$max` UPDATE** operators are now on the full `_bson_lt` port —
  `order::bson_lt`, a single strict-less that needs none of `$sort`'s
  transitivity guarantees — so bool / Decimal128 / NaN / Binary / Timestamp /
  Regex / Min-MaxKey and the decoded exotic text types all compute on the Rust
  engine; only a **DBPointer** operand still defers, because Python resolves it
  with a type-*name* tiebreak not worth reproducing. Pinned by curated
  update-parity cases.)
- [ ] **Aggregate gaps found by the three-way differential (2026-07-10, both
  servers).**
  **`$stdDevPop` last-ULP vs mongod** — both servers agree with each other but
  differ from mongod in the final ULP (e.g. `2.357022603955158` vs mongod's
  `2.3570226039551585`); mongod uses a different summation order. Precision-only,
  hard to match exactly — likely a permanent minor divergence.
- [ ] **`$meta` projection values** (`{score: {$meta: "recordId"}}` / `"indexKey"` /
  `"sortKey"`) — the recognized-but-unsupported `$meta` args now validate clean on
  both servers and the field is **omitted** (partial, graceful degradation) rather
  than faithfully computed — mongod returns the actual index / record-id / sort-key
  metadata, which SecantusDB doesn't model. Low priority (index-metadata surface
  SecantusDB doesn't otherwise expose). The two `$meta` *error* cases are now
  faithful on both servers: `{$meta: "textScore"}` without a `$text` query →
  `Location40218`, an unknown `$meta` arg → `Location17308`. Found alongside the
  positional-`$` projection fix by the three-way projection differential
  (2026-07-12).
- [x] **Query operator: `$jsonSchema` keyword surface — COMPLETE on both
  servers (2026-07-17, probed against real mongod 7.0).** Every mongod-accepted
  keyword now ships on both engines, including the previously-missing
  `multipleOf` (fmod semantics), tuple-form `items` + `additionalItems`, and
  the `title`/`description` metadata (accepted-and-ignored, type-checked).
  Exclusive bounds moved to mongod's **draft-4** semantics (`exclusiveMinimum`/
  `exclusiveMaximum` are booleans sharpening `minimum`/`maximum`; the draft-6
  numeric form is a parse error — the old numeric treatment was a divergence).
  Keyword *validation* is parse-time and recursive on both servers with
  mongod's verbatim codes/messages: unknown keyword / known-but-unsupported
  (`$ref`/`$schema`/`default`/`definitions`/`format`/`id`) → `9 FailedToParse`;
  type violations (`multipleOf` non-number, exclusive-bound non-boolean,
  non-string metadata, non-object schema) → `14 TypeMismatch`. Python:
  `query._check_json_schema_keywords` (QueryError now carries code/codeName);
  Rust: `secantus_core::query::json_schema_keyword_error` + the find-command
  parse-time check. (`type: "integer"` acceptance remains a small known
  divergence — mongod rejects the alias; both our servers accept it.)
- [x] **Error-code — unrecognized expression operator: FIXED on both servers.**
  Query `$expr` → `168 InvalidPipelineOperator` on both (find.rs parse-time
  check via `expressions::first_unknown_expr_operator`); aggregation
  `$project` → `Location31325` on both — the Rust server now runs a parse-time
  `validate_project_exprs` scan that skips single-key projection-only
  operators (`$slice` / `$elemMatch` / `$meta`) so they are never mislabeled,
  and flags only a truly-unknown `$`-operator (a recognised-but-deferred one
  still defers to Python). Pinned by
  `test_rust_server_smoke.py::test_unknown_expression_operator_error_codes`.

### 7.6 Standalone `secantusdb` binary: CLI-flag conformance shipped (beta.96)

The Rust `secantusdb` binary now mirrors the Python daemon's full flag surface
(`--config`, `--log-level`, `--cache-size`, `--session-max`, `--sync-on-commit`,
`--noop-heartbeat-seconds`, `--oplog-retention-seconds`, `--oplog-max-entries`),
including the `secantusdb.toml` loader (`crates/secantus-server/src/config.rs`,
a faithful port of `src/secantus/config.py`: strict unknown-table / unknown-key
rejection, the vestigial `[oplog] archive_dir` rejection, defaults < TOML < CLI
precedence, and the `./`, `~/.secantus/`, `/etc/secantus/` auto-discovery path).
Storage knobs flow through `secantus_storage::wt_config(cache_size, session_max,
sync_on_commit)` into `Storage::open_with_config`; oplog knobs via the existing
setters; `--noop-heartbeat-seconds` and `[storage] ttl_sweep_seconds` drive
background maintenance threads that observe a shutdown flag and are joined before
teardown. (The embedded `RustServer` handle now exposes the same knobs —
`cache_size` / `session_max` / `sync_on_commit` constructor parameters
threading into `wt_config`.)

### 7.7 Rust storage: no close-time checkpoint / no `durable` mode

- [x] **Rust `Storage` close-time checkpoint / `durable` mode — DONE
  (2026-07-23).** `impl Drop for Storage` now forces a WiredTiger checkpoint on
  clean shutdown when `durable` is set (after persisting the oplog-meta row, on
  the same session), mirroring Python `Storage.close`. The flag is resolved on
  open from the environment with Python's exact precedence (`resolve_durable` /
  `resolve_durable_from_env`): `SECANTUS_FORCE_DURABLE=1` > explicit override >
  `!SECANTUS_TEST_FAST_STORAGE`. So the shipped `secantusd-rs` daemon (no env)
  is durable — its graceful SIGINT/SIGTERM path drains connection threads via
  `RunningServer::stop` then drops `Storage`, so the checkpoint runs
  single-threaded with the data dir intact (no repeat of the beta.48
  checkpoint-vs-teardown race) — while the Python test suite (which sets
  `SECANTUS_TEST_FAST_STORAGE=1`) gets `durable=false` and skips the checkpoint,
  so the embedded `RustServer` gauge/tests are unchanged. Fast mode is still
  fully recoverable (journal stays on), just not checkpoint-bounded. New
  `open_with_config_durable(home, config, Option<bool>)` mirrors Python's
  `durable=` argument. Guards: `resolve_durable_precedence`,
  `durable_close_roundtrips_data_on_reopen`, `fast_close_still_recovers_via_log_replay`.
  The remaining WT-config divergences — `file_max` 10MB vs 128MB (#575), Rust-only
  `eviction` threads (#575), Rust-only `block_compressor=zlib` (#578) — are all
  intentional perf tunings, not risks (zlib in fact makes the Rust engine use
  *less* doc-data disk than Python).

### 7.8 RecordId keying (step 1): pre-PR sharded stores are refused at open (no migration)

- [x] **A Rust store written by a build BEFORE the RecordId-keying change has its
  doc shard tables keyed on the OLD `SSu` (`(db, coll, id_key)`) format with
  UNFRAMED blob values; this build keys them by a monotonic RecordId (`SSq`) with
  a framed value (`[u32 id_key_len][id_key][blob]`). RESOLVED by fail-fast, not
  migration (2026-07-23):** WiredTiger fixes a table's `key_format` at CREATE time
  and preserves it across reopen (the bootstrap `create` is a no-op for an
  existing table), so `Storage::open` now reads each `secantus_documents_sh{s}`
  table's on-disk `key_format` from the WT `metadata:` cursor
  (`reject_pre_recordid_doc_format` + `extract_key_format`, using the new
  `secantus-wt` `Cursor::get_value_s`) and returns a fatal `StorageError::Internal`
  if any shard is `SSu` — the server refuses to start rather than mis-read data
  with `SSq` cursor ops against an `SSu` btree. **Deliberately no in-place
  migration:** the two Rust servers are pre-1.0 beta with no upgrading users, so
  refusing an incompatible on-disk store is the correct, safe behaviour (per the
  "never mis-read stored data" rule). `migrate_legacy_docs` still handles the
  separate, supported pre-*shard* case (the legacy single `secantus_documents`
  table). If RecordId ever ships to users who must upgrade in place, THEN add a
  shard-generation migration (read each old `SSu` shard, mint RecordIds, write
  framed `SSq` rows into a fresh shard-table generation + the `_id` index, swap).
  Test: `open_rejects_pre_recordid_doc_shard` (fabricates an `SSu` shard, asserts
  open is refused).

## SQL / PostgreSQL interface — P0 spike limitations

### pgx gauge — `pgconn` findings and next steps (2026-08-14)

**Where it stands: `pgconn` is at 0 stable failures — MEASURED, package
CLOSED** (full run at `03d5c63b`, 2026-08-15, after the triggers +
tsvector build #901: the only failure was the documented load-sensitive
`TestDeadlineContextWatcherHandler`; the official full-gauge run reads
**376 P / 2 F / 22 S = 99.5%**, from 77.0% at the 2026-08-14 baseline —
`pgconn` 99.0%, `pgproto3`/`bgreader`/`ctxwatch` 100%) (of 216 tests;
full-package re-run measured 2026-08-15 at `f2891c28`, three runs). The
chain: 86 → 29 (#862) → 23 (#866) → 20 (#868) → 18 (#869) → 17 (#870) →
12 (#871, measured) → 8 (#873) → 5 (#876) → 4 (#877). Two additional tests are
LOAD-SENSITIVE, not broken — the ctxwatch timing-budget pair,
`TestDeadlineContextWatcherHandler` and (less often)
`TestCancelRequestContextWatcherHandler`: both give short pg_sleep queries
sub-second cancel/deadline budgets, pass repeatedly in isolation against
the same daemon (3/3 and 5/5 across measurement rounds, including at
moderate load), and miss their margins only under the full package's
16-way parallel load against the GIL-bound Python server on this shared
desktop (2026-08-15: flickered in 2 of 5 full runs; one earlier round was
also polluted by a leaked debug process, since killed). The Python server
is not a perf target (throughput/latency work goes to the Rust server) —
re-measure on a quiet machine before treating either as a server defect. The stable four: 86 → 29 after the `generate_series`
untyped-bound fix (#862), 29 → 23 after per-session temp-table namespacing
(#866), 23 → 20 after the stray-CopyData drain fix (#868), 20 → 18 after
plain-json compact rendering (#869), 18 → 17 after the legacy bare
`COPY … BINARY` keyword fix (#870), 17 → 13 after wire CancelRequest support
(#871), and the full re-run measured 12 — `TestConnExecBatchImplicitTransaction`
also passes now (pipeline implicit-transaction work, #856 line). The other
three pgx packages were already clean (`bgreader` 6/6, `ctxwatch` 6/6,
`pgproto3` 171/172), so `pgconn` is the whole gauge gap.

**What the 86 → 29 fix was, and why it was worth so much.** pgx's
`ensureConnValid` helper (`pgconn/helper_test.go:28`) runs
`select generate_series(1,$1)` with `$1` sent as untyped text, and is called
at the END of 66 `pgconn` tests to prove the connection still works. We
rejected that query, so every one of those tests failed at its final step
regardless of what it was testing — one missing coercion presenting as 66
unrelated bugs, including things like `TestConnEscapeString`. **The lesson is
the reusable part: in this gauge, a shared test helper can make one gap look
like a whole subsystem. Read the actual failure text before believing the
test names.**

**The four stable failures** (fixed clusters deleted per the
delete-when-fixed rule — the narratives live in `docs/changelog.md` and the
PR trail #866/#868/#869/#870/#871/#873/#876/#877):

* (none — the trigger/tsvector build landed BEFORE INSERT FOR EACH ROW
  triggers with plpgsql NEW records. Scope limits, deliberate: AFTER /
  UPDATE / DELETE / statement-level triggers stay rejected 0A000; a
  `pg_temp.`-homed trigger FUNCTION lingers in the catalog after its
  session ends (its trigger and temp table are torn down; the orphaned
  function entry is invisible to other sessions' bare-name lookups).

Known divergence recorded while fixing the batch shape (#876): a real PG
multi-statement simple query is ONE implicit transaction (an error rolls
back earlier statements' writes); ours runs per-statement autocommit, so
earlier writes survive. No gauge test observes it yet.

#### Temp tables: concurrent namespacing FIXED and CONFIRMED as the cluster's cause

The confirmed gap — two simultaneously-open connections each doing
`create temporary table bar(a int4)` had the second fail `42P07` because we
shared one temp namespace — is **fixed** (#866): every session now gets its
own lazily-allocated `pg_temp_<n>` namespace (catalog keys carry the prefix),
unqualified names resolve temp-first ahead of `public` (so a session's temp
table shadows a permanent one, like real PG), explicit `pg_temp.<name>`
resolves to the session's own namespace, and COPY / extended-protocol
Describe resolve through the same path. `tests/test_sql_temp_namespacing.py`
pins all of it, including the two-concurrent-psycopg-connections repro.

**Causality is now measured, not assumed.** The 2026-08-14 re-run at
`7cab7a3a` has **zero** `already exists` occurrences in the whole package log
(the 42P07 signature is gone), and the COPY cluster dropped 13 → 8 —
`TestConnCopyFrom`, `TestConnCopyFromConnectionTerminated` (the
abnormal-disconnect suspect), `TestConnCopyFromCanceled`,
`TestConnCopyFromPrecanceled`, `TestConnCopyFromGzipReader`,
`TestConnCopyToPrecanceled`, and `TestConnCopyToQueryError` all pass, as do
the baseline's `TestConnExecBatchDeferredError` and `TestConnDeallocate*`.
The earlier alternative hypothesis (abnormal-disconnect leak) is dead: the
shared namespace was the cause. The mechanism the original diagnosis missed
is `t.Parallel()` — 116 calls in `pgconn_test.go` — so tests' connections
are simultaneously open, and same-named temp tables collided across live
sessions even though every test closed its own connection cleanly.

Residual namespacing caveats (deliberate): stale `pg_temp_<n>.*` catalog
entries from a crashed process are not purged at startup (the namespace
counter is pid-seeded, so re-mint collisions are vanishingly unlikely, but
the dead entries linger invisibly until a same-named session drops them);
RBAC grants and `pg_namespace` don't know per-session namespaces
(`pg_namespace` still lists a single `pg_temp_1` row).

#### Reproductions

```
# gauge, one test
python -m secantus.sql.pgserver --host 127.0.0.1 --port 15435 --storage-path $(mktemp -d)
cd vendor/pgx && PGX_TEST_DATABASE="host=127.0.0.1 port=15435 dbname=postgres user=postgres" \
  go test -count=1 -run 'TestConnCopyFrom$' ./pgconn/

# whole package (~130s)
... go test -count=1 -timeout=600s ./pgconn/
```


### SQL-gauge re-baseline sweep (2026-08-15, at `36937245`)

Run after the pgx campaign to harvest gains and regression-check the
shared-surface changes (parse errors, startup GUCs, reply shapes,
`generate_series` int4, notifications, triggers):

- **sqlalchemy**: 978 P / 0 F / 447 S — clean, no regression.
- **sqllogictest**: 52/60 files, 8 expected divergences, 0 unexpected —
  identical to the committed report.
- **pgtest**: 10/58 files — identical to the committed report (an older
  backlog entry said 8/58; that was stale). Campaign started 2026-08-16:
  the `aborted_txn` file's two protocol gaps (25P02 for extended steps in
  aborted transactions with the COMMIT/ROLLBACK carve-out;
  `ignore_till_sync` covering interleaved simple Query) took the corpus
  43 → 40 failing files; slice 2 fixed the XX000-leak class (malformed
  binary array params → 08P01, malformed array-literal casts → 22P02 —
  files advance internally, count still 40). Known next divergences:
  slices 3–4 greened `array`, `json`, `json_array` (nested-constructor
  typing; JSON[] 199/114 identities; VERBATIM plain-json echo narrowing
  that divergence to table-stored columns; jsonb binary validation;
  42804/08P01 element-oid split). Slice 5 greened `batch_stmt` and
  CLOSED the recorded batch-transaction divergence: a multi-statement
  simple query is now ONE implicit transaction (mid-batch errors roll
  back earlier writes; BEGIN takes over with its characteristics —
  READ ONLY gates + poisons; COMMIT splits), with a top-level-semicolon
  segment-parse fallback for batches sqlglot rejects whole. Slice 6
  greened `bind_and_resolve`: reg-pseudotype parameter oids
  (`$1::REGCLASS` → 2205 in ParameterDescription) and bind-time portal
  snapshots — a portal bound inside an explicit txn runs eagerly at Bind
  (read-only SELECTs only; errors stay at Execute after BindComplete;
  cached-plan revalidation still trips 0A000 at Execute), so later
  same-txn DDL can't change what the held portal returns. Slice 7
  greened `char` (one documented divergence: char:250 pins crdb's
  TableOID=105 with no ignore_table_oids — impossible for any non-crdb
  server; recorded in EXPECTED_DIVERGENCES): the QUOTED `"char"`
  spelling now maps to PG's internal one-byte type — oid 18, typlen 1,
  column name `char` — via a token-context pre-parse rewrite to the
  `pg_char_1` sentinel (sqlglot collapses the quoted spelling into
  plain CHAR in both cast and column-def positions; the rewrite fires
  after `::`, after AS only inside CAST(...), and in CREATE/ALTER
  column-type position, never on aliases/column names/string
  literals). Input truncates to ONE character; empty/zero-byte input
  stores NULL; `0::"char"` is the zero byte (binary render 0x00);
  binary param/result codecs added. Slice 8 greened `citext`: citext
  now rides crdb's stable placeholder oid 90008 (like hstore's 16935 —
  the extension has no fixed catalog oid; the old citext-as-25 entry
  was omitted only because 25 collided with text in OID_TO_TAG),
  binary param/result codecs are the text bytes, and an unknown param
  compared against a citext COLUMN infers 90008 (citext-only on
  purpose — its own operator family; the char corpus pins a "char"
  comparison param at 25). Slice 9 greened `copy` (1187 lines, the
  biggest file): CSV parsing rewritten as PG's CopyReadAttributesCSV
  state machine (quoting decides NULL-ness — quoted "N" with NULL 'N'
  is the string N; quoted empty is ''; custom ESCAPE chars; a \\.
  line ends CSV data; unterminated quote → 22P04); COPY TO CSV
  force-quotes empty strings; _copy_options re-scans sqlglot's
  mangled legacy option syntax as a token stream (CSV NULL 'NS'
  DELIMITER '|'); ESCAPE/HEADER are CSV-only (0A000); text-mode
  \\xHH/\\OOO byte escapes decode; COPY (query) TO STDOUT works via
  the extended protocol (NoData + CopyOutResponse/CopyData/CopyDone
  in Execute; COPY takes 0 params — binding any is 08P01 with the
  statement-summary detail; an unbound $1 at Execute is 42P02);
  crdb's inline INDEX(...) table element is accepted-and-skipped and
  ADD COLUMN ... NOT VISIBLE parses as a normal column. Slice 10
  greened `copy_file_upload`: unknown COPY option keywords (crdb's
  ``WITH destination = 'nodelocal://…'``) now raise PG's 42601 at
  parse, before the target table resolves (was 42P01), via a
  known-keyword allowlist in the option scanner; the legit ``QUOTE``
  option is wired for real while at it (custom CSV quote char in
  parse + render, CSV-only 0A000 gate, 22023 for multi-char).
  Slice 11 greened `decimal`: the binary-numeric decoder always
  quantizes to dscale (a zero built from 8192 zero digit-groups kept
  exponent -32768 and rendered 0.000…0 — the #38139 regression
  payload), and a dscale outside PG's NUMERIC_DSCALE_MASK (0x3FFF)
  raises 22P03 at Bind like numeric_recv (the corpus sends 0xFFF0).
  Slice 12 greened `enum`: an unaliased cast to a user-defined type
  names its output column after the TYPE (SELECT 'hi'::te → column
  te), and RowDescription reports typlen 4 for minted enum oids
  (PG stores enum values as 4-byte oids; the [65000, 66000) range
  check in pgwire is pinned to catalog's bases by a test). Slice 13
  greened `errors`: multi-name DROP TABLE is ONE statement — one
  CommandComplete tag, and without IF EXISTS every name must resolve
  before anything drops (a MULTIDROP_TABLE command wrapping the
  per-name Drop nodes); Bind rejects format codes other than 0/1
  with 08P01 before BindComplete; the FK ConstraintName error field
  already passed unchanged. Corpus: 28 unexpected failures; green:
  aborted_txn, array, batch_stmt, bind_and_resolve, json,
  json_array, char, citext, copy, copy_file_upload, decimal, enum,
  errors (+2). Slice 14 greened `execute`: Describe(P) of a
  wire-parsed ``EXECUTE name(args)`` resolves the UNDERLYING
  SQL-level prepared statement and reports ITS shape (a SELECT's
  RowDescription, not NoData). Slice 15 greened `float`: float4
  casts narrow to single precision and float4out renders the
  shortest float32 round-trip (0.33333334, arrays included via the
  element tag); extra_float_digits < 1 reduces %g precision for both
  float widths; SET with a NEGATIVE value no longer drops the sign
  (Neg's .name is the bare inner literal); bare ARRAY[…]/ROW(…)
  constructors name their columns array/row. Slice 16 greened
  `implicit_txn`: a nested BEGIN inside an explicit block completes
  with the BEGIN tag but emits PG's WARNING notice — 25001 with the
  File/Routine identity fields (xact.c / BeginTransactionBlock),
  which the notice plumbing now carries (SQLResult.notices accepts
  (severity, message[, sqlstate[, file[, routine]]])). Slice 17
  greened `inet`: malformed binary inet payloads get PG's error
  classes — a truncated header is 08P01, a bad family or address
  length is 22P03 (inet_recv; XX000 leaked before). Slice 18 greened
  `int2vector` (one documented divergence: the corpus expects crdb's
  indoption=2 NULLS-FIRST pkey value; PG and we report 0 — matching
  2 would corrupt SQLAlchemy reflection; in EXPECTED_DIVERGENCES):
  the BINARY wire form of int2vector is now a real int2 ARRAY —
  elemoid 21, 2-byte elements, lower bound 1 (crdb #111907's
  regression class; the text "0" leaked through binary result
  format before). Slice 19 took `jsonpath` to its documented
  divergence only: the jsonpath TYPE landed (oid 4072, canonical
  member quoting via jsonpath.canonicalize — $.abc → $."abc" — 42601
  on an empty path, binary = version byte + PG's UNQUOTED canonical
  text; the corpus's quoted binary form is crdb's, recorded in
  EXPECTED_DIVERGENCES); jsonb_path_query coerces a string first arg
  to jsonb like PG's implicit cast; unaliased function-call columns
  are named after the function (PG rule — was ?column?). Corpus: 22
  unexpected failures. Slice 20 greened `ltree`: the ltree type
  landed on crdb's stable placeholder oid 90010 (like citext's
  90008), stored as validated dotted-label text, binary = version
  byte + text, INSERT-target and comparison inference (the
  citext-only comparison branch is now the {citext, ltree} operator-
  family set). Slice 21 advanced `multiple_active_portals`
  :137 → :681 (9 of 18 subtests): PG portal semantics landed —
  re-binding a NAMED portal live in the same explicit txn is 42P03
  (with crdb's detail shape), portals DIE at transaction end
  (cleared in _end_txn_state and at Sync when no block remains; a
  post-Sync Execute is 34000 "unknown portal"), and DROP TABLE
  refuses 55006 while an undrained portal in the session reads the
  table. **Remaining blocker (architectural): the query_timeout
  subtest needs row-LAZY portal execution** — pg_sleep evaluated per
  fetched row with a cumulative statement_timeout across
  Execute-resumes; our portals materialize fully at first Execute,
  so the timeout can't fire mid-drain. Slice 22 greened `oid`: the
  reg* pseudo-types (regproc 24, regprocedure 2202, regclass 2205,
  regtype 2206, regnamespace 4089, regrole 4096) ride the oid wire
  form — typlen 4 in RowDescription and a 4-byte unsigned binary
  decode (raw bytes echoed through before), with a wrong-length
  payload rejected 08P01 like PG's oidrecv. Slice 23 greened
  `param_status` (350 lines, :7 → green): ParameterStatus now follows
  the command's CommandComplete in BOTH protocol paths; a numeric SET
  TIME ZONE reports PG's POSIX spec (`+6` → `<+06>-06`, sign inverted
  after the label); IntervalStyle and is_superuser joined
  REPORTABLE_GUCS (IntervalStyle lowercased); DateStyle reports
  canonically as `<style>, <order>` regardless of written order;
  `SET LOCAL TIME ZONE` honours LOCAL scope and canonicalizes;
  SET ROLE / `SET [LOCAL] role =` report is_superuser, but only on a
  real change; savepoints snapshot the GUCs so ROLLBACK TO SAVEPOINT
  reverts and re-reports them; the transaction-end and ERROR-time
  unwinds report case-insensitively ordered, including is_superuser
  when the role reverts. Slice 24 greened `parameter_description`:
  PG's FigureColname precedence (a cast's OPERAND names the column, so
  `n::int4` is `n` while `2::int8` stays `int8`); `SET col = $N` and
  `col = $N` type the parameter from the COLUMN (uuid 2950,
  timestamptz 1184 — the old branch was citext/ltree-only; `"char"`
  stays text, which the char corpus pins); one-type-per-parameter
  conflicts raise 42883 (`lower($1)` with `$1::int`); a gap in
  parameter numbering (`SELECT $2 > 0`) and a bare parameter as the
  only CASE result raise 42P18. Slice 25 advanced `portals`
  :147 → :1182 of 1550 (now a documented divergence): an Execute
  delivering EXACTLY MaxRows always sends PortalSuspended — PG can't
  know the portal is exhausted until an Execute fetches past the last
  row — and each Execute's CommandComplete counts the rows THAT
  Execute delivered (the drained one reports `SELECT 0`; a slice-21
  test of mine had asserted the portal total). **:1182 is a
  fidelity-conflict divergence**: the stanza compares the
  CHECK-violation MESSAGE and pins crdb's wording; ours is real
  PostgreSQL's, so matching would be a regression.
  **Consequence to remember: the stanzas AFTER :1182 are not
  exercised** — they cover 34000 'unknown portal' (already
  implemented) and 42P03 'cursor "p" already exists as portal'
  (NOT implemented — a DECLARE colliding with an open portal name).
  Corpus: 17 unexpected failures. Slice 26 greened `prepare`: Bind now
  requires exactly as many parameters as the prepared statement has,
  and DECLARED oids count even when the query uses fewer placeholders
  (three declared / one used → a one-parameter Bind is 08P01). The
  check sits AFTER the COPY-specific 08P01, whose error carries PG's
  statement-summary Detail — putting it first regressed the `copy`
  file, which the full-corpus re-run caught.
  **Gauge-harness limitation FIXED (runner isolation):** the runner
  now spawns a FRESH daemon per corpus file (anchored
  `-run TestPGTest/^<file>$`, `-json` streams concatenated), because the
  corpus assumes a clean database per file — `execute` leaves table
  `t0` behind, so `prepare` and `copy` failed in a shared-daemon run
  while passing alone. Upstream crdb uses a per-file server for the
  same reason. **The report is now the honest measurement: 37/64 pass,
  4 expected divergences, 18 unexpected failures, 5 skipped** (campaign
  start: 10/58 pass, 43 unexpected). The 58→64 denominator shift is
  real: files that used to fail on line 1 now get far enough to emit
  their subtest events. Slice 27 greened
  `prepared_stmt_invalidation`: PG revalidates a named statement's
  cached plan during BIND, so a shape changed by DDL raises 0A000
  INSTEAD of BindComplete and no portal is created (we raised it at
  Execute, after BindComplete). `aborted_txn` ignores BindComplete, so
  Bind-time satisfies both files, and it keeps the check ahead of any
  side effect (the data-modifying-CTE case). Report: 38/64 pass, 17
  unexpected. Slice 28 took `row_description` to its last stanza
  (recorded as the 5th expected divergence — its final directive sends
  crdb-only `::STRING` / `::STRING(2)` with no `crdb_only` marker, and
  real PG 14 rejects both with `42704 type "string" does not exist`,
  probed). Four fixes: JOIN and view result columns now carry base-column
  identity (`out_sources` per output position, replacing the single
  `base_table`), `CREATE VIEW v (cols…)` no longer registers the view
  under an EMPTY name while reporting success (the column list parses as
  a `Schema` node wrapping the name — every reference to such a view
  failed 42P01), `char(n)` blank-pads on the wire, and `ALTER COLUMN …
  TYPE` recomputes the declared oid/typmod instead of inheriting the old
  one. Slice 29 took `typing` to its ceiling (recorded as an expected
  divergence — both its non-crdb stanzas use `keepErrMessage` to pin
  crdb's `22023 unsupported comparison operator: <varchar> = <uuid>`,
  where real PG 14 raises `42883 operator does not exist: character
  varying = uuid`, probed). The behaviour those stanzas regression-test
  IS now implemented: a comparison against a parameter whose DECLARED
  type has no operator against the other operand raises 42883 **at
  Parse** (the only place the declared OIDs are known), instead of
  silently matching no rows; the message names the declared type, not
  the folded storage tag. Known gap left behind: the same comparison
  written as a *cast* (`v = $1::uuid`) is still unjudged — `_CATEGORY`
  covers the cast path and doesn't model uuid/bytea/json/inet, which
  only `_PARAM_CATEGORY` does. Remaining files: `tuple`, `procedure`,
  `timezone` (`set`, `unknown`, `varbit`, `void` greened by #971; the
  geo trio `box2d` / `pgvector` / `spatial` reclassified by #973).
- **psycopg**: per-file failure signature matches the committed 99.0%
  baseline everywhere EXCEPT two REAL regressions the sweep caught from
  the temp-namespacing work — diag TABLE NAME leaking the ``pg_temp_<n>.``
  prefix, and self-referencing FKs in CREATE TEMP TABLE pointing at a
  nonexistent bare name (deferred checks silently never fired). Both
  fixed in this slice; ``tests/test_errors.py`` back to its baseline
  signature. (A 125-failure ``test_typing.py`` cluster in the sweep run
  was venv-artifact — mypy example files — not server behaviour;
  ``test_generators.py::test_cancel`` now times out instead of failing
  fast, consistent with cancel support landing — worth a look in the
  next official run.)

### SQL / PostgreSQL section — survey (2026-08-14)

**The headline count is wrong by 4x.** The section carries 126 unchecked
`- [ ]` items, which reads as 126 open problems. It is not: **95 of them are
completed work whose headline literally says "landed" / "shipped" / "FIXED"**
and whose box was never ticked. CLAUDE.md says to delete an item's line when
it is fixed; that has not been happening here, and the section has become a
changelog wearing a to-do list's clothes.

**Two entries were actively misleading, not merely stale.** `CHECK / UNIQUE
constraints — declared, reflected, NOT enforced` (b91) and `Foreign keys —
declared, reflected, NOT enforced` (b81) are contradicted by a LATER entry,
`Enforcement made uniform across all write paths` (b98). Measured against the
real engine rather than trusting either:

```
CHECK    violate -> SQLError: new row for relation "c1" violates check constraint "c1_n_check"
UNIQUE   dup     -> SQLError: duplicate key value violates unique constraint "u1_n_key"
NOT NULL null    -> SQLError: null value in column "n" violates not-null constraint
FK       orphan  -> SQLError: insert or update on table "ch1" violates foreign key constraint
```

All four are enforced, with Postgres-shaped messages. A reader skimming for
data-integrity risk would have found two entries announcing that constraints
are not enforced — the single most alarming thing a database backlog can say —
and both are false. That is worse than a stale checkbox: it misdirects
attention away from real problems.

**After removing landed work and status notes, ~17 items are genuinely open.**
They cluster into four groups, in the order I would tackle them:

1. **Shared-storage constraints (won't fix at this layer)** — sub-millisecond
   timestamp fidelity (~11 gauge tests) and `numeric` beyond 34 significant
   digits. Both are consequences of storing SQL values as BSON, the same
   representation the Mongo side reads. Fixing either means diverging SQL
   storage from the dual-protocol reflected-table path. These should be moved
   OUT of the open list into "out of scope with reasoning" — they are
   decisions, not tasks.

2. **Driver-gauge clusters** — pgjdbc, pgtest, and pgx each have a bucket of
   remaining failures. This is the same shape as the Mongo-side gauge work
   that took driver conformance from 53 real failures to ~2, and the same
   method applies: read the actual failure output, cluster by root cause, and
   expect one cause to explain many failures.

3. **Protocol/semantic gaps with real users** — query pipelining, streaming
   COPY OUT abort, cross-connection async NOTIFY delivery, and write-conflict
   retry (40001) semantics.

   **Correction (2026-08-14): this cluster originally named `SAVEPOINT` as a
   no-op and recommended it first. That was wrong, and wrong in exactly the
   way this survey warns about** — it came from the entry's stale HEADLINE
   ("SAVEPOINT is a no-op") while the entry's own BODY said "Real nested
   savepoints landed (b71)". Measured against the engine: `SAVEPOINT` /
   `ROLLBACK TO SAVEPOINT` / `RELEASE SAVEPOINT` all work, including nesting
   and RELEASE merging snapshots into the parent, with Postgres' SQLSTATEs
   (`3B001` unknown savepoint, `25P01` outside a transaction block). The
   headline is now fixed. The lesson stands and cost me directly: reading a
   headline is not reading the entry.

4. **Correctness edges** — cross-type comparisons returning false where
   Postgres raises `42883`, the `HAVING` general-shape residual, and
   `DatatypeMismatch` on untyped parameters.

**Recommended first action is not a code change.** Reconcile the section:
delete or tick the 95 landed items, move the two shared-storage constraints
to the out-of-scope section, and correct the two false "NOT enforced"
headlines. That takes the section from 126 apparent problems to ~17 real
ones, and removes two entries that actively point away from where the risk
is. Only then is a prioritised implementation plan worth writing, because
only then does the list describe reality.

**Method note.** Every claim above was checked against the engine or the
file, not inferred from the entry text. The two "NOT enforced" entries are
the fourth, fifth and sixth stale-or-wrong backlog claims found this session
(after the two transaction divergences, the php-lib "out of scope" reading,
and the `maxMessageSizeBytes` framing). The pattern is consistent enough to
be worth naming: **this backlog's prose is a hypothesis, not evidence.**


- [ ] **Cross-type comparisons: the remaining lenient pairs.** The plan-time
  42883 analysis shipped in `src/secantus/sql/typecheck.py` — a comparison
  between two *confidently-typed* operands drawn from two different categories
  (numeric / string / boolean / date-time) now raises `42883 operator does not
  exist: text = integer` before any row is read, on declared tables. What is
  still deliberately lenient, and why:
  - **Reflected (schema-on-read) tables are exempt wholesale.** A reflected
    column's type comes from sampling 50 documents, so a heterogeneous BSON
    field can be declared `text` while holding integers; the dual-protocol
    path *wants* the cross-BSON-type comparison. This is not a gap to close —
    it is the correct answer for that table kind.
  - **Categories not modelled**: `bytea`, `uuid`, `json`, `money`, `oid`,
    `interval`, `time`/`timetz`, `"char"`, arrays, ranges, geo, network, bit.
    Several of those pairs genuinely error in Postgres (`bytea = text`,
    `uuid = text`, `interval = integer`), but the implicit-cast graph is
    subtler and a false 42883 is worse than the lenient FALSE.
  - **Same-category pairs Postgres still rejects**: `enum = text` (an enum
    column carries the `text` tag here), `citext = name`. Lenient by design.
  - **Operand shapes not typed**: bound parameters, subqueries, arithmetic,
    `||`, `CASE`, and every function outside the small text-preserving /
    int-returning allowlist. Also `IN`-lists and `BETWEEN` (only the six binary
    comparison operators are analysed).
  - **Statement shapes skipped**: CTEs, derived tables, set operations, table
    functions in FROM, views, and any comparison nested inside a subquery
    scope. Also **FROM-less selects** (`SELECT 'a'::text = 1`) — the analysis
    is driven by declared column types, and constant-only expressions are most
    of what the psycopg / SQLAlchemy gauges evaluate, so widening to them wants
    its own gauge run.
  - **`UPDATE … SET col = expr`** is an assignment, not a comparison: Postgres
    reports an unassignable value as `42804 datatype_mismatch` under
    assignment-cast rules. Still unimplemented — a `SET text_col = 42` is
    silently coerced.
  - **Not yet verified against the psycopg / sqllogictest / SQLAlchemy
    gauges** (`invoke validate-psycopg` / `validate-slt` /
    `validate-sqlalchemy`). The 2812-test `tests/test_sql*.py` suite and the
    418-test `tests/test_pgserver*.py` suite are green, but the vendored
    gauges are the only population large enough to expose a false 42883.

The embedded SQL engine (`src/secantus/sql/`, `run_sql`) shipped as the P0 spike of
`tasks/sql-postgres-plan.md`. Known gaps, to close in later phases:

The psycopg conformance gauge stands at **99.5%** (4104/4126 ran) after the
2026-07 slice series (PRs #543 #548 #552 #558 #561 #564 + the idle-timeout
slice). The remaining ~22 failures fall into these buckets, which are the
honest edge of what the Postgres front-end can reach WITHOUT changing the
shared storage engine or building large new protocol subsystems:

- [ ] **Sub-millisecond timestamp fidelity (~11 gauge tests).** `timestamp`/
  `timestamptz` (and ts/tstz ranges) truncate to milliseconds — BSON datetime
  is an int64 of millis, the SAME representation the Mongo side uses, so this
  isn't a SQL-layer bug but a shared-storage constraint. Exact microseconds
  need a storage-representation change (ISO-text or a micros sidecar) that
  touches `typemap.coerce`/`to_py`, `scalar._eval_compare`, `sortkey`
  encoding, and every scalar datetime function — and would diverge SQL
  timestamp storage from the Mongo BSON datetimes that the dual-protocol
  reflected-table path reads. The failing tests are psycopg's random-data
  faker suites (`test_adapt::test_random`, `test_copy::test_copy_from_leaks`/
  `test_copy_table_across`) that assert exact round-trips, so they flap on
  whether a random draw's microseconds land on a whole millisecond.
- [ ] **`numeric` beyond 34 significant digits.** Stored as Decimal128, which
  caps at 34 digits (same shared constraint); wider *stored* values round.
  The binary wire codec now round-trips arbitrarily wide values (PR #564), so
  the param-only `test_dump_numeric_exhaustive` passes; only values that
  actually persist through Decimal128 storage lose precision. Exact wide
  storage would need a text/dual representation for `numeric`.
- [ ] **Query pipelining (`test_generators::test_pipeline_communicate_abort`).**
  psycopg's pipeline mode batches many extended-protocol messages before a
  Sync; the server processes each `Sync` synchronously rather than tracking a
  pipeline-abort boundary. A real feature (its own subsystem), not a bug.
- [ ] **CancelRequest handling (`test_generators::test_cancel`, and the
  environmental `test_cancel_safe_*` "proxy didn't start listening: Errno 22"
  harness failures).** The v3 CancelRequest startup packet is parsed but not
  acted on (statements run synchronously, so there is no mid-query cancel
  point to interrupt). The `test_cancel_safe_*` failures are a psycopg
  test-harness socket-proxy issue on this machine, not a server behaviour.
- [ ] **Reject connections to non-existent databases (`test_pgconn_error`,
  `test_connect_bad`).** SecantusDB creates a database on demand (the
  ephemeral-db model shared with the Mongo server), so connecting to
  `dbname=nosuchdb` succeeds where real PG raises. A deliberate divergence —
  rejecting unknown dbs would break the in-process test-db ergonomic.
- [ ] **Streaming COPY OUT abort (`test_copy_out_error_with_copy_not_finished`).**
  COPY OUT materializes the whole result and sends it in one burst, so a
  client that reads one row and aborts finds the copy already finished
  (transaction stays INTRANS instead of INERROR). Needs interleaved
  client-abort detection during a streamed COPY OUT.
- [ ] **`test_return_untyped[b]` / DatatypeMismatch.** A binary untyped
  parameter used in a context that PG rejects with 42804 — niche.
- [ ] **Write-conflict retry semantics (40001 under contention)**: concurrent
  writers to the same row surface WiredTiger's optimistic write conflict as a
  PG-SERIALIZABLE-style `40001 could not serialize access` instead of real
  PG's READ COMMITTED block-and-proceed. Clients that retry (pgbench 15+
  `--max-tries`, retry-loop applications) behave correctly; ones that treat
  40001 as fatal abort. A server-side fix means statement-level replay after
  waiting out the competing transaction (WT invalidates the whole txn on
  conflict, so the user transaction's prior statements must replay too).
  Until then the sql-stress smoke keeps its write lanes single-client.
- [ ] **pgjdbc gauge — remaining clusters** (`invoke validate-pgjdbc`, 93.7%
  over the `jdbc2` package — 5183 P / 347 F / 28 S,
  `docs/validation-report-pgjdbc.md`; was 89.7%). Read the per-message counts
  off the JUnit XML in `vendor/pgjdbc/pgjdbc/build/test-results/test/*.xml`
  rather than eyeballing class totals — the report lists test ids only, and
  attributing a class's failures to the wrong cause once already cost a
  feature that fixed nothing here. Remaining, largest first:
  - **No implicit transaction around a pipelined batch** (64,
    `BatchFailureTest`) — **root cause found, fix written and proven, but it
    cannot ship yet.** Postgres treats every message between two Syncs as one
    transaction when no explicit block is open, so a batch is all-or-nothing.
    We commit each Execute independently, so a batch that fails partway leaves
    its earlier rows behind while pgjdbc — correctly — reports every statement
    as `EXECUTE_FAILED` (the update-count array is all `-3`). The client's view
    and the table's contents then disagree, which is what the test asserts on.
    Reproduce in ten lines with psycopg's `pipeline()`: insert two good rows, a
    duplicate key, then another good row, and compare. Real PostgreSQL 14.13
    keeps only the pre-existing row; we keep the two good ones as well.

    The fix is an implicit transaction in `pgextended.ExtendedSession`: open it
    on the first Execute when `session.txn_handle is None`, commit it at Sync,
    roll it back if anything since the last Sync errored, and hand ownership
    over if an explicit BEGIN arrives (psycopg depends on a BEGIN surviving
    Sync). A working implementation — pipelined rollback, successful-batch
    commit and explicit transactions all verified against real PG — is kept at
    `scratchpad/implicit-txn-attempt.py`.

    **It is blocked on the write-conflict item above.** The implicit
    transaction spans a network round trip, so two connections doing ordinary
    concurrent autocommit writes now overlap inside WiredTiger, which aborts
    where Postgres blocks. That turns five `tests/test_pgserver_concurrency.py`
    tests (`test_concurrent_nextval_never_repeats`,
    `test_autocommit_computed_updates_lose_no_increments`, the two
    single-winner race tests, and the dual-protocol stall test) into `40001`
    failures. Trading a batch-semantics divergence for broken concurrent
    autocommit writes is the worse deal, so this waits for block-and-retry
    write-conflict handling and should land immediately after it.
  - ~~**`_pg_expandarray`** (28, `UpdateableResultTest`)~~ — RESOLVED
    (2026-08-15, #904): select-list record SRFs (bare composite + immediate
    `(SRF(x)).n` field access, lockstep multi-reference expansion),
    `(col).field` in JOIN ON, and search_path-aware `pg_table_is_visible`
    (the WHERE lowering hardcoded the default namespaces, hiding every
    user-schema relation after `SET search_path`). Both pgjdbc metadata
    queries (getPrimaryKeys / getPrimaryUniqueKeys) run verbatim.
    UpdateableResultTest's four residuals are separate features: datetime
    micros-vs-millis (BSON millisecond ceiling — now documented in
    `docs/sql.md` "Precision ceilings" alongside the Decimal128 34-digit
    numeric ceiling; both are permanent divergences, not fixable bugs),
    serial RETURN_GENERATED_KEYS, oid-column updatability, and the two
    metadata classes' setup blockers (`COMMENT ON FUNCTION`, composite
    `CREATE TYPE`) — the setup blockers themselves are now fixed.
    BatchDeadlockTest is green (8 pass, 8 upstream-`@Disabled` skips) —
    the old 6F cleared with the pipeline implicit-transaction work.
  - ~~**`relation "" does not exist`** (73)~~ — FIXED. It was pgjdbc's
    `TypeInfoCache` type-lookup query, and reduced to two independent bugs,
    both now closed (`JOIN … USING` cross-joining, and an SRF usable only as
    the sole FROM item). Clearing it took `PGObjectGetTest`,
    `PGObjectSetTest`, `GeometricTest` and `SearchPathLookupTest` with it and
    moved the gauge 92.5% → 93.7%. One divergence from that work remains:
    - ~~**`SELECT *` over a `USING` join does not merge the joined column.**~~
      RESOLVED (2026-08-14): `planner.expand_using_star` rewrites the lone
      star into the merged column list BEFORE `desugar_join_using` erases the
      USING marker — one site, not a dozen. `tbl.*` over joins (which
      crashed) expands via `expand_table_stars`. Pinned by
      `tests/test_sql_join_using.py::TestStarMerge`.
    Multi-entry `search_path` resolution landed separately and moved none of
    these.
  - ~~**`pg_type` has no array-type rows.**~~ RESOLVED (2026-08-14): every
    type with a `typarray` gets the paired array row (`_<name>`, `typelem`
    back-pointer, `typinput = array_in`); `typelem` column added; `::regproc`
    strips the schema like PG (planner + scalar cast paths). EnumTest's
    enum-array resolution works; cross-checked green on the psycopg/pg8000
    local suites. Named residuals, all separate features: pgjdbc
    `enumArrayArray` (nested enum-array resolution returns no rows — needs
    the exact TypeInfoCache query captured), `testNonStandardBounds`
    (negative array subscripts `intarr[-1]` unsupported in the planner),
    `testUnknownArrayType` (fails beyond the now-present `relacl` — next
    probe is the exact metadata query). Original entry:
  - **`pg_type` has no array-type rows.** `typarray` on a scalar type points
    at an oid (int4 → 1007) that has no row of its own, so a driver resolving
    an array type by oid finds nothing. `typinput` now exists and reports
    `array_in` for any `_`-prefixed typname, so the standard
    `typinput = 'pg_catalog.array_in'::regproc` array test will work the day
    those rows appear — but nothing currently matches it. Note that
    `::regproc` does not strip the schema the way real Postgres does; that
    only becomes observable once array rows exist, and both should be fixed
    together. Widening `pg_type` touches every gauge that reads the catalog
    (psycopg especially), so measure across gauges, not just pgjdbc.
  - **Metadata classes, revealed by the setup unlock (2026-08-16)** —
    ResultSetMetaDataTest is now a clean 60/60 (STRUCT oids + base-column
    identity + declared typmods, PR #920). DatabaseMetaDataTest's setup
    is fully unblocked (pg_description DML + operator DDL + PK USING
    INDEX + composite arrays + OUT-param arity): 74 pass, 82F across 18
    clusters (2026-08-16 run), biggest first: MaxIndexKeys via missing
    `pg_am`/`pg_settings` catalog data (18F, kills all FK metadata);
    assorted boolean asserts needing per-test triage (16F);
    **FK metadata queries** — getImportedKeys' 9-way comma-join FIXED:
    comma-joins are now keyed from WHERE equalities (was a 13-min hang /
    zero rows, then ~16s, then a 183GB OOM once the class ran past the
    #944 setup regression — now completes in ms), plus a hard per-stage
    materialization cap that surfaces 54000 instead of OOMing on any
    residual cross product;
    `pg_proc.proargmodes` column (6F); quoted-uppercase output aliases
    like `"TABLE_TYPE"` erroring instead of resolving (6F + 4F
    "DATA_TYPE" — likely one alias-resolution bug); COMMENT ON TYPE (6F)
    and COMMENT ON INDEX (2F) unsupported; `pg_class.reltuples` column
    (4F); ACL reflection: getTablePrivileges for tables/views/matviews
    FIXED (relowner→role oid + relacl materialization); the remaining ACL
    piece is **columnPrivileges** (2F) — getColumnPrivileges over the
    system catalog `pg_statistic`, which needs pg_statistic modeled as a
    queryable relation with columns in pg_class/pg_attribute;
    domainColumnSize FIXED (domain pg_type carries the base type's
    typtypmod/typbasetype; int-domain's 10 is int4's fixed precision,
    already right); customArrayTypeInfo FIXED (composite column oids +
    collision-avoiding array type names); indexInfo
    mixed expression+column multi-key index (2F); catalogs list missing
    "postgres" (2F); pg_get_keywords() in-context (2F);
    getColumnsCharOctetLength (2F); ALTER TABLE DROP of a quoted
    ``"name"`` column mis-parsed as `DROP name` (2F).
    RefCursorTest is 8/8 after the plpgsql OPEN/refcursor slice;
    RefCursorFetchTest: aggregates over SRF row sources now work
    (sum/string_agg/array_agg over generate_series, incl. in scalar
    subqueries), but the class's seeding INSERT still fails one layer
    deeper — its string_agg ARGUMENT is
    `lpad(to_hex(width_bucket(random(),0,1,256)-1),2,'0')` and
    `_agg_arg_to_expr` can't compile lpad/to_hex/width_bucket/random
    into pipeline expressions ("unsupported array_agg argument").
    Either extend the pipeline expression vocabulary or evaluate
    aggregate args per row in Python on the derived path.
  - **AutoRollbackTest savepoint/autosave semantics** (~24): `autosave`
    modes and `flushCacheOnDeallocate` / `DEALLOCATE ALL` behaviour around
    failed statements in a transaction.
  - **StatementTest / PreparedStatementTest residuals (2026-08-15)** — the
    grabbag slice fixed dollar quotes, nested comments, non-standard
    strings, the `{fn}` scalar surface, stable `now()`, CTAS, and
    TRUNCATE qualification. Still failing, each a separate feature:
    `parsingSemiColons` (CREATE RULE — rewrite rules unimplemented);
    `warningsAreAvailableAsap` / `closeInProgressStatement(Protocol32)` /
    `concurrentWarningReadAndClear` (mid-statement NOTICE streaming +
    blocking LOCK TABLE + plpgsql FOR; the Protocol32 variant additionally
    needs NegotiateProtocolVersion for a 3.2 StartupMessage);
    `setQueryTimeout{,WithSleep,OnPrepared}` (query cancel — a parallel
    session's cancel-request worktree owns this; this slice deliberately
    backed out its own overlapping implementation);
    `testDoubleQuestionMark` (`?-` prefix / `?#` infix geometric operators
    — sqlglot can't tokenize either, needs a parser extension, and the
    `lseg '...'` / `box '...'` typed-literal prefix syntax doesn't parse
    outside `point`);
    `testNumeric` (1000-digit numerics exceed Decimal128's 34 significant
    digits — a storage-representation ceiling, cast/round-trip truncates);
    `testUnknownSetObject` (extended-protocol strictness: a varchar-typed
    param inserting into an interval column must raise 42804; our insert
    coercion doesn't see the param's declared OID);
    `testSetObjectBigDecimalWithScale` (was the TRUNCATE qualifier bug —
    expected fixed by this slice, verify on the next gauge run).
  - ~~**Large objects** (11, `BlobTest`)~~ — FIXED (the `pg-large-objects`
    slice): the Fastpath sub-protocol + `lo_*` built-ins landed
    (`secantus/sql/largeobjects.py`), plus the surrounding pieces pgjdbc's
    callable shape needs (UDF-in-FROM typed by declared return type,
    describe-without-executing, void-arg drop, plpgsql `RAISE`, `lo_manage`
    accepts). `BlobTest` 28/28, `BlobTransactionTest` 1/1, `CallableStmtTest`
    14/14, `CleanupSavepointsWithFastpathTest` 10/10 — all four were zeroed
    before. Known divergences, deliberately accepted:
    - **LO descriptors close at session end, not transaction end** (PG closes
      them at commit/rollback). No gauge test distinguishes the two.
    - **`lo_manage` trigger DDL is an inert no-op** — replaced large objects
      are never unlinked by the trigger (nothing vacuums orphans here anyway).
      Every other `CREATE TRIGGER` stays rejected.
    - **`lo_import` / `lo_export` are absent** (server-side file I/O; nothing
      drives them over the wire in the gauges).
  - ~~**DateTest date offsets** (21)~~ — FIXED across several slices; now
    **192/192** (re-measured 2026-08-12) — see the dedicated section below; the
    shape is now measured rather than guessed at.
  - **`bind parameter $N has no value`** (8, BatchedInsertReWrite): pgjdbc's
    insert-rewrite batches leave a parameter unbound on a re-written statement.
  - ~~**`internal error`** (13)~~ — FIXED (gauge 5311 P / 219 F / 28 S =
    **96.04%**, measured after the fix; `BatchedInsertReWriteEnabledTest`
    16 → 4 failures, `BatchFailureTest` 64 → 48). Every one was a crash that
    dropped
    the connection rather than returning a usable message, and each collapsed
    to a small independent cause once the failing *statement* was paired with
    the traceback (see "Capture the statement behind the error" below — the
    same technique that cracked the earlier clusters):
    - 16-bit protocol **count** fields (parameter / column counts) were read
      *and written* as signed. Postgres allows 65535 parameters per Bind and
      pgjdbc's rewritten batches really do send tens of thousands; above 32767
      the count came back negative and walked the parse offset backwards
      (`not enough data to unpack 4 bytes at offset -2`). Genuinely-signed
      fields — attnum, type size, format codes — stay `_INT16`.
    - Geometric types had **no binary parameter decoder**, so a `point` / `box`
      / `polygon` sent in the binary format reached the *text* parser as raw
      bytes. Decoders for all seven now exist.
    - `line` could not be parsed even as text: its canonical form is three
      coefficients `{A,B,C}`, and the branch handling it sat *after* the
      coordinate-pair parse it could never survive — dead code.
    - `time ± interval` had no overload at all (`str + dict`).
    - Interval arithmetic in a WHERE was lowered into a Mongo `$expr`, which
      has no interval type; the cast was silently dropped and `$multiply` got
      the raw literal text. Interval casts now refuse to lower, so the
      predicate falls back to per-row scalar evaluation.
    Two further defects surfaced in the same blast radius and are also fixed:
    - **Binding N parameters was `O(N**2)`** — sqlglot re-parents every sibling
      on each `Expression.replace`. A 40000-parameter statement took over two
      minutes, so the batch tests unblocked above would have *timed out*
      instead of crashing. Now linear (0.38s). Pinned by
      `test_sql_planner.py::test_substitute_parameters_scales_linearly`.
    - The `pg_class` / `pg_attribute` / `pg_attrdef` / `pg_description` /
      `pg_index` builders **enumerated the table list twice** — once to assign
      OIDs, once to emit rows — so a table created by another session in
      between raised `KeyError` mid-scan. `virtual._tables_with_oids` is the
      single-snapshot accessor they all use now.
    A second measured pass caught two the first had *moved* rather than fixed:
    `timetz ± interval` had no overload either (only bare `time` did), and once
    the interval predicate correctly stopped being pushed down, comparing the
    `date` column against the computed timestamp raised `TypeError` one step
    further in — a stored `date` is ISO text, and Postgres promotes it to
    midnight. Both fixed (verified directly; gauge re-measure below). The lesson is the
    obvious one: re-measure after the fix, because "the crash moved" looks
    exactly like "the crash is gone" from the code side.
    Separately, `'23:59:60'::time` stored the literal second 60, which nothing
    downstream could parse — so `time - time` *already* crashed the same way.
    It now carries forward to `24:00:00` as Postgres does, and time arithmetic
    goes through `datetimes.time_micros` because that boundary value does not
    fit in a Python `datetime.time`.
- [ ] **Cross-connection async NOTIFY — WORKS; only the pgjdbc re-run is
  outstanding.** Measured 2026-08-18: with one psycopg connection blocked in
  `notifies()` (both the timeout form and the endless form, which is what
  pgjdbc's `NotifyTest` uses), a `NOTIFY` on a second connection IS delivered.
  `NotifyHub.notify` fans out to every listening session's queue and
  `pgserver._read_next_message` polls listeners on a 0.25s `select` slice and
  flushes them — the "needs to fan out and wake a blocked reader" work this
  entry described is done (the `select`-based poll from #882 is what closed
  it). `NotifyTest` stays excluded from the pgjdbc gauge only because it has
  never been re-run since: if it did still hang, a JUnit timeout cannot
  interrupt that socket read and it would wedge the weekly gauge. Closing this
  is one gauge run — drop the `EXCLUDE_CLASSES` entry and run
  `pgjdbc_validation.runner`.
- [x] **pgtest gauge — CAMPAIGN COMPLETE.** `docs/validation-report-pgtest.md`:
  **49/66 files pass, 12 expected divergences, ZERO unexpected failures**
  (5 skipped), from 43 unexpected failures when the campaign started
  2026-08-16. Every file that does not pass is a documented crdb-vs-PG conflict,
  each with its reason inline in `pgtest_validation/include_paths.py`.

  The divergences fall into two kinds, and neither is fixable without making
  SecantusDB *less* faithful to PostgreSQL:

  * **crdb internals pinned by the corpus** — `char` (crdb's deterministic
    TableOID), `int2vector` (crdb's NULLS-FIRST pkey indoption), `procedure`
    (crdb's NoticeResponse `File`/`Routine` = `builtins.go`/`func401`; real PG
    emits `exec_stmt_raise`/`pl_exec.c`, probed), `portals` and `typing`
    (`keepErrMessage` pinning crdb's error wording where PG's differs).
  * **crdb-only surface** — `row_description`'s `::STRING` casts (PG: `42704`),
    `jsonpath`'s binary form, and the extension types `spatial` / `box2d` /
    `pgvector`.

  Fixes that landed along the way, in order: 25P02 aborted-transaction rules,
  binary-param error classes, batch/segment parse, portal lifetime + snapshots,
  citext, COPY, decimal, enum, execute, float4, error fields, inet, cached-plan
  revalidation at Bind, base-column identity across joins/views/retypes,
  parameter-type resolution at Parse, timetz sub-minute offsets, `void`, `set`,
  `varbit`, `unknown`, row constructors, and CREATE PROCEDURE / CALL.

  **Note for re-runs:** drive the runner directly —
  `PYTHONPATH=. .venv/bin/python -m pgtest_validation.runner` then
  `... -m pgtest_validation.generate_report .validation/pgtest-raw.json
  docs/validation-report-pgtest.md`. `invoke validate-pgtest` spawns the daemon
  without `PYTHONPATH` and dies with "pg daemon did not become ready within
  15s" (the same wrapper issue the psycopg gauge has).


- [ ] **pgx gauge** (`invoke validate-pgx`, `docs/validation-report-pgx.md`):
  **2026-08-15 official run at `03d5c63b`: 376 P / 2 F / 22 S = 99.5%**,
  from the 2026-08-14 baseline 291/87/22 = 77.0% after the pgconn campaign
  (#866 #868–#871 #873 #876 #877 #885 #891 #901). Every package is at its
  ceiling: `pgconn` 99.0% (0 stable failures — the 2 failure entries are
  the single load-sensitive `TestDeadlineContextWatcherHandler` family,
  documented in the pgconn findings section), `pgproto3` / `bgreader` /
  `ctxwatch` 100%. Remaining work is only the load-sensitivity
  re-measurement on a quiet machine.
- [ ] **Sub-millisecond timestamps — READS are precise; PREDICATES are not.**
  BSON has no sub-ms date (a date is int64 milliseconds; `Timestamp` is
  seconds+ordinal, an internal replication type), so the remainder is stored
  beside the date in a hidden `__us_<field>` companion — see
  `src/secantus/sql/subms.py` and the "Sub-millisecond timestamps" section of
  `docs/sql.md`. INSERT / UPDATE / SELECT / RETURNING round-trip microseconds,
  `SELECT *` and reflection hide the companion, and every write resolves it (a
  whole-millisecond UPDATE *removes* it — a stale remainder would report a time
  that was never stored).

  **Still open, in rough order of value:**
  1. **Predicates are millisecond-blind.** `WHERE t = '…123456'` matches nothing
     and rows inside one millisecond sort arbitrarily — unchanged from before
     the companion existed (verified against `main`), but now the fix is
     possible: lower a comparison against BOTH fields
     (`{t: <trunc>, __us_t: <rem>}` for equality; an `$or` of "later
     millisecond" OR "same millisecond and larger remainder" for ranges) and add
     the companion as a sort tiebreaker (missing sorts lowest, which is 0 — the
     right answer).
  2. **Pipeline-shaped reads truncate.** GROUP BY / JOIN / DISTINCT read a
     *projected* document, and the companion was never projected. Each `$project`
     the planner builds would need to carry it, and a GROUP BY on a timestamp
     would need it in the group key to avoid merging rows that differ only in
     microseconds.
  3. **COPY and the Rust server** don't write the companion — they truncate, as
     before. Not wrong, just not precise.


- [x] **HAVING general-shape residual — DONE.** Any shape the `$match` lowerer
  can't express now falls back to the per-grouped-row residual route the
  HAVING-subquery case already used, instead of raising `0A000`: the plain
  group planner re-plans through `_plan_group_window_select` on a 0A000 (on a
  pristine copy — planning mutates the tree), and the group-window planner
  catches it inline. Only 0A000 falls back; a real user error (42803 "must
  appear in the GROUP BY clause") still surfaces. Nine of ten surveyed shapes
  used to fail — `NOT (...)`, `BETWEEN`, `count(*) * 2`, `sum(n) + count(*)`,
  `abs(sum(n))`, `CASE`, `coalesce`, `max - min`, `count(*)::text` — and all
  ten now match PostgreSQL 14 row for row.
- [x] **`char(n)` / `varchar(n)` declared length — ENFORCED.** An over-length
  value now raises `22001 value too long for type character varying(3)` /
  `character(3)` on INSERT and UPDATE, instead of being stored intact (a column
  holding data that violates its own declared schema). Trailing-blank overflow
  is trimmed rather than refused, matching PostgreSQL 14 (`'abc  '` into
  `varchar(3)` stores `'abc'`; `'abcd'` is refused) — `typemap.enforce_declared_length`.
  The `octet_length()` note that sat here turned out to understate the problem:
  `octet_length` and `bit_length` were dispatched to the BIT-STRING
  implementations for EVERY input, so `octet_length('abc')` answered
  `(3+7)//8 = 1` and `bit_length('abc')` answered 3 (PG: 3 and 24). Both now
  measure encoded bytes for text and keep the bit semantics for bit values.
  Still open, and genuinely narrow: `octet_length()` on a `char(n)` returns the
  unpadded count (4 in PG for `'xy'::char(4)`) because padding is applied at
  render, not in storage — the scalar evaluator has no access to the declared
  width; and `char(n)[]` elements are not padded.


- [x] **`::STRING` — WON'T FIX, and here is why.** The crdb type alias parses
  and behaves as `text`, where PG raises `42704 type "string" does not exist`.
  Rejecting it is not worth doing: sqlglot normalises `STRING` to `TEXT` at
  parse time, so by the time the planner sees the cast it is indistinguishable
  from a real `::text` — detecting it would mean sniffing the raw SQL string,
  which is fragile (a column named `string`, a literal containing `::STRING`)
  for a spelling no PostgreSQL client emits. The only thing that asks for it is
  crdb's own corpus, which is already a recorded divergence
  (`row_description`). Accepting a non-PG alias is a permissiveness, not a
  wrong answer.


- [x] **Casts to text — the predicate path is closed too.** `_eval_cast`
  converts int / float / Decimal / **Decimal128** / bool to Postgres' text
  spellings, and a WHERE comparing a cast-to-text now routes to per-row
  evaluation (`_where_has_text_cast_comparison`) instead of pushing a filter
  that compared the raw stored value — `WHERE n::text = '2'` matched NOTHING
  before. Decimal128 mattered specifically because `numeric` is stored as one,
  so `WHERE d::text = '2.50'` was comparing a Decimal128 against a string.
  `interval::text` is fixed too — it used to hand the client our INTERNAL
  subdocument (`{"interval": {"months": 0, "days": 1, ...}}`) instead of
  Postgres' `1 day`; it now renders through `intervals.render`, the same path
  the wire layer uses for an interval column. `bit::text` was already correct.
  Still unconverted: geo and other composite targets.


- [ ] **Multi-way comma-join performance — much improved, `select4`/`select5`
  still over the 300s cap.** Two pushdowns landed (see
  `_push_single_table_predicates` / `_key_comma_joins_from_where`):

  1. **Single-table WHERE conjuncts move to the stage that produces their rows**
     — a bare key into a `$match` ahead of the first `$lookup`, an
     `<alias>.<path>` key into that `$lookup`'s sub-pipeline, and an operator
     subtree (`$or`) when every field it touches is one table's. A top-level
     `$and` is flattened first: any WHERE containing an OR arrives as ONE
     `{"$and": [...]}` key, which read as table-spanning and blocked every
     conjunct beside it. Measured on 3 tables with one constant each: was cubic
     in table size (27k-row product 0.09s, 216k 0.56s, 1M 2.53s), now flat —
     a 343M-row product answers in 0.01s.
  2. **Unqualified cross-table equalities now key the comma joins.**
     `_key_comma_joins_from_where` only matched `c.this.table`, and the corpus
     writes `WHERE a3=b9`, not `t3.a3=t9.b9` — so every comma join stayed
     unkeyed and became a cartesian product. It now resolves an unqualified
     column to its owning alias through the catalog (unambiguous names only) and
     qualifies the relocated ON.

  **Not sound to push into a LEFT join's lookup** — WHERE runs after the join, so
  a predicate on the right table must delete the outer row; only an alias whose
  `$unwind` is non-preserving is eligible. Verified against PostgreSQL 14 across
  11 shapes incl. the LEFT-join traps and a cross-table OR.

  **Remaining:** both files now run instead of failing fast, but still exceed the
  runner's 300s cap (`select4` reached the 5M-doc guard on a 6-way join before
  the OR push; both now time out). The queries are well-constrained — the 6-way
  one expects 30 values — so what is left is the join *strategy*: unkeyed
  lookups materialise their whole foreign side per outer row. Index-backed or
  streaming lookups are the next step, not more predicate motion. They stay out
  of `slt_validation` INCLUDE until then.

- [ ] **Wire server: simple + extended protocol, trust + SCRAM auth, optional TLS.**
  `pgserver.py` speaks v3 startup, simple `Query` (P1), extended `Parse`/`Bind`/
  `Describe`/`Execute`/`Close`/`Sync` (P3), `SCRAM-SHA-256` auth + TLS (P4). Binary
  parameter *input* (`pgextended._BINARY`) **and binary result *output*** are both
  decoded/encoded for the common type-OID set
  (`int2`/`int4`/`int8`/`float4`/`float8`/`bool`/`bytea`/`text`/`varchar`/`date`/
  `timestamp`/`timestamptz`/`numeric`/`jsonb`): the extended protocol honours Bind's
  per-column result-format codes (`pgextended._OUT_BINARY` / `_result_value`, and the
  portal-Describe `RowDescription` reports the chosen formats). The simple-query path is
  always text. Still missing: channel binding (`SCRAM-SHA-256-PLUS`), mTLS client-cert
  auth, user management via
  SQL (`CREATE ROLE` — users are constructor config, not stored in the catalog / shared
  with the Mongo user store), the `Copy` subprotocol, and cursor `DECLARE`. Real-driver
  gauges run in CI via **pg8000** (pure-Python, text params) **and psycopg 3**
  (`tests/test_pgserver_psycopg.py` — libpq via the `psycopg[binary]` wheel, the
  strictest wire exercise: binary params + server-side prepared statements + the psycopg
  SQLAlchemy dialect), each with a SQLAlchemy Core reflection smoke; `psql`/JDBC as live
  gauges still need a libpq CLI / a JVM (absent in the dev env). SQLAlchemy **reflection**
  works end to end (see below).
- [ ] **Reflected tables are now read-write.** A collection with
  no `CREATE TABLE` reflects (sampled schema-on-read) for `SELECT` — incl. `->`/`->>`/`#>`
  jsonb navigation **and GROUP BY / aggregates / JOIN** (the pipeline planner reflects
  via `planner._lookup_table_def(..., storage)`) — **and for INSERT / UPDATE / DELETE**: the
  write gate (`engine._require_table(..., storage)`) falls back to `reflect.reflect`, and the
  INSERT/UPDATE planners are permissive for reflected tables (an un-sampled field is a valid
  write target of the `any` type; the reflected `_id` PK is NOT NULL and immutable, so an
  INSERT must supply it and `SET _id = …` is rejected `0A000`). A write to a *non-existent*
  collection still `42P01` (no implicit collection creation — `CREATE TABLE` first). Remaining
  gaps: in a JOIN, an
  *unqualified* reference to an *un-sampled* field of a reflected table can't be routed (the
  resolver matches on sampled columns — qualify it as `alias.field`, or it must appear in the
  50-doc sample); type inference samples 50 docs and picks the first non-null type per field (no
  widening across conflicting types — first-seen wins).
- [ ] **jsonb operator surface landed (one gap + one parser quirk).** Containment/existence
  operators in WHERE compile to Mongo filters: `@>` (object → dotted-path equalities, array →
  `$all`, scalar → equality), `?` / `?|` / `?&` (key-or-element existence via `$exists` + array-aware
  equality). `jsonb_build_object` / `jsonb_build_array` / `jsonb_array_length` / `jsonb_typeof`
  (scalar) and `jsonb_array_elements` / `jsonb_object_keys` (set-returning) are evaluated per row;
  `->`/`->>`/`#>`/`#>>` navigation now also works inside the per-row scalar evaluator (not just the
  find projection / WHERE). The manipulation functions landed in b120 (`scalar`): `jsonb_set` /
  `jsonb_set_lax` / `jsonb_insert` (path is a Postgres `text[]` via `_pg_text_path`; the value arg is
  JSON-parsed via `_as_json_value`; `create_missing` / `insert_after` honoured; a copy is returned so the
  stored row is untouched), `jsonb_strip_nulls` / `json_strip_nulls`, `jsonb_pretty`, and the `#-`
  delete-at-path operator (`exp.JSONBDeleteAtPath`). Type inference: the modifiers → `json`, `jsonb_pretty`
  → `text`. Tests: `tests/test_sql_jsonb_funcs.py`. **`<@` (contained-by) landed as a residual (#149, b191):**
  `field <@ const` / `const @> field` (and field-vs-field, and scalar-context `<@`/`@>`) now evaluate per-row
  via a COLLSCAN + residual predicate — `_where_has_jsonb_contained_predicate` (shape-based: only `field @> const`
  and `const <@ field` keep the `_jsonb_contains_filter` pushdown) routes them through `where_needs_per_row`, and
  `scalar._eval_jsonb_op` / `_jsonb_containment` implement Postgres object/array/scalar containment (JSON-text
  operands are `json.loads`-decoded first). **`jsonb_each` record SRFs landed (#155, b194):** `jsonb_each` /
  `json_each` (→ `(key text, value json)`) and `jsonb_each_text` / `json_each_text` (→ `(key text, value
  text)`, values rendered like `->>`) work in the base-less `FROM` form — two columns, `AS t(k, v)` renaming,
  `WITH ORDINALITY`, WHERE / ORDER BY (`srf._build_record` / `_record_values`). **Lateral-join form landed
  (#160, b198):** `FROM t, jsonb_each(t.doc) [AS e(k, v)]` expands each row's object into `(key, value)` pairs
  via a `$objectToArray` + `$unwind` stage (`planner._jsonb_each_join_stage`, dispatched next to
  `_unnest_join_stage`); columns default to `key`/`value` and resolve from the unwound `{k, v}` subdoc. **Still
  not modeled:** the lateral `jsonb_each_text` form (the value would need per-row text rendering inside the
  pipeline) and the base-less `SELECT jsonb_each(x)` composite form. **Parser quirk:** sqlglot reads
  a bare `f(a->'k')` arrow as a lambda, so a navigated *function argument* must be parenthesised
  (`f((a->'k'))`) or use `#>` (`f(a #> '{k}')`) — bare navigation in WHERE / projection is unaffected.
- [ ] **Aggregate/JOIN path has gaps (P5 + later slices landed the core).** `GROUP BY` +
  `HAVING` + `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, **multi-table** `INNER`/`LEFT JOIN` (equality
  `ON`, each join relating to the base or an already-joined table), **`SELECT DISTINCT`**
  (single-table and over a join), **and JOIN *combined* with GROUP BY / aggregates / HAVING /
  `array_agg`** compile to an aggregation pipeline. The join builds `$lookup`/`$unwind`, then
  `_plan_join_group_select` appends a `$group` whose keys + accumulators resolve through the
  join resolver (`a.region`, `SUM(b.amt)` → post-unwind paths); WHERE applies pre-group, HAVING
  post-group. **Computed scalar expressions in the SELECT list / ORDER BY** — arithmetic
  (`+`/`-`/`*`/`/`/`%`, PG int-division / mod semantics), `||`, and the common functions
  (`upper`/`lower`/`length`/`trim`/`substring`/`concat`/`abs`/`round`/`ceil`/`floor`/`power`/
  `coalesce`/`nullif`/`greatest`/`least`) now evaluate per row via the evaluated-select path
  (single-table and over a join). **Two-table `RIGHT` / `FULL OUTER JOIN`** now land (b53):
  `$lookup` is left-driven, so `A RIGHT JOIN B` is planned as `B LEFT JOIN A` (drive from B,
  preserve unmatched B) and `A FULL JOIN B` is the LEFT join from A `$unionWith` the B rows with
  no A match (an anti-join sub-pipeline that nests B under its alias so A's columns read NULL);
  `_build_outer_join_pipeline` / `_full_join_anti_branch` in the planner, `amap` kept in FROM order
  so `SELECT *` preserves Postgres column order. Works with WHERE / GROUP BY / aggregates / scalar
  exprs. **Pure-`RIGHT` chains of 3+ tables land** (b220): `_build_right_chain_pipeline` reverses an
  all-`RIGHT` chain into a `LEFT` chain driven from the last table (`(A RJ B) RJ C == C LJ B LJ A`),
  runs the existing lookup/unwind loop forcing `LEFT`, and rebuilds `amap` in FROM order for
  `SELECT *`. Sound only under an **adjacency guard** (`_on_referenced_aliases`): each `ON` must join
  its table to the immediately-prior FROM table, else re-association isn't valid → `0A000`. A chain
  mixing `LEFT`/`RIGHT`, a non-adjacent `RIGHT` `ON`, and an unqualified `ON` column stay `0A000`.
  **A leading `RIGHT`/`FULL` join + `INNER`/`LEFT` tail lands** (b225): `_build_leading_outer_join_pipeline`
  handles `A RIGHT|FULL JOIN B ON p1 [INNER|LEFT] JOIN C ON p2 …` — the leading outer join
  (`_outer_join_stages`, factored out of `_build_outer_join_pipeline`) builds the composite `(A⋈B)` as the
  driving *stream*, then each tail join runs the ordinary forward `$lookup`/`$unwind` (`_append_forward_join`,
  factored out of `_build_join_pipeline`'s loop) over it. Sound because the composite is only ever the
  driving side, never a `$lookup.from` / `$unionWith.coll`; for a leading FULL the tail runs after the
  anti-join `$unionWith` so it applies to both branches (the anti-branch already nests `b.<field>`, so the
  tail `ON` resolves identically and A's columns read NULL there). `amap` stays FROM-ordered (SELECT *
  correct); WHERE/GROUP BY unchanged.
  **A trailing `RIGHT` join over a two-table `INNER` composite lands** (b226): `_build_trailing_right_join_pipeline`
  handles `A JOIN B ON o1 RIGHT JOIN C ON o2` (`sides == ["", "RIGHT"]`) — the composite `A⋈B` sits on the
  *left* of the outer join, so it can't drive a `$lookup` and a *flat* forward reversal (`C LEFT JOIN pivot LEFT
  JOIN far`) is **unsound** (it leaks intermediate half-matches: a C row matching the pivot whose pivot row has no
  far match keeps the pivot's columns instead of the correct all-NULL pad). Sound lowering: drive from C, compute
  the INNER composite *atomically* with a nested `$lookup` — the outer lookup fetches the *pivot* (the composite
  table `o2` references) filtered by `o2`, and inside it a second `$lookup` fetches the *far* table filtered by `o1`
  with an INNER `$unwind` (a pivot row with no far match drops, exactly as `A⋈B` would); the outer `$unwind`
  preserves C (RIGHT side → unmatched C pads NULL). A/B are hoisted to alias-level keys so `_join_resolver` reads
  them as ordinary `join`-role aliases unchanged; `amap` stays FROM-ordered (SELECT * correct); WHERE/GROUP BY
  unchanged. Guarded to the provably-sound shape (qualified ON; `o1 ⊆ {A,B}`; `o2` references C + exactly one of
  A/B). The nested-composite lookup is factored into `_nested_composite_lookup` (+ `_trailing_composite_operands`
  for the shared resolve/validate) and reused by the FULL builder.
  **A trailing `FULL` join over a two-table `INNER` composite lands** (b227): `_build_trailing_full_join_pipeline`
  handles `A JOIN B ON o1 FULL JOIN C ON o2` (`sides == ["", "FULL"]`). `(A⋈B) FULL JOIN C` = `[(A⋈B) LEFT JOIN C]`
  ∪ `[C rows with no composite match, null-padded]`. The **main branch** is the ordinary forward pipeline (drive A,
  INNER `$lookup` B, LEFT `$lookup` C — C is a real collection, sound; roles A base, B/C nested). The **anti-branch**
  `$unionWith`s the C collection and reuses `_nested_composite_lookup` as an *emptiness test*: materialize the
  composite restricted by `o2`, keep only the C rows whose composite is `$size: 0` (a single-level pivot lookup
  could *not* detect this — a C matching a pivot whose pivot has no far match still belongs in the anti-branch), then
  `$replaceWith {c: $$ROOT}` so A/B read NULL. Both branches share the `{A base, B join, C join}` layout so the union
  lines up under one resolver. Same guards as b226 (via `_trailing_composite_operands`).
  **The trailing outer join now also covers a leading-`LEFT` composite** (b228): `A LEFT JOIN B ON o1 RIGHT|FULL
  JOIN C ON o2` (`sides == ["LEFT", "RIGHT"]` / `["LEFT", "FULL"]`) routes to the *same* builders. Insight: `A LEFT
  JOIN B` differs from `A INNER JOIN B` only in the `(a, NULL)` rows for `B`-less `a`s. When the outer `ON` targets
  the *non-driving* B (pivot=B), those NULL-`b` rows never satisfy `o2` on a B column, so the LEFT composite is
  **INNER-equivalent**; when it targets the *preserved base* A (pivot=A), those rows must survive. So the only change
  is the far `$unwind`'s preserve flag: `far_preserve = composite_is_LEFT and pivot == base_A` (threaded into
  `_nested_composite_lookup`), plus the FULL main branch's B-`$unwind` preserve = `composite_is_LEFT`. Everything else
  (nested lookup, RIGHT preserve, FULL anti-branch, resolver, guards) is unchanged.
  **The trailing outer join now also covers a THREE-table composite** (b229): `A [INNER|LEFT] JOIN B ON o1
  [INNER|LEFT] JOIN D ON o1b RIGHT|FULL JOIN C ON o2` (`sides == ["","","RIGHT"]` etc.; `_build_trailing3_composite_pipeline`,
  routed for `len(joins)==3` with the first two sides ∈ {INNER, LEFT} and the last RIGHT/FULL). Uses the **main ∪ anti**
  framing (proven exact): `(A⋈B⋈D) RIGHT JOIN C` = `[(A⋈B⋈D) INNER JOIN C] ∪ [C with no composite match]`, `FULL` =
  the same with the main C-join LEFT. **Main branch** = the ordinary *forward* pipeline (drive A, `$lookup` B then D
  honoring their INNER/LEFT sides, then C INNER for RIGHT / LEFT for FULL) — no reversal, so the composite is built at
  its natural root A and there is no half-match leak. **Anti branch** = `$unionWith` the C collection + a `$lookup`
  *from A* whose sub-pipeline rebuilds the *same forward composite* (via the shared `_emit_composite`) and `$match`es
  it by `o2`, keeping the C rows whose composite is `$size: 0`, then `$replaceWith {c: $$ROOT}`. Crucially the anti
  composite is rooted at **A, not re-rooted at the pivot** — re-rooting an all-INNER join is safe but re-rooting across
  a LEFT join changes the row set, so rooting at A is what makes this sound for **INNER *and* LEFT** composites at any
  pivot (A/B/D). The anti `$match` translates `o2` with `_OnTranslator(new_amap=composite)` (composite columns → their
  field paths, C columns → `$$` let vars) — a small reusable extension to `_OnTranslator`.
  **The trailing-composite builders are now unified into one `N`-table builder** (b230): `_build_trailing_composite_pipeline`
  replaces the four b226–b229 builders (and drops `_nested_composite_lookup` / `_trailing_composite_operands` / the
  `far_preserve` flag / the drive-from-C 2-table path). It handles a composite of **any** size (`joins[:-1]` are the
  leading INNER/LEFT joins, `joins[-1]` the trailing RIGHT/FULL) via the b229 main ∪ anti construction: forward main
  branch (`_emit_composite` loops the leading joins, then C joined INNER for RIGHT / LEFT for FULL), and a `$unionWith`
  anti branch that rebuilds the same forward composite in a `$lookup` from A and keeps the `$size:0` C rows. Because
  the composite is only ever built forward from A, the single-pivot restriction is gone — **`o2` may reference C plus
  any subset of the composite tables** (both the 2-table "spans both" case and 4/5-table composites now work). Routing
  collapsed to one guard: `all(s in ("","LEFT") for s in sides[:-1]) and sides[-1] in ("RIGHT","FULL")` (with
  `len==1` still the 2-table base case). The 2-table RIGHT plan now drives from A rather than C — identical rows, and
  `SELECT *` column order is unchanged (FROM-ordered `amap`). **Still `0A000`:** a composite whose own joins aren't
  adjacent (each leading `ON` must join its table to an already-known one), an unqualified/`o2`-doesn't-touch-composite
  ON, a non-plain-table source, a non-adjacent `RIGHT` `ON`, and a second `FULL` in the tail. **`CROSS JOIN` + comma-joins land** (b57): a join with no `ON` (`CROSS JOIN` or the implicit
  `FROM a, b` form) compiles to the cartesian product — an empty `$lookup` pipeline returns every
  foreign doc, then `$unwind` (no preserve) pairs each with the outer row; an outer join without `ON`
  is a `42601`. Non-equi (`a.x < b.y`) and `OR` join conditions already rode the `$lookup` `let`/
  `pipeline` form via `_OnTranslator` (the backlog was stale). **Scalar expressions *over* an
  aggregate** (`SUM(x) + 1`, `round(avg(x), 2)`) landed (b201, #167) via the evaluated group
  path. **Computed GROUP BY *keys*** landed (b203, #168): a non-column GROUP BY key
  (`GROUP BY lower(name)`, `x + 1`, `x % 2`, `coalesce(c, '?')`, `a || b`) is lowered to a Mongo
  aggregation expression, materialised into a synthetic `__gkeyN` field by a pre-`$group`
  `$addFields`, and every SQL-equal occurrence in SELECT / HAVING / ORDER BY is rewritten to that
  synthetic column — so the existing bare-column group machinery handles the rest
  (`_rewrite_computed_group_keys` + `_func_to_agg_expr` in the planner). **Extended to JOINs**
  (b204, #169): `GROUP BY lower(c.region)` / `o.amt + 1` across a join lower through the join
  resolver into a synthetic `__gkeyN` field (the rewrite core is factored into
  `_lower_computed_group_keys` / `_apply_group_key_rewrite`, shared with the single-table path).
  **Function-in-WHERE-vs-const also works** (b204): `WHERE upper(name) = 'X'` / `abs(x) = 3` lower
  through the same `_to_agg_expr` function branch into `$expr` (the field/const pair-check falls
  through for a function operand). Keys using a function the aggregation engine can't evaluate
  (e.g. `substr`) stay `0A000`. **Computed keys over GROUPING SETS / ROLLUP / CUBE also work**
  (b205, #170): `_computed_group_keys` collects keys across the rollup/cube/grouping-sets wrappers
  and `_plan_grouping_sets_select` injects the synthetic-key `$addFields` into the base pipeline
  *and* every `$unionWith` branch (each reads the collection fresh); `GROUPING(lower(x))` works via
  the same rewrite. Still `0A000`:
  a mixed `LEFT`/`RIGHT` or any `FULL` 3+ table chain (a *pure*-`RIGHT` adjacent chain works, b220),
  subqueries. SUM/MIN/MAX
  result typing is approximate (uses the column's tag; AVG → float8; arithmetic → numeric).
  **`DISTINCT` aggregates landed** (b48): `COUNT`/`SUM`/`AVG(DISTINCT col)` compile to a
  `$addToSet` accumulator plus a post-`$group` `$addFields` that reduces the set
  (`_distinct_reduction`: `$size`+`$filter` for count, `$reduce` for sum, `$reduce`/`$size`+`$cond`
  for avg; NULLs filtered to match SQL). `MIN`/`MAX(DISTINCT)` run the ordinary accumulator (a set's
  extremum equals the raw extremum). Wired in both the single-table (`_plan_group_select`) and
  join (`_plan_join_group_select`) paths via the shared `_register_distinct_agg`; `_aggregate_of` /
  `_join_aggregate_of` now return `(func, col, distinct)`. **DISTINCT in HAVING landed (single-table,
  #166, b201):** `_having_to_match` takes the `names` / `reductions` allocators and, for a `count`/`sum`/
  `avg(DISTINCT …)` term, calls `_register_distinct_agg` (reusing the SELECT-list registration when the
  same aggregate already appears there) — the reduction `$addFields` runs before the HAVING `$match`, so
  the match references the reduced field. **JOIN DISTINCT-in-HAVING landed (#167, b202):**
  `_join_having_to_match` now takes the same `names` / `reductions` allocators (in scope at both join
  call sites) and registers a distinct set the same way, so `count(DISTINCT b.col)` in a JOIN+GROUP
  HAVING works too.
  `string_agg` + the boolean aggregates landed in b121: `bool_and`/`bool_or` (registered in
  `_AGG_CLASSES`) lower to `$min`/`$max` over booleans, `every(x)` is recognised as `bool_and` in
  `_aggregate_of`; `string_agg(expr, sep)` (`exp.GroupConcat`) lowers to a `$push` accumulator plus a
  `$reduce` in the group `$project` (`_string_agg_project`) that joins the pushed array skipping NULL
  elements (NULL when all-NULL) — wired into the single-table, join, and grouping-set planners with the
  routing predicates updated. Tests: `tests/test_sql_string_agg.py`.
- [x] **Regex / string scalar functions landed** (b129): `regexp_replace(src, pat, repl [,flags])`
  (Python `re.sub`; `g` flag → global, `i`/`m`/`s`/`x` supported; PG `\&` whole-match → Python `\g<0>`,
  `\1`–`\9` pass through), `split_part(str, delim, n)` (1-based; negative counts from the end (PG14+);
  out-of-range → `''`), `translate(str, from, to)` (per-char map; extra `from` chars deleted),
  `regexp_count(str, pat)`, and `regexp_matches(str, pat [,flags])` — all in `scalar.py`
  (`_SCALAR_FUNC_NODES` for the dedicated nodes `RegexpReplace`/`SplitPart`/`Translate`/`RegexpCount`;
  `regexp_matches` is `Anonymous` → `_call_func`). Output types wired in `planner._infer_scalar_tag`
  (text / int4). **`regexp_matches` as a true SRF landed (#152, b190):** it's registered in `srf.py`'s
  `_NAMED_SRFS`, so `SELECT regexp_matches(…)` and `FROM regexp_matches(…) AS m` emit **one row per
  match** (each a `text[]` of the capture groups, or the whole match when there are none); the `g` flag
  yields every match, without it at most the first, and no match / NULL input yields no rows. The scalar
  path (`scalar.py`) is retained for a `regexp_matches` nested inside a larger expression or appearing
  alongside other projections (multi-target-list), where it still returns the first match's `text[]`.
- [x] **Math / numeric scalar functions landed** (b130): `trunc(x [,n])` (truncate toward zero;
  numeric), `sqrt` / `cbrt` (real cube root via `copysign` so negatives work), `sign` (−1/0/1,
  operand kind preserved), `ln`, `log(x)` (base-10 in PG) / `log(b, x)` / `log10` (base-10/2 use the
  exact `math.log10`/`log2` so `log10(1000) == 3.0`), `exp`, `pi()`, `degrees`, `radians`,
  `factorial`, plus `gcd` / `lcm` — in `scalar.py` (`_SCALAR_FUNC_NODES` for the dedicated nodes,
  registered via the version-tolerant getattr loop; `gcd`/`lcm`/`log10` fall through `_call_func`).
  Output types wired in `planner._infer_scalar_tag` (float8 for the transcendental/root funcs,
  numeric for trunc/sign/factorial, int8 for gcd/lcm). `mod` / `power` / `abs` / `ceil` / `floor` /
  `round` were already present.
- [x] **Composite types landed** (b131): `CREATE TYPE name AS (field type, …)` stores an ordered
  `(field, type_tag)` list in the `__sql_composites__` catalog collection (`Catalog.create_composite`
  / `get_composite` / `composite_exists` / `drop_composite` / `list_composites`). A composite-typed
  column carries `composite_type` + `composite_fields` on its `Column` (resolved from the type at
  CREATE TABLE via `executor._resolve_user_type_column`, `type_tag = "composite"`) and stores a
  subdocument. Write: `ROW(a, b)` → `planner._literal` returns a positional list, `_composite_value`
  maps it onto the named fields (coercing each). Read: `(col).field` (a `Dot(Paren(Column),
  Identifier)`) evaluates in `scalar.py` to the subdoc field, typed via `planner._composite_field_tag`
  (the resolver stashes its `TableDef` on `resolve.table`). Whole-composite selects render the PG
  record text literal `(f1,f2)` (`typemap._render_pg_composite`, RECORD oid 2249). Reflected via
  `pg_type` (`typtype = 'c'`, oid base 67000 in `virtual._composite_oids`). WHERE/UPDATE access and
  `pg_attribute` reflection landed in b134 (below). **Remaining limitation:** no nested composites.
- [x] **Composite type follow-ups landed** (b134): closed the b131 gaps. (1) `(col).field` in a WHERE
  predicate — `planner._composite_access_parts` (Paren-gated `Dot(Paren(Column), Identifier)` so a
  schema-qualified `pg_catalog.x` Dot is never misread) feeds `_field`/`_is_field_node`, lowering to a
  dotted Mongo path `col.field`. (2) UPDATE targets — `SET col.field = v` (`_composite_subfield_target`
  detects `Column(this=field, table=col)`, writes `$set: {"col.field": v}` coerced to the field's tag)
  and `SET col = ROW(...)` (whole value via `_composite_value`); an unknown subfield → 42703. (3)
  `pg_attribute` field reflection — added `typrelid` to pg_type + a `relkind='c'` pg_class row per type
  (`virtual._composite_rel_oids`, oid base 68000) with `reltype` pointing back, plus one pg_attribute
  row per field, so `pg_type.typrelid → pg_class.oid → pg_attribute.attrelid` resolves field names /
  oids (`psql \dT+`, SQLAlchemy). Added the `typrelid` column to the pg_type schema and `reltype` to
  pg_class.
- [x] **Nested composite types landed** (b139): a composite type whose own field is another composite,
  closing the b134 gap. Composite field entries became recursive 3-tuples `(name, tag, subfields)` —
  `subfields` is None for a scalar field or the referenced type's fields for a composite field (embedded
  at `CREATE TYPE` time by `engine._composite_fields_from_schema`, which now takes `catalog`/`db` and
  resolves a non-builtin field type as a composite; a direct self-reference raises `0A000`). Catalog
  ser/deser recurse (`catalog._ser_composite_fields` / `_deser_composite_fields`, backward-compatible
  with legacy 2-tuples). `planner._composite_walk` resolves arbitrary-depth access `((p).home).street`
  to a dotted path + tag (a composite field types as `composite` so it renders as a record; deeper walks
  key off `subfields`), driving `_composite_field_tag` / `_field` / `_is_field_node` (a new
  `_is_composite_access_shape` accepts the nested shape). `planner._build_composite` recurses to build
  nested subdocs from nested `ROW(...)` on INSERT and `SET col.field = ROW(...)` on UPDATE. The scalar
  evaluator already walked nested access recursively (it reads the data). `typemap._render_pg_composite`
  renders a dict-valued field as a nested `(…)` record (quoted/escaped when embedded).
  `virtual._pg_attribute` points a composite field at its subtype's composite oid. Tests:
  `tests/test_sql_nested_composite.py` (16, incl. three-level nesting, nested WHERE/UPDATE, reflection)
  plus a pg8000 wire test. **Limitation:** pg8000's own record parser mis-splits a *doubly*-nested
  anonymous record on the wire (a client-side limitation — the emitted text is byte-exact Postgres);
  single-level composite fields decode cleanly.
- [x] **Statistical + bitwise aggregates landed** (b136): `stddev`/`stddev_samp`/`stddev_pop`,
  `variance`/`var_samp`(=variance)/`var_pop`, and `bit_and`/`bit_or`/`bit_xor` (`every` already aliased
  `bool_and`). Added the dedicated sqlglot nodes to `planner._AGG_CLASSES` (via a version-tolerant
  getattr loop). stddev lowers to Mongo's native `$stdDevSamp`/`$stdDevPop` accumulators (newly
  implemented in the pure-Python engine: `aggregate._acc_std_pop`/`_acc_std_samp` + `_std_dev` in
  `_finalize` — pop is null for empty / 0 for one value, samp is null for <2). variance and the bit
  folds run through the `post_aggregates` path: variance accumulates the matching stdDev then squares it
  (`executor._stat_bit_value`, kind `variance`); `bit_*` `$push` the values and fold with
  `operator.and_`/`or_`/`xor_` (NULLs skipped, NULL for an empty group). Typed float8 (stddev) / numeric
  (variance) / int (bit). Routed through `_plan_group_select` for both grouped and whole-table single-table
  aggregates. **Now wired into the JOIN group path too** (b204, #169): `variance` / `var_pop` / `bit_and`
  / `bit_or` / `bit_xor` over a JOIN build the same `post_aggregates` finish (resolved through the join
  resolver), and `every()` is recognised as `bool_and` in `_join_aggregate_of`. **Limitations:**
  `every()`/`bool_and` still require a boolean **column** argument, not a boolean expression; a
  whole-table aggregate over an **empty** table returns no row (pre-existing, except `count`).
- [x] **Range types landed** (b137): `int4range`/`int8range`/`numrange`/`tsrange`/`daterange`. A new
  self-contained `secantus/sql/ranges.py` (build/parse/render/compare) stores a range as a subdocument
  `{"lower","upper","lower_inc","upper_inc"}` (or `{"empty": true}`); discrete types canonicalise to the
  half-open `[)` form (`(1,10]` → `[2,11)`). Wired through every layer: `typemap` (PG OIDs 3904/3906/3908/
  3912/3926, `_RANGE_TAGS`, `to_pg_text` → range text, `coerce` → parse literal); `scalar._call_func`
  (constructors, `isempty`, range-aware `lower`/`upper`) + `scalar._eval_range_op` (the `@>`/`<@`/`&&`
  operators, which sqlglot parses as `exp.ArrayContainsAll`/`ArrayContainedBy`/`ArrayOverlaps` — a
  `_NOT_RANGE` sentinel defers non-range operands to the existing jsonb/array containment path);
  `planner` (`_literal` builds the constructor subdoc; `_infer_scalar_tag` types constructor/cast → range
  tag, `@>`/`<@`/`&&` over a range operand → bool via `_has_range_operand`, `lower`/`upper` over a range →
  element tag, `isempty` → bool; `where_needs_per_row(stmt, table)` + `_where_has_range_predicate` route a
  range-operator WHERE to the per-row scalar path since the operators don't lower to a Mongo filter);
  `functions.is_scalar_function` excludes range constructors + `isempty` so a FROM-less `SELECT int4range(…)`
  / `isempty(…)` falls through to the full scalar evaluator; reflected via `pg_type` with `typtype = 'r'`.
  Tests: `tests/test_sql_ranges.py` (28: pure-module canonicalisation/contains/overlaps/parse/render +
  SQL surface) and a pg8000 wire test. **Limitations:** the `@>`/`<@`/`&&` operators run a COLLSCAN
  per-row (no lowered Mongo filter / index); multirange types, range GiST indexes, and the extra range
  functions are unimplemented (**range algebra + multirange landed b140, below**).
- [x] **Range algebra + multirange landed** (b140): the range set operators and the `range_agg` aggregate
  + multirange types (`int4multirange`/`int8multirange`/`nummultirange`/`tsmultirange`/`datemultirange`).
  `ranges.py` gained `merge` (range_merge, spans gaps), `intersect` (`*`), `union` (`+`, raises if
  non-contiguous), `difference` (`-`, raises if it splits), `adjacent` (`-|-`), and a multirange layer
  (`make_multirange` coalesces sorted/overlapping/adjacent members into `{"multirange": [range, …]}`,
  `render_multirange` → `{[a,b), …}`, `parse_multirange`, `RANGE_TO_MULTIRANGE`). `scalar._eval_arith`
  dispatches `Mul`/`Add`/`Sub` to intersect/union/difference when both operands are ranges (`_is_range_value`);
  `exp.Adjacent` (`-|-`) → `ranges.adjacent`; `_call_func` gains multirange constructors + `range_merge`;
  `_eval_cast` parses multirange literals. `typemap` adds `_MULTIRANGE_TAGS` (OIDs 4451/4532/4533/4535/4536),
  `to_pg_text` → `render_multirange`, `coerce` → `parse_multirange`, and multirange names to
  `type_tag_for_sql` / `SQL_TYPE_NAME` / `PG_TYPENAME`. `planner._infer_scalar_tag` types the operators →
  range tag, adjacency → bool, `range_merge` → range tag (`_range_tag_of`), multirange constructor/cast →
  multirange tag; `_literal` builds a multirange constructor subdoc. `range_agg` (`_range_agg_arg`) pushes
  the group's ranges then a `range_agg` post-aggregate (`executor._apply_post_aggregates`) coalesces them
  into a multirange (typed via `_multirange_tag_for_arg`); wired into the detection sites + the primary
  single-table group path. `functions.is_scalar_function` excludes multirange constructors + `range_merge`.
  Tests: `tests/test_sql_range_agg.py` (27: pure algebra + SQL surface) and a pg8000 wire test.
  **Multirange containment/overlap operators landed (#166, b201):** `@>` / `<@` / `&&` now accept
  multirange operands (any mix of range / multirange / scalar element) via `ranges.contains_any` /
  `overlaps_any` (fold over `multirange_members`; a range is contained iff a single member covers it,
  members being disjoint + non-adjacent), dispatched from the generalised `scalar._eval_range_op`. The
  WHERE path forces a COLLSCAN + residual for multirange predicates too (`planner._where_has_range_predicate`
  now unions `_RANGE_TAGS | _MULTIRANGE_TAGS`). Tests: `tests/test_sql_multirange_ops.py`. **Not yet:**
  `range_intersect_agg`, multirange extraction functions, range_agg over a JOIN or GROUPING SETS, and
  range GiST indexes.
- [x] **Full-text search landed** (b141): `tsvector` / `tsquery` types + `to_tsvector` / `to_tsquery` /
  `plainto_tsquery`, the `@@` match operator, and `ts_rank`. New self-contained `secantus/sql/fts.py`:
  `to_tsvector` → `{"tsvector": {lexeme: [pos, …]}}` (lower-cased tokens, English stop-words dropped, 1-based
  positions); `to_tsquery` (a recursive-descent parser over `& | !` + parens) / `plainto_tsquery` →
  `{"tsquery": <node>}` boolean tree; `matches` evaluates the tree against the vector's lexeme set;
  `ts_rank` is a log-dampened match-count (monotonic so `ORDER BY ts_rank(...) DESC` works). Wired through
  `typemap` (OIDs tsvector 3614 / tsquery 3615, `_FTS_TAGS`, `to_pg_text` → `render_tsvector` /
  `render_tsquery`, `coerce` → parse, names in `type_tag_for_sql` / `SQL_TYPE_NAME` / `PG_TYPENAME`);
  `scalar._call_func` (the builders + `ts_rank` / `ts_rank_cd`) and `scalar._eval_fts_match` (the `@@`
  operator — `exp.MatchAgainst`, shared with jsonb `@@`; a `_NOT_FTS` sentinel defers non-FTS operands to the
  jsonb-path predicate); `scalar._eval_cast` parses FTS literals; `planner._literal` builds the constructor
  subdoc, `_infer_scalar_tag` types the builders (tsvector/tsquery) + `ts_rank` (float8) — `@@` already types
  bool. `where_needs_per_row` routes a `@@` WHERE to the per-row scalar path, and `_build_evaluated_single`
  carries a non-lowerable WHERE (FTS `@@` / range op) as a per-row residual (`EvaluatedSelectPlan.where`) so
  `WHERE @@ … ORDER BY ts_rank(…)` works. `functions.is_scalar_function` excludes the FTS builders (FROM-less).
  Tests: `tests/test_sql_fts.py` (19: pure fts + SQL surface) plus a pg8000 wire test. **Simplifications:**
  fixed english config, **no stemming** (`cats` ≠ `cat`), `ts_rank` is a match-count not cover-density;
  weights (`:A` / `setweight`), prefix (`cat:*`), phrase (`<->`), `ts_headline`, and GIN/GiST FTS indexes
  are out of scope.
- [x] **Network address types landed** (b142): `inet` / `cidr` / `macaddr` types, the `<<` / `>>` / `&&`
  subnet-containment/overlap operators, and the `host` / `masklen` / `network` / `netmask` / `broadcast` /
  `abbrev` / `family` / `hostmask` accessor functions. New self-contained `secantus/sql/net.py` stores values
  as canonical text (`inet`/`cidr` as `addr/masklen`, `macaddr` as `xx:xx:xx:xx:xx:xx`) and parses with
  Python's `ipaddress` at operator time; `contains`/`overlaps` compare networks (different families never
  match). Wired through `typemap` (OIDs inet 869 / cidr 650 / macaddr 829, `_NET_TAGS`, `coerce` → normalise,
  `to_pg_text` → `render_inet` drops a full-host `/32`|`/128`, names in `type_tag_for_sql` / `SQL_TYPE_NAME` /
  `PG_TYPENAME`); `scalar._eval_net_op` (`<<` → contains(right,left), `>>` → contains(left,right), `&&` →
  overlaps; a `_NOT_NET` sentinel + `_is_net_value` defers non-net operands so `<<`/`>>` still act as the
  integer bit-shift on ints and `&&` as array-overlap on arrays), an `exp.Host` branch, net funcs in
  `_call_func`, `_NET_TAGS` in `_eval_cast`; `planner._where_has_net_predicate` routes a net-op WHERE to the
  per-row scalar path, `_infer_scalar_tag` types the operators bool / `host`,`abbrev` text / `masklen`,`family`
  int4 / `network` cidr / `netmask`,`broadcast`,`hostmask` inet / net casts to their tag. Tests:
  `tests/test_sql_net.py` (29: pure net + SQL surface) plus a pg8000 wire test. **Simplifications:** the `<<=`
  / `>>=` (contain-or-equal) operators aren't parsed by sqlglot, and `inet ± int` arithmetic, `macaddr8`, and
  GiST network indexes are out of scope.
- [x] **Bit-string types landed** (b143): `bit(n)` / `varbit` types, `B'…'` literals, the bitwise operators
  `&` / `|` / `#` / `~` / `<<` / `>>`, `||` concat, and `length` / `bit_length` / `octet_length` / `get_bit` /
  `set_bit` plus `int`↔`bit` casts. New self-contained `secantus/sql/bitstr.py` stores values as a canonical
  '0'/'1' string; `normalize` pads/truncates, `from_int`/`to_int`, `band`/`bor`/`bxor`/`bnot`,
  `shift_left`/`shift_right` (width-preserving), `get_bit`/`set_bit` (left-indexed), `is_bit_value` (operand
  disambiguation). Wired through `typemap` (OIDs bit 1560 / varbit 1562, `_BIT_TAGS`, `BIT` in `_DATATYPE_TAGS`,
  `varbit`/`bit varying` name-match, `coerce` → normalise, `to_pg_text` → the string, names in `SQL_TYPE_NAME`
  / `PG_TYPENAME`); `scalar` (`exp.BitString` literal, `_eval_bitwise` with a `_NOT_BIT` sentinel — overloaded
  across bit strings and integers, net-first for `<<`/`>>`; `exp.Getbit` + `exp.BitLength` nodes; `_eval_cast`
  bit↔int with `_bit_cast_length` + `_is_bit_expr`; `set_bit`/`get_bit`/`bit_length`/`octet_length` in
  `_call_func`); `planner` (`_literal` BitString, `_has_bit_operand`, `_infer_scalar_tag` types operators →
  varbit / int bitwise → int4 / bit funcs / casts, `_where_has_bit_predicate` routes a bit-op WHERE per-row);
  `functions._SCALAR_EVAL_ANON` for FROM-less bit funcs. Tests: `tests/test_sql_bitstr.py` (24: pure bitstr +
  SQL surface) plus a pg8000 wire test. **Simplifications:** a `bit(n)` *column* isn't padded to `n` on insert
  (declared length not tracked at storage — only explicit `::bit(n)` casts pad); a stored bit column can't be
  re-read via `::int` (only a `B'…'` literal or `::bit` cast is treated as a bit source); bit indexes are out
  of scope.
- [x] **Interval type landed** (b144): the `interval` type (stored as `{"interval": {"months", "days",
  "micros"}}`), interval literals (`interval '1 day'` / `'1 year 2 mons 3 days 04:05:06'` / `interval '1'
  day`), interval/date arithmetic (`interval ± interval`, `interval * n`, `date ± interval`, `timestamp -
  timestamp -> interval`, unary `-`), and the functions `make_interval` / `justify_days` / `justify_hours` /
  `justify_interval` / `age` plus `extract(field from interval)`. New self-contained `secantus/sql/intervals.py`
  (parse / render in the Postgres output style / add / sub / neg / mul / justify* / to_date / diff / age /
  extract_field). The old scalar `_Interval` class was retired in favour of the subdoc. Wired through `typemap`
  (OID 1186, `INTERVAL` in `_DATATYPE_TAGS`, name-match, `coerce` → parse, `to_pg_text` → render, names in
  `SQL_TYPE_NAME` / `PG_TYPENAME`); `scalar` (`_eval_interval` → subdoc, `_eval_interval_arith` with a
  `_NOT_INTERVAL` sentinel, Neg-of-interval, `exp.MakeInterval` / `exp.Justify*` typed nodes, interval funcs in
  `_call_func`, `extract` on an interval, and a `timestamp '…'` cast now coerces to a real `datetime` so date
  arithmetic lands); `planner` (`_literal` Interval, `_infer_value_tag` / `_infer_scalar_tag` typing incl.
  `_interval_arith_tag`, a `::timestamp` cast now types `timestamptz`); `functions._SCALAR_EVAL_ANON` for
  FROM-less interval funcs. Tests: `tests/test_sql_interval.py` (22: pure intervals + SQL surface) plus a
  pg8000 wire test. **Simplifications:** days are treated as 24h (no DST-aware arithmetic), the verbose `@`
  input grammar (beyond a trailing `ago`) isn't parsed, and interval indexes are out of scope.
- [x] **Full-text search follow-ups landed** (b145): prefix (`cat:*`), phrase (`foo <-> bar` / `foo <N> bar`),
  `phraseto_tsquery`, and `ts_headline` added to `secantus/sql/fts.py`. The `_TSQueryParser` gained a
  `_parse_phrase` level (phrase binds between `&` and factor) and a `:*` prefix suffix; `matches` / `ts_rank`
  now walk token positions (`_end_positions` / `_phrase_positions` / `_count_hits`) so adjacency queries work
  (positions were already stored by `to_tsvector`). `phraseto_tsquery` chains a text's non-stop-word lexemes
  with `<->` (a dropped stop-word widens the distance). `ts_headline(document, query)` wraps matched
  lexeme/prefix tokens in `<b>…</b>`. Wired through `scalar._call_func` (`phraseto_tsquery` + `ts_headline`),
  `planner._literal` / `_infer_scalar_tag` (phraseto → tsquery, ts_headline → text), and
  `functions._SCALAR_EVAL_ANON`. `@@` WHERE routing (per-row via `MatchAgainst`) already covers phrase/prefix
  queries. Tests: `tests/test_sql_fts.py` (+14, now 33) plus a pg8000 wire test. **Simplifications:**
  `ts_headline` returns the whole document (no fragment windowing), and lexeme weights (`:A` / `setweight` /
  weighted `ts_rank`) remain out of scope (the tsvector stores no per-lexeme weight).
- [x] **UUID type landed** (b146): the `uuid` type + `gen_random_uuid()` / `uuid_generate_v4()` /
  `uuid_generate_v1()` generators, uuid literals / casts (hyphenated, bare-hex, and `{braced}` forms all
  canonicalise to the lower-case hyphenated string), and equality / ordering that lower to a Mongo filter (no
  per-row eval — the value stores as its canonical string). New self-contained `secantus/sql/uuidtype.py`
  (`normalize` / `generate` / `is_uuid_value`; named `uuidtype` so it doesn't shadow stdlib `uuid`). Wired
  through `typemap` (OID 2950, `UUID` in `_DATATYPE_TAGS`, `coerce` → normalise, names in `SQL_TYPE_NAME` /
  `PG_TYPENAME`; `to_pg_text` needs no branch — the canonical string falls through the default encoder);
  `scalar` (`exp.Uuid` node → generate, `_eval_cast` uuid → normalise, `gen_random_uuid` / `uuid_generate_v4`
  / `uuid_generate_v1` in `_call_func`); `planner` (`_literal` for `exp.Uuid` + the `uuid_generate_*`
  anonymous, `_infer_scalar_tag` types `exp.Uuid` / uuid casts / the generators as uuid);
  `functions._SCALAR_EVAL_ANON` for FROM-less generators. Tests: `tests/test_sql_uuid.py` (15: pure uuidtype +
  SQL surface) plus a pg8000 wire test. **Simplifications:** only v4 (random) UUIDs are generated
  (`uuid_generate_v1` returns a v4, not a real time-based v1); no `uuid-ossp` namespace functions
  (`uuid_generate_v3` / `v5`).
- [x] **date / time / timetz distinct types landed** (b147): `date` / `time` / `timetz` are now distinct
  types (previously `date` collapsed to `timestamptz`), reporting the correct wire OIDs (1082 / 1083 / 1266)
  for driver/ORM reflection. New self-contained `secantus/sql/datetimes.py` stores them as canonical text
  (`date` `YYYY-MM-DD`, `time` `HH:MM:SS[.ffffff]`, `timetz` with an offset) — BSON has no date-only /
  time-only value, and ISO text orders/compares the same as the value (so equality / `ORDER BY` lower to a
  Mongo filter) and is distinguishable at eval time from a `datetime` (`timestamptz`). Arithmetic:
  `date - date -> int4` (days), `date ± int -> date`, `date ± interval -> timestamptz`, `time - time ->
  interval` (`scalar._eval_date_arith` with a `_NOT_DATE` sentinel, run before the interval/numeric
  fallbacks). Wired through `typemap` (OIDs, `DATE`/`TIME` in `_DATATYPE_TAGS` + `timetz` name-match,
  `coerce` → parse; `to_pg_text` needs no branch — the canonical string / `datetime.date` fall through the
  default encoder), `scalar` (`_eval_cast` date/time/timetz, `current_time` → `_fmt_current_time`,
  `_eval_date_arith` + `_time_sub_time`), `planner` (`_infer_scalar_tag` types casts / `current_date` → date /
  `current_time` → timetz / `_date_arith_tag`). `current_date` still returns a `datetime.date` (kept for the
  existing `test_current_date`). Tests: `tests/test_sql_datetime_types.py` (20: pure datetimes + SQL surface)
  plus a pg8000 wire test. **Simplifications:** `time(p)` precision isn't rounded, `timetz` preserves the
  literal's offset without converting, and mixing a bare `timestamp` with a `date` in one arithmetic
  expression isn't supported (cast one side).
- [x] **money type + to_char numeric formatting landed** (b148): the `money` type (OID 790, stored as a
  2-decimal `Decimal128`, rendered `$1,234.56`) with literals / casts (`'$1,234.56'::money`, `(1234.56)` →
  negative) and arithmetic (`money ± money` / `money * n` → money, `money / money` → float8); plus numeric
  `to_char(numeric, fmt)` supporting `9` / `0` / `.` / `,` / `$` / `L` / `S` / `MI` / `PR` / `FM`. New
  self-contained `secantus/sql/numformat.py` (`parse_money` / `render_money` / `to_char_numeric`). Wired
  through `typemap` (OID, `MONEY` in `_DATATYPE_TAGS`, `coerce` → `Decimal128`, `to_pg_text` → `render_money`),
  `scalar` (`_eval_cast` money, `_eval_to_char` routes a numeric source to `to_char_numeric`), and `planner`
  (`_infer_scalar_tag` money cast + money-arith typing). Also **fixed a pre-existing bug**: a stored
  `numeric`/`money` value is a `Decimal128` with no Python arithmetic/comparison operators, so computed
  `numeric_col + numeric_col` / comparisons in the scalar path raised `TypeError` — `scalar._unwrap_decimal`
  now unwraps `Decimal128` → `Decimal` at the `_eval_arith` / `_eval_compare` boundary. Tests:
  `tests/test_sql_money.py` (16: pure numformat + SQL surface) plus a pg8000 wire test. **Simplifications:**
  `$`-only currency (no locale), `to_char` omits `EEEE` / `RN` / `V` / `TH` / non-ASCII locale patterns, and
  `ORDER BY` on a money/decimal *column* relies on the storage engine's sort (the in-memory `FakeStorage`
  can't compare raw `Decimal128`).
- [x] **Geometric types landed** (b149): the core Postgres geometric types `point` / `box` / `circle` /
  `polygon` / `lseg` (and the `line` / `path` spellings — stored/canonicalised but not operated on), stored as
  their canonical Postgres text (`(1,2)`, `(2,2),(0,0)`, `<(0,0),5>`, `((0,0),(1,0),(1,1))`, `[(0,0),(1,1)]`),
  with the operators `<->` (distance → float8), `@>` (contains) / `<@` (contained by) / `&&` (overlaps) → bool.
  New self-contained `secantus/sql/pggeo.py` (`canonical` / `to_shapely` / `distance` / `contains` / `overlaps` /
  `is_geo_text`); geometry math delegates to Shapely (a `circle` = centre point buffered by radius). Wired through
  `typemap` (geo OIDs 600/601/602/603/604/628/718, `_GEO_TAGS` frozenset, `POINT` in `_DATATYPE_TAGS`,
  name-match `type_tag_for_sql`, `coerce` → `pggeo.canonical`), `scalar` (`_eval_cast` geo branch, `exp.Distance`
  → `pggeo.distance`, `_eval_geo_op` for `@>` / `<@` / `&&` disambiguated by `is_geo_text` value-shape since the
  shared `ArrayContains*` nodes carry no operand types), and `planner` (`_infer_scalar_tag` distance→float8 /
  geo-op→bool, `_has_geo_operand`, and `where_needs_per_row` / `_where_has_geo_predicate` routing the containment/
  overlap/distance predicates through the per-row scalar path since they can't lower to a Mongo `$match`). Tests:
  `tests/test_sql_geo.py` (19: pure pggeo + SQL surface) plus a pg8000 wire test. **Out of scope:** the infinite
  `line` type and open/closed `path` distinction for operators, the `#` / `##` / `?-` / `?|` positional operators,
  and geometric indexes.
- [x] **bytea functions + literal forms landed** (b150): the `bytea` binary type already round-tripped (OID 17,
  `coerce` → `bson.Binary`, `to_py` → `bytes`, `to_pg_text` → the `\x…` hex form); this slice adds the literal
  parsing and function surface. New self-contained `secantus/sql/bytea.py` (`parse` — hex `\x…` **and** escape form;
  `encode` / `decode` for `hex` / `base64` / `escape`; `get_byte` / `set_byte`; `concat`). Wired through `typemap`
  (`coerce` bytea → `bytea.parse`), `scalar` (`_eval_cast` bytea branch, `exp.Encode`/`exp.Decode` typed-node
  handlers reading `charset`, `get_byte`/`set_byte` in `_call_func`, `bytea`-aware `octet_length`/`bit_length`/
  `exp.Length`/`exp.BitLength`, and `bytea || bytea` byte concat in the `DPipe` handler), `functions` (`get_byte`/
  `set_byte` added to `_SCALAR_EVAL_ANON` so a FROM-less `SELECT get_byte(…)` defers to the full evaluator rather
  than the session-function literal path), and `planner` (`_infer_scalar_tag`: `encode`→text, `decode`→bytea,
  `get_byte`→int4, `set_byte`→bytea, cast→bytea, `DPipe` over a bytea operand→bytea via `_has_bytea_operand`).
  Tests: `tests/test_sql_bytea.py` (27: pure bytea + SQL surface) plus a pg8000 wire test. **Out of scope:** the
  `bytea_output = escape` server setting (output is always hex) and the digest functions (`md5`/`sha256`, crypto
  extensions).
- [x] **hstore key/value type landed** (b153): the `hstore` contrib type — a flat string→string map (NULL values
  allowed), stored as a **tagged** subdocument `{"hstore": {…}}` so it stays distinct from a plain `jsonb` object
  (the `->` / `@>` / `<@` / `?` / `?&` / `?|` / `||` operators are all spelled the same as jsonb's). New
  self-contained `secantus/sql/hstore.py` (`parse` / `render` / `is_hstore` / `as_map`; the operators
  `contains` / `contained_by` / `exists` / `exists_all` / `exists_any` / `lookup` / `merge` / `delete` / `defined`;
  `akeys` / `avals` / `to_json` / `from_pair` / `from_arrays`). Wired through `typemap` (placeholder OID 16935 —
  **deliberately not in PG_TYPENAME** so `to_regtype('hstore')` stays NULL and SQLAlchemy's psycopg connect-probe is
  a no-op; `HSTORE` in `_DATATYPE_TAGS`; `coerce` → `hstore.parse`; `to_pg_text` → `hstore.render`), `scalar`
  (`_eval_cast` hstore branch; `_eval_hstore_op` for `@>`/`<@` and `_eval_hstore_exists` for `?`/`?&`/`?|`
  disambiguated on the hstore tag, chained after the geo check; hstore lookup in `_eval_jsonb_nav` for `->`; hstore
  merge in the `DPipe` handler; the functions in `_call_func`), `functions` (hstore function names in
  `_SCALAR_EVAL_ANON`), and `planner` (`_infer_scalar_tag` hstore-op typing via `_has_hstore_operand`;
  `where_needs_per_row` + `_where_has_hstore_predicate` routing `@>`/`<@`/`?`/`?&`/`?|` per-row; **`_field` maps an
  `hstore -> key` to the dotted `<col>.hstore.<key>` path so the `->` lookup pushes down** as a plain projection /
  filter). Tests: `tests/test_sql_hstore.py` (25: pure hstore + SQL surface) plus a pg8000 wire test. **Out of
  scope:** the set-returning `each`/`skeys`/`svals` forms (the `akeys`/`avals` arrays cover the need), GiST/GIN
  indexing, and the `#=`/`%%`/`%#` record operators.
- [x] **citext case-insensitive text landed** (b154): the `citext` contrib type — text stored verbatim (case
  preserved for display) but compared / sorted case-insensitively. The case-folding is a **query-planner behaviour**,
  not a value shape (a citext value is an ordinary string, so it can't be tagged like hstore without breaking text
  rendering) — so it's driven off the column's `type_tag == "citext"`. Wired through `typemap` (OID 25 / the text OID
  so drivers read it as text; `citext` in `SQL_TYPE_NAME`; name-match in `type_tag_for_sql`; `coerce` → `str`; **not**
  in `PG_TYPENAME`), `planner` (`_citext_cmp_filter` lowers `=`/`<>`/`<`/`<=`/`>`/`>=` to `{$expr: {op: [{$toLower:
  "$field"}, lower(value)]}}`; `IN` / `BETWEEN` fold likewise; `LIKE` on citext adds the `i` regex option
  (= `ILIKE`); citext cast typing; `_citext_order_set` collects citext ORDER BY field paths onto `SelectPlan`), and
  `executor` (`_order_key_fn` folds those fields' string values to lower case for a case-insensitive sort). Tests:
  `tests/test_sql_citext.py` (11) plus a pg8000 wire test. **Simplification (documented divergence):** `GROUP BY` /
  `SELECT DISTINCT` on a citext column group case-**sensitively** (`Alice` ≠ `alice`), unlike real Postgres — the
  dominant citext uses (case-insensitive equality / uniqueness / range / sort) are faithful, but case-folding
  aggregation grouping isn't wired yet. citext indexing is out of scope.
- [x] **xml type + basic functions landed** (b155): the `xml` type (real builtin, OID 142) stored as its text and
  validated well-formed on cast / coerce, plus the constructor / extraction functions. New self-contained
  `secantus/sql/xmltype.py` (`is_well_formed` / `parse` / `element` / `forest` / `concat` / `xpath`); XML parsing +
  serialization go through the stdlib `xml.etree.ElementTree` (no external dep; external entities disabled → no XXE).
  Wired through `typemap` (OID 142; `xml` in `SQL_TYPE_NAME` / `PG_TYPENAME` / `_DATATYPE_TAGS`; `coerce` →
  `xmltype.parse`), `scalar` (`_eval_cast` xml branch; the dedicated `exp.XMLElement` node handler
  `_eval_xmlelement` reading `xmlattributes(...)`; `xmlforest` special-cased in `_eval_func` because it needs the
  per-arg `AS name` aliases; `xpath` / `xml_is_well_formed` / `xmlconcat` in `_call_func`), `functions` (the xml
  function names in `_SCALAR_EVAL_ANON`), and `planner` (`_infer_scalar_tag`: `xmlelement`/`xmlforest`/`xmlconcat`
  → xml, `xpath` → text[], `xml_is_well_formed` → bool, cast → xml). Tests: `tests/test_sql_xml.py` (25) plus a
  pg8000 wire test. **Simplifications:** `xpath` is a pragmatic subset (absolute `/a/b/c` child paths, a trailing
  `text()` / `@attr` step, a leading `//tag` descendant search) — not full XPath 1.0 (no namespaces / predicates /
  functions); the `xmltable` table function, the `xmlagg` aggregate, and the document/content-node distinction are
  out of scope.
- [x] **Full-text search ranking landed** (b166): two parts. (1) **`websearch_to_tsquery`** (`secantus/sql/fts.py`)
  — parses a web-search-style query (bare words AND'd, `"quoted phrases"` → adjacency via `phraseto_tsquery`, the
  bare word `or` → OR, leading `-` → NOT); registered in the four FTS dispatch sites (scalar `_call_func`, planner
  value-expr + `_infer_scalar_tag` → tsquery, `functions._SCALAR_EVAL_ANON`). (2) **ORDER BY output-alias
  resolution** in the single-table evaluated path (`planner._build_evaluated_single`) — mirrors the pipeline path:
  an unqualified `ORDER BY <name>` that matches a SELECT-list alias now resolves to that output expression, so a
  *ranked search* `SELECT …, ts_rank(…) AS rank … ORDER BY rank DESC` works (general, not FTS-specific — also fixes
  `ORDER BY <computed alias>` for arithmetic etc.). Tests: `tests/test_sql_fts_ranking.py` (7) + a pg8000 wire test.
  The rest of the FTS ranking surface already existed: `ts_rank` / `ts_rank_cd`, `ts_headline`, `phraseto_tsquery`,
  and `ORDER BY ts_rank(…)` (repeated expression). **Simplifications (unchanged):** `ts_rank_cd` == `ts_rank` (a
  monotonic match-count, not cover-density); fixed config; no stemming; no lexeme weights.
- [x] **generate_series + base-less FROM-clause SRFs landed** (b163): a set-returning function as the *whole* row
  source. New `secantus/sql/srf.py`: `from_source` (a base-less `FROM generate_series(…)` / `FROM unnest(…)` /
  `jsonb_array_elements` / `jsonb_object_keys` / `regexp_split_to_table`, incl. `WITH ORDINALITY` and `AS t(cols)`)
  and `fromless_projection` (a bare `SELECT generate_series(…)`). `engine._run_srf_select` materializes the generated
  rows into a synthetic `TableDef` + `virtual.MemoryBackend` and runs the normal `plan_select` + `execute_select`, so
  projection / WHERE / ORDER BY / LIMIT / `count(*)` all work for free. A single-column SRF's column takes the table
  alias (`generate_series(1,5) AS g` → column `g`), else an explicit column alias, else the function name; WITH
  ORDINALITY appends a 1-based ordinal column. sqlglot parses `generate_series` as `ExplodingGenerateSeries` and
  base-less `unnest` as a `From(this=Unnest)`; the FROM arg key is `from_` (not `from`). Tests:
  `tests/test_sql_srf.py` (17) + a pg8000 wire test. **date/timestamp `generate_series` landed (#150, b187):**
  `generate_series(ts_start, ts_stop, interval)` walks by the interval (`_generate_series_temporal` in `srf.py`,
  applying `intervals.to_date` per step; direction taken from whether one step moves forward/backward; zero step →
  `22023`; a 10M-row backstop → `54000`). Result column types as `timestamp` / `timestamptz` by the bound's tz-ness.
  Numeric ranges unchanged. **Simplifications:** a non-`count(*)` aggregate / `GROUP BY` directly over
  a base-less SRF isn't supported yet (the SRF path uses `plan_select`, not the pipeline planner) — wrap in a
  subquery/CTE or generate into a table first. The `FROM t, <srf>(…)` *join* form is unchanged (pipeline planner's
  `_unnest_join_stage`).
- [x] **SQL functions (CREATE FUNCTION) landed** (b162): `CREATE [OR REPLACE] FUNCTION name(params) RETURNS t AS $$
  body $$ LANGUAGE sql` + `DROP FUNCTION`. sqlglot parses these as `exp.Create`/`exp.Drop` with `kind=FUNCTION`
  (body = a `Heredoc` for `$$…$$` or a string `Literal`). `catalog.put_function`/`get_function`/`drop_function`
  persist to a new `__sql_functions__` collection keyed `name/nargs` (overload by arity). `engine._create_function`
  extracts params (named `ColumnDef` and/or bare-type → `None`), the `ReturnsProperty` type, and the `LanguageProperty`
  (non-`sql` → 0A000). Invocation: `scalar._invoke_udf` (hooked at the end of `_call_func`, after all builtins) binds
  args to named-param columns + positional `$N` and reduces the single-statement body via the existing
  `_eval_subquery` machinery (handles FROM-less scalar bodies *and* aggregate/table bodies). FROM-less calls route
  through `planner._udf_lookup` in `plan_constant_select`; WHERE-clause calls route to the per-row path via
  `where_needs_per_row(..., catalog, db)` + `_where_has_udf`; the evaluated-select column type comes from the UDF's
  `return_tag` (read off the planning `_pipeline_subctx` in `_infer_scalar_tag`). Nesting (a function calling another)
  works. Errors: duplicate `(name, arity)` without OR REPLACE → 42723; DROP of unknown → 42883 (IF EXISTS silences).
  Tests: `tests/test_sql_functions.py` (17) + a pg8000 wire test. **`LANGUAGE plpgsql` scalar bodies landed**
  (b234): a compact procedural interpreter in `secantus/sql/plpgsql.py` (own tokeniser + recursive-descent parser
  + tree-walking `_Runner`) runs the scalar subset — `[DECLARE …] BEGIN … END` (nestable), assignment (`:=` / `=`),
  `IF … ELSIF … ELSE … END IF`, `RETURN [expr]` / `RETURN NULL`, `SELECT … INTO var[, …]`, `PERFORM`, and bare
  `INSERT`/`UPDATE`/`DELETE`. `engine._create_function` validates the body via `plpgsql.parse` at CREATE and stores
  the raw text; `scalar._invoke_udf` dispatches on `func["language"]` to `plpgsql.invoke`. Expressions eval through
  `scalar.evaluate` with a scope that resolves declared vars / params (positional `$N` pre-substituted); embedded SQL
  statements inline vars/params as literals and run through `engine.run_inner_select` / `_run_statement`. `IF` treats
  NULL as false; declared type tags drive an optional assignment/return coercion. Tests: `tests/test_sql_plpgsql.py`
  (14). **Out of scope (0A000 at CREATE):** loops (`LOOP`/`WHILE`/`FOR`), `RAISE`, `RETURN QUERY`/`NEXT`
  (set-returning), `CASE` statements, cursors, `EXCEPTION` handlers, dynamic `EXECUTE`; block-scoped variable
  shadowing isn't modeled (flat env); embedded-SQL literal inlining stringifies date/Decimal params (`_value_to_node`
  limitation). **Still simplifications:** a *multi-statement* `LANGUAGE sql` body is still rejected (deferred — the
  other flagged site at `engine._create_function`); a set-returning (`SETOF`/`TABLE`) function yields only its first
  row in a scalar context.
- [x] **Arrays of the new types + array ops landed** (b161): two parts. (1) **Array type OIDs** — `typemap._ARRAY_PG_OID`
  gains the real Postgres array-type OIDs for the newer element types (`uuid[]` 2951, `inet[]`/`cidr[]`/`macaddr[]`,
  `date[]`/`time[]`/`timetz[]`, `interval[]`, `bit[]`/`varbit[]`, `money[]`, `xml[]`, `json[]`→jsonb 3807, the
  geometric arrays, and the range arrays), so a driver decodes the elements natively (pg8000 gives a `uuid[]` back as
  a list of `UUID`). (2) **Array containment/overlap operators** `@>` / `<@` / `&&` on Postgres *array* operands.
  `scalar._eval_array_op` (new `_NOT_ARRAY` sentinel) handles the list-vs-list case, inserted into the
  `@>`/`<@`/`&&` chain *after* range/net/geo/hstore so those keep priority. Array-operator WHEREs route to the
  per-row path via `planner._where_has_array_predicate` + `_is_array_operand` (array-typed column, `ARRAY[...]`
  literal, or array cast); `_has_array_operand` (resolve-based) drives the `_infer_scalar_tag` → bool typing for a
  SELECT-list array op. jsonb (non-array) `@>`/`<@` keep the jsonb pushdown path untouched — array detection requires
  an array-typed operand. Tests: `tests/test_sql_array_ops.py` (10) + a pg8000 wire test. **Simplifications:** array
  element equality is Python `==` (no cross-type array coercion beyond the element coerce); `citext[]` / `hstore[]`
  fall back to the text array OID (those element types have no fixed catalog OID); arrays stay one level deep.
- [x] **EXPLAIN for the SQL layer landed** (b158): `EXPLAIN [ANALYZE] [(options)] <statement>` returns a `QUERY
  PLAN` text column. New `secantus/sql/explain.py`: `parse_options` splits the tail (both the bare `EXPLAIN ANALYZE
  VERBOSE <stmt>` word form and the parenthesised `(ANALYZE, FORMAT JSON)` form); `_build_node` walks the parsed
  statement into a plan-node dict; `_text_lines` / `_json_node` render the indented tree or Postgres' single-row
  JSON plan. The scan node's Index Scan vs Seq Scan call is the **authoritative** one from `Storage.explain_plan`
  (the same router `find_matching` uses; a `list_indexes` leading-field heuristic is the fallback for storages
  without `explain_plan`, e.g. the test double), so EXPLAIN never claims an index the real query wouldn't use.
  Single-relation SELECT/UPDATE/DELETE get a faithful scan node with `Index Cond:` / `Filter:`; INSERT an *Insert*
  node; JOIN/GROUP BY/aggregate queries a coarse top node over the base-collection Seq Scan. `ANALYZE` runs the
  statement via a `run_stmt` callback (avoids a circular import back to `engine`; an `EXPLAIN ANALYZE` of a write
  performs the write, as Postgres does) and annotates the top node with `actual rows`. sqlglot falls back to a
  `Command` (verb EXPLAIN, tail as a string Literal). Tests: `tests/test_sql_explain.py` (19) + a pg8000 wire test.
  **Simplifications:** cost figures are placeholders (`cost=0.00..0.00` — no statistics engine); `ANALYZE` reports
  actual rows but no per-node timing; `BUFFERS`/`SETTINGS`/`COSTS`/`TIMING` accepted-and-ignored; only `FORMAT
  TEXT`/`JSON` (others → `0A000`); pipeline-query plans name the top operation coarsely rather than reproducing
  Postgres' full plan-node tree.
- [x] **PREPARE / EXECUTE / DEALLOCATE landed** (b157): SQL-level prepared statements on the session. `PREPARE name
  [(argtypes)] AS <query>` parses the query (with its `$N` placeholders) and stashes `(query_ast, param_count)` on
  the new `Session.prepared` dict; `EXECUTE name [(args)]` parses the args (`SELECT <args>` wrapper → expression
  nodes), substitutes them for the `$N` `exp.Parameter` nodes (`_bind_parameter_nodes`, node-for-node so casts /
  typed literals survive), and re-dispatches through `_run_statement`, returning the underlying statement's result +
  command tag (any DML kind works — a prepared INSERT tags `INSERT 0 N`). `DEALLOCATE name` / `DEALLOCATE ALL`
  removes from the dict (was previously a blanket no-op alongside DISCARD; DISCARD stays a no-op). sqlglot falls
  back to a `Command` (tail as a string Literal) for PREPARE/EXECUTE and a bare `Alias` for DEALLOCATE, so the
  handlers regex the tails. Errors: duplicate name `42P05`, unknown name `26000`, arg-count mismatch `08P01`;
  DEALLOCATE of an unknown name is tolerated (libpq/psycopg fire speculative DEALLOCATEs on cleanup). Per-session
  (not shared). The `(argtypes)` list is accepted and ignored — values are coerced by the target column's type.
  Distinct from the extended wire protocol's Parse/Bind portals (`pgextended.py`) — a driver's own `%s` binding
  never touches these. Tests: `tests/test_sql_prepare.py` (14) + a pg8000 wire test. **Simplification:** unquoted
  statement names aren't folded to lower case (matches the existing DECLARE CURSOR name handling).
- [x] **LISTEN / NOTIFY / UNLISTEN landed** (b156): cross-connection async pub/sub on the PG wire server. New
  `secantus/sql/pgnotify.py` (`NotifyHub` — a server-wide channel → listening-session registry, keyed by
  `id(session)` since `Session` is an unhashable dataclass). New `pgwire.notification_response` ('A'). `Session`
  gains `notify_hub`, a thread-safe inbound `_notify_deliveries` deque (drained by the owning connection thread —
  all socket writes stay on one thread) + `pending_notifies` (buffered in-txn). `engine._maybe_pubsub` handles the
  commands *before* sqlglot (which mis-parses `LISTEN chan` and errors on `NOTIFY chan, 'p'`); `is_pubsub_statement`
  lets the wire server skip the COPY probe. NOTIFY buffers inside a txn block and flushes at COMMIT (dropped on
  ROLLBACK); `pg_notify(channel, payload)` function form in `scalar._call_func`. `pgserver` owns one `NotifyHub`,
  attaches it to each session, and drops listens on disconnect. **Delivery is inline with the query cycle**:
  `_pending_notification_bytes` serializes the queued `NotificationResponse`s and they're written on the owning
  connection thread just before each `ReadyForQuery` (both the simple-`Q` and extended paths). An earlier
  `select`-based idle poll was reverted — it busy-woke every idle connection every 0.25s and, under CI's 2-core
  load with the whole suite's connections, starved the request-handling threads enough that clients hit their own
  timeouts (mass `network error` connection drops on the Linux/py3.10 job; green locally + on Windows). The
  inline design leaves the blocking read loop untouched. Tests: `tests/test_sql_notify.py` (14: hub + engine) plus
  a two-connection pg8000 wire test. **Simplifications:** duplicate `(channel, payload)` notifications in one txn
  aren't collapsed (Postgres collapses them); LISTEN/UNLISTEN take effect immediately, not at commit; and there is
  no out-of-band async push to a fully-idle connection (notifications ride the next query response).
- [x] **jsonb aggregates + builders landed** (b138): the aggregates `jsonb_agg` / `json_agg` and
  `jsonb_object_agg` / `json_object_agg`, plus the scalar builders `to_jsonb` / `to_json` /
  `row_to_json`. `jsonb_agg` / `json_agg` fold into `planner._array_agg_arg` (they build the same
  `$push` array and are already typed `json` here) so every group-plan + detection site lights up
  automatically, including the in-call `ORDER BY` (`json_agg` is the dedicated `exp.JSONArrayAgg`
  node; `jsonb_agg` an `Anonymous`). `jsonb_object_agg` (`_jsonb_object_agg_args` + `_jsonb_object_agg_push`)
  pushes `{k: {$toString: key}, v: val}` pairs then projects `{$arrayToObject: "$field"}` → a json
  object (key coerced to text); wired into the single-table, grouping-set and JOIN group paths plus
  the five aggregate-detection sites. `json_object_agg` parses as `exp.JSONObjectAgg` (args in an
  `expressions` list), `jsonb_object_agg` as `exp.JSONBObjectAgg` (`this`/`expression`). The builders
  are the identity in `scalar._call_func` (values already store as native Python that renders as json;
  a composite / `ROW(...)` arrives as a subdocument); `functions.is_scalar_function` excludes them
  (via `_SCALAR_EVAL_ANON`) so a FROM-less `SELECT to_jsonb(...)` defers to the scalar evaluator. All
  typed `json` in `planner._infer_scalar_tag`. Tests: `tests/test_sql_jsonb_agg.py` (18) + a pg8000
  wire test. **Not yet implemented:** `jsonb_each` / `jsonb_each_text` (a two-column `(key, value)`
  record SRF — the current SRF executor emits a single value per row, so a multi-column record SRF
  needs an executor change); `FILTER (WHERE …)` on the jsonb aggregates; the default output label for
  an un-aliased `jsonb_agg` is `array_agg` (cosmetic).
- [x] **SQL/JSON path queries landed** (b135): a compact `jsonpath` evaluator in `secantus/sql/jsonpath.py`
  (tokenizer + recursive-descent parser + evaluator) powering `jsonb_path_query` / `jsonb_path_query_array`
  / `jsonb_path_exists` / `jsonb_path_match` (via `scalar._call_func`) and the `@?` (`exp.JSONBPathExists`)
  / `@@` (`exp.MatchAgainst` — sqlglot puts the path in `this`, the doc in `expressions[0]`) operators
  (via `scalar.evaluate` → `_eval_jsonb_path_op`). Supported grammar: `$` root, `.key` / `."key"`, `[n]`
  (negative from end), `[*]`, `.*`, and `? (<pred>)` filters where a predicate compares `@`/`@.path`
  (`== != < <= > >=`) to a literal, combined with `&&`/`||`; `@@`/`jsonb_path_match` parse a top-level
  predicate (`$.a == 5`). Typed in `planner._infer_scalar_tag` (JSONBPathExists/MatchAgainst → bool;
  `jsonb_path_query`/`_array` → json; `jsonb_path_exists`/`_match` → bool). **Limitations:** unsupported
  jsonpath constructs (arithmetic, `.size()` and other methods, recursive `**`, `like_regex`, `exists()`
  inside a predicate, `$var` bindings) raise a faithful `feature_not_supported`; `jsonb_path_query` is
  genuinely set-returning in PG but returns only the **first** match in a scalar SELECT (use
  `jsonb_path_query_array` for the set); `@?`/`@@` in a WHERE predicate go through the scalar path
  (COLLSCAN), not a lowered Mongo filter.
- [x] **Date/time scalar functions landed** (b132): `extract(field FROM ts)` / `date_part('field', ts)`
  (year/month/day/hour/minute/second/quarter/dow[Sun=0]/isodow[Mon=1]/doy/week/epoch → numeric),
  `date_trunc('unit', ts)` (year/quarter/month/week[→Monday]/day/hour/minute/second → timestamptz),
  `to_char(ts, fmt)` (text), interval arithmetic `ts ± interval '…'`, and `now()` / `current_timestamp`
  / `current_date` — all in `scalar.py`. Intervals lower to an `_Interval` (calendar `months` +
  fixed `timedelta`) whose `__radd__`/`__rsub__` apply calendar-aware month/year math with day
  clamping (Jan 31 + 1 month → Feb 28); `_eval_interval` handles both `(value, unit)` and compound
  string forms (`'1 year 2 months 3 days'`). `to_char` relies on sqlglot pre-normalising the standard
  tokens to strftime directives, then maps the leftover word tokens (`Mon`/`Month`/`Dy`/`Day`/`AM`/`PM`)
  and strftimes once. Typed in `planner._infer_scalar_tag` (Extract → numeric, TimestampTrunc /
  Current* → timestamptz, TimeToStr → text, `ts ± interval` inherits the timestamp's tag). **Limitations:**
  `age()` (returns an interval *value* — we don't model an interval type) is not implemented;
  `to_char` full weekday names (`Day`/`Dy`) mis-render because sqlglot greedily eats the leading `D`
  during normalisation; date/time functions in a `WHERE` predicate go through COLLSCAN + the scalar
  path (not lowered to a Mongo filter), same as other computed predicates.
- [x] **String round-out scalar functions landed** (b133): `lpad`/`rpad` (pad or truncate to a length,
  default fill space), `left`/`right` (prefix/suffix; a negative count drops from the far end — `left`
  via `s[:n]`, `right` via `s[-i:]` with an `i==0 → ''` guard), `repeat`, `reverse`, `initcap`
  (`str.title()`), `ascii`/`chr`, `position(sub IN str)` / `strpos(str, sub)` (1-based, 0 if absent),
  and `overlay(str placing rep from start [for len])` — all typed nodes in `scalar.py` registered via
  the version-tolerant getattr loop. Typed in `planner._infer_scalar_tag` (pad/left/right/repeat/
  reverse/initcap/chr/overlay → text; ascii/strpos/position → int4). **Limitation:** `initcap` uses
  Python `str.title()`, which matches Postgres for ASCII words but can differ on apostrophes
  (`"o'brien"` → `"O'Brien"` vs PG `"O'Brien"` — same here, but exotic Unicode word boundaries may drift).
- [x] **Aggregate in-call `ORDER BY` landed** (b128): `array_agg(x ORDER BY y [DESC])` /
  `string_agg(x, sep ORDER BY y)` order the aggregated values. sqlglot keeps the ORDER BY as an
  `exp.Order` wrapping the value (the old "sqlglot drops it" note was wrong). `planner._agg_order_spec`
  unwraps it into `(value, [(key, direction, nulls_first), …])`; `_sorted_agg_push` emits a `$push` of
  `{v, k}` pairs, and the executor (`_sorted_agg_value`) sorts the pairs by the key list via the existing
  `_pg_sort` (per-key direction + Postgres NULL placement) before building the array / joining the string
  (NULL values skipped, NULL when all-NULL). Recorded as a `PipelineSelectPlan.post_aggregates` entry
  (`sorted_array` / `sorted_string`). Single-table + whole-table, **and over a JOIN** (b205, #170:
  `_sorted_agg_push_resolve` lowers the value / sort-key expressions through the join resolver, and the
  join planner records the same `post_aggregates` entry), **and under GROUPING SETS** (b224: each grouping
  set's branch — single-table `_grouping_set_branch` / join `_join_grouping_set_branch` — pushes the `{v, k}`
  pairs and records the `sorted_array` / `sorted_string` `post_aggregates` entry, which the grouping-sets
  planner threads onto the union's `PipelineSelectPlan`; the sort runs per output row over the whole union.
  `FILTER` with an in-call `ORDER BY` stays `0A000`). The finalization
  (`executor._apply_post_aggregates`) runs in **both** the top-level pipeline executor and derived-table
  materialization (`_run_subplan_to_docs`) so the `{v, k}` push pairs never leak — this also closed a
  latent b127 gap where an ordered-set agg inside a derived table (e.g. SQLAlchemy's index reflection,
  which does `array_agg(attname ORDER BY …)` over a derived table) leaked its raw pushed array.
- [x] **Ordered-set aggregates landed** (b127): `percentile_cont(f)` / `percentile_disc(f)` / `mode()`
  via `WITHIN GROUP (ORDER BY expr)` (sqlglot `exp.WithinGroup`). `planner._ordered_set_agg` detects them
  (wired into `select_needs_pipeline` + the two `has_aggregate` routing predicates); `_plan_group_select`
  collects the ORDER BY values into a `$push` accumulator and records a `PipelineSelectPlan.post_aggregates`
  entry `(field, kind, fraction)`. The executor (`_ordered_set_value`, run in `execute_pipeline_select`
  after `apply_pipeline`) drops NULLs, sorts, and computes: `percentile_cont` = linear interpolation
  between the two nearest ranks (→ `float8`); `percentile_disc` = first value whose cumulative fraction ≥
  `f` (keeps the element type); `mode` = most frequent (smallest on a tie). NULLs ignored; all-NULL / empty
  group → NULL; `f` outside `[0,1]` → `2202E`. (Computed in Python because the aggregation-expression
  engine has no `$sortArray`.) **Not supported:** ordered-set aggs over a JOIN (single-table + whole-table
  only), and — like all pipeline aggregates — a whole-table aggregate over zero input rows returns no row
  rather than one NULL row.
- [x] **WHERE: column-to-column + arithmetic + non-correlated subqueries landed.** `column OP
  literal` keeps the indexable `{field: {op: val}}` fast path. A comparison where neither side is
  a constant — `qty > shipped`, `price < cost * 1.5` — lowers to a Mongo `{$expr: {$op: [...]}}`
  (`planner._to_agg_expr`), with `+`/`-`/`*`/`/` arithmetic over columns and literals nesting
  inside. **Non-correlated subqueries**: `x IN (SELECT col FROM t [WHERE ...])` → `$in`,
  `NOT IN` → `$nin` (via the `NOT`→`$nor` wrapper), and a scalar `x = (SELECT max(col) ...)`
  (any comparison op) → the evaluated value. The inner SELECT runs through the engine
  (`engine.run_inner_select` ← `planner.SubqueryCtx`), so it may itself aggregate / filter; it
  must select exactly one column. Plumbed via the single-table `plan_select` path (a subquery in
  a GROUP BY/JOIN query's WHERE isn't wired yet → `0A000`). `$expr` / subquery filters can't use
  a storage index (→ COLLSCAN). **EXISTS / correlated subqueries landed** (b45): a WHERE with
  `EXISTS`/`NOT EXISTS` or a subquery that references the outer row can't push down, so
  `planner.where_needs_per_row` routes it to `executor.execute_correlated_select`, which scans the
  outer table and evaluates the whole WHERE per row via `secantus.sql.scalar` — the inner query
  reads inner-table rows with outer-row references falling through (`scalar._inner_row_scopes` /
  `_eval_exists` / `_eval_in`, aggregate inner projections reduced by `_SUBQUERY_AGG_REDUCERS`).
  **Correlated WHERE in the pipeline paths landed** (b70): a JOIN or single-table GROUP BY whose
  WHERE is correlated / `EXISTS` no longer errors. `planner.where_needs_per_row` now short-circuits
  the pushdown `$match` in `_build_join_pipeline` and `_plan_group_select`; the WHERE is carried as
  `EvaluatedSelectPlan.where` (a JOIN — evaluated per joined row *after* the pipeline, outer scope via
  the join resolver) or `PipelineSelectPlan.residual_where` + `residual_resolve` (a GROUP BY —
  evaluated per base doc *before* the `$group`, so only survivors group). The inner query is still a
  simple `SELECT … FROM one_table [WHERE …]`; the per-row scan is `O(outer × inner)` (no index use).
  **Correlated WHERE + JOIN + GROUP BY landed** (b76, also a correctness fix — b70 silently *dropped*
  the WHERE for this shape): `PipelineSelectPlan.residual_split` records how many leading pipeline
  stages (the join prefix) run before the Python filter, so `_plan_join_group_select` runs the
  `$lookup`/`$unwind`, filters the joined rows by the correlated WHERE (outer scope via the join
  resolver), then runs the `$group` over the survivors (`executor._pipeline_input_docs` applies the
  split). **A correlated WHERE combined with GROUP BY *and* a window function now works too** (b206,
  #171), single-table and over a JOIN: `EvaluatedSelectPlan` gained a pre-`$group` residual
  (`pre_where` / `pre_where_resolve` / `pre_where_split`) — the executor runs the leading join-prefix
  stages, filters the joined rows per the correlated predicate (WHERE precedes grouping), then runs the
  rest of the pipeline; `_plan_join_group_window_select` dropped its former rejection. **A correlated /
  subquery HAVING now works** (b206): a HAVING with a subquery routes to the group-window evaluated
  path, its aggregates rewritten to their computed fields and the predicate carried as a post-group
  residual (`_outer_agg_nodes` skips aggregates inside the subquery). A function-call comparison
  used as a *pushdown* filter (`WHERE amt = abs(target)`) works in GROUP BY / JOIN pipelines too (it
  rides the shared `_expr_to_filter` / `_to_agg_expr` lowering, same as the single-table path; an
  unlowerable function like `substr` stays `0A000`). Still `0A000`: `<@`-style structural predicates.
- [x] **`RETURNING` landed** (b46). `INSERT` / `UPDATE` / `DELETE … RETURNING <proj>` projects the
  affected rows back as a result set (`planner._returning_columns` reuses the SELECT projection
  vocabulary `_out_columns`: `*`, columns, aliases, jsonb nav). `execute_insert` pins an `_id` on
  each doc before insert so the in-hand list is the authoritative inserted set; `execute_update`
  captures matched `_id`s and re-reads the **post-image**; `execute_delete` snapshots the victims
  before deleting. The wire layer already emits RowDescription+DataRows whenever `res.columns` is
  non-empty, so no pgserver change. **Computed expressions in `RETURNING` landed** (b67): each
  returning item is now `(name, Column, expr)`; `expr` is None for a plain column / `*` / jsonb (read
  straight from the doc), else the raw node evaluated per returned row by `executor._returning_result`
  against a scope over that row (arithmetic, `||`, function calls, `CASE`, …). Works for INSERT /
  UPDATE (post-image) / DELETE and `INSERT … ON CONFLICT`. A subquery inside `RETURNING` isn't
  supported (the eval ctx has no catalog/session).
- [x] **Set operations landed** (b47). `UNION` / `INTERSECT` / `EXCEPT` (+ `ALL` variants, chained)
  in `engine._run_set_operation`: each arm runs through the full SELECT path, rows are combined
  with multiset semantics (`_combine_setop_rows` / `_multiset_filter` — DISTINCT collapses to set
  semantics, `ALL` keeps min-count for INTERSECT / left-minus-right for EXCEPT), output columns
  come from the first arm, and a trailing `ORDER BY` (output-column name or ordinal) + `LIMIT`/
  `OFFSET` apply to the combined result. `describe_statement` resolves the result shape from the
  leftmost arm so the extended protocol's Describe works. Arity mismatch → `42601`. **`VALUES` lists
  landed** (b232): `engine._run_values` evaluates a `VALUES (…), (…) [ORDER BY …] [LIMIT …]` constant
  table (each cell a constant expression via `scalar.evaluate` with `planner._const_scope`; columns
  named `column1…`/the `AS t(…)` alias, typed from the first non-NULL value per position via
  `planner._infer_value_tag`; ordinal/output-column `ORDER BY` reuses `_setop_order_limit`). Wired
  into both `_run_statement` (a standalone `VALUES` query) and `_run_query` (a `VALUES` set-op arm, so
  `SELECT … UNION VALUES (…)` and `VALUES (…) UNION SELECT …` work). Uneven row widths / a set-op arity
  mismatch → `42601`. **Limits (both faithful to Postgres):** no cross-arm type reconciliation
  (columns/types taken verbatim from the first arm); a set-op / `VALUES` `ORDER BY` accepts only an
  output-column name or ordinal, not an arbitrary expression (`42703`) — Postgres rejects the latter too.
  (`VALUES` as a FROM-clause derived table is a separate, still-open path.)
- [x] **Non-recursive CTEs landed** (b49). `WITH name AS (...) [, ...] <query>` in
  `engine._run_with`: each CTE is materialized to rows (run through `_run_query`) and registered as
  an ephemeral collection on a `CatalogBackend`, with a `_CTECatalog` overlay mapping CTE names to
  TableDefs built from each inner query's result shape; the `WITH` is stripped (`node.pop()`) and the
  main query runs against that backend + overlay, so CTE names resolve like tables in every path
  (single-table, pipeline/join, set-op). CTEs materialize in order (later may reference earlier);
  names are statement-scoped (overlay, no catalog mutation). `describe_statement` returns NoData for a
  `WITH` query (resolving columns would require executing the CTEs), so the extended protocol relies
  on Execute's RowDescription. **`WITH RECURSIVE` landed** (b56): `engine._run_recursive_cte` evaluates
  a CTE whose body is a `UNION [ALL]` of an anchor + a recursive term (detected by
  `_is_recursive_cte` — the right arm references the CTE name) via semi-naive iteration — run the anchor,
  then repeatedly run the recursive term against just the prior step's new rows (re-registered under the
  CTE name) until empty. `UNION` dedups vs all rows seen (cyclic graphs terminate); `UNION ALL` keeps
  every row, guarded by `_MAX_RECURSION_ROWS` (1M → `54001`). Optional column aliases (`name(a,b)`,
  `_cte_column_aliases`) rename the output and now apply to non-recursive CTEs too (`_register_cte`).
  A bare integer/bool literal in a SELECT list now types from its value (`_infer_scalar_tag` →
  `_infer_value_tag`) so `SELECT 0 AS lvl` rides the wire as an int, not text. **`WITH` on writes
  landed** (b68): `_run_with` accepts an `INSERT`/`UPDATE`/`DELETE` body — the CTEs materialize the
  same way, then the write is dispatched via `_run_statement` against the `CatalogBackend` (whose writes
  forward to real storage) + `_CTECatalog` overlay, with the CTE-aware `SubqueryCtx` published on
  `planner._pipeline_subctx` so an `UPDATE`/`DELETE` WHERE subquery over a CTE resolves. So
  `WITH cte AS (…) INSERT INTO t SELECT … FROM cte` and `… UPDATE/DELETE … WHERE id IN (SELECT … FROM
  cte)` work. **Data-modifying CTEs + `WITH RECURSIVE` before a write landed (#147, b187):**
  a CTE body may itself be `INSERT`/`UPDATE`/`DELETE` (optionally `… RETURNING`) — `_run_with`
  routes it through `_dispatch_cte_write` (execute for side effects against the backend, materialize
  its RETURNING rows as the CTE), so `WITH moved AS (DELETE … RETURNING …) INSERT … SELECT FROM moved`
  moves rows between tables in one statement; a body with no `RETURNING` still runs. `WITH RECURSIVE`
  before an `INSERT`/`UPDATE`/`DELETE` also works (the recursive CTE materializes first, then the write
  body dispatches). Not modeled: statement-level snapshot semantics (each data-modifying CTE sees the
  effects of earlier ones rather than a single pre-statement snapshot) and `WITH CHECK OPTION`.
- [x] **`INSERT … SELECT` landed** (b50). `INSERT INTO t [(cols)] SELECT …` routes through
  `engine._run_insert`: the source query (a SELECT / set operation; may join / aggregate / CTE) runs
  via `_run_query`, and its result rows map positionally onto the target columns through the shared
  `planner._insert_doc` (same coercion / NOT NULL / PK→`_id` path as VALUES, factored out alongside
  `insert_target_columns` / `plan_insert_rows`). Column-count mismatch (target vs query) → `42601`;
  `RETURNING` works (the source is materialized first, so a self-insert reads a stable snapshot).
  A leading `WITH` before an `INSERT` / `UPDATE` / `DELETE` / **`MERGE`** (b204, #169 added MERGE) all
  work — the CTEs materialise, then the write runs against the CTE-aware backend + catalog overlay.
- [x] **Window functions landed** (b51). `func(...) OVER (PARTITION BY … ORDER BY …)` routes through
  the evaluated-select path (a window expr already trips `_stmt_needs_evaluation`). `secantus.sql.window`
  computes each window over the fetched rows — partition (repr-keyed groups), order within partition
  (stable multi-key sort), then apply the function — and stores the value on each doc under a synthetic
  `__win_<k>` field; `scalar.evaluate` resolves an `exp.Window` node to that value via the scope (so a
  window can nest inside a larger expression). Supported: `ROW_NUMBER`/`RANK`/`DENSE_RANK`,
  `SUM`/`COUNT`/`AVG`/`MIN`/`MAX` (whole-partition, or running under the default RANGE frame — peers
  tied on the order key share the cumulative value), `LAG`/`LEAD` (offset + default). `_infer_scalar_tag`
  types them (rank/count→int8, avg→float8, sum/min/max/lag/lead→arg tag). **Frames + value/rank funcs
  landed** (b55): `NTILE`, `FIRST_VALUE`/`LAST_VALUE`/`NTH_VALUE`, and explicit frames — `window._frames`
  builds each row's inclusive `[lo, hi]` index range; ROWS frames take any `UNBOUNDED`/`CURRENT ROW`/
  `n PRECEDING`/`n FOLLOWING` bound, RANGE frames take `UNBOUNDED`/`CURRENT ROW` (peer-group) bounds.
  Aggregate windows and the value functions reduce/select over the frame; rank-like funcs ignore it.
  **Window + `GROUP BY` in one SELECT landed** (b69): `planner._plan_group_window_select` runs a two-phase
  plan — a `$group` computes the grouping columns + every group aggregate (collected anywhere in the SELECT
  list / ORDER BY via `_group_agg_nodes`, which excludes a window's own aggregate operand), then the
  evaluated executor computes the windows over the grouped rows. Each group aggregate is replaced in the
  AST by a reference to its computed field (so `RANK() OVER (ORDER BY SUM(sal))`, `SUM(SUM(sal)) OVER ()`,
  and `PARTITION BY <group col>` all resolve), `HAVING` prunes groups before the window, and `ORDER BY
  <window alias>` is resolved in this planner (a bare alias term is substituted with its output expression).
  **Window + `GROUP BY` + `JOIN` landed** (b73): `_plan_join_group_window_select` is the join analogue —
  it builds the `$lookup`/`$unwind`/`$match`/`$group`/`$project` pipeline (group keys + aggregates
  resolved through the join resolver), then the same window phase runs over the grouped rows. The shared
  tail (`_finish_group_window`) is factored out of both planners. **Numeric `RANGE` offset frames
  landed** (b208): `n PRECEDING` / `n FOLLOWING` with a numeric offset is a value window over the
  sorted order key (`window._range_offset_bound`, computed in a direction-normalised key space so ASC
  and DESC share one comparison); Postgres' single-ORDER-BY-column requirement is enforced, a negative
  offset is `22013`. **`INTERVAL` `RANGE` offsets landed** (b231): `RANGE BETWEEN INTERVAL '1 day'
  PRECEDING …` over a `date`/`timestamp` ORDER BY key (`window._range_interval_bound` + `_interval_subdoc`;
  the boundary is `intervals.to_date(cur, offset, direction*sign)` and rows are kept on the in-frame side,
  the operator flipping with the sort direction). Same single-ORDER-BY-column rule; a negative interval is
  `22013`; an interval offset on a non-temporal key is `0A000`. **`ORDER BY <output-alias>`
  landed everywhere** (b208): the simple pushdown path resolves a standalone output alias to its input
  column (`_rewrite_order_by_aliases`, a real column of the same name wins per Postgres precedence);
  the evaluated / group-window paths already resolved aliases.
- [x] **`INSERT … ON CONFLICT` landed** (b52). `INSERT … ON CONFLICT (cols) DO NOTHING | DO UPDATE SET …
  [WHERE …]` via `planner._plan_on_conflict` (an `OnConflict` on `InsertPlan`) + `executor.
  _execute_insert_on_conflict`: each proposed row probes the conflict target with `find_matching`; a
  clean row inserts, a conflicting row is skipped (`DO NOTHING`) or updated in place (`DO UPDATE`). SET
  expressions evaluate per-row through `scalar.evaluate` with a scope binding `EXCLUDED.<col>` to the
  proposed row and bare/target-qualified columns to the existing row (so `n = t.n + EXCLUDED.n` works);
  an optional `WHERE` gates the update. A bare `ON CONFLICT DO NOTHING` (no target) inserts and swallows
  any `11000` duplicate. Command tag counts rows inserted *or* updated (skipped don't count); `RETURNING`
  projects the inserted + updated rows. **`ON CONFLICT ON CONSTRAINT <name>` landed (#151, b189):**
  `_fields_for_constraint` resolves the name against the table's `unique_constraints` (by name) or the
  primary key (by its Postgres default name `<table>_pkey`) to the arbiter's storage fields; an unknown
  name raises `42704`. **Still unsupported:** `DO UPDATE` with no conflict target (→ `42601`).
- [x] **`MERGE` landed** (b74). `MERGE INTO target [alias] USING source [alias] ON <cond> WHEN [NOT]
  MATCHED [AND <cond>] THEN UPDATE SET … | DELETE | INSERT [(cols)] VALUES (…) | DO NOTHING` via
  `engine._run_merge`. Per source row it scans the target snapshot (loaded once at MERGE start) for rows
  the `ON` condition matches, then applies the first `WHEN` of the right kind whose optional `AND`
  condition holds; `_merge_source` materializes a table / reflected-collection / `(SELECT …) alias` source
  into name-keyed rows; conditions + `UPDATE` RHS + `INSERT` VALUES evaluate through `scalar.evaluate` with
  a scope that resolves target vs source columns by alias (`_merge_pick_when` / `_merge_apply_matched` /
  `_merge_apply_not_matched`). Command tag `MERGE n` counts inserts + updates + deletes; each target row is
  affected at most once (`done` id-set), and MERGE targets are captured for savepoint snapshots
  (`_write_target_collection` recognises `exp.Merge`). **`RETURNING` + `WHEN NOT MATCHED BY SOURCE`
  landed** (b77): `_merge_pick_when` now keys on both the `matched` and `source` (BY SOURCE) flags; after
  the source loop a BY-SOURCE pass applies `UPDATE`/`DELETE`/`DO NOTHING` to target rows no source row
  matched (`source_matched` id-set). The apply helpers return `(count, image)` — an updated row's
  post-image, an inserted row, or a deleted row's pre-image — and `RETURNING` projects them via
  `planner._returning_columns` + `executor._returning_result` (plain + computed target-column items).
  **Cardinality violation enforced (#156, b195):** when a target row is matched by more than one source row
  Postgres raises `21000` ("MERGE command cannot affect row a second time"); `_run_merge` now tracks the set
  of source-matched target docs and raises on the second match (a single source row matching many target rows
  is still allowed — each acted on once). **`RETURNING merge_action()` + source columns landed (#161, b198):**
  `_run_merge` tracks `(image, action, source_row)` per affected row and `_merge_returning_result` evaluates
  the projection against the MERGE scope (target image + source row), so `merge_action()` → `'INSERT'` /
  `'UPDATE'` / `'DELETE'` and `RETURNING s.col` reads the source. **MERGE UPDATE of a PK column landed
  (#164, b199):** a `WHEN MATCHED THEN UPDATE SET <pk> = …` now re-keys the row (delete + re-insert under
  the new `_id` / composite `_id.<name>`, via `set_path`) instead of leaking the immutable-`_id`
  `UpdateError`; a re-key that collides with an existing key raises `23505`, and parent-side FK actions
  (RESTRICT / CASCADE / SET NULL) fire via `executor._enforce_fk_on_parent_update` when the referenced
  column changes — mirroring the plain UPDATE re-key path (#157). **Limitations:** an unqualified column
  ambiguous between target and source resolves to the target.
- [x] **Join DML landed (#162/#163, b199).** `DELETE FROM t USING src[, …] WHERE <join>` and
  `UPDATE t SET … FROM src[, …] WHERE <join>` bring in other tables. `engine._run_statement` routes an
  UPDATE with `args["from_"]` → `_run_update_from` and a DELETE with `args["using"]` → `_run_delete_using`.
  Both collect source rows via `_collect_dml_sources` (reusing `_merge_source`, so a source may be a table
  or a `(SELECT …) alias`), then `_dml_join_matches` cartesian-products the sources per target doc and keeps
  the target on the first source combination whose `WHERE` (evaluated through `_dml_join_scope`, which
  resolves target vs source columns by alias) is truthy. DELETE is a **semi-join** — each matched target
  deleted once even when many source rows match (parent-delete FK checks via `enforce_parent_delete`).
  UPDATE evaluates each `SET` RHS against the joined scope (`typemap.coerce` to the target column type),
  validates the post-image (`enforce_update_images`), and writes `$set`. Both honour `RETURNING`
  (`executor._returning_result`). Command tags `DELETE n` / `UPDATE n`. **Before this the join clause was
  silently ignored — `DELETE … USING` deleted every target row (data-loss bug).** Limitations: a target
  row matched by multiple sources still updates from the *first* combination (Postgres leaves this
  unspecified); no self-join of the target back into the source list.
- [x] **Small cleanups landed** (b58). (1) A FROM-less `SELECT` now evaluates constant *expressions*
  (arithmetic, `||`, function calls, `CASE` …) via `scalar.evaluate` against an empty scope
  (`_const_scope`), not just bare literals + info functions; (2) a FROM-less `SELECT … WHERE <const>`
  is honoured — a false predicate yields zero rows (`ConstantSelectPlan.emit`), so a recursive-CTE
  anchor like `SELECT 1 WHERE 1=0` works; a column reference with no FROM → `42703`. (3) The jsonb `<@`
  (contained-by) operator lands in its pushable `const <@ field` form (== `field @> const`,
  `_jsonb_contains_filter`); the reverse `field <@ const` (subset-of-a-constant) form now runs as a
  COLLSCAN + residual — **landed in #149, b191 (see the jsonb-functions entry above).**
- [x] **WHERE subqueries in the pipeline paths landed** (b59). The single-table pushdown always threaded
  a `SubqueryCtx`, but the pipeline planners (JOIN / GROUP BY / evaluated / DISTINCT) called
  `_where_filter` from many places without one, so a WHERE scalar/`IN` subquery there was `0A000`.
  `plan_pipeline_select` now publishes the context via a planning-scoped `contextvars.ContextVar`
  (`_pipeline_subctx`, reset in a finally) that `_where_filter` / the join `$match` pick up — one
  set-point, no signature churn. So `WHERE x OP (SELECT …)` / `x IN (SELECT …)` work in JOIN / GROUP BY /
  scalar-expr queries and in a recursive-CTE term's WHERE (session is None on that path — data
  subqueries don't need it). **Correlated subqueries in a pipeline work** (verified & pinned b205,
  #170 — the note was stale): a correlated WHERE / EXISTS / IN over a GROUP BY or JOIN rides the
  residual per-row path, and a correlated scalar subquery in the SELECT list works single-table and
  over a join. **A scalar subquery *containing an aggregate* in the SELECT list is now robust** (b215):
  aggregate-detection (`_group_agg_nodes` / `_select_has_computed_aggregate`, via the new
  `_nested_in_subquery`) no longer descends into subqueries, so `SELECT g, (SELECT max(v) FROM u) FROM t`
  and `SELECT g, (SELECT count(*) FROM u WHERE u.g = t.g) FROM t` no longer mis-fire the "must appear in
  GROUP BY" (`42803`) error; a *grouped* query that also projects a subquery
  (`_select_projects_subquery`) routes to the evaluated group path. The one remaining gap is a correlated
  subquery in **HAVING** (`HAVING agg > (SELECT … WHERE t.k = outer.k)`) → `0A000`, since HAVING lowers to
  a post-`$group` `$match` with no per-group subquery evaluation.
- [x] **`ORDER BY` NULL placement landed** (b54). Postgres orders NULL as the largest value (ASC →
  NULLs last, DESC → NULLs first) with `NULLS FIRST`/`NULLS LAST` overriding; Mongo sort treats
  NULL/missing as the *smallest*, so the SQL layer no longer delegates NULL placement to storage.
  `planner._nulls_first` reads sqlglot's per-term flag (already PG-defaulted); the single-table,
  correlated, evaluated, and set-op paths sort in Python via the shared `executor._pg_sort`
  (`cmp_to_key`, NULL placement independent of direction) — which also pulls OFFSET/LIMIT off the
  storage fetch for an ordered single-table SELECT — and the join / GROUP BY pipeline paths get a
  companion null-rank `$addFields`/`$cond` field sorted ahead of each ORDER BY key
  (`planner._emit_pipeline_sort`), then `$unset`. **Note:** index-accelerated ORDER BY+LIMIT pushdown
  no longer applies to a single-table ordered SELECT (correctness over the storage-side sort
  optimisation — the SQL layer is a dev/test surface).
- [x] **Array columns landed** (b111). A `<type>[]` column stores a native BSON array; `ARRAY[…]` and
  `'{…}'` literals coerce in (`typemap._parse_pg_array_literal` / `coerce`), results render as Postgres
  array text (`_render_pg_array`) with the array type OID in `PG_OID` so a driver decodes back to a list.
  `<value> = ANY(col)` → array membership, `col @> ARRAY[…]` → containment (reuses the jsonb `$all` path),
  `array_length(col, 1)` / `cardinality(col)` → element count. Reflection: `information_schema.columns.
  data_type = 'ARRAY'` + `pg_attribute.atttypid` = array OID. Array subscripting / slicing landed in b114
  (`scalar._eval_bracket`: `arr[i]` 1-based element, NULL out of range; `arr[lo:hi]` 1-based inclusive
  slice; `WHERE arr[i]` lowers to `$arrayElemAt` via `planner._to_agg_expr`; element/array type inference
  in `_infer_scalar_tag`). `unnest(array_col)` in the SELECT list works (via the b-set-returning-function
  path). The array manipulation functions landed in b118 (`scalar._eval_array_{append,prepend,cat,position,
  remove,to_string}` + an `exp.Array` constructor handler; type inference in `_infer_scalar_tag`):
  `array_append` / `array_prepend` / `array_cat` (NULL array treated as empty, per Postgres),
  `array_position` (1-based, NULL if absent), `array_remove`, `array_to_string` (optional null-string
  arg; NULL elements dropped otherwise). `array_agg` populates a declared array column via `INSERT …
  SELECT`. **Multi-dimensional array introspection landed (#153, b192):** nested-array literals /
  columns (`int[][]`) round-trip and subscript (`g[2][3]`), and `array_ndims`, `array_dims`,
  `array_length(arr, dim)`, `array_upper` / `array_lower` (per dimension), and `cardinality` (total
  element count across all dimensions) are all dimension-aware — `scalar._array_dim_lengths` walks the
  rectangular shape, `array_length` (`exp.ArraySize`) and the Anonymous funcs share it, and the funcs are
  routed to the full scalar evaluator (added to `functions._SCALAR_EVAL_ANON`) so an `ARRAY[[…]]` argument
  evaluates. (A jagged / non-rectangular array reports lengths off its first element, as Postgres rejects
  those at build time anyway.) **Array `@>` / `&&` WHERE acceleration landed (#158, b197):** `field @>
  ARRAY[...]` and `field && ARRAY[...]` (`&&` symmetric) now lower to an *exact*, index-eligible Mongo filter
  instead of a COLLSCAN residual — `planner._array_index_filter`: `@>` → an `$and` of multikey equalities
  (`{$and: [{field: a}, {field: b}]}`, a lone element collapsing to `{field: a}`), `&&` → `{field: {$in:
  [...]}}`. `_where_has_array_predicate` / `_where_has_jsonb_contained_predicate` skip these shapes so they
  ride the pushdown (IXSCAN) path; a single-element `@>` and any `&&` light up a multikey index (`explain`
  reports IXSCAN). **Still per-row (residual COLLSCAN):** `<@` (subset — no exact filter), a *multi*-element
  `@>` (the planner doesn't index an `$and`-of-equalities, so it's correct but scans), `field @> ARRAY[]`
  (empty — true for all rows, which `$all: []` can't express), field-vs-field, and jsonb / range `@>`/`<@`/
  `&&` (unchanged). Other array functions are evaluated in Python (SELECT-list /
  INSERT-SELECT), not pushed into a Mongo
  `$match`; no element-type coercion beyond the scalar tags. The FROM-clause table form
  `SELECT … FROM t, unnest(t.tags) AS tag` landed in b119 (`planner._unnest_join_stage`: an `$addFields`
  exposing the array under the alias column + `$unwind`; a synthetic one-column `TableDef` registered at
  top level in the join `amap`; inner/comma/CROSS drops empty arrays, `LEFT JOIN … ON true` keeps them with
  a NULL element; the `AS x(v)` column-alias form works). Tests: `tests/test_sql_unnest_from.py`.
  **Remaining unnest limitations:** the base-less form (`FROM unnest(ARRAY[…])` with no other table) →
  `42703` (use the SELECT-list `SELECT unnest(…)` form); `WITH ORDINALITY` and multi-array `unnest(a, b)`
  unsupported.
- [x] ~~**No transactions, no parameters, no prepared statements.**~~ All three shipped;
  this was a P0-spike survey line that outlived its subject (verified 2026-08-20).
  `BEGIN`/`INSERT`/`ROLLBACK` really rolls back on a real WT user-transaction (probe:
  0 rows after), `PREPARE p (int) AS SELECT … WHERE id = $1` + `EXECUTE p (1)` returns
  the row, and the extended query protocol is the pgserver's normal path. Live detail
  is in "Transactions: single-connection atomicity; savepoints are real" and
  "PREPARE / EXECUTE / DEALLOCATE landed (b157)" further down this section.
- [x] **Composite primary keys landed** (b117): a `PRIMARY KEY (a, b)` maps to a subdocument `_id: {a, b}`
  (each PK column's `Column.field` is `_id.<name>`), so uniqueness rides the storage `_id` index exactly
  like a single-column PK. `planner._with_pk` maps the fields; `_insert_doc` builds the `_id` subdoc via
  `set_path` and `_canonicalize_composite_id` fixes its key order to the PK declaration order (Mongo treats
  `{a,b}`/`{b,a}` as distinct `_id`s). Reads ride the existing dotted-path projection / `get_path`. Upsert
  + MERGE fixed (`executor._find_conflict` / `_apply_conflict_update` use `has_path`/`get_path`; the
  UNIQUE-exclusion set uses `_hashable_id` since a subdoc `_id` is unhashable; `engine._merge_apply_not_
  matched` uses `set_path`). Reflection lists all PK columns (`virtual._index_relations` / `_pk_constraints`
  / `_foreign_keys` iterate `TableDef.pk_columns`) → SQLAlchemy `get_pk_constraint` returns both. Tests:
  `tests/test_sql_composite_pk.py` + a pg8000/SQLAlchemy wire test. **PK-column UPDATE landed (#157, b196):**
  updating a PK column (single or a composite subfield) now re-keys the row — `plan_update` flags
  `UpdatePlan.rekey`, and `executor._execute_update_rekey` computes each post-image (non-PK sets via
  `apply_update`, PK sets via `set_path` since `apply_update` refuses to touch `_id`), runs the shared
  post-image validation, checks the new `_id` is free (else `23505`), then deletes the old row and inserts
  the re-keyed one (all deletes before any insert, so a PK swap between rows doesn't collide). Statement-atomic.
  **Limitations:** a reflected collection's `_id` still can't be updated (`0A000` — it's the real Mongo key);
  renaming a composite-PK column via `ALTER TABLE` doesn't rewrite the `_id.<name>` subdoc key (edge case);
  a SERIAL/identity column inside a composite PK is untested. (A computed PK — `SET id = <expr>` — now works,
  including a PK swap; see the UPDATE-SET-expression entry below.)
- [x] **`UPDATE ... SET col = <expr>` — per-row computed assignment landed (#159, b198).** A SET RHS that isn't
  a literal (arithmetic, a column reference, `||`, a function call — `SET n = n + 1`, `SET a = b`, `SET s =
  upper(s)`) is collected into `UpdatePlan.computed` (`(field, type_tag, expr)`) by `plan_update` (via
  `_try_literal`), and `executor._execute_update_materialized` evaluates each against the **old** row (a
  `scope` over the pre-image, so a two-column swap `SET a=b, b=a` is correct), coerces to the column type,
  validates the post-image (NOT NULL `23502` / CHECK / UNIQUE / FK / generated — statement-atomic), and writes
  it back per row (or delete+insert when the computed target is the PK). The pure-literal UPDATE keeps the
  fast bulk `$set` path. Tests: `tests/test_sql_update_expr.py`. **Limitations:** a computed *composite-type*
  subfield is coerced as a scalar (nested composite value not rebuilt); a SET RHS that is a correlated
  subquery over another table isn't modelled.
- [x] ~~**`numeric`/`json`/`bytea` partial.**~~ Superseded (verified 2026-08-20). The
  `jsonb` half is simply wrong now: `->`, `->>`, `#>` and `@>` all evaluate — probed
  against `'{"a": {"b": 42}, "tags": ["x"]}'`, they return `{'b': 42}`, `42`, `42` and
  the containing row respectively. See "jsonb operator surface landed" and "SQL/JSON
  path queries landed (b135)". `numeric` still round-trips via Decimal128 and its real
  residual limit — 34 significant digits — has its own entry above.
- [ ] **Catalog surface: joins landed, column-level reflection still missing.**
  `information_schema.tables`/`.columns`/`.schemata` and `pg_catalog.pg_class`/
  `pg_namespace`/`pg_type`/`pg_database` are served as virtual tables, and JOINs / GROUP BY
  across them now execute (`virtual.CatalogBackend` + `planner._lookup_table_def`), so
  SQLAlchemy `get_table_names()`/`has_table()` and `psql`'s `\dt` work. WHERE now handles
  `CAST`/`::type`, `col = ANY(ARRAY[...])`, and the always-true `pg_table_is_visible`/
  `pg_type_is_visible` predicates. `pg_attribute`/`pg_attrdef`/`pg_description` virtual
  tables now exist (attrelid lines up with pg_class.oid), so column-level introspection
  *joins* resolve — e.g. `pg_attribute ⋈ pg_class ⋈ pg_namespace` returns a table's columns.
  The column-metadata query SQLAlchemy / `psql \d` emit now runs end to end: compound
  multi-condition join `ON`s (`… AND attnum > 0 AND NOT attisdropped`) compile to a `$lookup`
  sub-pipeline; scalar SELECT-list functions (`format_type`, `pg_get_expr`, `coalesce`), `CASE`,
  and correlated scalar subqueries are evaluated per row (`secantus.sql.scalar`); compound join
  `ON`s compile to a `$lookup` sub-pipeline; a `(SELECT … GROUP BY …) AS alias` derived table
  materializes into an ephemeral collection (`CatalogBackend.register_ephemeral`); `array_agg`
  (→ `$push`) and `pg_sequence`/`pg_collation`/`pg_constraint`/`pg_enum` (present-but-empty) +
  pg_type domain columns are modeled. **Full SQLAlchemy reflection now works end to end** —
  `get_columns` / `get_table_names` / `has_table` / `get_pk_constraint` / `get_indexes` /
  `get_foreign_keys` and whole-table `Table(autoload_with=...)`. PK/index reflection rides
  set-returning functions (`unnest` / `generate_subscripts` → row expansion in the evaluated
  executor), GROUP BY / `array_agg` over a derived-table FROM, populated `pg_index`/`pg_constraint`/
  `pg_am`/`pg_opclass`, and a fix so boolean expressions (`conrelid IS NOT NULL`) type as `bool`
  (else the wire text `'f'` reads truthy in `if row["has_constraint"]`). Column comments reflect as
  `None`. **Constraint / sequence `information_schema` views landed** (b75):
  `information_schema.table_constraints`, `key_column_usage`, and `constraint_column_usage` (built from
  `virtual._pk_constraints`) surface PRIMARY KEY (and now FOREIGN KEY) rows, so the standard PK
  reflection join (`table_constraints ⋈ key_column_usage`) that Alembic / SQLAlchemy's inspector emit
  resolves; `sequences` is present-but-empty (no sequences).
- [ ] **Enforcement made uniform across all write paths** (b98): the NOT NULL / CHECK / UNIQUE / FK
  validators now run on `MERGE` (its `INSERT` / `UPDATE` / `DELETE` actions in `engine._merge_apply_*`)
  and on `INSERT … ON CONFLICT` for constraints *other than the arbiter target* (the DO-insert branch
  and the DO-UPDATE post-image). Three shared entry points in `executor` — `enforce_insert_rows`,
  `enforce_update_images` (UNIQUE excludes the rewritten rows), `enforce_parent_delete` (FK referential
  actions) — are reused by `execute_insert`, `_execute_insert_on_conflict`, `_apply_conflict_update`,
  and the MERGE handlers, so every path enforces identically. Closes the MERGE-bypass and ON-CONFLICT
  secondary-constraint gaps noted in the b94/b95/b96 entries. **Still open:** deferred constraints
  aren't modeled (all checks are immediate — a future slice).
- [x] **Aggregate `FILTER (WHERE cond)` landed** (b126): `agg(...) FILTER (WHERE cond)` scopes an
  aggregate to matching rows. sqlglot parses it as `exp.Filter(this=<agg>, expression=Where(cond))`;
  the aggregate detectors (`_aggregate_of` / `_array_agg_arg` / `_string_agg_arg` / `_join_aggregate_of`)
  peel the Filter, and `_agg_filter_where` + `_filter_cond_to_agg` lower the predicate to a Mongo
  aggregation expression (comparisons, AND/OR/NOT, IS [NOT] NULL, bare boolean column). `_accumulator_for`
  gained a `filter_cond` param that wraps each accumulator in a `$cond` (neutral element 0 for sum/count,
  NULL for avg/min/max — the aggregate engine skips NULL there). Threaded through every accumulator site:
  single-table `$group`, GROUPING SETS, JOIN+GROUP, and the HAVING / ORDER BY resolvers (the HAVING
  comparison matcher recognises `exp.Filter` as the aggregate side). A lone `count(*) FILTER (...)` is
  kept off the simple-find fast path so the filter isn't dropped. **`FILTER` on the `$push` aggregates
  landed** (b208): `array_agg` / `string_agg` / `jsonb_object_agg` push a `None` sentinel for
  non-matching rows (`_push_filtered`) and drop it in the post-`$group` projection (`_array_agg_project`
  / `_jsonb_object_agg_project`; `string_agg`'s reduce already skips nulls) — across single-table,
  GROUPING SETS, and JOIN+GROUP paths. array_agg boxes matching values as `{v}` so a matching NULL
  survives the drop. **`FILTER` + `DISTINCT` landed** (b209): `count`/`sum`/`avg`(`DISTINCT x`)
  `FILTER (WHERE cond)` threads `fcond` into `_register_distinct_agg`, whose `$addToSet` now collects
  `_push_filtered(field, fcond)` — a non-matching row contributes `None`, which `_distinct_reduction`'s
  existing NULL filter drops, so only matching rows' distinct values count. Wired at every distinct-agg
  registration site (single-table + JOIN SELECT list, HAVING, and the group-window / hidden-agg
  closures — the last of which previously *silently dropped* a HAVING-only distinct FILTER). `min`/`max`
  (`DISTINCT`) already worked (they fall through to the plain accumulator, which threads `fcond`).
  `DISTINCT` count/sum/avg `FILTER` under GROUPING SETS also works (b211). **Not supported (→ `0A000`):**
  `FILTER` with an in-aggregate `ORDER BY` (the sorted-push path would need the sentinel threaded through
  the executor finish).
- [x] **`ALTER DOMAIN` landed** (b125): `ADD [CONSTRAINT c] CHECK (…) [NOT VALID]`, `DROP CONSTRAINT
  [IF EXISTS] c`, `SET DEFAULT expr` / `DROP DEFAULT`, `SET NOT NULL` / `DROP NOT NULL`, and `RENAME TO
  new`. Handled in `engine._alter_domain_command` (Command-parsed; catalog `update_domain`). `ADD …
  CHECK` and `SET NOT NULL` **re-validate every existing row** of every column typed with the domain
  (`_domain_columns` scans `catalog.list_tables`; `_revalidate_domain_check` → `23514`,
  `_revalidate_domain_not_null` → `23502`) and reject the ALTER without applying it if data would
  violate — `NOT VALID` skips the re-check (still enforced on new writes). Unnamed `ADD … CHECK`
  auto-names `<domain>_check[N]`; a duplicate explicit name → `42710`. `RENAME TO` re-keys the domain
  and repoints every referencing column's `domain_type` (columns track domains by name). **Not modeled:**
  `VALIDATE CONSTRAINT` (no-op accept — we validate eagerly), `RENAME CONSTRAINT`, dependency tracking.
- [x] **POSIX regex-match operators landed** (b124): `~` / `~*` / `!~` / `!~*` in WHERE lower to a Mongo
  `$regex` filter (`planner._expr_to_filter`, next to the LIKE handler; the pattern is a raw regex,
  *not* LIKE-translated, and matches unanchored — `re.search` semantics — like Postgres). `~*` adds
  `$options: "i"`; `!~` / `!~*` parse as `Not(RegexpLike/RegexpILike)` and negate through the existing
  `exp.Not` → `$nor` branch. In the scalar engine (SELECT-list booleans, CHECK constraints)
  `scalar._eval_regexp` runs Python `re.search`; `_BOOL_EXPR_TYPES` gained the two nodes so
  `(col ~ 'x')` types as `bool`. **This closes the regex gap in table + domain CHECK constraints.**
  **Limitation:** `!~` / `!~*` inherit the layer's existing NULL-in-negation divergence (a NULL row
  leaks into the negated result, shared with `!=` / `NOT LIKE`; the positive `~` correctly excludes
  NULL) — a broader NULL-semantics fix, not regex-specific.
- [x] **`CREATE DOMAIN` landed** (b122): a named base type with its own `NOT NULL` / `CHECK` (and
  optional `DEFAULT`). `CREATE DOMAIN name AS base [DEFAULT expr] [ [CONSTRAINT c] { NOT NULL | CHECK
  (…) } … ]` and `DROP DOMAIN [IF EXISTS] name` arrive as `exp.Command` (sqlglot doesn't model the
  grammar) and are handled in `engine._create_domain_command` / `_drop_domain_command`; the base type +
  constraints are re-parsed as a column def (`CREATE TABLE _ (value <body>)`) to reuse sqlglot's
  column-constraint grammar. Stored in the `__sql_domains__` catalog collection (`catalog.create_domain`
  / `get_domain` / `drop_domain` / `list_domains`). A domain-typed column stores as the domain's **base
  tag** (`Column.domain_type`; the planner tags any user type as `enum_type`, and
  `executor._resolve_user_type_column` disambiguates enum vs domain at `CREATE TABLE`, inheriting the
  domain `DEFAULT` when the column declares none). Enforcement (`executor._validate_domain_columns`,
  wired into `enforce_insert_rows` / `enforce_update_images` + the filter-update path): domain `NOT NULL`
  → `23502` (`domain <name> does not allow null values`), domain `CHECK` (references the value as
  `VALUE`) → `23514`; a `NULL` skips the `CHECK` (three-valued logic). Reflection: `pg_type` row
  `typtype='d'` with `typbasetype`/`typnotnull`, the column's `pg_attribute.atttypid` → domain oid, and
  each domain `CHECK` a `pg_constraint` row (`contype='c'`, `contypid`=domain oid). **Limitations:** the
  domain `CHECK` is evaluated by the scalar engine, so the `~` / `~*` regex-match operators aren't
  supported (→ `0A000`, same gap as table CHECKs); `ALTER DOMAIN` (add/drop constraint, set default) and
  domain-on-domain aren't modeled; `DROP DOMAIN` doesn't check for dependent columns (no RESTRICT/CASCADE
  dependency tracking).
- [x] **FOREIGN KEY enforcement on write landed** (b96): referential integrity is now enforced both
  ways (`23503`, `errors.foreign_key_violation`). **Child side** (`executor._validate_fk_child_rows`,
  wired into `execute_insert` + the UPDATE post-image path): an INSERT/UPDATE row whose FK columns are
  all non-NULL must have a matching parent row — MATCH SIMPLE, so a NULL in any FK column exempts the
  row; empty ref-column lists (`REFERENCES t`) resolve to the parent PK (`_fk_ref_columns`). **Parent
  side** (`_enforce_fk_on_parent_delete` from `execute_delete`, `_enforce_fk_on_parent_update` from the
  UPDATE path): deleting/updating a referenced row applies the declared action — NO ACTION / RESTRICT
  reject, `ON DELETE CASCADE` deletes children recursively (depth-guarded at 20), SET NULL / SET DEFAULT
  clear the child FK columns (`_fk_clear_value` uses the column default for SET DEFAULT). Reverse-FK
  lookup (`_referencing_fks`) scans the catalog. `execute_delete` / `execute_update` now take
  `catalog` + `session`; reflected tables have no FKs → ungated. **Limitations:** deferred constraints
  aren't modeled (checks are immediate — but see #69); parent-side
  UPDATE only fires when a referenced column actually changes (references usually target the immutable
  PK/`_id`); no cross-database FKs. (Historical note, now closed: `MERGE` writes *do* enforce — child-side
  via `enforce_insert_rows` / `enforce_update_images`, parent-side FK on a MERGE UPDATE via
  `_enforce_fk_on_parent_update`, and parent-side on a MERGE DELETE via `enforce_parent_delete` — see the
  MERGE bullet.)
- [x] **UNIQUE enforcement on write landed** (b95): `INSERT` / `UPDATE` on a **declared** table now
  reject a write that would create two rows sharing a value for a declared UNIQUE constraint (`23505`,
  `executor._validate_unique_rows`). NULLs are distinct — a row with any NULL in a constraint's columns
  is exempt (matches Postgres default, no `NULLS NOT DISTINCT`). Duplicates *within* an INSERT/UPDATE
  batch collide, and each row is probed against stored rows; an UPDATE excludes every row it is
  rewriting (`exclude_ids` = matched `_id`s) so unchanged rows and value-swaps across the matched set
  don't self-conflict. Wired into `execute_insert` and `execute_update` (via
  `_validate_update_post_images`, which now also does UNIQUE). The PK is still enforced separately by
  storage's `_id` index (code 11000 → 23505 in `_raise_write_error`). **Limitations:** no `NULLS NOT
  DISTINCT`. (Historical note, now closed by #67: `INSERT … ON CONFLICT` catches a secondary UNIQUE — the
  clean-insert branch runs `enforce_insert_rows` over every UNIQUE constraint, not just the arbiter — and
  `MERGE` writes enforce through the same shared helpers.)
- [x] **CHECK + NOT NULL enforcement on write landed** (b94): `INSERT` / `UPDATE` on a **declared**
  table now enforce NOT NULL (`23502`) and CHECK (`23514`) against the post-image — a violating write is
  rejected and the table left unchanged (`executor._validate_write_row` / `_validate_rows` /
  `_validate_update_post_images`). NOT NULL skips the PK column (storage auto-assigns `_id`). CHECK
  predicates are parsed from their stored text (`_parse_check_expr`, lru-cached) and evaluated per row
  via `scalar.evaluate` with a column→field scope; a predicate that returns NULL passes (Postgres
  three-valued semantics — comparisons with NULL yield None in `scalar._eval_compare`). Wired into
  `execute_insert`, both `INSERT … ON CONFLICT` branches (insert + DO UPDATE post-image), INSERT…SELECT
  (rows flow through `execute_insert`), and `execute_update` (post-image computed with
  `secantus.update.apply_update` before the storage write). Reflected (schema-on-read) tables carry no
  declared constraints, so their writes are never gated. **Limitations:** a multi-statement failure isn't
  rolled back unless inside an explicit transaction block (per-statement atomicity only for the failing
  statement). (Historical note, now closed: UNIQUE (#64) / FOREIGN KEY (#65) are enforced, and `MERGE`
  writes go through the shared enforcement helpers (#67), not a bypass.)
- [x] **CHECK / UNIQUE constraints — declared, reflected, and ENFORCED** (b91 declared them;
  b98 made enforcement uniform across every write path — headline corrected 2026-08-20, the
  "NOT enforced" wording had survived the slice that fixed it). Probed live: `INSERT` of `-5`
  into `n int CHECK (n > 0)` raises `23514 new row for relation "t" violates check constraint
  "t_n_check"`, and a duplicate into `e text UNIQUE` raises `23505 duplicate key value violates
  unique constraint "t_e_key"` — Postgres' SQLSTATEs and message wording. The declaration /
  reflection detail below is still accurate: column-level (`col int
  CHECK (col > 0)` / `col text UNIQUE`), table-level named (`CONSTRAINT c CHECK (...)` / `... UNIQUE (a,
  b)`), and table-level unnamed CHECK/UNIQUE are parsed by `planner._extract_constraints`, stored on
  `TableDef.check_constraints` (`catalog.CheckConstraint`) / `TableDef.unique_constraints`
  (`catalog.UniqueConstraint`), and persisted in the catalog doc. Unnamed constraints get Postgres'
  default names (`<table>_<col>_key`, `<table>_<col>_check`). Reflection: `pg_catalog.pg_constraint`
  gains `contype='u'`/`'c'` rows (`virtual._unique_constraints` / `_check_constraints`, oid bases
  45000/47000); each UNIQUE is backed by an implicit unique index (`_unique_constraint_index_relations`,
  idx oid base 46000) whose `indexrelid == conindid`, because SQLAlchemy's `get_unique_constraints`
  joins `pg_constraint.conindid = pg_index.indexrelid` and reads columns from `unnest(indkey)`.
  `information_schema.table_constraints` / `.check_constraints` (new) / `.key_column_usage` /
  `.constraint_column_usage` gain rows; `pg_get_constraintdef(oid)` renders `UNIQUE (…)` and `CHECK
  ((…))`. SQLAlchemy's `get_unique_constraints()` / `get_check_constraints()` resolve end to end. **Not
  enforced:** no CHECK-predicate validation, no UNIQUE-duplicate rejection on write — schema-shape
  record only. **Limitations:** CHECK columns aren't listed in `constraint_column_usage` (the
  predicate isn't parsed for referenced columns).
- [x] **`ALTER TABLE … ADD CONSTRAINT CHECK/UNIQUE` + `DROP CONSTRAINT` landed** (b93): `ADD
  [CONSTRAINT name] CHECK (…)` / `UNIQUE (…)` and unnamed `ADD UNIQUE (…)` append to
  `TableDef.check_constraints` / `unique_constraints` via `executor._apply_alter_action` (reusing
  `planner.make_check_constraint` / `make_unique_constraint`, factored out of `_extract_constraints`);
  they reflect exactly like a CREATE TABLE CHECK/UNIQUE (b91). `DROP CONSTRAINT [IF EXISTS] name`
  removes a declared FK / CHECK / UNIQUE by name (dispatched on `exp.Drop` kind='CONSTRAINT'; unknown
  name → `42704` unless IF EXISTS). Still not enforced. **Limitations:** unnamed `ADD CHECK (…)` isn't
  accepted (sqlglot can't parse it — a CHECK needs an explicit `CONSTRAINT name`); no `ALTER CONSTRAINT`
  / `VALIDATE CONSTRAINT`.
- [x] **Materialized-view polish landed** (b99): `WITH NO DATA` registers a matview unpopulated (a
  `populated` flag in the `__sql_matviews__` registry doc — `catalog.matview_populated` /
  `set_matview_populated`); querying an unpopulated matview errors `55000`
  (`object_not_in_prerequisite_state`, checked in `engine._run_select`), and its first `REFRESH` marks
  it populated. `WITH DATA` is the default (returns `SELECT N`; `WITH NO DATA` returns `CREATE
  MATERIALIZED VIEW`). **`REFRESH … CONCURRENTLY` now enforces its Postgres prerequisites (#154, b193):**
  `_refresh_matview` detects the `CONCURRENTLY` keyword and rejects it (`0A000` + the PG unique-index
  hint) unless the matview is already populated *and* carries a unique index (`storage.list_indexes` →
  any `unique`); otherwise it recomputes the snapshot exactly as a plain refresh (there is no true
  diff-based concurrent apply, and Postgres has no *incremental* matview refresh to model). `ALTER MATERIALIZED
  VIEW name RENAME TO new` moves the registry entry, the catalog `TableDef`, and the backing collection
  (`storage.rename_collection`), preserving the populated flag. All three (`CREATE … WITH [NO] DATA`,
  `REFRESH … CONCURRENTLY`, `ALTER … RENAME`) parse as `exp.Command` — sqlglot can't parse them as DDL —
  and are handled in the Command dispatch (`_create_matview_command` / `_alter_matview_command`).
  `_CTECatalog` gained `get_matview` / `matview_populated` delegation so the scannability check works
  inside a WITH. **Limitations:** the unpopulated check only fires for a matview in the query's primary
  FROM (not a secondary JOIN position); no real `CONCURRENTLY` snapshot isolation; still no indexes on
  the snapshot.
- [x] **Materialized views landed** (b97): `CREATE MATERIALIZED VIEW name AS SELECT …` runs the SELECT
  and stores a **snapshot** of its rows in a backing collection (named after the matview) plus the
  definition text in a per-db `__sql_matviews__` registry (`catalog.put_matview` / `get_matview` /
  `drop_matview` / `list_matviews`). The snapshot's shape is registered as a catalog `TableDef` (columns
  = the SELECT outputs) so `SELECT * FROM mv` projects the view's columns, not the storage-assigned
  `_id`. `REFRESH MATERIALIZED VIEW name` (parses as an `exp.Command` — sqlglot can't parse it as DDL —
  handled in `engine._refresh_matview`) re-runs the SELECT and replaces the snapshot rows; `DROP
  MATERIALIZED VIEW [IF EXISTS]` (`exp.Drop` kind='VIEW' `materialized=True`) drops the registry entry,
  catalog `TableDef`, and backing collection. CREATE detection uses the `MaterializedProperty` in
  `exp.Create.properties` (`_is_materialized`). Reflection: `pg_class` reports `relkind='m'` for matview
  names (`virtual._matview_names`), `pg_get_viewdef(oid)` returns the definition (`viewdef_for_oid`
  extended), and matviews are **excluded** from `information_schema.tables` (matching Postgres).
  SQLAlchemy's `get_materialized_view_names()` reflects them; they don't appear in `get_table_names()`.
  **Limitations:** no incremental refresh (full recompute only). (Historical note, now closed by #68:
  `WITH NO DATA` / `WITH DATA` *are* modeled — an unpopulated matview raises `55000` on scan until its
  first `REFRESH` — and `REFRESH … CONCURRENTLY` + a unique index on the snapshot + `ALTER MATERIALIZED
  VIEW … RENAME TO` are supported; other `ALTER MATERIALIZED VIEW` subcommands remain `0A000`.)
- [x] **`CREATE VIEW` / `DROP VIEW` landed** (b87): a view is a stored `SELECT` persisted as its query
  text in a per-db `__sql_views__` collection (`catalog.put_view` / `get_view` / `drop_view` /
  `list_views`). `CREATE [OR REPLACE] VIEW` and `DROP VIEW [IF EXISTS]` dispatch on `exp.Create` /
  `exp.Drop` kind `'VIEW'` (`executor.execute_create_view` / `execute_drop_view`). Querying a view
  works by inline expansion: `engine._expand_views` rewrites every `FROM` / `JOIN` reference to a
  declared view into a `(<view def>) AS name` subquery before dispatch, so single-table reads,
  aggregates, joins against real tables, and nested views (a view over a view, expanded recursively
  with a depth guard) all ride the existing derived-table (`_resolve_source`) machinery.
  `select_needs_pipeline` now routes a from-subquery to the pipeline path so a bare `SELECT * FROM
  view` materializes. CTE names defined in the same statement shadow views. Reflection: `pg_class`
  gains `relkind='v'` rows (`virtual._view_oids`, base 50000), `pg_get_viewdef(oid)`
  (`scalar._call_func` → `virtual.viewdef_for_oid`), `information_schema.views`, and a `table_type='VIEW'`
  row in `information_schema.tables` — SQLAlchemy's `get_view_names()` / `get_view_definition()` reflect
  end to end. **Writable views landed (#146, b186):** INSERT / UPDATE / DELETE through an
  *automatically-updatable* view rewrite onto the base table (`engine._rewrite_write_through_view` /
  `_updatable_view_base`, run before the write branches). Updatable = the PG auto-updatable subset: one base
  table (no join / set-op), no DISTINCT / GROUP BY / HAVING / window / LIMIT / WITH, and every output column
  a plain **unaliased** base column (or `*`) — so view names equal base names and no column remap is needed.
  A view's `WHERE` is AND-ed into UPDATE/DELETE (rows outside the view can't be touched). Anything else raises
  `0A000` ("not an automatically-updatable view" — PG would need an INSTEAD OF trigger). Tests:
  `test_sql_views.py` (writable-view section). **`WITH CHECK OPTION` landed (#164, b199):**
  `CREATE VIEW … WITH [LOCAL|CASCADED] CHECK OPTION` exceeds sqlglot (→ Command), so
  `_create_view_check_option_command` strips the clause, re-parses the inner CREATE VIEW, and stores the
  mode on the view doc (`catalog.put_view(..., check_option=)` / `get_view_check_option`). On write-through
  `_rewrite_write_through_view` returns the view's WHERE as a `(predicate, view_name)` pair threaded onto
  `InsertPlan.check_option` / `UpdatePlan.check_option`; `executor._validate_check_option` rejects any
  INSERT / UPDATE post-image (incl. the ON CONFLICT insert + DO UPDATE branches and the computed /
  materialized UPDATE path) whose row is not visible through the view — the predicate not TRUE (FALSE *or*
  NULL) → `44000`. Reflected in `information_schema.views.check_option`. **Still:** not materialized (each
  query re-reads the base tables); no `CASCADE`/`RESTRICT` on `DROP`; no column-list aliasing (`CREATE VIEW
  v (a, b) AS …`); CHECK OPTION cascades only one level (a CASCADED view over another CHECK OPTION view
  doesn't re-check the inner condition); aliased / expression projections aren't updatable.
- [x] **`COMMENT ON TABLE` / `COLUMN` landed** (b86): the comment is stored on `TableDef.comment` /
  `Column.comment` (persisted in the catalog doc) by `executor.execute_comment` (dispatched on
  `exp.Comment`), surfaced through `virtual._pg_description` (table comment → `objsubid 0`, column
  comment → the column's attnum, `classoid` = pg_class 1259). SQLAlchemy's `get_table_comment()` and
  the `comment` field of `get_columns()` reflect them. `COMMENT ON … IS NULL` removes the comment
  (sqlglot can't parse a NULL comment expression, so `planner.parse` rewrites a whole `COMMENT ON … IS
  NULL` statement — anchored so a query's `WHERE x IS NULL` is untouched — to an `UNCOMMENT_SENTINEL`
  the executor reads as removal). `get_table_comment`'s join needs `'pg_catalog.pg_class'::regclass`, so
  `_coerce_cast` now maps a `regclass` cast of a catalog relation name to its OID (`_REGCLASS_OIDS`).
- [x] **Foreign keys — declared, reflected, and ENFORCED** (b81 declared them; b98 made
  enforcement uniform — headline corrected 2026-08-20, same stale-wording problem as the
  CHECK/UNIQUE entry). Probed live: inserting a child row whose `pid` has no parent raises
  `23503 insert or update on table "child" violates foreign key constraint "child_pid_fkey"`.
  The declaration / reflection detail below is still accurate: column-level `col type REFERENCES
  t(c)` and table-level `FOREIGN KEY (c) REFERENCES t(c)` (incl. `ON DELETE` / `ON UPDATE` actions and
  the columnless `REFERENCES t` → target-PK form) are parsed by `planner._extract_foreign_keys`, stored
  on `TableDef.foreign_keys` (`catalog.ForeignKey`), and persisted in the catalog doc. Reflection:
  `information_schema.referential_constraints` (one row per FK), FK rows added to `table_constraints` /
  `key_column_usage` / `constraint_column_usage`, `pg_catalog.pg_constraint` gains `contype='f'` rows
  (`conrelid`/`confrelid`/`conkey`/`confkey`), and `pg_get_constraintdef(oid)` renders the `FOREIGN KEY
  (…) REFERENCES …` string (`virtual._foreign_keys` / `constraint_def_for_oid`, called from
  `scalar._call_func`). SQLAlchemy's `Inspector.get_foreign_keys()` + full `MetaData.reflect()` resolve
  the relationship end to end. **Not enforced:** no referential-integrity check on insert/update/delete
  — this is a schema-shape record only. Adding a FK after the fact via `ALTER TABLE … ADD [CONSTRAINT
  name] FOREIGN KEY` landed in b85 (see below). **Limitations:** `MATCH` renders as the default.
  (FK enforcement + referential actions landed in later slices; `DEFERRABLE` is captured and
  honoured — see "Constraint enforcement" and "Deferred constraints" below.)
- [x] **`ALTER TABLE … ADD [CONSTRAINT name] FOREIGN KEY` landed** (b85): parsed as `exp.AddConstraint`
  (a bare `ForeignKey` or a named `Constraint` wrapping one) in `executor._apply_alter_action`, which
  appends a `catalog.ForeignKey` (via `planner._make_fk`, now taking an optional constraint name) to
  the table and persists it through `Catalog.replace`. Reflects exactly like a CREATE TABLE FK
  (`information_schema.referential_constraints`, `pg_constraint` contype='f', SQLAlchemy
  `get_foreign_keys()`). Non-FK `ADD CONSTRAINT` (CHECK / UNIQUE) → `feature_not_supported`. Still not
  enforced.
- [x] **Deferred constraints landed** (b100): `UNIQUE` / `FOREIGN KEY` declared `DEFERRABLE` /
  `INITIALLY DEFERRED` are parsed (`planner._deferrable_flags`), stored on `catalog.UniqueConstraint` /
  `ForeignKey` (`deferrable` / `initially_deferred`), and reflected via `pg_constraint.condeferrable` /
  `condeferred` and `information_schema.table_constraints.is_deferrable` / `initially_deferred`. When a
  deferrable constraint is currently deferred inside a transaction, `executor._maybe_defer` records a
  pending `(kind, table, name)` re-check on the `Session` instead of raising; `executor.flush_deferred`
  re-validates the whole constraint against the in-txn state at `COMMIT` (`engine._commit_txn`, which
  aborts + re-raises `23505` / `23503` on a surviving violation) or at `SET CONSTRAINTS … IMMEDIATE`
  (`engine._set_constraints_command`; supports `ALL` and named forms, `DEFERRED` / `IMMEDIATE`). Session
  deferral state (`deferred_all` / `deferred_names` / `pending_deferred`) resets at end of transaction.
  **Limitations:** re-check is a whole-constraint rescan (not per-row). (The named-FK parsing gap this
  note used to describe was fixed in b101 — see "Named FK constraint parsing" below.)
- [x] **Named FK constraint parsing landed** (b101): `planner._extract_foreign_keys` now handles a
  table-level `CONSTRAINT n FOREIGN KEY (cols) REFERENCES …` (sqlglot wraps it in an `exp.Constraint`
  whose `expressions` hold the `exp.ForeignKey`) — previously not parsed into a FK **at all** — and a
  column-level `col … CONSTRAINT n REFERENCES …` now keeps the explicit name (read from the
  `ColumnConstraint`'s `this`) instead of falling back to the auto `<table>_<col>_fkey`. Shared
  `_fk_from_node` builds the `ForeignKey` from an `exp.ForeignKey` node (columns + reference), threading
  the name through `_make_fk`; composite columns, `ON DELETE`/`ON UPDATE`, and `DEFERRABLE` all carry
  through. Enforcement + reflection + named `SET CONSTRAINTS` all light up under the real name. Tests:
  `tests/test_sql_foreign_keys.py`.
- [x] **SERIAL columns + sequences landed** (b102): `SERIAL` / `BIGSERIAL` / `SMALLSERIAL` columns
  (int + implicit NOT NULL + owned sequence `<table>_<col>_seq`), `CREATE SEQUENCE` / `DROP SEQUENCE`
  (`START WITH` / `INCREMENT BY` / `MINVALUE` / `MAXVALUE` / `CYCLE`), `DEFAULT nextval('seq')`, and the
  `nextval` / `currval` / `setval` / `lastval` functions. Sequence state persists in a per-db
  `__sql_sequences__` collection (`Catalog.create_sequence` / `sequence_nextval` / `sequence_setval`);
  `Column.sequence` marks a sequence-backed column, filled by `executor._assign_sequences` at INSERT
  (planner leaves it unset — planning is storage-free). currval/lastval are per-session
  (`Session.seq_values` / `record_sequence_value`, error 55000 before first nextval); the FROM-less
  `SELECT nextval(...)` path routes through the scalar evaluator (`plan_constant_select` now takes
  storage/catalog/db). Reflection: `pg_class` relkind='S', `information_schema.sequences`,
  `pg_catalog.pg_sequence`. Overflow past MAXVALUE → 2200H (CYCLE wraps). Tests:
  `tests/test_sql_sequences.py`. **Limitations:** `nextval` is a read-modify-write (not a single atomic
  op) — a small duplicate-value window exists under truly concurrent `nextval` on the same sequence from
  different connections (acceptable for the dev/test surface; the storage RLock keeps each write
  atomic); no `ALTER SEQUENCE`, no `CACHE`, no `OWNED BY`, and an explicit value into a SERIAL column
  doesn't bump the sequence (matches Postgres).
- [x] **SQL-level roles landed** (b103): `CREATE ROLE` / `CREATE USER` / `ALTER ROLE` / `DROP ROLE`
  (all arrive as `exp.Command`; parsed by `engine._run_role_command` / `_parse_role_attrs` — `LOGIN` /
  `SUPERUSER` / `CREATEDB` / `CREATEROLE` / `INHERIT` / `REPLICATION` + `NO…` negations, `PASSWORD`,
  `CONNECTION LIMIT`; `USER` implies LOGIN). Stored in a per-db `__sql_roles__` collection
  (`Catalog.put_role` / `get_role` / `drop_role` / `list_roles`), reflected via `pg_catalog.pg_roles`
  (`_pg_roles`), which also surfaces the connecting `session.user` as a superuser login role (Postgres
  bootstrap-superuser analogue). Role-membership `GRANT`/`REVOKE` (via `Command`, e.g. `GRANT admin TO
  alice`) and grants on schemas/databases/sequences are accepted as no-ops; table-privilege
  `GRANT`/`REVOKE` is enforced (next entry). Tests: `tests/test_sql_roles.py`. **Limitations:**
  roles are a reflection / DDL-acceptance record only — **distinct from the wire server's SCRAM auth
  users** (constructor `users={}`), no `pg_authid` / `pg_auth_members` / role-membership graph, password
  not stored (only a `password_set` flag), and roles live in the connection's db rather than being
  cluster-wide.
- [x] **Enforced table-level GRANT/REVOKE landed** (#127, b167): `GRANT`/`REVOKE` of
  `SELECT`/`INSERT`/`UPDATE`/`DELETE` (or `ALL`) `ON <table> TO/FROM <role>` (`exp.Grant`/`exp.Revoke`
  with a `Table` securable) persist per-`(table, grantee)` in `__sql_grants__`
  (`Catalog.grant_table_privileges` / `revoke_table_privileges` / `get_table_grants` /
  `list_table_grants` / `has_table_privilege`; `engine._run_grant`). Enforced as an **additive** layer
  over the Mongo RBAC gate in `authz.authorize`: a data op is allowed when the Mongo role covers it *or*
  a table grant does (grantee = the session user, one of its role names, or `PUBLIC`), gated on
  `session.authz_active` like the rest of the SQL RBAC (trust mode / embedded `run_sql` record but don't
  enforce). Surfaced via `information_schema.role_table_grants` / `.table_privileges`
  (`virtual._info_table_grants`) and `has_table_privilege([user,] table, privilege)`
  (`scalar._has_table_privilege`). Tests: `tests/test_sql_grants.py` +
  `test_pgserver_pg8000.py::test_grant_revoke_reflection_via_driver`. **Limitations:** additive only —
  a table grant never *restricts* a broader Mongo role (`readWrite` still writes any table regardless of
  table grants); no per-column / `WITH GRANT OPTION` enforcement (the flag is stored + reflected but
  re-grant chains aren't checked); `TRUNCATE`/`REFERENCES`/`TRIGGER` recorded for `ALL` fidelity but not
  enforced (no such ops); no table-owner tracking (owners aren't auto-granted — the seeding/trust-mode
  session is unrestricted anyway); grant target must be a single identifiable table (multi-table /
  subquery statements get no table-grant fallback). Not ported to the Rust server.
- [x] **SET ROLE / SET SESSION AUTHORIZATION landed** (#128, b168): `SET [SESSION|LOCAL] ROLE { name |
  NONE | DEFAULT }`, `SET [SESSION|LOCAL] SESSION AUTHORIZATION { name | DEFAULT }`, and their `RESET`
  forms (all arrive as `exp.Command`; handled by `engine._run_authorization_command`). `Session.role` is
  the current-role override (SET ROLE), `Session.user` the session user (SET SESSION AUTHORIZATION),
  `Session.login_user` the immutable login (captured in `__post_init__`) for RESET; `effective_user`
  (`role or user`) drives `current_user` / `current_role` / `user` and the #127 table-grant identity,
  while `session_user` reports `Session.user`. `session_user` (`exp.SessionUser`) and the keyword
  synonyms `current_role` / `user` (bare `exp.Column` in a FROM-less SELECT) now resolve via
  `functions.evaluate_scalar` / `is_scalar_function`. `SHOW role` / `current_setting('role')` track the
  current role. Escalation guard (`_can_assume_identity`): with `authz_active`, a session may assume only
  its login, a role in its bindings, or anything as `root` — else `42501`; trust mode is unrestricted.
  Tests: `tests/test_sql_set_role.py` + `test_pgserver_pg8000.py::test_set_role_and_session_authorization_via_driver`.
  **Limitations:** no role-membership graph beyond the session's own bindings (SET ROLE to an arbitrary
  granted-but-unbound role isn't validated against `pg_auth_members`); `SET LOCAL` isn't scoped to the
  transaction (behaves like `SET`); the Mongo RBAC db-level gate still uses the login's `session.roles`
  (SET ROLE changes the table-grant identity + `current_user`, not the underlying db-wide Mongo role
  bindings). Not ported to the Rust server.
- [x] **Row-level security (RLS) landed** (#129, b169): `ALTER TABLE t {ENABLE|DISABLE|FORCE|NO FORCE}
  ROW LEVEL SECURITY` + `CREATE POLICY name ON t [AS PERMISSIVE|RESTRICTIVE] [FOR cmd] [TO roles]
  [USING (expr)] [WITH CHECK (expr)]` + `DROP POLICY [IF EXISTS] name ON t` (all `exp.Command`;
  regex-parsed in `engine._alter_rls_command` / `_create_policy_command` / `_drop_policy_command`,
  balanced-paren extraction via `_paren_after`). Persisted in `__sql_rls__` (per-table enabled/forced)
  and `__sql_policies__` (per-`(table, name)`: command / roles / permissive / using / check) via
  `Catalog.set_rls` / `get_rls` / `create_policy` / `drop_policy` / `get_policies` / `list_policies`.
  Enforcement in `secantus.sql.rls`: `apply_read` AND-injects the combined USING predicate (permissive
  OR'd, restrictive AND'd, `current_user`/`session_user` substituted to string literals) into a
  single-table SELECT/UPDATE/DELETE WHERE (via `engine._apply_rls_read` before dispatch);
  `check_write_row` validates the combined WITH CHECK (falling back to USING) per new row on
  INSERT/UPDATE (wired into `executor._validate_rls_check` at the two write chokepoints), raising `42501`.
  Default-deny (RLS on, no applicable permissive policy) renders as `WHERE FALSE` — `planner._expr_to_filter`
  now lowers `exp.Boolean` (`TRUE`→`{}`, `FALSE`→`{"$nor":[{}]}`). Gated on `session.authz_active`, `root`
  bypasses; trust mode / embedded record but don't enforce. Reflected via `pg_catalog.pg_policies`
  (`virtual._pg_policies`). Tests: `tests/test_sql_rls.py` + `test_pgserver_pg8000.py::test_row_level_security_reflection_via_driver`.
  **Limitations:** USING injection is single-table only (a join doesn't get the base table's policy — COLLSCAN-safe but unfiltered on joined reads);
  no table-owner concept (RLS applies to every non-root role under active authz, not "all but the owner"); `FORCE` is
  recorded but behaves like `ENABLE` (no owner to force against); RLS DDL itself needs no privilege (any authenticated
  user can add/alter policies — no ownership check); policies over the pipeline/set-operation/CTE paths aren't injected
  (only the direct single-table SELECT/UPDATE/DELETE dispatch). Not ported to the Rust server.
- [x] **UDF reflection landed** (#130, b170): `CREATE FUNCTION` (#124) definitions now surface through
  `pg_catalog.pg_proc` (`virtual._pg_proc`: oid / proname / pronamespace / prolang=14 / prorettype /
  pronargs / proargtypes / proargnames / prosrc / prokind='f' / proretset), `information_schema.routines`
  + `.parameters` (`_info_routines` / `_info_parameters`), and `pg_get_functiondef` /
  `pg_get_function_arguments` / `pg_get_function_result` (`virtual.functiondef_for_oid` /
  `function_arguments_for_oid` / `function_result_for_oid`, wired in `scalar._call_func` + registered in
  `functions._SCALAR_EVAL_ANON` so FROM-less calls defer to the scalar evaluator). `CREATE FUNCTION` now
  also stores `param_types` (via `engine._function_param_types`) so arg types reflect. Stable oids from
  `_FUNCTION_OID_BASE = 65000`. Tests: `tests/test_sql_udf_reflection.py` +
  `test_pgserver_pg8000.py::test_udf_reflection_via_driver`. **Limitations:** `pg_proc` lists only
  user-defined functions (built-ins aren't enumerated — a `\df` of a builtin shows nothing); no
  `proargmodes` / `proargdefaults` (all params reflect as `IN`, no defaults); `is_deterministic` is a
  fixed `NO`; overloads share a `proname` but get distinct oids/`specific_name`. Not ported to the Rust
  server.
- [x] **Column-level privileges landed** (#131, b171): `GRANT`/`REVOKE` `SELECT`/`INSERT`/`UPDATE (col,
  …)` `ON t` (the `GrantPrivilege.expressions` column list) persist per-`(table, grantee, column)` in
  `__sql_column_grants__` (`Catalog.grant_column_privileges` / `revoke_column_privileges` /
  `get_column_grants` / `list_column_grants` / `has_column_privilege`; `engine._grant_privileges` now
  splits table- vs column-scoped grants, `_run_grant` routes both). Enforced additively in
  `authz._table_grant_allows`: when the Mongo role and whole-table grant don't cover the op, allow only
  when *every* column the statement touches is column-granted (`_touched_columns`: SELECT projection +
  WHERE `exp.Column`s, `SELECT *`/list-less INSERT expand to the table's columns, UPDATE `SET` targets).
  Reflected via `information_schema.column_privileges` (`virtual._info_column_grants`) and
  `has_column_privilege([user,] table, column, privilege)` (`scalar._has_column_privilege`; a whole-table
  grant satisfies it). Tests: `tests/test_sql_column_grants.py` +
  `test_pgserver_pg8000.py::test_column_privileges_reflection_via_driver`. **Limitations:** column-grant
  enforcement is single-table only (multi-table/join SELECTs get no column-grant fallback — they need a
  role or table grant); `count(*)`/no-column-ref SELECTs fall back to table-level (can't be authorized by
  a column grant alone); `REFERENCES`/`TRIGGER` column privileges aren't enforced; `is_grantable` always
  `NO`. Not ported to the Rust server.
- [x] **IDENTITY columns + ALTER SEQUENCE landed** (b104): `GENERATED { ALWAYS | BY DEFAULT } AS
  IDENTITY [(START WITH n INCREMENT BY n)]` columns (`planner._identity_spec`) reuse the SERIAL sequence
  machinery — an owned `<table>_<col>_seq`, NOT NULL, auto-filled on omit. `Column.identity` is
  `"always"` / `"by_default"`; ALWAYS rejects a user-supplied value with `428C9` but accepts the
  `DEFAULT` keyword (a VALUES `DEFAULT` cell is now filtered to "omitted" in `plan_insert` via
  `_is_default_cell` — this also fixed general `VALUES (DEFAULT)` handling). `ALTER SEQUENCE [IF EXISTS]
  name { RESTART [WITH n] | INCREMENT BY n | MINVALUE | MAXVALUE | START WITH | [NO] CYCLE }…` arrives as
  a Command (`engine._alter_sequence_command` / `_parse_alter_sequence_opts` → `Catalog.alter_sequence`).
  Reflection: `pg_attribute.attidentity` (`'a'` / `'d'`), `atthasdef` now true for sequence/default
  columns. Tests: `tests/test_sql_identity.py`. **Limitations:** no `OVERRIDING SYSTEM VALUE` (so an
  ALWAYS column can't be force-overridden), no `ALTER TABLE … ADD/DROP/SET GENERATED`, no
  `ALTER SEQUENCE … OWNED BY` / `RESTART` distinction from `is_called` edge cases beyond the basic reset.
- [x] **Enum types landed** (b107): `CREATE TYPE name AS ENUM ('a', …)` / `DROP TYPE [IF EXISTS]` store
  the label list in a per-db `__sql_enums__` collection (`Catalog.create_enum` / `get_enum` / `drop_enum`
  / `list_enums`); dispatched from `engine._create_type` / `_drop_type`. An enum-typed column
  (`Column.enum_type`, stored as `text`) is detected in `plan_create_table` via `_enum_type_name` (a
  USERDEFINED DataType); `execute_create_table` verifies the enum exists (else `42704`), and
  `executor._validate_enum_columns` rejects a value outside the labels with `22P02` on every write path
  (INSERT / UPDATE / ON CONFLICT / MERGE). Reflection: `pg_type` (`typtype='e'`, oid base 65000, minted
  by `Catalog.enum_type_oids` — allocation-stable, persisted at CREATE, never renumbered/reused), `pg_enum`
  (label rows with `enumsortorder`), and enum columns' `pg_attribute.atttypid` → the enum oid.
  `RowDescription` reports the same minted oid for enum result columns (SELECT / correlated SELECT /
  RETURNING incl. MERGE / extended-protocol Describe) via `executor._out_column_descs`, so psycopg's
  `EnumInfo.fetch` + `register_enum` round-trips to Python enum members. User types (enum / domain /
  composite) report a derived `pg_type.typarray` (`oid + USER_TYPE_ARRAY_OID_OFFSET`) — never 0, which
  psycopg's `test_register_scope` used to pop the global INVALID_OID fallback loader (the 212-test
  "unknown oid loader not found" gauge cluster). Casts to a declared enum (`'ok'::mood` / `%s::mood` /
  `%s::mood[]`) validate labels (22P02) and describe with the enum oid (arrays: the paired array oid —
  `scalar.enum_cast_target` / `planner._constant_enum_override`); a Bind parameter declared with an enum
  oid is label-validated (`pgextended._check_enum_param`); binary array params/results handle user-type
  array oids (elements travel as text); `oid::regtype::text` quotes mixed-case names
  (`virtual.quote_type_name`). psycopg's `tests/types/test_enum.py` passes 197/197. Tests:
  `tests/test_sql_enum.py`, `tests/test_pgserver_psycopg.py`. **Limitations:** only the ENUM form of
  `CREATE TYPE` (composite / range / base types → `0A000`); enums live in the connection's db, not
  schema-scoped; JOIN / GROUP BY / evaluated-expression plans (`PipelineSelectPlan` /
  `EvaluatedSelectPlan`, `out_columns` as `(name, tag)` pairs) drop the enum tag at plan time so those
  result columns still describe as `text` 25; enum-cast oids ride constant selects only (a cast inside a
  table SELECT's projection types by the column machinery); no `pg_type` row for the paired `_name`
  array type itself (only `typarray` on the base row); `mood[]` table columns aren't supported.
- [x] **`ALTER TYPE … ADD VALUE` + enum-aware ORDER BY landed** (b112): `ALTER TYPE name ADD VALUE
  [IF NOT EXISTS] 'label' [BEFORE|AFTER 'other']` (arrives as a `Command`, parsed by
  `engine._ALTER_TYPE_ADD_RE` → `Catalog.alter_enum_add_value`) inserts a new label into the enum's
  ordered label list at the end or relative to a neighbour; duplicate → `42710` (unless `IF NOT EXISTS`),
  missing type / neighbour → `42704`. A single-table `ORDER BY` on an enum column now sorts by the
  **declared** label order: `planner._enum_order_map` records the label list on `SelectPlan.enum_orders`
  (via the catalog on the pushdown `SubqueryCtx`), and `executor._order_key_fn` maps each value to its
  ordinal. Tests: `tests/test_sql_alter_type.py` + a pg8000 wire round-trip. Enum-aware ordering was
  extended to the pipeline / evaluated paths in b116 (`planner._emit_pipeline_sort` gains an
  `$indexOfArray` ordinal companion; `_append_sort_limit` / `_append_join_tail` thread the enum labels via
  `_enum_labels_for_column` + `_column_for_order_node`; the evaluated planners carry
  `EvaluatedSelectPlan.enum_orders`, applied in `executor._evaluated_value_rows`) — so GROUP BY, DISTINCT,
  JOIN, JOIN+GROUP BY, and computed-column ORDER BY all sort an enum by declared order. The
  **correlated single-table SELECT** path also sorts an enum by declared order now (b205, #170:
  `plan_correlated_select` populates `CorrelatedSelectPlan.enum_orders` via `_enum_order_map`, applied
  by `_order_key_fn` in `executor.execute_correlated_select`). Tests: `tests/test_sql_enum_order.py`,
  `tests/test_sql_correlated_extras.py`. **Limitations:** `ALTER TYPE RENAME VALUE` / composite-type
  alters → `0A000`.
- [x] **Generated columns landed** (b108): `GENERATED ALWAYS AS (expr) STORED` columns
  (`planner._generated_expr` stores the rendered SQL on `Column.generated`). Computed from the row's
  other columns on every write by `executor._apply_generated_columns` (evaluates the expr via
  `scalar.evaluate` with a column→field scope, reusing the CHECK-constraint machinery) — runs before
  NOT NULL / CHECK / UNIQUE so they see the value. A user value is rejected with `428C9` on INSERT
  (`_insert_doc`) and UPDATE (`plan_update` — only `= DEFAULT` is allowed, which recomputes). `execute_update`
  does a per-row second pass to persist the recomputed value (the bulk `$set` can't carry a per-row
  expression). Reflection: `pg_attribute.attgenerated = 's'`. Tests: `tests/test_sql_generated.py`.
  **Limitations:** only `STORED` (all Postgres offers); the expression may reference only columns of the
  same row (no subqueries / volatile functions guard); no `ALTER TABLE … ADD COLUMN … GENERATED` (the
  ALTER ADD path doesn't parse the constraint yet); a generated column isn't re-derived if the underlying
  data was written directly via the Mongo API (SQL writes only).
- [x] **COPY FROM/TO STDIN/STDOUT landed** (b109): the `COPY` bulk-load / dump sub-protocol over the
  wire (`psql \copy`, `pg_dump`). `pgwire` gained `copy_in_response` ('G') / `copy_out_response` ('H') /
  `copy_data` ('d') / `copy_done` ('c') / `copy_fail` ('f'); `pgserver._handle_copy` / `_copy_in` /
  `_copy_out` drive the streaming (detected in `_handle_query` when the single parsed statement is
  `exp.Copy`). `engine.copy_plan` resolves the target + options (`FORMAT` / `CSV` / `DELIMITER` /
  `NULL` / `HEADER`, incl. the legacy `WITH CSV HEADER` bundling); `engine.copy_insert` coerces cells and
  routes through `executor.execute_insert` (so COPY enforces the same NOT NULL / CHECK / UNIQUE / FK +
  sequence / generated / enum rules as INSERT); `engine.copy_extract` renders rows for TO. The text/CSV
  codec is `secantus.sql.copyfmt` (pure string↔rows; text `\N` NULL + backslash escapes; CSV NULL =
  unquoted-empty vs `""` = empty string, `HEADER` skip/emit). A no-column-list `COPY FROM` excludes
  generated / identity-always columns. Tests: `tests/test_pgserver_copy.py` (wire) + `tests/test_sql_copyfmt.py`
  (codec). `COPY (SELECT …) TO STDOUT` (query-form, b113) runs an arbitrary query via `engine._run_query`
  (joins / aggregates / `WITH` / set operations) and renders its `SQLResult` to copy cells in
  `engine._copy_query_rows` (CSV `HEADER` uses the query's output column names); it is dump-only —
  `COPY (query) FROM` → `42601`. **Limitations:** text + CSV only (no binary `COPY`); `STDIN` / `STDOUT`
  only (no server-side file paths — the client streams, like `\copy`); the embedded `run_sql` API can't
  drive the streaming COPY sub-protocol (no stream) — it's wire-only.
- [ ] **Partial indexes landed** (b110): `CREATE INDEX … WHERE <predicate>` lowers the predicate to a
  Mongo filter (`planner.plan_create_index` calls `_expr_to_filter` on the index's `params.where`) and
  passes it to storage as `partialFilterExpression` (`executor.execute_create_index`), so the query
  planner accelerates matching queries and `explain` reports `IXSCAN` `isPartial: true` (the storage
  layer already supported partial indexes; this wires the SQL surface to it). Works with `UNIQUE` and
  `IF NOT EXISTS`, and compound `AND` predicates merge into one filter. **Expression indexes**
  (`CREATE INDEX … ((a + b))`) are rejected `0A000` — the storage engine indexes stored fields, not
  computed values (add a `GENERATED … STORED` column and index that). Tests:
  `tests/test_sql_partial_index.py`. **Limitations:** `pg_index.indpred` still reflects as NULL (the
  index works + accelerates, but SQLAlchemy's `get_indexes` won't report it as partial — rendering the
  Mongo filter back to a SQL predicate for `pg_get_expr` isn't done); a partial predicate that doesn't
  lower to a field filter (e.g. a function call) would raise at CREATE rather than degrade to a full
  index.
- [x] **`DISTINCT ON` + `LATERAL` joins landed** (b82). **`DISTINCT ON (exprs)`** keeps the first row
  per distinct value of `exprs` in ORDER BY order (single-table + join) — routed through the evaluated
  path (`planner._distinct_on`, `EvaluatedSelectPlan.distinct_on`, dedup in `executor._evaluated_value_rows`);
  before this it was silently mistreated as plain full-row DISTINCT. **`LATERAL`** (comma / `CROSS JOIN
  LATERAL` / `JOIN LATERAL … ON true` / `LEFT JOIN LATERAL … ON true`) lowers a single-table correlated
  subquery to a `$lookup` (`let` outer bindings + sub-`pipeline` with a `$match {$expr}` correlation via
  the existing `_OnTranslator`, optional `$sort`/`$limit` for top-N, `$project`) + `$unwind`
  (`planner._lateral_stage`, dispatched in `_build_join_pipeline`'s join loop). **Rich `LATERAL`
  subqueries landed** (b233): a subquery containing a join / `GROUP BY` / `HAVING` / `DISTINCT` /
  bare aggregate no longer errors — it's evaluated per outer row instead of lowered to a `$lookup`.
  The planner collects such laterals via the `_lateral_collect` ContextVar (set for the span of
  `_build_join_pipeline`), forces the evaluated path, and stashes them on `EvaluatedSelectPlan.lateral_joins`
  as `LateralJoin(alias, tdef, select, side, inner_aliases)` where `select` is the *un-substituted*
  correlated AST. At execution `executor._expand_lateral` binds each outer column ref to that row's value
  (`planner._substitute_outer_columns` rewrites the refs to literals), runs the now-uncorrelated inner
  query via `engine.run_inner_select`, and cross-joins its rows onto the outer row; `INNER`/`CROSS`
  drops an outer row with no lateral rows, `LEFT` null-pads it. **Limitations:** `JOIN LATERAL` with a
  non-`TRUE` ON is still rejected (correlate in the subquery WHERE); a rich lateral encountered outside a
  join-pipeline context (ContextVar unset) raises `0A000`. A bare scalar aggregate over an empty group
  in an `INNER`/`CROSS` lateral drops the outer row rather than keeping it with a NULL — a consequence
  of the pre-existing "whole-table aggregate over an empty table returns no row (except `count`)" gap
  (see §aggregate note above); `count`-style aggregates and `LEFT JOIN LATERAL` are unaffected.
  `DISTINCT ON` doesn't enforce Postgres' "ORDER BY must start with the DISTINCT ON exprs" rule
  (lenient — keeps whatever the sort order gives).
- [x] **`GROUP BY ROLLUP` / `CUBE` / `GROUPING SETS` landed** (b83, single-table). Enumerated grouping
  sets (`planner._grouping_sets`: ROLLUP → prefixes, CUBE → all subsets, explicit GROUPING SETS as
  written, a leading plain `GROUP BY a, …` as a prefix in every set) are each compiled to a
  `$group`+`$project` branch (`_grouping_set_branch`; group columns absent from a set project as
  `{$literal: None}` so every branch shares one output shape) and combined with `$unionWith`
  (`_plan_grouping_sets_select`, routed in `_plan_pipeline_select`). **`GROUPING()` helper landed (#167,
  b202):** `GROUPING(a, …)` (parsed as `exp.Grouping`) projects a per-branch bitmask — 1 for each
  argument rolled up (absent from that branch's grouping set), 0 otherwise, MSB first
  (`_grouping_args` / `_grouping_bitmask`, emitted in `_grouping_set_branch` and, always 0, in the plain
  `_plan_group_select`). **DISTINCT aggregates under GROUPING SETS landed** (b211): `count`/`sum`/`avg`
  (`DISTINCT x`) — with or without `FILTER` — route through `_register_distinct_agg` inside each grouping
  set's branch (a `$addToSet` accumulator + a per-branch `$addFields` reduction stage before the branch's
  `$project`); `min`/`max`(`DISTINCT`) take the plain accumulator (a distinct extremum equals the raw
  extremum). `count(DISTINCT *)` → `0A000`. **Statistical / bitwise aggregates under GROUPING SETS landed**
  (b223): `variance`/`var_pop` (a `$stdDevSamp`/`$stdDevPop` accumulator squared) and `bit_and`/`bit_or`/
  `bit_xor` (a `$push` + Python fold) now work per grouping set — each branch (`_grouping_set_branch` /
  `_join_grouping_set_branch`) carries the accumulator and returns a `post_aggregates` entry; the grouping-sets
  planners thread one copy (identical across branches) onto the `PipelineSelectPlan`, and
  `executor._apply_post_aggregates` runs the finish over the whole union. Single-table and over a JOIN. `stddev*`
  needs no finish (native `$stdDev*`). `FILTER` on these / `func(*)` → `0A000`. **`HAVING` with GROUPING SETS landed** (b213): each grouping
  set's branch resolves the HAVING via the shared `_having_to_match` (before its `$group` is built, so any
  hidden aggregate accumulator the predicate needs lands in the group stage) and applies the resulting
  `$match` to that branch's grouped rows; every branch registers HAVING identically so the `$unionWith`
  shapes stay aligned. Aggregate predicates (incl. `count`/`sum`/`avg`(`DISTINCT`) and aggregates not in
  the select list), group-column predicates (a set that aggregated the column away compares against NULL,
  per Postgres), and `AND`/`OR` combinations all work. **GROUPING SETS over a JOIN landed** (b218):
  `_plan_join_grouping_sets_select` + `_join_grouping_set_branch` build the `$lookup`/`$unwind`/`$match`
  join prefix once (via `_build_join_pipeline`), then union a per-grouping-set `$group`/`$project` branch
  that *replays* that prefix in each `$unionWith` (re-reading the base collection). Group columns /
  aggregate args resolve through the join resolver (`_group_col_nodes` maps each bare grouping name to its
  qualified `exp.Column` so `resolve` can reach the post-unwind path); `HAVING`, `count`/`sum`/`avg`
  (`DISTINCT`), `array_agg`/`string_agg`/`jsonb_object_agg`, and the `GROUPING()` helper all work per set.
  **Window over GROUPING SETS landed** (b219, single-table): `_plan_grouping_sets_window_select` runs the
  grouping-sets `$unionWith` pipeline but each branch projects *flat* group-column + aggregate fields (via
  the same `register_agg` the window+GROUP path uses), then hands the union to `_finish_group_window` so the
  evaluated executor computes the windows over the grouped rows. A rolled-up row (group column NULL) still
  participates in the window; `GROUPING()` is materialised as a per-branch literal field and usable inside
  the window's `ORDER BY`/`PARTITION BY`; `HAVING` filters each branch before the window.
  **Window over GROUPING SETS *and* a JOIN landed** (b221): `_plan_join_grouping_sets_window_select` combines
  b218 (join grouping-sets union, aggregate args / group keys resolved through the join resolver, join prefix
  replayed per branch) with b219 (flat per-branch projection + `GROUPING()` materialisation → `_finish_group_window`
  runs the windows over the union). `HAVING`, `count(DISTINCT)`, and `GROUPING()` in the window ORDER BY all work.
  **Computed grouping key over a JOIN landed** (b222): `_lower_join_group_keys` lowers each computed key
  (`ROLLUP(lower(d.label))`, `n.k+1`) through the join resolver into a synthetic `__gkeyN` field, appends a
  `$addFields` materialising it to the *shared* join prefix (so it exists in the base pipeline and every replayed
  `$unionWith` branch), and rewrites SELECT / GROUP BY / HAVING / ORDER references to the bare `__gkeyN` column
  (`_apply_group_key_rewrite`). Branch group_id / types resolve the synthetic key to its own top-level field (tag
  `any`) via a `key_path` map; works with `HAVING`, `GROUPING()`, and a window. An unlowerable key (e.g. `substr`)
  still → `0A000`.
  **Limitations:** a subquery in `HAVING` alongside a window over GROUPING SETS → `0A000`; a correlated /
  per-row WHERE with GROUPING SETS over a JOIN → `feature_not_supported`. (An in-aggregate `ORDER BY` under
  GROUPING SETS, single-table or over a JOIN, now works — b224.)
- [x] **Expression over an aggregate landed (#167, b202):** a SELECT item that *wraps* an aggregate
  (`sum(x) + 1`, `round(avg(x), 2)`, `sum(x) - min(x)`) is now supported — `_select_has_computed_aggregate`
  routes it to the window-aware `_plan_group_window_select`, which rewrites each aggregate to its `$group`
  output field and evaluates the wrapping expression per grouped row via the evaluated executor (the same
  machinery window functions use). A bare aggregate stays on the fast `$group` path. Computed GROUP BY
  keys (`GROUP BY lower(name)`, `GROUP BY a + b`) work (single-table + JOIN) via
  `_computed_group_keys` / `_lower_computed_group_keys`; a key using an unlowerable function (`substr`)
  stays `0A000`.
- [x] **ORDER BY completeness in GROUP BY / pipeline queries landed** (b210): a pipeline `ORDER BY` now
  accepts a **positional reference** (`ORDER BY 1`, `ORDER BY 2 DESC` → the Nth select item; out-of-range
  → `42P10`) and an **aggregate / computed expression that matches a select-list item** (`ORDER BY
  count(*) DESC`, `ORDER BY sum(x)` when that aggregate is selected). `_append_sort_limit` now takes the
  ordered `out_columns` list and resolves each term via `_resolve_order_output` (positional index →
  output name; else match the term's SQL against each select item's expression, select items and
  `out_columns` being 1:1 in order; else a plain column). Works across single-table, JOIN, GROUPING SETS,
  and group-window paths; enum-order resolution still applies only to plain-column terms. **ORDER BY an
  aggregate that is *not* in the select list landed** (b212): `SELECT dept … GROUP BY dept ORDER BY
  sum(sal) DESC` — `_register_orderby_aggs_single` / `_register_orderby_aggs_join` register a hidden
  `$group` accumulator (a bare `count`/`sum`/`avg`/`min`/`max`, or `count`/`sum`/`avg`(`DISTINCT`) via
  `_register_distinct_agg`) for each such term, projected so the `$sort` can reach it but kept out of
  `out_columns` so the executor drops it from the output; `_resolve_order_output` maps the term's SQL to
  the hidden field. Still `0A000`: a non-aggregate computed ORDER BY expression not in the select list,
  and ORDER BY an unselected aggregate under GROUPING SETS (the union branches don't share hidden fields).
- [x] **`ALTER TABLE` landed** (b80): `ADD COLUMN [IF NOT EXISTS]`, `DROP COLUMN [IF EXISTS]`
  (`$unset`s the field on every doc), `RENAME COLUMN` (`$rename`s a non-PK field; a PK rename keeps
  the `_id` field and only changes the SQL name), `RENAME TO` (renames the table *and* moves the
  backing collection via `Storage.rename_collection`, so the old name stops resolving — otherwise the
  leftover collection reflects as a phantom table), and `ALTER COLUMN … SET/DROP NOT NULL`.
  `ALTER TABLE IF EXISTS` on a missing table is a no-op; dropping the PK column is rejected
  (`0A000`). `execute_alter_table` / `_apply_alter_action` in `executor.py`; catalog rewrite via
  `Catalog.replace`. `ALTER COLUMN … TYPE t` (retype in catalog) and `SET`/`DROP DEFAULT` landed in
  b84 (see below). **Multiple actions in one statement landed (#145, b185):** a *mixed-kind* action list
  (`ADD …, DROP …`) exceeds sqlglot's ALTER parser and falls back to an opaque `Command`, so
  `engine._run_mixed_alter_table` splits the list at top-level commas (`_split_top_level_commas`,
  paren/quote-aware), re-parses each action as its own single-action `ALTER TABLE` (sqlglot handles any
  single action → `exp.Alter`), and merges the actions into one `exp.Alter` routed through the normal
  single-ALTER path. Homogeneous lists (all-ADD / all-DROP) already parsed natively and were unaffected.
  Handles `IF EXISTS` and preserves data through a mid-list `RENAME COLUMN`. Tests: `test_sql_alter.py`
  (`test_multi_action_*`).
- [x] **Literal column DEFAULTs + `ALTER COLUMN TYPE` / `SET`/`DROP DEFAULT` landed** (b84). `Column`
  gained `has_default` / `default`; a literal DEFAULT (number / string / bool / NULL) from `CREATE
  TABLE` (`planner._column_default`) or `ALTER COLUMN SET DEFAULT` is filled in for an omitted column
  in `_insert_doc`. `ALTER COLUMN … TYPE t` updates the catalog `type_tag` (new inserts/reads use it;
  already-stored BSON values are **not** rewritten). Fixed a latent bug: `DROP DEFAULT` and `DROP NOT
  NULL` both parse with `drop=True`, and the old AlterColumn handler conflated them, wrongly setting
  the column NOT NULL on `DROP DEFAULT`. **Expression defaults landed (#166, b201):** a non-literal
  DEFAULT (`now()` / `CURRENT_TIMESTAMP` / `gen_random_uuid()` / arithmetic / function) is now stored as
  its rendered SQL on `Column.default_expr` (`planner._default_expr`, from `CREATE TABLE` and `ALTER
  COLUMN SET DEFAULT`) and evaluated **per omitted row** at INSERT via `scalar.evaluate`
  (`_parse_default_expr`, lru-cached) — so `gen_random_uuid()` yields a fresh value per row. Reflected in
  `information_schema.columns.column_default` (`virtual._column_default_text`). A default referencing
  another column raises `0A000`. `pg_catalog.pg_attrdef` now emits one row per column with a DEFAULT
  (`adbin` = the rendered default text via `virtual._pg_attrdef` / `_column_default_text`, b206 #171),
  matching `information_schema.columns.column_default`. **Limitations:** a `TYPE` change doesn't recast
  existing rows.
- [ ] **`SET` is accept-and-record.** GUCs persist on the session and reportable ones
  echo a `ParameterStatus`, but nothing acts on them (e.g. `search_path` doesn't affect
  name resolution). (`BEGIN`/`COMMIT`/`ROLLBACK` are now real transactions — see below.)
- [ ] **Transactions: single-connection atomicity; savepoints are real (DDL excepted).**
  `BEGIN`/`COMMIT`/`ROLLBACK` open/commit/abort a real `Storage` user-transaction
  (statements in the block run on its WT session; ROLLBACK undoes them; an error poisons
  the block with `25P02` until it ends). `SET TRANSACTION ISOLATION LEVEL` / `READ ONLY` /
  `READ WRITE`, `SET SESSION CHARACTERISTICS`, and `BEGIN ISOLATION LEVEL …` are
  **accepted as no-ops** (single-node — isolation/read-only don't change behaviour).
  **Real nested savepoints landed** (b71): `SAVEPOINT name` / `ROLLBACK TO SAVEPOINT name` /
  `RELEASE SAVEPOINT name` do actual partial rollback. Each open savepoint (`session.savepoints`,
  a stack of `_Savepoint`) lazily captures a touched collection's deep-copied pre-image the first
  time it's written after the savepoint (`engine._capture_savepoint_snapshots`, run before every
  DML inside the txn — the snapshot pins to the savepoint's establishment state since nothing wrote
  in between). `ROLLBACK TO` restores each collection to the oldest captured snapshot among the
  target savepoint and the nested ones (`delete_matching({})` + re-`insert`), drops the nested
  savepoints, un-poisons the block, and keeps the savepoint open; `RELEASE` merges a savepoint's
  snapshots down into its parent (oldest-per-collection wins) so the parent can still undo them.
  `parse()` rewrites `RELEASE SAVEPOINT x` → `RELEASE x` (sqlglot parses only the latter). Outside a
  txn block → `25P01`; unknown savepoint → `3B001`. **Limitations:** it's a collection-granularity
  snapshot (fine for the ephemeral test data SecantusDB targets, `O(rows-in-touched-collection)` per
  first-write-per-savepoint, not a WT-native savepoint); **DDL inside a savepoint is not undone**
  (verified 2026-08-14: `SAVEPOINT sp; CREATE TABLE x; ROLLBACK TO sp` leaves `x` present, where
  Postgres drops it — the savepoint snapshots row data, not catalog state. This is the ONLY savepoint
  gap left; everything else in this entry was confirmed working against the engine.)
  (`CREATE`/`DROP`/`CREATE INDEX` — only DML restores); and recovery after a storage-engine
  `WT_ROLLBACK`-class error (vs an ordinary constraint violation) may leave the WT txn unusable for
  the restore writes. `DISCARD` remains a no-op (`DEALLOCATE` is now a real prepared-statement command — see the
  b157 entry above). **Server-side cursors landed** (b72):
  `DECLARE name [WITH HOLD] CURSOR FOR <query>` materializes the query at declaration
  (`engine._declare_cursor`, stored as a `session._Cursor`); `FETCH` / `MOVE` walk a scroll position
  (`_cursor_slice` — forward / backward / absolute / relative, so cursors are fully scrollable) and
  `CLOSE name` / `CLOSE ALL` drop them. `FETCH` accepts `NEXT` / bare-count / `ALL` / `PRIOR` /
  `FIRST` / `LAST` / `FORWARD [n|ALL]` / `BACKWARD [n|ALL]` / `ABSOLUTE n` / `RELATIVE n`; `MOVE`
  positions without a result set. `WITHOUT HOLD` cursors close at COMMIT/ROLLBACK, `WITH HOLD`
  survive. `MOVE` is hand-built in `planner.parse` (sqlglot can't tokenize it); FETCH/DECLARE come
  through as `exp.Command`, CLOSE as a bare `Alias`. Unknown/closed cursor → `34000`. **Limitations:**
  the cursor is a materialized snapshot at DECLARE (later same-txn writes aren't visible through it),
  and it isn't wired into the extended protocol's Portal machinery (it's a SQL-level cursor, like
  psycopg's named server-side cursors). DDL is transactional via BEGIN/COMMIT/ROLLBACK. Cross-connection
  isolation is the WT engine's job (the test double only models atomicity).
- [x] **Row-locking clauses landed** (#132, b172): `SELECT … FOR UPDATE | FOR SHARE | FOR NO KEY UPDATE
  | FOR KEY SHARE` with `NOWAIT` / `SKIP LOCKED` / `OF <table>` (sqlglot `stmt.args["locks"]` =
  `[exp.Lock]`) are **accepted as single-node no-ops** that return the rows — so SQLAlchemy's
  `with_for_update()` works. Honored across every SELECT shape (plain / join / group / distinct / CTE /
  limit) since the planner never consulted `locks`. `engine._validate_locks` (called at the top of
  `_run_select`) adds one real check: an `OF <table>` target that isn't a FROM/JOIN relation errors
  `42P01` (scope gathered from `from_`/`joins` only, so the lock's own OF-table doesn't self-satisfy; a
  table alias masks its base name, matching Postgres). Tests: `tests/test_sql_row_locking.py` +
  `test_pgserver_pg8000.py::test_select_for_update_via_driver`. **Limitations:** no actual locking (no
  concurrency to lock against within a connection — the storage `RLock` serializes); `FOR UPDATE` on an
  aggregate / `DISTINCT` / set-op / group is accepted rather than rejected as Postgres would; OF-target
  validation is only applied on the direct single-table/JOIN `_run_select` path (set-op/pipeline locks
  aren't re-validated). Not ported to the Rust server.
- [x] **TRUNCATE TABLE landed** (#133, b173): `TRUNCATE [TABLE] t [, …] [RESTART | CONTINUE IDENTITY]
  [CASCADE | RESTRICT] [IF EXISTS]` (sqlglot `exp.TruncateTable`: `expressions` = tables, `identity` =
  RESTART/CONTINUE, `option` = CASCADE/RESTRICT) → `engine._run_truncate`. Empties each table via
  `storage.delete_matching(db, coll, {})` (index entries maintained). `RESTART IDENTITY` resets each
  table's owned `SERIAL`/`IDENTITY` sequences (`col.sequence` → `catalog.alter_sequence(…, {"restart":
  None})`); `CONTINUE` (default) leaves them. FK handling reuses `executor._referencing_fks`: `CASCADE`
  adds the transitive closure of referencing tables to the truncate set; `RESTRICT` (default) errors
  `0A000` if a table is referenced from outside the set (allowed when the referencer is truncated in the
  same statement). `IF EXISTS` skips missing tables; otherwise unknown → `42P01`. Gated as `A_REMOVE` in
  `authz.required_privilege` (a `read` role is denied `42501`, `readWrite` allowed). Tests:
  `tests/test_sql_truncate.py` + `test_pgserver_pg8000.py::test_truncate_via_driver`. **Limitations:**
  no `ONLY` / partition semantics; `CASCADE` empties referencing tables but doesn't reset *their*
  identities unless they're also named; runs within the session transaction (rolls back with it) but
  isn't the O(1) file-truncate a real engine does — it's a bulk `delete_matching`. Not ported to the
  Rust server.
- [x] **Index / constraint reflection for `\d` landed** (#134, b174): `pg_catalog.pg_indexes`
  (`virtual._pg_indexes`) lists one row per index (`schemaname`/`tablename`/`indexname`/`tablespace`=NULL/
  `indexdef`) and `pg_get_indexdef(oid)` (`virtual.indexdef_for_oid`, wired in `scalar._call_func` +
  registered in `functions._SCALAR_EVAL_ANON`) both render `CREATE [UNIQUE] INDEX <name> ON public.<table>
  USING btree (<cols>)` with `DESC` on descending columns. Backed by a new rich `virtual._indexes` (PK
  index as `<t>_pkey`, each user `CREATE INDEX`, and each UNIQUE-constraint index) that also carries the
  owning table + rendered column list; `_index_relations` is now its projection to the historical
  pg_index/pg_class shape. WiredTiger's physical `_id_` index is skipped so it never leaks into the SQL
  surface. `pg_get_constraintdef(oid)` now also renders **PRIMARY KEY (…)** (`virtual.constraint_def_for_oid`
  gained a PK branch keyed to `_PK_CON_OID_BASE` = 30000, mirroring `_pg_constraint`'s PK-oid assignment)
  alongside the existing FOREIGN KEY / UNIQUE / CHECK rendering. Tests: `tests/test_sql_index_reflection.py`
  + `test_pgserver_pg8000.py::test_index_constraint_reflection_via_driver`. **Limitations:** a partial
  index's predicate isn't reversed back to a `WHERE` clause in `indexdef` (the `partial` flag is tracked
  but the expression text isn't rendered); expression/functional index columns reflect only when every
  key field maps to a declared column (index over a raw field is skipped); no `INCLUDE`/opclass/collation
  detail in `indexdef`. Not ported to the Rust server.
- [x] **Advisory locks landed** (#135, b175): the `pg_advisory_lock` family — `pg_advisory_lock` /
  `pg_advisory_unlock` / `pg_advisory_unlock_all` plus the `_shared`, `_xact_`, and `pg_try_*` variants
  (eleven functions total) — as **session-tracked single-node no-op locking**. All parse as `exp.Anonymous`;
  registered in `functions._SCALAR_EVAL_ANON` so FROM-less `SELECT pg_advisory_*(…)` routes to
  `scalar._advisory_lock` (which has the session via `ScalarContext`). Single-node → a lock is always
  granted immediately, so we only *track* what the session holds: `Session.advisory_locks` keyed by
  `(classid, objid, objsubid, mode, xact)` → re-entrant stack count, with `advisory_lock_acquire` /
  `advisory_lock_release` / `advisory_unlock_all` / `release_xact_advisory_locks` / `held_advisory_locks`.
  `pg_try_*` always return `true`; `pg_advisory_unlock*` return whether a session-level lock was held
  (`false` otherwise, as Postgres); locks are re-entrant (N locks need N unlocks); `pg_advisory_xact_lock*`
  release at COMMIT/ROLLBACK (hooked in `engine._end_txn_state`) and aren't manually unlockable. A single
  `bigint` key splits into signed 32-bit `(classid, objid)` halves (objsubid 1); a `(int4, int4)` pair maps
  through (objsubid 2). Reflected via `pg_catalog.pg_locks` (`virtual._pg_locks`: `locktype='advisory'`, one
  row per key+mode, always `granted`). Return-type tags in `planner._infer_scalar_tag` (`pg_try_*` /
  `pg_advisory_unlock*` → bool; void forms → text NULL). Tests: `tests/test_sql_advisory_locks.py` +
  `test_pgserver_pg8000.py::test_advisory_locks_via_driver`. **Limitations:** no actual cross-session
  locking (single-node, storage `RLock` serializes) — `pg_try_*` can't ever fail; `pg_locks` reflects only
  *this* connection's advisory locks (no cross-backend visibility) and no non-advisory lock types
  (relation/tuple/transactionid rows); `objsubid`/`tuple` reflect as `int4` (no `int2` type tag). Not ported
  to the Rust server.
- [x] **SET LOCAL + SHOW ALL / pg_settings landed** (#136, b176): `SET LOCAL name = value`
  (sqlglot `exp.SetItem(kind=LOCAL)`) applies a GUC only for the rest of the current transaction and
  reverts at COMMIT/ROLLBACK (`Session.set_local` / `restore_local_gucs`, hooked in
  `engine._end_txn_state`); outside a transaction it has no lasting effect (Postgres warns and drops it).
  `SHOW ALL` (an `exp.Command` fallback) now returns every GUC as a three-column `(name, setting,
  description)` table (`engine._run_command` + the `describe_statement` SHOW path both special-case it).
  `pg_catalog.pg_settings` (`virtual._pg_settings`, backed by `Session.all_settings` = defaults overlaid
  with SET overrides) exposes `name`/`setting`/`vartype`/`source`/`boot_val`/`reset_val`/... — the subset
  psql's `\dconfig` and ORMs read; `vartype` is inferred from the value, `source` is `session` for an
  override else `default`. Tests: `tests/test_sql_set_local.py` +
  `test_pgserver_pg8000.py::{test_show_all_and_pg_settings_via_driver,test_set_local_reverts_at_commit_via_driver}`.
  **Limitations:** a plain (non-LOCAL) `SET` inside a transaction is session-scoped and does *not* revert on
  ROLLBACK (real Postgres reverts transactional GUCs); the txn-end revert of a *reportable* GUC (search_path
  etc.) doesn't emit a compensating `ParameterStatus`; `pg_settings` metadata is coarse (generic category,
  empty short_desc, NULL unit/min/max/enumvals, no per-GUC context). Not ported to the Rust server.
- [x] **Monitoring views (pg_stat_activity) landed** (#137, b177): `pg_catalog.pg_stat_activity` (one row
  per live backend) + `pg_catalog.pg_stat_database` (per-db backend count) reflect a new server-level
  `session.ActivityRegistry` — `SecantusPGServer` registers each connection's `Session` on connect and
  unregisters on disconnect (parallel to the `_notify`/`_conns` pattern), assigns a unique per-connection
  `backend_pid` via an `itertools.count` (real Postgres gives each backend a distinct pid; in-process we'd
  otherwise share `os.getpid()`), and stamps `backend_start` / `client_addr`. The wire query paths
  (`pgserver._handle_query` simple + `pgextended._execute` extended) set `session.state`='active' + the
  `current_query` text for the duration of a query (idle with last-query afterwards), so a client running
  the `pg_stat_activity` SELECT sees its own row `active`. Builders `virtual._pg_stat_activity` /
  `_pg_stat_database` read `session.activity_registry.snapshot()` (falling back to just the calling session
  for the embedded `run_sql` API). Tests: `tests/test_sql_stat_activity.py` +
  `test_pgserver_pg8000.py::test_pg_stat_activity_via_driver`. **Limitations:** `pg_stat_database`
  cumulative counters (`xact_commit`/`blks_hit`/`tup_*`/...) are fixed `0` (no stats collector);
  `pg_stat_activity` omits live `xact_start` / `state_change` / `wait_event*` / `leader_pid` /
  `backend_xid` (NULL); `client_port` is NULL and `client_addr` is text (not `inet`); COPY sub-protocol and
  the initial handshake don't update `state`. Not ported to the Rust server.
- [x] **Role membership (GRANT role TO role) landed** (#138, b178): `GRANT <roles> TO <members> [WITH
  ADMIN OPTION]` / `REVOKE [ADMIN OPTION FOR] <roles> FROM <members> [CASCADE|RESTRICT]` — role-membership
  grants parse as `exp.Command` (no `ON` target, unlike privilege grants which are `exp.Grant`), routed by
  `engine._run_role_membership` (regex-split the tail; a Command carrying `ON` is a privilege grant → no-op
  as before). Persisted per `(role, member)` in `__sql_role_members__`
  (`Catalog.grant_role_membership` / `revoke_role_membership` / `revoke_role_admin_option` /
  `list_role_memberships`); `WITH ADMIN OPTION` is tracked and a plain re-grant keeps an existing one
  (union), `REVOKE ADMIN OPTION FOR` clears just the admin flag and keeps the membership. Reflected via
  `pg_catalog.pg_auth_members` (`virtual._pg_auth_members`: roleid / member / grantor / admin_option) whose
  oids come from a new shared `virtual._role_oid_map` (refactored out of `_pg_roles`) so they join to
  `pg_roles.oid`; grantor = the connecting user's oid. Command tags `GRANT ROLE` / `REVOKE ROLE`. Tests:
  `tests/test_sql_role_membership.py` + `test_pgserver_pg8000.py::test_role_membership_via_driver`.
  **Limitations:** membership is recorded/reflected but **not enforced** (a member doesn't inherit the group
  role's table grants — the additive authz gate keys off direct grants only); `CASCADE`/`RESTRICT` on
  REVOKE are accepted but ignored (no dependency tracking); a membership referencing a name that isn't a
  declared role or the connecting user reflects with `oid 0` (won't join to `pg_roles`); no cycle detection.
  Not ported to the Rust server.
- [ ] **FakeStorage removed — all SQL/PG tests on real Storage** (#140, b179): the legacy `tests/sqlfake.py`
  mock is deleted; every SQL / pg-server test now drives the real WiredTiger `Storage(str(tmp_path))` with a
  `try: yield finally: s.close()` fixture. The migration surfaced (and this slice fixed) four real bugs the
  mock had masked: (1) `query._coerce_datetime` — a tz-aware SQL literal vs a tz-naive-UTC stored datetime
  raised a `TypeError` that was swallowed → silently-empty WHERE result; now naive is treated as UTC; (2)
  `typemap` `bytea` equality (`Binary(x, 0)` vs `bytes`) fixed in the batch migration; (3)
  `engine.describe_statement` now runs its planning reads inside the session's open transaction, so the
  extended-protocol Describe sees the connection's own uncommitted `CREATE TABLE` — previously it returned
  `NoData` while Execute (which runs in the txn) emitted `DataRow`s, a protocol violation that crashed
  pg8000 on `CREATE + INSERT + parameterised SELECT` in one txn; (4) `typemap.to_pg_text` tags a tz-naive
  `timestamptz` as UTC before rendering so the wire text format carries the offset (client parses tz-aware).
  **Remaining (task #141):** the *embedded* `run_sql` result still surfaces a stored `timestamptz` tz-naive
  (BSON decodes naive); the wire path is now tz-aware. `tests/test_sql_datetime_funcs.py` +
  `test_sql_spike.py` carry a small UTC-normalising shim (commented, referencing #141) so they assert the
  PG-correct tz-aware instant; remove the shim when #141 lands. **(Resolved in b181 — see below.)**
- [x] **Two-phase commit (PREPARE TRANSACTION) landed** (#139, b180): `PREPARE TRANSACTION 'gid'` /
  `COMMIT PREPARED 'gid'` / `ROLLBACK PREPARED 'gid'`. Handled *before* sqlglot in `run_sql`
  (`engine._maybe_two_phase` / `_TWO_PHASE_RE`) because sqlglot can't parse `COMMIT`/`ROLLBACK PREPARED` at
  all and `PREPARE TRANSACTION` collides with the SQL-level `PREPARE name AS` (#121). `PREPARE` detaches the
  block's open `Storage` user-transaction handle into a server-wide `session.PreparedXactRegistry` (keyed by
  gid; shared across connections by the wire server, lazily per-session for embedded `run_sql`), leaving the
  session with no active txn; the WT session/snapshot stays open holding the uncommitted writes. `COMMIT
  PREPARED` / `ROLLBACK PREPARED` — possibly on a *different* backend — pop the gid and call
  `storage.commit_user_transaction` / `abort_user_transaction` on the stashed handle (WT-safe cross-thread:
  the handle owns its session and commits under the storage `RLock`). Deferred constraints are re-checked at
  PREPARE time (like COMMIT); buffered NOTIFYs travel with the prepared xact and deliver at COMMIT PREPARED.
  Reflected via `pg_catalog.pg_prepared_xacts` (`virtual._pg_prepared_xacts`: transaction / gid / prepared /
  owner / database). Errors match PG: PREPARE outside a block → `25P01`; duplicate gid → `42710` (block left
  intact); unknown gid on COMMIT/ROLLBACK PREPARED → `42704`; COMMIT/ROLLBACK PREPARED inside a block →
  `25001`. Wire server skips the COPY probe for these via `engine.is_two_phase_statement` (mirrors the
  pubsub guard). Tests: `tests/test_sql_two_phase.py` + `test_pgserver_pg8000.py` cross-connection.
  **Limitations:** prepared xacts are **in-memory only** — they do NOT survive a server restart (real PG
  persists to `pg_twophase`); the statements work only over the **simple query protocol** (the extended
  Parse/Bind path routes a bound AST through `run_statement`, bypassing the pre-parse interceptor — same
  constraint as LISTEN/NOTIFY). Not ported to the Rust server.
- [ ] **Embedded run_sql returns tz-aware `timestamptz` (#141, b181):** a stored `timestamptz` decodes
  tz-naive UTC from BSON, so the embedded `run_sql` result used to hand back a naive datetime while the wire
  path already rendered it tz-aware — an embedded/wire inconsistency, and the naive value silently
  mis-compared against a tz-aware literal. `engine._normalize_result` now tags naive `timestamptz` /
  `timestamptz[]` result values UTC (`typemap.normalize_result_value`, recursing into arrays) at the
  `run_sql` / `run_statement` boundary, so embedded results match the wire instant. `timestamp` / `date` /
  `time` stay naive. The two shims in `test_sql_datetime_funcs.py` + `test_sql_spike.py` are removed; direct
  regression tests live in `test_sql_datetime_types.py` (`test_stored_timestamptz_is_tzaware`,
  `test_timestamptz_array_elements_tzaware`). **Known limitation (unchanged, separate from #141):**
  SecantusDB has **no distinct naive `timestamp` type** — `type_tag_for_sql` collapses `DATETIME` /
  `TIMESTAMP` / `TIMESTAMPTZ` all to the single `timestamptz` tag — so `date + interval` /
  `timestamp + interval` (PG: naive `timestamp`) come back tz-aware UTC here (both embedded and wire).
  `test_date_plus_interval_is_timestamp` / `test_timestamp_plus_interval` assert the tz-aware value and note
  the divergence. A real naive-`timestamp` type (tag + OID 1114 + render/coerce/round-trip) is a larger
  slice — **now landed in #143, b183 (see below).** (The related WHERE `timestamptz_col = '…+00:00'` /
  `::timestamptz` filter-coercion quirk is **fixed in #142, b182** — see below.)
- [ ] **timestamptz WHERE-equality bridges naive/aware (#142, b182):** `WHERE ts = '…+00:00'` /
  `= '…'::timestamptz` used to match **nothing** — the equality path (`query._eq_numeric_aware`, shared by
  bare equality / `$eq` / `$in` / `$ne`) did numeric coercion but not the tz-aware/naive datetime alignment
  the range operators already had (`_try_cmp` → `_coerce_datetime`), so a tz-aware SQL literal never equalled
  the tz-naive-UTC stored value (`naive == aware` is always False in Python). `_eq_numeric_aware` now bridges
  two datetimes of the same instant by treating naive as UTC (the convention pymongo's BSON encoder uses), so
  equality matches by instant across the boundary — an offset-shifted literal for the same instant matches, a
  genuinely different instant does not. Indexed equality already worked (`sortkey.encode_value` normalises to
  UTC). Mongo-path/Rust-parity safe: in pure-Mongo usage and the parity harness both operands round-trip
  through BSON (always UTC millis, tzinfo states match), so the new branch is a no-op there and never drifts
  from the Rust `eq_scalar` (which compares `Bson::DateTime` millis). Tests: `test_query.py`
  ::test_datetime_naive_aware_equality_same_instant + `test_sql_datetime_types.py`
  ::test_timestamptz_where_equality_matches_offset_literal / _uses_index.
- [ ] **Distinct naive `timestamp` type (#143, b183):** `TIMESTAMP` / `DATETIME` (and `::timestamp` casts,
  `timestamp '…'` literals, `date + interval` / `timestamp + interval` arithmetic) now type as a distinct
  **`timestamp`** tag (OID 1114, `timestamp without time zone`) instead of collapsing to `timestamptz`
  (1184) — matching Postgres, which types those naive. Only explicit `TIMESTAMPTZ` /
  `TIMESTAMP WITH TIME ZONE` (and `now()` / `current_timestamp`) stay tz-aware. Wiring: `typemap.PG_OID` /
  `SQL_TYPE_NAME` / `PG_TYPENAME` / `_DATATYPE_TAGS` / `_ARRAY_PG_OID` (1115) gain a `timestamp` entry;
  `coerce` drops any offset (keeps wall-clock fields, PG semantics); `to_pg_text` renders naive (no offset);
  `scalar._eval_cast` produces a naive datetime for a `timestamp` cast and strips tz when casting an aware
  value to `timestamp`; `planner._date_arith_tag` / `_interval_arith_tag` return `timestamp` (naive) for
  `date/timestamp ± interval` and `timestamp - timestamp -> interval`. `engine._normalize_result` (from #141)
  only UTC-tags `timestamptz`, so a `timestamp` result stays naive end-to-end (embedded + wire; pg8000
  decodes OID 1114 as a naive datetime). Reflected as `timestamp without time zone` in
  `information_schema.columns`. The #141 arithmetic tests are reverted to assert naive; new regression tests:
  `test_sql_datetime_types.py::test_timestamp_column_is_naive / _oid_and_typename / _literal_drops_offset /
  test_cast_timestamptz_to_timestamp_strips_tz / test_timestamp_array_naive`. SQL-layer change only — no Rust
  parity impact (the Rust engines cover query/update/expr/aggregate, not the SQL type mapping). **Follow-up
  (`date_trunc` argument typing) landed in #144, b184 — see below.**
- [ ] **date_trunc preserves argument tz-ness (#144, b184):** `date_trunc(unit, src)` now types as the
  tz-ness of `src` — `date_trunc(text, timestamptz) -> timestamptz`, `date_trunc(text, timestamp) -> timestamp`
  (a `date` argument casts to naive timestamp) — instead of always `timestamptz`. `planner._infer_scalar_tag`
  threads the argument tag through the `TimestampTrunc` node (`CurrentTimestamp` / `now()` stay `timestamptz`);
  an argument whose type can't be proven naive defaults to `timestamptz` (historical behaviour). The evaluated
  value's tz-ness already followed the input (truncation preserves tzinfo) and `engine._normalize_result`
  UTC-tags only `timestamptz`, so a `timestamp`-typed truncation stays naive end to end. Tests:
  `test_sql_datetime_funcs.py::test_date_trunc_timestamp_arg_is_naive / _timestamptz_arg_stays_tzaware /
  _timestamp_literal_is_naive / _timestamptz_literal_is_tzaware`. SQL-layer only — no Rust parity impact.
  **`date_trunc(unit, interval)` landed (#148, b187):** `_eval_date_trunc` now detects an interval-valued
  argument and truncates the interval, zeroing every component finer than `unit` (years > months > days > time;
  `_date_trunc_interval` in `scalar.py`). Result types as `interval` (`_infer_scalar_tag`). `week` is not a valid
  unit for an interval (→ `0A000`, matching Postgres). SQL-layer only — no Rust parity impact.
- [x] **CREATE/DROP INDEX landed; ALTER not.** `CREATE [UNIQUE] INDEX [name] ON t (col [DESC], …)`
  maps to `Storage.create_index` (PK column → `_id`; auto-generated `field_dir` name when
  unnamed; duplicate → `42P07`); `DROP INDEX [IF EXISTS] name` finds the owning collection by
  scanning the catalog and calls `drop_index` (`42704` when absent). Indexes now reflect back
  through `get_indexes` / `Table(autoload_with=…).indexes` (the populated `pg_index` virtual
  table — `indkey`/`indclass`/`indoption` are typed as `int2vector`/`oidvector` so a libpq
  client renders them as the space-separated text its catalog reflection parses). **Partial
  indexes** (`CREATE INDEX … WHERE …`) and **expression indexes** (`CREATE INDEX … ((expr))`)
  are both supported. Expression indexes materialise the expression into a hidden
  `__expr_<name>` field (registered on the `TableDef` as an `ExprIndex`, recomputed on every
  write via `_apply_expr_index_fields`, backfilled on create); a matching `WHERE` is rewritten
  to the hidden field (`rewrite_expr_index_refs`) so it rides the normal single-field-index
  path (`IXSCAN`); the field is hidden from `SELECT *` / reflection; `DROP INDEX` unregisters
  the `ExprIndex` and strips the field. `ORDER BY` on the expression is correct but not
  index-accelerated (the hidden field is projected away before a pipeline `$sort`). Still
  missing: `ALTER TABLE`-driven index changes.
- [ ] **Dev-env import shim:** `tests/conftest.py` stubs `wiredtiger` only when the
  extension is absent so the pure SQL/operator tests import without a WT build. Inert in
  CI. Revisit if `secantus/__init__` is made lazy (would let `secantus.sql` import without
  dragging in the WT-backed server).

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.

## Concurrent-writer contention (found by bench.concurrency, 2026-07-16)

- ~~Shared oplog-meta row hotspot~~ and ~~5s WriteConflict deadline~~ —
  **fixed**: the meta row is no longer written per oplog emit or per
  cluster-time mint (recovery bumps the clock +1s past everything
  recoverable instead), and the non-transaction write-conflict retry is
  unbounded with periodic warnings, matching mongod's
  `writeConflictRetry`. Post-fix sweeps show zero client-visible conflict
  errors and zero retry stretches ≥5s.
- **Residual**: under sustained multi-writer saturation WiredTiger still
  occasionally marks concurrent batch transactions rollback-only
  (observed as commit-time EINVAL before the mapping fix; cause not fully
  attributed — cache/eviction pressure suspected, key-level conflicts
  ruled out for per-collection writers). Retries absorb it today. If it
  resurfaces as a throughput cliff, instrument `get_rollback_reason()`
  in the retry wrapper and check WT eviction statistics. Multi-writer
  THROUGHPUT scaling itself remains capped by the WT-binding/GIL ceiling
  documented in docs/concurrency.md; lifting it is the
  tasks/wt-concurrency-plan.md end-game. (The Rust server's equivalent
  lever — the global write mutex — was pulled 2026-07-17: per-collection
  write locks + per-statement WT snapshot transactions + the conflict
  machinery, the rust-coll-locks slice. Re-measure with
  `bench.concurrency --server rust` and refresh docs/concurrency.md.)

- ~~Rust DDL paths run autocommit-per-operation~~ — **fixed (rust-backlog-clear
  slice)**: create/drop index, drop-all-indexes, create/drop/rename collection
  and dropDatabase now run their row writes (registry, index entries, options,
  oplog) inside `with_statement_txn` + `retry_write_conflicts`, so a crash
  mid-DDL can no longer strand orphan index-entry rows. dropDatabase is
  per-collection transactions (mongod's unit; one db-wide WT transaction could
  blow the cache dirty limit). WT schema `create` calls inside a wrapped DDL
  are safe — WiredTiger runs them on an internal session
  (`__wt_schema_internal_session`), so only an idempotent empty table can leak
  on a crash. Pinned by `tests/ddl_txn.rs` (DDL joins + rolls back with a user
  transaction). One residual: a DDL issued *inside* a user (multi-document)
  transaction bumps the DDL generation (below) at statement end, not at the
  user-txn commit where its rows become visible — irrelevant today because the
  command layer never routes drop/rename through a user transaction (mongod
  prohibits them in transactions too).

## Concurrency races (found by the concurrent stress suites, 2026-07-16)

Found by the two concurrency harnesses — `tests/test_mongo_server_concurrency.py`
(pymongo vs the Python AND Rust servers, one parametrized suite) and
`tests/test_pgserver_concurrency.py` (psycopg vs the PG server); fixed items are
recorded in that slice's changelog fragment. Still open:

- ~~**SQL UNIQUE constraints race across open transactions.**~~ — **FIXED
  (#775 + #778)**: exactly the fix this entry called for landed — storage-level
  unique indexes (`table:secantus_unique_keys`, WiredTiger-enforced) back both
  the Mongo persona's unique indexes and SQL `UNIQUE` / `PRIMARY KEY`
  constraints, so cross-transaction and simultaneous-uncommitted duplicates
  collide on the key itself. See "UNIQUE constraints across transactions —
  FIXED (storage + SQL)" below for the full record. Sole surviving sliver:
  **DEFERRABLE constraints** keep the commit-time check (an every-write index
  would reject legitimately-transient violations, e.g. the value-swap case) and
  therefore keep the old race — a narrow, documented trade.
- ~~**SQL advisory locks provide no cross-connection exclusion**~~ — **FIXED
  (advisory-lock-hub slice)**: exactly the implementation this entry called
  for — a server-wide `AdvisoryLockHub` (`sql/pgadvisory.py`, the `NotifyHub`
  pattern) is now the authority: exclusive/shared grant rules, re-entrant
  per-owner holds with session- and transaction-level lifetimes, BLOCKING
  `pg_advisory_lock*` waits with a wait-for-graph deadlock check every ~1s
  (PostgreSQL's `deadlock_timeout` shape) surfacing `40P01 deadlock
  detected`, `pg_try_*` returning real grant results, and release at
  unlock / unlock_all / COMMIT (xact locks) / connection teardown. The
  per-`Session` bookkeeping stays as the `pg_locks` reflection layer and is
  hub-synced; embedded `run_sql` sessions (no hub) keep the old single-
  connection behaviour. Pinned by the cross-connection suite in
  `tests/test_sql_advisory_locks.py` (exclusion, blocking, shared-vs-
  exclusive, deadlock, xact-release, teardown-release, plus a wire-level
  two-psycopg-connection test).

## Rust lock-free reads: DDL-vs-scan wobble (2026-07-17) — FIXED

- ~~Partial result sets when a rename/drop/dropDatabase races a lock-free
  read~~ — **fixed (rust-backlog-clear slice)** by the namespace-generation
  check this entry proposed, made sound by the DDL statement transactions
  above (a DDL's row writes now commit atomically, so a re-run whose
  generation held still is a point-in-time answer). `Storage.ddl_generation`
  is a seqlock-style counter: every namespace DDL holds it odd for its
  duration (`ddl_generation_scope`, drop-guarded so an error can't stick it
  odd; sound because DDL serialises on the global lock), and
  `find_matching_with` / `count_matching` re-run a scan whose generation was
  odd or moved (`with_ddl_generation_check`, bounded at 5 re-runs so a DDL
  storm can't livelock a reader). The pre-bump matters: a post-commit-only
  bump left a window where a reader finished a partial scan and checked the
  generation before the DDL thread bumped it. Pinned by
  `tests/concurrent_reads.rs::scans_racing_namespace_ddl_are_never_partial`.

## Follow-ups from the #451 (concurrency stress suites) review, 2026-07-17

- [x] ~~**Rust `UpdateOutcome::post_image` is cloned unconditionally**~~ —
  fixed (rust-backlog-clear slice): `update_matching` /
  `update_matching_pipeline` / the command-seam
  `update_matching_array_filters` / `update_matching_pipeline` take a
  `want_post_image` flag; only `findAndModify` passes `return_new`, so a
  plain update no longer pays the per-doc post-image clone (mirrors the
  Python side's `return_post_images` gating).

## CI build cost

- [ ] **Vendored WiredTiger is rebuilt from source in every CI job** (~100 s x 13
  jobs) and on every *local* `uv sync`, despite `BUILD_ALWAYS OFF`. Root cause
  found: ExternalProject records `PATCH_COMMAND` / `CMAKE_ARGS` verbatim and
  both named `${Python3_EXECUTABLE}`, which under a PEP 517 build is the
  isolated build env's interpreter — a fresh temp path on every build — so the
  patch and configure stamps were invalidated every time. Local half is fixed
  (interpreter passed by file + `REALPATH`; rebuild 37 s -> 1.3 s). **The CI half is
  fixed too — corrected 2026-08-20.** The build-dir cache this entry called for was
  built and is live: `test.yml` carries a "Restore the WiredTiger build"
  `actions/cache` step on `path: build`, keyed on runner OS + image version + WT rev
  + config + interpreter, deliberately with no `restore-keys` so a near-miss cold-builds
  rather than reusing objects from a different compiler. The immediately following item
  (#562) records the follow-up fix. **What actually remains:** Windows is still
  unstabilised (venv pythons are copies, so `REALPATH` is a no-op).
- [x] ~~**The WiredTiger build cache restores but the test lanes rebuild anyway.**~~
  ANSWERED and fixed (#562). scikit-build-core writes `Python3_EXECUTABLE` into
  the TOP-LEVEL CMakeCache, and under PEP 517 that is the isolated build env — a
  fresh temp dir every build. The changed cache variable forces a reconfigure
  that takes the WiredTiger ExternalProject with it: every stamp regenerates,
  including `mkdir`, so the prefix is recreated and WT recompiles on a cache
  HIT. The 3.10/3.12 split was uv reusing a cached build env for the pinned
  `.python-version` (3.12) while minting fresh ones for 3.10.
  The pin must come from the CALLER — setting it inside CMakeLists.txt is too
  late, since scikit-build passes `-DPython3_EXECUTABLE` on the command line
  (tried and measured: still rebuilt). CI now exports
  `SKBUILD_CMAKE_DEFINE=Python3_EXECUTABLE=$(uv python find <ver>)` and folds
  that path into the wtbuild cache key. Measured cold -> warm on the 3.10 test
  lanes: sync 86s/81s/83s/83s -> 3s/5s/5s/5s.
  Still open, smaller: LOCAL builds do not get the pin (it lives in the
  workflow), so a developer on 3.10 still pays the full ~38s WT rebuild per
  `uv sync`; the workaround is to export the same variable. And the workflow's
  critical path is now `storage-engine (windows-latest)`, which is excluded
  from the WT cache because `uv build --wheel` leaves no reusable build dir on
  Windows.

## pymongo gauges: nondeterministic ordering (FIXED) + an intermittent worker crash (OPEN) (2026-07-20)

**Root cause: `pytest-randomly` was shuffling the vendored upstream suites.**
It is a project dev dependency and active by default, so every gauge run
reordered pymongo's tests with a fresh, unrecorded seed (`--no-header` hid it).
Those suites assume their own ordering — collections created by one test and
reused by the next, shared class fixtures — so shuffling manufactured failures
that said nothing about SecantusDB.

Measured on identical code, the async gauge produced **three different results**
purely from ordering:

| ordering | result |
|---|---|
| shuffled (unrecorded seed) | 9 failed, 923 passed + xdist worker crash |
| shuffled (`--randomly-seed=3`) | 16 failed, 916 passed + 2 worker crashes |
| upstream order (`-p no:randomly`) | **6 failed, 926 passed**, no crash |

The reordered runs added a `CollectionInvalid: collection coll already exists`
pileup across `test_read_concern.py` (leftover state from a test that upstream
never runs first) and a `KeyError: 'data'` in `test_custom_types.py` — textbook
test-isolation breakage, not conformance divergence.

Fixed by adding `-p no:randomly` to all three pymongo gauge invocations in
`tasks.py` (`validate`, `validate-one`, `validate-pymongo-async`). Verified: two
consecutive runs now produce byte-identical failure sets (6 failures, all
documented out-of-scope divergences — hashed/text indexes, `$where`, csot,
maxtime message, hedge deprecation).

**Still open — the worker crash is NOT fully explained by ordering.** Removing
the shuffle removed the *variance*, not the crash class. With `-p no:randomly`
in place the sync gauge still crashed once on
`test_bulk.py::TestBulk::test_bulk_max_message_size` (6 failed / 1225 passed),
then ran clean on the very next invocation (5 failed / 1226 passed) — so it is
intermittent even at fixed ordering.

Evidence pointing at memory/resource pressure rather than a logic bug:
- Every test that has crashed a worker is one of the heavy ones —
  `test_numerous_inserts`, `test_bulk_max_message_size`,
  `test_overflow_int_w_custom_decoder`.
- Each passes in isolation in ~2s (`invoke validate-one`), so nothing is wrong
  with the operation itself.
- The gauge runs the whole suite in ONE xdist worker (`-n1`); sampled worker RSS
  peaks around **1.3 GB** by late in the run.
- The first observation came from a `validate-all --jobs 4` run, i.e. under
  maximum machine contention.

Next step for whoever picks this up: confirm or refute OOM. The runner still
does not surface worker stderr, so start there — capture the worker's exit
signal (a SIGKILL from the OS points at memory; a SIGSEGV points at the
extension). If it is memory, the fix is likely periodic worker recycling
(`--max-worker-restart` already tolerates restarts; xdist has no built-in
max-tests-per-worker, so this may mean splitting the heavy bulk files into their
own gauge invocation).

The original guess in this entry — leaked `Storage` fixtures, by analogy with
e233fe61 — was wrong; nothing pointed at fixture lifetime once ordering was
controlled.

## `validate-all` runs the java and kotlin gauges concurrently (2026-07-20)

Both gauges drive Gradle in the **same** vendored monorepo
(`vendor/mongo-java-driver` — the Kotlin driver ships inside the Java driver's
repo). At the default `--jobs 4` they are scheduled concurrently, and the two
Gradle builds contend on the shared project directory / build outputs. Observed
2026-07-20: `validate-all --jobs 4` reported `kotlin FAILED (rc=1)` with
`no JUnit XML written (gradle exit 1; build error?)` — a build failure, not test
failures. Re-running `invoke validate-kotlin` serially immediately afterwards
gave `294 passed, 0 failed, 244 skipped` and `BUILD SUCCESSFUL`.

The runner already special-cases `validate-cxx` (must stay serial because
mongocxx hard-wires port 27017). java + kotlin need the same treatment, but for
a different reason: shared Gradle project state rather than a port. Fix is to
make the two mutually exclusive in the thread-pool scheduling (a shared lock
keyed on the vendored repo) so the pair can still run alongside other gauges but
never alongside each other.

Until then, a `kotlin FAILED` from `validate-all` should be re-checked serially
before it is believed — and the same caveat applies to `java`.

## Warning-class RUSTSEC advisories (surfaced by cargo-audit, not gated) (2026-07-20)

The `cargo-audit` CI gate (`.github/workflows/cargo-audit.yml`, security review
I22) fails only on *vulnerability*-class advisories. These warning-class ones
(unmaintained / unsound) are printed in the gate log every run but do not block,
and are not otherwise tracked. None is a reachable exploit; all are
dependency-hygiene items to clear opportunistically when the owning dep is next
touched:

- **RUSTSEC-2025-0134** — `rustls-pemfile` 2.2.0 unmaintained. Transitive; its
  API folded into `rustls-pki-types`. Clears when the TLS dep chain updates.
- ~~**RUSTSEC-2026-0190** — `anyhow` unsound~~ — cleared (rust-backlog-clear
  slice): `cargo update -p anyhow` → 1.0.104 in all four lockfiles
  (`crates/`, `secantusdb`, `secantus-storage`, `secantus-storage-py`).
- **RUSTSEC-2026-0196 / -0197** — `cgmath` 0.18.0 unmaintained + `swap_columns`
  UB. Pulled in transitively via the geo/spatial stack (`s2`). No fixed release;
  revisit if the geo dependency is ever swapped.

The two *vulnerability*-class pyo3 advisories (RUSTSEC-2025-0020 / -2026-0177) are
tracked separately in #584 (the 0.22 -> 0.29 migration) and are the two IDs on
the gate's `--ignore` baseline. When #584 lands, drop those ignores.

## Oplog in-flight window races on Windows CI (2026-08-01) — RESOLVED by #743

`tests/test_oplog_visibility.py::test_find_seq_for_ts_waits_for_in_flight_mint`
fails intermittently on the Windows CI lanes (seen on PRs #739 and #741, both
of which touch only the SQL front end and cannot reach this code). It has not
been reproduced on macOS.

**This is a real product race, not a slow runner.** It was first mis-diagnosed
as `find_seq_for_ts`'s bounded 0.5s wait expiring under load, and the bound was
made injectable so the test could widen it to 30s. **It still failed after
that**, which rules the deadline out.

What the failure means, from the code: `find_seq_for_ts` returns the
committed-view answer once `r - 1 <= oplog_visible_tail_seq()`. With seq 2
minted inside an open batch transaction, `oplog_visible_tail_seq()` must report
`min(_oplog_in_flight) - 1 == 1`, so the wait continues until seq 2 commits and
the answer is 2. Getting 3 means `_oplog_in_flight` did **not** contain seq 2 at
that moment, so the in-flight registration was either not yet visible to the
reading thread or was dropped.

Why this matters beyond a red lane: `oplog_visible_tail_seq` is what stops a
change-stream reader advancing past a hole. If the in-flight window can be
observed empty while a mint is genuinely in flight, a `startAtOperationTime`
position can be finalised above an uncommitted entry and that event is
**permanently skipped** when its transaction commits. That is silent event loss,
which is exactly what the visibility work in #706 existed to prevent.

Where to look: `Storage._mint_oplog_seqs` registers the range under
`_oplog_seq_lock`, and `_deregister_minted` removes it at the emitting scope's
resolution point, with the pending ranges parked on **thread-local**
`_tls.pending_minted`. Suspect the interaction between that thread-local
bookkeeping and a batch transaction whose emit and commit happen on different
threads from the reader, plus `oplog_visible_tail_seq_nolock`'s deliberately
lock-free dict read. Windows' coarser scheduling quantum likely just widens a
window that exists everywhere.

Do not "fix" this by deselecting the test or by widening a timeout again.

**RESOLVED (verified 2026-08-09): this was the sample-after-scan bug #743
fixed the same evening; the entry was written mid-investigation and never
closed.** The timeline is decisive: every observed failure — including the
still-failed-with-a-30s-bound runs — rode PRs #739 (16:02) and #741 (18:05)
on 2026-08-01, and #743 ("sample the visible tail before the scan, not
after — silent event loss") merged at 19:22 that day, touching exactly this
test with the load-bearing sample-order comment now in
``find_seq_for_ts``. Since #743: zero failures of this test across every
lane (8 days of CI, Windows lanes on every push — the two later red runs
that existed were the since-fixed ``test_math_funcs_via_driver`` numeric
failures, no ``find_seq_for_ts`` involvement). Freshly re-verified against
current main: a 500-iteration jittered stress of the exact choreography in
fast-storage mode plus 500 more under ``SECANTUS_FORCE_DURABLE=1`` — zero
failures — and a code audit of the invariants (range registration is atomic
with minting under ``_oplog_seq_lock``; ``_mint_ts`` stays strictly
monotonic even under a backwards wall clock; with the visible tail sampled
before the scan, no event-loss interleaving is constructible).

## UNIQUE constraints across transactions (2026-08-01) — FIXED (storage + SQL)

**Was: a data-integrity bug on the SQL front end; duplicates were silently stored.**
The common case is fixed (see "What is fixed" / "What remains" at the end of
this entry); the original diagnosis is kept because the residual holes share it.

Reproduce in one script: connection A opens a transaction and reads (taking its
snapshot), connection B inserts and commits a row, then A inserts a row with the
same `UNIQUE` value and commits. Both rows land. Real PostgreSQL 14.13 rejects
A's insert with `23505 duplicate key value violates unique constraint` — checked
side by side.

```sql
CREATE TABLE uq (id bigint primary key, val int unique);
-- A: BEGIN; SELECT count(*) FROM uq;      -- snapshot taken
-- B: INSERT INTO uq VALUES (1, 42);       -- commits
-- A: INSERT INTO uq VALUES (2, 42); COMMIT;   -- should fail, currently succeeds
-- => 2 rows, 1 distinct val
```

Mechanism: uniqueness is enforced by `Storage._unique_conflict`, a prefix probe
over `table:secantus_index_entries` **read through the caller's transaction
snapshot**. A row committed after that snapshot is invisible, so no conflict is
detected. WiredTiger cannot catch it either: an index entry's key is
`escape(sortkey) + \x00\x00 + RecordId`, so two docs sharing an indexed value
produce *different* keys and never collide.

Today's autocommit path survives only because each statement is its own short
transaction (fresh snapshot) *and* `_coll_lock(db, coll)` serialises the
probe-and-insert within a process. Neither protects a multi-statement
transaction, which is why the explicit-transaction case above is broken now.

Fixing it is the prerequisite for the implicit-transaction work below — that
change gives every batch a snapshot older than its statements, which widened
this from a narrow window to routine (a probe test stored 150 rows across 20
distinct values). Options, in rough order of fidelity:

- Give unique indexes a key with **no RecordId suffix**, so WiredTiger's own key
  uniqueness and conflict detection enforce them, as a real unique index does.
  This is the principled fix and the one that also handles two *simultaneous*
  uncommitted inserts. It changes the on-disk entry layout, so it needs an
  `entryFormat` bump and the Rust twin.
- Or probe on a **fresh session** (latest committed) in addition to the
  transaction's own view, so own-transaction writes are still seen. Cheaper, but
  it leaves the simultaneous-uncommitted case to the collection lock and costs an
  extra probe on every insert into a table with a unique index — measure against
  the perf gates.

Do not paper over this by re-scoping the tests. `tests/test_pgserver_concurrency.py`
already asserts the invariant for the autocommit path; the explicit-transaction
case needs a test of its own once fixed.

## `numeric` exactness (2026-08-01) — FIXED, with one divergence left

`planner._literal` turns any decimal literal into a Python `float`, so Postgres'
arbitrary-precision exact `numeric` behaves like a double. Verified against real
PostgreSQL 14.13:

| expression | ours | real PG |
| --- | --- | --- |
| `SELECT 0.1 + 0.2` | `0.30000000000000004` | `0.3` |
| `SELECT 0.1 + 0.2 = 0.3` | **false** | true |
| `SELECT 0.000000` | `0` | `0.000000` |
| `SELECT 12345678901234567890.12345 + 1` | `1.2345678901234567E+19` | `12345678901234567891.12345` |

The last row is the serious one: a value wider than a double silently drops
digits, which for money-shaped data is corruption rather than rounding. The
scale loss is what pgjdbc's `NumericTransfer2Test` fails on (26 of the gauge's
remaining failures, all `getBigDecimal for SELECT 0.x00000`).

Parsing the literal as `Decimal` fixes the scale half and is a two-line change,
**but it is not sufficient on its own** — it was tried and reverted. Arithmetic
is evaluated elsewhere and still coerces to float, so the exactness rows above
stay wrong, and `secantus.expressions` raises `ExpressionError` when a `Decimal`
reaches it (`tests/test_sql_colref.py::test_col_lt_arithmetic` and
`tests/test_sql_types.py::test_integer_division_truncates_in_aggregate_args`
both fail). Land it as one piece: literal parsing, the expression engine's
numeric tower, and the float8-vs-numeric distinction (`1e3` is float8 in
Postgres, a plain `1.5` is numeric) together.

### UNIQUE across transactions — what is fixed, and what remains

**RECONCILED 2026-08-10: everything below is superseded — both residual holes
are CLOSED (#775 storage, #778 SQL wiring); the section is kept as a stub so
in-flight references resolve.** The interim `find_matching_committed` probe
described here (and its two residual holes: a transaction that had already
written to the table, and two simultaneous uncommitted inserts) was replaced
by the storage-level fix the old text proposed "for when someone takes it on"
— implemented additively as `table:secantus_unique_keys` rather than the
entry-table re-key, so no format bump was needed. The Mongo persona's
multi-document-transaction gap closed with it (the unique-keys table is
engine-enforced, snapshot-independent). Pinned by
`tests/test_storage_unique_keys.py` and the wire-level
`tests/test_sql_unique_across_transactions.py` concurrency tests (eight
concurrent writers, exactly one winner). See "The storage-level fix landed"
and "SQL UNIQUE is now storage-backed too" further down for the design record.
The one deliberate residue: **DEFERRABLE constraints** keep the commit-time
check and with it the old cross-transaction race (narrow, documented there).

**Post-reconcile regression, found and fixed the same week (unique-keys-drop-
purge slice, 2026-08-10):** the claims table survived namespace teardown — no
drop path purged `table:secantus_unique_keys`, so DROP TABLE → recreate →
re-insert falsely rejected the value with 23505 (the FALSE-rejection class
#775's additive design was supposed to make impossible). Caught by the first
full weekly sweep after #775: slt's `index/delete` lane failed in both
engines (its corpus cycles drop/create with unique indexes), reproduced in
eight lines. (It was NOT the pgjdbc 2h blowout's cause — that turned out to
be a leaked idle-in-transaction connection pinning WT's oldest snapshot; see
the pgjdbc-lane entry below.)
All five teardown paths now purge claims (drop_collection / drop_database /
drop_index / drop_all_indexes / rename src+dst), pinned by
`TestClaimsDieWithTheirNamespace` in `tests/test_storage_unique_keys.py`.

### `numeric` — what landed, and the division-scale divergence

**Fixed.** A decimal literal is now `Decimal128`, the same exact type a
`numeric` column already stored (`typemap.number_literal`, shared by the
planner and the scalar evaluator — they previously carried separate copies,
which is why fixing only one left arithmetic on floats). Exponent notation is
numeric too: `pg_typeof(1e3)` is `numeric`, checked against 14.13, which is not
what the notation suggests. Comparison operators unwrap Decimal128 rather than
swallowing the TypeError and answering false.
`tests/test_sql_numeric_exact.py` covers it.

~~**Divergence: Postgres derives a scale for numeric division; we do not.**~~
— **FIXED (numeric-div-scale slice)**: `select_div_scale` is ported verbatim
into `typemap.numeric_div` (base-10000 operand weight + leading digit →
16-significant-digit result scale, floored by the operands' display scales,
clamped to 0..1000; ROUND_HALF_UP, PG's half-away-from-zero), wired through
`scalar._pg_div` for numeric/numeric (int/int keeps truncation; any float8
operand keeps float coercion). Probed against a LIVE local PostgreSQL 14.13:
20 division cases, byte-identical text renders end-to-end. Pinned by
`tests/test_sql_numeric_div_scale.py` (the probed battery) and the upgraded
`test_float_by_numeric_typmod`, which now asserts the scale it previously
deliberately ignored.

**Anything that consumes a numeric value must unwrap it first**
(`typemap.unwrap_numeric`): `Decimal128` implements no Python numeric
protocol, so `int()`, `float()`, arithmetic, comparison and unary minus all
reject it outright. Landing this change flushed out call sites in `coerce`,
the percentile/sequence planners, `log()`, unary minus, and the binary wire
encoders — each of which had been fed a float before and raised a bare
TypeError on the first decimal.

**Residual: values beyond Decimal128's 34 significant digits.** The pgjdbc
numeric cluster went 26 -> 4 with this change; the remaining 4 are
`SELECT 0.1000000000000000000000000000000...` and its integer counterpart,
which carry more than 34 significant digits and so round when stored as
Decimal128. Postgres' numeric is arbitrary precision. Fixing it means not
using Decimal128 as the carrier — a bigger change than this one, since
Decimal128 is what the storage layer stores for a `numeric` column — and it is
the same limit already noted against `typemap.coerce`.

## ORDER BY does not sort rows expanded by a set-returning function (2026-08-02)

Silent wrong ordering, pre-existing and affecting every SRF in the select
list, not just the one added most recently:

```sql
CREATE TABLE src (id int primary key); INSERT INTO src VALUES (1);
SELECT unnest(ARRAY[9,8,7]) FROM src ORDER BY 1;   -- we give 9, 8, 7
                                                   -- Postgres gives 7, 8, 9
```

Cause: in `executor._evaluated_value_rows` the ORDER BY keys are computed once
per SOURCE document (`_order_key` over `make_scope(doc)`) and only then is the
row expanded by `_expand_srf`. Every expanded row therefore inherits one
identical sort key and they keep the array's own order. With a single source
row nothing sorts at all.

Fixing it means deriving the sort key from the EXPANDED row when an ORDER BY
term refers to an output column that an SRF produced — positional terms
(`ORDER BY 1`) map straight onto the output tuple, and an expression term has
to be matched against the projection. Watch DISTINCT ON while doing it: that
key is deliberately row-level and computed before expansion, and the two must
not be conflated.

Pinned by `tests/test_sql_pg_expandarray.py::TestFieldSelection::
test_order_by_does_not_sort_expanded_rows` so the current shape is visible
rather than assumed correct.

## Unquoted identifiers are not case-folded (2026-08-02) — FIXED

Postgres folds an unquoted identifier to lower case, so `AS TABLE_NAME` and a
later `r.table_name` are the same column. We preserve the case as written and
compare exactly, so they are two different names:

```sql
CREATE TABLE src (id int primary key); INSERT INTO src VALUES (1);
SELECT r.table_name FROM (SELECT id AS TABLE_NAME FROM src) r;
-- ERROR: column "table_name" does not exist   (Postgres: 1)
SELECT r.TABLE_NAME FROM (SELECT id AS table_name FROM src) r;
-- ERROR: column "TABLE_NAME" does not exist   (Postgres: 1)
```

Matching case works, which is why this went unnoticed — code that writes an
alias one way and reads it back the same way never trips it. Anything
generated, or written in the SQL-standard uppercase style, does.

**This is now what blocks the pgjdbc `UpdateableResultTest` cluster (26).**
Its `DatabaseMetaData.getPrimaryKeys` aliases columns as `AS TABLE_NAME` /
`AS KEY_SEQ` in a subquery and reads them back lower-cased from the outer
query. The `_pg_expandarray` support those queries also needed has landed, and
the failure message moved from `function _pg_expandarray() is not supported`
to `field access on a non-composite value` to `column "table_name" does not
exist` — each fix exposing the next layer.

Fixing it means folding unquoted identifiers at parse/resolve time — aliases,
column references, and table names alike — while leaving quoted ones exact.
Note sqlglot records whether an identifier was quoted (`Identifier.quoted`),
which is the signal to key off. Check the catalog paths too: table and column
names created unquoted should also fold, and `pg_catalog` lookups already
assume lower case.

### Case folding landed; the empty column name that followed is also fixed

Folding happens once in `planner._fold_unquoted_identifiers`, right after the
parse: every unquoted `Identifier` is lower-cased and quoted ones are left
exactly as written, so no downstream consumer needs case logic of its own.
`PUBLIC` is restored in `_grant_principals` because Postgres treats it as a
keyword and `information_schema.role_table_grants` reports it upper-case.

The pgjdbc `UpdateableResultTest` cluster went **33 -> 24** and the gauge
94.2% -> 94.4%. The remaining 21 now fail with:

```
ERROR: column "" does not exist
```

— an EMPTY column name, the same smell as the `relation ""` bug that turned
out to be `TypeInfoCache`. It comes from `DatabaseMetaData.getPrimaryKeys`,
whose outer query wraps the `_pg_expandarray` subquery. The obvious candidates
were each reproduced in isolation and are all fine: a `NULL AS ALIAS` column in
a derived table, a quoted upper-case outer alias over a folded inner one, and
the missing space in the driver's own `AS "IS_NOT_NULL"FROM` concatenation.

**Do not reconstruct the query by hand to find it** — that is what cost the
time here. Log the statement server-side (the `pgserver` query path) during a
`validate-pgjdbc` run and read back exactly what the driver sent, including
the parameter placeholders and the `pg_table_is_visible` / `ct.relname = ?`
clauses that only appear for some call shapes.

### `column "" does not exist` — FIXED (RowDescription source identity)

Found by doing what the note above said: instrumenting the server to log the
statement behind each SQLError during a `validate-pgjdbc` run, rather than
reconstructing the query. What the driver actually sent was

```sql
UPDATE primaryunique SET  "" = $1 WHERE "id" = $2
```

— pgjdbc had generated the empty column name itself. Its
`PgResultSetMetaData.getBaseColumnName` returns `""` the moment a field's
table oid is 0, and we sent 0 for the table oid and 0 for the attnum on every
RowDescription field. `ColumnDesc` now carries both and the wire encoder emits
them.

Note the Describe path needed fixing separately from the executed path: the
extended protocol describes without executing (`engine._describe_statement`),
and that is the one a JDBC client reads. Fixing only the executed path moved
nothing, which is why `tests/test_sql_row_description_source.py` covers both.

`UpdateableResultTest` went 24 failures -> 8. The remaining 8 are a mix rather
than a cluster: two more `column ""` (a select shape that still describes
without a base table — likely the pipeline/evaluated path, which builds its
columns from `(name, tag)` pairs with no Column object to carry the identity),
a timestamp-precision mismatch, an `INSERT has 2 values but 3 columns`, and a
"resultset tuples, but no field structure" protocol complaint.

## Cached-plan invalidation on DDL (2026-08-03)

Postgres caches a prepared statement's plan. When DDL changes a table so the
statement's result type would differ, re-executing it fails with

```
0A000  cached plan must not change result type
```

which aborts the transaction. We re-plan on every execute, so the statement
just succeeds with the new shape and the transaction lives on.

This is the last cluster in pgjdbc's `AutoRollbackTest` (8 of its original 24 —
the other 16 were the transaction-abort and `DEALLOCATE ALL` command-tag pair,
both now fixed). Its assertion reads "autosave=NEVER + flushCacheOnDdl=false,
thus the transaction should be killed".

Sketch: record each prepared statement's described column shape at Parse, and a
catalog generation counter bumped by any DDL. On Execute, only when the
generation has moved since the statement was prepared, re-derive the shape and
compare — so the common path costs one integer comparison and DDL, which is
rare, pays for the re-describe. Raise `feature_not_supported` with Postgres'
exact wording, since drivers match on the message as well as the SQLSTATE.

## Weekly validate.yml failures (2026-08-03 triage)

The 2026-07-27 and 2026-08-03 scheduled runs did not finish green. Three
distinct problems, triaged from the run logs:

- ~~**`rust` gauge (mongo-rust-driver vs the Python server) crawls to the
  6-hour job kill** — both weeks; 7 minutes on 2026-07-20.~~ **FIXED
  (rust-gauge-wedge slice).** Root cause was a compounding loop: the cargo
  cache (populated 2026-07-20) hit the 7-day eviction TTL; the cold run's
  FIRST per-filter `cargo test` invocation had to compile the whole driver
  test binary inside the 600s per-invocation timeout, which killed cargo
  mid-compile, so each of the 88 invocations resumed a partial build inside
  its own 600s window (log signature: hours-long silent gaps between filter
  outputs); the job overran the 6h default and was cancelled; and the
  cancelled job skipped `actions/cache`'s success-only save, so every later
  weekly run started cold again — a permanent wedge. Fix: (1)
  `rust_validation/runner.py` pre-builds with `cargo test --no-run` under its
  own 3600s wall so the 600s limit bounds only test runtime; (2) the cargo
  cache is split into `actions/cache/restore` + an `if: always()`
  `actions/cache/save`, so even a failed/killed run persists the compiled
  target; (3) the `validate` job carries `timeout-minutes: 120` so any future
  wedge fails fast instead of burning six runner-hours per lane.
- ~~**REFINED (2026-08-04): rust-driver tests genuinely hang against the
  Python server**~~ — **ROOT-CAUSED AND FIXED (2026-08-09, insert-chunked-txn
  slice): a WiredTiger dirty-cache livelock from writing one whole insert
  message in a single statement transaction.** The per-filter streaming logs
  (a lane-filtered `only=rust` dispatch, run 30889017762) named the wedge
  precisely: filters 1–32 fast, then `test::coll::large_insert` (35k ×
  1.18KB docs ≈ 39.5MB in ONE insert command) hung — and EVERY later filter
  timed out behind the wedged server. The test skips on non-Linux
  (`std::env::consts::OS != "linux"`), which is the entire "CI-only,
  passes-locally" mystery: it had simply never run on the dev Mac. Once
  replicated via pymongo it wedged on macOS too. A stack probe caught the
  connection thread drafted into the oplog-prune scan, and a cache-size
  experiment nailed the mechanism: 39.5MB @ 4G cache → 1.6s; 11MB @ 128M
  cache → wedged. `insert()` wrote the whole wire batch (docs + full-doc
  oplog entries + index entries ≈ 2–3× message bytes) as UNEVICTABLE dirty
  content in one WT transaction; near the cache's ~20% dirty-stall fraction
  WT drafts every thread into eviction that cannot evict anything — only the
  writer's own commit could free the cache. The Jul 20 → Jul 27 flip was the
  RecordId/`_id`-index rewrites adding bytes-per-doc to that same
  transaction, pushing the same test across the 1G default cache's line.
  **Fix:** `storage.insert` now chunks the batch into ≤1000-doc / ≤4MB
  statement transactions (mongod chunks internal insert batches too; batch
  inserts are per-document atomic only, so the commit points are invisible
  to clients), with the conflict-retry moved to chunk scope and capped-FIFO
  fresh-keys threaded across chunks. Guards:
  `test_storage.py::test_large_batch_insert_survives_a_small_cache`
  (35k × 1.1KB @ 128M cache) + ordered/unordered cross-chunk semantics
  tests.
- ~~**Rust server: same transaction-size class, currently saved by cache
  headroom.**~~ — **mirrored (rust-insert-chunked-txn slice)**:
  `crates/secantus-storage`'s `insert` now commits in the same bounded
  chunks as the Python fix (≤1000 docs / ≤4MB per statement transaction;
  conflict-retry per chunk, capped-FIFO fresh keys threaded across chunks
  and merged post-commit, empty batch still lazily creates the collection).
  Previously only the 4G embedded default cache stood between a
  `--cache-size 256M` daemon and the same WT dirty-cache livelock. Pinned by
  `tests/batch_insert.rs::large_batch_insert_survives_a_small_cache`
  (35k × 1.1KB @ 128M cache) + ordered/unordered cross-chunk semantics
  tests.
- [ ] **User (multi-document) transactions dirty-budget guard** — the
  remaining member of the livelock class. **Python server: DONE
  (txn-too-large-guard slice)** — `_emit_oplog`'s buffering branch tracks
  the transaction's approximate write volume (`handle.dirty_bytes`, from
  the full-doc oplog entries; engine dirty ≈ 2×) and raises past ~15% of
  the cache (0.75 × WT's ~20% dirty trigger, mirroring mongod's
  threshold), surfaced as mongod's `TransactionTooLargeForCache` (313, no
  transient label — a retry would hit the same wall; the failed statement
  aborts the transaction server-side). Pinned by
  `test_storage_user_txn.py::test_transaction_dirty_budget_guard` and
  `test_transactions.py::test_transaction_too_large_for_cache` (128M
  cache). **Rust server: DONE (rust-txn-too-large slice)** — the same
  guard, harvested through the `PENDING_DIRTY_BYTES` thread-local into
  `UserTransactionHandle.dirty_bytes` per statement (the `PENDING_MINTED`
  pattern), budget parsed from the connection config's `cache_size`
  (default 4G → trips past ~300MB emitted), surfaced as 313 via the
  adapter (`code_name_for` knows `TransactionTooLargeForCache`; not in
  the transient set). Pinned by
  `tests/txn_budget.rs::transaction_dirty_budget_guard` (128M cache).
  **The dirty-cache livelock class is now closed on both servers**:
  chunked batch inserts (#787 / #789) + the transaction dirty budget
  (Python #791 / #792+#793).
- [ ] **Multi-document update/delete chunking** — the class's final member:
  updateMany / deleteMany rewrite an unbounded matched set. **Rust server:
  DONE (rust-multiwrite-chunked slice)** — `update_matching_core` routes
  multi=true (outside user transactions) through a chunked driver: one
  RecordId scan under the coll lock, then ≤1000-doc / ≤4MB statement
  transactions that RE-FETCH each doc row inside their own transaction
  (reusing the scan's blobs across chunk commits would let a user-txn
  commit landing between chunks be silently overwritten — no overlapping
  WT transactions, no conflict detection; found during design). RecordIds
  are partitioned across chunks and a conflict retries only its own
  rolled-back chunk, so `$inc` applies exactly once. `delete_matching`
  (limit=0) mirrors it. Zero-match runs delegate to the single-txn body
  (which degenerates to the upsert branch) AFTER releasing the coll lock —
  holding it across the delegation self-deadlocks on the non-reentrant
  mutex (found by the first test run). Chunk-boundary states are
  reader-visible, which is mongod-faithful: updateMany/deleteMany are
  per-document write units, documented non-atomic. Pinned by
  `tests/multiwrite_chunk.rs` (35k-doc rewrite + deleteMany @ 128M cache;
  exactly-once `$inc` across chunks; bounded paths unchanged).
  **Python server: DONE (py-multiwrite-chunked slice)** — the twin driver:
  `update_matching` (multi, outside user txns) and `delete_matching`
  (limit=0) route through chunked drivers with the same re-fetch +
  partition rules (the coll RLock makes the zero-match delegation
  reentrancy-safe); single-doc / upsert / in-transaction paths keep the
  single-`_batch_transaction` body. Pinned by the same three guards in
  `tests/test_storage.py` (35k rewrite + deleteMany @ 128M cache;
  exactly-once `$inc`; bounded paths). **With this, the WT dirty-cache
  livelock class is fully closed on both servers across all three
  surfaces: batch inserts, multi-document updates/deletes, and
  multi-document transactions.**
- ~~**`slt` gauge "regression"**~~ — NOT a regression; **timeout calibration,
  fixed (rust-gauge-wedge slice).** 2026-08-03 was the slt lane's FIRST
  weekly run (the SQL gauge lanes postdate the 2026-07-27 schedule), and the
  ~15 `postgres-extended` `index/*` / `random/*` files it timed out at 300s
  pass locally in ~25s each — verified by running
  `index/orderby/10/slt_good_0.test` against BOTH current main and the
  2026-07-27 head (`c4873cb0`) on this machine: 24.5s vs 24.4s, byte-same
  timing, so no server slowdown exists. The 2-core CI runner is simply
  5-10x slower than the dev machine the 300s cap was calibrated on (its
  passing `select3.test` extended lane took 277s — right at the wall).
  `FILE_TIMEOUT_SECONDS` is now `SECANTUS_SLT_FILE_TIMEOUT`-overridable and
  `validate.yml` sets 900 for the slt lane (with a 300-minute lane timeout,
  since the honest runtime is long on that hardware).
- [ ] **Don't run the pgjdbc gauge alongside the test suite.** It is
  timing-sensitive, and its wall clock under contention says nothing about the
  server: a run competing with a full pytest suite blew the (then 1h) budget
  while the same code re-run alone finished in **8m16s**. Note also that a
  healthy full run is legitimately long — `CopyLargeFileTest` alone is ~282s,
  inherently, since it streams a large file through COPY (`CopyTest` beside it
  is 1.8s). Check `uptime` before drawing any conclusion from gauge timings;
  this machine has repeatedly carried runaway processes from parallel sessions.
- **pgjdbc 2h lane hang — fully closed (wt-snapshot-release slice,
  2026-08-10).** The idle-in-transaction default (#810, below) fixed one
  vulnerability but not the observed wedge: with the fix in place the CI
  sweep and a local full-gauge run still wedged in `CopyLargeFileTest`, and a
  live `pg_stat_activity` check showed **no open transactions** (note:
  `xact_start` is a hardcoded stub and `state` never reports
  "idle in transaction" — do not use them to rule out open txns). The real
  pinner: an idle connection's cached per-thread WT session left with a
  positioned cursor after its last statement — an *implicit* WT transaction,
  invisible to all PG-level accounting, pinning the oldest-txn horizon
  (`transaction range of IDs currently pinned` grew to ~100k; resetting every
  cached session mid-wedge collapsed it instantly and unwedged the run).
  Arming needed a specific mix of prior traffic (the batch classes + the
  error-path classes + ConnectionTest — every subset was clean; the union
  wedged deterministically). Fix: `Storage.release_thread_snapshot()` called
  by both wire servers before each idle wait; regression tests in
  `tests/test_storage_snapshot_release.py` read the WT pinned-range statistic
  directly. **Rust server audited — immune by construction** (2026-08-10):
  each autocommit operation runs on a fresh WT session (`OpSession::Fresh`
  in `crates/secantus-storage/src/lib.rs`) closed when the operation drops,
  and transaction sessions live only for the transaction (bounded by the
  lifetime limit) — no cached per-connection session survives into an idle
  wait, so an idle Rust connection cannot pin the oldest-txn horizon.
- **pgjdbc 2h lane hang — first fix (pg-idle-txn-default slice,
  2026-08-10).** The wedge was a leaked idle-in-transaction connection: a
  failed autocommit-off test (the CleanupSavepoints / AutoSave cluster) left
  its connection open mid-transaction for the rest of the run, and with
  `idle_in_transaction_session_timeout` defaulting to 0 the server never
  aborted it. The open WT transaction pins the oldest snapshot, so every
  later write's history is unevictable — per-operation cost grows linearly
  with churn (probe: flat 3.7s/100k-cycle control vs +2s/cycle unbounded
  with one abandoned txn) — until `CopyLargeFileTest`'s 1M-row COPY/TRUNCATE
  churn stalls in `__wt_page_in_func` sleep-retry and the whole suite hangs
  (JUnit's same-thread 60s timeout cannot interrupt a blocked socket read).
  Fix: `SecantusPGServer` now applies a 120s server-config default for the
  GUC (session `SET` overrides, 0 opts out, `RESET` returns to the server
  value) — verified by re-running the leak + churn scenario end-to-end
  (flat timings, leak reaped with 25P03).
- **Read-only transactions are enforced (pg-readonly-isolation slice,
  2026-08-11)** — writes under `BEGIN READ ONLY` / `SET TRANSACTION READ
  ONLY` / `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY` fail with
  PG's 25006, and `SHOW TRANSACTION ISOLATION LEVEL` (pgjdbc's spelling)
  resolves to `transaction_isolation`. Known divergences from PG, none
  exercised by any gauge: temp-table writes are also blocked (PG allows
  them in read-only txns), and `SELECT … FOR UPDATE` / `nextval()` are NOT
  blocked (PG blocks both).
- [x] **RESOLVED: Rust server: `drop` of a heavily-churned collection wedges
  behind a WT eviction storm.** Root cause found by code reading, then
  reproduced deterministically in seconds (`crates/secantus-storage/tests/
  drop_chunk.rs`): collections share the sharded doc tables, so `drop` is a
  row purge — and it ran as ONE statement transaction. A collection whose
  delete markers exceed the cache's dirty budget gets a cache-pressure
  `WT_ROLLBACK` (unevictable dirty content — the same class the chunked
  insert / updateMany / deleteMany work closed), which the blanket
  `WT_ROLLBACK -> WriteConflict` mapping fed into `retry_write_conflicts`'s
  UNBOUNDED retry loop: re-run purge, re-storm, roll back, forever. That is
  the whole incident signature — eviction threads spinning with zero
  connections (the drop thread kept retrying server-side), no self-recovery,
  SIGTERM dead behind the in-flight dispatch. The three negative repro
  attempts failed because the missing ingredient was the COLLECTION-SIZE-TO-
  CACHE ratio, not churn history: at the 4G default cache 1M small docs fit;
  a 165MB collection under a 128M cache wedges every time. Fixed with a
  chunked two-phase drop: phase 1 (small txn) unregisters the collection +
  writes a `secantus_drop_tombstones` row + emits the drop oplog entry;
  phase 2 purges rows in 4000-row statement transactions, then clears the
  tombstone. A crash mid-purge is finished at the next open
  (`recover_pending_drops`) before traffic can re-create the name, so orphan
  rows can't resurface. `dropDatabase` converted the same way; inside a user
  transaction both keep the old single-transaction path (the user-txn dirty
  budget guard bounds it). The tombstone table is additive to the shared
  layout (unique-keys precedent). Original entry follows for the incident
  record.
  Original: **Rust server: `drop` of a heavily-churned collection wedges behind a
  WT eviction storm.** Found by the 2026-08-11 concurrency-report refresh:
  after rounds of multi-writer churn (~1M+ docs inserted per row), a
  subsequent `drop` sat in dispatch 40+ minutes while every WT eviction
  thread spun in `__evict_lru_pages` / `__evict_page` / `__tree_walk` —
  and the storm did NOT self-recover after the client disconnected (the
  daemon burned ~112% CPU for 1.5h with zero connections) nor respond to
  SIGTERM (needed SIGKILL). Native stacks captured in the session
  scratchpad (`server.sample`). Standalone `drop` of a fresh 1M-doc
  collection is 1.46s — the wedge needs the accumulated-churn cache
  state. Same WT-livelock family as the chunked-write / pinned-snapshot
  fixes; likely WT wants the whole dirty tree evicted before the drop's
  exclusive dhandle access, and eviction thrashes. Repro attempts (2026-08-11, all NEGATIVE — drops sub-second): 3-4
  rounds x 1M-doc 4-writer churn then drop-oldest; 1KB payloads (4GB
  written) then drop-freshest-still-dirty; 5x drop-recreate cycles on the
  same names. So the wedge needs more than churn volume, dirty-cache
  drops, or name reuse alone — the incident server had hours of mixed
  sweep history (SIGTERM'd writers mid-batch, dozens of collections,
  the venv-staged binary). Note the Rust server DOES run a checkpoint
  thread (`checkpoint_seconds`), unlike the Python server. Next attempt
  should replay the original pre-drop harness verbatim for multiple full
  sweep runs on one daemon. Original repro shape:
  `bench.concurrency` rows without the fresh-collection-names harness fix
  (drop a prior row's collection mid-churn). Also note: a drop queued
  behind live writers starves indefinitely (lock fairness) — the bench
  harness now avoids drops entirely (fresh per-row collection names).
- [x] **RESOLVED: Rust server: `rename_collection` re-keys every row in one
  statement transaction** — now a chunked two-phase move reusing the drop
  tombstones (tombstone dst -> batched copy -> small switch txn -> batched
  src purge); both crash windows recover through the existing
  `recover_pending_drops` as plain drops, on both servers. Pinned by
  `crates/secantus-storage/tests/rename_chunk.rs` (150k docs under a 128M
  cache — the shape that wedged — plus index/claim/order/drop_target
  semantics). User-transaction renames keep the atomic path (dirty-budget
  guarded). Original entry: A rename of a collection whose re-key volume exceeds the cache's
  dirty budget would hit the same cache-pressure `WT_ROLLBACK` + unbounded
  `retry_write_conflicts` loop. Unlike drop, rename NEEDS single-transaction
  atomicity (a crash mid-rename must not leave the namespace half-moved), so
  the chunked-drop pattern doesn't transplant directly — it wants a
  tombstone-style two-phase move (register dst + tombstone src, batched
  re-key, finalize) with open-time recovery. Not observed in the wild
  (renames of huge collections are rare); bounded today only by luck of
  cache headroom. The `drop_target=true` purge inside rename shares the
  shape.
- [x] **RESOLVED (for real this time): the xdist "worker death" cluster
  (2026-08-13 → 08-16) — workers starved on the machine-wide rust-binary
  test flock until the GLOBAL 600s thread-method pytest-timeout os._exit'd
  them.** The definitive evidence (instrumented run, 2026-08-16): four
  workers entered `test_rust_binary_pitr` tests at 13:37:50-54, queued on
  the blocking `fcntl.flock` in the file's autouse serialization fixture
  (one holder + three waiters; a SECOND suite in a parallel worktree
  shared the same /tmp lock, stretching the queue), and died at
  13:47:48-52 — exactly 600s later. Signal tracing (SECANTUS_SIGTRACE=1
  handlers on TERM/INT/HUP/QUIT/USR1/2) captured NOTHING on the victims:
  not a signal at all, but pytest-timeout's thread-method `os._exit` —
  which the file's `timeout(1200, method="signal")` marker does NOT
  override for FIXTURE waits — with the dump swallowed by xdist capture.
  Every historical signature matches: victims always in (or queued behind)
  this file, multiple simultaneous deaths (xdist hands out a file's tests
  together, so several workers hit 600s within seconds), the "fast" test
  victim (blocked, not slow), never reproducible in isolation (no second
  suite → short queue), rerun-clean, no .ips, no dump. The earlier
  pytest-timeout finding (#878) was the same killer at a different depth —
  the "signal method" change (#886) missed because the flock wait lives in
  a fixture. FIX: the serialization fixture acquires the lock NON-BLOCKING
  in a 480s bounded poll and raises a NAMED AssertionError on expiry — a
  visible failure inside every timeout budget; the worker survives.
  Diagnostics kept: `pytest_handlecrashitem` (names crashed tests) and the
  env-gated `SECANTUS_SIGTRACE=1` signal-receipt tracer in conftest.
  **Coda (2026-08-17, the ACTUAL last piece): the file's
  `timeout(1200, method="signal")` marker never applied at all** — a
  second bare `pytestmark =` assignment (the binary-availability skipif,
  lower in the module) silently OVERWROTE the first, so the PITR test
  BODIES still ran under the global 600s thread-method timeout and a
  slow-disk run os._exit'd the worker mid-test (two more deaths,
  2026-08-16/17, same test, quiet machine, sigtrace silent, faulthandler
  empty — the os._exit signature). Both marks now ride one combined
  `pytestmark` list, and `tests/test_meta_pytestmark.py` walks every test
  module's AST rejecting double assignment so the overwrite pattern can't
  recur anywhere.
