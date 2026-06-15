# mongo-ruby-driver Validation Report

Generated 2026-06-12 — SecantusDB 0.5.2b18 vs mongo-ruby-driver f68d676643c1 (`vendor/mongo-ruby-driver/`).

Run `uv run python -m invoke validate-ruby` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver gauges for the official Ruby driver — the same gem Rails + Sinatra applications and the Ruby ecosystem build on.

## Summary by category

| Category | Passed | Failed | Pending | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `spec/mongo` | 206 | 1 | 16 | 223 | 99.5% |
| `spec/support` | 89 | 0 | 8 | 97 | 100.0% |
| **Overall** | **295** | **1** | **24** | **320** | **99.7%** |

Run time: 13.69s.

## Failures (1)

First 30 failed examples for triage:

```
spec/mongo :: Mongo::Collection#create when the collection has options when the collection has a write concern when write concern passed in as an option applies the write concern passed in as an option
```

## How this is generated

**mongo-ruby-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-ruby-driver/` is checked out at the pinned upstream tag with zero local edits. `ruby_validation/runner.py` runs `bundle install` (one-time per checkout), then does a two-phase spawn: phase 1 boots `python -m secantus --port 27018 --storage-path <tempdir>` without `--auth` and uses pymongo to createUser `root-user` (root role) and `ruby-test-user` (readWriteAnyDatabase + dbAdminAnyDatabase); phase 2 stops that daemon and restarts on the same tempdir **with `--auth`**, so the user records persist and the server now enforces auth. rspec runs against phase 2 with `MONGODB_URI=mongodb://root-user:password@127.0.0.1:27018/?authSource=admin`. On-disk tempdir is rmtree'd after the run.

These are **integration specs** — every test opens a real TCP connection to the SecantusDB daemon, SCRAM-authenticates, and exchanges wire commands end-to-end. The pass rate is therefore a true measure of SecantusDB's compatibility with the Ruby driver, not of the driver's own pure-code logic.

The include set is currently narrow on purpose: a single broken test in `spec/mongo/cursor_spec.rb` or `spec/mongo/bulk_write_spec.rb` can pin the runner indefinitely on a tailable getMore that never completes. Each new file is added to `include_paths.py` only after a manual confirmation that it terminates within the runner's wall-clock guard (300 s by default).

Pending tests are honest skips driven by mutually-exclusive environment gates upstream tests opt into: `require_no_auth` (skips when auth is configured — and our gauge always runs with auth), `require_topology :single` (skips on replica-set topology — we always advertise as a single-node RS primary so change streams work), `min_server_version 4.5` (skips on >= 4.6 — we report 7.0.0). Resolving them would require running additional gauge configurations rather than fixing SecantusDB behaviour. The pending number is therefore a lower bound on tests SecantusDB "could pass" if the runner spun up alternate daemons; the failed number is the only signal that matters for conformance.
