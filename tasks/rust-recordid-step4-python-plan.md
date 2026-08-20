# RecordId step 4 — Python-server mirror: implementation plan

> **DELIVERED (audited 2026-08-20).** The Python server mirrors the Rust RecordId
> scheme byte-for-byte: `src/secantus/storage.py` carries ~86 RecordId /
> `_frame_doc_value` references and CLAUDE.md documents the shared layout as
> "byte-identical to the Rust server's layout (cross-server backup / PITR
> portability)". Cross-server restore is covered by
> `tests/test_rust_pitr_cross_server.py`. Kept for the porting sequence.

**Goal.** Bring `src/secantus/storage.py` (+ `src/secantus/commands.py`) to the
**byte-identical** RecordId on-disk scheme the Rust server already ships (steps
1–3). Cross-server backup / PITR portability requires every stored byte to match,
so the Rust code is the *spec*, not just a reference.

**Decision on record (2026-07-24, Joe): FAIL-FAST, no migration** — for both 4a
and 4b. Even though the Python server is the shipped PyPI package, an old-format
(`SSu` / id_key) store is **refused at open** with a clear error, exactly like the
Rust `_reject_pre_recordid_doc_format` / `reject_legacy_index_entry_format`. This
keeps the work a straight mirror and off the risky open-path-migration surface.

**Do it as THREE gated sub-PRs** (4a → 4b → 4c), one per Rust step. Highest-risk
("silent data loss") class — do NOT attempt all three in one pass. Each sub-PR must
end green on: `invoke test` (full Python suite), `invoke lint`, and the pymongo
gauge (`./inv validate` — Python server; and re-run the Rust gauge to prove
byte-parity didn't break the Rust reader).

---

## The byte contract (what Python must reproduce exactly)

The authoritative source is the Rust code on `main`:

| Piece | Rust (spec) | Python (to change) |
|---|---|---|
| Doc-table `key_format` | `SSq` (`DOC_TABLE_CFG`, `secantus-storage/src/lib.rs`) | `_DOC_TABLE`/shards created `SSu` at `storage.py:1201,1205` → `SSq` |
| Doc-table value framing | `frame_doc_value(id_key, blob)` = `[u32-LE id_key_len][id_key][blob]`; `unframe_doc_value` | new `_frame_doc_value`/`_unframe_doc_value` |
| RecordId | monotonic per-collection nat-seq (`write_nat_entry` mints, returns i64) | `_mint_nat_seq` exists (`storage.py:1473`); reuse |
| `_id` index | `secantus_natural_seq`: `(db,coll,id_key) → RecordId`, `overwrite=false` | `_NAT_SEQ_TABLE` (already `SSu→q`); keep |
| Forward NAT table | **DROPPED** (the 4th write; 4→3 write-amp) | `_NAT_TABLE` write in `_write_nat_entry` (`storage.py:1479`) → remove |
| Index-entry trailing half | 8-byte **big-endian** RecordId (`pack_entry(kb, recordid)`) | `_pack_entry`/`_unpack_entry` (`storage.py:253,263`) currently id_key |
| Index catalog marker | `options.entryFormat = 2` (`ENTRY_FORMAT_RECORDID`) | add in `create_index` (`storage.py:5079`) |

Pin byte-parity with a cross-server test where feasible: write a store with the
Rust server (or the `_secantus_storage` extension), open it with the Python
`Storage`, and assert reads match — and vice-versa. At minimum, unit-test the
frame layout and the 8-byte-BE entry against the Rust unit tests' exact bytes
(`pack_entry_layout_and_unpack_roundtrip`, `frame_doc_value` tests in
`crates/secantus-storage/src/lib.rs`'s `mod tests`).

---

## 4a — doc-table RecordId keying (mirror of Rust #613 `b90b5490`) — **DONE**

All eight steps below are implemented in `src/secantus/storage.py`. Notes from the
pass, for 4b/4c:

- `_scan_docs` now yields `(RecordId, id_key, blob)`; ~20 call sites updated.
  `_scan_docs_natural` is a thin alias (kept for call-site readability).
- `_write_nat_entry` returns the RecordId, or `None` for a duplicate `_id` — the
  insert path shapes the 11000 error from that, so no new exception type crosses
  the storage boundary. A WT rollback still surfaces as `WriteConflictError`.
- `_delete_doc_row(db, coll, recordid)` is the single doc-row delete used by
  `delete_matching`, capped eviction and `prune_ttl`.
- Two methods were pinned to their old contract in 4a because they are
  *documented* as id_key-ordered: `scan_docs_after_id_key` sorted its result by
  `id_key`, and `collection_min_id_key` took the `min` across the scan rather than
  the first row. Both were tailable-cursor-only and **4c deleted them.**
- The `hint: "_id_"` path sorts the scan by `id_key` (it used to rely on the doc
  table already being in that order).
- `rename_collection` needs no change: it copies doc rows *and* `_NAT_SEQ_TABLE`
  rows verbatim, so RecordIds are preserved rather than re-minted (this is where
  the Rust side had to rebuild index entries — Python doesn't).
**Byte-parity VERIFIED against the shipped Rust storage** (2026-07-25, the
`_secantus_storage` extension built from `main`, one process per open — WT allows
only one open of a home dir per process):

| direction | result |
|---|---|
| Rust writes 3 docs (non-monotonic `_id`s) → Python `Storage` opens it | reads all 3 **in insertion order**, `_id` point-lookup works |
| Python appends to that store → Rust reopens | Rust sees all 4 and finds the Python-written doc by `_id` |
| Python writes a fresh store (no secondary index) → Rust opens | reads all docs, filtered lookup works |
| Python writes a fresh store **with a secondary index** → Rust opens | **refused**: `index 'x_1' … is entryFormat 1, but this build requires 2` |

That last row is **expected and is exactly what 4b closes** — Python still writes
step-1-format index entries (id_key in the trailing half, no `entryFormat`
marker). The doc-table half of the format is complete and portable both ways; the
index-entry half is 4b's job.

- Fixed as a side effect: `update_matching(multi=False)` / `delete_matching` now
  visit candidates in insertion order, so they pick the oldest match like mongod
  (backlog divergence deleted; `tests/test_indexes.py::test_update_multi_false_
  updates_natural_first_match` rewritten to assert the mongod behaviour — it fails
  on pre-change code).

**Python functions in play:** `_write_nat_entry` (1479), `_delete_nat_entry`
(1489), `_scan_docs_natural` (1505), `_scan_docs` (3403), `_mint_nat_seq` (1473),
`_scan_max_nat_seq` (1456), `insert` (3483), `update_matching` (4403),
`delete_matching` (4637), `find_by_id`, the bootstrap (1200–1216), and every
`set_key(db, coll, id_key)` on a doc-table cursor (~9 sites).

**Steps (each mirrors the Rust step-1 commit — read `b90b5490`'s diff of
`crates/secantus-storage/src/lib.rs` alongside):**
1. `_frame_doc_value(id_key, blob)` / `_unframe_doc_value(value) -> (id_key, blob)`
   — `[u32-LE len][id_key][blob]`, byte-identical to Rust `frame_doc_value`.
2. Doc-table `key_format` `SSu` → `SSq` at bootstrap (1201, 1205). WT fixes
   key_format at create, so this alone makes an old store's shards mismatch — which
   is what the fail-fast check keys on.
3. `_write_nat_entry` → mint RecordId, write ONLY the reverse `_id` index
   (`_NAT_SEQ_TABLE`: id_key→RecordId) with `overwrite=false`, **drop the forward
   `_NAT_TABLE` write**, return the RecordId. Move **dup-`_id` detection** here (the
   doc table now keys by unique RecordId so it can't reject dups) → raise the
   duplicate-key error the insert path expects.
4. `insert` / `_insert_one`: `_write_nat_entry` FIRST (mint + `_id` index, catch
   dup), then doc-table insert keyed by RecordId with the framed value.
5. `_scan_docs` walks the doc table directly (`SSq` = RecordId order) and yields
   `(RecordId, id_key, blob)` via `_unframe_doc_value` — no `_id` decode, timeseries
   suffix carried in the frame. `_scan_docs_natural` delegates to it. **DELETE the
   forward-`_NAT_TABLE` scan** (the current `_scan_docs_natural` body).
6. Every doc-table READ/DELETE resolves `id_key → _doc_recordid → RecordId`: add
   `_doc_recordid(db, coll, id_key)` (reverse `_id`-index lookup), and route
   `find_by_id`, `delete_matching`, `update_matching`, capped eviction, TTL prune
   through it. Keep "doc row first, entries after" ordering.
7. `_scan_max_nat_seq` scans the doc **shards** for the max RecordId (the forward
   NAT is gone) so `_mint_nat_seq` recovers on reopen.
8. **Fail-fast** `_reject_pre_recordid_doc_format` (mirror the Rust fn): read each
   doc shard's on-disk `key_format` from the WT `metadata:` cursor; raise a fatal
   error at `Storage.__init__`/open if any is `SSu`. Python `wt` metadata cursor:
   `session.open_cursor("metadata:")`, key = table name (str), value = config str.

**Tests:** existing `tests/test_storage.py` + `tests/test_crud.py` are the parity
oracle (they run against the Python server). Add a `_frame_doc_value` byte test and
a "refuse an SSu store" test (fabricate an SSu doc shard, assert open raises).
`tests/test_natural_order.py` (Python) must still pass — insertion-order `find()`.

## 4b — index entries carry the RecordId (mirror of Rust #637 `4af58aae`) — **DONE**

Implemented. Deltas from the sketch below:

- Naming follows the Rust side: the picker helpers keep their `*_id_keys` names
  while carrying RecordIds (`Vec<i64>` there, `list[int]` here); only
  `_docs_by_id_keys` → **`_docs_by_recordids`** was renamed. Type annotations were
  updated everywhere so the change is visible at each signature.
- `_write_index_entries` / `_delete_index_entries` now take `recordid=` and have
  **no** `id_key_override`: the timeseries special case that parameter existed for
  disappears with the id_key.
- The `_id` point-lookup fast path in `_try_index_id_keys` resolves its id_keys
  through the `_id` index so every picker path returns RecordIds uniformly (same
  as Rust — for an `_id` lookup that index IS the access path, not the removed hop).
- `_scan_geo_range` was hand-splitting the packed entry; it now goes through
  `_unpack_entry` and dedups by RecordId.
- Every entry reader skips a `None` RecordId (a pre-change entry) rather than
  appending it — belt and braces behind the open-time refusal.
- `entryFormat` is stripped from `listIndexes` alongside `multikey`
  (`commands.py`, one shared `_INTERNAL_INDEX_FIELDS` tuple).
- **`rename_collection` needed no change** (unlike Rust #637): Python copies the
  doc rows and `_NAT_SEQ_TABLE` rows verbatim, so RecordIds are preserved and the
  copied entries stay valid. Guarded by a new rename-keeps-index-reachable test,
  proven real by mutation — re-minting the destination's RecordIds makes the
  hinted lookup return `[]` while `$natural` still returns everything.

**Gates run for 4b:** full Python suite **4857 passed / 0 failed**; `invoke lint`
clean; pymongo gauge **5 failed / 1226 passed**, the same five known out-of-scope
failures as `main`.

**Cross-server byte-parity now covers indexes too** (same method as 4a):

| direction | result |
|---|---|
| Python writes docs + a secondary index → Rust opens | reads the collection **and** serves the indexed lookup |
| Rust writes docs + builds the index → Python opens | `explain` says `IXSCAN x_1`, returns the same rows Rust did |
| Python then inserts through the Rust-built index → Rust reopens | Rust's indexed lookup sees the Python-inserted doc |

The 4a refusal (`entryFormat 1`) is gone — both halves of the on-disk format,
documents and index entries, are now interchangeable between the two servers.

### Original sketch

**Python functions:** `_pack_entry` (253), `_unpack_entry` (263),
`_write_index_entries` (5552), `_delete_index_entries` (5588), `create_index`
(5079), the IXSCAN fetch (`_docs_by_id_keys` / candidate paths), uniqueness probes,
and the `listIndexes` strip in `commands.py` (find where `multikey` is removed).

**Steps (read `4af58aae`'s diff):**
1. `_pack_entry(kb, recordid: int)` = `escape(kb) + b"\x00\x00" + recordid.to_bytes(8, "big")`;
   `_unpack_entry(packed) -> (esc_kb, recordid|None)` (None if the trailing half
   isn't 8 bytes = a step-1 entry — never silently mis-read).
2. Thread the RecordId through `_write_index_entries` / `_delete_index_entries` /
   the diff path (they take the RecordId; `_id` is immutable so an update keeps it)
   and `create_index`'s backfill (the doc-table scan already yields RecordIds).
3. IXSCAN fetch reads the doc row directly by RecordId — **drop the `id_key → _id
   index → RecordId` hop** (`_docs_by_id_keys` → resolve nothing, use the entry's
   RecordId).
4. `options["entryFormat"] = 2` in `create_index`; `_reject_legacy_index_entry_format`
   at open (scan `_IDX_TABLE`, refuse if any index lacks `entryFormat >= 2`); strip
   `entryFormat` from `listIndexes` where `multikey` is already stripped.
5. **`rename_collection` re-mints RecordIds → REBUILD index entries, do not copy**
   the packed rows (the bug #637 caught in Rust — copied entries would point at
   destination-nonexistent RecordIds and silently break every index).
6. Uniqueness: exclude self by RecordId, not id_key.

**Tests:** `tests/test_indexes.py` is the oracle. Add the "refuse a step-1 entry
store" test and a rename-keeps-index-reachable test (mirror the Rust
`rename_keeps_secondary_index_reachable_through_the_index`, hint-forced IXSCAN vs
collection scan — and verify it FAILS on a verbatim-copy rename before trusting it).

## 4c — tailable capped cursor tracks RecordId (mirror of Rust #640 `6f3a8e05`) — **DONE**

Implemented. Deltas from the sketch below:

- New storage methods: `scan_docs_after_recordid`, `collection_min_recordid`,
  `collection_max_recordid`, plus `recordid_for_id` (public `_id`-index accessor).
  The old `scan_docs_after_id_key` / `collection_min_id_key` are **deleted** —
  `commands._find_tailable` was their only caller, so nothing keeps them alive
  (the Rust side kept its versions because `stats.rs` still used them).
- **The watermark is seeded from the last doc handed out, not the collection's
  max RecordId** (where Rust seeds it). Python's producer must not skip matched
  docs past `batch_size` — the go-gauge bug the existing comment records — so it
  anchors on `initial_docs[-1]`'s RecordId via `recordid_for_id`. Same ordering
  basis as Rust, different (safer) seed.
- The pre-change failure is worse than the Rust commit message describes: with
  `_id`s `500, 400, 300` then `20, 10`, the id_key watermark returned
  `[500, 400, 300, 400, 500]` — it both dropped the new docs AND redelivered old
  ones. The new test pins `[500, 400, 300, 20, 10]` and fails on pre-change code
  exactly that way.

### Original sketch


**Only after 4a/4b** — until the Python doc table is RecordId-keyed, its natural
order IS id_key order and the current id_key tailable is correct.

**Python functions:** `_find_tailable` (`commands.py:2211`), which calls
`storage.collection_min_id_key` (2267) + `storage.scan_docs_after_id_key` (2270);
`scan_docs_after_id_key` (`storage.py:3433`), `collection_min_id_key` (3454).

**Steps (read `6f3a8e05`'s diff):** add `_scan_docs_after_recordid(db, coll, after:int|None)`,
`_collection_min_recordid`, `_collection_max_recordid` (all ride `_scan_docs`, which
is RecordId-ascending after 4a); switch `_find_tailable`'s watermark to a RecordId
(seed from `_collection_max_recordid` at setup, rollover via `_collection_min_recordid`).
Keep `scan_docs_after_id_key` — non-tailable callers still use it.

**Test:** mirror the Rust end-to-end
`test_tailable_capped_follows_inserts_with_nonmonotonic_ids` but against the **Python**
server (a capped collection with descending/non-monotonic `_id`s + tailable cursor,
assert follow-up smaller-`_id` inserts still arrive in insertion order).

---

## Status (2026-07-25): 4a, 4b and 4c are all implemented on `rust-recordid-step4`

Gates run per step, all green: full Python suite (4863 passed / 0 failed at 4c),
`invoke lint` (ruff check + format), and the pymongo gauge at **5 failed / 1226
passed** — the same five known out-of-scope failures `main` publishes, at every
step. Cross-server byte parity was measured directly against the shipped
`_secantus_storage` extension (tables in the 4a / 4b sections): a store written by
either server now opens, reads, index-scans and accepts writes on the other.

Every new test was proven to fail on pre-change code (the guard-can-catch-its-bug
rule): the four 4a guards by stashing `storage.py`, the 4b rename guard by
mutation (re-minting the destination's RecordIds → index returns `[]` while the
scan returns everything), and the 4c tailable guard by stashing both modules
(pre-change output was `[500, 400, 300, 400, 500]` — dropped inserts AND
redelivered docs).

Not done here: committing / opening the three sub-PRs, and re-running the *Rust*
server's own pymongo gauge (`./inv validate --server rust`), which needs the
WT-linked extension built in this worktree. The extension-level cross-server
checks above cover the same on-disk-format question.

## Measured — Python server, 4a+4b+4c vs pre-change (2026-07-26)

Alternating A/B on an idle box (load < 2.5): the working tree (all three steps)
against a **detached worktree frozen at `01152b1d`** (the pre-change tree), same
venv, same machine, 6 interleaved rounds × 3 reps each, `n=10000`, driven through
`pymongo` by `bench.compare_servers`' workloads (Python server only). Medians:

| workload | pre | post | Δ |
|---|---|---|---|
| insert | 347.95 ms | **295.03 ms** | **−15.2%** |
| find_all (scan) | 108.57 ms | **70.37 ms** | **−35.2%** |
| aggregate `$group` | 157.12 ms | **119.52 ms** | **−23.9%** |
| aggregate multistage | 212.79 ms | **175.96 ms** | **−17.3%** |
| delete_many_half | 273.98 ms | 266.29 ms | −2.8% |
| find_indexed_range | 34.77 ms | 36.28 ms | **+4.3%** |
| update_many_half | 506.34 ms | 524.94 ms | **+3.7%** |

Noise floor ≈ ±3% (max/min across each leg's 6 round medians was 1.01–1.04×), so
the four big wins are unambiguous and the two small regressions sit just at the
edge — but they reproduced in both independent 3-round batches, so treat them as
real.

**Why insert / scan / aggregate win:** insert dropped a whole row (4→3), and every
sequential walk now reads the doc table directly instead of walking the natural
index and fetching each doc by `id_key` — that second hop is what the 35% scan win
is made of.

**Why the two small regressions:** before this change an indexed read was already
direct (entries held the `id_key`, the doc table was `id_key`-keyed), so 4b's
"drop the hop" only gives back what 4a spent — it doesn't beat the original. What's
left is new per-row work: `_unframe_doc_value` on every doc read and the
`_unpack_entry` int parse on every entry. Same shape as the Rust side, where step 1
cost +14.7% on `find_indexed_range` and step 2 aimed to return it to baseline.
Net across the workload set the change is strongly positive.

## Resume instructions (cold-start)

- Branch `rust-recordid-step4` (worktree `../SecantusDB-recordid4`). Base off current
  `main` (steps 1–3 landed). One sub-PR per 4a/4b/4c off `main`.
- Python-only change — **no Rust build needed** for 4a/4b/4c themselves. Run
  `invoke test` / `invoke lint` directly. (A worktree may lack the WT-linked venv;
  `invoke lint` works without a WT build — see memory `inv-in-worktrees`.)
- The Rust code on `main` is the byte spec: `git show b90b5490 -- crates/secantus-storage/src/lib.rs`
  (4a), `4af58aae` (4b), `6f3a8e05` (4c).
- Gauge per sub-PR: `./inv validate` (Python server) must stay non-regressing, AND
  `./inv validate --server rust` to prove the Rust reader still opens/reads a
  Python-written store byte-identically (build the ext with `./inv rust-server-build`
  first; fresh worktree needs `git submodule update --init --depth 1 vendor/wiredtiger`
  and, for the gauge, `git submodule update --init --recursive vendor/pymongo-tests`
  BEFORE `./inv validate` or it recursively clones every driver — see the ci-check
  catalog).
- Don't bump versions in the sub-PRs (see CLAUDE.md); add a `changelog.d/` fragment.

## Verification bar per sub-PR
1. Full Python suite green (`invoke test`).
2. `invoke lint` (ruff check + format).
3. pymongo gauge non-regressing on BOTH servers (the known-5 out-of-scope failures,
   no new).
4. A regression test proven to FAIL on the pre-change behaviour (the session's hard
   rule — a guard that can't catch its bug is theatre).
5. CI green on the PR before merge.
