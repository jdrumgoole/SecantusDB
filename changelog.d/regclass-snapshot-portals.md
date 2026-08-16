### Regclass parameter oids and bind-time portal snapshots

A `$1::REGCLASS` parameter (and the other reg-pseudotype casts — regtype,
regproc, regprocedure, regnamespace, regrole, oid) now describes with its
real oid (2205 for regclass) in ParameterDescription instead of falling
through to text. And a portal bound inside an explicit transaction block now
captures its results at Bind, matching PG's portal-snapshot semantics: a
later same-transaction DDL statement (for example `ALTER TABLE … RENAME`)
no longer changes what a held portal returns at Execute. Execution errors
still surface at Execute, after BindComplete, and cached-plan revalidation
still raises 0A000 at Execute. Both shapes are pinned byte-for-byte by the
pgtest `bind_and_resolve` corpus file, now fully green.

#### Fixed
- `$1::REGCLASS` and sibling reg-pseudotype casts report their parameter
  oids in ParameterDescription (pgtest `bind_and_resolve:29`).
- Portals bound inside an explicit transaction execute eagerly at Bind
  (read-only SELECTs only), so later same-transaction DDL is invisible to
  the held portal (pgtest `bind_and_resolve:132`).
