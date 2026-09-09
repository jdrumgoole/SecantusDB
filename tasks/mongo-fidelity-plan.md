# MongoDB fidelity: what is left, and in what order

**Status 2026-09-09.** Written after a session that took the Rust server's
expression corpus from 91 code divergences to 12 and opened three probe surfaces
nothing had covered. Every item below is sized from a **measurement against
mongod 8.2.11**, not from reading the source — this repo's own record says
estimates from reading have been wrong in both directions, four times.

The rule that governs all of it: **each server's exemplar is the real product it
imitates.** `mongod` for both MongoDB servers, PostgreSQL for both PG servers.
Comparing two SecantusDB servers to each other is a drift detector, never a
correctness check (see CLAUDE.md, "Design constraints").

---

## Where the numbers stand

| sweep | cells | divergent | note |
| --- | --- | --- | --- |
| expression corpus (`agg_expressions.py`) | 6,628 | **2 msg** | the 12 code were item 1, now fixed |
| decimal transcendental rounding (new) | 285 | **2-3** | the rest is mongod's OWN error; item 1 |
| comparison operators | 399 | **0** | |
| query result sets (new) | 266 | **0** | |
| positional path matrix (new) | 22 | **0** | |
| update result documents (new) | 527 | **0** | |
| findAndModify | 160 | **0** | shares the update path |
| upsert seeding (new) | 120 | **0** | fixed, item 2 |
| sort-path ambiguity (new) | 204 | **0** | fixed, item 3 |
| sort-path resolution (new) | 48 | **0** | fixed, item 3 |
| `$rename` paths (new) | 35 | **0** | fixed, item 4 |
| decimal extremes | 364 | 0 | |
| aggregation stages / update ops / arg types / error surface | 3,940 | 0 | |

---

## The work, in order

### 1. Decimal transcendentals — DONE 2026-09-09, and the entry was wrong in BOTH directions

The entry said "12 operators refuse a finite non-zero `Decimal128`" and framed
the work as a policy choice between correctly-rounded and tracking mongod's
error. Measured, neither half held.

**Which server refuses.** The PYTHON server refused nothing — all fifteen
answered already. The RUST server refused all ten trig/hyperbolic operators,
108 of 285 measured shapes. The entry conflated the two servers, and a first
re-probe of mine then conflated "refuses a decimal" with "refuses an
out-of-domain input", because it ran one aggregate over a corpus containing
values outside `[-1,1]`.

**Whether the policy choice exists.** It does not. Compared against a 60-digit
mpmath reference over 285 shapes, **mongod is correctly-rounded only ~78% of
the time** — it carries Intel RDFP's last-digit error. Exact agreement is
therefore CAPPED, and no working precision reaches it, because the residual is
an implementation artefact rather than a rule. What follows is the useful part:
**being correct is the best available approximation to mongod.** Where we are
correctly-rounded, agreement equals mongod's own correctness rate.

That immediately condemned the "compute at 34 digits to reproduce mongod's
accumulation" strategy the Python hyperbolics used, which had been adopted on
the strength of a single `$cosh` case:

| | agreement at 34 digits | computed wide |
| --- | --- | --- |
| `$tanh` | 3/20 | **12/20** |
| `$sinh` | 8/20 | **12/20** |
| `$acosh` | 12/15 | **14/15** |
| `$cosh` | 16/20 | 16/20 |

`$cosh` did not even drop, so the case that justified the strategy does not
support it.

**Done:** the Python hyperbolics now compute wide and round once; the Rust
server implements all ten missing operators (`sinh` / `cosh` / `tanh` / `acosh`
/ `atanh` from the existing `hp_exp` / `hp_ln` / `hp_sqrt`; `atan` / `asin` /
`acos` from an argument-reduced Taylor series and a pi/2 constant; `sin` / `cos`
/ `tan` by reduction modulo an embedded 2*pi). Both servers now answer all
fifteen with **zero refusals** and identical results.

Agreement with mongod: Python 209 -> 224 of 285, Rust **90 -> 224**. The
remaining 61 are mongod's own error; the probe counts only the 2-3 that are
still ours. Sweep: `tools/probes/decimal_transcendental_rounding.py` — the only
probe here that asks "is the answer RIGHT?" as well as "does it match?", which
is what separates our bug from mongod's. Gate:
`tests/test_decimal_trig_family.py` (38).

**One bounded gap remains:** `sin` / `cos` / `tan` on the Rust server reduce
against 2*pi embedded to ~1200 digits, so an argument past about 1e1100 defers
rather than answering. mongod carries pi to the full decimal128 range. Widening
the constant is the fix if anyone meets one.

### 2. Upsert seeding — DONE 2026-09-08 (was 24 of 120)

When an upsert inserts, mongod seeds the new document from the QUERY. Both
servers seed only bare equality, so five query forms lose their fields:

| query | mongod seeds | both servers |
| --- | --- | --- |
| `{a: {$eq: 1}}` | `a: 1` | nothing |
| `{a: {$in: [1]}}` (ONE element) | `a: 1` | nothing |
| `{a: {$all: [1]}}` (ONE element) | `a: 1` | nothing |
| `{$and: [{a: 1}, {b: 2}]}` | `a: 1, b: 2` | nothing |
| `{$or: [{a: 1}]}` (ONE branch) | `a: 1` | nothing |

A multi-element `$in` and any range operator seed nothing on mongod either, so
the rule is **"clauses that imply a single equality"** — the same question
`_query_implies_partial` already answers for partial indexes. **Check whether
that helper can be reused before writing a second implication engine**; it is
sound-not-complete by design and its NaN / type-bracket gates were hard-won.

**Resolved** by seeding every implied equality on both servers, with mongod's
`54 cannot infer query fields to set` when two clauses name one path. Values are
0 of 120 across mongod, Rust and Python. `_query_implies_partial` turned out NOT
to be the right thing to reuse — it answers "does query A imply partial filter
B", a two-sided question, where this needs "what single equality does this
clause imply", which is one-sided and much smaller.

**Deliberately not reproduced:** the seeded field ORDER. Query
`{a: 1, b: 2}` gives `b, a`; `{a: 1, b: 2, c: 3}` gives `b, a, c`. Stable across
five runs and every update operator, so it is real — but it is neither the
query's order nor sorted, and CLAUDE.md records that this ordering CHANGED
between 6.0.16 and newer servers. **Measure a wider set of key names before
pinning anything.**

### 3. Sort-path resolution — DONE 2026-09-09 (was 1 shape; found 2 rules)

Filed as one shape. It was two, and the SECOND was the serious one.

**The ambiguity refusal.** `{x: [{"0": 5}]}` is a document where both readings
of `x.0` resolve, and sorting by it is `16746 Ambiguous field name` on mongod
while both servers sorted happily. The rule took three attempts to get right and
is narrower than it looks: a component is ambiguous when it is a **valid index**
of the array *and* some element document carries that **exact key**. Both halves
bite — `x.1` over `[{"1": 5}]` is allowed (index 1 is past the end) and `x.0`
over `[{"00": 5}]` is allowed (`"00"` is not the key `"0"`), and the element
carrying the key need not be the one at that index. A first implementation that
checked only "the component is numeric and some element has that key"
**over-fired**, refusing queries mongod answers. 19 measured shapes.

**The descent rule, which nothing had ever probed.** mongod descends one level
into an array-valued sort key reached by a FIELD NAME and does **not** descend
one reached by an INDEX. Both servers descended in every case, so `{x: [[5]]}`
sorted by `x.0` ranked among the NUMBERS instead of the arrays — wrong order,
and wrong RESULTS under a `limit`. Neither parity nor `index_result_sets` could
see it: parity pinned the two engines to each other and they were wrong
together, and the index probe compares `_id` SETS precisely so ordering stays
out of it. This is the "parity is not correctness" shape again, and it is why
the probe promoted below compares ORDER.

**Also fixed on the way:** the Rust aggregation `$sort` stage resolved its keys
with `get_path` — no array descent at all — so it disagreed with the same
server's `find` on 9 of 48 shapes; and the Rust `find` handler reported this
refusal under the UPDATE executor wrapper with an empty command name
(`Plan executor error during  ::`), because `command_error` assumed a read
command never carries an execution-time error. Both servers now share one copy
of the walk (`secantus_core::paths::sort_path_values` /
`ambiguous_sort_path`), which is what stops the two Rust sort paths drifting
again.

Values are 0 of 204 (ambiguity) and 0 of 48 (resolution) across mongod, Rust and
Python. Gate: `tests/test_mongod_differential.py -k sortpath` (14 cases) plus
`tests/test_sort_path_resolution.py` (39). Sweep:
`tools/probes/sort_path_resolution.py`.

**The old backlog entry was wrong** and has been deleted: it claimed mongod
*ranks* `[[5]]` after `[{y: 5}]` and "ours puts it before". Five isolated
subsets, including the exact pair named, agree on all three servers.

### 4. `$rename` path refusals — DONE 2026-09-09 (was "4 shapes")

Sized as four shapes and a test to correct. The four were right; what the entry
missed is that mongod distinguishes the two refusals by **when it can decide
them**, and reports them differently:

* a **dynamic** component (`$`, `$[]`, `$[id]`) in either path is a PARSE
  error — raised without looking at the document, so an absent source still
  errors, and sent bare;
* a path that indexes into an **array** is an EXECUTION error — discovered per
  document, sent under `Plan executor error during update :: caused by ::`, and
  skipped entirely when the source field is absent, because then the `$rename`
  is a no-op.

Precedence, measured: source-dynamic > destination-dynamic > source-array >
destination-array, with the general `No array filter found for identifier` check
ahead of all four. The message also names **the field that holds the array**
(`deep.n.0.a` → `'n'`), and renders `_id` with mongod's VALUE repr — a string
`_id` is quoted, an ObjectId wrapped — where the Python server used `str()`.

**Two more fell out of the same probe, neither in the entry:**

- the Python server sent the code-28 `cannot use the part (s of s.a) to traverse
  the element` **bare**; mongod wraps every code-28 traverse failure (`$set` /
  `$inc` / `$push` measured alongside). The Rust twin already had `.exec()` —
  single-server drift, exactly the shape CLAUDE.md says to grep for;
- the Rust server reported `No array filter found for identifier` as a
  **command failure** rather than a per-statement `writeErrors` entry, which a
  driver sees as a different exception class (`OperationFailure`, not
  `WriteError`) and which fails a whole unordered batch instead of one statement.

`tests/test_crud.py::test_rename_with_positional_via_pymongo` asserted that
`items.$[].a` renames element-wise, so it pinned behaviour mongod rejects. It is
now `test_rename_rejects_a_positional_path_via_pymongo` and asserts the refusal
plus the document being left untouched.

0 of 35 across mongod, Rust and Python. Sweep
`tools/probes/rename_paths.py`; gates `tests/test_mongod_differential.py -k
rename` (17 cases) and `tests/test_rename_array_and_dynamic_paths.py` (25).

### 5. `$toLower` / `$toUpper` of a `Timestamp` — PYTHON DONE, Rust needs a DEPENDENCY decision

mongod renders a `Timestamp` through a legacy `asctime`-like path in **local
time**, measured by running a second mongod 8.2.11 under `TZ=UTC` beside the
default one (2026-09-09):

| expression | mongod, host TZ (Europe/Dublin) | mongod, `TZ=UTC` |
| --- | --- | --- |
| `{$toLower: Timestamp(1, 1)}` | `jan  1 01:00:01:1` | `jan  1 00:00:01:1` |
| `{$toLower: Timestamp(1700000000, 3)}` | `nov 14 22:13:20:3` | `nov 14 22:13:20:3` |

The second row agrees on both because Ireland is on UTC in November — a
one-shape probe would have concluded "not TZ-dependent" and been wrong.

**The Python server already matches mongod on every measured value**, rendering
in the process's local timezone via `time.localtime`. Like item 1, the entry
described a gap that was Rust-only.

**The Rust server still answers `16007`, and closing it is a DEPENDENCY
decision, not a coding one.** `secantus-core` is deliberately dependency-light
and Rust's `std` exposes no timezone database, so local-time rendering needs
`chrono` (or an equivalent) added to the crate the whole Rust server builds on.
That is worth deciding deliberately rather than smuggling in behind two
operators. The format itself is settled: `%b %e %H:%M:%S:<increment>`, the
increment unpadded, then ASCII-cased.

### 6. `$toDate` string parsing — DONE 2026-09-09 (was re-sized three times)

The entry's history is the lesson: "2 shapes, messages only, lowest value here",
then "8 of 12, three wrong ANSWERS", then "timelib's full format table". Each
version was written with confidence and each was wrong, because each probe was
narrower than the area.

mongod's `$toDate` runs **timelib**, and both servers implemented a small
ISO-8601 subset. 15 of 19 shapes diverged. Both now match on all 45, including
the refusals.

Three rules, all measured rather than assumed:

* **The slash form is US-first by RULE.** `31/12/2020` is refused outright, so
  `MM/DD/YYYY` wins and day-first is not a fallback.
* **A trailing letter is a MILITARY TIMEZONE**, not the ISO separator.
  `"2020-01-01T"` is `07:00:00` because `T` is UTC-7 — which is what the earlier
  "looks host-dependent" caution was actually seeing. `J` is invalid. Verified
  across ten letters, and a `TZ=UTC` server answers the same.
* **An out-of-range component is a parse FAILURE, not a rollover** —
  `13/01/2020` and `12/32/2020` are both refused.

Also accepted now: `YYYY/MM/DD`, month names in either order with an optional
comma, non-padded ISO, `@<unix seconds>` (negative and fractional), compact
`YYYYMMDD[THHMMSS]`, ISO week dates, an hour with no minutes, and surrounding
whitespace.

**Still deferred, unchanged:** mongod's per-position timelib diagnostic for a
string its scanner got partway through. That needs timelib's own lexer and
abbreviation tables; inventing a position would look authoritative and be wrong.
Both servers give the same code and a general message.

Sweep `tools/probes/todate_string_formats.py` (45 shapes, 0 divergent both
servers); gates `tests/test_todate_string_formats.py` (46) and
`tests/test_mongod_differential.py -k todate` (28).

---

## Housekeeping the sweeps earned

- **Promote three probes into `tools/probes/`** — DONE 2026-09-09, and the
  promotion itself found two bugs. They are now `query_result_sets.py`,
  `update_result_documents.py` and `upsert_seeding.py` (266 / 527 / 120 shapes,
  0 divergent). Two more went in alongside: `sort_path_resolution.py` (item 3 —
  the only probe here that compares ORDER) and `rename_paths.py` (item 4).

  **The `/tmp` originals compared mongod against the RUST server only.** Moving
  them onto `_servers.py` added the Python column, and the query sweep
  immediately found a CRASH — `$expr` with `$gt` over a `Decimal128("NaN")`
  answered `internal server error` — plus `{$eq: [NaN, NaN]}` answering false
  where mongod says true (the Rust server got that right for `Decimal128` and
  wrong for a plain `double`, so the answer depended on the numeric type). Both
  fixed; see `changelog.d/expression-nan-equality.md`.

  Worth keeping: **a throwaway probe's column list is part of its result.** A
  sweep that omits a server proves half of what it claims, and "0 divergent"
  from it reads exactly like "0 divergent" from a complete one.

  One deliberate asymmetry: `upsert_seeding.py` compares field/value PAIRS
  rather than key order, because mongod's seeded field order is its own hash
  order and both servers sort deliberately. Comparing it would report four
  divergences forever and train the reader to ignore the probe.
- **Two backlog entries are STALE and should be deleted** — DONE. Both are gone
  from `tasks/backlog.md` as of 2026-09-09: "`$expr` with `$gt` over a
  mixed-type collection defers" (fixed in #1418) and "a dotted POSITIONAL
  component ... we implement only half of it" (fixed in #1420).

---

## Two lessons this session paid for, worth keeping in front of whoever picks
## this up

1. **A test can pin an unverified claim as easily as a comment can**, and it
   then reads as proof. Three did this session: `$topN`'s object-spec message
   (passing only because `Unrecognized expression` shares code 168, and the code
   was compared first), `cmp(NaN, 5) == Equal`, and the `$pull` docstring
   asserting "equality via the same query engine" — which is exactly the engine
   that adds membership.
2. **Probes lie in specific ways, and mine lied four times**: calling a helper
   directly instead of going over the wire; asserting evaluator-raised errors
   against the parse-time walker; a reference helper using unary `+`, which
   applies the *default* 28-digit decimal context; and a "0 divergent of 384"
   engine-vs-engine number that was circular, because the sweep had been used to
   choose the behaviour it was measuring. Give every sweep a case whose answer
   you already know.
