### Fix: DROP TABLE no longer pins itself against the active-cursor guard

The active-portal DROP guard (which refuses `DROP TABLE` with 55006 while
a suspended cursor in the same session still reads the table) counted the
`DROP TABLE` statement's own extended-protocol portal as a "query using
the table" — so a plain `DROP TABLE t` via the extended protocol refused
itself. This broke pgjdbc's DatabaseMetaDataTest at setup (its
`DROP TABLE IF EXISTS bestrowid CASCADE` failed, aborting the whole
class). Only an active READ cursor now pins a table: a write portal (DML
/ DDL, including the DROP being executed) or a not-yet-executed portal
never does. The real suspended-SELECT pin (55006) is unchanged.

#### Fixed
- `DROP TABLE` over the extended protocol no longer self-pins with 55006.
