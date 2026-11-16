### SecantusDB is back on PyPI, carrying both Rust servers

`pip install SecantusDB` installs the Rust MongoDB server (`secantusd-rs`)
and the Rust PostgreSQL server (`secantusd-pg`, beta) as prebuilt binaries
for Linux x86_64 and macOS arm64, with a small `secantusdb` launcher that
starts either one from a test in two lines. The wheel no longer contains the
deprecated Python servers.

#### Added

- `publish-pypi.yml`: after each `secantusdb-v*` binary release, repackage
  that release's `secantusd-rs` and the newest `secantusd-pg` release into
  one `py3-none` wheel per platform, smoke-test each wheel in a clean venv
  on Linux and macOS (pymongo and psycopg round trips, both console scripts),
  and publish it through PyPI trusted publishing with attestations.
  `workflow_dispatch` builds and smoke-tests any tag, and publishes only
  when asked.
- `packaging/pypi/build_wheels.py`: the wheel builder. It checks each
  archive's sha256, reads the platform tag from the binaries (newest glibc
  symbol version; macOS minimum version from the Mach-O header) and versions
  the wheel as the MongoDB server (`0.5.3-beta.196` -> `0.5.3b196`).
- The `secantusdb` Python package: `MongoServer` (`.uri`) and
  `PostgresServer` (`.dsn`) on a free port and a temporary data directory.
- `tests/test_pypi_wheel_build.py`.
