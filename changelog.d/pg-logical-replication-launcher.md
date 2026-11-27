### The PostgreSQL server runs a logical replication launcher, whose scans of `pg_subscription` survive a restart

`secantusd-pg` now runs PostgreSQL 15's logical replication launcher
(`ApplyLauncherMain`) as a thread of its own: it heap-scans
`pg_subscription` twice as it starts, again on every reload and every
180 seconds, and its counts reach the cumulative statistics only as it
exits, at a clean shutdown. Across a restart
`pg_stat_get_numscans('pg_subscription'::regclass::oid)` therefore moves by
the run before's scans, as on PostgreSQL 15.20.

#### Fixed

- Across a restart the `pg_subscription` scan count did not move, where
  PostgreSQL's moves by its launcher's scans (2 after a run with no reload,
  one more per reload). A server whose configuration sets
  `max_logical_replication_workers = 0` starts no launcher, as
  `ApplyLauncherRegister` does. The launcher counts in
  `pg_stat_get_db_numbackends(0)` (now 1, where PostgreSQL counts its five
  background processes: RP-1141). A SIGHUP now reaches the launcher even
  when the server has no TLS or `pg_hba.conf` file to re-read.
  Fixes RP-1096 (tests/backlog/test_rp_catalog_reads.py::test_the_logical_replication_launcher_scans_pg_subscription,
  tests/backlog/test_rp_catalog_reads.py::test_no_launcher_runs_without_logical_replication_workers)
