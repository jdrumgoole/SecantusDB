### Both servers answer mongod's argument errors, and the range operators bracket by type

A measured sweep of both servers against mongod 8.2.11 found the same shape of
bug over and over: an operator that works perfectly well was refusing, or
silently mis-answering, a *bad argument*. The Rust server had the worse version
of it — with no Python engine behind it, any refusal it could not name reached
the client as "not supported by the Rust server", so `{$round: ["$n", 1.5]}`
reported that the server cannot do `$round`.

#### Fixed

- **Range operators are type-bracketed.** `{v: {$gt: 3}}` matches numbers
  greater than 3 and nothing else. Only three brackets were enforced, so a
  collection holding a `MaxKey` returned that document for *every* `$gt` query.
  96 of 112 probed (bound, operator, collation) shapes disagreed with mongod;
  all 112 now agree. A `MinKey` / `MaxKey` **bound** remains the one exception
  and compares across every type.
- **A JavaScript value is not a string.** `bson.Code` subclasses `str`, so it
  took the string type rank, sorted among the strings, and matched a string
  bound. Being unhashable, it also crashed the cached collation key, so an
  ordinary collated sort over a collection holding one answered `internal
  server error`. mongod ranks JavaScript between Regex and MaxKey.
- **`$round` / `$trunc` validate their precision** the way mongod does — three
  ordered checks with three different codes. A fractional `Decimal128`, an
  out-of-int32 integer, a string and a bool were all silently accepted:
  `{$round: ["$n", -25]}` answered `0.0`.
- **`$indexOfArray` has its own error codes** (9711600 / 9711601), and a
  negative index is an error rather than a clamp — `{$indexOfArray: [[1,2,3],
  3, -1]}` answered `2`.
- **`$toDouble` follows C's `strtod`.** It accepts the hexadecimal spellings
  mongod accepts (`"0X1f"` is 31.0) and reports an unrepresentable magnitude as
  an error rather than saturating (`"1e400"` answered `inf`). The `"0x"` gate is
  the literal lower-case prefix mongod uses, not a case- and sign-insensitive
  one.
- **`$toObjectId`** names the offending character when the length is right,
  instead of reporting "expected 24 but found 24".
- **`$dateToString`'s format language is not `strftime`.** mongod accepts
  exactly `%b %d %j %m %u %w %z %B %G %H %L %M %S %U %V %Y %%` and refuses the
  rest; a typo'd `%a` used to render a value instead of erroring. `%z` and `%Z`
  were empty and are now the offset (`+0000`) and the offset in minutes (`0`),
  and month names no longer come from the machine's locale.
- **`$toUpper` / `$toLower` / `$strcasecmp` map ASCII only**, and `$trim`'s
  default strips mongod's fixed 20-code-point table. Python's Unicode case
  folding turned `"straße"` into `"STRASSE"` where mongod answers `"STRAßE"`.
- **A regex as the bound of a range operator or `$ne`** is refused at parse
  time, where an empty result set used to hide the malformed query.
- **`$range` with a zero step**, and the domain guards on `$sqrt` / `$ln` /
  `$log10` / `$log` / `$pow`, carry mongod's codes.

#### Changed

- The Rust engines share one `Fallback` type that can carry a mongod error, so
  a refusal reaches the client verbatim instead of collapsing into a generic
  "not supported". The command layer picks mongod's constant-folding wrapper by
  where the failure came from, and `StorageError` gained the matching variant
  for query-side refusals.
- The six `$toX` conversion shorthands now route through one `$convert`
  implementation on the Rust side, as they already did in Python. They had
  drifted: `$toBool` and `$convert {to: "bool"}` disagreed on the empty string
  inside a single engine, `$toObjectId` was not registered at all, and the
  numeric ones still deferred on a string source.
