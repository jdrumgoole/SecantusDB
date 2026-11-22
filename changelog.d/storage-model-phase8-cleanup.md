### Dropping and renaming a collection are catalog-only, and the old-layout code is gone

Dropping or renaming a collection now changes the catalog and nothing else, as
in mongod: one transaction removes or moves the collection's registry rows, its
placement and its `_mdb_catalog` row together with the operation's oplog entry,
and a dropped collection's tables go to the drop-pending reaper once it
commits. A rename that replaces a target is one transaction too, where it used
to be four around a tombstone. The code that read the layouts earlier builds
wrote, and the markers it left, is removed. The store format is now 10; a
format-9 store is refused by name, like the earlier ones.

#### Changed

- `secantus-storage`: the drop tombstone table (`secantus_drop_tombstones`),
  `purge_dropped_collection` and `recover_pending_drops` are gone. A kill after
  a drop or rename commits leaves tables no placement names, which the next
  open drops, and no registry row. Store format 10
  (`store_format::STORE_FORMAT_VERSION`). Proved by
  `crates/secantus-storage/tests/drop_chunk.rs::a_kill_after_a_drop_commits_leaves_no_registry_row`
  and `crates/secantus-storage/tests/rename_chunk.rs::a_kill_after_a_rename_commits_leaves_no_registry_row`.
- `secantus-storage`: the `entryFormat`, `numberKeys`, `pythonKeys` and
  `geoKeys` index-option markers are no longer filtered anywhere, and
  `geoKeys` is no longer written (nothing read it, and `_mdb_catalog` never
  carried it).

#### Removed

- `secantus-core`: `sortkey`'s number-key and `_id`-key format 1
  (`NUMBER_KEY_FORMAT`, `ID_KEY_FORMAT`, `with_number_key_format_1`,
  `with_id_key_format_1`, `may_be_format1_id_key`, `is_format1_number_key`,
  `encode_id_key_format_1`).
- `secantus-commands`: `upgrade_time_series` (a store of this format cannot
  hold measurements as documents) is replaced by `create_missing_buckets`,
  which makes the bucket collection of a time-series collection a kill left
  without one; `auth::backfill_user_ids` (RM-031).
- `secantus-pg`: name-keyed index comments and their rekey on
  `ALTER INDEX ... RENAME` (RP-923), and the `pg_largeobject` listing of an
  object with no page map.
- Tests retired with a written reason (the stores they built can no longer be
  opened): `a_stored_user_without_a_user_id_gets_one`,
  `an_index_comment_recorded_under_its_name_survives_a_rename`,
  `interrupted_drop_recovers_at_open`.

#### Fixed

- Closes RM-153: a build from before number-key format 2 writing format-1 keys into
  a store already at format 2 cannot happen, as no build that wrote that
  layout opens a store in this one, and none of this build's reads has a
  format-1 key to find.

#### Docs

- CLAUDE.md's "One on-disk format", the `python-servers` skill and the
  architecture, indexes and concurrency pages describe mongod's layout; the
  old shard-and-frame layout is described as the deprecated Python servers'
  only.
