### PostgreSQL portal semantics

Named portals now behave like PostgreSQL's: re-binding a portal name that is
still live inside the same explicit transaction raises 42P03 "portal already
exists" (the unnamed portal keeps its silent replace), portals are destroyed
at transaction end — a suspended portal resumed after its implicit
transaction settled at Sync answers 34000 — and DROP TABLE refuses with
55006 while an undrained portal in the session still reads the table,
poisoning the block. Interleaved suspended portals (multiple active portals
draining alternately under MaxRows) work across the board. Nine of the
pgtest `multiple_active_portals` subtests pin these shapes; the file's
remaining subtests need row-lazy portal execution (tracked in the backlog).

#### Fixed
- Re-binding a live named portal silently replaced it.
- Suspended portals survived transaction end.
- DROP TABLE succeeded under active portals reading the table.
