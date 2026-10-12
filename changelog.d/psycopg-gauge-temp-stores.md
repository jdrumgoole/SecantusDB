### The psycopg gauge removes its server's store

#### Fixed

- `psycopg_validation.runner` made the server's storage directory, and the
  TLS run's certificate directory, with `mkdtemp` and never removed them:
  about 100 MB of WiredTiger left in the temporary directory by every gauge
  run. Both are removed when the runner exits.
