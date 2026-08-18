### DROP TRIGGER

`DROP TRIGGER [IF EXISTS] name ON table` is now supported, removing a
BEFORE INSERT FOR EACH ROW trigger so it stops firing on subsequent inserts.
Dropping a trigger that doesn't exist raises `42704` (or is silently tolerated
with `IF EXISTS`). This completes the trigger DDL surface (CREATE TRIGGER
already worked) and greens the pgtest `schema_changes_implicit_txn` corpus
file, whose `triggers` subtest prepares CREATE TRIGGER and DROP TRIGGER over
the extended protocol.

#### Added

- `engine.py` / `catalog.py`: `DROP TRIGGER [IF EXISTS] name ON table`
  (`catalog.drop_trigger`), routed through the DROP dispatch.
