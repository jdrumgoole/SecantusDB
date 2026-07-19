### Array / set / string operator type-guards match mongod's error codes (and stop silently accepting)

A discovery sweep of ~120 aggregation-operator error cases against real mongod
7.0.12 found a bounded set of type-guard divergences, several of them **silent
accepts** — operators that returned a value where mongod errors. They now match
mongod: `$arrayElemAt`, `$in`, and `$regexMatch`/`$regexFind`/`$regexFindAll`
were silently returning `null`/`false`/`[]` on a bad argument and now error, and
the rest returned a generic `TypeMismatch` (14) where mongod uses a specific
`Location` code. Both the Python and Rust servers are fixed (the Rust core defers
each case — `$in`, `$arrayElemAt`, and the regex ops needed Rust-side fixes to
stop computing a value), verified against real mongod.

#### Fixed

- **Silent accepts, now errors (both engines):** `$arrayElemAt` non-array →
  `Location28689`; `$in` non-array second argument → `Location40081`;
  `$regexMatch` / `$regexFind` / `$regexFindAll` non-string `input` →
  `Location51104` (a `null`/missing input stays valid — `false`/`null`/`[]`).
- **Generic `TypeMismatch` (14) → mongod's `Location` code:** `$size` (17124),
  `$indexOfArray` (40090), `$setUnion` (17043), `$setIntersection` (17047),
  `$setDifference` (17048), `$setIsSubset` (17046), `$anyElementTrue` (17041),
  `$allElementsTrue` (17040), `$mergeObjects` (40400), `$range` non-numeric bound
  (34443), `$indexOfBytes` non-string (40091/40092), `$binarySize` (51276),
  `$bsonSize` (31393).
