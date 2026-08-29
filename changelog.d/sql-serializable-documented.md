### `SERIALIZABLE` is documented as snapshot isolation

The SQL server accepts all three isolation levels and reports back whichever was
requested, but every explicit transaction runs on the storage engine's snapshot
isolation — what PostgreSQL calls `REPEATABLE READ`. For `SERIALIZABLE` that
difference has teeth: snapshot isolation permits write skew, so two transactions
can each read what the other is about to change and both commit, where
PostgreSQL aborts one.

The SQL guide now spells this out, with the four-way comparison against a real
PostgreSQL, a worked write-skew example, and what to use instead when an
invariant genuinely needs protecting (an explicit lock, or a constraint the
database can check).

Mapping `SERIALIZABLE` onto snapshot isolation is deliberate and has precedent —
Oracle has long done the same — and it keeps drivers and ORMs that request the
level working. Being silent about it was the problem, not the mapping.
