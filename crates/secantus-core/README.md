# secantus-core

The pure-Rust operator engines: BSON sort keys, the query matcher, update operators, the aggregation expression evaluator and pipeline, projection, and the change-stream diff.

**Internal to [SecantusDB](https://github.com/jdrumgoole/SecantusDB).** This crate is an
implementation detail of the SecantusDB Rust servers, published so they can be
built from crates.io. It carries **no semver promise**: any release may change
its API. Depend on `secantus-mdb` instead.

## Licence

GPL-2.0-only.
