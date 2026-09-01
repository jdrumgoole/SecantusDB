### A regex is a value, not only a pattern — and `$addToSet` dedups by BSON equality

Continues the measure-against-mongod-8.2.11 sweep, taking the last Rust-engine
capability defers. Two new standing probes cover the ground:
`tools/probes/regex_value_semantics.py` (104 shapes) and
`tools/probes/addtoset_membership.py` (30).

Three of the fixes are silent WRONG ANSWERS on the Python server, not errors —
found while reproducing a Rust-only backlog item, which is the whole argument
for running a probe against both servers rather than reading either one.

#### Fixed

- **A bare regex filter never matched a stored regex.** mongod matches `/ab/i`
  against a *string* by pattern and against a stored *regex* by equality, so
  `find({v: /ab/i})` returns `{v: /ab/i}`. Both servers returned nothing.
  Equality is exact-pattern plus options-as-a-SET: `/ab/im` is `/ab/mi`, and
  `/ab/i` is not `/ab/mi`. `$in`, `$nin` and `$regex` carry the same rule.
- **A regex was applied to JavaScript.** `bson.Code` subclasses `str`, so
  `find({v: /ab/})` matched `{v: Code("ab")}`. mongod never regex-matches code.
- **`$max` / `$min` over two regexes never moved.** `bson.Regex` defines no
  `__lt__`, so the comparison fell to a type-name fallback that reported every
  pair EQUAL — and both Rust arms had that accident written in as a comment
  citing what Python did. mongod orders by pattern, then option string.
- **`$eq` with a regex operand errored on the Rust server** for every document
  in the collection. On mongod `$eq` with a regex is equality *only* — the
  opposite of a bare regex, which matches — and a defer is an error on the
  standalone server.
- **`$addToSet` of a bool, a document, or a `Code` errored on the Rust server.**
  It deferred on the strength of a stale comment; `py_eq` had since grown the
  bool and `Code` rules, leaving only document field ORDER to add.
- **A `Code` value made `$set` fail on the Rust server** — the oplog update-diff
  walks every field through `py_eq`, which deferred on a JavaScript value paired
  with anything else.
- **`$pop`'s error quoted the argument as written**, so `Decimal128("-0")` read
  `found: -0`. mongod reports the coerced integer (`1E+2` is `found: 100`).

#### Changed

- The regex option-character table is now shared (`bsontypes.REGEX_OPTION_CHARS`
  / `regex_options_string`) between the in-memory sort and the index-entry
  encoder, on both servers. Two functions disagreeing about a sort key is how an
  index comes to change the sort answer. No `entryFormat` bump: the index
  encoder was the half that was already right.
