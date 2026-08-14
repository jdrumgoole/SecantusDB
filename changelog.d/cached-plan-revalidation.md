### Prepared statements revalidate their cached plan, like Postgres

A named server-prepared statement whose result shape changed under DDL
(a `SELECT *` after `ALTER TABLE ADD COLUMN`) now raises PostgreSQL's
`cached plan must not change result type` (0A000) instead of silently
re-planning — and it raises at planning time, before any side effect, so
a data-modifying CTE's INSERT does not run. The ErrorResponse carries
`ROUTINE=RevalidateCachedQuery`, which is the field (not the SQLSTATE)
pgjdbc's transparent re-prepare-and-retry matches on; without it every
recoverable case surfaced the raw error. Unnamed statements re-plan per
Bind and never raise, matching PG. pgjdbc's AutoRollbackTest — the
1056-variant autosave × DDL × transaction matrix — now passes in full.

#### Fixed
- Named prepared statements: result-shape changes under DDL raise 0A000
  with the RevalidateCachedQuery routine field, before side effects;
  first execution captures the plan identity; unnamed statements are
  exempt.
