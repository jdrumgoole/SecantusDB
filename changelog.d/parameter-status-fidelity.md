### GUC reporting matches PostgreSQL

ParameterStatus messages now follow the command's CommandComplete (both the
simple and extended protocol paths) rather than preceding it, and the values
themselves are reported the way PostgreSQL reports them: a numeric
`SET TIME ZONE` becomes a POSIX zone spec (`+6` → `<+06>-06`, `-11.5` →
`<-11:30>+11:30`), DateStyle always reads `<style>, <order>` no matter which
order it was written in, and IntervalStyle lowercases. IntervalStyle and
is_superuser are now reported GUCs, so a role switch tells the client its
superuser status changed — once, on a real change. Transaction-scoped
reverts report too: savepoints snapshot GUC state so ROLLBACK TO SAVEPOINT
restores and re-reports whatever changed after it, an error that aborts a
block reverts its SET LOCALs immediately (with the reports alongside the
error, as PG sends them), and every unwind list is ordered
case-insensitively by name. `SET LOCAL TIME ZONE` also honours LOCAL scope
instead of leaking past the transaction. The pgtest `param_status` corpus
file pins all of this and is now green.

#### Added
- IntervalStyle and is_superuser as reported GUCs; savepoint-scoped GUC
  snapshots.

#### Fixed
- ParameterStatus was sent before CommandComplete.
- Numeric time-zone offsets were echoed verbatim instead of as POSIX specs.
- DateStyle echoed the written component order.
- `SET LOCAL TIME ZONE` persisted past the transaction.
