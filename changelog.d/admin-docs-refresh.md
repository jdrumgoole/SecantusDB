### Admin console docs catch up, and the docs build goes green again

The admin web UI documentation still described the console as it was
before the Rust server and the SQL server existed. Most conspicuously it
carried a per-server feature table asserting that the Rust server lacked
archive restore, oplog and TTL pruning, role grant/revoke, `killOp`,
logs, and profiling — a table that had been wrong for months, and whose
in-code counterpart has since been removed. The page now explains why
there is no such table any more, and what replaced it: SecantusDB
targets start permissive, and a feature is withdrawn only when the
server itself reports the command missing.

The rest of the page caught up with what shipped alongside that —
point-in-time recovery, launching the Rust server from the embedded
control, index collation, target-sourced roles, the wider set of `_id`
types the collection browser can paginate, and the fact that `admin.db`
is a credential store and is now permissioned like one. Several stale
claims went with it, including a "one target server per launch" line that
predated the target hot-swap, and a limitations entry for a saved-
connections page that has since shipped.

Separately, `invoke docs` had been failing. Eleven Rust-server driver
validation reports were never added to the toctree, and since docs-only
commits are deliberately skipped by CI, nothing caught it. All thirteen
reports are now listed and the build is clean again.

#### Fixed

- `invoke docs` builds warning-free. Eleven `validation-report-*-rust-server`
  pages were missing from the toctree, failing the warnings-as-errors build.

#### Changed

- The admin UI docs describe the current capability model, the PITR panel,
  the Rust embedded-server option, index collation, target-sourced roles,
  `admin.db` permissions, and the supported `_id` types for pagination.
