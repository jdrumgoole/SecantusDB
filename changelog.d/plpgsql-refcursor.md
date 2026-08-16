### plpgsql refcursors: OPEN … FOR, CLOSE, and FETCH by portal name

plpgsql functions can now declare `refcursor` variables, bind them with
`OPEN <cursor> FOR <query>`, `CLOSE` them, and return them. An OPEN
materializes the query into a session cursor named like PG's unnamed
portals (`<unnamed portal N>`); the returned name is typed `refcursor`
(oid 1790) on the wire, which is what tells a driver to fetch the
result set with `FETCH ALL IN "<name>"` — the exact round-trip pgjdbc's
CallableStatement performs for `{? = call f()}` on a
refcursor-returning function. FETCH/MOVE now also accept quoted cursor
names containing spaces, which the unnamed-portal naming requires.

#### Added
- plpgsql `OPEN <cursor> FOR <query>` and `CLOSE <cursor>` statements;
  `refcursor` declarations and returns (the `OPEN … FOR EXECUTE` form
  stays unsupported).
- The `refcursor` type (oid 1790) in result descriptors.

#### Fixed
- `FETCH`/`MOVE` with a double-quoted cursor name containing spaces no
  longer truncates the name at the last space.
