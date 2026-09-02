### A wrong-typed operand names mongod's error instead of "not supported"

An aggregation operator that had to *refuse* an argument had only one way to say so in the Rust engine — `Fallback::Defer` — and a defer on the standalone Rust server has no Python behind it. It surfaces as `2 BadValue`, "aggregation pipeline uses a stage or operator not supported by the Rust server". So `{$size: 1}` told the client this server cannot do `$size`. It can; `1` is not an array.

`tools/probes/agg_expressions.py` measured **908 such divergences** against mongod 8.2.11 across ~120 operators, with **zero wrong values** — the whole surface was error-shaped. Roughly 900 of them were one defer standing in for an ordinary argument complaint.

| | Before | After |
| --- | --- | --- |
| `agg_expressions.py`, Rust server | 902 code + 6 message | **117 code + 0 message** |
| `agg_expressions.py`, Python server | 50 code + 212 message | **28 code + 80 message** |
| Wrong values, either server | 0 | 0 |

Every expectation was measured against 8.2.11 rather than derived, and several defeat a reasonable guess.

#### Fixed

- **The thirteen date extractors** (`$year` / `$month` / `$hour` / …) report `16006 can't convert from BSON type <T> to Date` instead of deferring — 169 shapes from one helper. Their `{date, timezone}` options form is now validated too: an unrecognised key is `40535` and reports the first offender *even when `date` is present and valid*, and a spec with no `date` is `40539`, echoing the document back.
- **The eight `$convert` shorthands.** An array of any length but one is `50723 $toInt requires a single argument, got 3`; an unsupported pair is `241 Unsupported conversion from objectId to int`; overflow, NaN and infinity each carry their own sentence under that same code. `{$toInt: [1]}` now unwraps to the single argument mongod reads.
- **Fourteen type-guarded operators** — `$size`, `$first`/`$last`, `$strLenCP`/`$strLenBytes`, `$reverseArray`, `$arrayToObject`/`$objectToArray`, `$bsonSize`, `$binarySize`, `$tsSecond`/`$tsIncrement`, `$allElementsTrue`/`$anyElementTrue` — plus `$concat`, `$concatArrays`, `$in`, `$arrayElemAt`, `$slice`, `$indexOfArray`, `$indexOfBytes`, `$indexOfCP`, `$split`, `$toLower`/`$toUpper` and the `$bit*` family.
- **`$ifNull`, `$setEquals`, `$rand` and `$getField`** are refused at parse time, under the stage's `Invalid $addFields ::` wrapper rather than a pipeline one.

#### Capabilities the Rust server was missing

Four conversions mongod performs were reported as unsupported: `$toDate` of a **date string**, an **ObjectId** or a **Timestamp**, and `$toLong` / `$toDouble` / `$toDecimal` of a **date** (`$toInt` of a date really is refused — the arms are not interchangeable). `$convert`'s `onError` now catches an unsupported *pair*, which it previously ignored; the one form that exists to survive a bad conversion was the one that could not.

#### Bugs this found on the Python server too

These sit outside the probe corpus, so nothing had ever compared them:

- `$setUnion` / `$setIntersection` / `$setDifference` answer **null** for a null operand; both servers raised. Operands are scanned left to right, so `{$setUnion: [null, 1]}` is null while `{$setUnion: [1, null]}` raises on the int. `$setEquals` and `$setIsSubset` refuse null instead — two rules, not one.
- `$setDifference` and `$setIsSubset` carry a **different code per position** (17048/17049 and 17046/17042); both reported the first-argument code either way.
- `$getField`'s bare form is an **expression**, not a literal field name. A plain string still evaluates to itself, so `{$getField: "s"}` is unchanged, but `{$getField: "$n"}` resolves the path and then refuses the int — where taking it literally looked for a field named `$n` and answered *missing*. A literally-dollared name goes through `$literal`, which both engines had been reading as the options form and rejecting as an unknown argument. The object form requires `input` and does not fall back to `$$CURRENT`.
- `$toDecimal` of a date, `$tsSecond`/`$tsIncrement`'s wording, and `$slice`'s "but is of type:" phrasing.
- `$split`'s **second** argument is `10503900` on 8.2.11, not the `40086` recorded here; the first keeps `40085`. Only one of the pair moved.
- The four `$bit*` operators do not agree on how to refuse a bool: `{$bitOr: [1, true]}` is the fold family's bare `14 ... only supports int and long operands.` with **no type named**, while `$bitNot` calls a bool non-numeric (`28765`). One sentence had been standing in for both.
- **One value renderer was standing in for two mongod serializers.** `specification must be an object; found $firstN: [ 3, 1, 2 ]` renders in mongod's *shell* form — inner spaces, `ObjectId('…')`, `new Date(1767323045000)`, `BinData(0, 7A)` — while `$replaceRoot`'s `Input document: {n: 1}` uses the compact form with none of those. Both call sites went through `bson_value_repr_stage`, so one of them was always wrong — and neither wholesale choice works: rendering everything compact leaves 66 shapes wrong, rendering everything shell-form leaves 13. Probing the two families side by side is what separated them.

#### Rules worth recording

- mongod says `missing` for an absent field path where it says `null` for an explicit null. One `eval` collapses both, so the distinction is recovered when a message is built rather than threaded through evaluation.
- Null-tolerance is **per operator**: `$size` and `$strLenCP` refuse null; `$first` and `$reverseArray` answer null.
- `$bitNot` uses **two** codes — `28765` for a non-numeric operand, `14` for a numeric one it cannot use — where `$bitAnd` / `$bitOr` / `$bitXor` use one sentence for both.
- `$getField` **never folds**, not even with a wholly literal `input`, because it reads `$$CURRENT`. That now lives in `is_constant_expression`, which both engines consult.
- The wordings are not interchangeable: "found: {}" vs "but is {}" vs "but was of type: {}", `$setEquals`'s literal "1-th argument", and `$tsSecond`'s verbatim **leading space**.

#### Caught by the parity suite

Two of these were found by `tests/test_rust_expressions_parity.py` after the Rust half moved and the Python half had not — which is what that suite is for. It also caught a defect in this change: a **finite** double too large for the target (`{$toLong: 1e30}`) fell into the non-finite arm and was reported as "Attempt to convert infinity value", where mongod says it overflowed and echoes `1e+30`. Parity flagged the drift within seconds; the oracle said which side to move.

Two parity tests asserted the Rust engine `raw is None` for these operators — pinning the gating *decision* rather than the behaviour, so they broke the moment the operators learned to name their own errors. They now assert what actually matters: defer or name it, but the client sees mongod's code either way.

#### A stale citation, not a regression

`$split`'s second-argument code was recorded as `40086` in three places, one of them a test whose docstring read "mongod 7.0.12-verified." It *was* right for 7.0.12 — 8.2.11 answers `10503900`, while the first argument keeps `40085` — and the expectation survived the 8.x retarget unchecked. Re-probed and re-dated. This is the shape `CLAUDE.md` warns about: a version citation records when something was measured, not what the server does now.

`tests/test_ci_runs_rust_server_tests.py` also did its job — the new test file is gated on `importorskip("_secantus_server")`, so it would have run **nowhere** until named in the `storage-engine` job. Now wired in.

#### Also fixed

`Conv::Failed` was serving three different mongod messages at once — overflow, non-finite, and unsupported-pair — which was invisible while all three deferred. One of its own comments already described a case as "Unsupported conversion" while the code classified it as a failure.

`tests/test_expression_operand_errors.py` pins all of it against **both** servers.

Two existing claims turned out to be untested rather than wrong-by-regression. Three tests asserted `$getField`'s non-string `field` was `5654602`; mongod answers `3041704`. And `{$getField: 0}` was listed as a *parse* error, which nothing could check — it only reached the parse pass because the constant-fold pre-pass happened to evaluate it and surface the exception. On an empty collection mongod returns no documents and no error, so it is a runtime error; that test now drives the evaluator.

#### Still open

117 shapes, characterised in `tasks/backlog.md`: Decimal128 arithmetic and the transcendentals (the deliberately deferred half), `$mergeObjects` and the trigonometric domain errors (both need mongod's value rendering), and a handful of value gaps in `$substr*` / `$strcasecmp` / `$range`.
