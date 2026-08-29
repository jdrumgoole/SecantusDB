### Tests start from a cloned WiredTiger home instead of building one each time

The test suite spent most of its time waiting on WiredTiger rather than running
Python. Measuring the per-test fixture floor showed that of the ~281 ms it cost
to stand a server up, ~234 ms was inside WiredTiger's C library — and ~137 ms of
that was WiredTiger creating the same dozen empty tables over and over, once per
test, at roughly 9.7 ms per table. That work is identical every time: every test
begins from the same empty schema.

So each worker now builds one pristine database home at the start of a session
and copies it per test, rather than asking WiredTiger to construct a new one from
scratch. Where the filesystem supports copy-on-write the copy is nearly free and
uses less disk than the old approach did. Across the 22 test files converted so
far this cut their runtime from 260 s to 195 s, and the equivalence a change like
this depends on — that a copied database behaves exactly like a freshly built one
— is pinned by tests that compare the two directly rather than taking it on faith.

#### Added
- `tests/wt_template.py` (`build_template` / `clone_template`) and the
  session-scoped `_wt_template` + per-test `wt_home` fixtures in
  `tests/conftest.py`.
- `tests/test_wt_template.py`, pinning cloned-vs-created equivalence, clone
  isolation, and durable close-and-reopen.
- `tasks/rust-test-harness-investigation.md`, recording the measurements behind
  this (and why reimplementing the harness in Rust was rejected).

#### Changed
- 22 test files now take the `wt_home` fixture instead of creating a WiredTiger
  home in `tmp_path`.
