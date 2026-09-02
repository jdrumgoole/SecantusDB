### Decimal128 takes part in the numeric order on the Rust server

mongod treats `Decimal128` as one of the numeric types for comparison and sorting: a mixed field sorts `Decimal128("1") < 2 < Decimal128("2.5") < 3.0`, and `{$gt: [Decimal128("2.5"), 2]}` is true.

`order::cmp` had always handled that — rank 3 routes through `numeric::classify`, which understands decimals. Only `order::is_sortable` excluded them, and that one predicate is what every comparison consults first. So on the Rust server **every comparison involving a decimal deferred**, and a defer there is a generic `BadValue` with no Python behind it.

The practical effect was larger than the probe count suggests: `$gt` / `$lt` / `$cmp`, `sort()`, and range queries like `find({v: {$gt: 2}})` were all unusable on a collection holding decimals.

| | Before | After |
| --- | --- | --- |
| `agg_expressions.py` codes, Rust | 912 | **907** |
| Decimal sort / compare / range query | deferred | matches mongod |

Five corpus shapes; a whole capability in practice. `tests/test_decimal128_ordering.py` pins it against **both** servers, because the defect was on the Rust one and a Python-only test would have proved nothing about it.

#### A note on the test that had to change

`order::tests::sortable_gating` asserted `!is_sortable(Decimal128)` — pinning a *gating decision* rather than a behaviour. Its own comment records the same thing having happened with bools, which were excluded for the same wrong reason. mongod's actual sort order was probed before the assertion was changed.
