### CI caches its Python interpreters

#### Fixed

- On 2026-11-08, three PR runs went red before any test ran. In each, the
  Python 3.10 download in the pg-oracle shards got HTTP 504 from GitHub
  three times in a row (run 42469436515). The ubuntu-26.04 runner image has
  no 3.10 in its tool cache, so every shard downloaded the interpreter on
  every run. Every other Linux job did the same for 3.12 through
  `uv python install`.
- A new composite action, `.github/actions/uv-python`, restores uv's
  python-dir from the Actions cache. The cache key is the OS, the
  architecture, the image and the Python version, and does not change with
  `uv.lock`. On a miss, the action retries the install with a backoff, so
  one 504 no longer fails the job.
  - It replaces the Python setup in test.yml's `test`, `rust`,
    `rust-storage`, `storage-engine` and `pg-oracle` jobs, in
    psycopg-tls.yml, and in validate.yml's three uv jobs.
  - Windows keeps the runner tool cache, because uv's python-dir does not
    restore on NTFS (#524).
  - pg-oracle and psycopg-tls now build against a uv-managed 3.10 instead of
    setup-python's. Their WiredTiger build cache gets a new key once, then
    restores as before.
