### Rust PostgreSQL server: system indexes, SQL-function error context, SECURITY DEFINER

#### Fixed

- Writes to system catalogs were silently wrong: UPDATE and DELETE reported 0 rows, and INSERT created a hidden table. Now:
  - an UPDATE that leaves every matched row unchanged reports its rows, as PostgreSQL does;
  - a real change, a DELETE that matches rows, or an INSERT is refused with 0A000.
- `pg_index`, `pg_indexes` and `pg_get_indexdef` list PostgreSQL's own 162 system indexes. Catalog result columns carry their table oid and column number.
- Errors in `LANGUAGE sql` functions carry PostgreSQL's CONTEXT: `statement N`, `during inlining`, or no frame, following PostgreSQL's inlining rules.
- `CREATE FUNCTION ... SECURITY DEFINER` and `SET` clauses were dropped. They are now stored and applied: a definer function runs as its owner, and a SET clause holds for the call only.

#### Performance

- Extended-protocol replies are buffered until Sync or Flush rather than flushed once per message. Extended `select 1` went from 61 to 53 µs.
