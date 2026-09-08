### Rust pgserver: multi-predicate JOIN/subquery WHERE

A JOIN (or aggregate-subquery) WHERE clause now accepts several ANDed
predicates on either side — `WHERE t.oid = $1 AND a.attnum > 0 AND NOT
a.attisdropped` — with the operators `=`, `>`, `>=`, `<`, `<=`, and `NOT
<boolcol>`, where before only a single left-side equality was allowed. A
right-side predicate now filters the right rows (correct for the INNER joins
these catalog queries use) rather than being refused.

This is the first piece of the composite `CompositeInfo.fetch` campaign (its
inner subquery joins `pg_attribute` to `pg_type` with exactly this
multi-predicate WHERE); the remaining piece — a subquery as a LEFT-JOIN side —
is tracked in `tasks/backlog.md`.

#### Added
- `=` / `>` / `>=` / `<` / `<=` / `NOT <bool>` predicates, ANDed, on either
  side of a JOIN's WHERE (`JoinSelect.filter` is now a `Vec<JoinPred>`).
