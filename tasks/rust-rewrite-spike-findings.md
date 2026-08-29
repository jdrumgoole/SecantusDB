# Phase 0 spike findings — Python → Rust rewrite

> **HISTORICAL — superseded (audited 2026-08-20).** This plan is built on the
> *in-process selectable-engine* model (`SECANTUS_ENGINE=python|rust|auto`, the
> `secantus.engine` shims, the `EngineFallback` adapter). That model was retired
> in favour of **two separate servers**, and CLAUDE.md names
> `tasks/rust-server-plan.md` as the authoritative plan. Kept for the design
> reasoning and the measurements; do not take its next-steps as current work.

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

---

## Phase 2 — aggregation pipeline (first slice)

With all six leaf engines ported, the pipeline is the first composite engine.
`crates/secantus-core/src/aggregate.rs` ports `apply_pipeline` behind a
**list-of-docs byte seam** (`{"d": [docs]}` in, `{"d": [docs]}` out; the
pipeline arrives as `{"p": [stages]}`) and drives the already-ported leaf
engines directly in Rust — `query::matches` for `$match`, `expressions::evaluate`
for computed fields, the `paths` write helpers for `$set`/`$unset`/`$project`.
A pure pipeline therefore runs end to end in Rust without re-entering Python per
stage or per document.

Stages handled this slice: `$match`, `$limit`, `$skip`, `$count`, `$project`
(inclusion / exclusion / computed — mirroring `_project_one`, including the
mapping-only `_path_present` that does *not* walk into arrays),
`$addFields`/`$set`, `$unset`, `$replaceRoot`/`$replaceWith`.

**Graceful whole-pipeline fallback** keeps it strictly correct: any unported
stage (`$sort`/`$group`/`$unwind`/`$facet`/storage-backed/`$sample`/…) or any
deferred inner expression makes `apply_pipeline` return `Fallback` → `None` at
the seam, and the authoritative pure-Python pipeline runs instead.
`secantus.aggregate.apply_pipeline` delegates when `SECANTUS_RUST_AGGREGATE=1`.

The comparison/equality-heavy stages are the next widening target and each
needs faithful reproduction of a Python quirk: `$sort` the `_bson_lt` cross-type
ordering, `$group`/`$sortByCount`/`$bucket` the group-key collision where
`1 == 1.0 == True` hash together (plus the accumulators), `$unwind` the
missing/non-array/empty-array edge cases. Storage-backed stages
(`$lookup`/`$geoNear`/`$out`/`$merge`) and non-deterministic `$sample` wait for
the storage layer to move into Rust (Phase 3+).

Validation: `cargo test` (45) + `tests/test_rust_aggregate_parity.py` (curated +
4000-case pipeline fuzz) green; the full Rust parity sweep is **484 cases**.

A `$dateDiff` fidelity fix rode along: the sub-day units (`hour`/`minute`/
`second`/`millisecond`) route through Python's lossy
`timedelta.total_seconds()` = `total_microseconds / 10**6` (an int/int
*correctly-rounded* true division), then `// n` (floor) or `int(...)` (trunc).
Rust reproduces the single-rounding float path, guarded to `|total_us| <= 2**53`
so the `as f64` conversion is exact (matching CPython's single rounding) and
deferring extreme dates to Python where double-rounding could diverge.

### Pipeline widening: `$sort` + `$unwind`

`$sort` is the first stage needing a cross-type value comparator.
`crates/secantus-core/src/order.rs` ports `_bson_lt` / `_bson_type_rank` as a
total `cmp(a, b) -> Ordering` (type ranks, embedded-doc / array recursion, the
unified numeric type with NaN treated as equal-not-less to match Python's `<`
returning False both ways). Strict-fidelity gate: the stage runs
`order::is_sortable` over every sort-key value first and defers the whole
pipeline to Python on Decimal128 (Python's Decimal-widening branch) or exotic /
uncomparable types (Python's `TypeError` → type-name fallback), so `cmp` itself
never has to represent "can't compare". Stable, single + multi-field, both
directions. `$unwind` ported alongside (string + doc spec, `includeArrayIndex`,
`preserveNullAndEmptyArrays`, and the missing / null / non-array / empty-array
edges).

A layering cleanup rode along: the pure sort comparator
(`sort_docs`/`_bson_lt`/`_bson_type_rank`/`_SortKey`/`_to_decimal`) moved out of
`storage.py` — where it sat next to the WiredTiger code and so was unimportable
without the `wiredtiger` extension — into a new I/O-free `secantus.ordering`
module. `storage` re-exports the names (back-compat for the many existing
`from secantus.storage import sort_docs` call sites and the monkeypatch tests),
and the parity harness can now load `ordering` by path without WT. This is the
same pure-operator-engine layering as `query` / `update` / `expressions`.

Validation: `cargo test` (51) + `tests/test_rust_aggregate_parity.py` (mixed-type
sort corpus + 4000-case fuzz over arrays / mixed-type fields) green; full Rust
parity sweep is **491 cases**.

### Pipeline widening: `$group` + `$sortByCount`

The hardest pipeline stage. `crates/secantus-core/src/group.rs` reproduces two
delicate Python behaviours:

* **Group-key bucketing = Python dict semantics.** `_stage_group` buckets on
  `_hashable(evaluate(_id))` in a plain dict, so `1 == 1.0 == True == Decimal128("1")`
  collapse into one bucket and embedded docs/arrays recurse (key-sorted tuples).
  We canonicalise each key into a hashable `GKey` — numbers + bool normalised
  through `numeric::NumVal` (which gained `Eq + Hash`), so the cross-type
  collision is exact — preserving first-seen `_id` and group insertion order.
  Key types we can't canonicalise without a fidelity risk (Decimal128, NaN,
  Binary/Timestamp/Regex/Min/MaxKey, exotic) defer the whole stage.

* **Accumulators reproduce Python's numeric + raise-on-mixed-type semantics.**
  `$sum`/`$avg` accumulate through a `Num{Int(i128),Float(f64)}` enum mirroring
  Python `+` (int stays int; any float widens; `$avg` is always a double and the
  field stays *absent* when no non-null value arrives — the pure code never
  creates the bucket key; non-numeric operands `TypeError` -> defer). `$min`/`$max`
  reuse `expressions::py_order` (Python native `<`/`>`), so cross-type pairs that
  Python would raise on defer rather than get guessed, and null is a no-op that
  never "unsets". `$addToSet` membership uses `expressions::py_eq` (Python `==`,
  incl. bool-as-int + structural). `py_order`/`py_eq` are now `pub`.

`$sortByCount` = `$group` with `{$sum: 1}` then a stable count-descending sort
(`list.sort(reverse=True)` keeps insertion order on ties).

The `$sort` gate was tightened in the same pass: `sort_docs` wraps keys in
`_SortKey` whose `__eq__` is Python `==` but whose `__lt__` is rank-based
`_bson_lt`, and a tuple sort only advances to the next field when `==` is True —
so the comparator is non-transitive whenever bool/NaN (or Binary-subtype /
Timestamp / Regex / Min/MaxKey) mix with other values, and Rust's stable sort
can't be guaranteed to match Python's Timsort. `order::is_sortable` now only
green-lights the types where Python `==` agrees with `cmp == Equal` and defers
the rest. (A fuzz seed shift surfaced this as a real `False`-vs-`0` divergence.)

Validation: `cargo test` (57) + `tests/test_rust_aggregate_parity.py` (curated +
5-seed fuzz) green, plus 8 extra local seeds with ~1,650-1,730 group/sortByCount
pipelines handled each and zero mismatches; full Rust parity sweep **501 cases**.

### Pipeline widening: `$bucket` + `$facet`

`$bucket` (`group::bucket_stage`) reuses the `$group` accumulator machinery: it
places each doc into the half-open range `boundaries[i] <= value <
boundaries[i+1]` via `expressions::py_order` (Python's native `<=`/`<`, so
cross-type / Decimal128 / array-doc boundaries defer rather than guess; NaN and
TypeError fall through to `default`), then runs the `output` accumulators per
bucket through a shared `accumulate_into` helper. The pure quirks are
reproduced: an empty bucket emits only `{_id}` (the accumulator fields are never
created, because the inner per-doc loop never runs), an explicit `null` default
counts as absent, a missing/empty `output` falls back to `{count: {$sum: 1}}`,
and pathological Python-equal bucket keys (which the pure dict would collapse)
defer.

`$facet` (`aggregate::facet_stage`) is structurally trivial — it runs each named
sub-pipeline over a clone of the input through the recursive `apply_pipeline`
and gathers the results into one output doc; any deferring sub-pipeline defers
the whole stage.

Validation: `cargo test` (57) + `tests/test_rust_aggregate_parity.py` (curated +
5-seed fuzz, now generating `$bucket`/`$facet` with non-recursive facet
sub-pipelines) green, plus 8 extra local seeds (~500-580 `$bucket`, ~370-410
`$facet` pipelines handled per 5000, zero mismatches). Full Rust parity sweep
**508 cases**.

The pure-Python pipeline stages now ported to Rust: `$match`, `$limit`, `$skip`,
`$count`, `$project`, `$addFields`/`$set`, `$unset`, `$replaceRoot`/
`$replaceWith`, `$sort`, `$unwind`, `$group`, `$sortByCount`, `$bucket`,
`$facet`. Remaining: `$densify` and the storage-backed stages
(`$lookup`/`$graphLookup`/`$geoNear`/`$out`/`$merge`/`$sample`), which wait for
the storage layer to move into Rust (Phase 3+).

### Pipeline widening: `$densify` (numeric) — pipeline reaches the storage boundary

`densify::densify_stage` ports the numeric `$densify`: partition the docs (Python
dict semantics via the shared `group::GKey`), sort each partition by the numeric
field, then fill every multiple of `step` strictly between the bounds
(`"full"`/`"partition"` = the partition's observed min/max; explicit `[lo, hi]`).
The cursor arithmetic mirrors Python exactly — a `Num{Int(i128),Float(f64)}`
enum so `int + int` stays int and widens to f64 once a float enters, and
`_densify_canon` collapses an integer-valued float filler back to an int — and
the `existing_values` membership reproduces the set's `1 == 1.0 == True`
collision through `numeric::NumVal`. The no-input-docs-with-explicit-bounds case
and the "originals at/beyond `hi`" tail are reproduced too.

Defers to Python: any `range.unit` (date densify — both fixed-duration
`timedelta` and variable-length `relativedelta` month/quarter/year), non-numeric
field values / bounds / partition keys (Python's `sorted` / `<` would raise),
and explicit bounds that would emit > 1M fillers (Python raises there).
`numeric::from_int`/`from_f64` and `group::gkey`/`GKey` were exposed for reuse.

Validation: `cargo test` (62) + `tests/test_rust_aggregate_parity.py` (curated +
a dedicated 4000-case densify fuzz) green, plus 8 extra local seeds (5000
densify pipelines each, all handled, zero mismatches). Full Rust parity sweep
**512 cases**.

**Phase 2 milestone:** every aggregation-pipeline stage that doesn't touch
`Storage` is now ported to Rust — `$match`, `$limit`, `$skip`, `$count`,
`$project`, `$addFields`/`$set`, `$unset`, `$replaceRoot`/`$replaceWith`,
`$sort`, `$unwind`, `$group`, `$sortByCount`, `$bucket`, `$facet`, `$densify`.
The remaining stages (`$lookup`/`$graphLookup`/`$geoNear`/`$out`/`$merge` and
non-deterministic `$sample`) all need the storage layer, so the pipeline port
has reached its natural boundary; the next frontier is Phase 3 (moving storage /
wire / dispatch into Rust so the byte seam shifts outward and these light up).
