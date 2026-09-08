# MongoDB fidelity: what is left, and in what order

**Status 2026-09-08.** Written after a session that took the Rust server's
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
| expression corpus (`agg_expressions.py`) | 6,628 | **12 code + 2 msg** | all 12 are item 1 below |
| comparison operators | 399 | **0** | |
| query result sets (new) | 266 | **0** | |
| positional path matrix (new) | 22 | **0** | |
| update result documents (new) | 527 | **0** | |
| findAndModify | 160 | **0** | shares the update path |
| **upsert seeding (new)** | 120 | **24** | item 2 below |
| decimal extremes | 364 | 0 | |
| aggregation stages / update ops / arg types / error surface | 3,940 | 0 | |

---

## The work, in order

### 1. Decimal transcendentals: the trig and hyperbolic family — DECISION FIRST

**12 of the 12 remaining expression-corpus divergences.** `$sin`, `$cos`,
`$tan`, `$asin`, `$acos`, `$atan`, `$atan2`, `$sinh`, `$cosh`, `$tanh`,
`$acosh`, `$pow` refuse a finite non-zero `Decimal128`; mongod answers at 34
digits.

**This is blocked on a decision, not on effort.** Joe chose correctly-rounded
for `$ln` / `$log10` / `$exp` / `$asinh` on 2026-09-08, knowing it diverges from
mongod in the last digit on ~20% of inputs. The same trade-off applies here,
with one extra wrinkle worth stating before it is extended:

> `src/secantus/expressions.py`'s `_DEC_TRIG` table computes at 34 digits
> **deliberately, to track mongod's own error** — its comment says so and cites
> a `$cosh` case that got *worse* when guard digits were tried. Extending
> correct rounding here REVERSES that earlier considered choice rather than
> fixing a bug.

**Size, if authorised:** the high-precision core already exists (`hp_mul` /
`hp_div` / `hp_sqrt` / `hp_ln` / `hp_exp` in `crates/secantus-core/src/decimal.rs`,
with Ziv-style rounding verification). The genuinely new piece is **argument
reduction modulo π** for `$sin` / `$cos` / `$tan`, which needs π to ~6,200
digits for the worst decimal128 argument. `$pow` is `exp(y·ln x)` and needs
nothing new. Estimate one focused slice, but **re-probe before starting** — the
`$sqrt` / `$asinh` work was smaller than its write-up implied.

### 2. Upsert seeding — 24 of 120, silently wrong writes

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

**Not in this item, and not yet understood:** the seeded field ORDER. Query
`{a: 1, b: 2}` gives `b, a`; `{a: 1, b: 2, c: 3}` gives `b, a, c`. Stable across
five runs and every update operator, so it is real — but it is neither the
query's order nor sorted, and CLAUDE.md records that this ordering CHANGED
between 6.0.16 and newer servers. **Measure a wider set of key names before
pinning anything.**

### 3. `sort` on an ambiguous positional path — 1 shape, and the backlog was wrong

`{x: [{"0": 5}]}` is the one document where both readings of `x.0` resolve: the
index gives `{"0": 5}` and the field name gives `5`. Sorting by `x.0` over a
collection containing it is **`16746 Ambiguous field name`** on mongod; both
servers sort it happily.

The existing entry claims something else entirely — that mongod *ranks* `[[5]]`
after `[{y: 5}]` and "ours puts it before". **That is not reproducible**: five
isolated subsets, including the exact pair named, agree on all three servers.
Correct the entry when this is worked.

### 4. `$rename` into an array element — 4 shapes, and a test to correct

mongod refuses all four; both servers accept them. `{$rename: {"v.0.a":
"v.0.b"}}` is `2 The source field cannot be an array element`, and the `$[]`
form is `2 The source field for $rename may not be dynamic`.

**`tests/test_crud.py::test_rename_with_positional_via_pymongo` asserts the
`$[]` case succeeds**, so it pins behaviour mongod rejects — the fourth test
this session written from what the server did rather than from a probe. Fix the
servers and the test together. `_rename_traverses_array` and two "cannot be an
array element" messages already exist; check their coverage first.

### 5. `$toLower` / `$toUpper` of a `Timestamp` — 2 shapes, needs a decision

mongod renders a `Timestamp` through a legacy `asctime`-like path in **local
time**: `Timestamp(1, 1)` prints `jan  1 01:00:01:1` on this box, because the UK
and Ireland were on UTC+1 through 1970. Reproducing it faithfully bakes a
timezone into the answer.

Note `$toString` of a `Timestamp` is a `241` on mongod, so these two operators
accept a type `$toString` refuses. **Measure on a `TZ=UTC` server before
implementing**, and decide whether a host-timezone-dependent answer is wanted at
all.

### 6. `$toDate` parse messages — 2 shapes, known and bounded

mongod's per-position timelib diagnostic (`'abc'` names the offending character
and where its scanner stopped). Needs timelib's own lexer, timezone abbreviation
tables and per-position error accumulation. Both servers already give the same
message as each other and the correct code; inventing a position would look
authoritative and be wrong. **Lowest value of anything here.**

---

## Housekeeping the sweeps earned

- **Promote three probes into `tools/probes/`.** The query-result-set,
  update-result-document and upsert-seeding sweeps are all `/tmp`-local. Each
  found real bugs on its first run; each is worth re-running after any change to
  `query.rs` / `update.rs`.
- **Two backlog entries are STALE and should be deleted**, both verified against
  mongod on 2026-09-08: "`$expr` with `$gt` over a mixed-type collection defers"
  (fixed in #1418) and "a dotted POSITIONAL component ... we implement only half
  of it" (fixed in #1420).

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
