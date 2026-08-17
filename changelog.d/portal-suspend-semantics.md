### Portal Execute suspension and row counts

An Execute that delivers exactly its MaxRows now always answers
PortalSuspended, even when the portal happens to be exhausted — PostgreSQL
cannot know it reached the end until a later Execute fetches past the last
row, and clients that loop until CommandComplete depend on that. Each
Execute's CommandComplete also reports the number of rows *that* Execute
returned rather than the portal's running total, so the final drained
Execute reports `SELECT 0`. The pgtest `portals` corpus file exercises 1182
of its 1550 lines against this (it stops at a stanza that pins
CockroachDB's CHECK-violation message text where we emit PostgreSQL's;
recorded as an expected divergence).

#### Fixed
- An Execute delivering exactly MaxRows sent CommandComplete instead of
  PortalSuspended when no rows remained.
- Portal CommandComplete counted the portal's total rows, not the rows the
  Execute delivered.
