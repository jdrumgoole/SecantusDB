### UNIQUE constraints are enforced by the storage engine

A `UNIQUE` constraint was upheld by looking for a clashing row before writing
one. That look happens against the snapshot the writing transaction is reading,
so it could not see a value another transaction had just committed, nor one a
second writer was inserting at that moment. Either way a duplicate was stored,
and the constraint quietly did not hold.

Declaring a constraint now creates the index that enforces it, so the storage
engine decides: a value already present is refused whoever wrote it and
whenever, and two transactions reaching for the same value collide so that only
one keeps it. Adding a constraint to an existing table does the same, and
dropping it removes the index.

The SQL rules around NULL are preserved: any number of NULLs satisfy a `UNIQUE`
constraint, and a constraint over several columns does not apply to a row where
any of them is NULL.

Constraints declared `DEFERRABLE` are unchanged. Those are allowed to be
violated part-way through a transaction and are judged when it commits — a
swap of two values being the usual case — so they continue to be checked at
commit rather than on every write.

#### Fixed

- A `UNIQUE` constraint no longer admits a duplicate written by a transaction
  that began before the value was committed, or by two transactions at once.
