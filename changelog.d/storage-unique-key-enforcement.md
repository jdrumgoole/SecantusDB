### Unique indexes are enforced by the storage engine

A unique index used to be upheld by looking for a clashing value before writing
one. That look happens against the snapshot the writer is reading, which cannot
show a value another transaction committed a moment earlier, and cannot show a
value a second writer is inserting right now. Both cases stored a duplicate.

Unique indexes now also record each indexed value under a key that is the value
itself, so WiredTiger decides. A value already present is refused by the engine
whoever wrote it and whenever; two writers reaching for the same value collide
and only one keeps it. Creating a unique index over rows that already exist
claims their values too.

Nothing else changes: the existing index entries, and every query path that
reads them, are untouched, and a database written by an earlier version stays
readable.

This covers unique indexes as used through the MongoDB interface. A `UNIQUE`
constraint declared in SQL is still upheld the older way and keeps the same two
gaps; the groundwork for closing that is now in place.

#### Fixed

- A unique index no longer admits a duplicate written by a transaction that
  began before the value was committed, or by two writers at once.
