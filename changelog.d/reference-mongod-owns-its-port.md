### The reference mongod's port race no longer reaches another worker's server

The test harness that starts a throwaway `mongod` for the Rust MongoDB server's comparisons picked a free port, closed its probe socket and spawned `mongod` on it. `mongod` opens WiredTiger before it binds, so another xdist worker's mongod could take that port while ours was still starting. The harness only asked whether its own process had exited, which it had not yet, so it went on talking to the other worker's server. On 2026-11-27 (rm-batch snapshot PR #3076, run 45974752150, job `mdb-binary (live)`) that was 108 setup errors in `tests/mongod_live/test_interrupt_checks.py` (`replSetInitiate` reached a standalone mongod: "This node was not started with replication enabled", code 76). The same race could have measured a server the harness did not own and reported agreement.

Ownership is now a positive check: the pid the answering server reports in `serverStatus` has to be the process the harness spawned. When it is not, the attempt raises `_LostPortRace` and the existing retry takes another port; four lost races in a row is still an error.

#### Fixed

- `tests/mongod_reference.py`: `_wait_owned` and `_assert_owns_port` compare the answering server's `serverStatus().pid` with `proc.pid`, instead of "our process has not exited".
- `tests/backlog/test_rm_storage_model.py` (`_mongod_on`, `_mongod_standalone_on`) and `tests/backlog/test_rm_index_spec_fidelity.py` (`_standalone_mongod`) probed a port the same way and now use the same identity check, taking another port when they lose it.
- Fixes RM-834 (`tests/backlog/test_rm_reference_port_race.py::test_a_live_process_is_not_proof_of_ownership`, `::test_a_mongod_that_lost_its_port_is_refused_not_trusted`, `::test_the_retry_takes_another_port_and_returns_its_own_mongod`, `::test_four_lost_races_are_still_an_error`).
