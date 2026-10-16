### Rust PostgreSQL server: a SIGHUP at startup no longer stops the server

#### Fixed

- **A SIGHUP sent just after `secantusd-pg` printed its "listening on" line
  stopped it.** The reload handler was installed after that line, and until
  then SIGHUP was the stop handler's, so a harness that reloaded the moment
  the server announced itself could stop it instead. The handler now goes in
  before the line, as the stop handler already did
  (`crates/secantus-pg/tests/signals.rs::sighup_does_not_stop_a_server_with_nothing_to_reload`,
  which caught it on Linux CI).
