### `$strLenCP` measured a JavaScript value, and the `$to*` family parsed one

`bson.Code` subclasses `str`. A previous fix addressed the surfaces that NAME a
type — `$type` and the shared type-name helper — and stopped there. Naming a
type correctly and DISPATCHING on it correctly are different things, and the
operators that *consume* a string still admitted a JavaScript value:

- `$strLenBytes`, `$strLenCP` and `$binarySize` returned **3** — the length of
  `x=1` — where mongod refuses the argument outright. Wrong values, not wrong
  messages. Each already had an error branch naming the type correctly; the
  guard `isinstance(s, str)` meant it was never reached.
- `$toInt` / `$toLong` / `$toDouble` / `$toDecimal` / `$toDate` / `$toObjectId`
  fed the JavaScript source text to string *parsers*, so `$toInt` complained
  "Did not consume whole string" and `$toDate` tried to read `x=1` as a date,
  where mongod answers `241 Unsupported conversion from javascript to <target>`.

`$toString` had been fixed for `Code` already — at one site, with the type name
hardcoded as the literal `"javascript"`. That is the second one-site fix for
this root cause, and it is why a scoped `Code` was still misnamed there.

**`$toBool` is the measured exception.** `{$toBool: Code("x=1")}` is `true` on
mongod; every other target refuses. A single uniform "reject Code in $convert"
guard would have been simpler, wrong, and would have looked correct against
seven of the eight targets. All eight were probed (8.2.11, 2026-09-04/05)
rather than assumed.

Against mongod the probe goes from 31 wrong codes and 148 message differences
to **28 and 142** — exactly the nine attributable to this cause. The three
`Code` cases that remain (`$ifNull`, `$rand`, `$setEquals`) are stage-wrapper
differences that diverge for any value.

The Rust engine was already correct on all eleven: its `Bson` enum has a
distinct `JavaScriptCode` variant, so the subclass trap cannot occur there.

#### Fixed

- `expressions.py`: `$strLenBytes` / `$strLenCP` / `$binarySize` test
  `is_bson_string`; the five non-bool arms of `_convert_value` do too, so a
  `Code` falls through to the ConversionFailure the function already raised;
  `$toString`'s rejection derives the type name instead of hardcoding it.

#### Changed

- `tests/test_bson_code_type_and_signed_zero.py`: 22 more cases covering both
  surfaces, scoped and unscoped, including the `$toBool` exception and why the
  fix is per-arm.
