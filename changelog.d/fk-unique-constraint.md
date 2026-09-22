### A foreign key reported the wrong referenced constraint

`pg_constraint.conindid` — the index backing the constraint a foreign key
references — was hardcoded to the referenced table's PRIMARY KEY index, and the
comment beside it stated that as the rule. A foreign key may reference any
UNIQUE constraint, so `REFERENCES pkt(b)` reported `pkt_pk_a` where PostgreSQL
14 reports `pkt_un_b` (measured 2026-09-20).

pgjdbc's `getImportedKeys` reads `PK_NAME` by joining `pkic.oid = con.conindid`,
so this is the name an ORM sees for the key a foreign key points at — naming the
wrong constraint misdescribes the relationship rather than merely omitting it.

#### Fixed

- `conindid` resolves from the foreign key's `confkey`, matching whichever
  PRIMARY KEY or UNIQUE constraint covers those columns, and falling back to the
  primary key when nothing matches.

Columns are matched as a sorted set, because a foreign key may name the
referenced columns in a different order than the constraint declares them. All
three shapes are pinned by the test: the UNIQUE case that was wrong, the PRIMARY
KEY case that was right and must stay so, and a composite unique.

Measured on pgjdbc's `DatabaseMetaDataTest` (194 tests): **23 failures → 21**,
14 distinct → 13, no regressions. `foreignKeysToUniqueIndexes` goes green.
