### Multi-statement batches are one implicit transaction, like Postgres

A multi-statement simple query now runs in a single implicit
transaction, matching real PG: a mid-batch error rolls back the earlier
statements' writes (their result rows were already streamed — PG
streams too), an explicit `BEGIN` inside the batch takes the
transaction over — its characteristics included, so `BEGIN READ ONLY`
makes a following write fail `25006` and poison the block — and
`COMMIT`/`ROLLBACK` end it with the remainder starting a fresh implicit
transaction. Previously each statement ran in its own autocommit
transaction, so a failed batch left earlier writes behind — a recorded
semantic divergence, now closed (pinned by the pgtest `batch_stmt`
corpus, which is fully green).

In support: a batch whose statements only parse individually (``BEGIN
READ ONLY`` mid-batch needs the regex fallback) now splits at top-level
semicolons — respecting quotes, dollar-quotes, and comments — and
parses each segment through the full entry point.

#### Fixed

- `sql/engine.py`: `run_sql` wraps multi-statement batches in an
  implicit transaction with PG's takeover/settle rules; BEGIN takeover
  applies the BEGIN's characteristics; the read-only gate poisons an
  open block.
- `sql/planner.py`: top-level-semicolon segment fallback when a batch
  fails to parse as one string.
