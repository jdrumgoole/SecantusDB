### `$near` distance bounds are validated like mongod's

`{$near: {..., $minDistance: null}}` ran as an unbounded query instead of being
rejected, and negative bounds were accepted outright. The bound parser could not
distinguish *key absent* from *key present with null*, so an explicit null was
silently treated as "no bound". Strings and bools were already refused, which is
why the gap looked covered.

Found by differential-probing `$near` against a real mongod: 4 of 8 cases
diverged.

While fixing it, a rationale recorded in the Rust engine turned out to be false.
A comment there justified accepting null by claiming the Java driver "sends
`$minDistance: null` when the caller passes no minimum". The driver source
disproves that — it omits the field entirely, in both serialisation paths
(`Filters.java`, `if (minDistance != null) { ... }`). So null-tolerance was never
needed for the Java gauge, and it made both servers silently run a query mongod
errors on. The comment and its three tests have been corrected.

#### Fixed

- `$near` / `$nearSphere` reject a null or negative `$minDistance` /
  `$maxDistance`, matching mongod's messages (`must be a number`,
  `must be non-negative`).
- The codes match per form, which differ: the nested GeoJSON form uses BadValue
  (2), while the legacy sibling form (`{geo: {$near: [x, y], $maxDistance: …}}`)
  uses mongod's dedicated 16895 (`$maxDistance`) and 16893 (`$minDistance`).
- An *absent* bound still means unbounded — the fix distinguishes it from an
  explicit null rather than rejecting both.
