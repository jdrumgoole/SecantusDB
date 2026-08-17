### Cached-plan revalidation happens at Bind

A named prepared statement whose result shape changed under DDL now raises
`cached plan must not change result type` (0A000) during Bind, replacing
BindComplete, so no portal is created — matching PostgreSQL, where the
revalidation is part of planning rather than execution. Previously the error
arrived at Execute, after the client had already been told the bind
succeeded. Checking at Bind also keeps the revalidation ahead of any side
effect, which is what a data-modifying CTE needs. The pgtest
`prepared_stmt_invalidation` corpus file pins the exact reply and is now
green.

#### Fixed
- The cached-plan error was reported after BindComplete instead of instead
  of it.
