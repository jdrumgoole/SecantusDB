### A reference mongod that never becomes ready fails the test instead of skipping it

A `mongod` that crashed or hung while starting made every test that needed it a skip, which read as a comparison nobody had asked for.

#### Fixed

- Fixes RM-837 (`tests/backlog/test_rm_837_reference_never_ready.py::test_a_mongod_that_exits_during_startup_fails_with_its_log`, `::test_a_mongod_that_never_answers_fails_with_its_log`).
  `tests/mongod_reference.py::_start_mongod_once` called `pytest.skip("mongod did not become ready")`. It now raises, with the process's exit status (or that it was still running) and the last 2000 characters of mongod's own log, and still terminates the process and removes its dbpath. Left as skips, as scope decisions: the "no mongod on PATH" skips of the deprecated Python server's differential suite (`tests/test_mongod_differential.py`), `only_on` (a mongod answer that differs by platform, checked on the others) and the RM-219 test's "openssl is needed". The backlog and `mongod_live` fixtures already fail, not skip, when there is no mongod, and `tests/backlog/test_rm_storage_model.py` and `test_rm_index_spec_fidelity.py` fail when theirs does not answer.
