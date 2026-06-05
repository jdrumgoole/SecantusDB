# Phase 0 spike findings — Python → Rust rewrite

Companion to `tasks/rust-rewrite-plan.md`. These are the results of the three
de-risking spikes that §6 Phase 0 calls go/no-go gates. **All three passed.**
Spike code lives in `rust/` (a throwaway cargo workspace) with driver harnesses
in `rust/harness/`. Reproduce with `rust/run_spikes.sh`.

Environment: Rust 1.94, `bson` crate 2.15.0, vendored WiredTiger
mongodb-7.0.33, clang 18 / gcc 13, cmake 3.28 + ninja, Python 3.12 + pymongo.

---

## Spike 1 — BSON byte-fidelity (pymongo ⇄ `bson` crate): **PASS**

Round-tripped a 25-document corpus spanning every type the "pymongo can't tell
us apart" thesis depends on — ObjectId, Decimal128 (incl. `Infinity`/`NaN`/
`1.00`), int32-vs-int64 boundaries, tz-aware/naive/epoch dates, Binary subtypes
0/3/4, Regex, Timestamp, MinKey/MaxKey, Code, deep nesting, mixed arrays,
insertion-order key preservation, and `$`/dotted-looking keys — through the
Rust `bson` crate (both the zero-copy `RawDocument` path and the typed
`Document` path). **Every document re-encoded to bytes identical to pymongo's
output.**

Implication: the Rust `bson` crate is a faithful drop-in for the byte seam
(`tasks/rust-rewrite-plan.md` §3). The "opaque BSON blob" design carries over
to Rust without a custom encoder. **Plan decision §10.2/§3 de-risked.**

Not yet covered (low risk, worth a later pass): genuinely duplicate keys (a
Python `dict` can't express them, so the corpus can't either — would need
hand-rolled raw bytes), and adversarial/malformed BSON (that's the wire layer's
bounds-checking job, Phase 5).

## Spike 2 — WiredTiger FFI: **PASS**

Built the vendored WiredTiger as a **static lib with `ENABLE_PYTHON=OFF`**
(`libwiredtiger.a`, 4.4 MB), `bindgen`-generated FFI from the CMake-generated
`wiredtiger.h`, and ran a smoke that opens a connection on a temp dir, opens a
session, creates a table, inserts rows via a cursor, does a point `search`, and
range-scans in order — all green.

Two findings that matter beyond "it works":

1. **No SWIG needed.** The production *Python* wheel needs SWIG only to generate
   the Python binding; FFI links the C library directly, so the Rust path drops
   the SWIG build-time dependency entirely. That removes a documented source of
   cross-platform wheel pain (`CLAUDE.md` Tooling notes).
2. **The static-core build is small and dependency-light** — links only
   `pthread`/`dl`/`m` (+ optional zlib if present); the lz4/snappy/zstd/sodium
   compressors were absent and simply not built, no failure.

Implication: option A in §4.1 (FFI into the vendored C, keep the engine) is
viable on this toolchain. **Plan decision §10.2 leans firmly toward option A;
the Rust-native-KV escape hatch (option B) is not forced.**

Caveats / scope for the real port: this only exercised the `S`/`S` (string)
schema and a single session. The production layer needs the `SSu`/`SSSu`
packed-`u` schemas, thread-affine sessions, the global lock discipline,
checkpoints, and `in_memory=true`. None of those are new *FFI* risks — they're
WT-config/usage the Python layer already proves — but they're the next thing to
exercise before committing Phase 4. Cross-platform (macOS arm64, musllinux,
Windows) FFI builds are a packaging task for Phase 6, untested here.

## Spike 3 — `secantus.sortkey` byte-exact reproduction: **PASS**

Generated 35 golden `(value, key_bytes)` vectors from the production
`encode_value` and reproduced them byte-for-byte with an independent Rust port
of the encoder — including the parts most likely to drift:

- the **"lexical decimal" numeric form** (sign byte + biased exponent + paired
  BCD + terminator), and
- **cross-type numeric collision**: int `3` ≡ double `3.0` ≡ `Decimal128("3")`
  produced identical key bytes, as did `Decimal128("1.00")` ≡ int `1` and
  `Decimal128("123.45")` ≡ double `123.45` — the exact property the unified
  numeric index ordering depends on.

Also matched: null/min/max/bool ranks, ObjectId, signed-int64 date encoding
(incl. pre-epoch negatives), Timestamp, escaped strings/binary.

Implication: the on-disk sort-key format is faithfully reproducible in Rust.
This means we can **keep the WT tables byte-compatible if we choose to**, which
softens §10.3 — a format-version bump is no longer *required* by sortkey risk
(it may still be chosen for other reasons). **Plan decision §10.3 de-risked.**

Scope for the real port: documents/arrays-as-keys (route through `bson::encode`
+ escape — covered transitively by Spike 1), collation-normalised string keys
(`collation.normalize_for_index_bytes`), descending `invert_bytes`, and the
single-byte-exponent overflow fallback path were not in the golden set. Add
them to the golden corpus when porting `sortkey` for real in Phase 1.

---

## Net verdict

All three Phase 0 gates are green. The two genuine hard problems from the plan
(WiredTiger-from-Rust, BSON/sortkey byte-fidelity) are **de-risked on this
platform**, and the FFI path even buys a packaging simplification (no SWIG).
Nothing surfaced that argues against the PyO3 strangler-fig plan; the
recommended answers to §10.1–§10.3 (PyO3 extension, WT-FFI/option A, and now
*optionally* byte-compatible on-disk format) all hold.

Recommended next step: a small **Phase 1 starter** — stand up the real
`crates/secantus-core` with maturin, and port the first leaf engine
(`sortkey`, then `query.matches`) behind the byte seam with the existing
`tests/test_query.py` / `test_indexes.py` as the net.

---

## Phase 1 starter — `sortkey` ported: **DONE**

Stood up `crates/secantus-core` (PyO3 + maturin, abi3 wheel valid for CPython
3.10+) and ported `sortkey` as the first leaf engine, behind the fat byte seam:
values cross as `bson.encode({"v": value})` and the Rust side returns the key
bytes. `secantus.sortkey` is now a shim that delegates to `_secantus_core` when
`SECANTUS_RUST_SORTKEY=1` and is pure-Python otherwise (and always when a
collation is supplied — collation-aware encoding isn't ported yet).

Validation (runs without WiredTiger — `invoke rust-test` + `invoke rust-parity`):
`cargo test` green; `tests/test_rust_sortkey_parity.py` green — a curated corpus
plus a **2000-case randomised fuzz**, all byte-identical between the Rust port
and the authoritative pure-Python encoder.

**The port surfaced two latent bugs in the pure-Python `sortkey`** (the Rust
port matched mongod; the Python encoder didn't), now fixed so all three agree:

1. *Date keys* were computed via a float `total_seconds() * 1000`, rounding
   sub-second values off by up to 1ms vs the integer millis BSON stores → now
   integer-exact.
2. *Regex keys* did `bytes(r.flags)`, which on a BSON-round-tripped regex (where
   `flags` is an int) emitted N NUL bytes instead of the option string → now
   reconstructs option chars in pymongo's on-wire order (`ilmsux`).

Caveat worth flagging to a maintainer: both fixes change the on-disk
index-key bytes for the affected values. That's immaterial for SecantusDB's
ephemeral test data, but the full WiredTiger-backed suite (`test_indexes.py`,
`test_sort_*`) should be run in CI to confirm no ordering regression — it
couldn't be run in the spike environment (no SWIG → the WT Python extension
doesn't build there). The changes only make ordering *more* faithful to mongod,
so no regression is expected.

Next: port `query.matches` behind the same seam, then decide on flipping the
`sortkey` default to Rust (gated on Phase 6 packaging — merging the maturin and
scikit-build wheels — and the per-call re-encode overhead question).

---

## Phase 1 — `query.matches` ported (common operators): **DONE**

Second leaf engine. `crates/secantus-core/src/query.rs` (+ `numeric.rs` for the
int32/int64/double/Decimal128 bridge) ports the field- and document-level
matchers behind the byte seam (doc + query cross as BSON bytes). The key design
choice is **graceful fallback**: the Rust matcher returns `None` for anything it
can't reproduce byte-for-byte, and `secantus.query.matches` (when
`SECANTUS_RUST_QUERY=1`, no collation) uses that to defer to the pure-Python
matcher. So the port is always correct — the operators it *does* handle match
Python exactly, and everything else runs the existing code.

Handled in Rust: `$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`, `$in`/`$nin`, `$exists`,
`$not`, `$type`, `$size`, `$elemMatch`, `$mod`, `$bits*`, `$and`/`$or`/`$nor`,
`$comment`, bare-value equality with dotted-path + array fan-out, and the
numeric cross-type bridge / bool-distinctness. Deferred to Python (fallback):
collation, `$expr`, `$jsonSchema`, geo, **any regex** (Python `re` semantics),
`$all`, structural/compound (array/doc) equality, bool-as-int comparison, and
exotic BSON types.

Validation: `cargo test` green; `tests/test_rust_query_parity.py` — a curated
corpus mirroring `tests/test_query.py` plus a **6000-case randomised fuzz** —
green, with the fuzz asserting the Rust matcher actually handled >1000 cases
(not just falling back). Two faithful-semantics bugs were caught and fixed
during the port (both in `$mod`): Python computes `bool % div` (bool is an int
subclass) so a bool value participates, and the remainder is compared literally
(`-26 % 2 == 2` is `0 == 2` → False, not normalised). No changes to the Python
side were needed this time — the Rust port matched the existing pure-Python
behaviour once those two were right.

Next leaf engine: `update.apply_update` (or `expressions`, which would unlock
`$expr` in the matcher and remove that fallback).
