### `SET search_path` now decides which schema a bare table name resolves to

The setting was recorded — `SHOW search_path` returned it — but ignored when
resolving an unqualified relation. With the same table name in two schemas, the
answer never moved: `SET search_path TO sa` still read `public.t`, and
`sa, public` and `public, sa` gave the same result, so the order asked for made
no difference.

Three behaviours change, each checked against a real PostgreSQL:

- The path is walked in order, and the first schema holding the name wins.
- A relation in no schema on the path is *invisible* rather than lower
  priority — selecting a `public`-only table with `search_path` set elsewhere
  now reports that the relation does not exist, where it previously returned
  rows.
- `CREATE TABLE` creates into the path's first schema, so a same-named relation
  there is now a conflict. Previously the table was created in `public` while
  every read of that same name resolved to the other schema — writes and reads
  landing in different places.

Two existing tests asserted the old behaviour and have been rewritten. One of
them contradicted its own docstring, which described PostgreSQL's rule
correctly.

One difference remains, recorded rather than silently left: when a bare name
resolves nowhere, the error names the schema that was tried (`"sa.onlypub"`)
rather than the name as written (`"onlypub"`). The error code is correct.
