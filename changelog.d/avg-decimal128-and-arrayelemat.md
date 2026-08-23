### `$avg` no longer crashes on Decimal128, and `$arrayElemAt` stops inventing nulls

A `$group` whose `$avg` saw a Decimal128 alongside any other numeric type threw an
unhandled `TypeError` out of the accumulator, which reached the client as a bare
"internal server error". `$sum` had always used the type-preserving `bson_add`;
`$avg` used a raw `+=` and was simply missed.

Fixing the crash uncovered a second bug beneath it: the average came back with 27
significant digits where mongod gives 34, because Python's default decimal context
is 28 while Decimal128 carries 34. Widened for the division, the result is now
byte-identical to mongod.

Separately, `$arrayElemAt` with an out-of-range index returned null on both
servers. mongod evaluates it to *missing*, so `$project` omits the field entirely —
we were adding a field mongod does not send. A missing or null input array really
is null, and that is unchanged.

#### Fixed

- `$avg` over Decimal128 returns a full-precision Decimal128 instead of raising
  `TypeError`. `tests/test_avg_decimal128.py`.
- `$arrayElemAt` out of range evaluates to missing on both servers, so `$project`
  omits the field as mongod does.
