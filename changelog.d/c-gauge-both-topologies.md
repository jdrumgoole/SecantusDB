### The C driver gauge covers change streams again

Running libmongoc's suite against a `--standalone` server fixed four tests
that assert standalone semantics, but libmongoc then skipped every test that
needs a replica set -- most of its change-stream suite. The gauge now runs the
suite against the default single-node replica set and re-runs just the six
standalone-only tests against a standalone server, then merges the two. On the
Rust server that is 790 passed, 2 failed (the known IPv6 tests), up from 768.

#### Changed

- `c_validation` runs two passes and merges them; the standalone tests are
  listed in `c_validation/include_paths.py` as `STANDALONE_ONLY`.
