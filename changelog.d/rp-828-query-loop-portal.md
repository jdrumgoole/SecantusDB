### Rust PG server: a PL/pgSQL FOR loop over a query walks a portal

A PL/pgSQL `for x in select ... loop` or `for x in execute '...' loop` now
opens its query as an unnamed portal and walks it pinned, as PostgreSQL
15.19 does (`exec_stmt_fors` / `exec_stmt_dynfors` through
`exec_for_query`), the same way a cursor FOR loop already did. While the
loop runs, `pg_cursors` lists `<unnamed portal N>` with the query's text,
and a `close all` in the body is refused. FOUND is now set when the loop
ends rather than before its first row, so the body sees the FOUND it was
entered with.

#### Fixed

- `pg_cursors` lists a FOR loop's `<unnamed portal N>` (its statement the
  query's text) while the loop runs, numbered in the order the loops open,
  and drops it when the loop ends, by EXIT or RETURN too.
- An `execute 'close all'` in the body of a FOR loop over a query or an
  EXECUTE is 24000 `cannot drop pinned portal "<unnamed portal N>"`, where
  it ran.
- FOUND inside such a loop's body is the value it had before the loop;
  it was already true on the first row.

Fixes RP-828 (tests/backlog/test_rp_found_rp823.py::test_a_query_loop_walks_a_pinned_portal)

Files RP-905 (a FOR loop or OPEN over INSERT/UPDATE/DELETE is 0A000),
RP-906 (`EXECUTE ... USING` in a FOR loop or OPEN ignores USING) and
RP-907 (a portal whose query fails running takes no number).
