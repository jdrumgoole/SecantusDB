### pg-oracle retries the PostgreSQL signing-key download on any error

#### Fixed

- In the `pg-oracle` job, the "Add the PostgreSQL apt repository" step
  downloads postgresql.org's signing key with `curl --retry 3`. That flag
  retries only timeouts and 5xx answers. On 2026-11-08 (PR #2503) a TLS
  read cut short (curl exit 55) failed the step after the suite had
  passed, so the PostgreSQL 15 login comparisons never ran.
- The download now uses `--retry 5 --retry-all-errors --retry-delay 10`.
