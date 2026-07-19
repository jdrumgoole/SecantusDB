### SQL server: savepoint rollback reverts DDL; wide binary numerics round-trip

`ROLLBACK TO SAVEPOINT` now undoes schema changes made after the savepoint,
not just data writes. A `CREATE TYPE` / `CREATE TABLE` / `DROP` / `ALTER`
inside a savepoint snapshots every catalog collection (they're tiny), so the
rollback restores the pre-savepoint schema — a re-`CREATE` of the same type
then succeeds, and a `DROP` is undone. Previously only DML target
collections were snapshotted, so the catalog change leaked past the abort
(psycopg's `test_change_type_savepoint`, which creates and rolls back an enum
three times, hit "type already exists").

The binary `numeric` decoder handles arbitrarily wide values: a wide
integral magnitude combined with a large declared scale sized the Decimal
context too small and made the final quantize raise `InvalidOperation`
(surfacing to the client as an internal error). The context now spans the
full integer + fractional digit count, so `test_dump_numeric_exhaustive`'s
50-plus-digit values round-trip.

#### Added

- `catalog.ALL_CATALOG_COLLECTIONS`; `engine._is_ddl` drives the catalog
  snapshot for savepoint rollback.

#### Fixed

- `pgextended._decode_numeric` context precision spans the whole value.
