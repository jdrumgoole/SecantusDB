# mongo-ruby-driver Validation Report

Generated 2026-05-10 — SecantusDB 0.4.0b10 vs mongo-ruby-driver f68d676643c1 (`vendor/mongo-ruby-driver/`).

Run `uv run python -m invoke validate-ruby` to refresh. The pass rate is the analogue of the pymongo / mongo-go-driver / mongo-node-driver / mongo-java-driver gauges for the official Ruby driver — the same gem Rails + Sinatra applications and the Ruby ecosystem build on.

## Summary by category

| Category | Passed | Failed | Pending | Total | Pass rate |
|---|---:|---:|---:|---:|---:|
| `spec/mongo` | 1199 | 0 | 249 | 1448 | 100.0% |
| `spec/runners` | 521 | 0 | 5 | 526 | 100.0% |
| `spec/spec_tests` | 5154 | 0 | 20 | 5174 | 100.0% |
| `spec/support` | 93 | 0 | 0 | 93 | 100.0% |
| **Overall** | **6967** | **0** | **274** | **7241** | **100.0%** |

Run time: 33.44s.

## How this is generated

**mongo-ruby-driver's tests are run unmodified, against a standalone SecantusDB daemon.** The submodule at `vendor/mongo-ruby-driver/` is checked out at the pinned upstream tag with zero local edits. `ruby_validation/runner.py` runs `bundle install` (one-time per checkout), spawns `python -m secantus --host 127.0.0.1 --port <free> --storage-path ':memory:'` as a subprocess, exports `MONGODB_URI` (the env var `spec/support/spec_config.rb` reads at bootstrap), then runs `bundle exec rspec --format json <paths>` for the in-scope set in `ruby_validation/include_paths.py`.

Initial scope is **lite specs only** — files that `require 'lite_spec_helper'`. Those don't connect to a server, so they exercise the BSON / URI / auth-handshake / event / topology-decoding logic without depending on cluster behaviour. Once the lite gauge is green, integration specs that work against a single-node deployment will be added.
