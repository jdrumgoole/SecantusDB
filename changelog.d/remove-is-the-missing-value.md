### `$$REMOVE` is the missing value, not a marker of its own

Probed 9-for-9 against mongod 8.2.11: in every position — a projected field, an
array element, a nested document, `$ifNull`, `$type`, `$eq`, `$cond`, `$concat`,
`$sum`, a `$group` `_id` — `$$REMOVE` answers exactly what the equivalent
**absent field path** answers. Both engines instead gave it a marker of its own,
which then escaped the places that knew about it.

#### Fixed

- **A crash.** `{"$addFields": {"arr": [1, "$$REMOVE", 2]}}` put the marker
  object into the result, where `bson.encode` failed and the command returned
  `internal server error` (code 1). mongod returns `[1, null, 2]`.
- `$type: "$$REMOVE"` answered `"object"` — the marker's own Python type —
  where mongod says `"missing"`.
- `$concat` with `$$REMOVE` raised 16702; mongod returns null.
- `$ifNull: ["$$REMOVE", 9]` omitted the field instead of returning `9`.
- **The Rust engine deferred the variable entirely**, which on a server with no
  Python surfaced as a generic `BadValue` (2) for all 15 shapes probed.

`$$REMOVE` now follows the same two-position rule the engines already apply to
an absent path: the missing marker in field-value position (so `$project` /
`$addFields` omit the key), `null` as an operator argument.

#### Also fixed, found next door

- **`$setField` with an absent path wrote a null where mongod removes the
  field.** It evaluated its `value` in operator position, so only the literal
  `$$REMOVE` removed anything; `{"value": "$nosuch"}` set null. mongod treats
  both the same — probed.
- **`$setField` rejected `value: null` outright** (`$setField requires field,
  input, value`) because a present-but-null argument was tested with `is None`
  and read as absent. That is the one form that distinguishes *write a null*
  from *remove the field*.

#### Notes

The two names for the marker are now one: `MISSING` was already an alias of the
`$$REMOVE` sentinel, which is what made the conflation easy to miss.

23 cases added to `tests/test_mongod_differential.py`, five of them **twins** that
pair a `$$REMOVE` shape with its absent-path equivalent — the two must answer
identically, which is the claim this change rests on.
