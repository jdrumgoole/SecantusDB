### The Rust PostgreSQL server's release build is optimised across the whole program, and by a profile

The released `secantusd-pg` binary was compiled at Cargo's defaults: no
link-time optimisation and no profile. Measured on a 16-vCPU droplet with one
client selecting by primary key, that binary answered 12,052 statements a
second. Built with link-time optimisation it answered 14,402, and with a
profile as well 17,855.

#### Changed

- `crates/secantus-pg` builds its release profile with `lto = "fat"` and one
  codegen unit. `cargo install secantus-pg` gets this too. A release build
  takes longer: about seven minutes on 16 vCPUs.
- The released binaries are built in two stages by
  `scripts/build_pg_release.sh`: an instrumented build runs
  `bench/pgo_workload_pg.py`, and the build that ships uses the profile it
  wrote. `cargo install` cannot have the profile.

#### Fixed

- `invoke do-pg-bench` measured PostgreSQL over TLS and `secantusd-pg` in the
  clear. Ubuntu's PostgreSQL has `ssl = on` and psycopg asks for TLS when the
  server offers it, so PostgreSQL paid for encryption the other side did not.
  Every DSN the benchmark uses now says `sslmode=disable`, the run records
  `pg_stat_ssl` for its own connection, and it stops if that says TLS was on.
