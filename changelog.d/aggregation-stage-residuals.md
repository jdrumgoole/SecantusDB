### Six aggregation stages answered differently from MongoDB

Follow-up to the aggregation-results probe: **both servers now match MongoDB on
all 48 stages it measures**, up from 42 (Python) and 41 (Rust).

#### Fixed

- **`$setWindowFields` returned rows in the wrong order.** MongoDB emits
  partition by partition, in first-seen partition order, and within each
  partition in `sortBy` order. Both servers preserved the *input* order — the
  docstring even claimed that was correct. Wrong order is wrong results as soon
  as a `$limit` follows.
- **`{$count: {}}` failed in a `$group`** with *"Unrecognized expression
  '$count'"*. The accumulator existed and evaluated correctly all along;
  nothing ever reached it. The constant folder got there first — `{$count: {}}`
  reads no field, so it looked like a constant expression and was handed to the
  expression evaluator, which doesn't know `$count`. It now works in `$group`
  and `$setWindowFields`, the two accumulator positions, and is still refused
  as an expression elsewhere. The `$count` **stage** (`{$count: "<field>"}`) is
  unaffected, including inside `$facet`.
- **A dotted `$project` dropped the surviving parent.** `{$project: {"sub.k":
  1}}` over `{sub: {}}` emits `sub: {}` on MongoDB, and an array of documents
  is pruned element-wise. `find`'s projection had this right; the `$project`
  stage was a second implementation that only checked the leaf. It now
  delegates, so there is one implementation of the rule.
- **`$bucket` put a `Decimal128` in the `default` bucket.** The placement used
  Python's comparison operators, which have no ordering between a `Decimal128`
  and an `int` — and the resulting `TypeError` was silently swallowed. It now
  uses the same BSON ordering the boundary validation already used.
- **`$bucketAuto` split equal values across buckets and misreported `max`.**
  `1.5` and `Decimal128("1.5")` are the same value to MongoDB, which never puts
  equal values in different buckets; and the remainder goes to the *earlier*
  buckets (8 values into 3 is 3/3/2, not 2/3/3).
- **`$bucket` and `$bucketAuto` were fixed on the Rust server too**, and had to
  be: fixing only the Python half turned six engine-parity cases red, because
  the two servers then genuinely disagreed. Parity doesn't say which side is
  right, but it does say they move together.
- On the Rust server: **`$max` over `{NaN, Infinity}` answered `NaN`** — its
  `bson_lt` treated NaN as unordered, following Python's `<` rather than
  MongoDB's order, so the accumulator never moved off the first NaN it saw. And
  **a `$group` keyed on a `MinKey`, `Timestamp`, `Binary`, `Regex` or `Code`
  failed the whole stage**; each of those buckets by exact value.

Measured against mongod 8.2.11.
