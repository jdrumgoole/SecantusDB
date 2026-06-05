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

---

## Phase 1 — `update.apply_update` ported (common operators): **DONE**

Third leaf engine. `crates/secantus-core/src/update.rs` ports the deterministic,
high-value operators behind the byte seam (doc + update cross as BSON bytes,
result returned as bytes), reproducing `secantus.paths`' dotted-path
create/set/unset/get semantics (incl. array-index growth and the
list-growth cap). Same graceful-fallback design: `secantus.update.apply_update`
delegates when `SECANTUS_RUST_UPDATE=1` and uses the Rust result unless it's
`None`.

Handled in Rust: replacement-style updates (with `_id` preservation), `$set`,
`$setOnInsert` (upsert-gated), `$unset`, `$inc`, `$mul`, `$push`, `$pop`,
`$rename`, and `_id` immutability. Deferred to Python: pipeline (array) updates,
positional operators (`$`/`$[]`/`$[id]`) + array filters, `$currentDate`
(non-deterministic), `$min`/`$max`/`$pull`/`$addToSet`/`$bit` (Python
comparison/`==` semantics), Decimal128/non-numeric arithmetic, and **every error
condition** — Rust returns `None` so the pure-Python path raises the exact
`UpdateError`/`PathError`.

The arithmetic was the careful part: `$inc`/`$mul` reproduce Python's int-subclass
bool handling (True→1), int/float promotion, and — crucially — the result's BSON
width is chosen by *magnitude* (int32 if it fits, else int64), matching how
pymongo re-encodes Python's plain-`int` results. Validation: `cargo test` green;
`tests/test_rust_update_parity.py` — curated corpus mirroring `test_update.py` +
a 6000-case fuzz — green (no Python-side changes needed this time).

One flip-blocker noted in `tasks/backlog.md` §7: confirm `bson::Document::insert`
preserves field position on an existing key (Python `set_path` does), so a
future Rust default doesn't reorder fields vs mongod. The parity test uses
order-insensitive dict `==`, so it wouldn't catch a reorder — the WiredTiger
conformance suite would.

Next leaf engine: `expressions.evaluate` — the aggregation expression language,
which would also unlock `$expr` in the matcher and pipeline updates.

---

## Phase 1 — `expressions.evaluate` core ported: **DONE**

Fourth and largest leaf (~80 operators in Python). `crates/secantus-core/src/
expressions.rs` ports a **coherent high-value core** behind the byte seam (expr
+ doc + vars cross as BSON bytes, result wrapped as `{"r": ...}`): field paths /
`$$var` / `$$ROOT` / `$literal`; comparison; logic; control flow (`$cond` /
`$ifNull` / `$switch`); arithmetic; and common array ops. Because expressions
are recursive, the evaluator returns `Fallback` (whole-call) the moment it hits
any operator/value it doesn't yet handle — strings, dates, regex, conversions,
`$map`/`$filter`/`$reduce`/`$let`, object ops — so the entire call defers to
Python. `secantus.expressions.evaluate` delegates when `SECANTUS_RUST_EXPR=1`.

This also **unlocked `$expr` in the Rust query matcher**: the matcher now calls
the Rust evaluator directly (Rust->Rust, no re-encode), so `query.matches`
delegated `$expr` clauses no longer fall back when the expression is in the
supported core. `query_matches` gained a `vars` argument threaded through.

Shared-code cleanup landed alongside: dotted-path *read* helpers moved to a new
`paths.rs` and numeric conversion helpers (`as_int_like`/`as_float_like`/
`int_to_bson`) to `numeric.rs`, both now shared by the update and expression
engines instead of duplicated.

The careful parts were Python's two different comparison semantics — `$eq`/`$ne`
use total `==` (where `null == null` is true and unlike types are simply
unequal), while `$gt`/`$lt`/… use `<`/`>` (which raise on incomparable operands,
incl. `null` vs `null`, caught as false) — and `$add`'s identity quirks (empty
`$add` raises; single-element `$add` returns the value unchanged, preserving
bool/string). The 8000-case nested fuzz caught the null-ordering bug before it
shipped. Validation: `cargo test` (22) + `tests/test_rust_expressions_parity.py`
(curated + fuzz) green; all four leaf-engine parity suites total 264 cases.

Next leaf engines: `projection` and `diff` (smaller); or widen the expression
evaluator's operator coverage (strings/dates/conversions) to shrink its
fallback surface.

---

## Phase 1 — `projection` and `diff` ported: **DONE** (all six leaf engines)

Fifth and sixth leaf engines, finishing the Phase-1 leaf set.

`projection.apply_projection` (`projection.rs`): inclusion / exclusion / `$slice`
/ `$elemMatch` shapes behind the byte seam (doc + spec cross as BSON bytes).
Defers to Python for mixed inclusion/exclusion (Python raises), nested-document
specs, unusual `$slice` argument types, and `$elemMatch` sub-filters the matcher
defers (it reuses the Rust query matcher). `SECANTUS_RUST_PROJECTION=1`.

`diff.compute_update_description` (`diff.rs`): the change-stream `$v: 2` update
diff — `{updatedFields, removedFields, truncatedArrays}` for a pre->post image —
reusing the expression engine's Python-`==` value equality (now `pub`), so the
numeric/bool bridging that decides "did this leaf change?" matches Python
exactly. Defers on Decimal128 / exotic values. `SECANTUS_RUST_DIFF=1`.

A shared-code cleanup landed too: the dotted-path **write** helpers
(`set_path`/`unset_path`) moved into `paths.rs` next to the read helpers, shared
by the update and projection engines (`update.rs` keeps a thin error-mapping
wrapper).

Validation: `cargo test` (32) + both new parity suites (curated + 6000-case
fuzz each) green; all **six** leaf-engine parity suites total **298 cases**.

All six pure leaf engines are now ported, opt-in, and parity-pinned:
`sortkey`, `query.matches`, `update.apply_update`, `expressions.evaluate`,
`projection.apply_projection`, `diff.compute_update_description`. Remaining
Phase-1 work: `collation` (to retire the collation fallbacks), and widening the
already-ported engines' operator coverage. Then Phase 2+ (aggregate, storage,
wire/dispatch) and the eventual default-flip + packaging (Phase 6).

---

## Engine selection — both engines are permanent

**Decision: SecantusDB keeps both the pure-Python engines and the Rust core as
first-class, permanently-supported implementations.** The Rust rewrite is an
*accelerator*, not a replacement — Python is never removed.

`secantus.engine` is the single source of truth: `available()` (is the Rust
extension importable), `selected()` (`python`/`rust`/`auto`), `set_engine()`,
and `enabled(component)`. All six shims call `engine.enabled(<component>)`
instead of reading their own env var. Selection is process-wide:

- `SECANTUS_ENGINE=python` (default) / `rust` / `auto`
- `SecantusDBServer(engine="rust")` and `secantusdb --engine rust`
- per-component overrides `SECANTUS_RUST_<COMPONENT>=1/0` (debugging) win

`python` is the default; `rust` transparently falls back to Python for any
component not ported (and warns once if the extension is missing). Unit-tested
WT-independently by `tests/test_engine.py`.
