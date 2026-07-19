### `$push` `$sort` direction validation and `$currentDate` boolean acceptance

Two more update-operator divergences from real mongod are closed. A `$push`
with a `$sort` modifier now rejects any direction that isn't exactly `1` or
`-1` (previously an out-of-range value such as `2` silently sorted anyway), and
`$currentDate` now accepts a boolean `false` (like `true`, it sets the current
Date) instead of wrongly rejecting it. Both are verified against real mongod
7.0.12.

#### Fixed

- `$push` `$sort` now raises code 2 when the whole-element sort direction is a
  number other than `±1` ("The $sort element value must be either 1 or -1"),
  when a `{field: dir}` direction is not `±1` ("The sort element value must be
  either 1 or -1"), or when the spec is a non-numeric value such as a string,
  bool, or array ("The $sort is invalid: use 1/-1 …"). A whole-double `±1`
  (e.g. `1.0`, or `{field: 1.0}`) is accepted and sorts, matching mongod —
  previously a whole-double scalar sort was wrongly rejected.
- `$currentDate: {field: false}` now sets the current Date (a boolean `false`
  is the same set-Date form as `true`) instead of raising. A non-boolean scalar
  argument and a bad or missing `{$type: …}` now raise code 2 with mongod's exact
  message instead of an uncoded "not understood" error.
