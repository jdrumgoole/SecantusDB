# secantus-pgcatalog

The SQL catalog documents of the SecantusDB PostgreSQL server: the on-disk format of tables, columns, types and the other catalog objects, shared with the Python reference server.

**Internal to [SecantusDB](https://github.com/jdrumgoole/SecantusDB).** This crate is an
implementation detail of the SecantusDB PostgreSQL server, published so it can be
built from crates.io. It carries **no semver promise**: any release may change
its API. Depend on `secantus-pg` instead.

## Licence

Apache-2.0.
