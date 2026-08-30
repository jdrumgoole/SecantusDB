### `invoke sync` now produces a environment that can actually run the suite

The task that sets up a development environment installed only the base test
dependencies. That left out the compiled Rust engine, and because the ~1700 tests
that compare the Rust and Python engines skip themselves when it is missing — on
purpose, so the pure-Python parts still work anywhere — the suite would run to
completion, report success, and be about 1700 tests short. Nothing failed; the
tests simply were not there.

It also never rebuilt that engine when its source changed, so pulling a change to
the Rust side left the comparison running against a stale build. That surfaced as
31 failures on a perfectly healthy main branch, which looks exactly like someone
broke something.

Both are fixed, and the reasoning is written down next to the task and in the
project guide so the next person does not have to rediscover it.

#### Fixed
- `invoke sync` installs all extras and forces a rebuild of the Rust engine.
