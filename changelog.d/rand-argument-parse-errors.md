### Every wrong argument to `$rand` carried the wrong wrapper

`$rand` takes no arguments, and both ways of getting that wrong are PARSE
errors on mongod, so they take the stage's wrapper (`Invalid $addFields ::
caused by ::`) rather than the executor's. The pure engine already produced the
right code and the right sentence for all of them — only the routing was wrong,
on 45 shapes, which is 76% of every remaining message difference on this
surface.

The rule has a shape worth stating, because a single "must be a document" check
gets it wrong three ways (measured against mongod 8.2.11, 2026-09-05):

| argument | mongod |
|---|---|
| `{}` **or `[]`** | valid — returns a double |
| non-empty document or array | `3040501` "$rand does not currently accept arguments" |
| any scalar | `10065` "invalid parameter: expected an object ($rand)" |

An empty *array* being accepted is the easy one to miss, and the two wrong
arguments get two different codes for what reads as one mistake.

The Rust engine already had all of this in its parse-time scanner, correctly,
from an earlier pass — this brings the pure engine to it.

With this, the probe's message differences fall from 59 to **14**, and the
wrong-code set is byte-identical to before. What remains on this surface is a
flat tail: 28 codes across 28 different operators with one shape each, and 14
messages likewise. No further rule-shaped win is visible.

#### Fixed

- `aggregate.py`: `_expression_shape_problem` classifies both `$rand` argument
  errors as parse errors.

#### Changed

- `tests/test_expression_parse_time_wrappers.py`: 10 more cases covering every
  measured `$rand` shape, including the two valid ones.
