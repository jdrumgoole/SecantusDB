### EXECUTE portals describe their underlying statement

Describing a portal bound to a wire-parsed `EXECUTE name(args)` — the
SQL-level PREPARE/EXECUTE flow driven through the extended protocol — now
resolves the underlying prepared statement and reports its result shape: a
prepared SELECT answers with its RowDescription instead of NoData, so
clients no longer receive DataRows without a preceding row description. The
pgtest `execute` corpus file pins the exchange and is now green.

#### Fixed
- `Describe(P)` of an `EXECUTE` portal returned NoData for row-returning
  prepared statements.
