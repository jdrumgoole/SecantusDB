### The PostgreSQL server's debug build optimises the code it spends its time in

#### Changed

- `crates/secantus-pg/Cargo.toml` gives the dev profile per-package
  optimisation: third-party crates at `opt-level = 2`, `secantus-core`,
  `secantus-storage`, `secantus-wt`, `secantus-pgwire`, `secantus-pgcatalog`
  and `secantus-auth` at 1, and `secantus-pgplan` and `secantus-pg` (325k of
  the 458k lines, most of the compile) left at 0. `debug-assertions` and
  `overflow-checks` stay on everywhere.
- Measured 2026-11-09 at one tree, with only our crates rebuilt as CI does
  on a cache hit: compiling went from 50 s to 72 s, the corpora, rule trees
  and builtin coverage (328 tests) from 292 s to 170 s, and the backlog
  regression tests (2,792) from 546 s to 252 s. Every `pg-oracle` shard, the
  local gates and the gauges run this build.
- CI's backlog step hands out tests with `--dist worksteal`, as the suites
  step already does: its isolation specs run up to 126 s each.
