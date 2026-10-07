# secantus-pgplan

PostgreSQL parse trees (from libpg_query, via `pg_query`) lowered to the MQL filters and pipelines the SecantusDB storage evaluates, plus the scalar function surface of the SecantusDB PostgreSQL server.

**Internal to [SecantusDB](https://github.com/jdrumgoole/SecantusDB).** This crate is an
implementation detail of the SecantusDB PostgreSQL server, published so it can be
built from crates.io. It carries **no semver promise**: any release may change
its API. Depend on `secantus-pg` instead.

## Licence

Apache-2.0.
