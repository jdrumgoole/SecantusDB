### The Rust PostgreSQL server reuses a plan that stores a small integer in a bigint column

A driver sends a small integer as `int2` or `int4`. Bound for a `bigint`
column, the planner widens it, and a prepared `INSERT` or `UPDATE ... SET`
that did so was planned again on every execution.

#### Changed

- `secantusd-pg` reuses the plan when an `int2` or `int4` parameter is stored
  in a plain `bigint` column, carrying the widening with the value.
- The `secantusd-pg` binary allocates with mimalloc, as `secantusd-rs` does.
  It is the default `mimalloc` feature of the `secantus-pg` crate;
  `--no-default-features --features bin` builds without it. A program that
  embeds `secantus_pg::PgServer` keeps its own allocator.
