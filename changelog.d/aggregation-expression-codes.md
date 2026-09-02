### Expression arity and spec shape are PARSE errors, not fold errors

`tools/probes/agg_expressions.py` had never been reported on. Running it found **551 wrong error codes** across 58 operators on a 3,968-case corpus — the largest unaddressed surface left. It is now **50**.

#### The systematic cause

mongod builds the expression tree *before* it folds anything, so an argument-count or spec-shape mistake is a **parse** error, reported under the stage's wrapper (`Invalid $addFields :: caused by ::`). We folded first and reported the optimizer's (`Failed to optimize pipeline`), so 279 shapes carried both the wrong wrapper *and* the wrong code. `aggregate._expression_shape_problem` is the new pre-pass, alongside the literal-timezone one.

#### Fixed — a wrong answer

- **An ObjectId or Timestamp IS a date.** mongod accepts every BSON type carrying a timestamp, so `{$year: ObjectId("64b7f9a2…")}` answers 2023; we raised 16006 and refused the document. A one-element array is also the argument itself — `{$year: [<date>]}` — which we rejected too.

#### Fixed — codes and messages

- **Arity** (`$indexOfArray` / `$indexOfBytes` / `$indexOfCP` / `$range` / `$slice`): 28667, naming the bounds and the count.
- **Date extractors given an array** of any length but one: 40536.
- **Object-spec expressions** (`$firstN`, `$lastN`, `$minN`, `$maxN`, `$median`, `$percentile`, `$topN`, `$bottomN`): each carries its **own** Location code — 5787801, 5787900, 7436201, 7436200, 168 — not one shared code.
- **Unrecognised date-spec arguments**: eight operators, eight different codes, and only three append what they expected.
- `$rand`, `$getField`, `$indexOfCP`, `$bitNot`, `$setEquals` each answer mongod's codes now. `$getField` with an unknown argument, and `$rand` with a non-container spec, previously answered `ok`.

#### The families that look uniform and are not

Two fixes had to be narrowed after they made things worse:

- `$bitNot` splits its message by whether the operand is a **number at all** (28765 vs a bare 14). Its three siblings `$bitAnd` / `$bitOr` / `$bitXor` name no type whatsoever and always answer 14.
- `$setEquals` checks arity before types and **numbers** the offending argument (`2-th argument`). `$setUnion` and `$setIntersection` accept a single array and say "One argument", with their own codes.

Applying either rule to the family cost 46 shapes before the probe caught it.

#### Also

`agg_expressions.py` no longer compares `$rand`'s **value** — two correct servers disagree by design, and the moment `$rand` started succeeding the probe reported it as a wrong answer.
